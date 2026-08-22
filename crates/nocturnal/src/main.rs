//! Nocturnal — event-sourced Discord DKP + telemetry-provisioning bot.
//!
//! Boot: config → tracing → instance lock (B2) → replay → health → gateway.
//! See docs/operations.md for the operational contract.

mod auctions;
mod config;
mod discord;
mod driver;
mod health;
mod items;
mod lock;
mod raidhelper;
mod scheduler;

use anyhow::Context as _;
use config::Config;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mut config_path: Option<&str> = None;
    let mut mode_check = false;
    let mut mode_print = false;
    let mut offline = false;
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--config" => config_path = it.next().map(String::as_str),
            "--check" => mode_check = true,
            "--print-config" => mode_print = true,
            "--offline" => offline = true,
            "--version" | "-V" => {
                println!("nocturnal {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            other => anyhow::bail!(
                "unknown argument {other:?} (known: --config <path>, --check, --print-config, --offline, --version)"
            ),
        }
    }

    let cfg = Config::load(config_path)?;
    if mode_print {
        println!("{}", cfg.printable());
        return Ok(());
    }

    // The OTLP exporters spawn background tasks: give them a reactor first.
    let rt = tokio::runtime::Runtime::new().context("tokio runtime")?;
    let _rt_guard = rt.enter();
    let _telemetry = nocturnal_telemetry::init(&nocturnal_telemetry::TelemetryConfig {
        default_service_name: "nocturnal".to_owned(),
        log_filter: cfg.log.level.clone(),
        log_json: cfg.log.format == "json",
    })?;

    // One writer, ever (hazard B2). Held until exit.
    let _lock = lock::acquire(&cfg.data.dir)?;

    let archive = match &cfg.archive.bucket {
        Some(bucket) => {
            let archive = nocturnal_store::Archive::s3(bucket, &cfg.archive.prefix)
                .with_context(|| format!("configuring archive bucket {bucket}"))?;
            tracing::info!(bucket, prefix = %cfg.archive.prefix, "compacted history is archived off-site");
            Some(archive)
        }
        None => None,
    };
    let (driver, replayed) = driver::start_with_archive(&cfg.data.dir, archive)?;
    if mode_check {
        println!("config ok; ledger ok ({replayed} events)");
        return Ok(());
    }

    let readiness = health::Readiness::default();
    if let Some(bind) = &cfg.health.bind {
        health::serve(bind, readiness.clone())?;
    }

    if offline {
        tracing::info!(
            replayed,
            "offline mode: ledger up, no gateway; ctrl-c to exit"
        );
        readiness.set_ready();
        rt.block_on(async {
            let _ = tokio::signal::ctrl_c().await;
        });
        return Ok(());
    }

    rt.block_on(discord::run(&cfg, driver, readiness))
}
