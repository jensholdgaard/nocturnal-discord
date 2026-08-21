//! Commands: validated *requests*. A command never reaches the log — the
//! decide step turns it into zero-or-more events or a typed [`crate::Rejection`].

use crate::event::{
    Actor, ConfigPatch, Flavor, GuildId, ImportedAttendance, ImportedLogEntry, Item, PlayerId,
};

/// Ambient facts about the request, supplied by the driver (single writer).
#[derive(Debug, Clone, Copy)]
pub struct Ctx {
    pub guild: GuildId,
    pub actor: Actor,
    pub now_ms: i64,
}

#[derive(Debug, Clone)]
pub enum Command {
    LinkCharacter {
        player: PlayerId,
        character: String,
    },
    /// `/adddkp` (+) and `/removedkp` (−); the active raid is attached
    /// automatically, mirroring the legacy commands.
    AdjustDkp {
        player: PlayerId,
        delta: i64,
        comment: String,
        item: Option<Item>,
    },
    /// One `/parsedkps` character line; the driver resolves per character.
    AdjustByCharacter {
        character: String,
        delta: i64,
        comment: String,
    },
    StartRaid {
        raid_id: String,
        name: String,
        tick_interval_ms: i64,
        dkp_per_tick: i64,
        players_present: Vec<PlayerId>,
        event_id: Option<String>,
    },
    /// Scheduler-driven raid tick for the active raid.
    Tick {
        players_present: Vec<PlayerId>,
    },
    /// `/addraiddkp`: award everyone present, with an attendance entry.
    AwardRaid {
        players: Vec<PlayerId>,
        amount: i64,
        comment: String,
    },
    EndRaid {
        players_present: Vec<PlayerId>,
        reason: String,
    },
    OpenAuction {
        auction_id: String,
        item: Item,
        flavor: Flavor,
        min_bid: i64,
        num_items: u32,
        min_bid_to_lock_for_main: i64,
        over_bid_to_win_main: i64,
        duration_ms: i64,
    },
    PlaceBid {
        auction_id: String,
        player: PlayerId,
        amount: i64,
        for_main: bool,
    },
    RetractBid {
        auction_id: String,
        player: PlayerId,
    },
    CloseAuction {
        auction_id: String,
    },
    /// Debits the winners (short: officer confirm; long: scheduler).
    FinalizeAuction {
        auction_id: String,
        seed: u64,
    },
    CancelAuction {
        auction_id: String,
        reason: String,
    },
    UpdateConfig {
        patch: ConfigPatch,
    },
    // -- telemetry provisioning (M8) --
    IssueToken {
        username: String,
        token: String,
        role: String,
    },
    RefreshAccess {
        username: String,
        role: String,
    },
    RevokeToken {
        username: String,
    },
    // -- genesis (migration only) --
    ImportPlayer {
        player: PlayerId,
        balance: i64,
        characters: Vec<String>,
        creation_ts_ms: i64,
        log: Vec<ImportedLogEntry>,
    },
    ImportRaid {
        raid_id: String,
        name: String,
        date_ms: i64,
        entries: Vec<ImportedAttendance>,
    },
}

impl Command {
    /// Low-cardinality label for telemetry (`nocturnal.command`).
    pub fn kind(&self) -> &'static str {
        match self {
            Command::LinkCharacter { .. } => "link_character",
            Command::AdjustDkp { .. } => "adjust_dkp",
            Command::AdjustByCharacter { .. } => "adjust_by_character",
            Command::StartRaid { .. } => "start_raid",
            Command::Tick { .. } => "tick",
            Command::AwardRaid { .. } => "award_raid",
            Command::EndRaid { .. } => "end_raid",
            Command::OpenAuction { .. } => "open_auction",
            Command::PlaceBid { .. } => "place_bid",
            Command::RetractBid { .. } => "retract_bid",
            Command::CloseAuction { .. } => "close_auction",
            Command::FinalizeAuction { .. } => "finalize_auction",
            Command::CancelAuction { .. } => "cancel_auction",
            Command::UpdateConfig { .. } => "update_config",
            Command::IssueToken { .. } => "issue_token",
            Command::RefreshAccess { .. } => "refresh_access",
            Command::RevokeToken { .. } => "revoke_token",
            Command::ImportPlayer { .. } => "import_player",
            Command::ImportRaid { .. } => "import_raid",
        }
    }
}
