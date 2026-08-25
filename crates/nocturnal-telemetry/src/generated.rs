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
    pub const NOCTURNAL_SCHEDULER_TIMER: &str = "nocturnal.scheduler.timer";
    pub const NOCTURNAL_WAL_SEGMENT: &str = "nocturnal.wal.segment";
    pub const NOCTURNAL_ARCHIVE_BUCKET: &str = "nocturnal.archive.bucket";
    pub const NOCTURNAL_ARCHIVE_PARTITIONS_RESTORED: &str = "nocturnal.archive.partitions_restored";
    pub const NOCTURNAL_ARCHIVE_PREFIX: &str = "nocturnal.archive.prefix";
    pub const NOCTURNAL_AUCTION_OPEN_COUNT: &str = "nocturnal.auction.open.count";
    pub const NOCTURNAL_AUCTION_TIMER_ACTION: &str = "nocturnal.auction.timer.action";
    pub const NOCTURNAL_BELL_PLAYED: &str = "nocturnal.bell.played";
    pub const NOCTURNAL_BELL_POSITION: &str = "nocturnal.bell.position";
    pub const NOCTURNAL_BELL_STATE: &str = "nocturnal.bell.state";
    pub const NOCTURNAL_BID_ACCEPTED: &str = "nocturnal.bid.accepted";
    pub const NOCTURNAL_BID_AMOUNT: &str = "nocturnal.bid.amount";
    pub const NOCTURNAL_BID_FOR_MAIN: &str = "nocturnal.bid.for_main";
    pub const NOCTURNAL_BID_REPLY_LENGTH: &str = "nocturnal.bid.reply.length";
    pub const NOCTURNAL_COMPACTION_EVENT_COUNT: &str = "nocturnal.compaction.event.count";
    pub const NOCTURNAL_COMPACTION_INTERVAL: &str = "nocturnal.compaction.interval";
    pub const NOCTURNAL_COMPACTION_PARTITIONS: &str = "nocturnal.compaction.partitions";
    pub const NOCTURNAL_COMPACTION_SEGMENTS_DELETED: &str = "nocturnal.compaction.segments_deleted";
    pub const NOCTURNAL_DISCORD_CHANNEL_ID: &str = "nocturnal.discord.channel.id";
    pub const NOCTURNAL_DISCORD_CUSTOM_ID: &str = "nocturnal.discord.custom_id";
    pub const NOCTURNAL_DISCORD_MESSAGE_LENGTH: &str = "nocturnal.discord.message.length";
    pub const NOCTURNAL_DISCORD_RATELIMIT_DELAY: &str = "nocturnal.discord.ratelimit.delay";
    pub const NOCTURNAL_DISCORD_USER_ID: &str = "nocturnal.discord.user.id";
    pub const NOCTURNAL_DISCORD_USER_NAME: &str = "nocturnal.discord.user.name";
    pub const NOCTURNAL_ERROR_MESSAGE: &str = "nocturnal.error.message";
    pub const NOCTURNAL_GUILD_REMAP_FROM: &str = "nocturnal.guild.remap.from";
    pub const NOCTURNAL_GUILD_REMAP_TO: &str = "nocturnal.guild.remap.to";
    pub const NOCTURNAL_HEALTH_BIND: &str = "nocturnal.health.bind";
    pub const NOCTURNAL_PLAYER_ID: &str = "nocturnal.player.id";
    pub const NOCTURNAL_PROVISION_FILES_REMOVED: &str = "nocturnal.provision.files_removed";
    pub const NOCTURNAL_PROVISION_FILES_WRITTEN: &str = "nocturnal.provision.files_written";
    pub const NOCTURNAL_PROVISION_TOKENS_REWRITTEN: &str = "nocturnal.provision.tokens_rewritten";
    pub const NOCTURNAL_RAID_AWARD_SUMMARY: &str = "nocturnal.raid.award.summary";
    pub const NOCTURNAL_RAID_EVENT_ID: &str = "nocturnal.raid.event_id";
    pub const NOCTURNAL_RAID_NAME: &str = "nocturnal.raid.name";
    pub const NOCTURNAL_RAID_TICK_PLAYER_COUNT: &str = "nocturnal.raid.tick.player.count";
    pub const NOCTURNAL_REPLAY_DURATION: &str = "nocturnal.replay.duration";
    pub const NOCTURNAL_REPLAY_EVENT_COUNT: &str = "nocturnal.replay.event.count";
    pub const NOCTURNAL_STRESSTEST_ACCEPTED_COUNT: &str = "nocturnal.stresstest.accepted.count";
    pub const NOCTURNAL_STRESSTEST_AUCTION_COUNT: &str = "nocturnal.stresstest.auction.count";
    pub const NOCTURNAL_STRESSTEST_BIDDER_COUNT: &str = "nocturnal.stresstest.bidder.count";
    pub const NOCTURNAL_STRESSTEST_DURATION: &str = "nocturnal.stresstest.duration";
    pub const NOCTURNAL_STRESSTEST_LOOKUP_COUNT: &str = "nocturnal.stresstest.lookup.count";
    pub const NOCTURNAL_STRESSTEST_REJECTED_COUNT: &str = "nocturnal.stresstest.rejected.count";
    pub const NOCTURNAL_TELEMETRY_ENDPOINT: &str = "nocturnal.telemetry.endpoint";
    pub const NOCTURNAL_TELEMETRY_PROTOCOL: &str = "nocturnal.telemetry.protocol";
    pub const FILE_PATH: &str = "file.path";
    pub const HTTP_REQUEST_METHOD: &str = "http.request.method";
    pub const URL_PATH: &str = "url.path";
    pub const CPU_MODE: &str = "cpu.mode";
    pub const SYSTEM_FILESYSTEM_MOUNTPOINT: &str = "system.filesystem.mountpoint";
    pub const SYSTEM_FILESYSTEM_STATE: &str = "system.filesystem.state";
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
    pub const PROCESS_CPU_TIME: &str = "process.cpu.time";
    pub const PROCESS_MEMORY_USAGE: &str = "process.memory.usage";
    pub const PROCESS_MEMORY_VIRTUAL: &str = "process.memory.virtual";
    pub const PROCESS_OPEN_FILE_DESCRIPTORS: &str = "process.open_file_descriptors";
    pub const PROCESS_THREAD_COUNT: &str = "process.thread.count";
    pub const PROCESS_UPTIME: &str = "process.uptime";
    pub const SYSTEM_FILESYSTEM_USAGE: &str = "system.filesystem.usage";
}
