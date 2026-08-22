# Hazards

Two halves: (A) the audit's failure classes and where the design eliminates or
contains each, (B) new hazards this design introduces and their mitigations.
Ourios-style: every hazard should end up pinned by a test or a runbook line.

## A. Audit failure classes → disposition

| # | Legacy failure class | Disposition in the rewrite |
|---|---|---|
| A1 | Unhandled rejection kills process (10062 et al.) | **Eliminated.** Fallible handlers by type; poise error hook; immediate defer; panics contained per task |
| A2 | Unprotected worker loops (10 s/60 s/1 h) crash-loop | **Eliminated.** No polling loops; scheduler injects commands into the single writer; a failed side effect never re-fires the decision |
| A3 | Long-auction winner never debited | **Unrepresentable.** `auction.finalized` is the debit in the fold |
| A4 | Double-spend across parallel auctions / negative balances | **Unrepresentable.** Single writer + decide-time balance check + committed-bid reservation |
| A5 | Tie-breaker draws from wrong array | **Eliminated + auditable.** `auction.tie_broken` records candidates, seed, winner |
| A6 | Short auctions in memory only; crash erases them | **Eliminated.** Auctions exist as events from `auction.opened`; replay resumes them |
| A7 | Double ticks, duplicate bids, two active raids, split player docs | **Unrepresentable.** Serialization through one writer; `tick_no` idempotence |
| A8 | Transient Discord error permanently ends raid | **Eliminated.** Only `raid.ended` ends a raid; side-effect failures are logged, retried, non-fatal |
| A9 | Slow: full-collection scans, unbounded docs, chatty queries | **Gone with the database.** All reads are in-memory projections; worst command is O(page size) |
| A10 | Closed DMs crash the DM bid path | **Contained.** Typed, expected error → ephemeral fallback ("open your DMs / bid via button") |
| A11 | No timeouts on item scraping; main-thread DOM parse | **Contained.** reqwest with timeout, spawned task, cached results, failure degrades to "no item info" |
| A12 | Boot = git pull + npm install + full command re-registration | **Eliminated.** Static binary; sub-second replay; registration only on definition hash change |
| A13 | Atlas M0 quota bleed (debuglog) | **Gone.** Debug output goes to OTLP with retention owned by the observability stack |
| A14 | Input bounds (`/removedkp` negative amounts etc.) | **Eliminated.** Typed command validation in `decide`; invalid values never become events |

## B. New hazards introduced by this design

| # | Hazard | Mitigation |
|---|---|---|
| B1 | **WAL torn/partial write** on crash mid-append | Length- or newline-delimited records with per-record CRC; replay truncates a trailing partial record (and only a trailing one); fsync before ack |
| B2 | **Two instances write concurrently** (deploy overlap, Pterodactyl restart race) | flock on the data dir taken before gateway connect; second instance exits loudly. Test: start two, assert one refuses |
| B3 | **Replay divergence** after a code change to `apply` | `apply` is pure and versioned by `(kind, v)`; CI replays recorded logs and compares state hashes across the change; determinism property test (no wall clock, no RNG in the fold — tie-break randomness enters via the recorded seed) |
| B4 | **Event schema evolution mistakes** (field meaning drift) | Append-only payload rule + `v` bump discipline (events.md); serde round-trip tests pinned per version; old fixtures never deleted |
| B5 | **Compaction data loss** (WAL deleted before Parquet durable) | Temp-write + atomic rename; row-count and seq-range verification read-back before WAL segment deletion; compaction is crash-safe at every step (idempotent re-run) |
| B6 | **Missed/duplicated timers across restarts** (raid ticks during downtime, auction deadline passed) | Timers derived from state, not remembered; catch-up rules are explicit `decide` logic with `tick_no` idempotence; tests kill the process mid-auction and mid-raid and assert exact resume |
| B7 | **fsync durability on host volume** | **Verified on eq-perses 2026-08-22**: ext4 on /dev/sda1, fsync p50 1.54 ms / p95 2.34 ms — real flushes, not a no-op (which would be microseconds), matching the `wal.fsync.duration` metric in production. Nightly verified backups; RPO stated in the runbook |
| B8 | **Unbounded projection growth** (all events in RAM at boot… eventually) | Non-issue at guild scale for years (thousands of events/yr); snapshot design documented as the escape hatch, deliberately not built now |
| B9 | **Clock jumps** (host NTP step) affecting deadlines | Deadlines stored as absolute UTC in events; scheduler tolerates monotonic/wall divergence; `ts` is informational — `seq` is the order of truth |
| B10 | **Migration produces wrong balances** | Mandatory verification report (per-player legacy vs replayed); 100 % match or officer-signed diff; legacy bot keeps running until sign-off |
| B11 | **Discord outage vs ledger truth** (event appended, reply/announce lost) | Ledger-first ordering means worst case is a *missing announcement*, never a missing/extra charge; on boot, active auctions re-post their embeds; an idempotent "announce" side-effect queue retries |
| B12 | **Stale components** (buttons from before restart) | Auction id in custom id; unknown/closed id → friendly ephemeral reply |
| B14 | **Discord protocol requirements move under us** (observed 2026-08-22: voice closed with `4017 E2EE/DAVE protocol required`; songbird 0.5 predates DAVE, so the bot joined the channel and sat silent) | songbird ≥ 0.6, which implements DAVE via `davey` — the same protocol the legacy bot pulled in as `@snazzah/davey`. Structurally: the bell is decorative and fire-and-forget, so even a total voice failure costs nothing but silence. `/belltest` reproduces voice end to end without an auction, and `--bell-test <guild>:<channel>` does it from a shell with no ledger, no lock and no slash command |
| B13 | **Secrets in exported telemetry** (occurred 2026-08-21: tracing-opentelemetry exported *all* spans, incl. serenity's — whose fields carry gateway payload dumps: the Identify frame with the bot token, InteractionCreate with interaction tokens/member data — into guild-visible Jaeger) | Export allowlist at the source (`nocturnal_telemetry::export_targets`, pinned by test): only spans/events from our own crates leave the process; library internals stay in local logs. Jaeger storage wiped; bot token rotated. Second line (in-process, 2026-08-22): a `RedactSpans` processor strips every span attribute not on a strict allowlist before export — serenity's request spans now export as *redacted* Discord client spans. Third line (recommended, eqobs repo): `redaction` processor on the gateway collector with `blocked_key_patterns: [".*token.*", ".*auth.*"]` |
