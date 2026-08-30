//! `/backup` — the ledger handed back as the two legacy documents.
//!
//! This is not the disaster-recovery backup; `deploy/backup.sh` and its
//! nightly timer own that, and a tar of the data directory is both smaller and
//! exactly restorable. This command exists because its *output* is an
//! interface: the guild's roster page reads `{guild}_players.json` and
//! `{guild}_raids.json`, and the legacy bot is what taught it that shape.
//! Losing the command would take the roster page down with it.
//!
//! The rendering happens on the writer thread (~230 ms for the guild's real
//! history) because that is the only place a consistent snapshot exists. The
//! compression — seconds, and CPU-bound — does not.

use poise::serenity_prelude as serenity;

use crate::discord::{Context, Data, Error};

/// Discord refuses attachments above the guild's boost-tier limit. The floor
/// is 10 MiB and we cannot know the tier from here without another fetch, so
/// the check is against the floor: better a clear refusal naming the nightly
/// tarball than a Discord error the officer has to interpret.
const ATTACHMENT_LIMIT: usize = 10 * 1024 * 1024;

/// Zip the rendered documents. Deflate at default level: the guild's 71 MB of
/// JSON becomes about 5 MB, and the level-9 saving is under 2 % for several
/// times the CPU.
fn zip_files(files: &[(String, Vec<u8>)]) -> std::io::Result<Vec<u8>> {
    use std::io::Write;
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options: zip::write::FileOptions<()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in files {
        zip.start_file(name, options)?;
        zip.write_all(bytes)?;
    }
    Ok(zip.finish()?.into_inner())
}

fn human(bytes: usize) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.0} kB", bytes as f64 / 1024.0)
    }
}

/// Export the DKP ledger as the legacy players/raids JSON pair, zipped.
#[tracing::instrument(name = "command.backup", skip_all, fields(otel.kind = "server"))]
#[poise::command(
    slash_command,
    ephemeral,
    rename = "backup",
    default_member_permissions = "ADMINISTRATOR"
)]
pub async fn backup(ctx: Context<'_>) -> Result<(), Error> {
    let ledger_guild = crate::discord::require_guild(&ctx)?;
    crate::discord::ack_ephemeral(&ctx).await?;

    let now_ms = crate::discord::chrono_now_ms();
    let rendered = ctx
        .data()
        .driver
        .query(move |l| {
            l.state()
                .guild(ledger_guild)
                .map(|g| nocturnal_migrate::export::files(g, ledger_guild, now_ms))
        })
        .await;

    let files = match rendered {
        Some(Ok(files)) => files,
        Some(Err(e)) => {
            tracing::warn!(
                { nocturnal_telemetry::attr::NOCTURNAL_ERROR_MESSAGE } = %e,
                "backup export failed"
            );
            ctx.say(":no_entry: Could not render the backup — check the logs.")
                .await?;
            return Ok(());
        }
        None => {
            ctx.say(":no_entry: Nothing to back up yet — this guild has no ledger.")
                .await?;
            return Ok(());
        }
    };

    let raw: usize = files.iter().map(|(_, b)| b.len()).sum();
    let names: Vec<String> = files.iter().map(|(n, _)| n.clone()).collect();
    let zipped = match tokio::task::spawn_blocking(move || zip_files(&files)).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(e)) => {
            tracing::warn!(
                { nocturnal_telemetry::attr::NOCTURNAL_ERROR_MESSAGE } = %e,
                "backup compression failed"
            );
            ctx.say(":no_entry: Could not compress the backup — check the logs.")
                .await?;
            return Ok(());
        }
        Err(e) => {
            tracing::warn!(
                { nocturnal_telemetry::attr::NOCTURNAL_ERROR_MESSAGE } = %e,
                "backup task failed"
            );
            ctx.say(":no_entry: The backup task failed — check the logs.")
                .await?;
            return Ok(());
        }
    };

    if zipped.len() > ATTACHMENT_LIMIT {
        ctx.say(format!(
            ":no_entry: The backup is {} — larger than Discord accepts here. \
             The nightly tarball on the host has the same data (`deploy/backup.sh`).",
            human(zipped.len())
        ))
        .await?;
        return Ok(());
    }

    // `backup.zip`, exactly as the legacy command named it. Anything that
    // unpacks one of these looks for that name and for the two plain entries
    // inside it, so the filename is part of the contract, not decoration.
    let filename = "backup.zip".to_owned();
    ctx.send(
        poise::CreateReply::default()
            .content(format!(
                "Backup created — `{}`, {} zipped, {} uncompressed.",
                names.join("`, `"),
                human(zipped.len()),
                human(raw)
            ))
            .attachment(serenity::CreateAttachment::bytes(zipped, filename))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![backup()]
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // tests assert; unwrap is the assertion
mod tests {
    use super::*;

    #[test]
    fn the_zip_holds_both_documents_under_their_legacy_names() {
        let files = vec![
            ("players.json".to_owned(), b"[{\"player\":\"1\"}]".to_vec()),
            ("raids.json".to_owned(), b"[]".to_vec()),
        ];
        let bytes = zip_files(&files).unwrap();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        assert_eq!(archive.len(), 2);
        let names: Vec<String> = archive.file_names().map(str::to_owned).collect();
        assert!(names.contains(&"players.json".to_owned()), "{names:?}");
        assert!(names.contains(&"raids.json".to_owned()), "{names:?}");

        use std::io::Read;
        let mut s = String::new();
        archive
            .by_name("players.json")
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();
        assert_eq!(
            s, "[{\"player\":\"1\"}]",
            "the payload survived the round trip"
        );
    }

    #[test]
    fn sizes_read_as_sizes() {
        assert_eq!(human(5_242_880), "5.0 MB");
        assert_eq!(human(2048), "2 kB");
    }
}
