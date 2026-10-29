pub const CURRENT_SCHEMA_VERSION: i32 = 2;
pub const DATABASE_READ_VERSION: i32 = 1;
pub const DATABASE_WRITE_VERSION: i32 = 1;
pub const LIST_ORDER_GAP: i64 = 1_000_000;
pub const LIST_ORDER_BASE: i64 = 1_024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DatabaseInfo {
    pub schema_version: i32,
    pub minimum_read_version: i32,
    pub minimum_write_version: i32,
    pub foreign_keys_enabled: bool,
    pub journal_mode: String,
    pub busy_timeout_millis: i64,
}
