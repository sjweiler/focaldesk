//! Password-wrapped master key for focald-secrets.
//!
//! The daemon's 32-byte master key is random (never password-derived directly,
//! so password changes only rewrap, never re-encrypt the store). It is stored
//! wrapped under a KEK derived from the login password:
//!
//!   FKEY2 = magic || Argon2 parameters || salt || nonce || AEAD(master)
//!   kek   = Argon2id(password, salt, m=64 MiB, t=3, p=1)
//!
//! Wrapped file lives at ~/.local/share/focaldesk/secrets.key.enc.
//! FKEY1/PBKDF2 files remain readable for migration and are upgraded after a
//! successful login.

#![forbid(unsafe_code)]

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use rand::RngCore;
use zeroize::{Zeroize, Zeroizing};

use argon2::{Algorithm, Argon2, Params, Version};

const MAGIC_V1: &[u8; 5] = b"FKEY1";
const MAGIC_V2: &[u8; 5] = b"FKEY2";
const PBKDF2_ITERS: u32 = 600_000;
const ARGON2_MEMORY_KIB: u32 = 64 * 1024;
const ARGON2_ITERATIONS: u32 = 3;
const ARGON2_LANES: u32 = 1;
pub const LEGACY_WRAPPED_LEN: usize = 5 + 16 + 12 + 32 + 16;
pub const WRAPPED_LEN: usize = 5 + 12 + 16 + 12 + 32 + 16;
pub const MIN_WRAPPED_LEN: usize = LEGACY_WRAPPED_LEN;
pub const MAX_WRAPPED_LEN: usize = WRAPPED_LEN;

#[derive(Debug, PartialEq)]
pub enum Error {
    Format,
    BadPassword,
    Crypto,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Format => write!(f, "not a focald-secrets wrapped key file"),
            Error::BadPassword => write!(f, "wrong password (or corrupt key file)"),
            Error::Crypto => write!(f, "crypto failure"),
        }
    }
}
impl std::error::Error for Error {}

fn legacy_kek(password: &[u8], salt: &[u8; 16]) -> Zeroizing<[u8; 32]> {
    let mut out = Zeroizing::new([0u8; 32]);
    pbkdf2::pbkdf2_hmac::<sha2::Sha256>(password, salt, PBKDF2_ITERS, out.as_mut());
    out
}

fn argon2_kek(password: &[u8], salt: &[u8; 16]) -> Result<Zeroizing<[u8; 32]>, Error> {
    let params = Params::new(ARGON2_MEMORY_KIB, ARGON2_ITERATIONS, ARGON2_LANES, Some(32))
        .map_err(|_| Error::Crypto)?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut out = Zeroizing::new([0u8; 32]);
    argon2
        .hash_password_into(password, salt, out.as_mut())
        .map_err(|_| Error::Crypto)?;
    Ok(out)
}

/// Generate a fresh master key and wrap it. Returns (master, wrapped_file_bytes).
pub fn create(password: &[u8]) -> Result<(Zeroizing<[u8; 32]>, Vec<u8>), Error> {
    let mut master = Zeroizing::new([0u8; 32]);
    rand::rngs::OsRng.fill_bytes(master.as_mut());
    let wrapped = wrap(password, &master)?;
    Ok((master, wrapped))
}

/// Wrap an existing master key under a password.
pub fn wrap(password: &[u8], master: &[u8; 32]) -> Result<Vec<u8>, Error> {
    let salt: [u8; 16] = rand::random();
    let nonce: [u8; 12] = rand::random();
    let k = argon2_kek(password, &salt)?;
    let cipher = ChaCha20Poly1305::new(k.as_ref().into());
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce), master.as_slice())
        .map_err(|_| Error::Crypto)?;
    let mut out = Vec::with_capacity(WRAPPED_LEN);
    out.extend_from_slice(MAGIC_V2);
    out.extend_from_slice(&ARGON2_MEMORY_KIB.to_be_bytes());
    out.extend_from_slice(&ARGON2_ITERATIONS.to_be_bytes());
    out.extend_from_slice(&ARGON2_LANES.to_be_bytes());
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Unwrap the master key with the password.
pub fn unwrap(password: &[u8], wrapped: &[u8]) -> Result<Zeroizing<[u8; 32]>, Error> {
    if wrapped.len() == LEGACY_WRAPPED_LEN && &wrapped[..5] == MAGIC_V1 {
        return unwrap_v1(password, wrapped);
    }
    if wrapped.len() != WRAPPED_LEN || &wrapped[..5] != MAGIC_V2 {
        return Err(Error::Format);
    }
    let memory = u32::from_be_bytes(wrapped[5..9].try_into().unwrap());
    let iterations = u32::from_be_bytes(wrapped[9..13].try_into().unwrap());
    let lanes = u32::from_be_bytes(wrapped[13..17].try_into().unwrap());
    if (memory, iterations, lanes) != (ARGON2_MEMORY_KIB, ARGON2_ITERATIONS, ARGON2_LANES) {
        return Err(Error::Format);
    }
    let salt: [u8; 16] = wrapped[17..33].try_into().unwrap();
    let nonce = Nonce::from_slice(&wrapped[33..45]);
    let k = argon2_kek(password, &salt)?;
    let cipher = ChaCha20Poly1305::new(k.as_ref().into());
    let mut plain = cipher
        .decrypt(nonce, &wrapped[45..])
        .map_err(|_| Error::BadPassword)?;
    finish_unwrap(&mut plain)
}

fn unwrap_v1(password: &[u8], wrapped: &[u8]) -> Result<Zeroizing<[u8; 32]>, Error> {
    let salt: [u8; 16] = wrapped[5..21].try_into().unwrap();
    let nonce = Nonce::from_slice(&wrapped[21..33]);
    let k = legacy_kek(password, &salt);
    let cipher = ChaCha20Poly1305::new(k.as_ref().into());
    let mut plain = cipher
        .decrypt(nonce, &wrapped[33..])
        .map_err(|_| Error::BadPassword)?;
    finish_unwrap(&mut plain)
}

fn finish_unwrap(plain: &mut Vec<u8>) -> Result<Zeroizing<[u8; 32]>, Error> {
    if plain.len() != 32 {
        plain.zeroize();
        return Err(Error::Format);
    }
    let mut master = Zeroizing::new([0u8; 32]);
    master.copy_from_slice(plain);
    plain.zeroize();
    Ok(master)
}

pub fn needs_upgrade(wrapped: &[u8]) -> bool {
    wrapped.len() == LEGACY_WRAPPED_LEN && wrapped.starts_with(MAGIC_V1)
}

/// Rewrap under a new password (login password change).
pub fn rewrap(old_password: &[u8], new_password: &[u8], wrapped: &[u8]) -> Result<Vec<u8>, Error> {
    let master = unwrap(old_password, wrapped)?;
    wrap(new_password, &master)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn random_password() -> [u8; 32] {
        rand::random()
    }

    #[test]
    fn roundtrip() {
        let password = random_password();
        let (master, wrapped) = create(&password).unwrap();
        assert_eq!(wrapped.len(), WRAPPED_LEN);
        let un = unwrap(&password, &wrapped).unwrap();
        assert_eq!(master.as_ref(), un.as_ref());
    }

    #[test]
    fn wrong_password() {
        let password = random_password();
        let mut wrong_password = password;
        wrong_password[0] ^= 1;
        let (_, wrapped) = create(&password).unwrap();
        assert_eq!(
            unwrap(&wrong_password, &wrapped).unwrap_err(),
            Error::BadPassword
        );
    }

    #[test]
    fn rewrap_preserves_master() {
        let old_password = random_password();
        let new_password = random_password();
        let (master, wrapped) = create(&old_password).unwrap();
        let rewrapped = rewrap(&old_password, &new_password, &wrapped).unwrap();
        assert_eq!(
            unwrap(&old_password, &rewrapped).unwrap_err(),
            Error::BadPassword
        );
        assert_eq!(
            unwrap(&new_password, &rewrapped).unwrap().as_ref(),
            master.as_ref()
        );
    }

    #[test]
    fn format_rejects_garbage() {
        let password = random_password();
        assert_eq!(unwrap(&password, b"nonsense").unwrap_err(), Error::Format);
    }

    fn legacy_wrap(password: &[u8], master: &[u8; 32]) -> Vec<u8> {
        let salt: [u8; 16] = rand::random();
        let nonce: [u8; 12] = rand::random();
        let k = legacy_kek(password, &salt);
        let cipher = ChaCha20Poly1305::new(k.as_ref().into());
        let ct = cipher
            .encrypt(Nonce::from_slice(&nonce), master.as_slice())
            .unwrap();
        let mut out = Vec::with_capacity(LEGACY_WRAPPED_LEN);
        out.extend_from_slice(MAGIC_V1);
        out.extend_from_slice(&salt);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        out
    }

    #[test]
    fn legacy_file_unwraps_and_requests_upgrade() {
        let password = random_password();
        let master: [u8; 32] = rand::random();
        let wrapped = legacy_wrap(&password, &master);
        assert!(needs_upgrade(&wrapped));
        assert_eq!(unwrap(&password, &wrapped).unwrap().as_ref(), &master);
        assert!(!needs_upgrade(&wrap(&password, &master).unwrap()));
    }

    #[test]
    fn rejects_unbounded_argon2_parameters() {
        let password = random_password();
        let (master, mut wrapped) = create(&password).unwrap();
        wrapped[5..9].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(unwrap(&password, &wrapped).unwrap_err(), Error::Format);
        drop(master);
    }
}
