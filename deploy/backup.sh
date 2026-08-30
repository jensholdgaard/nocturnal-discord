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

# Members' personal Perses projects (the u-<name> dashboards /dpstoken grants)
# live only in Perses' local database — the guild dashboards are provisioned
# from git, but these are user-created and restorable from nowhere else. The
# plugins trees are 380 MB of re-installable archives and stay out.
pout=""
if [ -d /var/lib/perses/data ]; then
  pout="$DEST/perses-$stamp.tar.gz"
  tar -C /var/lib/perses -czf "$pout.tmp" data && mv "$pout.tmp" "$pout"
  tar -tzf "$pout" >/dev/null && echo "wrote $pout"
  ls -1t "$DEST"/perses-*.tar.gz 2>/dev/null | tail -n +$((KEEP + 1)) | xargs -r rm --
fi
echo "wrote $out ($(du -h "$out" | cut -f1))"

# Prove it is readable before trusting it.
tar -tzf "$out" >/dev/null
echo "verified $out"

ls -1t "$DEST"/nocturnal-*.tar.gz | tail -n +$((KEEP + 1)) | xargs -r rm --
echo "kept the newest $KEEP backups"

# Off-site copy. The archive mirrors compacted Parquet, but the WAL tail —
# everything since the last compaction — exists only on this disk without
# this. curl signs SigV4 natively, so no CLI or SDK is needed; credentials
# come from the same env file the service reads. Failure is loud but does
# not fail the local backup above: an unreachable bucket must never stop
# the tarball existing at all.
# Only the AWS_ lines. Sourcing the whole file is wrong twice over: systemd
# EnvironmentFile syntax allows unquoted spaces the shell does not (the OTLP
# header line *executed its own bearer token* as a command and printed it to
# the journal), and this script has no business seeing the other secrets.
if [ -f /etc/nocturnal/env ]; then
  while IFS= read -r line; do
    case "$line" in AWS_*=*) export "$line" ;; esac
  done < /etc/nocturnal/env
fi
if [ -n "${AWS_ACCESS_KEY_ID:-}" ] && [ -n "${AWS_ENDPOINT:-}" ]; then
  host="${AWS_ENDPOINT#https://}"
  for f in "$out" $pout; do
    [ -f "$f" ] || continue
    if curl -fsS --aws-sigv4 "aws:amz:${AWS_REGION:-fsn1}:s3" --user "${AWS_ACCESS_KEY_ID}:${AWS_SECRET_ACCESS_KEY}" -T "$f" "https://nocturnal-ledger.${host}/backups/$(basename "$f")"; then
      echo "off-site copy: backups/$(basename "$f")"
    else
      echo "off-site copy FAILED for $(basename "$f") (local backup unaffected)" >&2
    fi
  done
else
  echo "off-site copy skipped: no AWS_* in the environment" >&2
fi
