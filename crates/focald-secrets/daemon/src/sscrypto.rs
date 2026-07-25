//! Session crypto for the org.freedesktop.secrets `dh-ietf1024-sha256-aes128-cbc-pkcs7`
//! algorithm, wire-compatible with libsecret / gnome-keyring / oo7:
//!
//! * DH over the 1024-bit MODP group from RFC 2409 (Second Oakley Group), g = 2
//! * shared secret left-padded to 128 bytes, then HKDF-SHA256 (no salt, no info) -> 16-byte AES key
//! * secrets encrypted with AES-128-CBC + PKCS#7; the Secret's `parameters` field is the 16-byte IV

use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use hkdf::Hkdf;
use num_bigint::BigUint;
use num_traits::FromPrimitive;
use rand::RngCore;
use sha2::Sha256;
use zeroize::{Zeroize, ZeroizeOnDrop};

type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;
type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

/// RFC 2409, section 6.2 — 1024-bit MODP prime.
const RFC2409_GROUP2_PRIME: [u8; 128] = [
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xC9, 0x0F, 0xDA, 0xA2, 0x21, 0x68, 0xC2, 0x34,
    0xC4, 0xC6, 0x62, 0x8B, 0x80, 0xDC, 0x1C, 0xD1, 0x29, 0x02, 0x4E, 0x08, 0x8A, 0x67, 0xCC, 0x74,
    0x02, 0x0B, 0xBE, 0xA6, 0x3B, 0x13, 0x9B, 0x22, 0x51, 0x4A, 0x08, 0x79, 0x8E, 0x34, 0x04, 0xDD,
    0xEF, 0x95, 0x19, 0xB3, 0xCD, 0x3A, 0x43, 0x1B, 0x30, 0x2B, 0x0A, 0x6D, 0xF2, 0x5F, 0x14, 0x37,
    0x4F, 0xE1, 0x35, 0x6D, 0x6D, 0x51, 0xC2, 0x45, 0xE4, 0x85, 0xB5, 0x76, 0x62, 0x5E, 0x7E, 0xC6,
    0xF4, 0x4C, 0x42, 0xE9, 0xA6, 0x37, 0xED, 0x6B, 0x0B, 0xFF, 0x5C, 0xB6, 0xF4, 0x06, 0xB7, 0xED,
    0xEE, 0x38, 0x6B, 0xFB, 0x5A, 0x89, 0x9F, 0xA5, 0xAE, 0x9F, 0x24, 0x11, 0x7C, 0x4B, 0x1F, 0xE6,
    0x49, 0x28, 0x66, 0x51, 0xEC, 0xE6, 0x53, 0x81, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
];

pub const ALG_PLAIN: &str = "plain";
pub const ALG_DH: &str = "dh-ietf1024-sha256-aes128-cbc-pkcs7";

/// Per-session key material. `None` => plain session.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SessionCipher {
    aes_key: Option<[u8; 16]>,
}

impl SessionCipher {
    pub fn plain() -> Self {
        SessionCipher { aes_key: None }
    }

    /// Perform our half of the DH exchange. Returns (cipher, our_public_key_bytes).
    pub fn dh(mut client_public: &[u8]) -> Result<(Self, Vec<u8>), String> {
        // Some libsecret/GnuPG MPI versions preserve a sign-padding zero when
        // the high bit is set, producing a 129-byte representation of this
        // 128-byte group element. Leading zeroes do not change the integer.
        while client_public.len() > 128 && client_public.first() == Some(&0) {
            client_public = &client_public[1..];
        }
        if client_public.is_empty() || client_public.len() > 128 {
            return Err("client public key must be 1..=128 bytes".into());
        }
        let p = BigUint::from_bytes_be(&RFC2409_GROUP2_PRIME);
        let g = BigUint::from_u32(2).unwrap();
        let client_pub = BigUint::from_bytes_be(client_public);
        if client_pub < BigUint::from_u32(2).unwrap() || client_pub >= p {
            return Err("client public key out of range".into());
        }

        let mut priv_bytes = [0u8; 128];
        rand::rngs::OsRng.fill_bytes(&mut priv_bytes);
        let priv_key = BigUint::from_bytes_be(&priv_bytes);
        priv_bytes.zeroize();

        let our_pub = g.modpow(&priv_key, &p);
        let shared = client_pub.modpow(&priv_key, &p);

        // Left-pad shared secret to the prime length (128 bytes) — this matches
        // libsecret's egg-dh and the oo7/secret-service crates. Getting this
        // wrong breaks interop ~0.4% of the time (leading zero bytes).
        let mut ikm = [0u8; 128];
        let sb = shared.to_bytes_be();
        ikm[128 - sb.len()..].copy_from_slice(&sb);

        let hk = Hkdf::<Sha256>::new(None, &ikm);
        ikm.zeroize();
        let mut okm = [0u8; 16];
        hk.expand(&[], &mut okm).map_err(|e| format!("hkdf: {e}"))?;

        Ok((SessionCipher { aes_key: Some(okm) }, our_pub.to_bytes_be()))
    }

    /// Encrypt a secret value for the wire. Returns (parameters/IV, ciphertext).
    pub fn encrypt(&self, plaintext: &[u8]) -> (Vec<u8>, Vec<u8>) {
        match &self.aes_key {
            None => (Vec::new(), plaintext.to_vec()),
            Some(key) => {
                let mut iv = [0u8; 16];
                rand::rngs::OsRng.fill_bytes(&mut iv);
                let ct = Aes128CbcEnc::new(key.into(), &iv.into())
                    .encrypt_padded_vec_mut::<Pkcs7>(plaintext);
                (iv.to_vec(), ct)
            }
        }
    }

    /// Decrypt a secret value from the wire.
    pub fn decrypt(&self, params: &[u8], value: &[u8]) -> Result<Vec<u8>, String> {
        match &self.aes_key {
            None => Ok(value.to_vec()),
            Some(key) => {
                if params.len() != 16 {
                    return Err("expected 16-byte IV in secret parameters".into());
                }
                let iv: [u8; 16] = params.try_into().unwrap();
                Aes128CbcDec::new(key.into(), &iv.into())
                    .decrypt_padded_vec_mut::<Pkcs7>(value)
                    .map_err(|_| "secret decryption failed (bad padding)".into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SessionCipher, RFC2409_GROUP2_PRIME};

    #[test]
    fn accepts_mpi_sign_padding() {
        let mut padded = Vec::with_capacity(129);
        padded.push(0);
        // p - 1 is a valid group-range value and has its high bit set.
        let mut public = RFC2409_GROUP2_PRIME;
        *public.last_mut().unwrap() -= 1;
        padded.extend_from_slice(&public);
        assert!(SessionCipher::dh(&padded).is_ok());
    }

    #[test]
    fn rejects_nonzero_overlong_public_key() {
        assert!(SessionCipher::dh(&[1; 129]).is_err());
    }
}
