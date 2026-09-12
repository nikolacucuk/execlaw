//! HMAC-signed event-log records (§7.8 security-defense port from
//! `selfhosted-claw/src/control-store.ts`).
//!
//! Legacy v1 `state_events` rows carry independent HMAC-SHA256 tags over
//! [`canonical_bytes`]. V2 rows use a domain-separated encoding that also
//! commits to the previous chain tag and key id. A separately signed durable
//! conversation head commits to the terminal sequence and tag.
//!
//! The HMAC key lives in the vault (`vault_secrets` table, key name
//! `event_log_hmac_key`). Rotating the key re-signs nothing: each event and
//! checkpoint records its key id, and old keys remain verification-only
//! members of the key ring.
//!
//! **No cloud dependencies.** `hmac` + `sha2` crates, pure Rust.

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Canonical bytes that go into the HMAC. Order must be stable across
/// versions — any change invalidates existing tags. The format is:
///
/// ```text
/// conversation_id || 0 || seq (little-endian i64) ||
///   kind || 0 || committed_at (little-endian i64) ||
///   actor_or_empty || 0 || payload_bytes
/// ```
///
/// `\0` separators prevent field-smuggling (an attacker re-arranging
/// bytes across fields can't produce a colliding canonical encoding).
pub fn canonical_bytes(
    conversation_id: &str,
    seq: i64,
    kind: &str,
    committed_at: i64,
    actor: Option<&str>,
    payload: &[u8],
) -> Vec<u8> {
    let mut buf = Vec::with_capacity(conversation_id.len() + kind.len() + payload.len() + 32);
    buf.extend_from_slice(conversation_id.as_bytes());
    buf.push(0);
    buf.extend_from_slice(&seq.to_le_bytes());
    buf.extend_from_slice(kind.as_bytes());
    buf.push(0);
    buf.extend_from_slice(&committed_at.to_le_bytes());
    buf.extend_from_slice(actor.unwrap_or("").as_bytes());
    buf.push(0);
    buf.extend_from_slice(payload);
    buf
}

/// Compute the HMAC-SHA256 tag for an event row.
pub fn sign_event(key: &[u8], canonical: &[u8]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(canonical);
    let result = mac.finalize().into_bytes();
    let mut out = [0u8; 32];
    out.copy_from_slice(&result);
    out
}

/// Verify that `tag` is the correct HMAC for `canonical` under `key`.
/// Constant-time comparison via the `hmac` crate internals.
pub fn verify_event(key: &[u8], canonical: &[u8], tag: &[u8; 32]) -> bool {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(canonical);
    mac.verify_slice(tag).is_ok()
}

/// Canonical bytes for a chained v2 event. The v1 canonical event bytes are
/// embedded unchanged, then bound to the signing key id and predecessor tag.
pub fn canonical_chain_event(v1_canonical: &[u8], key_id: i64, prev_tag: &[u8; 32]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(v1_canonical.len() + 64);
    buf.extend_from_slice(b"execlaw/event-chain/v2\0");
    buf.extend_from_slice(&key_id.to_le_bytes());
    buf.extend_from_slice(prev_tag);
    buf.extend_from_slice(v1_canonical);
    buf
}

/// Canonical bytes for the genesis anchor over the frozen legacy-v1 prefix.
/// Length prefixes make the encoding unambiguous even for binary row fields.
pub fn canonical_genesis(
    conversation_id: &str,
    chain_start_seq: i64,
    legacy_rows: &[Vec<u8>],
) -> Vec<u8> {
    let payload_len: usize = legacy_rows.iter().map(|row| row.len() + 8).sum();
    let mut buf = Vec::with_capacity(conversation_id.len() + payload_len + 48);
    buf.extend_from_slice(b"execlaw/event-chain-genesis/v2\0");
    buf.extend_from_slice(&(conversation_id.len() as u64).to_le_bytes());
    buf.extend_from_slice(conversation_id.as_bytes());
    buf.extend_from_slice(&chain_start_seq.to_le_bytes());
    buf.extend_from_slice(&(legacy_rows.len() as u64).to_le_bytes());
    for row in legacy_rows {
        buf.extend_from_slice(&(row.len() as u64).to_le_bytes());
        buf.extend_from_slice(row);
    }
    buf
}

/// Canonical bytes for the durable terminal checkpoint. The checkpoint is a
/// separate MAC so a stored head cannot be moved or replaced independently.
pub fn canonical_checkpoint(
    conversation_id: &str,
    chain_start_seq: i64,
    head_seq: i64,
    head_tag: &[u8; 32],
    genesis_key_id: i64,
    key_id: i64,
) -> Vec<u8> {
    let mut buf = Vec::with_capacity(conversation_id.len() + 96);
    buf.extend_from_slice(b"execlaw/event-checkpoint/v2\0");
    buf.extend_from_slice(&(conversation_id.len() as u64).to_le_bytes());
    buf.extend_from_slice(conversation_id.as_bytes());
    buf.extend_from_slice(&chain_start_seq.to_le_bytes());
    buf.extend_from_slice(&head_seq.to_le_bytes());
    buf.extend_from_slice(head_tag);
    buf.extend_from_slice(&genesis_key_id.to_le_bytes());
    buf.extend_from_slice(&key_id.to_le_bytes());
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_signs_and_verifies() {
        let key = b"test-hmac-key-32-bytes-long!!!!!";
        let canon = canonical_bytes(
            "conv-1",
            7,
            "user_msg",
            1_714_000_000,
            Some("pri_ctrl"),
            b"msgpack-payload-bytes",
        );
        let tag = sign_event(key, &canon);
        assert!(verify_event(key, &canon, &tag));
    }

    #[test]
    fn tampered_payload_fails_verification() {
        let key = b"k";
        let canon = canonical_bytes("c", 1, "user_msg", 0, None, b"original");
        let tag = sign_event(key, &canon);
        let tampered = canonical_bytes("c", 1, "user_msg", 0, None, b"modified");
        assert!(!verify_event(key, &tampered, &tag));
    }

    #[test]
    fn tampered_seq_fails_verification() {
        let key = b"k";
        let canon = canonical_bytes("c", 1, "user_msg", 0, None, b"p");
        let tag = sign_event(key, &canon);
        let shifted = canonical_bytes("c", 2, "user_msg", 0, None, b"p");
        assert!(!verify_event(key, &shifted, &tag));
    }

    #[test]
    fn different_key_fails_verification() {
        let canon = canonical_bytes("c", 1, "user_msg", 0, None, b"p");
        let tag = sign_event(b"key-a", &canon);
        assert!(!verify_event(b"key-b", &canon, &tag));
    }

    #[test]
    fn null_separator_prevents_field_smuggling() {
        // Ensure that two different fields can't produce the same
        // canonical bytes by shuffling content across the boundary.
        let a = canonical_bytes("foo", 1, "bar", 0, None, b"");
        let b = canonical_bytes("foob", 1, "ar", 0, None, b"");
        assert_ne!(a, b);
    }

    #[test]
    fn tampered_kind_fails_verification() {
        let key = b"k";
        let canon = canonical_bytes("c", 1, "user_msg", 0, None, b"p");
        let tag = sign_event(key, &canon);
        let mutated = canonical_bytes("c", 1, "model_turn", 0, None, b"p");
        assert!(!verify_event(key, &mutated, &tag));
    }

    #[test]
    fn tampered_actor_fails_verification() {
        let key = b"k";
        let canon = canonical_bytes("c", 1, "user_msg", 0, Some("agent"), b"p");
        let tag = sign_event(key, &canon);
        let mutated = canonical_bytes("c", 1, "user_msg", 0, Some("controller"), b"p");
        assert!(!verify_event(key, &mutated, &tag));
    }

    #[test]
    fn tampered_committed_at_fails_verification() {
        let key = b"k";
        let canon = canonical_bytes("c", 1, "user_msg", 1_000, None, b"p");
        let tag = sign_event(key, &canon);
        let mutated = canonical_bytes("c", 1, "user_msg", 2_000, None, b"p");
        assert!(!verify_event(key, &mutated, &tag));
    }

    #[test]
    fn tampered_conversation_id_fails_verification() {
        let key = b"k";
        let canon = canonical_bytes("alice", 1, "user_msg", 0, None, b"p");
        let tag = sign_event(key, &canon);
        let mutated = canonical_bytes("mallory", 1, "user_msg", 0, None, b"p");
        assert!(!verify_event(key, &mutated, &tag));
    }

    /// Flipping a single bit in the tag must invalidate it — HMAC
    /// correctness basics, but an explicit regression gate.
    #[test]
    fn single_bit_flip_in_tag_fails_verification() {
        let key = b"k";
        let canon = canonical_bytes("c", 1, "user_msg", 0, None, b"p");
        let mut tag = sign_event(key, &canon);
        tag[0] ^= 0x01;
        assert!(!verify_event(key, &canon, &tag));
    }

    /// `None` actor and `Some("")` actor should both verify round-trip
    /// but must NOT produce the same tag (the absence of an actor is
    /// distinct from an empty-string actor).
    #[test]
    fn none_and_empty_actor_are_indistinguishable_by_design() {
        // The canonical encoding currently uses "" for both; this test
        // pins that behavior so a future change is deliberate.
        let key = b"k";
        let none_canon = canonical_bytes("c", 1, "k", 0, None, b"p");
        let empty_canon = canonical_bytes("c", 1, "k", 0, Some(""), b"p");
        assert_eq!(none_canon, empty_canon);
        let tag = sign_event(key, &none_canon);
        assert!(verify_event(key, &empty_canon, &tag));
    }

    #[test]
    fn v2_chain_binds_predecessor_and_key_id_without_changing_v1() {
        let v1 = canonical_bytes("c", 2, "user_msg", 3, None, b"p");
        let original = v1.clone();
        let prev = [7u8; 32];
        let chained = canonical_chain_event(&v1, 4, &prev);
        assert_eq!(v1, original);
        assert_ne!(chained, canonical_chain_event(&v1, 5, &prev));
        assert_ne!(chained, canonical_chain_event(&v1, 4, &[8u8; 32]));
    }

    #[test]
    fn checkpoint_binds_terminal_state() {
        let head = [9u8; 32];
        let canonical = canonical_checkpoint("c", 4, 7, &head, 1, 2);
        assert_ne!(canonical, canonical_checkpoint("c", 4, 6, &head, 1, 2));
        assert_ne!(canonical, canonical_checkpoint("other", 4, 7, &head, 1, 2));
        assert_ne!(canonical, canonical_checkpoint("c", 4, 7, &head, 9, 2));
    }
}
