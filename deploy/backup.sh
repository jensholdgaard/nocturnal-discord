#!/usr/bin/env bash
# Scheduled backup: the ledger is one directory, so a backup is one tarball.
# Restore is untar + start — rehearsed in docs/runbook.md.
set -euo pipefail

DATA_DIR="${NOCTURNAL_DATA_DIR:-/var/lib/nocturnal}"
DEST="${NOCTURNAL_BACKUP_DIR:-/var/backups/nocturnal}"
KEEP="${NOCTURNAL_BACKUP_KEEP:-14}"

mkdir -p "$DEST"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
out="$DEST/nocturnal-$stamp.tar.gz"

# The WAL is append-only and every record is checksummed, so a tar taken while
# the bot is running is consistent: a record either made it or it did not, and
# replay truncates a torn tail exactly as it would after a crash.
tar -C "$DATA_DIR" -czf "$out.tmp" events wal
mv "$out.tmp" "$out"
echo "wrote $out ($(du -h "$out" | cut -f1))"

# Prove it is readable before trusting it.
tar -tzf "$out" >/dev/null
echo "verified $out"

ls -1t "$DEST"/nocturnal-*.tar.gz | tail -n +$((KEEP + 1)) | xargs -r rm --
echo "kept the newest $KEEP backups"
