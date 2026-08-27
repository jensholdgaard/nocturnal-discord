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

### Ports on the observability VM

Both standard OTLP ports are already spoken for on `eq-perses`, so nothing
here sits where an OTel-shaped guess would put it. Each service took the next
free number as it arrived, in this order:

| Port | Owner | Since | Notes |
|---|---|---|---|
| 4318 | Ourios OTLP receiver | 2026-08-23 | The standard OTLP/HTTP port, taken by the log backend |
| 4319 | eq-gateway (otelcol) | 2026-07-31 | Member ingest, bearer-authenticated; Caddy proxies `/otlp/*` here, and the bot exports here |
| 4320 | Ourios querier | 2026-08-23 | Ourios documents 4319 for this; the gateway already had it |
| 14317 | Jaeger OTLP/gRPC | | Enabled by Jaeger's config; nothing here writes to it |
| 14318 | Jaeger OTLP/HTTP | | Traces, written by the gateway |
| 9090 | Prometheus OTLP | | Metrics, written by the gateway |

Two consequences worth knowing before touching any of it. **Nothing listens
on 4317**, the standard OTLP/gRPC port: every ingest path here is HTTP, and
the only gRPC listener in the stack is Jaeger's 14317, which nothing writes
to. And **the gateway cannot simply be moved to 4318**: it chose 4319 on
2026-07-31 when 4318 was free, but Ourios took 4318 three weeks later, so
what was once an arbitrary choice is now load-bearing. Moving it would need
Ourios moved first, three coordinated restarts across two repos, and would
end up exactly where we already are.

Everything above binds to localhost. Members never see a port: they post to
the public `/otlp/*` path and Caddy routes it.

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
- **Metrics** — OTLP metrics plus an optional Prometheus `/metrics` bind. The
  set is organised around the four golden signals; see below.
- **Health** — small HTTP server (bind configurable, default off outside
  containers): `/healthz` (process live), `/readyz` (replay done + gateway
  connected + WAL writable), `/metrics`. Used by Docker `HEALTHCHECK` and any
  future orchestrator.

### The four golden signals

Every metric in `semconv/registry/metrics.yaml` is emitted, and the Perses
`Overview` dashboard has a row per signal. What to look at, and what it means:

Every duration histogram declares its own **bucket boundaries in seconds**.
The SDK's defaults (`0, 5, 10, 25, … 10000`) are milliseconds; recording
seconds against them put every observation into the first `(0, 5]` bucket, and
`histogram_quantile` interpolated inside it — a p95 reported as "4.75 s" for
work that actually took 1.7 ms. A quantile is only as precise as the bucket it
lands in. `nocturnal.interaction.ack.duration` has an exact edge on 3.0, so
`..._bucket{le="3"}` answers "how many interactions blew Discord's window?"
without interpolating.

Dashboard quantile panels are written
`histogram_quantile(...) and on() (sum(rate(..._count[5m])) > 0)`. Without the
guard an idle bot renders `NaN`: `rate()` over an untouched histogram is zero
in every bucket, so `histogram_quantile` returns a series whose *value* is NaN,
and the usual `or vector(0)` fallback never fires because a series does exist.
The panels are deliberately blank rather than zero when nothing has happened —
a p95 over no observations has no answer, and 0 would claim a speed nothing
achieved.

**Latency.** `nocturnal.interaction.ack.duration` is the one that matters:
Discord hangs up on an unacknowledged interaction after three seconds, and
that deadline killed the legacy bot repeatedly. It is measured from the
interaction's *snowflake* — the instant Discord created it — so the gateway
hop we cannot otherwise see is included; timing from a local `Instant` would
have quietly excluded exactly the part that fails first under load. It is
split by `nocturnal.interaction.kind`, because buttons (the bid-storm path)
and commands (the officer path) saturate independently.
`nocturnal.interaction.commit.duration` then covers receive→fsync, and
`nocturnal.wal.fsync.duration` the durability step inside it.

**Traffic.** `nocturnal.commands` by command and outcome,
`nocturnal.ledger.events` by event kind.

**Errors.** `nocturnal.commands{outcome="error"}` is infrastructure failure —
a *rejection* is a healthy typed refusal and is counted separately.
`nocturnal.compaction.runs{outcome="error"}` matters more than its rate
suggests: nothing is watching a compaction that fails on a timer, and the WAL
it should have drained just keeps growing (hazard B5).
`nocturnal.discord.reconnects` completes the picture.

**Saturation.** The weakest signal to instrument and the most useful once you
have it.

- `nocturnal.scheduler.drift` — how late raid ticks and auction closes fired
  against their due instant. Because timers here are state rather than
  callbacks (hazard B6), a backed-up writer delays the work *between* cycles,
  so drift moves before commit latency does. One 10-second cycle of drift is
  the floor and is entirely normal.
- `nocturnal.wal.size` and `system.filesystem.usage` — the compaction backlog
  against the disk it is consuming. Read together; either alone is unreadable.
- `process.*` — CPU, RSS, virtual size, descriptors, threads, uptime, sampled
  from `/proc/self`. The bot reports these itself because the VM runs no
  node_exporter and the collector only reports on itself. RSS holds the entire
  replayed ledger, so steady growth *between restarts* is a leak rather than
  load. These use OpenTelemetry's own metric names, not `nocturnal.*`: they
  describe a process, not this bot, and a standard dashboard should read them
  without translation.
- `nocturnal.auctions.active` / `nocturnal.raids.active` — concurrent work in
  the ledger. More than one active raid per guild violates a core invariant.

`nocturnal.scheduler.heartbeat` sits outside the four: it ticks once per
10-second cycle, and its *absence* is the liveness page — timers stopping
while the process stays up is otherwise completely silent.

Note that a metric only appears in Prometheus once it has been recorded at
least once. A counter at zero on a freshly started bot means "has not happened
yet", not "not instrumented".

`OTEL_METRIC_EXPORT_INTERVAL=15000` on the VM, against an SDK default of 60s.
A bid storm lasts about twenty seconds: at the default the entire burst lands
in a single exported sample, `rate()` has no increase to compute, and every
quantile panel reads blank for precisely the event you wanted to look at. The
same effect appears for one interval after any restart, because counters and
histograms have no series until their first observation — so a burst that is
also a series' birth is invisible whatever the interval. Gauges
(`nocturnal.ledger.seq`, `wal.size`, the `process.*` set) are seeded at boot
and do not have this blind spot.

### Compaction

Sealed WAL segments roll into month-partitioned Parquet. **This does not run on
its own unless you configure it:**

```yaml
compaction:
  interval_secs: 86400   # nightly; unset = never
```

(or `NOCTURNAL_COMPACTION__INTERVAL_SECS`). With it unset — the default, and
what every deployment did before this option existed — the WAL only ever grows;
`nocturnal.wal.size` is the gauge that says how much runway is left.

A scheduled run seals the active segment first, so it means "drain the WAL"
rather than "drain it only once it passed 16 MB". Runs happen on the writer
thread, which is why the interval is floored at 60 seconds: commands queue
behind a compaction. It is crash-safe and idempotent by construction (temp
file, fsync, rename, read back and count, and only then delete a WAL segment),
so a run interrupted anywhere is simply redone by the next one, and a re-run
with nothing to move is a successful no-op.

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
collector already owns 4319 on this host. See the port table above for who
holds what, and why none of it is where you would guess.

### Attribute naming

Every attribute name — on metrics, spans **and log records** — is declared in
`semconv/` and reaches the code as a generated constant, so a typo is a compile
error rather than an empty panel. Log records were the gap: until 2026-08-25
they carried bare `tracing` field names (`guild_id`, `count`, `error`,
`accepted`), which broke three of OpenTelemetry's naming rules at once —
names SHOULD be namespaced with a dot delimiter, SHOULD be precise rather than
ambiguous (upstream's own example: `security_rule`, not `rule`), and
application-specific names SHOULD carry the application's prefix.

It was not only untidy. `ledger.execute` spans already carried
`nocturnal.guild.id` while the log line beside them said `guild_id`, so the
same concept had two names and a query on the registry name silently missed
every log record.

The rules we follow, from [OTel's naming guidance][naming]:

- Ours are prefixed `nocturnal.*` — our application name, which does not
  collide with an existing semconv namespace. We never prefix our own
  attributes with someone else's namespace (`otel.*` is reserved outright).
- Lowercase, dot-delimited namespaces, `snake_case` *within* one component,
  following `{object}.{property}`.
- Where upstream already defines the concept we use theirs verbatim rather
  than reinventing it: `http.request.method`, `url.path`, `file.path`,
  `cpu.mode`, `system.filesystem.state`, `service.name`.
- Precise, never ambiguous. `count` became
  `nocturnal.auction.open.count` and `nocturnal.archive.partitions_restored`;
  `user` split into `nocturnal.discord.user.id` and `.user.name`; `path`
  split into `url.path` (a Discord REST route) and `file.path` (a bell
  sound) — these were genuinely different things sharing a name.
- Failures use `nocturnal.error.message`, not `error.message`: upstream
  deprecated the latter in favour of exactly this domain-specific pattern,
  and reserved `error.type` for a low-cardinality error *class*.
- Plural plus an array type only when it really is many entities
  (`nocturnal.compaction.partitions`); counts take a `.count` suffix.

Note `cpu.mode` in that list: `process.cpu.state` is **deprecated** upstream,
which unified every `*.cpu.state` into one shared `cpu.mode`.

Records written before this change keep their old bare names in Parquet —
history is not rewritten — so a query spanning the cutover may need both.

[naming]: https://opentelemetry.io/docs/specs/semconv/general/naming/

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
