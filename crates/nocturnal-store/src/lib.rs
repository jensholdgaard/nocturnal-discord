//! Event store: the write-ahead log that makes the ledger durable.
//!
//! Format: one event per line, `"{crc32:08x} {json}\n"` — human-greppable on
//! purpose (loot disputes are settled with `grep`). Appends fsync before
//! returning. On open, a torn *trailing* record (crash mid-write, hazard B1)
//! is truncated away; corruption anywhere else refuses to load.
//!
//! Parquet compaction of sealed segments lands in M2.

pub mod wal;

pub use wal::{Wal, WalError};
