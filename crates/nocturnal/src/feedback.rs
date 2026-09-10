//! The guild's feedback channels, mirrored into Ourios.
//!
//! Every message in a configured channel (2026-09-10: channels - feedback
//! for the site, bot-discussions for the bot, named in config and resolved
//! at boot) becomes one
//! `nocturnal.feedback.message` event record on the bot's own OTLP pipeline (gateway → Ourios, tenant
//! `nocturnal`): the text is the record body, the who/when/which are
//! attributes. Live messages arrive on the gateway; at boot the channel's
//! history is read back to the last id already mirrored, so a restart or
//! an outage loses nothing and a fresh deploy backfills the whole channel.
//!
//! Requires the privileged MESSAGE_CONTENT intent (developer portal) — see
//! `DiscordConfig::feedback_channel_id` and docs/feedback.md.

use nocturnal_telemetry::attr;
use nocturnal_telemetry::event;
use poise::serenity_prelude as serenity;
use std::path::{Path, PathBuf};

/// Per-boot backfill ceiling; the channel is small and Ourios is not a
/// message archive.
const BACKFILL_MAX: usize = 2000;

pub struct Feedback {
    /// The channels mirrored: the configured id, plus the ones named in
    /// config once the guild's channel list has been read at boot.
    channels: std::sync::RwLock<Vec<serenity::ChannelId>>,
    /// Channel names still to resolve against the guild.
    pending_names: std::sync::RwLock<Vec<String>>,
    /// One cursor file per channel, holding the highest message id mirrored.
    data_dir: PathBuf,
}

impl Feedback {
    pub fn new(channel: u64, names: Vec<String>, data_dir: &Path) -> Self {
        // The first channel's cursor predates the per-channel files: carry it
        // over so the original channel is not backfilled again.
        let legacy = data_dir.join("feedback.cursor");
        let mine = data_dir.join(format!("feedback-{channel}.cursor"));
        if legacy.exists() && !mine.exists() {
            let _ = std::fs::rename(&legacy, &mine);
        }
        Self {
            channels: std::sync::RwLock::new(vec![serenity::ChannelId::new(channel)]),
            pending_names: std::sync::RwLock::new(names),
            data_dir: data_dir.to_path_buf(),
        }
    }

    pub fn channels(&self) -> Vec<serenity::ChannelId> {
        self.channels.read().map(|c| c.clone()).unwrap_or_default()
    }

    /// Match the configured names against the guild's channels (called once
    /// the gateway is up). A name nobody has is logged and left pending.
    pub async fn resolve_names(&self, http: &serenity::Http, guild: serenity::GuildId) {
        let names: Vec<String> = self
            .pending_names
            .read()
            .map(|n| n.clone())
            .unwrap_or_default();
        if names.is_empty() {
            return;
        }
        let listed = match guild.channels(http).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!({ attr::NOCTURNAL_ERROR_MESSAGE } = %e, "feedback: could not list the guild's channels");
                return;
            }
        };
        for name in names {
            let found = listed
                .values()
                .find(|c| c.name.eq_ignore_ascii_case(name.trim_start_matches('#')))
                .map(|c| c.id);
            match found {
                Some(id) => {
                    if let Ok(mut cs) = self.channels.write() {
                        if !cs.contains(&id) {
                            cs.push(id);
                        }
                    }
                    if let Ok(mut p) = self.pending_names.write() {
                        p.retain(|n| n != &name);
                    }
                    tracing::info!(
                        { attr::NOCTURNAL_DISCORD_CHANNEL_ID } = id.get(),
                        "feedback: mirroring #{name}"
                    );
                }
                None => tracing::warn!("feedback: no channel named #{name} in the guild"),
            }
        }
    }

    fn cursor(&self, channel: serenity::ChannelId) -> PathBuf {
        self.data_dir
            .join(format!("feedback-{}.cursor", channel.get()))
    }

    fn last_seen(&self, channel: serenity::ChannelId) -> Option<u64> {
        std::fs::read_to_string(self.cursor(channel))
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }

    /// Monotonic: an edit of an old message never moves the cursor back.
    fn mark(&self, channel: serenity::ChannelId, id: u64) {
        if self.last_seen(channel).is_some_and(|seen| seen >= id) {
            return;
        }
        let cursor = self.cursor(channel);
        let tmp = cursor.with_extension("cursor.tmp");
        if std::fs::write(&tmp, id.to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, &cursor);
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
            name: event::NOCTURNAL_FEEDBACK_MESSAGE,
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
        self.mark(m.channel_id, m.id.get());
    }

    /// Gateway message in a mirrored channel? Bots (this one included)
    /// are skipped so the mirror never feeds on its own replies.
    pub fn wants(&self, m: &serenity::Message) -> bool {
        self.mirrors(m.channel_id) && !m.author.bot
    }

    pub fn mirrors(&self, channel: serenity::ChannelId) -> bool {
        self.channels
            .read()
            .map(|c| c.contains(&channel))
            .unwrap_or(false)
    }

    /// Every mirrored channel, read back to its cursor.
    pub async fn backfill(&self, http: &serenity::Http) {
        for channel in self.channels() {
            self.backfill_channel(http, channel).await;
        }
    }

    /// Read one channel newest-first down to the stored cursor (or the
    /// beginning, capped), then emit oldest-first so Ourios sees them in
    /// order. Failures log and leave the cursor alone; the next boot retries.
    async fn backfill_channel(&self, http: &serenity::Http, channel: serenity::ChannelId) {
        let floor = self.last_seen(channel).unwrap_or(0);
        let mut pending: Vec<serenity::Message> = Vec::new();
        let mut before: Option<serenity::MessageId> = None;
        loop {
            let mut req = serenity::GetMessages::new().limit(100);
            if let Some(b) = before {
                req = req.before(b);
            }
            let page = match channel.messages(http, req).await {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(
                        { attr::NOCTURNAL_ERROR_MESSAGE } = %e,
                        { attr::NOCTURNAL_DISCORD_CHANNEL_ID } = channel.get(),
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
            { attr::NOCTURNAL_DISCORD_CHANNEL_ID } = channel.get(),
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
        let f = Feedback::new(1, vec!["bot-discussions".into()], &dir);
        let one = serenity::ChannelId::new(1);
        assert_eq!(f.last_seen(one), None);
        f.mark(one, 10);
        f.mark(one, 7);
        assert_eq!(
            f.last_seen(one),
            Some(10),
            "an older id never rewinds the cursor"
        );
        f.mark(one, 11);
        assert_eq!(f.last_seen(one), Some(11));
        // Channels keep their own cursors; an unresolved name mirrors nothing yet.
        let two = serenity::ChannelId::new(2);
        assert_eq!(f.last_seen(two), None);
        assert!(f.mirrors(one) && !f.mirrors(two));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_original_cursor_is_carried_over() {
        let dir =
            std::env::temp_dir().join(format!("nocturnal-feedback-legacy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap_or_default();
        std::fs::write(dir.join("feedback.cursor"), "42").unwrap_or_default();
        let f = Feedback::new(9, Vec::new(), &dir);
        assert_eq!(f.last_seen(serenity::ChannelId::new(9)), Some(42));
        assert!(!dir.join("feedback.cursor").exists());
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
