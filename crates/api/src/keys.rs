//! API-key generation, hashing, and verification.
//!
//! A key has the shape `cell_<8 hex prefix>_<32 hex secret>`. The prefix
//! is stored plaintext as the primary key of the `api_keys` table and is
//! safe to log or display in support tooling. The secret half is hashed
//! with Argon2id at issuance and only the [PHC string] is persisted.
//!
//! [PHC string]: https://github.com/P-H-C/phc-string-format/blob/master/phc-sf-spec.md
//!
//! Verification compares a presented secret against the stored PHC string
//! via [`argon2::Argon2::verify_password`]. Argon2id parameters are tuned
//! to the lower end ("interactive") of the standard recommendations
//! because verification sits on the request hot path and is paired with
//! an in-process cache.

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::SaltString;
use argon2::{Algorithm, Argon2, Params, PasswordHash, PasswordHasher, PasswordVerifier, Version};
use rand::RngCore;
use thiserror::Error;

const KEY_PREFIX_TAG: &str = "cell";
const PREFIX_HEX_LEN: usize = 8;
const SECRET_HEX_LEN: usize = 32;

/// Argon2id parameters used to hash key secrets.
///
/// Memory: 19 MiB, iterations: 2, parallelism: 1. These are the OWASP
/// "minimum" recommendations and verify in roughly 10 ms on commodity
/// CPUs — fast enough for the hot path when paired with the verification
/// cache, slow enough that an attacker who steals the database has to do
/// real work per attempt.
fn argon2_params() -> Params {
    // The unwrap is unreachable: these literal values are valid per the
    // argon2 crate's own internal range checks. The function is fallible
    // only to support runtime-configurable inputs we don't use here.
    #[allow(clippy::expect_used)]
    Params::new(19_456, 2, 1, None).expect("argon2 params (constants are in-range)")
}

fn argon2() -> Argon2<'static> {
    Argon2::new(Algorithm::Argon2id, Version::V0x13, argon2_params())
}

/// A freshly issued key. Returned only at creation time — the full string
/// is shown to the operator once and never persisted.
#[derive(Debug)]
pub struct IssuedKey {
    /// Public identifier, e.g. `cell_a1b2c3d4`. Safe to log or display.
    pub prefix: String,
    /// Secret half, hex-encoded, 32 chars. Combined with the prefix to
    /// form the full key handed to the operator.
    pub secret: String,
    /// Argon2id PHC string of the secret. This is what the database
    /// receives.
    pub secret_hash: String,
    /// Convenience: the full `cell_<prefix>_<secret>` string.
    pub full: String,
}

/// Errors raised by key generation and verification.
#[derive(Debug, Error)]
pub enum KeyError {
    /// Argon2 hashing failed. Surfaces from the `argon2` crate; either a
    /// programming error in our parameters or an extreme OOM condition.
    #[error("argon2 hashing failed: {0}")]
    Hashing(String),
    /// The supplied key string is not in the `cell_<prefix>_<secret>`
    /// shape, or its parts are the wrong length / wrong charset.
    #[error("key format is invalid")]
    BadFormat,
    /// The presented secret did not match the stored hash. Returned for
    /// any verification failure — including malformed PHC strings — so a
    /// caller cannot distinguish "no such key" from "wrong secret".
    #[error("key verification failed")]
    Mismatch,
}

/// Generate a new API key. Returns the prefix, the plaintext secret, and
/// the Argon2id PHC string for storage. The plaintext is the only chance
/// the operator has to record the key — it is not retrievable later.
pub fn generate() -> Result<IssuedKey, KeyError> {
    let mut prefix_bytes = [0u8; PREFIX_HEX_LEN / 2];
    OsRng.fill_bytes(&mut prefix_bytes);
    let mut secret_bytes = [0u8; SECRET_HEX_LEN / 2];
    OsRng.fill_bytes(&mut secret_bytes);

    let prefix_random = ::hex::encode(prefix_bytes);
    let secret = ::hex::encode(secret_bytes);
    let prefix = format!("{KEY_PREFIX_TAG}_{prefix_random}");
    let full = format!("{prefix}_{secret}");

    let salt = SaltString::generate(&mut OsRng);
    let phc = argon2()
        .hash_password(secret.as_bytes(), &salt)
        .map_err(|err| KeyError::Hashing(err.to_string()))?
        .to_string();

    Ok(IssuedKey {
        prefix,
        secret,
        secret_hash: phc,
        full,
    })
}

/// Split a full key string into `(prefix, secret)`. The prefix retains
/// the `cell_<8 hex>` shape; the secret is the trailing 32 hex chars.
///
/// Returns [`KeyError::BadFormat`] if the input does not match the
/// expected shape — caller is responsible for translating that to a 401.
pub fn split(full: &str) -> Result<(&str, &str), KeyError> {
    // Expect exactly two underscores: `cell_<prefix-random>_<secret>`.
    let mut parts = full.splitn(3, '_');
    let tag = parts.next().ok_or(KeyError::BadFormat)?;
    let prefix_random = parts.next().ok_or(KeyError::BadFormat)?;
    let secret = parts.next().ok_or(KeyError::BadFormat)?;

    if tag != KEY_PREFIX_TAG {
        return Err(KeyError::BadFormat);
    }
    if prefix_random.len() != PREFIX_HEX_LEN
        || !prefix_random.chars().all(|c| c.is_ascii_hexdigit())
    {
        return Err(KeyError::BadFormat);
    }
    if secret.len() != SECRET_HEX_LEN || !secret.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(KeyError::BadFormat);
    }
    // Slice the original `full` so the prefix portion includes the tag.
    let prefix_end = tag.len() + 1 + prefix_random.len();
    Ok((&full[..prefix_end], &full[prefix_end + 1..]))
}

/// Verify a presented secret against the stored PHC hash. Constant-ish
/// time inside `argon2` — does not leak timing information about hash
/// internals.
pub fn verify(secret: &str, phc: &str) -> Result<(), KeyError> {
    let parsed = PasswordHash::new(phc).map_err(|_| KeyError::Mismatch)?;
    argon2()
        .verify_password(secret.as_bytes(), &parsed)
        .map_err(|_| KeyError::Mismatch)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn generated_key_round_trips() {
        let issued = generate().unwrap();
        assert!(issued.prefix.starts_with("cell_"));
        assert_eq!(issued.prefix.len(), 5 + PREFIX_HEX_LEN);
        assert_eq!(issued.secret.len(), SECRET_HEX_LEN);
        assert_eq!(issued.full.len(), 5 + PREFIX_HEX_LEN + 1 + SECRET_HEX_LEN);

        let (prefix, secret) = split(&issued.full).unwrap();
        assert_eq!(prefix, issued.prefix);
        assert_eq!(secret, issued.secret);

        verify(&issued.secret, &issued.secret_hash).unwrap();
    }

    #[test]
    fn each_call_yields_distinct_keys() {
        let a = generate().unwrap();
        let b = generate().unwrap();
        assert_ne!(a.prefix, b.prefix);
        assert_ne!(a.secret, b.secret);
        assert_ne!(a.secret_hash, b.secret_hash);
    }

    #[test]
    fn verify_rejects_wrong_secret() {
        let issued = generate().unwrap();
        let other = generate().unwrap();
        assert!(matches!(
            verify(&other.secret, &issued.secret_hash),
            Err(KeyError::Mismatch)
        ));
    }

    #[test]
    fn split_rejects_bad_shapes() {
        assert!(matches!(split(""), Err(KeyError::BadFormat)));
        assert!(matches!(split("cell"), Err(KeyError::BadFormat)));
        assert!(matches!(split("cell_aaaa"), Err(KeyError::BadFormat)));
        assert!(matches!(
            split("cell_aaaaaaaa_zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"),
            Err(KeyError::BadFormat)
        ));
        assert!(matches!(
            split("xxxx_aaaaaaaa_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            Err(KeyError::BadFormat)
        ));
        assert!(matches!(
            split("cell_aaaaaaa_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            Err(KeyError::BadFormat)
        ));
    }
}
