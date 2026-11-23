//! Shared-key encryption, built on the RustCrypto libraries: the key comes
//! from the passphrase by Argon2id, and each block is sealed with
//! ChaCha20-Poly1305.

use argon2::Argon2;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::ChaCha20Poly1305;
use sha2::{Digest, Sha256};

pub const SALT_LEN: usize = 8;
pub const NONCE_LEN: usize = 4;
pub const TAG_LEN: usize = 16;
/// Bytes added to each encrypted block: salt, counter and authentication tag.
pub const OVERHEAD: usize = SALT_LEN + NONCE_LEN + TAG_LEN;

/// A short fingerprint of some data, for two people to compare by eye.
pub fn fingerprint(data: &[u8]) -> String {
    let hash = Sha256::digest(data);
    format!("{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}", hash[0], hash[1], hash[2], hash[3], hash[4], hash[5])
}

/// The one-byte tag that stands for a receiver's address on the air.
/// Never zero: zero means "anyone".
pub fn address_tag(address: &str) -> u8 {
    1 + Sha256::digest(address.as_bytes())[0] % 255
}

/// Encrypts and decrypts blocks under one passphrase.
///
/// Each block on the air is `salt | counter | ciphertext | tag`. The key is
/// derived from the passphrase and the salt; a sender picks one random salt
/// per run and never reuses a counter with it. A receiver refuses a counter
/// it has already seen under the same salt, so a recording cannot be replayed
/// into the same session.
pub struct Cipher {
    passphrase: Vec<u8>,
    salt: [u8; SALT_LEN],
    key: Option<([u8; SALT_LEN], [u8; 32])>,
    counter: u32,
    seen: Option<([u8; SALT_LEN], u32)>,
}

impl Cipher {
    pub fn new(passphrase: &str) -> Result<Cipher, String> {
        let mut salt = [0u8; SALT_LEN];
        getrandom::fill(&mut salt).map_err(|e| format!("no secure random numbers: {e}"))?;
        Ok(Cipher { passphrase: passphrase.as_bytes().to_vec(), salt, key: None, counter: 0, seen: None })
    }

    fn key_for(&mut self, salt: [u8; SALT_LEN]) -> Result<[u8; 32], &'static str> {
        if let Some((s, k)) = self.key {
            if s == salt {
                return Ok(k);
            }
        }
        let mut salted = *b"vocsend1________";
        salted[8..].copy_from_slice(&salt);
        let mut key = [0u8; 32];
        Argon2::default()
            .hash_password_into(&self.passphrase, &salted, &mut key)
            .map_err(|_| "could not derive the key")?;
        self.key = Some((salt, key));
        Ok(key)
    }

    fn nonce(counter: u32) -> [u8; 12] {
        let mut nonce = [0u8; 12];
        nonce[8..].copy_from_slice(&counter.to_be_bytes());
        nonce
    }

    /// `aad` is bound to the block but not sent: both sides must supply the same.
    pub fn seal(&mut self, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, &'static str> {
        self.counter += 1;
        let key = self.key_for(self.salt)?;
        let cipher = ChaCha20Poly1305::new_from_slice(&key).map_err(|_| "bad key")?;
        let sealed = cipher
            .encrypt(&Self::nonce(self.counter).into(), Payload { msg: plaintext, aad })
            .map_err(|_| "encryption failed")?;
        let mut out = self.salt.to_vec();
        out.extend_from_slice(&self.counter.to_be_bytes());
        out.extend_from_slice(&sealed);
        Ok(out)
    }

    pub fn open(&mut self, block: &[u8], aad: &[u8]) -> Result<Vec<u8>, &'static str> {
        const WRONG: &str = "wrong key, or the data was altered";
        if block.len() < OVERHEAD {
            return Err(WRONG);
        }
        let salt: [u8; SALT_LEN] = block[..SALT_LEN].try_into().unwrap();
        let counter = u32::from_be_bytes(block[SALT_LEN..SALT_LEN + NONCE_LEN].try_into().unwrap());
        let key = self.key_for(salt)?;
        let cipher = ChaCha20Poly1305::new_from_slice(&key).map_err(|_| WRONG)?;
        let plain = cipher
            .decrypt(&Self::nonce(counter).into(), Payload { msg: &block[SALT_LEN + NONCE_LEN..], aad })
            .map_err(|_| WRONG)?;
        if let Some((s, last)) = self.seen {
            if s == salt && counter <= last {
                return Err("this block was already received once: refusing a replay");
            }
        }
        self.seen = Some((salt, counter));
        Ok(plain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_and_open() {
        let mut tx = Cipher::new("correct horse").unwrap();
        let mut rx = Cipher::new("correct horse").unwrap();
        let sealed = tx.seal(b"attack at dawn", b"aad").unwrap();
        assert_eq!(sealed.len(), 14 + OVERHEAD);
        assert_eq!(rx.open(&sealed, b"aad").unwrap(), b"attack at dawn");
        assert!(rx.open(&sealed, b"aad").is_err(), "replay must be refused");
        let second = tx.seal(b"", b"aad").unwrap();
        assert_eq!(rx.open(&second, b"aad").unwrap(), b"");
        let third = tx.seal(b"x", b"aad").unwrap();
        assert!(rx.open(&third, b"other").is_err(), "aad is bound");
        let mut tampered = tx.seal(b"hello", b"").unwrap();
        tampered[SALT_LEN + NONCE_LEN] ^= 1;
        assert!(rx.open(&tampered, b"").is_err());
        let mut wrong = Cipher::new("battery staple").unwrap();
        assert!(wrong.open(&tx.seal(b"hello", b"").unwrap(), b"").is_err());
    }

    #[test]
    fn fingerprints_and_tags() {
        assert_eq!(fingerprint(b"abc"), "ba78-16bf-8f01");
        assert_ne!(fingerprint(b"abc"), fingerprint(b"abd"));
        assert!(address_tag("laptop") != 0 && address_tag("") != 0);
        assert_eq!(address_tag("laptop"), address_tag("laptop"));
    }
}
