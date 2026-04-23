//! Opaque cursors for list endpoints.
//!
//! Cursors are serialised as base64url-encoded JSON. The wire format is an
//! implementation detail — clients treat cursors as opaque strings and pass
//! them back verbatim on the next request. Tampered or malformed cursors
//! produce a 400 `invalid_cursor` response rather than a 500, so clients
//! can recover by dropping the bad cursor and starting a new scan.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use cellora_db::cells::CellCursor;
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::hex;

/// Wire shape of a cells cursor. Field names are short to keep the encoded
/// form compact, but the shape is still structured JSON so we can evolve
/// it in a backward-compatible way if necessary.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CellsCursorWire {
    bn: i64,
    tx: String,
    oi: i32,
}

/// Encode a [`CellCursor`] as the opaque string clients pass back.
pub fn encode_cells_cursor(cursor: &CellCursor) -> String {
    let wire = CellsCursorWire {
        bn: cursor.block_number,
        tx: format!("0x{}", ::hex::encode(&cursor.tx_hash)),
        oi: cursor.output_index,
    };
    // `to_vec` on a struct of primitive fields is infallible.
    let json = serde_json::to_vec(&wire).unwrap_or_default();
    URL_SAFE_NO_PAD.encode(json)
}

/// Decode a cells cursor previously produced by [`encode_cells_cursor`].
/// Returns [`ApiError::InvalidCursor`] on any malformed input.
pub fn decode_cells_cursor(raw: &str) -> Result<CellCursor, ApiError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| ApiError::InvalidCursor("cursor is not valid base64url"))?;
    let wire: CellsCursorWire = serde_json::from_slice(&bytes)
        .map_err(|_| ApiError::InvalidCursor("cursor is not well-formed"))?;
    let tx_hash = hex::decode_prefixed(&wire.tx)
        .ok_or(ApiError::InvalidCursor("cursor tx_hash is not valid hex"))?;
    if tx_hash.len() != 32 {
        return Err(ApiError::InvalidCursor(
            "cursor tx_hash must be exactly 32 bytes",
        ));
    }
    Ok(CellCursor {
        block_number: wire.bn,
        tx_hash,
        output_index: wire.oi,
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn fixture() -> CellCursor {
        CellCursor {
            block_number: 123,
            tx_hash: vec![0xab; 32],
            output_index: 4,
        }
    }

    #[test]
    fn round_trip() {
        let original = fixture();
        let encoded = encode_cells_cursor(&original);
        let decoded = decode_cells_cursor(&encoded).unwrap();
        assert_eq!(decoded.block_number, original.block_number);
        assert_eq!(decoded.tx_hash, original.tx_hash);
        assert_eq!(decoded.output_index, original.output_index);
    }

    #[test]
    fn encoded_form_is_url_safe_and_unpadded() {
        let encoded = encode_cells_cursor(&fixture());
        assert!(!encoded.contains('='));
        assert!(!encoded.contains('+'));
        assert!(!encoded.contains('/'));
    }

    #[test]
    fn rejects_garbage_input() {
        let err = decode_cells_cursor("!!!not-base64!!!").unwrap_err();
        assert!(matches!(err, ApiError::InvalidCursor(_)));
    }

    #[test]
    fn rejects_well_formed_base64_but_wrong_json() {
        let encoded = URL_SAFE_NO_PAD.encode(b"not the expected json");
        let err = decode_cells_cursor(&encoded).unwrap_err();
        assert!(matches!(err, ApiError::InvalidCursor(_)));
    }

    #[test]
    fn rejects_wrong_tx_hash_length() {
        let wire = CellsCursorWire {
            bn: 1,
            tx: "0xdeadbeef".to_owned(),
            oi: 0,
        };
        let json = serde_json::to_vec(&wire).unwrap();
        let encoded = URL_SAFE_NO_PAD.encode(json);
        let err = decode_cells_cursor(&encoded).unwrap_err();
        assert!(matches!(err, ApiError::InvalidCursor(_)));
    }
}
