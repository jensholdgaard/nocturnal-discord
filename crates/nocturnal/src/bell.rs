//! The auction bell (legacy `utils/Player.js`), kept because officers know it.
//!
//! Strictly decorative and strictly fire-and-forget: it runs in its own task,
//! every failure is logged and swallowed, and the whole thing is bounded by a
//! timeout — the legacy version could hang forever on a voice promise that
//! never settled (audit #13), and an audio error could kill the process.
//! Nothing here can touch an auction.
//!
//! The sound is compiled into the binary (34 KB), so there is no asset to
//! deploy, no fetch on the hot path, and no failure mode between "auction
//! starts" and "bell rings". A path in config overrides it.

use std::time::Duration;

use poise::serenity_prelude as serenity;
use songbird::input::Input;

/// The legacy bell, embedded.
const BELL: &[u8] = include_bytes!("../assets/bell.mp3");

/// A bell must never outlive the auction it announces.
const PLAY_TIMEOUT: Duration = Duration::from_secs(10);

fn sound(path: Option<&std::path::Path>) -> Input {
    match path {
        Some(p) => match std::fs::read(p) {
            Ok(bytes) => Input::from(bytes),
            Err(e) => {
                tracing::warn!(path = %p.display(), error = %e, "bell file unreadable; using the built-in sound");
                Input::from(BELL)
            }
        },
        None => Input::from(BELL),
    }
}

/// Ring the bell in each raid voice channel, then leave. Never fails.
pub fn ring(
    ctx: &serenity::Context,
    guild_id: u64,
    channels: Vec<u64>,
    path: Option<std::path::PathBuf>,
) {
    if channels.is_empty() {
        return;
    }
    let ctx = ctx.clone();
    tokio::spawn(async move {
        for channel in channels {
            let span = tracing::info_span!("bell.ring", guild_id, channel);
            let _entered = span.enter();
            match tokio::time::timeout(
                PLAY_TIMEOUT,
                play_in(&ctx, guild_id, channel, path.as_deref()),
            )
            .await
            {
                Ok(Ok(())) => tracing::info!("bell rung"),
                Ok(Err(e)) => tracing::info!(error = %e, "bell skipped"),
                Err(_) => tracing::warn!("bell timed out"),
            }
            // Always hand the channel back, however the attempt went.
            if let Some(manager) = songbird::get(&ctx).await {
                let _ = manager.remove(serenity::GuildId::new(guild_id)).await;
            }
        }
    });
}

async fn play_in(
    ctx: &serenity::Context,
    guild_id: u64,
    channel: u64,
    path: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    let manager = songbird::get(ctx)
        .await
        .ok_or_else(|| anyhow::anyhow!("voice support not registered"))?;
    let call = manager
        .join(
            serenity::GuildId::new(guild_id),
            serenity::ChannelId::new(channel),
        )
        .await?;
    let mut call = call.lock().await;
    let track = call.play_input(sound(path));
    drop(call);

    // Wait for the sound to finish rather than cutting it off on leave.
    loop {
        match track.get_info().await {
            Ok(info) if info.playing.is_done() => break,
            Ok(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            // The track is gone: finished or dropped. Either way we are done.
            Err(_) => break,
        }
    }
    Ok(())
}
