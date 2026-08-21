//! CLI: legacy backup JSONs → genesis WAL + verification report.
//!
//! Usage: nocturnal-migrate <players.json> <raids.json> <out-data-dir>
//!
//! Writes `<out-data-dir>/wal/` with the genesis events, prints the
//! verification report, exits non-zero on any balance mismatch (hazard B10:
//! cutover requires a clean report).

use std::process::ExitCode;

use nocturnal_core::Ledger;
use nocturnal_migrate::{genesis_commands, run_genesis, LegacyPlayer, LegacyRaid};
use nocturnal_store::Wal;

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().collect();
    let mut deprecation_days: Option<i64> = None;
    if let Some(i) = args.iter().position(|a| a == "--raid-deprecation-days") {
        let value = args.get(i + 1).and_then(|v| v.parse().ok());
        let Some(v) = value else {
            eprintln!("--raid-deprecation-days needs an integer");
            return ExitCode::from(2);
        };
        deprecation_days = Some(v);
        args.drain(i..=i + 1);
    }
    let [_, players_path, raids_path, out_dir] = &args[..] else {
        eprintln!(
            "usage: nocturnal-migrate <players.json> <raids.json> <out-data-dir> [--raid-deprecation-days N]"
        );
        return ExitCode::from(2);
    };

    let players: Vec<LegacyPlayer> = match read_json(players_path) {
        Ok(v) => v,
        Err(e) => return fail(&format!("{players_path}: {e}")),
    };
    let raids: Vec<LegacyRaid> = match read_json(raids_path) {
        Ok(v) => v,
        Err(e) => return fail(&format!("{raids_path}: {e}")),
    };

    let (guild, commands, warnings) = genesis_commands(&players, &raids, deprecation_days);
    for w in &warnings {
        eprintln!("warning: {w}");
    }

    // Timestamp of the snapshot: newest log entry (keeps genesis reproducible;
    // no wall clock involved).
    let now_ms = players
        .iter()
        .flat_map(|p| p.log.iter().map(|e| e.date))
        .max()
        .unwrap_or(0);

    let mut ledger = Ledger::new();
    let (envelopes, lines, mismatches) =
        run_genesis(&mut ledger, guild, &commands, &players, now_ms);

    let wal_dir = format!("{out_dir}/wal");
    let (mut wal, existing) = match Wal::open(&wal_dir) {
        Ok(v) => v,
        Err(e) => return fail(&format!("{wal_dir}: {e}")),
    };
    if !existing.is_empty() {
        return fail(&format!(
            "{wal_dir} is not empty — refusing to mix genesis into an existing ledger"
        ));
    }
    if let Err(e) = wal.append(&envelopes) {
        return fail(&format!("append: {e}"));
    }

    println!("== Nocturnal migration report ==");
    println!("guild:            {guild}");
    println!("raids imported:   {}", raids.len());
    println!("players imported: {}", lines.len());
    println!("genesis events:   {}", envelopes.len());
    println!("warnings:         {}", warnings.len());
    let total: i64 = lines.iter().map(|l| l.snapshot).sum();
    println!("total DKP:        {total}");
    if mismatches == 0 {
        println!(
            "balance check:    OK — all {} replayed balances match the snapshot",
            lines.len()
        );
        ExitCode::SUCCESS
    } else {
        println!("balance check:    FAILED — {mismatches} mismatches:");
        for l in lines.iter().filter(|l| l.replayed != l.snapshot) {
            println!(
                "  player {}: snapshot {} != replayed {}",
                l.player, l.snapshot, l.replayed
            );
        }
        ExitCode::FAILURE
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &str) -> Result<T, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

fn fail(msg: &str) -> ExitCode {
    eprintln!("error: {msg}");
    ExitCode::FAILURE
}
