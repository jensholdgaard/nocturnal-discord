# Nocturnal

Ground-up rewrite of the guild's EverQuest DKP Discord bot — Rust, event-sourced,
no database. The predecessor ([Ziglax/nocturnal-dkp-bot](https://github.com/Ziglax/nocturnal-dkp-bot))
is a Node.js/MongoDB bot whose August 2026 audit found 88 verified issues, nearly all
rooted in three systemic causes: no process-level error handling, maximally expensive
database access, and unguarded concurrency around DKP mutations.

The bot also absorbs the observability stack's provisioning bot
(`everquest-observability/bot/dpsbot.py` — `/dpstoken`, `/dpsrevoke`), so the
guild runs a single bot for DKP and telemetry access.

This rewrite keeps the external behaviour officers know (same slash commands, same
embeds, same auction flows) and replaces the internals with an architecture where the
audit's failure classes are impossible rather than patched.

## Design pillars

1. **Append-only event log as the sole source of truth.** Every DKP-relevant change
   is one immutable event. Durable tail in a WAL, compacted to Parquet for history.
   No MongoDB, no external database at all.
2. **Single-writer core.** One task owns all state; Discord handlers submit commands
   and await results. Every race the audit found (double-spend, double ticks,
   duplicate bids, two active raids) is structurally eliminated.
3. **Crash-resume by replay.** Boot = replay log → rebuild projections → re-arm
   timers. Auctions and raids survive any crash or redeploy.
4. **Errors never kill the process.** Handlers are fallible by type; panics are
   contained per task; Discord interactions are deferred immediately.
5. **Observable.** OpenTelemetry (traces + logs) into the guild's existing
   [everquest-observability](https://github.com/jensholdgaard/everquest-observability)
   stack; events optionally shipped to [Ourios](https://github.com/jensholdgaard/ourios).

## Documentation

| Doc | Contents |
|---|---|
| [PROJECT_BRIEF.md](PROJECT_BRIEF.md) | Consolidated context: audit summary, source repos, constraints |
| [docs/architecture.md](docs/architecture.md) | Crate layout, event store, single-writer core, timers, Discord layer |
| [docs/events.md](docs/events.md) | Event taxonomy and envelope (draft) |
| [docs/commands.md](docs/commands.md) | Behavioural spec: every command, flow, embed, and rule of the legacy bot |
| [docs/operations.md](docs/operations.md) | Production readiness: config layering, OTLP, health, containers, CI, backups |
| [docs/plan.md](docs/plan.md) | Milestones M0–M7 with acceptance criteria |
| [docs/audit-2026-08.md](docs/audit-2026-08.md) | Full August 2026 audit of the legacy bot (all 88 findings, file:line) |
| [semconv/](semconv/) | OTel Weaver registry: governed attribute/metric/span names mirroring the event taxonomy |
| [docs/hazards.md](docs/hazards.md) | Failure modes: audit classes → how the design addresses them, plus new hazards this design introduces |

## Process

Same approach as Ourios, without the RFC formality: design-first docs, each milestone
pinned by acceptance scenarios written as failing tests before implementation
(drafted → specified → red → green → validated). Development is AI-assisted (Claude)
with a human maintainer owning direction, review, and every merge.

## Status

Planning (M0). Nothing runs yet. The old bot stays in production — a guildie is
landing the audit's Phase 1–2 stability fixes on it — until this rewrite passes
shadow verification on real raid data.
