// Generated from semconv/ by OTel Weaver — do not edit.
// Regenerate: weaver registry generate -r semconv -t templates rust crates/nocturnal-telemetry/src/

/// Attribute keys.
pub mod attr {
    pub const NOCTURNAL_AUCTION_FLAVOR: &str = "nocturnal.auction.flavor";
    pub const NOCTURNAL_AUCTION_ID: &str = "nocturnal.auction.id";
    pub const NOCTURNAL_COMMAND: &str = "nocturnal.command";
    pub const NOCTURNAL_COMPACTION_PARTITION: &str = "nocturnal.compaction.partition";
    pub const NOCTURNAL_DECISION_OUTCOME: &str = "nocturnal.decision.outcome";
    pub const NOCTURNAL_DECISION_REJECTION: &str = "nocturnal.decision.rejection";
    pub const NOCTURNAL_DISCORD_RATELIMIT_GLOBAL: &str = "nocturnal.discord.ratelimit.global";
    pub const NOCTURNAL_EVENT_KIND: &str = "nocturnal.event.kind";
    pub const NOCTURNAL_EVENT_SEQ: &str = "nocturnal.event.seq";
    pub const NOCTURNAL_EVENT_VERSION: &str = "nocturnal.event.version";
    pub const NOCTURNAL_GUILD_ID: &str = "nocturnal.guild.id";
    pub const NOCTURNAL_INTERACTION_KIND: &str = "nocturnal.interaction.kind";
    pub const NOCTURNAL_PROVISION_OPERATION: &str = "nocturnal.provision.operation";
    pub const NOCTURNAL_RAID_ID: &str = "nocturnal.raid.id";
    pub const NOCTURNAL_RAID_TICK_NO: &str = "nocturnal.raid.tick_no";
    pub const NOCTURNAL_WAL_SEGMENT: &str = "nocturnal.wal.segment";
}

/// Metric names.
pub mod metric {
    pub const NOCTURNAL_AUCTIONS_ACTIVE: &str = "nocturnal.auctions.active";
    pub const NOCTURNAL_COMMANDS: &str = "nocturnal.commands";
    pub const NOCTURNAL_COMPACTION_RUNS: &str = "nocturnal.compaction.runs";
    pub const NOCTURNAL_DISCORD_GATEWAY_LATENCY: &str = "nocturnal.discord.gateway.latency";
    pub const NOCTURNAL_DISCORD_RATELIMIT_DELAY_DURATION: &str =
        "nocturnal.discord.ratelimit.delay.duration";
    pub const NOCTURNAL_DISCORD_RATELIMIT_DELAYS: &str = "nocturnal.discord.ratelimit.delays";
    pub const NOCTURNAL_DISCORD_RECONNECTS: &str = "nocturnal.discord.reconnects";
    pub const NOCTURNAL_INTERACTION_ACK_DURATION: &str = "nocturnal.interaction.ack.duration";
    pub const NOCTURNAL_INTERACTION_COMMIT_DURATION: &str = "nocturnal.interaction.commit.duration";
    pub const NOCTURNAL_LEDGER_EVENTS: &str = "nocturnal.ledger.events";
    pub const NOCTURNAL_LEDGER_SEQ: &str = "nocturnal.ledger.seq";
    pub const NOCTURNAL_PROVISION_OPERATIONS: &str = "nocturnal.provision.operations";
    pub const NOCTURNAL_RAIDS_ACTIVE: &str = "nocturnal.raids.active";
    pub const NOCTURNAL_SCHEDULER_DRIFT: &str = "nocturnal.scheduler.drift";
    pub const NOCTURNAL_SCHEDULER_HEARTBEAT: &str = "nocturnal.scheduler.heartbeat";
    pub const NOCTURNAL_WAL_FSYNC_DURATION: &str = "nocturnal.wal.fsync.duration";
    pub const NOCTURNAL_WAL_SIZE: &str = "nocturnal.wal.size";
}
