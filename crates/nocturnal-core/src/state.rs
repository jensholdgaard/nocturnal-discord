//! Projections: all mutable state, rebuilt by replaying the log.
//! Everything here derives `PartialEq` so replay determinism is testable.

use std::collections::{BTreeMap, BTreeSet};

use crate::event::{ConfigPatch, Flavor, GuildId, Item, PlayerId, RaidRef};

/// One line of a player's history (mirrors the legacy log shape).
#[derive(Debug, Clone, PartialEq)]
pub struct LogEntry {
    pub dkp: i64,
    pub comment: String,
    pub ts_ms: i64,
    pub raid: Option<RaidRef>,
    pub item: Option<Item>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Player {
    pub balance: i64,
    pub characters: Vec<String>,
    pub creation_ts_ms: i64,
    pub log: Vec<LogEntry>,
}

/// One attendance entry (Start / Tick / End / award comment).
#[derive(Debug, Clone, PartialEq)]
pub struct AttendanceEntry {
    pub players: Vec<PlayerId>,
    pub comment: String,
    pub ts_ms: i64,
    pub amount: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Raid {
    pub name: String,
    pub date_ms: i64,
    pub tick_interval_ms: i64,
    pub dkp_per_tick: i64,
    pub active: bool,
    pub tick_no: u32,
    pub event_id: Option<String>,
    pub entries: Vec<AttendanceEntry>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Bid {
    pub player: PlayerId,
    pub amount: i64,
    pub for_main: bool,
    pub attendance: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuctionStatus {
    Open,
    Closed,
    Finalized,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Auction {
    pub item: Item,
    pub flavor: Flavor,
    pub min_bid: i64,
    pub num_items: u32,
    pub min_bid_to_lock_for_main: i64,
    pub over_bid_to_win_main: i64,
    pub deadline_ts_ms: i64,
    pub status: AuctionStatus,
    pub bids: Vec<Bid>,
    pub winners: Vec<crate::event::Winner>,
}

/// Per-guild behavioural config with the *fixed* defaults (audit S9: the
/// legacy raid-deprecation fallback was 90 milliseconds).
#[derive(Debug, Clone, PartialEq)]
pub struct GuildConfig {
    pub admin_role: Option<u64>,
    pub raid_channel: Option<u64>,
    pub second_raid_channel: Option<u64>,
    pub log_channel: Option<u64>,
    pub auction_channel: Option<u64>,
    pub long_auction_channel: Option<u64>,
    pub tick_duration_ms: i64,
    pub raid_deprecation_ms: i64,
    pub bid_time_s: i64,
    pub min_bid: i64,
    pub min_bid_to_lock_for_main: i64,
    pub over_bid_to_win_main: i64,
    pub raidhelper_api_key: Option<String>,
    /// DKP awarded to signups who attended, when a linked raid ends. Legacy
    /// hardcoded 5; officers can set it.
    pub raidhelper_event_dkp: i64,
}

pub const DAY_MS: i64 = 86_400_000;

impl Default for GuildConfig {
    fn default() -> Self {
        GuildConfig {
            admin_role: None,
            raid_channel: None,
            second_raid_channel: None,
            log_channel: None,
            auction_channel: None,
            long_auction_channel: None,
            tick_duration_ms: 6 * 60_000,
            raid_deprecation_ms: 90 * DAY_MS,
            bid_time_s: 60,
            min_bid: 0,
            min_bid_to_lock_for_main: 0,
            over_bid_to_win_main: 0,
            raidhelper_api_key: None,
            raidhelper_event_dkp: 5,
        }
    }
}

impl GuildConfig {
    pub fn apply_patch(&mut self, p: &ConfigPatch) {
        macro_rules! opt {
            ($($f:ident),*) => { $( if let Some(v) = &p.$f { self.$f = Some(v.clone()); } )* };
        }
        macro_rules! val {
            ($($f:ident),*) => { $( if let Some(v) = p.$f { self.$f = v; } )* };
        }
        opt!(raidhelper_api_key);
        if let Some(v) = p.admin_role {
            self.admin_role = Some(v);
        }
        if let Some(v) = p.raid_channel {
            self.raid_channel = Some(v);
        }
        if let Some(v) = p.second_raid_channel {
            self.second_raid_channel = Some(v);
        }
        if let Some(v) = p.log_channel {
            self.log_channel = Some(v);
        }
        if let Some(v) = p.auction_channel {
            self.auction_channel = Some(v);
        }
        if let Some(v) = p.long_auction_channel {
            self.long_auction_channel = Some(v);
        }
        val!(
            raidhelper_event_dkp,
            tick_duration_ms,
            raid_deprecation_ms,
            bid_time_s,
            min_bid,
            min_bid_to_lock_for_main,
            over_bid_to_win_main
        );
    }
}

/// A member's telemetry provisioning grant (dpsbot successor, M8).
#[derive(Debug, Clone, PartialEq)]
pub struct TokenGrant {
    /// sha256 of the member's token. The secret is not stored here, and not
    /// anywhere else in the ledger — see `Event::TelemetryTokenIssued`.
    pub token_fp: String,
    pub role: String,
}

/// All projections for one Discord guild.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GuildState {
    pub players: BTreeMap<PlayerId, Player>,
    /// lowercase character name -> owner (deliberate change #6: case-insensitive).
    pub characters: BTreeMap<String, PlayerId>,
    pub raids: BTreeMap<String, Raid>,
    pub active_raid: Option<String>,
    pub auctions: BTreeMap<String, Auction>,
    pub config: GuildConfig,
    pub telemetry: BTreeMap<String, TokenGrant>,
    /// Every username the ledger has ever issued a telemetry token to.
    ///
    /// Grows only — a revoke drops the grant but keeps the name here, because
    /// the derived files live in directories the ledger does not own.
    /// `tokens.txt` also carries service credentials (the bot's own among
    /// them), so "rewrite from the projection" is only safe when the writer
    /// can tell *its* lines from everyone else's. This set is that answer.
    pub telemetry_managed: BTreeSet<String>,
}

/// The whole projected world.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct State {
    pub guilds: BTreeMap<GuildId, GuildState>,
}

impl State {
    pub fn guild(&self, id: GuildId) -> Option<&GuildState> {
        self.guilds.get(&id)
    }

    pub fn guild_mut(&mut self, id: GuildId) -> &mut GuildState {
        self.guilds.entry(id).or_default()
    }
}

impl GuildState {
    pub fn balance(&self, player: PlayerId) -> i64 {
        self.players.get(&player).map_or(0, |p| p.balance)
    }

    /// A raid counts for attendance while younger than the deprecation window
    /// (derived — no deprecation events needed).
    fn raid_counts(&self, raid: &Raid, now_ms: i64) -> bool {
        raid.date_ms >= now_ms - self.config.raid_deprecation_ms
    }

    /// Attendance %, legacy formula (2 decimal places; no possible entries =>
    /// 100). Entries in raids that predate the player count only from the
    /// player's creation timestamp onward.
    pub fn attendance_pct(&self, player: PlayerId, now_ms: i64) -> f64 {
        let creation = self
            .players
            .get(&player)
            .map_or(now_ms, |p| p.creation_ts_ms);
        let mut possible = 0u64;
        let mut attended = 0u64;
        for raid in self.raids.values() {
            if !self.raid_counts(raid, now_ms) {
                continue;
            }
            if raid.date_ms < creation {
                possible += raid.entries.iter().filter(|e| e.ts_ms >= creation).count() as u64;
            } else {
                possible += raid.entries.len() as u64;
            }
            attended += raid
                .entries
                .iter()
                .filter(|e| e.players.contains(&player))
                .count() as u64;
        }
        if possible == 0 {
            return 100.0;
        }
        let pct = attended as f64 / possible as f64 * 100.0;
        (pct * 100.0).round() / 100.0
    }

    /// DKP a player has committed as standing bids on *other* open auctions —
    /// the cross-auction double-spend guard (audit #46).
    pub fn committed_elsewhere(&self, player: PlayerId, except_auction: &str) -> i64 {
        self.auctions
            .iter()
            .filter(|(id, a)| a.status == AuctionStatus::Open && id.as_str() != except_auction)
            .flat_map(|(_, a)| a.bids.iter())
            .filter(|b| b.player == player)
            .map(|b| b.amount)
            .sum()
    }
}
