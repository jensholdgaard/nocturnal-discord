# Plan

Milestones M0–M7. Same working method as Ourios minus the RFC formality: each
milestone is *specified* in these docs, its acceptance scenarios are written as
**failing tests first** (red), implemented (green), then *validated* against
real data or a real Discord server. A milestone is done when its acceptance
list is green and its hazards rows are pinned.

Effort is evenings-scale, one primary developer + Claude. Estimates are honest
guesses, not commitments.

Parallel context: a guildie is landing the audit's Phase 1–2 fixes on the
legacy bot. That is complementary, not competing — it keeps raid nights
survivable while this rewrite matures, and lowers the pressure to cut over early.

---

## M0 — Groundwork & behavioural spec *(~1 week)*

The rewrite's contract is "same behaviour officers know". That behaviour must be
written down before code.

- [x] Extract from the legacy repo (`main@8ec128e`, the merged
      `docker-deployment` audit revision) a **command inventory**: every slash
      command, option, permission gate, button/DM flow, embed layout,
      pagination behaviour → `commands.md`. Includes the RaidHelper event
      integration and main/alt bid mechanics the original brief omitted.
- [x] Extract the **accounting rules** (winner selection, main/alt priority,
      validation, attendance formula) → `commands.md` §Auctions/§Raid lifecycle;
      the legacy `Auction`/`DKPManager` jest specs become `nocturnal-core`
      fixtures in M1.
- [x] Note deliberate behaviour *changes* (all fixes, not features):
      long-auction debit, tie-break correctness, bid-window determinism,
      input bounds, no negative balances, case-insensitive character matching
      (audit E11), escaped/validated search input (E6). Officers should sign
      off on this short list — the only places old and new intentionally differ.
- [ ] Decide bid UX with officers: keep the DM flow, or replace it with a
      Discord modal (the audit's own suggestion — kills the closed-DM and
      cross-auction-collector failure classes #5/#39/#50 outright).
- [ ] Decide the fate of the voice/bell feature with officers (default: drop).
- [ ] Cargo workspace scaffold, CI (fmt, clippy, test), crate skeletons —
      per `operations.md` (config layering, OTLP, health, container, CI are
      specified there and land across M3/M6).

**Exit:** `commands.md` reviewed by the guildie/officers (sign-off on the
deliberate-changes list + the three open decisions); workspace compiles.

*Status note (2026-08-21):* the guildie's `phase1-stability` branch on the
legacy repo already lands process safety nets, worker guards, auction handler
wrapping, config-default fixes and log rotation — Phase 1 is real, which takes
the time pressure off this rewrite.

## M1 — Ledger core *(~1–2 weeks)*

`nocturnal-core` + WAL: events, decide/apply folds, projections. No Discord.

- [ ] Event types per `events.md` with serde round-trip tests per `(kind, v)`.
- [ ] Single-writer decide/apply; all integrity invariants (architecture.md §invariants)
      as property tests (proptest): random command streams never yield negative
      balances, never double-charge, never admit two active raids…
- [ ] WAL append/replay with CRC + trailing-truncation recovery (hazard B1);
      crash-injection test (kill mid-append, replay, assert state).
- [ ] Replay determinism test: fold(log) twice → identical state hash (B3).
- [ ] Legacy `DKPManager` fixtures pass against the new fold.

**Exit:** a headless binary can ingest a scripted raid night (raid, ticks,
three overlapping auctions, crash, resume) and end with provably correct balances.

## M2 — Parquet + migration *(~1 week)*

- [ ] Compaction: sealed WAL segments → month-partitioned Parquet, crash-safe
      and idempotent (B5); replay reads Parquet + WAL tail seamlessly.
- [ ] `nocturnal-migrate`: Atlas export → genesis events (`*.imported`).
- [ ] **Balance verification report**: per-player legacy vs replayed (B10),
      run against the real production export.
- [ ] Backup = tar of data dir; restore test.

**Exit:** production data migrated on a workstation; verification report at
100 % (or diffs explained); history queryable via DataFusion CLI for fun.

## M3 — Discord read-only *(~1 week)*

First contact with Discord, zero risk: nothing mutates.

- [ ] serenity/poise wiring: immediate defer, error hook, panic containment,
      command registration on definition-hash change only.
- [ ] `/dkp`, `/history` (+ pagination helper — one, shared), roster/attendance
      views, running in a **test server** against the migrated production data.
- [ ] OTLP wiring: traces + logs into everquest-observability; `/healthz`.
- [ ] flock double-instance guard (B2) with test.

**Exit:** officers can browse real (migrated) balances and history in the test
server; spans visible in Jaeger.

## M4 — Raids *(~1–2 weeks)*

- [ ] `/startraid`, `/endraid`, tick scheduler (state-derived, B6), catch-up rules.
- [ ] `/who` log parsing → roster updates + character registration
      (port `logParser/`, with fixtures from real EQ logs).
- [ ] `/adddkp`, `/removedkp`, `/parsedkps` with typed bounds.
- [ ] Kill-and-resume test: crash mid-raid → ticks continue correctly, none
      doubled, none lost.

**Exit:** a full mock raid night runs in the test server, bot restarted twice
mid-raid, ledger perfect.

## M5 — Auctions *(~2 weeks — the crown jewel)*

- [ ] One auction state machine, `flavor: short | long` (legacy dup collapsed).
- [ ] Bid via button and DM; re-bid replaces; committed-bid reservation across
      concurrent auctions; deterministic close at `auction.closed` seq.
- [ ] Tie-break as recorded draw (`auction.tie_broken`).
- [ ] Finalization-as-debit; multi-winner/quantity support as per legacy behaviour.
- [ ] Stale-button handling (B12); re-post active auction embeds on boot (B11).
- [ ] Chaos suite: N overlapping auctions + raid ticks + kill -9 at random
      points → resume, finish, verify every invariant.

**Exit:** the "anatomy of a typical crash" scenario from the audit is replayed
step by step against the new bot and is boring.

## M6 — Ops & hardening *(~1 week)*

- [ ] Docker image (distroless, static binary), volume layout, fsync behaviour
      verified on the actual host (B7); scheduled backups.
- [ ] Item-info lookup with timeout/cache (or cut it — officer call).
- [ ] Admin/config commands (`/setadminrole`, options → `config.updated`).
- [ ] Runbook: deploy, restore, dispute-resolution via log grep, RPO statement.

## M7 — Shadow & cutover *(~1–2 raid weeks, calendar time)*

- [ ] Fresh migration snapshot; rewrite runs in test server through ≥1 real
      raid week, officers mirror key actions.
- [ ] Final verification report; officer sign-off on the deliberate-changes list.
- [ ] Cutover: legacy bot demoted (commands unregistered), rewrite joins prod
      guild, final migration run, Atlas archived (export kept), done.
- [ ] Two-week soak with legacy bot restorable, then retire it.

---

## Sequencing notes

- M1–M2 are pure Rust with no Discord account, deployment, or coordination
  needed — ideal first coding stretch.
- The real production export should be obtained **early** (M2) — it is the best
  test fixture this project will ever have.
- If raid-night pain gets bad before M7, that pressure lands on the guildie's
  legacy Phase 1–2 work, not on rushing cutover.

## Out of scope (recorded so they stay out)

- Multi-guild support (design keeps `guild_id`, nothing more).
- Web UI / dashboards beyond what Perses+Ourios give for free.
- Rewriting `dpsbot.py` — same patterns apply later, separate project.
- Temporal — revisit only if replay-based resume proves insufficient (it won't
  at this scale).
