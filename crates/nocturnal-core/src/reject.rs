//! Typed rejections: expected, non-error outcomes of the decide step. The
//! Discord layer turns these into the bot's flavor text ("DKP Bot scowls…").

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    PlayerNotFound,
    CharacterNotRegistered {
        character: String,
    },
    CharacterAlreadyRegistered {
        character: String,
    },
    InvalidAmount,
    /// `available` = balance minus `committed` (standing bids on other open
    /// auctions). Carrying both lets the Discord layer explain the
    /// cross-auction reservation instead of just refusing.
    InsufficientBalance {
        available: i64,
        committed: i64,
        needed: i64,
    },
    RaidAlreadyActive {
        name: String,
    },
    NoActiveRaid,
    RaidNotFound,
    TickTooSoon,
    AuctionNotFound,
    AuctionIdTaken,
    AuctionNotActive,
    AuctionNotClosed,
    BidBelowMinimum {
        min_bid: i64,
    },
    AlreadyProvisioned {
        username: String,
    },
    NotProvisioned {
        username: String,
    },
    /// `/roster edit` or `remove` for a character the player has not added,
    /// or `/roster add` for one they already have — the name is the key.
    RosterCharacterMissing {
        name: String,
    },
    RosterCharacterExists {
        name: String,
    },
    /// A roster value the ledger refuses: unknown class, level or AA out of
    /// range, a profile link that is not a quarmy.com page.
    InvalidRosterEntry {
        field: &'static str,
        reason: String,
    },
    /// A `/configure` value the ledger refuses to hold. Carries the option
    /// name so the officer is told which one, not just that something was
    /// wrong.
    InvalidConfig {
        setting: &'static str,
        reason: String,
    },
}

impl Rejection {
    /// Low-cardinality slug for the `nocturnal.decision.rejection` attribute.
    pub fn slug(&self) -> &'static str {
        match self {
            Rejection::PlayerNotFound => "player_not_found",
            Rejection::CharacterNotRegistered { .. } => "character_not_registered",
            Rejection::CharacterAlreadyRegistered { .. } => "character_already_registered",
            Rejection::InvalidAmount => "invalid_amount",
            Rejection::InsufficientBalance { .. } => "insufficient_balance",
            Rejection::RaidAlreadyActive { .. } => "raid_already_active",
            Rejection::NoActiveRaid => "no_active_raid",
            Rejection::RaidNotFound => "raid_not_found",
            Rejection::TickTooSoon => "tick_too_soon",
            Rejection::AuctionNotFound => "auction_not_found",
            Rejection::AuctionIdTaken => "auction_id_taken",
            Rejection::AuctionNotActive => "auction_not_active",
            Rejection::AuctionNotClosed => "auction_not_closed",
            Rejection::BidBelowMinimum { .. } => "bid_below_minimum",
            Rejection::AlreadyProvisioned { .. } => "already_provisioned",
            Rejection::NotProvisioned { .. } => "not_provisioned",
            Rejection::InvalidConfig { .. } => "invalid_config",
            Rejection::RosterCharacterMissing { .. } => "roster_character_missing",
            Rejection::RosterCharacterExists { .. } => "roster_character_exists",
            Rejection::InvalidRosterEntry { .. } => "invalid_roster_entry",
        }
    }
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.slug())
    }
}
