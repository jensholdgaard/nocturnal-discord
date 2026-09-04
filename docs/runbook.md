# Runbook

Operating Nocturnal. Everything here has been rehearsed; where it hasn't, it
says so.

## What the system is, in one paragraph

One static binary and one directory. The directory (`/var/lib/nocturnal` on
the VM, `/data` in the container) holds `wal/` — append-only, checksummed,
human-readable JSONL — and `events/` — the same events compacted into
month-partitioned Parquet. That directory *is* the DKP ledger; balances,
auctions and raids are projections rebuilt by replaying it at boot. There is
no database. Config comes from a TOML file plus `NOCTURNAL_*` overrides;
telemetry comes from the standard `OTEL_*` environment; secrets come from the
environment only.

## Health

| Check | Meaning |
|---|---|
| `curl localhost:8090/healthz` | the process is alive |
| `curl localhost:8090/readyz` | the ledger has replayed and the gateway is connected |
| `nocturnal --check` | config parses, data dir opens, ledger replays — then exits |
| `nocturnal --print-config` | the resolved config, secrets redacted |
| `deploy/verify-vm.sh` | services, readiness, export errors, metrics and traces in one pass |

Dashboard: Perses → project **Nocturnal Bot** → *Overview*. The numbers that
matter are commit p95 (must stay far below Discord's 3-second window), storage
errors (should be zero — anything else is a page), and the pre-429 rate-limit
delay panel.

## Deploy

    deploy/install-vm.sh          # idempotent: binary, config, unit, dashboard

It never reseeds an existing ledger. A deploy is a binary swap plus a restart;
boot is process start plus replay (~2 s for the guild's full history). Crash
restart is safe by design — the restart policy is the supervisor — and an
exclusive lock on the data directory means two instances can never both write:
the second refuses to start.

## Backup and restore

    deploy/backup.sh              # tarball + verification + retention

The nightly timer (`deploy/nocturnal-backup.{service,timer}`) runs it. Taking
a backup while the bot is running is safe: the WAL is append-only and every
record is checksummed, so a record either made it or it did not.

Restore:

1. `systemctl stop nocturnal`
2. `rm -rf /var/lib/nocturnal/{events,wal}`
3. `tar -C /var/lib/nocturnal -xzf <backup>.tar.gz`
4. `chown -R nocturnal:nocturnal /var/lib/nocturnal`
5. `sudo -u nocturnal nocturnal --config /etc/nocturnal/nocturnal.yaml --check`
6. `systemctl start nocturnal`

**RPO:** one fsynced event. A command is acknowledged only after its events are
on disk, so nothing acknowledged is ever lost; anything in flight during a
crash simply never happened. (The fsync guarantee of the host volume itself is
verified per host — hazard B7.)

## Losing the VM

If the archive is configured, the ledger's compacted history is in object
storage. Rebuilding: install on a fresh host, put the same `[archive]` bucket
and AWS credentials in place, and start. Boot downloads every partition it is
missing before replay. What is *not* in the archive is the current WAL tail —
events since the last compaction — so pair this with the nightly backup
(which does include the WAL) for a complete recovery.

## Settling a DKP dispute

The ledger is greppable, which is the point:

    grep -h '"player":211154610876841984' /var/lib/nocturnal/wal/*.jsonl | tail -20

For history already compacted into Parquet, query it with DataFusion or any
Parquet reader; the columns are `seq, ts_ms, guild, kind, json`. Every event
carries who did it (`actor`), when (`ts_ms`), and its position in the ledger
(`seq`). An auction's whole story — opened, each bid, closed, the tie-break
seed, who was charged — is the events sharing its `auction_id`.

## Common situations

**A command was refused and the officer disagrees.** Refusals are typed and
logged with a reason (`insufficient_balance`, `raid_already_active`, …). The
reason is in the bot's reply and in the logs; no event was written, so nothing
needs undoing.

**An auction looks stuck.** Auctions are closed and finalized by the scheduler
from ledger state, not by a timer that can be lost. If an embed looks stale,
restarting re-posts every open auction and rebuilds its buttons; the ledger is
unaffected.

**A raid tick was missed.** Ticks are due-checked against the last attendance
entry, so a missed cycle awards on the next one; `tick_no` makes a repeat
impossible.

**The bot restarted mid-auction.** Nothing to do. Buttons carry their auction
id, open auctions re-post on boot, and bids already accepted are on disk.

**The bell is silent.** Run `/belltest` — it reports Connect and Speak. If
permissions are fine, run `nocturnal --bell-test <guild>:<channel>` on the
host with `NOCTURNAL_LOG=info,songbird=debug`: a `4017 E2EE/DAVE protocol
required` close means the voice library is behind Discord's protocol. Either
way the auction is unaffected; the bell cannot touch it.

**Telemetry stopped.** Export is entirely `OTEL_*` environment; check
`/etc/nocturnal/env` and the gateway collector. The bot runs identically with
export off — it just logs locally.

## Rolling back

Keep the previous binary (`/usr/local/bin/nocturnal.prev` if you moved it
aside). The event log is forward-compatible: older binaries read events they
know, and every payload change is additive with a version bump, so a rollback
does not corrupt anything. A rollback across a *new event kind* would leave
that kind unhandled — check the release notes, which list any new `(kind, v)`.

## Is the bot actually alive? (readiness, heartbeat, watchdog)

Since 2026-09-04 `/readyz` means: replay done, gateway connected, **and the
ledger writer thread beat within the last 60 s**. On 2026-09-03 the writer
died mid-compaction while the process, the gateway and the scheduler all
looked fine and `/readyz` said 200 for hours; every command failed with
"driver gone". The signals that now catch that:

- `rate(nocturnal_ledger_writer_heartbeat_total[2m])` — healthy 0.05–0.1/s,
  dead exactly 0. First panel on the dashboard ("Bot health").
- `/readyz` → 503 with "the ledger writer is not beating".
- `nocturnal-watchdog.timer` (every minute) restarts the service after three
  consecutive unready minutes; `journalctl -u nocturnal-watchdog` shows strikes.
- Compaction failures: `nocturnal_compaction_runs_total{nocturnal_decision_outcome="error"}`
  and `store.compact` spans with status ERROR and `error.type` (`storage` |
  `panic`). Command failures: `nocturnal_commands_total{...="error"}` and
  `command.*` spans with status ERROR and an `exception` event.
- Which build: every span, metric and log carries `service.version`
  (`0.1.0+<commit>`) and `service.instance.id` (one per boot).
