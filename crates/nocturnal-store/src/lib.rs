//! Event store: WAL append/replay (CRC, trailing-truncation recovery),
//! Parquet compaction, backups. Hazards B1/B5 live and die here. (M1–M2)

pub const CRATE: &str = "nocturnal-store";
