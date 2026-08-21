//! One-shot migration (M2/M8): legacy Mongo export → genesis events;
//! legacy tokens.txt + provisioning dir → genesis telemetry events.
//! Emits the balance-verification report officers sign off on.

pub const CRATE: &str = "nocturnal-migrate";
