# Operations & production readiness

What "a modern system" means for this bot, concretely. Everything here is a
requirement on the M3/M6 milestones, not aspiration; each item lands with a
test or a runbook line.

## Configuration

Three layers, strictly ordered (later wins), all validated at boot with
fail-fast errors that name the offending key:

1. **Config file** — `nocturnal.toml`, path via `--config` / `NOCTURNAL_CONFIG`.
   All operational knobs: data dir, WAL segment size, compaction cadence,
   OTLP endpoint/protocol/headers, log format/level, health bind address,
   HTTP timeouts, backup schedule. Ships with a commented example
   (`nocturnal.example.toml`); every knob has a sane default — an empty file
   is a valid config.
2. **Environment** — `NOCTURNAL_*` overrides for every file key (12-factor,
   container-friendly), e.g. `NOCTURNAL_OTLP__ENDPOINT`. Standard OTel vars
   (`OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_SERVICE_NAME`, `OTEL_RESOURCE_ATTRIBUTES`)
   are honored as the fallback for telemetry, so the bot drops into the
   everquest-observability stack with zero bot-specific config.
3. **Secrets** — never in the file: `DISCORD_TOKEN` (or `DISCORD_TOKEN_FILE`
   for Docker/K8s secrets). Config dumps and spans never echo secrets.

`discord.command_prefix` prepends every slash-command name (e.g.
`controels-` → `/controels-playerdkp`), so a test server can share the bot
application with other deployments without name collisions; empty in
production.

Provisioning (M8) adds a `[provision]` section: `tokens_path`,
`perses_provisioning_dir`, `roles_map_path`, `dashboard_url` — all optional;
absent config cleanly disables `/dpstoken`//`/dpsrevoke`. The roles map stays a
live-editable YAML file (officers edit it today; re-read per command), not
ledger state.

Per-guild *behavioural* config (channels, roles, bid rules) is **state, not
deployment config** — it stays in the event log via `/configure`
(`config.updated` events), exactly as the legacy bot's officers expect.

`nocturnal --print-config` prints the fully resolved config (secrets redacted)
and exits; `nocturnal --check` validates config + data dir + replay and exits —
usable as a pre-flight in CI and as a Docker healthcheck during rollout.

## Observability

- **Export allowlist** — only telemetry from `nocturnal*` crate targets is
  exported (hazard B13); dependency spans/logs never leave the process, no
  matter what libraries stuff into their fields. The allowlist is pinned by a
  unit test.
- **Traces** — `tracing` + OTLP export (grpc or http/protobuf, configurable;
  off by default outside containers). One span per interaction from gateway
  receive → decide → fsync → reply, with `guild_id`, `command`, `event.seq`
  attributes; worker/scheduler cycles and compaction runs are spans too.
  Sampling configurable (default: parent-based, always-on — traffic is tiny).
- **Logs** — structured via `tracing` too: human-readable to stderr by default,
  JSON when `log.format = "json"` (containers). Level per module via
  `NOCTURNAL_LOG` / `RUST_LOG`. Discord 10062-class noise at `debug`, real
  faults at `warn/error`. Log events ride the OTLP pipe when it's on — the bot
  shows up in the guild's Perses/Jaeger next to Zeal telemetry, and Ourios can
  ingest the same stream for long retention.
- **Metrics** — OTLP metrics plus an optional Prometheus `/metrics` bind:
  commands total/errors by kind, interaction→ack and interaction→fsync latency
  histograms, event log seq (gauge), WAL fsync latency, compaction runs/failures,
  active auctions/raids, Discord gateway reconnects, timer drift. A `heartbeat`
  metric ticks every scheduler cycle — its absence is the page.
- **Health** — small HTTP server (bind configurable, default off outside
  containers): `/healthz` (process live), `/readyz` (replay done + gateway
  connected + WAL writable), `/metrics`. Used by Docker `HEALTHCHECK` and any
  future orchestrator.

### Semantic conventions (`semconv/`)

Telemetry names are governed, not improvised — same Weaver workflow as Ourios:

- `semconv/` holds the registry (attributes, metrics, spans). Attribute names
  mirror the event taxonomy (`nocturnal.event.kind`, `nocturnal.auction.id`,
  …) so the ledger, traces, logs, and dashboards share one vocabulary.
- `weaver registry check -r semconv` gates CI; `weaver registry generate`
  emits the `nocturnal-telemetry` constants crate (M3) — a misspelled
  attribute is a compile error, not an empty dashboard panel.
- **Cardinality rule:** actor/player ids never appear on metrics — only on
  spans and in the ledger. Metric attributes come exclusively from the
  registry's bounded enums and low-cardinality ids.
- Generated markdown docs of the registry feed the Perses dashboard work and
  give Ourios stable attribute names to prune on.

## Container & deployment

- **Image** — multi-stage build: `rust:1.x` builder → `gcr.io/distroless/cc`
  (or static musl → `scratch`) runtime. Non-root user, read-only rootfs, data
  dir volume at `/data`. Multi-arch `linux/amd64` + `linux/arm64` (NAS).
  Image tags: `vX.Y.Z` + `sha-<short>`; `latest` never deployed by tag.
- **Compose** — `docker-compose.yml` (prod) with volume, env, healthcheck,
  `restart: unless-stopped`, log rotation; a NAS variant mirroring the legacy
  repo's Container Manager quirks (no logging block).
- **Lifecycle** — SIGTERM/SIGINT → graceful stop: stop intake, finish the
  in-flight command, fsync, close gateway cleanly, exit 0 (bounded by a grace
  timeout). Boot: flock data dir → replay → re-arm timers → connect gateway →
  ready. Crash-restart is safe *by design*; the restart policy is the
  supervisor.
- **No Pterodactyl-style boot installs** — the image is the artifact; deploys
  are image swaps, restart time is process start + sub-second replay.

## CI / release

GitHub Actions:
- Every push/PR: `cargo fmt --check`, `clippy -D warnings`, `cargo test`
  (includes replay-determinism + crash-injection suites), `cargo deny`
  (licenses + advisories).
- Tags `v*`: build + push multi-arch image to GHCR, attach release binaries,
  generate SBOM. Release notes list any event-schema `(kind, v)` additions.
- CI replays the recorded production log fixture (once M2 produces it) against
  every change to `apply` — state-hash compared (hazard B3).

## Data & backups

- Layout: `/data/wal/*.jsonl` (active tail), `/data/events/*.parquet`
  (compacted history), `/data/LOCK`.
- `/backup` command = tar.gz of the data dir streamed to Discord (complete by
  construction — it *is* the ledger); plus a scheduled backup to a configurable
  path/volume with retention. Restore = untar + start; rehearsed in M6.
- RPO: one fsynced event (i.e. zero acknowledged writes lost); stated in the
  runbook after B7 verification on the real host.

## Versioning

- SemVer for the binary/image. Event payload schemas versioned independently
  per `(kind, v)` (events.md) — additive forever; the binary always reads all
  historical versions. `nocturnal --version` prints build info + newest event
  schema versions.
