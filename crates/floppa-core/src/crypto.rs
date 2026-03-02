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
    let nonce = Nonce::from_slice(nonce_bytes);

    let cipher = ChaCha20Poly1305::new(encryption_key.into());
    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| CryptoError::DecryptionFailed)?;

    String::from_utf8(plaintext).map_err(|_| CryptoError::InvalidFormat)
}

/// Parse a hex-encoded 32-byte encryption key from config.
pub fn parse_encryption_key(hex_key: &str) -> Result<[u8; 32], CryptoError> {
    let hex_key = hex_key.trim();
    if hex_key.len() != 64 {
        return Err(CryptoError::InvalidKeyLength);
    }

    let mut key = [0u8; 32];
    for (i, chunk) in hex_key.as_bytes().chunks(2).enumerate() {
        let hex_str = std::str::from_utf8(chunk).map_err(|_| CryptoError::InvalidFormat)?;
        key[i] = u8::from_str_radix(hex_str, 16).map_err(|_| CryptoError::InvalidFormat)?;
    }
    Ok(key)
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

    #[test]
    fn test_parse_encryption_key() {
        let hex = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let key = parse_encryption_key(hex).unwrap();
        assert_eq!(key[0], 0x01);
        assert_eq!(key[1], 0x23);
        assert_eq!(key[31], 0xef);
    }
}
