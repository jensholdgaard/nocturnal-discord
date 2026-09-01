//! Projections: all mutable state, rebuilt by replaying the log.
//! Everything here derives `PartialEq` so replay determinism is testable.

use std::collections::{BTreeMap, BTreeSet};

use crate::event::{
    ConfigPatch, Flavor, GuildId, Item, PlayerId, RaidRef, RosterCharacter, Secret,
};

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
    /// The `_id` of the legacy Mongo document, for players that came from the
    /// migration. `None` for anyone the rewrite met first — nothing in the
    /// bot reads it; it exists so `/backup` can reproduce the document.
    pub legacy_id: Option<String>,
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
    /// When `/endraid` happened (the envelope's timestamp), so a raid's
    /// window is start-to-end, not first-tick-to-last-tick-plus-a-guess.
    /// `None` for imported raids and while active.
    pub ended_ms: Option<i64>,
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
    /// Set when the auction was cancelled: who did it, and when. `None` for
    /// a cancel the scheduler made on its own.
    pub cancelled_by: Option<PlayerId>,
    pub cancelled_ts_ms: Option<i64>,
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
    pub raidhelper_api_key: Option<Secret>,
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
    /// The guild roster: each member's characters, keyed by lowercase name
    /// so `Shaku` and `shaku` are one character. Absorbed from the roster
    /// bot, whose store was a Google Sheet.
    pub roster: BTreeMap<PlayerId, BTreeMap<String, RosterCharacter>>,
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
    pub fn raid_counts(&self, raid: &Raid, now_ms: i64) -> bool {
        raid.date_ms >= now_ms - self.config.raid_deprecation_ms
    }

    /// The players who count as raiding right now: anyone with ledger
    /// activity inside the raid-deprecation window.
    ///
    /// This is *the* definition. `/listplayersdkps` lists exactly these rows,
    /// the `nocturnal.guild.attendance.average` gauge averages exactly these
    /// rows, and anything else that reports "the raiders" must call this —
    /// two hand-written copies of the same filter is how a dashboard and a
    /// command quietly start disagreeing about who is in the guild.
    pub fn raiding_players(&self, now_ms: i64) -> impl Iterator<Item = (PlayerId, &Player)> + '_ {
        let cutoff = now_ms - self.config.raid_deprecation_ms;
        self.players
            .iter()
            .filter(move |(_, p)| p.log.last().is_some_and(|e| e.ts_ms >= cutoff))
            .map(|(id, p)| (*id, p))
    }

    /// Mean attendance over [`raiding_players`], or `None` when nobody is
    /// raiding — so a gauge can stay absent rather than report a fictional 0.
    pub fn average_attendance(&self, now_ms: i64) -> Option<f64> {
        let pcts: Vec<f64> = self
            .raiding_players(now_ms)
            .map(|(id, _)| self.attendance_pct(id, now_ms))
            .collect();
        (!pcts.is_empty()).then(|| pcts.iter().sum::<f64>() / pcts.len() as f64)
    }

    /// The ledger's raids as people experience them: back-to-back raids
    /// under one name are one raid night. A false `/startraid` ended and
    /// redone minutes later stays two ids in the ledger and reads as one
    /// night everywhere else. Newest night first; inside a night, oldest
    /// raid first.
    pub fn raid_nights(&self) -> Vec<Vec<(&String, &Raid)>> {
        let mut raids: Vec<(&String, &Raid)> = self.raids.iter().collect();
        raids.sort_by_key(|(_, r)| raid_window(r).0);
        let mut nights: Vec<Vec<(&String, &Raid)>> = Vec::new();
        for (id, r) in raids {
            match nights.last_mut() {
                Some(n) if n.iter().any(|(_, x)| same_raid(x, r)) => n.push((id, r)),
                _ => nights.push(vec![(id, r)]),
            }
        }
        nights.reverse();
        nights
    }

    /// Raid attendance, the guild's one definition — the rule Zig's roster
    /// sheet uses, reverse-engineered on 2026-09-01 to a 240/240 exact match
    /// against two snapshots of that sheet (docs/attendance.md):
    ///
    /// 1. Count DKP-bearing ticks only (`Start` and `Tick` entries; `End`
    ///    and awards are not ticks).
    /// 2. Bucket them into weeks (Monday 00:00 UTC) and keep the ten most
    ///    recent weeks that had any raid, the current partial week included.
    /// 3. Drop the two weeks with the lowest percentage; among equal
    ///    percentages the week with more ticks held goes first. Eight weeks
    ///    or fewer: keep them all.
    /// 4. `floor(attended / held * 100)` over the kept weeks, pooled — not a
    ///    mean of weekly percentages.
    ///
    /// No raids at all reads as 100 (nothing was possible), as before.
    pub fn attendance_pct(&self, player: PlayerId, now_ms: i64) -> f64 {
        let mut weeks: BTreeMap<i64, (u64, u64)> = BTreeMap::new();
        for raid in self.raids.values() {
            for e in &raid.entries {
                if e.ts_ms > now_ms || !(e.comment == "Tick" || e.comment == "Start") {
                    continue;
                }
                // The epoch was a Thursday; +3 days makes the buckets start
                // on Mondays.
                let week = (e.ts_ms + 3 * DAY_MS).div_euclid(WEEK_MS);
                let w = weeks.entry(week).or_default();
                w.1 += 1;
                if e.players.contains(&player) {
                    w.0 += 1;
                }
            }
        }
        let mut recent: Vec<(u64, u64)> = weeks.values().rev().take(10).copied().collect();
        if recent.is_empty() {
            return 100.0;
        }
        if recent.len() > 8 {
            let pct = |w: &(u64, u64)| w.0 as f64 / w.1 as f64;
            recent.sort_by(|a, b| pct(a).total_cmp(&pct(b)).then(b.1.cmp(&a.1)));
            recent.drain(..2);
        }
        let (attended, held) = recent
            .iter()
            .fold((0u64, 0u64), |acc, w| (acc.0 + w.0, acc.1 + w.1));
        (attended as f64 / held as f64 * 100.0).floor()
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

/// A week of raid attendance.
const WEEK_MS: i64 = 7 * DAY_MS;

/// How far apart two raids of the same name may sit and still be one raid
/// night: a false `/startraid` ended and redone (Aug 31 2026: nine seconds),
/// or a raid ended by mistake and restarted.
pub const SAME_RAID_GAP_MS: i64 = 30 * 60_000;

/// `(start, end)` — `/startraid` to `/endraid`, or to the last entry while
/// it runs.
fn raid_window(r: &Raid) -> (i64, i64) {
    let start = r
        .entries
        .first()
        .map_or(r.date_ms, |e| e.ts_ms.min(r.date_ms));
    let end = r
        .ended_ms
        .unwrap_or_else(|| r.entries.last().map_or(r.date_ms, |e| e.ts_ms));
    (start, end)
}

/// Same name (case and whitespace aside) and windows within
/// [`SAME_RAID_GAP_MS`] of each other.
pub fn same_raid(a: &Raid, b: &Raid) -> bool {
    if !a.name.trim().eq_ignore_ascii_case(b.name.trim()) {
        return false;
    }
    let (sa, ea) = raid_window(a);
    let (sb, eb) = raid_window(b);
    sb <= ea + SAME_RAID_GAP_MS && sa <= eb + SAME_RAID_GAP_MS
}
