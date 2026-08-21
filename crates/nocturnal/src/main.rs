//! Bin crate: wiring (config → store → core → discord → telemetry),
//! scheduler, health endpoints, graceful shutdown. See docs/operations.md.

fn main() {
    println!(
        "nocturnal {} — scaffold; the ledger arrives in M1 (docs/plan.md)",
        env!("CARGO_PKG_VERSION")
    );
}
