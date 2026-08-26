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
      legacy negative balances confirmed (audit #46 in the wild). 2026-08-21:
      re-run on a **fresh backup (2026-08-20)** — 281 players, 506 raids, 787
      genesis events, all balances match (worst double-spend casualty: −237);
      deployed to the VM, standard 90-day window.
- [x] Backup = tar of data dir; restore rehearsed on the real host.
- [x] Off-site archive: compacted Parquet mirrored to S3-compatible object
      storage (Hetzner), write-through after local verification and
      read-through on boot, so a fresh disk rebuilds its history from the
      bucket. Tested against a local-filesystem object store; an unreachable
      archive is never load-bearing.

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

- [x] `/startraid`, `/endraid` (with the aggregated movement-log embeds),
      `/configure` + `/showconfig`, officer gating (admin-perm bypass +
      configured role, legacy semantics). Tick scheduler: proposes a tick
      every 10 s and lets `decide` judge due-ness — `TickTooSoon` is the
      quiet normal case, so restarts/missed cycles self-correct (B6).
- [x] `/who` parsing ported to `nocturnal-core::who` (legacy jest fixture
      verbatim, incl. the timestamp); `/registercharacter` (case-insensitive,
      E11 fixed).
- [x] `/adddkp`, `/removedkp` (min 1 — E8 fixed), `/addraiddkp`,
      `/parsedkps` (per-character errors reported; active raid attached
      properly — E10 fixed).
- [x] Kill-and-resume covered by `raid_night.rs` (crash mid-raid + mid-
      auction, tick_no idempotence); live restart drill happens in the mock
      raid night below.
- [x] RaidHelper integration (`raidhelper.rs`): `/startraid` names and links a
      raid from an event starting within ±10 minutes; ending a linked raid
      awards signups who actually attended (the legacy rule —
      `min(10, max(1, attendees/2))` ticks — ported with tests); manual
      `/addraideventdkp` for past raids; the award amount is configurable
      (deliberate change #9) instead of hardcoded 5. Every call has a timeout
      and a RaidHelper outage can never stop a raid starting or ending.
      *Needs a live event and the guild's API key to exercise end to end.*

**Exit:** a full mock raid night runs in the test server, bot restarted twice
mid-raid, ledger perfect. *(Code deployed to the VM 2026-08-22; awaiting the
live mock night.)*

## M5 — Auctions *(~2 weeks — the crown jewel)*

- [x] One auction state machine, `flavor: short | long` — the legacy ~80 %
      duplicated `/startbid` and `/startlongbid` code paths collapse into one
      (audit S3), sharing item lookup, embeds and lifecycle.
- [x] Bid via button and DM (legacy UX kept, hardened: collector bound to its
      auction (#50), no stacked prompts (#39), closed DMs fall back to an
      ephemeral hint (#5)); `/bid` for long auctions with 0 = retract;
      re-bid replaces; cross-auction reservation; deterministic close.
- [x] Finalization-as-debit with a recorded tie-break seed; multi-winner and
      quantity support.
- [x] Stale-button handling (B12) and boot re-post of open auctions (B11) —
      buttons are stateless (auction id in the custom id), so a restart
      mid-auction changes nothing.
- [x] Auction timers in the scheduler: deadline → close, long + 20 min grace →
      finalize; both idempotent and state-derived.
- [x] `/auctiondetails` (with the legacy public "peek" callout).
- [x] Bell sound at short-auction start: songbird with libopus built from
      source and linked statically (no ffmpeg, no shared library, no 75 MB
      voice stack — the sound itself is 34 KB embedded in the binary).
      Strictly fire-and-forget: own task, ten-second timeout, every failure
      swallowed, and `bell.enabled: false` turns it off.
- [x] Chaos suite (`nocturnal-store/tests/chaos.rs`): 25 seeded scenarios of
      overlapping auctions + raid ticks with kill -9 at random points,
      including torn WAL tails, asserting no negative balances, at most one
      active raid, every reported charge present exactly once (never twice,
      never missing), no duplicated ticks, and replay determinism. Plus a
      torn-write test proving an interrupted append is all-or-nothing.
      (`/stresstest` drives the load half against live Discord.)

**Exit:** the "anatomy of a typical crash" scenario from the audit is replayed
step by step against the new bot and is boring.

### Ported from upstream (2026-08-26)

The legacy bot gained a run of auction work on 2026-08-24/25, after our audit.
Taken so far:

- [x] `/auctiondetails` refuses a running auction (officers bid too), and the
      public peek notice fires only when bids were actually shown. A cancelled
      auction reads back with who pulled it and when.
- [x] `/cancelauction` and `/endauction`, both gated on the officer **role**
      itself rather than Administrator. `/endauction` rewrites the deadline to
      the moment bidding stopped, then settles down the scheduler's own path.

- [x] Modal bid entry replaces the DM prompt, long auctions get the same two
      bid buttons, `/bid` is gone. The race upstream fixed we never had (one
      pending prompt per bidder); what carries over is that a modal needs no
      open DMs and that both auction kinds behave alike.
- [x] `/parsedkps` removed, as upstream removed it. The `/who` parser stays in
      `nocturnal-core`.
- [x] Embed field-limit guard: a full raid bidding on one item used to produce
      no embed at all, not a long one.
- [x] Item stat block tightened (raider feedback, 2026-08-26): padding and
      blank rows out, dash rule slimmed.

Still open: per-auction `autodebit` / `lockdelay`.

The baseline moved: "keep current behaviour" was pinned to the bot officers
knew in August, and these changes are live in the bot they use now.

### Deployment kit (pulled forward from M6, 2026-08-21)

`deploy/`: static musl binary via cargo-zigbuild, `nocturnal.toml` for the
observability VM (data on the host at `/var/lib/nocturnal`, OTLP through the
on-box gateway with a dedicated bearer token), hardened systemd unit,
Perses dashboard (`Nocturnal Bot` in project everquest: ledger head, command
rates/outcomes, commit + fsync latency percentiles, events by kind), and the
idempotent `install-vm.sh` (run by the maintainer; agent SSH is
policy-blocked).

## M6 — Ops & hardening *(~1 week)*

- [x] Docker image (distroless, static musl binary, non-root, read-only
      rootfs, `/data` volume) + compose stack; backup script with verification
      and retention, plus a nightly systemd timer.
- [x] fsync verified on the actual host volume (B7: ext4, p50 1.54 ms — real
      flushes) and the **restore rehearsed**: last night's backup extracted to
      a scratch dir and replayed clean at 3,897 events, live ledger untouched.
      The attempt also re-demonstrated B2 — the instance lock refused a second
      writer against the live directory.
- [x] Item-info lookup (`crates/nocturnal/src/items.rs`): pqdi.cc (Quarm) +
      takproject allaclone (TAKP), 5 s timeouts, URL-encoded queries, status
      checks, null-safe parsing, permanent in-memory cache — audit #42/E12
      fixed. `/searchitem` ports the legacy picker UX (1 hit → embed, 2–25 →
      button picker, 26–40 → list, >40 → refine).
- [x] Admin/config commands: `/configure` (every legacy option → a single
      `config.updated` patch; the legacy `/setadminrole` is its `role` option)
      and `/showconfig`. Values are validated in the **decide step**, not on
      the slash-command options, so the migrator and every future caller are
      held to the same rules and the officer is told which setting was refused
      — a zero tick interval used to be accepted here and refused later at
      `/startraid`. The two raid channels are checked against the *merged*
      config, so they cannot collide across separate calls. The RaidHelper API
      key is a `Secret`: transparent on the wire, redacted in `Debug`, and
      shown by `/showconfig` as presence only.
- [x] Runbook (`docs/runbook.md`): health checks, deploy, backup/restore with
      an RPO statement, dispute resolution by grepping the ledger, the common
      situations, and rollback rules.

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

- [x] `telemetry.*` events + projection + lifecycle tests (issue/refresh/revoke,
      re-issue after revoke, replay determinism); the `managed` set outlives a
      grant so the materializer can tell its own lines from service tokens.
- [x] File materializers (`nocturnal-provision`): `tokens.txt` + Perses
      provisioning YAMLs, atomic write + rename, byte-compatible with the
      legacy formats; re-materialized on boot; golden-file tests against
      output captured from the live VM.
- [x] `/dpstoken` `/dpsrevoke` with the exact legacy UX (DM template, spoiler
      fallback, role-map refusal); `roles.yaml` mapping re-read per command.
- [x] Migration (`--import-provisioning`): parses `tokens.txt` + the
      provisioning dir into genesis `telemetry.*` events and refuses to report
      success unless the re-derived file matches. Rehearsed against a copy of
      the live VM's data: 10 grants imported, the `nocturnal-bot` service token
      skipped, every token value preserved, 0 provisioning files rewritten.
- [ ] Deploy the unified bot on the observability VM; retire `dpsbot.py`.

### `/backup` (2026-08-26 — the one legacy command that was still missing)

- [x] `nocturnal-migrate::export`: the projection rendered back into
      `{guild}_players.json` / `{guild}_raids.json` using **the same structs
      the importer parses**, so there is one definition of the format.
- [x] `/backup` (Administrator, ephemeral): both documents zipped — 71 MB of
      JSON becomes ~5 MB — with a size refusal that names the nightly tarball.
- [x] Round-trip proof against the real 2026-08-20 snapshot: all 281 players
      and 506 raids, field by field and log line by log line.
- [x] `raid.imported` and `player.imported` carry the legacy fields the export
      has to give back (`tickDuration`, `dkpsPerTick`, `eventId`, the Mongo
      `_id`). Additive, so existing logs replay unchanged.

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
