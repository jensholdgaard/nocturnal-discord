//! The inverse of the import: the ledger rendered back into the legacy
//! `{guild}_players.json` / `{guild}_raids.json` documents.
//!
//! This is not a convenience. `/backup`'s output is an interface: the guild's
//! roster page reads these two files, and the legacy bot is what taught it the
//! shape. So the shapes here are *the same structs the importer parses* —
//! there is one definition of the format, and a round-trip test proves the
//! rewrite gives back what it was given.
//!
//! Ordering is ours, not Mongo's: players by id, raids by date. Nothing in the
//! format implies an order, and a stable one makes two backups diffable.

use serde::Serialize;

use nocturnal_core::state::{GuildState, Raid};
use nocturnal_core::GuildId;

use crate::{
    LegacyAttendance, LegacyItem, LegacyLogEntry, LegacyPlayer, LegacyRaid, LegacyRaidRef,
};

/// Render `{guild}_players.json`.
pub fn players(g: &GuildState, guild: GuildId) -> Vec<LegacyPlayer> {
    g.players
        .iter()
        .map(|(id, p)| LegacyPlayer {
            id: p.legacy_id.clone(),
            player: id.to_string(),
            guild: guild.to_string(),
            current: p.balance,
            creation_date: p.creation_ts_ms,
            characters: p.characters.clone(),
            log: p
                .log
                .iter()
                .map(|e| LegacyLogEntry {
                    dkp: e.dkp,
                    comment: Some(e.comment.clone()),
                    date: e.ts_ms,
                    // Always present, `null` when the entry has no raid — the
                    // legacy documents carry the key on every entry.
                    raid: Some(e.raid.as_ref().map(|r| LegacyRaidRef {
                        id: Some(r.raid_id.clone()),
                        name: Some(r.name.clone()),
                    })),
                    item: e.item.as_ref().map(|i| LegacyItem {
                        id: Some(serde_json::Value::String(i.id.clone())),
                        name: Some(i.name.clone()),
                        url: i.url.clone(),
                        data: i.data.clone(),
                        image: i.image.clone(),
                    }),
                })
                .collect(),
        })
        .collect()
}

/// Render `{guild}_raids.json`.
///
/// `deprecated` is stored in legacy and derived here (hazard-free: there are
/// no deprecation events, the window is config), so it is computed against
/// `now_ms` — the same rule attendance uses.
pub fn raids(g: &GuildState, guild: GuildId, now_ms: i64) -> Vec<LegacyRaid> {
    let mut out: Vec<(&String, &Raid)> = g.raids.iter().collect();
    out.sort_by_key(|(id, r)| (r.date_ms, (*id).clone()));
    out.into_iter()
        .map(|(id, r)| LegacyRaid {
            id: Some(id.clone()),
            guild: guild.to_string(),
            name: r.name.clone(),
            date: r.date_ms,
            attendance: r
                .entries
                .iter()
                .map(|e| LegacyAttendance {
                    players: e.players.iter().map(|p| p.to_string()).collect(),
                    comment: Some(e.comment.clone()),
                    date: e.ts_ms,
                    dkps: e.amount,
                })
                .collect(),
            tick_duration: r.tick_interval_ms,
            dkps_per_tick: r.dkp_per_tick,
            event_id: r.event_id.clone(),
            active: r.active,
            deprecated: !g.raid_counts(r, now_ms),
        })
        .collect()
}

/// The two files `/backup` ships, named as the legacy bot named them (the
/// importer reads these names, and so does whatever else has been pointed at
/// them over the years).
pub fn files(
    g: &GuildState,
    guild: GuildId,
    now_ms: i64,
) -> Result<Vec<(String, Vec<u8>)>, serde_json::Error> {
    Ok(vec![
        (
            format!("{guild}_players.json"),
            to_json(&players(g, guild))?,
        ),
        (
            format!("{guild}_raids.json"),
            to_json(&raids(g, guild, now_ms))?,
        ),
    ])
}

fn to_json<T: Serialize>(v: &T) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(v)
}
