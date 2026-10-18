//! Stable, content-addressed identifiers.
//!
//! All hashes are BLAKE3 digests. Short hashes are display-only prefixes and
//! must never be treated as unambiguous identifiers for storage or lookup.

use blake3::Hasher;
use serde::{Deserialize, Serialize};

/// Length of a BLAKE3 digest in bytes.
pub const HASH_LEN: usize = 32;

/// A full content hash (state object, commit, or root).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Hash(pub [u8; HASH_LEN]);

impl Hash {
    pub const fn from_bytes(bytes: [u8; HASH_LEN]) -> Self {
        Hash(bytes)
    }

    pub fn as_bytes(&self) -> [u8; HASH_LEN] {
        self.0
    }

    /// Compute a short, human-readable hash prefix. Collisions are possible;
    /// callers must fall back to the full hash when disambiguation fails.
    pub fn short(&self, len: usize) -> ShortHash {
        let hex = self.hex();
        let len = len.min(hex.len());
        ShortHash(hex[..len].to_string())
    }

    pub fn hex(&self) -> String {
        hex_encode(&self.0)
    }

    pub fn from_hex(hex: &str) -> Result<Self, crate::error::MeldError> {
        let bytes = parse_hex(hex)?;
        if bytes.len() != HASH_LEN {
            return Err(crate::error::MeldError::InvalidInput(format!(
                "hash must be {} hex characters, got {}",
                HASH_LEN * 2,
                hex.len()
            )));
        }
        let mut arr = [0u8; HASH_LEN];
        arr.copy_from_slice(&bytes);
        Ok(Hash(arr))
    }
}

impl std::fmt::Display for Hash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.hex())
    }
}

impl std::fmt::LowerHex for Hash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for b in &self.0 {
            write!(f, "{:02x}", b)?;
        }
        Ok(())
    }
}

/// A short hash prefix for display only.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ShortHash(pub String);

impl ShortHash {
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Display for ShortHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A commit identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CommitHash(pub Hash);

impl CommitHash {
    pub const ZERO: Self = Self(Hash([0u8; HASH_LEN]));
    pub fn is_zero(&self) -> bool {
        self.0.0 == [0u8; HASH_LEN]
    }
    pub fn short(&self, len: usize) -> ShortHash {
        self.0.short(len)
    }
    pub fn hex(&self) -> String {
        self.0.hex()
    }
    pub fn from_bytes(bytes: [u8; HASH_LEN]) -> Self {
        CommitHash(Hash::from_bytes(bytes))
    }
}

impl std::fmt::Display for CommitHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A content-addressed object identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ObjectHash(pub Hash);

impl ObjectHash {
    pub fn from_bytes(bytes: [u8; HASH_LEN]) -> Self {
        ObjectHash(Hash::from_bytes(bytes))
    }
    pub fn short(&self, len: usize) -> ShortHash {
        self.0.short(len)
    }
    pub fn hex(&self) -> String {
        self.0.hex()
    }
}

impl std::fmt::Display for ObjectHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A root state hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RootHash(pub Hash);

impl RootHash {
    pub fn short(&self, len: usize) -> ShortHash {
        self.0.short(len)
    }
    pub fn hex(&self) -> String {
        self.0.hex()
    }
    pub fn from_bytes(bytes: [u8; HASH_LEN]) -> Self {
        RootHash(Hash::from_bytes(bytes))
    }
}

impl std::fmt::Display for RootHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Stable object identifier generated for an object where no natural key exists.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ObjectId(pub uuid::Uuid);

impl ObjectId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

impl Default for ObjectId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ObjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Hash arbitrary bytes with a domain separator.
pub fn hash_state_bytes(domain: &str, bytes: &[u8]) -> Hash {
    let mut hasher = Hasher::new();
    hasher.update(domain.as_bytes());
    hasher.update(b"\0");
    hasher.update(bytes);
    let out = hasher.finalize();
    let mut arr = [0u8; HASH_LEN];
    arr.copy_from_slice(out.as_bytes());
    Hash(arr)
}

/// Hash a canonical-serializable value.
pub fn hash_canonical<T: crate::canonical::Canonicalize>(domain: &str, value: &T) -> Hash {
    hash_state_bytes(domain, &value.canonical_bytes())
}

/// Content address for a stored object (domain: `meld-object-v1`).
pub fn hash_object(bytes: &[u8]) -> Hash {
    hash_state_bytes(crate::canonical::DOMAIN_OBJECT, bytes)
}

/// Content address for a stored commit (domain: `meld-commit-v1`).
pub fn hash_commit(bytes: &[u8]) -> Hash {
    hash_state_bytes(crate::canonical::DOMAIN_COMMIT, bytes)
}

fn parse_hex(hex: &str) -> Result<Vec<u8>, crate::error::MeldError> {
    if !hex.len().is_multiple_of(2) {
        return Err(crate::error::MeldError::InvalidInput(format!(
            "odd-length hex string: {hex}"
        )));
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| crate::error::MeldError::InvalidInput(format!("invalid hex: {e}")))
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_round_trip_hex() {
        let bytes = [42u8; HASH_LEN];
        let h = Hash::from_bytes(bytes);
        let parsed = Hash::from_hex(&h.hex()).unwrap();
        assert_eq!(h, parsed);
        assert_eq!(h.short(12).len(), 12);
    }

    #[test]
    fn commit_hash_zero() {
        assert!(CommitHash::ZERO.is_zero());
    }

    #[test]
    fn short_hash_is_prefix() {
        let h = Hash::from_bytes([1u8; HASH_LEN]);
        let s = h.short(8);
        assert_eq!(s.len(), 8);
        assert_eq!(s.0, "01010101");
    }
}
