# Operations & production readiness

What "a modern system" means for this bot, concretely. Everything here is a
requirement on the M3/M6 milestones, not aspiration; each item lands with a
test or a runbook line.

## Configuration

Three layers, strictly ordered (later wins), all validated at boot with
fail-fast errors that name the offending key:

1. **Config file** — `nocturnal.yaml` (YAML like the rest of the stack; TOML still accepted, chosen by extension), path via `--config` / `NOCTURNAL_CONFIG`.
   All operational knobs: data dir, WAL segment size, compaction cadence,
   OTLP endpoint/protocol/headers, log format/level, health bind address,
   HTTP timeouts, backup schedule. Ships with a commented example
   (`nocturnal.example.yaml`); every knob has a sane default — an empty file
   is a valid config.
2. **Environment** — `NOCTURNAL_*` overrides for every file key (12-factor,
   container-friendly), e.g. `NOCTURNAL_OTLP__ENDPOINT`. 
3. **Telemetry** — the standard OpenTelemetry environment only
   (`OTEL_EXPORTER_OTLP_ENDPOINT` and per-signal variants, `_PROTOCOL`,
   `_HEADERS`, `_TIMEOUT`, `OTEL_SERVICE_NAME`, `OTEL_RESOURCE_ATTRIBUTES`,
   `OTEL_SDK_DISABLED`). The bot defines **no** telemetry config keys of its
   own: operators configure it like any other OTel component, and the SDK
   resolves endpoints, per-signal paths, headers and timeouts itself. No
   endpoint set = local logging only, all instruments no-ops.
4. **Secrets** — never in the file: `DISCORD_TOKEN` (or `DISCORD_TOKEN_FILE`
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
- **Span model** (OTel span-kind guidance):
  - `command.<name>` — **SERVER**: Discord interaction handling (incoming
    request/response; Discord awaits the reply inside its 3-second window).
  - `scheduler.cycle` — **INTERNAL** root for timer-driven work.
  - `ledger.execute` — **INTERNAL**, child of whichever span caused it. The
    trace context is propagated explicitly across the writer channel (the
    writer is a thread we start ourselves, so implicit context does not
    flow), with `ledger.decide` / `wal.append` / `ledger.apply` beneath it —
    a slow command says *which phase* was slow.
  - `discord.request` — **CLIENT**: outbound Discord REST, created before the
    call; serenity's own request spans nest underneath.
  - Span status is set explicitly: a typed rejection is `OK` (a valid
    outcome), only storage failures are `ERROR`.
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

### The log backend (Ourios)

Logs land in Ourios, queryable from the same Perses project as the metrics and
traces. The path, and the three things that surprise people about it:

    bot --OTLP/http--> eq-gateway :4319 --otlphttp/ourios--> receiver :4318
        --publish--> Parquet --> querier :4320 --HTTPProxy--> Perses panel

- **The tenant is named out of band** (RFC 0046, Ourios **0.8.0**). The
  collector sets `x-ourios-tenant: nocturnal` on the export, and the Perses
  datasource sends the same name on every query. Resource attributes describe
  the producer; they never choose the tenant. 0.7.0 derived the tenant from
  `service.name` instead — that model was wrong, and 0.8.0 is the fix. Keep
  the header, `OTEL_SERVICE_NAME`, and the datasource's `tenant` in step.
- **Records are ~5 minutes behind.** A partition flushes when it is big enough
  or when its oldest buffered record exceeds `SINK_MAX_BUFFER_AGE` (300s),
  swept every 30s. At the bot's volume it is always the age trigger, so a
  quiet bot's logs take ~5 minutes to appear. An empty panel right after an
  incident means *wait*, not *broken* — `journalctl -u nocturnal` is the
  real-time view.
- **Upgrading past 0.8.0 needs a drained WAL.** Pre-RFC-0046 WAL frames carry
  no tenant and 0.8.0 refuses to replay them (loudly, at startup, rather than
  guessing a tenant). Let the old version publish everything to Parquet first,
  then start the new one on a fresh `OURIOS_WAL_ROOT`. Published Parquet is
  unaffected and stays queryable across the upgrade.

The querier binds **4320** here, not the documented 4319 — the eq-gateway
collector already owns 4319 on this host.

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

### Off-site archive (Hetzner Object Storage / any S3)

Compacted Parquet partitions are immutable, so they mirror cleanly to object
storage. The WAL stays local — it is the fsync hot path — but everything older
than the current segment lives in two places, which is what stops the VM's
disk being a single point of failure for guild history.

* **Write-through**: a partition is uploaded only after it has been written,
  fsynced *and verified readable* locally.
* **Read-through on boot**: any partition the archive holds and the local disk
  lacks is downloaded before replay, so a fresh disk (or an empty container
  volume) rebuilds its history by itself.
* **Never load-bearing**: an unreachable archive logs a warning and the bot
  carries on — local history is authoritative for replay.

Configuration follows the same rule as telemetry: credentials and endpoint use
the conventional AWS variables, and only the bucket and prefix — which have no
standard variable — are ours.

    # /etc/nocturnal/env
    AWS_ACCESS_KEY_ID=...
    AWS_SECRET_ACCESS_KEY=...
    AWS_ENDPOINT_URL_S3=https://fsn1.your-objectstorage.com   # Hetzner region
    AWS_REGION=fsn1

    # nocturnal.yaml
    [archive]
    bucket = "nocturnal-ledger"
    prefix = "prod"          # optional; keeps test and prod side by side

Setting it up on Hetzner: create a bucket in the Cloud Console (Object
Storage), generate S3 credentials, and point `AWS_ENDPOINT_URL_S3` at that
bucket's region endpoint. Nothing else changes — with no `bucket` set the
archive is simply off.

### The auction bell

The legacy bell rings in the raid voice channels when a short auction opens.
The sound is **compiled into the binary** (34 KB), so there is no asset to
deploy and nothing to fetch on the hot path; `bell.path` overrides it with a
file, and `bell.enabled: false` turns it off.

Voice needs libopus, which is built from source and linked statically, so the
release binary stays a single static artifact — no ffmpeg, no shared library,
no ~75 MB voice stack in the image (the legacy bot's `package.json` carried
exactly that). Two build-time notes: `LIBOPUS_STATIC` is set by the
`audiopus_sys/static` feature, and the vendored libopus ships a CMakeLists too
old for CMake 4, so builds export `CMAKE_POLICY_VERSION_MINIMUM=3.5`.

Discord now requires **DAVE** (its end-to-end encrypted voice protocol) — a
voice connection without it is closed with `4017 E2EE/DAVE protocol required`,
which looks exactly like a bot that joins and says nothing. songbird 0.6
implements it; do not downgrade.

Two ways to test voice without running an auction: `/belltest` in Discord
(also reports whether the bot has Connect and Speak), or from a shell on the
host, `nocturnal --bell-test <guild_id>:<voice_channel_id>` — it connects,
joins, plays, logs each track state, and exits, touching neither the ledger
nor the instance lock.

Discord permissions: the bot needs **Connect** and **Speak** in the raid voice
channels. Without them the bell logs "bell skipped" and the auction is
unaffected — it is decorative by construction: its own task, bounded by a
ten-second timeout, every failure swallowed.

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
