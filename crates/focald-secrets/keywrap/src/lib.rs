//! Password-wrapped master key for focald-secrets.
//!
//! The daemon's 32-byte master key is random (never password-derived directly,
//! so password changes only rewrap, never re-encrypt the store). It is stored
//! wrapped under a KEK derived from the login password:
//!
//!   file = b"FKEY1" || salt[16] || nonce[12] || ChaCha20-Poly1305(kek, master[32])
//!   kek  = PBKDF2-HMAC-SHA256(password, salt, 600_000)
//!
//! Wrapped file lives at ~/.local/share/focaldesk/secrets.key.enc.
//! The unwrapped key is written to $XDG_RUNTIME_DIR/focaldesk/secrets.key
//! (tmpfs, 0600) at login by the PAM module, and dies with the session.

#![forbid(unsafe_code)]

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use rand::RngCore;
use zeroize::{Zeroize, Zeroizing};

const MAGIC: &[u8; 5] = b"FKEY1";
const PBKDF2_ITERS: u32 = 600_000;
pub const WRAPPED_LEN: usize = 5 + 16 + 12 + 32 + 16; // magic+salt+nonce+ct+tag

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

fn kek(password: &[u8], salt: &[u8; 16]) -> Zeroizing<[u8; 32]> {
    let mut out = Zeroizing::new([0u8; 32]);
    pbkdf2::pbkdf2_hmac::<sha2::Sha256>(password, salt, PBKDF2_ITERS, out.as_mut());
    out
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
    let k = kek(password, &salt);
    let cipher = ChaCha20Poly1305::new(k.as_ref().into());
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce), master.as_slice())
        .map_err(|_| Error::Crypto)?;
    let mut out = Vec::with_capacity(WRAPPED_LEN);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Unwrap the master key with the password.
pub fn unwrap(password: &[u8], wrapped: &[u8]) -> Result<Zeroizing<[u8; 32]>, Error> {
    if wrapped.len() != WRAPPED_LEN || &wrapped[..5] != MAGIC {
        return Err(Error::Format);
    }
    let salt: [u8; 16] = wrapped[5..21].try_into().unwrap();
    let nonce = Nonce::from_slice(&wrapped[21..33]);
    let k = kek(password, &salt);
    let cipher = ChaCha20Poly1305::new(k.as_ref().into());
    let mut plain = cipher
        .decrypt(nonce, &wrapped[33..])
        .map_err(|_| Error::BadPassword)?;
    if plain.len() != 32 {
        plain.zeroize();
        return Err(Error::Format);
    }
    let mut master = Zeroizing::new([0u8; 32]);
    master.copy_from_slice(&plain);
    plain.zeroize();
    Ok(master)
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
}
