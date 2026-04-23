//! Wire-format helpers for byte slices.
//!
//! CKB stores hashes as raw bytes (32 bytes for block / transaction / script
//! hashes, variable for cell data and script args). The HTTP API renders
//! everything as `0x`-prefixed lowercase hex, matching the convention used
//! throughout the CKB ecosystem.
//!
//! [`Hex32`] wraps a fixed 32-byte buffer and serialises with the `0x`
//! prefix on the way out. It does not currently accept client input — the
//! Week 2 endpoints all take hashes via query strings, which are parsed by
//! a separate decode helper in the routes layer.

use serde::{Serialize, Serializer};

use crate::error::ApiError;

/// A 32-byte CKB hash (block / tx / script hash, DAO field).
///
/// Serialises as a `0x`-prefixed lowercase hex string and fails to construct
/// from any slice that is not exactly 32 bytes long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hex32([u8; 32]);

impl Hex32 {
    /// Construct from a raw 32-byte buffer.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrow the underlying bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Try to construct from a slice that must be exactly 32 bytes.
    /// Returns [`ApiError::Internal`] when the slice is the wrong length —
    /// this path is only reached on a database row that violates the
    /// schema's expected width, which is a bug, not a client error.
    pub fn try_from_slice(slice: &[u8]) -> Result<Self, ApiError> {
        let bytes: [u8; 32] = slice.try_into().map_err(|_| {
            ApiError::Internal(anyhow::anyhow!(
                "expected 32-byte hash, got {} bytes",
                slice.len()
            ))
        })?;
        Ok(Self(bytes))
    }
}

impl From<[u8; 32]> for Hex32 {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl Serialize for Hex32 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // "0x" + 64 hex chars
        let mut buf = String::with_capacity(66);
        buf.push_str("0x");
        for byte in self.0 {
            use std::fmt::Write;
            // write! on a String never fails
            let _ = write!(&mut buf, "{byte:02x}");
        }
        serializer.serialize_str(&buf)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn serializes_as_prefixed_lowercase_hex() {
        let hex = Hex32::new([
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb,
            0xcc, 0xdd, 0xee, 0xff,
        ]);
        let json = serde_json::to_string(&hex).expect("serialize");
        assert_eq!(
            json,
            "\"0x00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff\""
        );
    }

    #[test]
    fn try_from_slice_rejects_wrong_length() {
        assert!(Hex32::try_from_slice(&[0; 31]).is_err());
        assert!(Hex32::try_from_slice(&[0; 33]).is_err());
        assert!(Hex32::try_from_slice(&[0; 32]).is_ok());
    }
}
