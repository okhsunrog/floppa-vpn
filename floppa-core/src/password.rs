//! Password hashing for credential (login + password) auth.
//!
//! Uses Argon2id (the `Argon2::default()` variant) with a per-hash random salt,
//! producing a self-describing PHC string stored in `auth_identities.secret_hash`.

use argon2::Argon2;
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};

use crate::error::{FloppaError, Result};

/// Hash a plaintext password into a PHC string (Argon2id, random salt).
pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| FloppaError::Encryption(format!("password hash failed: {e}")))
}

/// Verify a plaintext password against a stored PHC string.
///
/// Fails closed: a malformed/unparseable hash returns `false`, never an error.
pub fn verify_password(password: &str, phc: &str) -> bool {
    match PasswordHash::new(phc) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// A valid Argon2 PHC string, computed once, used for constant-time dummy verification
/// when a login is not found — avoids leaking account existence via response timing.
/// Must be a real (parseable) hash so the Argon2 work actually runs on each call.
fn dummy_hash() -> &'static str {
    static HASH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HASH.get_or_init(|| {
        hash_password("dummy-password-for-constant-time-verify").expect("dummy hash must build")
    })
}

/// Run a verify against the dummy hash purely for timing parity. Result is ignored.
pub fn dummy_verify(password: &str) {
    let _ = verify_password(password, dummy_hash());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_then_verify_roundtrip() {
        let phc = hash_password("correct horse battery staple").unwrap();
        assert!(verify_password("correct horse battery staple", &phc));
    }

    #[test]
    fn wrong_password_fails() {
        let phc = hash_password("s3cret").unwrap();
        assert!(!verify_password("wrong", &phc));
    }

    #[test]
    fn malformed_hash_fails_closed() {
        assert!(!verify_password("whatever", "not-a-phc-string"));
        assert!(!verify_password("whatever", ""));
    }
}
