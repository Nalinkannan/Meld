//! Deterministic canonical serialization and content addressing.
//!
//! The same logical state must produce the same hash when extracted
//! independently, provided the scope, consistency rules, normalization rules,
//! and canonicalization version are identical.
//!
//! Design contract:
//! - Stable ordering for map keys and fields whose order is semantically
//!   irrelevant.
//! - Semantically significant order is preserved: PostgreSQL column ordinal
//!   order, index key order, array order.
//! - BSON type information is preserved.
//! - Timestamps, numerics, and other values are normalized per documented
//!   rules.
//! - Credentials and secrets are never hashed into committed state.
//! - Format/version tags and domain separation separate state, object, and
//!   commit hashes.

use crate::ids::{Hash, hash_state_bytes};
use indexmap::IndexMap;

/// Current canonical format version. Increment whenever the canonical byte
/// format changes in a non-backward-compatible way.
pub const CANONICAL_FORMAT_VERSION: u32 = 1;

/// Domain separation prefixes so that hashes for different purposes never
/// collide even if the byte inputs are identical.
pub const DOMAIN_STATE: &str = "meld-state-v1";
pub const DOMAIN_OBJECT: &str = "meld-object-v1";
pub const DOMAIN_COMMIT: &str = "meld-commit-v1";
pub const DOMAIN_DIFF: &str = "meld-diff-v1";

/// Types that can be serialized to a deterministic canonical byte form.
pub trait Canonicalize {
    /// Append the canonical byte representation of `self` to `out`.
    fn canonicalize(&self, out: &mut Vec<u8>);

    /// Return the canonical byte representation of `self`.
    fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.canonicalize(&mut out);
        out
    }
}

/// Compute the canonical byte representation of a value.
pub fn canonical_bytes<T: Canonicalize>(value: &T) -> Vec<u8> {
    value.canonical_bytes()
}

/// Compute the content hash of a canonical-serializable state value.
pub fn hash_state<T: Canonicalize>(value: &T) -> Hash {
    hash_state_bytes(DOMAIN_STATE, &value.canonical_bytes())
}

/// Compute the content hash of an opaque object.
pub fn hash_object(bytes: &[u8]) -> Hash {
    hash_state_bytes(DOMAIN_OBJECT, bytes)
}

/// Compute the content hash of a commit.
pub fn hash_commit(bytes: &[u8]) -> Hash {
    hash_state_bytes(DOMAIN_COMMIT, bytes)
}

// ---------------------------------------------------------------------------
// Low-level deterministic writer
// ---------------------------------------------------------------------------

/// Append a length-prefixed byte string (length as u64 big-endian).
pub fn write_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.push(0x42); // type tag: raw bytes
    write_u64(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

/// Append a UTF-8 string with length prefix.
pub fn write_str(out: &mut Vec<u8>, s: &str) {
    out.push(0x53); // type tag: string
    write_u64(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

/// Append a u64 in big-endian (fixed width, deterministic).
pub fn write_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_be_bytes());
}

/// Append a u32 in big-endian.
pub fn write_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

/// Append an i32 in big-endian (two's complement).
pub fn write_i32(out: &mut Vec<u8>, value: i32) {
    out.extend_from_slice(&value.to_be_bytes());
}

/// Append an i64 in big-endian (two's complement).
pub fn write_i64(out: &mut Vec<u8>, value: i64) {
    out.extend_from_slice(&value.to_be_bytes());
}

/// Append a boolean.
pub fn write_bool(out: &mut Vec<u8>, value: bool) {
    out.push(if value { 0x01 } else { 0x00 });
}

/// Append a null marker.
pub fn write_null(out: &mut Vec<u8>) {
    out.push(0x00);
}

/// Append a map with keys sorted lexicographically by their canonical bytes.
pub fn write_sorted_map<K: Canonicalize + ?Sized, V: Canonicalize + ?Sized>(
    out: &mut Vec<u8>,
    entries: &[(&K, &V)],
) {
    out.push(0x4d); // type tag: map
    write_u64(out, entries.len() as u64);
    let mut ordered: Vec<(&K, &V)> = entries.to_vec();
    ordered.sort_by(|a, b| {
        let ak = a.0.canonical_bytes();
        let bk = b.0.canonical_bytes();
        ak.cmp(&bk)
    });
    for (k, v) in ordered {
        k.canonicalize(out);
        v.canonicalize(out);
    }
}

/// Blanket implementation so references canonicalize like their referent.
impl<T: Canonicalize + ?Sized> Canonicalize for &T {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        (**self).canonicalize(out);
    }
}

/// Append a sequence preserving element order.
pub fn write_sequence<V: Canonicalize>(out: &mut Vec<u8>, elements: &[V]) {
    out.push(0x5b); // type tag: sequence
    write_u64(out, elements.len() as u64);
    for e in elements {
        e.canonicalize(out);
    }
}

// ---------------------------------------------------------------------------
// Primitive implementations
// ---------------------------------------------------------------------------

impl Canonicalize for u8 {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        out.push(0x30);
        out.push(*self);
    }
}

impl Canonicalize for u64 {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        out.push(0x31);
        write_u64(out, *self);
    }
}

impl Canonicalize for i64 {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        out.push(0x32);
        // Sign-preserving big-endian: reinterpret two's-complement as u64.
        write_u64(out, u64::from_be_bytes(self.to_be_bytes()));
    }
}

impl Canonicalize for bool {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        write_bool(out, *self);
    }
}

impl Canonicalize for str {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        write_str(out, self);
    }
}

impl Canonicalize for String {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        write_str(out, self);
    }
}

impl<T: Canonicalize> Canonicalize for Vec<T> {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        write_sequence(out, self);
    }
}

impl<T: Canonicalize + Ord> Canonicalize for std::collections::BTreeMap<String, T> {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        let entries: Vec<(&String, &T)> = self.iter().collect();
        write_sorted_map(out, &entries);
    }
}

impl<K: Canonicalize + Ord + std::hash::Hash, V: Canonicalize> Canonicalize for IndexMap<K, V> {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        let mut entries: Vec<(&K, &V)> = self.iter().collect();
        entries.sort_by_key(|a| a.0.canonical_bytes());
        write_sorted_map(out, &entries);
    }
}

impl<T: Canonicalize> Canonicalize for Option<T> {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        match self {
            Some(v) => {
                out.push(0x01);
                v.canonicalize(out);
            }
            None => write_null(out),
        }
    }
}

impl Canonicalize for () {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        write_null(out);
    }
}

impl Canonicalize for u32 {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        out.push(0x33);
        write_u32(out, *self);
    }
}

// ---------------------------------------------------------------------------
// Chrono / uuid support
// ---------------------------------------------------------------------------

impl Canonicalize for chrono::DateTime<chrono::Utc> {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        // Normalize to RFC3339 UTC with sub-second precision preserved.
        out.push(0x44); // type tag: datetime
        write_str(
            out,
            &self.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        );
    }
}

impl Canonicalize for uuid::Uuid {
    fn canonicalize(&self, out: &mut Vec<u8>) {
        out.push(0x55); // type tag: uuid
        write_bytes(out, self.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;

    #[derive(Clone)]
    struct Wrap(IndexMap<String, u64>);
    impl Canonicalize for Wrap {
        fn canonicalize(&self, out: &mut Vec<u8>) {
            let entries: Vec<(&String, &u64)> = self.0.iter().collect();
            write_sorted_map(out, &entries);
        }
    }

    #[test]
    fn canonical_bytes_deterministic_under_reordering() {
        let mut a = IndexMap::new();
        a.insert("z".to_string(), 1u64);
        a.insert("a".to_string(), 2u64);
        let mut b = IndexMap::new();
        b.insert("a".to_string(), 2u64);
        b.insert("z".to_string(), 1u64);
        let wa = Wrap(a.clone());
        let wb = Wrap(b.clone());
        assert_eq!(wa.canonical_bytes(), wb.canonical_bytes());
        assert_eq!(a.canonical_bytes(), b.canonical_bytes());
    }

    #[test]
    fn canonical_sequence_preserves_order() {
        let x = vec![1u64, 2u64, 3u64];
        let y = vec![3u64, 2u64, 1u64];
        assert_ne!(x.canonical_bytes(), y.canonical_bytes());
    }

    #[test]
    fn hash_stability_across_extraction() {
        let mut a = IndexMap::new();
        a.insert("b".to_string(), 10u64);
        a.insert("a".to_string(), 20u64);
        let h1 = hash_state(&Wrap(a));
        let mut a2 = IndexMap::new();
        a2.insert("a".to_string(), 20u64);
        a2.insert("b".to_string(), 10u64);
        let h2 = hash_state(&Wrap(a2));
        assert_eq!(h1, h2);
    }
}
