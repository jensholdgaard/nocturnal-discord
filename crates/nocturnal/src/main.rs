//! Nocturnal — event-sourced Discord DKP + telemetry-provisioning bot.
//!
//! Boot: config → tracing → instance lock (B2) → replay → health → gateway.
//! See docs/operations.md for the operational contract.

mod auctions;
mod bell;
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
            "--bell-test" => {
                // Diagnostic: connect, join a voice channel, play the bell,
                // report, exit. No ledger, no lock — just the voice path.
                let target = it.next().cloned().unwrap_or_default();
                return bell_test(&target);
            }
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
    // Saturation for the process itself: nothing else on the box reports it.
    // Held for the life of the process — dropping it stops the callbacks.
    let _process_metrics = nocturnal_telemetry::ProcessMetrics::install(&cfg.data.dir);

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

    if let Some(secs) = cfg.compaction.interval_secs {
        let driver = driver.clone();
        let period = std::time::Duration::from_secs(secs.max(60));
        tracing::info!(
            interval_secs = period.as_secs(),
            "automatic compaction enabled"
        );
        rt.spawn(async move {
            let mut interval = tokio::time::interval(period);
            // The first tick fires immediately; skip it so a restart loop can
            // never turn into a compaction loop.
            interval.tick().await;
            loop {
                interval.tick().await;
                if let Err(e) = driver.compact().await {
                    // Already counted and logged by the writer; a failed run
                    // is never fatal, the next one retries from a clean state.
                    tracing::warn!(error = %e, "scheduled compaction failed");
                }
            }
        });
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

/// `nocturnal --bell-test <guild_id>:<voice_channel_id>`
fn bell_test(target: &str) -> anyhow::Result<()> {
    use poise::serenity_prelude as serenity;

    let (guild, channel) = target
        .split_once(':')
        .context("usage: --bell-test <guild_id>:<voice_channel_id>")?;
    let guild: u64 = guild.parse().context("guild id")?;
    let channel: u64 = channel.parse().context("voice channel id")?;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("NOCTURNAL_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,songbird=debug")),
        )
        .init();

    let token = Config::discord_token()?;
    let rt = tokio::runtime::Runtime::new().context("tokio runtime")?;
    rt.block_on(async move {
        let voice = songbird::Songbird::serenity();
        let mut client = serenity::ClientBuilder::new(
            &token,
            serenity::GatewayIntents::GUILDS | serenity::GatewayIntents::GUILD_VOICE_STATES,
        )
        .voice_manager_arc(voice.clone())
        .await
        .context("building client")?;

        let shard_manager = client.shard_manager.clone();
        tokio::spawn(async move { client.start().await });

        // Give the gateway a moment to come up and cache the guild.
        tokio::time::sleep(std::time::Duration::from_secs(6)).await;

        tracing::info!(guild, channel, "joining voice channel");
        let call = voice
            .join(
                serenity::GuildId::new(guild),
                serenity::ChannelId::new(channel),
            )
            .await
            .context("join voice channel")?;
        tracing::info!("joined; playing the bell");

        let input = nocturnal_bell_input()
            .make_playable_async(
                songbird::input::codecs::get_codec_registry(),
                songbird::input::codecs::get_probe(),
            )
            .await
            .map_err(|e| anyhow::anyhow!("decode: {e}"))?;
        let track = call.lock().await.play_input(input);

        for _ in 0..40 {
            match track.get_info().await {
                Ok(info) => {
                    tracing::info!(
                        mode = ?info.playing,
                        position_ms = info.position.as_millis() as u64,
                        played_ms = info.play_time.as_millis() as u64,
                        "track state"
                    );
                    if info.playing.is_done() {
                        break;
                    }
                }
                Err(e) => {
                    tracing::info!(error = ?e, "track handle gone");
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        let _ = voice.remove(serenity::GuildId::new(guild)).await;
        shard_manager.shutdown_all().await;
        Ok::<(), anyhow::Error>(())
    })
}

fn nocturnal_bell_input() -> songbird::input::Input {
    songbird::input::Input::from(bell::embedded())
}
