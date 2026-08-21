# Nocturnal DKP Bot — Resilience & Rewrite Project Brief
**Date:** August 2026
**Purpose:** Complete context for the rewrite: audit conclusions, source repos, phased plan for the legacy bot, and the longer-term vision. (Original brief compiled from the August 2026 audit; preserved here verbatim as reference. The rewrite's own decisions live in `docs/`.)

## 1. Current Situation

The guild runs a Discord DKP (Dragon Kill Points) bot for EverQuest raiding.
Primary pain points reported by officers:

- It crashes constantly on raid nights.
- It slows to a crawl when many auctions run at once.
- Some integrity bugs can corrupt the ledger (negative balances, wrong tie-break winners, long-auction winners never charged, etc.).

A full independent code audit (August 2026) was performed. Key numbers:

- 88 findings (75 independently verified, 13 supplemental)
- 0 findings refuted on adversarial re-check
- 4 improvement tracks, phased by payoff
- **0 database schema changes required** — the bot can keep running against the existing production MongoDB Atlas M0 free-tier database.

Core DKP accounting logic is sound and is the only part covered by automated tests. All problems live in the Discord/database glue layer.

## 2. Source Repositories

### Primary bot (the one under audit)
- **Fork / working copy:** https://github.com/Ziglax/nocturnal-dkp-bot/
- Language: Node.js / JavaScript (discord.js)
- Key directories: `DKPManager/`, `Auctioner/`, `worker/`, `commands/`, `utils/`, `logParser/`, `search/`, `db.js`, `index.js`
- Features: slash commands for DKP add/remove/history, raid start/end, short & long auctions, EQ `/who` log parsing, character registration, backup, admin-role setting, etc.
- Database: MongoDB (Atlas free tier)
- Deployment: currently via Pterodactyl panel (git pull + npm install on every boot). Docker files already exist in the repo.

### Related systems (context for longer-term vision)
- **everquest-observability** — https://github.com/jensholdgaard/everquest-observability
  Guild-side OTLP backend for EverQuest/Zeal telemetry (Perses dashboards + Discord SSO + Prometheus + Jaeger). Contains a small Python Discord bot (`bot/dpsbot.py`) that provisions OTLP tokens and Perses RoleBindings via flat files. Demonstrates the same Discord + file-based provisioning pattern that needs better resilience.

- **ourios** — https://github.com/jensholdgaard/ourios
  Pre-release log storage & query backend built on Apache Parquet + Drain-derived template miner + Apache DataFusion. Designed for selective time + attribute queries over long retention with excellent pruning. Intended as the durable audit/history store for any future rewrite.

## 3. Full Audit Summary (Nocturnal DKP Audit — August 2026)

### Executive Summary
- Core accounting is solid.
- Systemic problems:
  1. **Zero process-level error handling** — any unhandled rejection (Discord 10062, Mongo hiccup, closed DMs, etc.) kills the entire process.
  2. **Database access is maximally expensive** — zero indexes, full-collection scans, downloading entire raid history / unbounded player logs just to read a balance, chatty sequential queries.
  3. **Race conditions & integrity bugs** that can produce negative balances, double-charged or never-charged winners, double-awarded ticks, wrong attendance tie-break winners, phantom active raids, etc.
- Phased fixes exist that require **no schema changes**.

### Anatomy of a typical crash
1. Officer clicks "Confirm Winner/s" on a short auction.
2. Bot is busy (raid tick doing 40 parallel writes, other embeds, congested event loop).
3. `i.update()` (startbid.js:117) arrives after Discord's 3-second window → `DiscordAPIError[10062] Unknown interaction`.
4. No try/catch and no global rejection handler → process exits with code 1.
5. Winner already announced but DKP never deducted; all in-memory short auctions + bids lost; dead buttons left in channel.
6. Pterodactyl restarts → git pull + npm install + full slash-command re-registration (minutes of downtime).

### Why it crashes (ranked)
- **Critical** — No `process.on('unhandledRejection')` / `uncaughtException` / client error handlers.
- **Critical** — Background worker loops (10 s / 60 s / 1 h) completely unprotected.
- **Critical** — Long-auction finalization fire-and-forget + throwing player lookup → crash loop every 60 s.
- **High** — Closed DMs on "I want to bid" → reply-after-acknowledge throws inside catch.
- **High** — Auction-close timer and bell sound unprotected.
- **High** — Short auctions live only in memory; every crash erases them.
- Long tail of the same unprotected-async pattern everywhere (DKP writes, pagination, raid embeds, etc.).

### Why it is slow
- Zero MongoDB indexes → every query is a full collection scan.
- `getPlayer()` downloads the entire non-deprecated raids collection (attendance rosters included) on every call.
- Player documents grow forever (unbounded log arrays) and are always fetched whole.
- Chatty patterns: 5 sequential round-trips per `/bid`, ~41 individual writes per raid tick, 80 sequential queries for a 40-character `/parsedkps`.
- Item scraping with no timeout + heavyweight DOM parse on the main thread.
- Synchronous error logging that blocks the event loop.
- Mongo client defaults unsuitable for free-tier Atlas (100-connection pool, infinite timeouts, no compression).
- Commands never defer → they miss the 3-second window under load.

### DKP fairness & integrity (verified)
- Long-auction winners are **never debited** by any code path.
- Attendance tie-breaker draws the random index from the wrong array (wrong player can win).
- Double-spend across parallel auctions → negative balances (balance checked at bid time, debited minutes later with no remaining-balance condition).
- Race conditions: double ticks, duplicate bids, two simultaneous active raids, split new-player documents, non-atomic finalization that can announce two different winners.
- Smaller leaks: `/removedkp` accepts negative amounts, transient Discord error permanently ends the active raid, bids during close window are nondeterministic, corrupted "null raid" history entries, etc.

### Four improvement tracks (from the audit)

**Track 1 — Stability safety nets (2–4 days)**
Global error handlers; try/catch + await around every handler and worker loop; immediate defer/update; make bell & debug logging non-fatal; timeouts on external HTTP; async error logging.
→ Crashes drop from "several per raid night" to rare.

**Track 2 — Database performance (3–5 days)**
Create the recommended index set (including TTL on debuglog); stop unnecessary attendance computation; projections; batch tick writes; consolidate chatty commands; cache guild options; proper Mongo client options; defer every slow command.
→ Sub-second responses even with many concurrent auctions.

**Track 3 — DKP integrity (~1 week)**
Conditional debits; fix tie-breaker; debit long-auction winners; persist short auctions from the start; atomic updates + unique indexes; tick serialization; input bounds; stop ending raids on transient errors.

**Track 4 — Simplification & hygiene (1–2 weeks incremental)**
Merge the ~80 % duplicated short/long auction commands; single pagination helper; unify bid validation; fix 90 ms raid-deprecation default; centralize defaults; drop voice stack if possible; fix tests so they are runnable and safe; register slash commands only when they change; remove dead code.

### Suggested roadmap (from the audit)

| Phase | Contents                          | Effort   | What officers will notice                          |
|-------|-----------------------------------|----------|----------------------------------------------------|
| 1     | Track 1 + three one-line fixes    | 2–4 days | Raid nights without restarts; auctions stop vanishing |
| 2     | Track 2 (indexes can ship first)  | 3–5 days | Commands respond in <1 s; no more "application did not respond" |
| 3     | Track 3                           | ~1 week  | No more negative balances, phantom raids, disputed ties; auctions survive restarts |
| 4     | Track 4 (incremental)             | 1–2 weeks| Mostly invisible — faster boots, cheaper future changes |

Phases 1 and 2 are the decision that matters. They address the two complaints that prompted the audit and carry minimal risk.

### Hosting notes that compound the problems
- Production runs Node v20 while the voice library requires ≥22.12 → EBADENGINE warnings and unsupported runtime.
- Every boot does `git pull + npm install --production` → minutes of downtime per crash + dozens of unnecessary global slash-command re-registrations.
- Atlas M0 free tier is shared, throttled, latency-distant, and 512 MB capped (debuglog is actively eating the quota). The code fixes make the bot a polite M0 citizen; the TTL index stops the storage bleed.

## 4. Constraints & ground rules

For the **legacy bot** (guildie's Phase 1–2 work):
- Do not change the MongoDB schema. Only additive indexes are allowed.
- Prefer the smallest change that delivers the next phase's outcome.

For the **rewrite** (this repo):
- Preserve existing slash-command behaviour and Discord UX that officers rely on.
- The legacy bot stays in production until the rewrite passes shadow verification.
- Migration is one-way and verified: legacy Mongo data → genesis events; balances must match to the point.

## 5. Key file references in the legacy bot

(Use these when extracting behaviour to replicate)

- `index.js` — boot, slash-command registration, reply paths.
- `worker/Worker.js` — 10 s / 60 s / 1 h loops, long-auction finalization, raid-tick writes, guild-options polling.
- `Auctioner/Auctioner.js` & `Auction.js` — short-auction state machine, close callback, attendance tie-breaker, finalization.
- `commands/startbid.js` — short-auction UI flow (buttons, collectors, bell sound).
- `utils/Logger.js` — DM bidding path.
- `DKPManager.js` — accounting rules (the sound, tested part — the behavioural spec for the rewrite's ledger).
- `db.js` — Mongo client (relevant only to the migration tool).
