//! Store schema definitions and versioning.

use redb::TableDefinition;

/// Current on-disk schema version for Meld's local store.
pub const STORE_SCHEMA_VERSION: u32 = 1;

pub const STORE_META: &str = "store_meta";
pub const TABLE_OBJECTS: &str = "objects";
pub const TABLE_COMMITS: &str = "commits";
pub const TABLE_BRANCHES: &str = "branches";
pub const TABLE_HEAD: &str = "head";
pub const TABLE_CONNECTIONS: &str = "connections";

/// Key used for the HEAD reference in the head table.
pub const HEAD_KEY: &str = "HEAD";

/// Table definition for the store-meta table (String keys, String values).
pub const STORE_META_TABLE: TableDefinition<String, String> = TableDefinition::new(STORE_META);

/// Table definitions for Meld's content-addressed object and commit tables.
pub const OBJECTS_TABLE: TableDefinition<String, Vec<u8>> = TableDefinition::new(TABLE_OBJECTS);
pub const COMMITS_TABLE: TableDefinition<String, Vec<u8>> = TableDefinition::new(TABLE_COMMITS);
pub const BRANCHES_TABLE: TableDefinition<String, String> = TableDefinition::new(TABLE_BRANCHES);
pub const HEAD_TABLE: TableDefinition<String, String> = TableDefinition::new(TABLE_HEAD);
pub const CONNECTIONS_TABLE: TableDefinition<String, String> =
    TableDefinition::new(TABLE_CONNECTIONS);
