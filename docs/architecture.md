# Architecture

Rust rewrite of the DKP bot. Same externally visible behaviour as the legacy bot;
internals redesigned so the audit's three systemic failure classes (unhandled async
errors, expensive data access, write races) cannot occur by construction.

## Decisions (settled 2026-08-21)

| Decision | Choice | Rationale |
|---|---|---|
| Language | **Rust** (serenity + poise) | Matches Ourios tooling and discipline; single static binary; fallibility in the type system |
| Storage | **Event log only** — WAL tail + Parquet, no database | Guild scale (~hundreds of players, thousands of events/year) makes full replay-on-boot trivial; Ourios conventions reusable |
| Durable execution | **Event-sourced replay**, not Temporal | Replay + timer re-derivation gives the same crash-resume guarantee with zero extra infrastructure |
| Process | Ourios approach without RFCs | Design-first docs; milestone acceptance scenarios as failing tests before code |

## System shape

```
Discord gateway (serenity/poise)
        │  defer immediately, then…
        ▼
   Command  ──── mpsc ────►  ┌──────────────────────────┐
   (validated request)       │   Core (single writer)   │
                             │  decide → append → apply │
        ◄──── oneshot ─────  └───────────┬──────────────┘
   Decision (accepted event              │ append (fsync)
   or typed rejection)                   ▼
                             WAL (JSONL segments)
                                         │ compaction (background)
                                         ▼
                             Parquet (month-partitioned)
```

### The single-writer core

One tokio task owns all mutable state (the projections). Everything that mutates
DKP — slash commands, button clicks, DM bids, raid ticks, auction closes — is a
`Command` sent over a channel; the caller awaits a `Decision`.

Processing a command is strictly sequential:

1. **Decide** — validate against current projections (pure function).
   Rejections are values (`InsufficientBalance`, `AuctionClosed`,
   `RaidAlreadyActive`, …), never errors.
2. **Append** — write the resulting event(s) to the WAL and fsync.
   Only after fsync is the command considered done.
3. **Apply** — fold the event into projections (pure function, the same one
   replay uses).
4. **Reply** — the Discord layer edits its deferred response; side effects
   (embeds, announcements, bell) happen after the ledger fact is durable
   and are always non-fatal.

This one structural choice erases the audit's whole race-condition track:
double-spend across parallel auctions, double ticks, duplicate bids, two active
raids, split player documents, non-atomic finalization — none can happen when
there is exactly one writer and validation happens in the same breath as the write.

It also fixes the double-spend class properly: balance is checked at **debit
decision time**, not bid time. `AuctionFinalized` *is* the debit in the fold —
a winner can never be announced without being charged (the legacy long-auction
bug), and never charged into a negative balance.

Throughput is a non-issue: worst case on a raid night is a few commands per
second against in-memory maps plus one fsync each.

### Event store

- **Envelope**: `seq` (u64, contiguous), `ts`, `actor` (Discord user id),
  `guild_id`, `kind`, `payload`, optional `correlation_id` (ties bids to their
  auction, ticks to their raid). See `events.md`.
- **WAL**: append-only JSONL segments (`wal/000123.jsonl`), fsync per event,
  rotated at a size threshold. Human-readable on purpose — an officer dispute is
  settled with `grep`.
- **Compaction**: a background job rewrites sealed WAL segments into Parquet
  files partitioned by month (`events/2026-08.parquet`), written to a temp path
  and atomically renamed; the WAL segment is deleted only after the Parquet file
  is verified readable and row-count-matched. Ourios crates/conventions
  (schema, writer settings, bloom filters) are reused where they fit, but hour
  partitioning and template mining are overkill here — structured events with a
  `kind` column already prune perfectly at this volume.
- **Boot**: read all Parquet (seq order) + WAL tail → replay through the same
  `apply` fold → projections ready. At full guild scale this is tens of
  thousands of rows: milliseconds. Snapshots are a documented future option,
  not built.
- **Backups**: `tar` of the data directory. `/backup` becomes trivial and
  actually complete.

### Projections (all in-memory, rebuilt on boot)

- `balances`: player → DKP (plus lifetime earned/spent for display)
- `players`: registration, linked EQ characters, aliases
- `active_raid`: at most one, enforced by the decide step; tick schedule state
- `auctions`: active short + long auctions, bids, deadlines
- `attendance`: rolling windows used for tie-breaks (computed from raid events)
- `config`: guild options (admin roles, tick interval, defaults)
- `history` indexes: per-player event refs for `/history` pagination

### Timers

Auction deadlines and raid ticks are **state, not callbacks**. A scheduler task
watches the projections' "next due instant" and injects `Command::CloseAuction` /
`Command::RaidTick` into the same single-writer channel as user commands (so
they serialize with everything else). On boot, due times are re-derived from
replayed state: an auction whose deadline passed during downtime closes
immediately and correctly; a raid tick that was missed is awarded per the same
catch-up rules the legacy worker intended. Crash mid-auction now means "resume",
not "the auction vanished".

### Discord layer

- Every interaction **defers within the first await** — the 3-second-window
  crash class (10062) is gone.
- All handlers return `Result`; the poise error hook logs and answers with a
  friendly ephemeral message. Nothing escapes to kill the process; panics in
  spawned tasks are contained and logged.
- Closed DMs, missing permissions, deleted channels: expected, typed, non-fatal.
  (M0 decides whether the DM bid flow survives at all — a Discord modal removes
  the closed-DM and shared-collector failure classes entirely.)
- Component interactions (bid buttons, confirm-winner) carry the auction id in
  the custom id; stale buttons from before a restart get a "this auction has
  ended" reply instead of a crash.
- Slash commands are registered only when their definitions change (hash check),
  not on every boot.
- Outbound HTTP (item lookups) goes through reqwest with timeouts, off the hot path.
- The voice/bell stack is dropped unless officers actively want it (audit Track 4);
  if kept, it is fire-and-forget and non-fatal.

### Crate layout

```
crates/
  nocturnal-core      # domain: commands, events, decide/apply folds, projections.
                      # Pure — no I/O, no tokio, no Discord. Property-tested hard.
  nocturnal-store     # WAL append/replay, Parquet compaction, backup. (Reuses
                      # Ourios parquet conventions where sensible.)
  nocturnal-discord   # serenity/poise: slash commands, components, embeds,
                      # pagination, /who log parsing UI.
  nocturnal-telemetry # weaver-generated constants from semconv/ (attribute
                      # names, metric names) + OTLP wiring helpers.
  nocturnal-migrate   # one-shot: Mongo export (or live Atlas read) → genesis
                      # events; balance-verification report vs legacy output.
  nocturnal           # bin: wiring, scheduler, config, OTLP, health.
```

`nocturnal-core` is the ledger. It is the part the audit called "sound and
tested" in the legacy bot — here it is the *only* place rules live, and the
legacy `DKPManager` tests become its behavioural spec.

### Integrity invariants (enforced and property-tested)

1. Balance is a fold of events; there is no stored balance to drift.
2. No decide step admits an event that takes a balance negative.
3. `AuctionFinalized` implies the debit — same event, same fold step.
4. At most one active raid; at most one bid per player per auction (re-bid replaces).
5. Tie-breaks are seeded, logged draws over the *correct* candidate set — the
   draw itself is an event, so disputes are answerable from the log.
6. Replay determinism: replaying the full log twice yields identical state;
   CI replays recorded production logs after every change to `apply`.

### Observability

(Full spec: `operations.md`.)

`tracing` throughout; OTLP export (traces + logs) into the existing
everquest-observability stack — the bot shows up in Jaeger/Perses next to the
Zeal telemetry. Every command carries a span from interaction to fsync.
Optionally, events are also shipped to Ourios as structured logs, making
Ourios the long-retention query surface for "what happened in that auction
last March". A `/healthz` endpoint plus a heartbeat metric replace guessing
whether the bot is up.

### Deployment

Single static binary in a scratch/distroless Docker image, data directory on a
volume. Boot time is process start + replay (sub-second) — no git pull, no npm
install, no command re-registration. An exclusive lock (flock) on the WAL
directory guarantees two instances can never both write — a deploy overlap
becomes a clean refusal instead of a corrupted ledger.

### Migration (one-way, verified)

1. Export the Atlas collections (players, raids, auctions, options).
2. `nocturnal-migrate` synthesizes genesis events: `PlayerImported` (with
   balance and compacted history), `RaidImported`, `ConfigImported` — genesis
   is a distinct event family; history before cutover is honest about its provenance.
3. Verification report: every player's replayed balance vs the legacy bot's
   computed balance. Cutover requires 100 % match or an explained diff signed
   off by officers.
4. Shadow period: the rewrite runs in a test server (or with read-only commands
   in prod) against migrated data for at least one raid week before the legacy
   bot is retired.
