//! Cryptographic utilities for encrypting/decrypting sensitive data.
//!
//! Uses ChaCha20-Poly1305 AEAD cipher for encrypting WireGuard private keys.
//! Format: base64(nonce || ciphertext || tag)

use base64::prelude::*;
use chacha20poly1305::{
    AeadCore, ChaCha20Poly1305, Nonce,
    aead::{Aead, KeyInit, OsRng},
};

const NONCE_SIZE: usize = 12;

/// Encrypt a WireGuard private key using ChaCha20-Poly1305.
///
/// Returns base64-encoded string containing nonce + ciphertext.
pub fn encrypt_private_key(
    private_key: &str,
    encryption_key: &[u8; 32],
) -> Result<String, CryptoError> {
    let cipher = ChaCha20Poly1305::new(encryption_key.into());
    let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);

    let ciphertext = cipher
        .encrypt(&nonce, private_key.as_bytes())
        .map_err(|_| CryptoError::EncryptionFailed)?;

    // Prepend nonce to ciphertext
    let mut result = Vec::with_capacity(NONCE_SIZE + ciphertext.len());
    result.extend_from_slice(&nonce);
    result.extend_from_slice(&ciphertext);

    Ok(BASE64_STANDARD.encode(result))
}

/// Decrypt a WireGuard private key.
///
/// Expects base64-encoded string containing nonce + ciphertext.
pub fn decrypt_private_key(
    encrypted: &str,
    encryption_key: &[u8; 32],
) -> Result<String, CryptoError> {
    let data = BASE64_STANDARD
        .decode(encrypted)
        .map_err(|_| CryptoError::InvalidFormat)?;

    if data.len() < NONCE_SIZE + 1 {
        return Err(CryptoError::InvalidFormat);
    }

    let (nonce_bytes, ciphertext) = data.split_at(NONCE_SIZE);
    // `Nonce::from_slice` is deprecated in the generic-array version chacha20poly1305 0.10 pins.
    // The length check above plus `split_at` already guarantee 12 bytes, so this conversion
    // cannot fail — it is written as an error rather than an unwrap so the guarantee stays local.
    let nonce_bytes: [u8; NONCE_SIZE] = nonce_bytes
        .try_into()
        .map_err(|_| CryptoError::InvalidFormat)?;
    let nonce = Nonce::from(nonce_bytes);

    let cipher = ChaCha20Poly1305::new(encryption_key.into());
    let plaintext = cipher
        .decrypt(&nonce, ciphertext)
        .map_err(|_| CryptoError::DecryptionFailed)?;

    String::from_utf8(plaintext).map_err(|_| CryptoError::InvalidFormat)
}

/// Parse a hex-encoded 32-byte encryption key from config.
pub fn parse_encryption_key(hex_key: &str) -> Result<[u8; 32], CryptoError> {
    let hex_key = hex_key.trim();
    let bytes = hex::decode(hex_key).map_err(|_| CryptoError::InvalidFormat)?;
    bytes.try_into().map_err(|_| CryptoError::InvalidKeyLength)
}

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("Encryption failed")]
    EncryptionFailed,
    #[error("Decryption failed - wrong key or corrupted data")]
    DecryptionFailed,
    #[error("Invalid data format")]
    InvalidFormat,
    #[error("Invalid key length - expected 64 hex characters (32 bytes)")]
    InvalidKeyLength,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip() {
        let key = [0x42u8; 32];
        let private_key = "cGxhaW50ZXh0IHByaXZhdGUga2V5"; // base64 WG key format

        let encrypted = encrypt_private_key(private_key, &key).unwrap();
        assert_ne!(encrypted, private_key);

        let decrypted = decrypt_private_key(&encrypted, &key).unwrap();
        assert_eq!(decrypted, private_key);
    }

    #[test]
    fn test_wrong_key_fails() {
        let key1 = [0x42u8; 32];
        let key2 = [0x43u8; 32];
        let private_key = "test_key";

        let encrypted = encrypt_private_key(private_key, &key1).unwrap();
        let result = decrypt_private_key(&encrypted, &key2);
        assert!(result.is_err());
    }

    /// Decrypts a ciphertext produced by an earlier build.
    ///
    /// The round-trip test above proves this module is symmetric with itself, which is exactly
    /// what a change to the stored format would keep true — encrypt and decrypt would move
    /// together and every private key already in the database would quietly stop decrypting.
    /// Only a constant can catch that, so this one was captured under chacha20poly1305 0.10 and
    /// must keep decrypting whatever the crate version becomes.
    #[test]
    fn ciphertext_written_by_an_older_build_still_decrypts() {
        const KEY: [u8; 32] = [0x42u8; 32];
        const CIPHERTEXT: &str =
            "J2Wil9jf4NkR2M9kmVUjiJBSTVNbWYky6cPhzndUa8SqnLA4mHriScBxEXoQOyR2D82sR2SMqXY=";

        assert_eq!(
            decrypt_private_key(CIPHERTEXT, &KEY).unwrap(),
            "cGxhaW50ZXh0IHByaXZhdGUga2V5"
        );
    }

    #[test]
    fn test_parse_encryption_key() {
        let hex = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let key = parse_encryption_key(hex).unwrap();
        assert_eq!(key[0], 0x01);
        assert_eq!(key[1], 0x23);
        assert_eq!(key[31], 0xef);
    }
}
