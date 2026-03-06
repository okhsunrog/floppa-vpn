//! WireGuard key generation and validation.
//!
//! Uses x25519-dalek for key generation (no external `wg` dependency needed).

use base64::prelude::*;
use x25519_dalek::{PublicKey as X25519Public, StaticSecret};

const WG_KEY_LEN: usize = 32;
const WG_KEY_BASE64_LEN: usize = 44; // 32 bytes -> 44 base64 chars

/// A validated WireGuard private key.
#[derive(Clone, PartialEq, Eq, veil::Redact)]
pub struct PrivateKey(#[redact] String);

/// A validated WireGuard public key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicKey(String);

impl PrivateKey {
    /// Parse and validate a base64-encoded private key.
    pub fn from_base64(s: &str) -> Result<Self, KeyError> {
        validate_key(s)?;
        Ok(Self(s.to_string()))
    }

    /// Get the base64 representation.
    pub fn as_base64(&self) -> &str {
        &self.0
    }
}

impl PublicKey {
    /// Parse and validate a base64-encoded public key.
    pub fn from_base64(s: &str) -> Result<Self, KeyError> {
        validate_key(s)?;
        Ok(Self(s.to_string()))
    }

    /// Get the base64 representation.
    pub fn as_base64(&self) -> &str {
        &self.0
    }
}

fn validate_key(s: &str) -> Result<(), KeyError> {
    let s = s.trim();
    if s.len() != WG_KEY_BASE64_LEN {
        return Err(KeyError::InvalidLength {
            expected: WG_KEY_BASE64_LEN,
            got: s.len(),
        });
    }

    let bytes = BASE64_STANDARD
        .decode(s)
        .map_err(|_| KeyError::InvalidBase64)?;

    if bytes.len() != WG_KEY_LEN {
        return Err(KeyError::InvalidLength {
            expected: WG_KEY_LEN,
            got: bytes.len(),
        });
    }

    Ok(())
}

/// Generate a WireGuard keypair using x25519-dalek.
pub fn generate_keypair() -> Result<(PrivateKey, PublicKey), KeyError> {
    let mut key_bytes = [0u8; 32];
    rand::fill(&mut key_bytes);
    let secret = StaticSecret::from(key_bytes);
    let public = X25519Public::from(&secret);

    let private_b64 = BASE64_STANDARD.encode(secret.as_bytes());
    let public_b64 = BASE64_STANDARD.encode(public.as_bytes());

    let private_key = PrivateKey::from_base64(&private_b64)?;
    let public_key = PublicKey::from_base64(&public_b64)?;

    Ok((private_key, public_key))
}

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("Invalid base64 encoding")]
    InvalidBase64,
    #[error("Invalid key length: expected {expected}, got {got}")]
    InvalidLength { expected: usize, got: usize },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_key() {
        // Valid 32-byte key in base64
        let key = "YWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWE=";
        assert!(PrivateKey::from_base64(key).is_ok());
        assert!(PublicKey::from_base64(key).is_ok());
    }

    #[test]
    fn test_invalid_length() {
        let key = "dG9vc2hvcnQ="; // "tooshort"
        assert!(matches!(
            PrivateKey::from_base64(key),
            Err(KeyError::InvalidLength { .. })
        ));
    }

    #[test]
    fn test_invalid_base64() {
        // 44 chars but invalid base64 (contains invalid characters)
        let key = "!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!=";
        assert!(matches!(
            PrivateKey::from_base64(key),
            Err(KeyError::InvalidBase64)
        ));
    }

    #[test]
    fn test_generate_keypair() {
        let (private, public) = generate_keypair().unwrap();
        // Keys should be valid base64 of correct length
        assert_eq!(private.as_base64().len(), WG_KEY_BASE64_LEN);
        assert_eq!(public.as_base64().len(), WG_KEY_BASE64_LEN);
        // Private and public should be different
        assert_ne!(private.as_base64(), public.as_base64());
    }

    #[test]
    fn test_generate_keypair_deterministic_derivation() {
        // Verify that the public key is correctly derived from private
        let (private, public) = generate_keypair().unwrap();
        let private_bytes = BASE64_STANDARD.decode(private.as_base64()).unwrap();
        let mut key_array = [0u8; 32];
        key_array.copy_from_slice(&private_bytes);
        let secret = x25519_dalek::StaticSecret::from(key_array);
        let derived_public = x25519_dalek::PublicKey::from(&secret);
        let derived_b64 = BASE64_STANDARD.encode(derived_public.as_bytes());
        assert_eq!(public.as_base64(), derived_b64);
    }
}
