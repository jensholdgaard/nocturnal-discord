//! The guild's feedback channel, mirrored into Ourios.
//!
//! Every message in the configured channel becomes one `discord.feedback`
//! log record on the bot's own OTLP pipeline (gateway → Ourios, tenant
//! `nocturnal`): the text is the record body, the who/when/which are
//! attributes. Live messages arrive on the gateway; at boot the channel's
//! history is read back to the last id already mirrored, so a restart or
//! an outage loses nothing and a fresh deploy backfills the whole channel.
//!
//! Requires the privileged MESSAGE_CONTENT intent (developer portal) — see
//! `DiscordConfig::feedback_channel_id` and docs/feedback.md.

use nocturnal_telemetry::attr;
use poise::serenity_prelude as serenity;
use std::path::{Path, PathBuf};

/// Per-boot backfill ceiling; the channel is small and Ourios is not a
/// message archive.
const BACKFILL_MAX: usize = 2000;

pub struct Feedback {
    pub channel: serenity::ChannelId,
    /// Holds the highest message id mirrored so far.
    cursor: PathBuf,
}

impl Feedback {
    pub fn new(channel: u64, data_dir: &Path) -> Self {
        Self {
            channel: serenity::ChannelId::new(channel),
            cursor: data_dir.join("feedback.cursor"),
        }
    }

    fn last_seen(&self) -> Option<u64> {
        std::fs::read_to_string(&self.cursor)
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }

    /// Monotonic: an edit of an old message never moves the cursor back.
    fn mark(&self, id: u64) {
        if self.last_seen().is_some_and(|seen| seen >= id) {
            return;
        }
        let tmp = self.cursor.with_extension("cursor.tmp");
        if std::fs::write(&tmp, id.to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, &self.cursor);
        }
    }

    /// One record per message. `kind` is one of the registry's
    /// `nocturnal.feedback.kind` members.
    pub fn record(&self, m: &serenity::Message, kind: &'static str) {
        let reply_to = m
            .message_reference
            .as_ref()
            .and_then(|r| r.message_id)
            .map(|id| id.to_string())
            .unwrap_or_default();
        tracing::event!(
            name: "discord.feedback",
            target: "nocturnal::feedback",
            tracing::Level::INFO,
            { attr::NOCTURNAL_DISCORD_CHANNEL_ID } = m.channel_id.get(),
            { attr::NOCTURNAL_DISCORD_USER_ID } = m.author.id.get(),
            { attr::NOCTURNAL_DISCORD_USER_NAME } = %m.author.name,
            { attr::NOCTURNAL_FEEDBACK_MESSAGE_ID } = %m.id,
            { attr::NOCTURNAL_FEEDBACK_KIND } = kind,
            { attr::NOCTURNAL_FEEDBACK_ATTACHMENTS } = m.attachments.len(),
            { attr::NOCTURNAL_FEEDBACK_REPLY_TO } = %reply_to,
            { attr::NOCTURNAL_FEEDBACK_POSTED_MS } = m.timestamp.unix_timestamp() * 1000,
            "{}",
            m.content
        );
        self.mark(m.id.get());
    }

    /// Gateway message in the mirrored channel? Bots (this one included)
    /// are skipped so the mirror never feeds on its own replies.
    pub fn wants(&self, m: &serenity::Message) -> bool {
        m.channel_id == self.channel && !m.author.bot
    }

    /// Read the channel newest-first down to the stored cursor (or the
    /// beginning, capped), then emit oldest-first so Ourios sees them in
    /// order. Failures log and leave the cursor alone; the next boot retries.
    pub async fn backfill(&self, http: &serenity::Http) {
        let floor = self.last_seen().unwrap_or(0);
        let mut pending: Vec<serenity::Message> = Vec::new();
        let mut before: Option<serenity::MessageId> = None;
        loop {
            let mut req = serenity::GetMessages::new().limit(100);
            if let Some(b) = before {
                req = req.before(b);
            }
            let page = match self.channel.messages(http, req).await {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(
                        { attr::NOCTURNAL_ERROR_MESSAGE } = %e,
                        { attr::NOCTURNAL_DISCORD_CHANNEL_ID } = self.channel.get(),
                        "feedback backfill: could not read the channel (missing access, or the \
                         MESSAGE_CONTENT intent is off in the developer portal)"
                    );
                    return;
                }
            };
            let Some(last) = page.last().map(|m| m.id) else {
                break;
            };
            let mut reached_floor = false;
            for m in page {
                if m.id.get() <= floor {
                    reached_floor = true;
                    break;
                }
                if !m.author.bot {
                    pending.push(m);
                }
            }
            if reached_floor || pending.len() >= BACKFILL_MAX {
                break;
            }
            before = Some(last);
        }
        pending.sort_by_key(|m| m.id.get());
        let n = pending.len();
        for m in &pending {
            self.record(m, "backfill");
        }
        tracing::info!(
            { attr::NOCTURNAL_DISCORD_CHANNEL_ID } = self.channel.get(),
            { attr::NOCTURNAL_FEEDBACK_ATTACHMENTS } = n,
            "feedback backfill: {n} messages mirrored"
        );
    }
}

/// The bot's gateway intents: guild/channel data, voice states (raid tick
/// attendance), DMs (the bid flow) — and, only when a feedback channel is
/// configured, guild messages with their content. MESSAGE_CONTENT is
/// privileged: requesting it without the portal toggle takes the bot down.
pub fn gateway_intents(feedback: bool) -> serenity::GatewayIntents {
    let base = serenity::GatewayIntents::GUILDS
        | serenity::GatewayIntents::GUILD_VOICE_STATES
        | serenity::GatewayIntents::DIRECT_MESSAGES;
    if feedback {
        base | serenity::GatewayIntents::GUILD_MESSAGES | serenity::GatewayIntents::MESSAGE_CONTENT
    } else {
        base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_only_moves_forward() {
        let dir = std::env::temp_dir().join(format!("nocturnal-feedback-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap_or_default();
        let f = Feedback::new(1, &dir);
        assert_eq!(f.last_seen(), None);
        f.mark(10);
        f.mark(7);
        assert_eq!(
            f.last_seen(),
            Some(10),
            "an older id never rewinds the cursor"
        );
        f.mark(11);
        assert_eq!(f.last_seen(), Some(11));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn intents_are_privileged_only_on_request() {
        assert!(!gateway_intents(false).contains(serenity::GatewayIntents::MESSAGE_CONTENT));
        let on = gateway_intents(true);
        assert!(on.contains(serenity::GatewayIntents::MESSAGE_CONTENT));
        assert!(on.contains(serenity::GatewayIntents::GUILD_MESSAGES));
        assert!(on.contains(serenity::GatewayIntents::GUILD_VOICE_STATES));
    }
}
