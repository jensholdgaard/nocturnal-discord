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
- [x] Bid UX resolved (2026-08-21): keep the DM flow, hardened; modal stays a
      possible later upgrade.
- [x] Bell resolved (2026-08-21): kept, strictly fire-and-forget.
- [x] Telemetry semantic-convention registry (`semconv/`, OTel Weaver format,
      `weaver registry check` clean) — attributes/metrics/spans mirroring the
      event taxonomy; codegen to `nocturnal-telemetry` lands in M3.
- [x] Cargo workspace scaffold (7 crates incl. `nocturnal-provision`), CI
      (fmt, clippy -D warnings, test, `weaver registry check`),
      `nocturnal.example.toml` — fmt/clippy/test green.

**Exit (met 2026-08-21):** behaviour policy set — current UX kept everywhere,
integrity fixes ship, officer change requests handled later; workspace green.

*Status note (2026-08-21):* the guildie's `phase1-stability` branch on the
legacy repo already lands process safety nets, worker guards, auction handler
wrapping, config-default fixes and log rotation — Phase 1 is real, which takes
the time pressure off this rewrite.

## M1 — Ledger core *(~1–2 weeks)*

`nocturnal-core` + WAL: events, decide/apply folds, projections. No Discord.

- [x] Event types with serde round-trip tests and pinned wire `kind` strings.
- [x] Single-writer decide/apply (`Ledger::propose`/`commit` mirroring the
      decide → fsync → apply loop); integrity invariants as proptest suites
      (no negative balances, one active raid, rejections mutate nothing).
- [x] WAL: crc32-per-record JSONL segments, fsync per append, rotation,
      torn-tail truncation, corruption refusal, seq-gap refusal (hazard B1).
- [x] Replay determinism: property test + end-to-end in the raid-night scenario (B3).
- [x] Legacy jest fixtures ported (`legacy_fixtures.rs`): winner selection
      incl. main/alt lock + overbid promotion, attendance 80/100/100 case,
      re-bid replace, tie-breaks, validation bounds.

**Exit (met 2026-08-21):** `raid_night.rs` scripts the audit's "anatomy of a
typical crash" — raid, ticks, three overlapping auctions, kill mid-auction,
replay, finish — and asserts every balance to the point.

## M2 — Parquet + migration *(~1 week)*

- [x] Compaction: sealed WAL segments → month-partitioned Parquet
      (temp-write → rename → read-back verify → delete), idempotent after
      partial crash (B5); `Store::open` replays Parquet + WAL seamlessly.
      Real data: 16 MB genesis WAL → 2.3 MB Parquet, boot in ~90 ms.
- [x] `nocturnal-migrate`: legacy backup JSONs → genesis events with a
      per-player balance verification report; skips unparseable ids with
      warnings, carries legacy negative balances honestly.
- [x] **Balance verification report** run against real production data
      (2024-12-19 snapshot recovered from the Discord log channel): 163
      players, 145 raids, 308 genesis events, **all balances match**; two
      legacy negative balances confirmed (audit #46 in the wild). A fresh
      snapshot needs read access to `#bot-backups` (or an officer repost).
- [ ] Backup = tar of data dir; restore test. (Trivial now; lands with M6 ops.)

**Exit:** production data migrated on a workstation; verification report at
100 % (or diffs explained); history queryable via DataFusion CLI for fun.

## M3 — Discord read-only *(~1 week)*

First contact with Discord, zero risk: nothing mutates.

- [x] serenity/poise wiring: immediate defer, error hook, guild-scoped
      registration (never global while the legacy bot lives; definition-hash
      skip is a later nicety). Single-writer driver thread (decide → fsync →
      apply; reads as closures on the same thread), layered TOML/env config
      with `--check`/`--print-config`/`--offline`, flock instance lock (B2,
      verified: second instance refuses), `/healthz` + `/readyz`.
- [x] `/playerdkp`, `/dkphistory`, `/listplayersdkps`, `/searchlogs` with the
      one shared pagination helper (S12), legacy embed formats. **Live** in the
      test guild (controels-test-bot, `controels-` prefix) over the migrated
      production ledger, 2026-08-21.
- [x] OTLP wiring: `nocturnal-telemetry` constants are Weaver-generated from
      `semconv/` (CI diffs against a fresh generation); traces + logs +
      metrics export via grpc or http/protobuf when `otlp.endpoint` (or the
      standard `OTEL_EXPORTER_OTLP_ENDPOINT`) is set, no-op otherwise. The
      driver emits `ledger.execute` spans and the commands/commit-duration/
      ledger-events/seq/fsync metrics with registry attribute names.
      End-to-end span visibility in Jaeger to be eyeballed when deployed next
      to the stack.
- [x] flock double-instance guard (B2), verified against the real data dir.

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

### Deployment kit (pulled forward from M6, 2026-08-21)

`deploy/`: static musl binary via cargo-zigbuild, `nocturnal.toml` for the
observability VM (data on the host at `/var/lib/nocturnal`, OTLP through the
on-box gateway with a dedicated bearer token), hardened systemd unit,
Perses dashboard (`Nocturnal Bot` in project everquest: ledger head, command
rates/outcomes, commit + fsync latency percentiles, events by kind), and the
idempotent `install-vm.sh` (run by the maintainer; agent SSH is
policy-blocked).

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

## M8 — Telemetry provisioning (dpsbot absorbed) *(~1 week, parallelizable)*

Independent of the DKP cutover — can land any time after M3's Discord layer
exists; the Python dpsbot retires when this ships.

- [ ] `telemetry.*` events + projection + property tests (issue/refresh/revoke
      idempotence; materialization is a pure function of the projection).
- [ ] File materializers: `tokens.txt` + Perses provisioning YAMLs, atomic
      write + rename, byte-compatible with the legacy formats; re-materialize
      on boot; golden-file tests against outputs captured from dpsbot.
- [ ] `/dpstoken` `/dpsrevoke` with the exact legacy UX (DM template, spoiler
      fallback, role-map refusal); `roles.yaml` mapping re-read per command.
- [ ] Migration: parse existing `tokens.txt` + provisioning dir → genesis
      `telemetry.*` events; verify re-materialization is byte-identical.
- [ ] Deploy the unified bot on the observability VM; retire `dpsbot.py`.

**Exit:** `/dpstoken` on the real VM issues a working token; `kill -9` between
event and file write heals on restart; dpsbot's systemd unit disabled.

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
- ~~Rewriting `dpsbot.py`~~ — **pulled into scope 2026-08-21** as milestone
  M8: the unified bot absorbs `/dpstoken`//`/dpsrevoke` (see `commands.md`
  §Telemetry provisioning).
- Temporal — revisit only if replay-based resume proves insufficient (it won't
  at this scale).
