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

use nocturnal_telemetry::attr;
use std::time::Duration;

use poise::serenity_prelude as serenity;
use songbird::input::Input;

/// The legacy bell, embedded.
const BELL: &[u8] = include_bytes!("../assets/bell.mp3");

/// The embedded sound, for the `--bell-test` diagnostic.
pub fn embedded() -> &'static [u8] {
    BELL
}

/// A bell must never outlive the auction it announces.
const PLAY_TIMEOUT: Duration = Duration::from_secs(10);

fn sound(path: Option<&std::path::Path>) -> Input {
    match path {
        Some(p) => match std::fs::read(p) {
            Ok(bytes) => Input::from(bytes),
            Err(e) => {
                tracing::warn!({ attr::FILE_PATH } = %p.display(),
                    { attr::NOCTURNAL_ERROR_MESSAGE } = %e, "bell file unreadable; using the built-in sound");
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
                Ok(Err(e)) => {
                    tracing::info!({ attr::NOCTURNAL_ERROR_MESSAGE } = %e, "bell skipped")
                }
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

    // Decode *before* joining: a bad sound then fails loudly here instead of
    // looking like a bot that sits silently in the channel.
    let input = sound(path)
        .make_playable_async(
            songbird::input::codecs::get_codec_registry(),
            songbird::input::codecs::get_probe(),
        )
        .await
        .map_err(|e| anyhow::anyhow!("bell audio could not be decoded: {e}"))?;

    let call = manager
        .join(
            serenity::GuildId::new(guild_id),
            serenity::ChannelId::new(channel),
        )
        .await?;
    let mut locked = call.lock().await;
    let track = locked.play_input(input);
    drop(locked);

    // Wait for the sound to finish rather than cutting it off on leave.
    let started = std::time::Instant::now();
    loop {
        match track.get_info().await {
            Ok(info) if info.playing.is_done() => break,
            Ok(info) => {
                if started.elapsed() > Duration::from_secs(8) {
                    tracing::warn!(
                        { attr::NOCTURNAL_BELL_STATE } = ?info.playing,
                        { attr::NOCTURNAL_BELL_POSITION } = info.position.as_secs_f64(),
                        { attr::NOCTURNAL_BELL_PLAYED } = info.play_time.as_secs_f64(),
                        "bell still not finished; leaving anyway"
                    );
                    break;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            // The track is gone: finished or dropped. Either way we are done.
            Err(_) => break,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{sound, BELL};

    /// The embedded bell must actually decode. Without this, a broken asset
    /// or a missing codec feature shows up as a bot that joins the voice
    /// channel and sits there in silence — which is exactly what happened.
    #[tokio::test]
    async fn embedded_bell_decodes() {
        assert!(!BELL.is_empty(), "bell asset is embedded");
        let playable = sound(None)
            .make_playable_async(
                songbird::input::codecs::get_codec_registry(),
                songbird::input::codecs::get_probe(),
            )
            .await
            .expect("the embedded bell decodes with the codecs we ship");
        assert!(playable.is_playable());
    }

    /// An unreadable override falls back to the embedded sound rather than
    /// failing the auction.
    #[tokio::test]
    async fn missing_override_falls_back() {
        let input = sound(Some(std::path::Path::new("/nonexistent/bell.mp3")));
        let playable = input
            .make_playable_async(
                songbird::input::codecs::get_codec_registry(),
                songbird::input::codecs::get_probe(),
            )
            .await;
        assert!(playable.is_ok(), "fell back to the embedded bell");
    }
}
