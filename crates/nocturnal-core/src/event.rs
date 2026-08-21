//! Ledger events: immutable, past-tense facts. See `docs/events.md`.
//!
//! Payload schemas are append-only; changing a field's meaning requires a new
//! envelope `v` and explicit handling in the fold. Serialized `kind` strings
//! are pinned by tests and must never change.

use serde::{Deserialize, Serialize};

pub type GuildId = u64;
pub type PlayerId = u64;

/// Who caused an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
    User(PlayerId),
    /// Scheduler / worker driven (ticks, auction closes, migrations).
    System,
}

/// An EQ item attached to auctions and loot debits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Legacy stat-block text (embed body).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    /// Legacy icon URL (embed thumbnail).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}

/// Reference to a raid stored on ledger entries (legacy log shape).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RaidRef {
    pub raid_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Flavor {
    Short,
    Long,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Winner {
    pub player: PlayerId,
    pub amount: i64,
    pub for_main: bool,
}

/// A historical ledger line imported from the legacy bot (genesis only).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportedLogEntry {
    pub dkp: i64,
    pub comment: String,
    pub ts_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raid: Option<RaidRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<Item>,
}

/// Patch to per-guild behavioural config (`/configure`). Absent = unchanged.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConfigPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin_role: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raid_channel: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub second_raid_channel: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_channel: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auction_channel: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub long_auction_channel: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tick_duration_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raid_deprecation_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bid_time_s: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_bid: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_bid_to_lock_for_main: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub over_bid_to_win_main: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raidhelper_api_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload")]
pub enum Event {
    // -- players & characters ------------------------------------------------
    #[serde(rename = "player.character_linked")]
    CharacterLinked { player: PlayerId, character: String },
    #[serde(rename = "player.imported")]
    PlayerImported {
        player: PlayerId,
        balance: i64,
        characters: Vec<String>,
        creation_ts_ms: i64,
        log: Vec<ImportedLogEntry>,
    },

    // -- DKP ledger ----------------------------------------------------------
    #[serde(rename = "dkp.adjusted")]
    DkpAdjusted {
        player: PlayerId,
        /// Non-zero. Negative = debit; a debit never takes a balance below 0.
        delta: i64,
        comment: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raid: Option<RaidRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item: Option<Item>,
    },

    // -- raids ---------------------------------------------------------------
    #[serde(rename = "raid.started")]
    RaidStarted {
        raid_id: String,
        name: String,
        tick_interval_ms: i64,
        dkp_per_tick: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        event_id: Option<String>,
    },
    /// Non-tick attendance entry: Start, End (amount 0), /addraiddkp.
    #[serde(rename = "raid.awarded")]
    RaidAwarded {
        raid_id: String,
        players: Vec<PlayerId>,
        amount: i64,
        comment: String,
    },
    #[serde(rename = "raid.tick")]
    RaidTicked {
        raid_id: String,
        /// Monotonic per raid; the idempotence key (audit #35/#47).
        tick_no: u32,
        players: Vec<PlayerId>,
        amount: i64,
    },
    #[serde(rename = "raid.ended")]
    RaidEnded { raid_id: String, reason: String },
    #[serde(rename = "raid.imported")]
    RaidImported {
        raid_id: String,
        name: String,
        date_ms: i64,
        entries: Vec<ImportedAttendance>,
    },

    // -- auctions ------------------------------------------------------------
    #[serde(rename = "auction.opened")]
    AuctionOpened {
        auction_id: String,
        item: Item,
        flavor: Flavor,
        min_bid: i64,
        num_items: u32,
        min_bid_to_lock_for_main: i64,
        over_bid_to_win_main: i64,
        deadline_ts_ms: i64,
    },
    #[serde(rename = "auction.bid_placed")]
    BidPlaced {
        auction_id: String,
        player: PlayerId,
        amount: i64,
        for_main: bool,
        /// Attendance % captured at bid time (legacy semantics).
        attendance: f64,
    },
    #[serde(rename = "auction.bid_retracted")]
    BidRetracted {
        auction_id: String,
        player: PlayerId,
    },
    /// Bidding ends deterministically at this event's seq (audit #48).
    #[serde(rename = "auction.closed")]
    AuctionClosed { auction_id: String },
    /// The debit: winners are charged in this fold step (audit E2).
    #[serde(rename = "auction.finalized")]
    AuctionFinalized {
        auction_id: String,
        winners: Vec<Winner>,
        /// Tie-break RNG seed — makes any draw reproducible and auditable (E3).
        seed: u64,
    },
    #[serde(rename = "auction.cancelled")]
    AuctionCancelled { auction_id: String, reason: String },

    // -- config & telemetry provisioning -------------------------------------
    #[serde(rename = "config.updated")]
    ConfigUpdated { patch: ConfigPatch },
    #[serde(rename = "telemetry.token.issued")]
    TelemetryTokenIssued {
        username: String,
        token: String,
        role: String,
    },
    #[serde(rename = "telemetry.access.updated")]
    TelemetryAccessUpdated { username: String, role: String },
    #[serde(rename = "telemetry.token.revoked")]
    TelemetryTokenRevoked { username: String },
}

/// Attendance entry imported from a legacy raid document (genesis only).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportedAttendance {
    pub players: Vec<PlayerId>,
    pub comment: String,
    pub ts_ms: i64,
    pub amount: i64,
}

/// The envelope every persisted event wears. `seq` is contiguous and is the
/// clock of the system; `ts_ms` is informational.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub seq: u64,
    pub ts_ms: i64,
    pub guild: GuildId,
    pub actor: Actor,
    #[serde(default = "default_v")]
    pub v: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(flatten)]
    pub event: Event,
}

fn default_v() -> u8 {
    1
}

impl Event {
    /// The wire `kind` string (used for telemetry attributes and tests).
    pub fn kind(&self) -> &'static str {
        match self {
            Event::CharacterLinked { .. } => "player.character_linked",
            Event::PlayerImported { .. } => "player.imported",
            Event::DkpAdjusted { .. } => "dkp.adjusted",
            Event::RaidStarted { .. } => "raid.started",
            Event::RaidAwarded { .. } => "raid.awarded",
            Event::RaidTicked { .. } => "raid.tick",
            Event::RaidEnded { .. } => "raid.ended",
            Event::RaidImported { .. } => "raid.imported",
            Event::AuctionOpened { .. } => "auction.opened",
            Event::BidPlaced { .. } => "auction.bid_placed",
            Event::BidRetracted { .. } => "auction.bid_retracted",
            Event::AuctionClosed { .. } => "auction.closed",
            Event::AuctionFinalized { .. } => "auction.finalized",
            Event::AuctionCancelled { .. } => "auction.cancelled",
            Event::ConfigUpdated { .. } => "config.updated",
            Event::TelemetryTokenIssued { .. } => "telemetry.token.issued",
            Event::TelemetryAccessUpdated { .. } => "telemetry.access.updated",
            Event::TelemetryTokenRevoked { .. } => "telemetry.token.revoked",
        }
    }
}
