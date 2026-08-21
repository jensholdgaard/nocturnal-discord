//! dpsbot successor (M8): telemetry token + Perses dashboard provisioning.
//! Projections come from nocturnal-core `telemetry.*` events; this crate
//! materializes them to `tokens.txt` and Perses provisioning YAMLs —
//! atomically, idempotently, and again on every boot.

pub const CRATE: &str = "nocturnal-provision";
