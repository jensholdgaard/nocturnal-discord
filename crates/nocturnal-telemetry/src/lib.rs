//! Telemetry: attribute/metric names generated from `semconv/` by OTel
//! Weaver (never hand-written — a misspelled attribute is a compile error,
//! not an empty dashboard), plus the OTLP wiring that puts the bot into the
//! guild's everquest-observability stack.
//!
//! Regenerate after editing the registry:
//! `weaver registry generate -r semconv -t templates rust crates/nocturnal-telemetry/src/`
//! CI diffs the committed file against a fresh generation.

pub mod generated;
pub mod otlp;
pub mod process;

pub use generated::{attr, event, metric};
pub use otlp::{init, metrics, Metrics, TelemetryConfig, TelemetryGuard};
pub use process::ProcessMetrics;
