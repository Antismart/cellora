//! Static well-known-script lookup.
//!
//! Each [`WellKnownScript`] entry pairs a `(code_hash, hash_type)` with
//! a short label and the network it applies to. Lookups are performed
//! by linear scan over a small `&'static [WellKnownScript]` table —
//! the table is on the order of ten entries, so a hashmap would just
//! be ceremony.
//!
//! Sources, copied verbatim with citations:
//! - <https://github.com/nervosnetwork/ckb-system-scripts>
//! - <https://explorer.nervos.org/scripts>
//!
//! New entries should arrive via PR per ADR 0005. Treat this file as
//! API surface — a label like `"sighash"` is consumed by clients.

use cellora_common::config::Network;

/// Whether an entry applies to a lock script, a type script, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScriptKind {
    /// Tag eligible only when the script appears in a cell's `lock`.
    Lock,
    /// Tag eligible only when the script appears in a cell's `type`.
    Type,
    /// Tag eligible whether the script is the lock or the type.
    Either,
}

/// A single well-known script entry.
#[derive(Debug, Clone, Copy)]
pub struct WellKnownScript {
    /// 32-byte `code_hash` of the script.
    pub code_hash: [u8; 32],
    /// CKB `hash_type`, encoded as the SMALLINT we store in the
    /// `cells` table: 0 = data, 1 = type, 2 = data1, 3 = data2.
    pub hash_type: i16,
    /// Network the entry applies to.
    pub network: Network,
    /// Slot the entry can tag.
    pub kind: ScriptKind,
    /// Stable, lower-case label the API surfaces (e.g. `"sighash"`).
    pub label: &'static str,
}

/// Find the canonical label for `(code_hash, hash_type)` on `network`,
/// when the script appears in `slot` (lock or type). Returns `None` when
/// the pair is not in the registry — the API then omits the
/// `lock_kind` / `type_kind` field rather than emitting a placeholder.
pub fn lookup(
    network: Network,
    code_hash: &[u8],
    hash_type: i16,
    slot: ScriptSlot,
) -> Option<&'static str> {
    if code_hash.len() != 32 {
        return None;
    }
    REGISTRY
        .iter()
        .find(|entry| {
            entry.network == network
                && entry.hash_type == hash_type
                && entry.code_hash == code_hash[..]
                && match entry.kind {
                    ScriptKind::Either => true,
                    ScriptKind::Lock => matches!(slot, ScriptSlot::Lock),
                    ScriptKind::Type => matches!(slot, ScriptSlot::Type),
                }
        })
        .map(|entry| entry.label)
}

/// Which slot of a cell is being looked up. Lets the registry encode
/// scripts that can only legitimately appear in one of the two
/// positions (e.g. NervosDAO is always a type script).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptSlot {
    /// Cell's `lock` slot.
    Lock,
    /// Cell's `type` slot.
    Type,
}

// Convenience: parse a 0x-prefixed hex literal into a 32-byte array at
// const-eval time. Keeps the table below readable.
const fn h(literal: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let mut i = 0;
    let mut j = 2; // skip "0x"
    while i < 32 {
        out[i] = (hex_nibble(literal[j]) << 4) | hex_nibble(literal[j + 1]);
        i += 1;
        j += 2;
    }
    out
}

const fn hex_nibble(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => 0xff, // unreachable for the literals below
    }
}

// SMALLINT encodings of HashType, mirroring `cellora_db::models::HashType::as_i16`.
// Only `HT_TYPE` is referenced by the table today; the others are kept
// for symmetry and so future entries can adopt them without redefining
// the constants.
#[allow(dead_code)]
const HT_DATA: i16 = 0;
const HT_TYPE: i16 = 1;
#[allow(dead_code)]
const HT_DATA1: i16 = 2;
#[allow(dead_code)]
const HT_DATA2: i16 = 3;

/// The full table. Each entry copies the canonical hash from one of
/// the two source URLs at the top of the module — bumping any value
/// is a deliberate, reviewable change rather than a transparent
/// dependency on an upstream's wire format.
const REGISTRY: &[WellKnownScript] = &[
    // Sighash (default lock) — mainnet / testnet share the same
    // code_hash because it is a system script.
    WellKnownScript {
        code_hash: h(b"0x9bd7e06f3ecf4be0f2fcd2188b23f1b9fcc88e5d4b65a8637b17723bbda3cce8"),
        hash_type: HT_TYPE,
        network: Network::Mainnet,
        kind: ScriptKind::Lock,
        label: "sighash",
    },
    WellKnownScript {
        code_hash: h(b"0x9bd7e06f3ecf4be0f2fcd2188b23f1b9fcc88e5d4b65a8637b17723bbda3cce8"),
        hash_type: HT_TYPE,
        network: Network::Testnet,
        kind: ScriptKind::Lock,
        label: "sighash",
    },
    // MultiSig.
    WellKnownScript {
        code_hash: h(b"0x5c5069eb0857efc65e1bca0c07df34c31663b3622fd3876c876320fc9634e2a8"),
        hash_type: HT_TYPE,
        network: Network::Mainnet,
        kind: ScriptKind::Lock,
        label: "multisig",
    },
    WellKnownScript {
        code_hash: h(b"0x5c5069eb0857efc65e1bca0c07df34c31663b3622fd3876c876320fc9634e2a8"),
        hash_type: HT_TYPE,
        network: Network::Testnet,
        kind: ScriptKind::Lock,
        label: "multisig",
    },
    // Nervos DAO — only ever a type script.
    WellKnownScript {
        code_hash: h(b"0x82d76d1b75fe2fd9a27dfbaa65a039221a380d76c926f378d3f81cf3e7e13f2e"),
        hash_type: HT_TYPE,
        network: Network::Mainnet,
        kind: ScriptKind::Type,
        label: "nervos_dao",
    },
    WellKnownScript {
        code_hash: h(b"0x82d76d1b75fe2fd9a27dfbaa65a039221a380d76c926f378d3f81cf3e7e13f2e"),
        hash_type: HT_TYPE,
        network: Network::Testnet,
        kind: ScriptKind::Type,
        label: "nervos_dao",
    },
    // Additional well-known scripts (xUDT, Spore, Omnilock, RGB++,
    // Nostr binding) should land here via PR per ADR 0005, with a
    // citation to the explorer.nervos.org/scripts entry on the same
    // commit so reviewers can verify the code_hash.
];

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn registry_has_no_duplicate_entries() {
        let mut seen = HashSet::new();
        for entry in REGISTRY {
            let key = (entry.network, entry.hash_type, entry.code_hash, entry.kind);
            assert!(
                seen.insert(key),
                "duplicate registry entry: {:?}",
                entry.label
            );
        }
    }

    #[test]
    fn lookup_finds_sighash_on_mainnet_lock() {
        let sighash = h(b"0x9bd7e06f3ecf4be0f2fcd2188b23f1b9fcc88e5d4b65a8637b17723bbda3cce8");
        let label = lookup(Network::Mainnet, &sighash, HT_TYPE, ScriptSlot::Lock);
        assert_eq!(label, Some("sighash"));
    }

    #[test]
    fn lookup_does_not_match_sighash_in_type_slot() {
        // Sighash is registered as Lock-only — it would be unusual to
        // see it in the type slot, and tagging it there would be
        // misleading.
        let sighash = h(b"0x9bd7e06f3ecf4be0f2fcd2188b23f1b9fcc88e5d4b65a8637b17723bbda3cce8");
        let label = lookup(Network::Mainnet, &sighash, HT_TYPE, ScriptSlot::Type);
        assert!(label.is_none());
    }

    #[test]
    fn lookup_returns_none_on_devnet_for_mainnet_only_entries() {
        let sighash = h(b"0x9bd7e06f3ecf4be0f2fcd2188b23f1b9fcc88e5d4b65a8637b17723bbda3cce8");
        assert!(lookup(Network::Devnet, &sighash, HT_TYPE, ScriptSlot::Lock).is_none());
    }

    #[test]
    fn lookup_returns_none_for_unknown_code_hash() {
        let custom = [0xCC; 32];
        assert!(lookup(Network::Mainnet, &custom, HT_TYPE, ScriptSlot::Lock).is_none());
    }

    #[test]
    fn lookup_returns_none_when_hash_type_disagrees() {
        let sighash = h(b"0x9bd7e06f3ecf4be0f2fcd2188b23f1b9fcc88e5d4b65a8637b17723bbda3cce8");
        assert!(lookup(Network::Mainnet, &sighash, HT_DATA, ScriptSlot::Lock).is_none());
        assert!(lookup(Network::Mainnet, &sighash, HT_DATA1, ScriptSlot::Lock).is_none());
    }

    #[test]
    fn lookup_returns_none_for_wrong_length_code_hash() {
        assert!(lookup(Network::Mainnet, &[0u8; 31], HT_TYPE, ScriptSlot::Lock).is_none());
    }
}
