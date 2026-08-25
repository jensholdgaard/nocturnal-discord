//! OTLP wiring.
//!
//! Configuration is **entirely** the standard OpenTelemetry environment:
//! `OTEL_EXPORTER_OTLP_ENDPOINT` (and per-signal variants), `_PROTOCOL`,
//! `_HEADERS`, `_TIMEOUT`, `OTEL_SERVICE_NAME`, `OTEL_RESOURCE_ATTRIBUTES`,
//! `OTEL_SDK_DISABLED`. We invent no names of our own and set nothing
//! programmatically, so this bot is configured like any other OTel component.
//! With no endpoint configured, only local `fmt` logging is installed and every
//! instrument is a no-op.

use anyhow::Context as _;
use opentelemetry::global;
use opentelemetry::metrics::{Counter, Gauge, Histogram};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_sdk::logs::{BatchLogProcessor, LogProcessor, SdkLogRecord, SdkLoggerProvider};
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::trace::{BatchSpanProcessor, SdkTracerProvider, SpanData, SpanProcessor};
use opentelemetry_sdk::Resource;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::Layer as _;

use crate::metric;

#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    /// Service name used only when `OTEL_SERVICE_NAME` is unset.
    pub default_service_name: String,
    /// tracing filter directive for local logging (e.g. "info").
    pub log_filter: String,
    pub log_json: bool,
}

/// Keeps providers alive; flushes and shuts them down on drop.
pub struct TelemetryGuard {
    tracer: Option<SdkTracerProvider>,
    logger: Option<SdkLoggerProvider>,
    meter: Option<SdkMeterProvider>,
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Some(p) = &self.tracer {
            let _ = p.shutdown();
        }
        if let Some(p) = &self.logger {
            let _ = p.shutdown();
        }
        if let Some(p) = &self.meter {
            let _ = p.shutdown();
        }
    }
}

fn export_enabled() -> bool {
    if std::env::var("OTEL_SDK_DISABLED").is_ok_and(|v| v.eq_ignore_ascii_case("true")) {
        return false;
    }
    [
        "OTEL_EXPORTER_OTLP_ENDPOINT",
        "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
        "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
        "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
    ]
    .iter()
    .any(|v| std::env::var(v).is_ok())
}

/// The one dispatch the Rust SDK cannot make from the environment: gRPC vs
/// HTTP transport. Everything downstream (URL, per-signal paths, headers,
/// timeouts, compression) is resolved by the SDK from the same env vars.
fn use_http(signal_protocol_var: &str) -> bool {
    std::env::var(signal_protocol_var)
        .or_else(|_| std::env::var("OTEL_EXPORTER_OTLP_PROTOCOL"))
        .unwrap_or_default()
        .starts_with("http")
}

macro_rules! exporter {
    ($builder:expr, $signal_protocol_var:expr) => {{
        if use_http($signal_protocol_var) {
            $builder.with_http().build()
        } else {
            $builder.with_tonic().build()
        }
    }};
}

/// Export allowlist (OTel sensitive-data guidance: don't collect what you
/// didn't deliberately author). Only spans/events from our own crates leave
/// the process. NOT serenity: its request path emits five generic internal
/// spans per call whose fields dump whole requests including auth (the B13
/// incident) — we emit our own CLIENT `discord.request` spans instead, which
/// cover the same latency with attributes we author.
fn export_targets() -> Targets {
    Targets::new()
        .with_target("nocturnal", LevelFilter::TRACE)
        .with_target("nocturnal_core", LevelFilter::TRACE)
        .with_target("nocturnal_store", LevelFilter::TRACE)
        .with_target("nocturnal_telemetry", LevelFilter::TRACE)
        .with_target("nocturnal_discord", LevelFilter::TRACE)
        .with_target("nocturnal_provision", LevelFilter::TRACE)
        .with_target("nocturnal_migrate", LevelFilter::TRACE)
}

/// Attribute allowlist for exported spans (fail closed). Only keys we
/// deliberately author, or harmless runtime metadata, survive.
fn safe_attribute(key: &str) -> bool {
    key.starts_with("nocturnal.")
        || key.starts_with("code.")
        || key.starts_with("thread.")
        || key.starts_with("otel.")
        || key.starts_with("http.")
        || key.starts_with("server.")
        || key.starts_with("network.")
        || key.starts_with("error.")
        || key == "busy_ns"
        || key == "idle_ns"
        || key == "events"
}

/// Span processor decorator applying [`safe_attribute`] before export.
#[derive(Debug)]
struct RedactSpans<P>(P);

impl<P: SpanProcessor> SpanProcessor for RedactSpans<P> {
    fn on_start(&self, span: &mut opentelemetry_sdk::trace::Span, cx: &opentelemetry::Context) {
        self.0.on_start(span, cx);
    }

    fn on_end(&self, mut span: SpanData) {
        span.attributes.retain(|kv| safe_attribute(kv.key.as_str()));
        self.0.on_end(span);
    }

    fn force_flush(&self) -> opentelemetry_sdk::error::OTelSdkResult {
        self.0.force_flush()
    }

    fn shutdown(&self) -> opentelemetry_sdk::error::OTelSdkResult {
        self.0.shutdown()
    }

    fn shutdown_with_timeout(
        &self,
        timeout: std::time::Duration,
    ) -> opentelemetry_sdk::error::OTelSdkResult {
        self.0.shutdown_with_timeout(timeout)
    }

    /// MUST delegate: the SDK hands the Resource (service.name and friends) to
    /// processors this way. Without it the wrapped exporter ships spans with
    /// an empty resource and they land under "missing-service-name".
    fn set_resource(&mut self, resource: &Resource) {
        self.0.set_resource(resource);
    }
}

/// Log processor decorator that gives every record a `Timestamp`.
///
/// A `tracing` event carries no timestamp of its own, so the appender fills in
/// only `ObservedTimestamp` and leaves `Timestamp` unset — which serialises as
/// 0 on the wire. Backends are entitled to sort by it, and then every line the
/// bot ever logged sits at the Unix epoch. Stamping the observed time here
/// (the moment the event was emitted, in-process) is both the closest true
/// value and the one the OTLP spec nominates as the fallback.
#[derive(Debug)]
struct StampLogs<P>(P);

impl<P: LogProcessor> LogProcessor for StampLogs<P> {
    fn emit(&self, data: &mut SdkLogRecord, instrumentation: &opentelemetry::InstrumentationScope) {
        if data.timestamp().is_none() {
            use opentelemetry::logs::LogRecord as _;
            let observed = data
                .observed_timestamp()
                .unwrap_or_else(std::time::SystemTime::now);
            data.set_timestamp(observed);
        }
        self.0.emit(data, instrumentation);
    }

    fn force_flush(&self) -> opentelemetry_sdk::error::OTelSdkResult {
        self.0.force_flush()
    }

    fn shutdown(&self) -> opentelemetry_sdk::error::OTelSdkResult {
        self.0.shutdown()
    }

    fn shutdown_with_timeout(
        &self,
        timeout: std::time::Duration,
    ) -> opentelemetry_sdk::error::OTelSdkResult {
        self.0.shutdown_with_timeout(timeout)
    }

    /// MUST delegate, for the same reason [`RedactSpans::set_resource`] must:
    /// this is how the SDK hands the Resource down, and swallowing it ships
    /// records with no service.name.
    fn set_resource(&mut self, resource: &Resource) {
        self.0.set_resource(resource);
    }
}

/// Install the global tracing subscriber (fmt + OTLP layers) and the global
/// meter provider. Call once, early; hold the guard until exit.
pub fn init(cfg: &TelemetryConfig) -> anyhow::Result<TelemetryGuard> {
    let filter = tracing_subscriber::EnvFilter::try_from_env("NOCTURNAL_LOG")
        .or_else(|_| tracing_subscriber::EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&cfg.log_filter));

    let fmt_layer = if cfg.log_json {
        tracing_subscriber::fmt::layer().json().boxed()
    } else {
        tracing_subscriber::fmt::layer().boxed()
    };

    if !export_enabled() {
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt_layer)
            .init();
        return Ok(TelemetryGuard {
            tracer: None,
            logger: None,
            meter: None,
        });
    }

    // OTEL_SERVICE_NAME / OTEL_RESOURCE_ATTRIBUTES win; ours is the fallback.
    let resource = if std::env::var("OTEL_SERVICE_NAME").is_ok() {
        Resource::builder().build()
    } else {
        Resource::builder()
            .with_service_name(cfg.default_service_name.clone())
            .build()
    };

    let span_exporter = exporter!(
        opentelemetry_otlp::SpanExporter::builder(),
        "OTEL_EXPORTER_OTLP_TRACES_PROTOCOL"
    )
    .context("building OTLP span exporter")?;
    let tracer_provider = SdkTracerProvider::builder()
        .with_span_processor(RedactSpans(
            BatchSpanProcessor::builder(span_exporter).build(),
        ))
        .with_resource(resource.clone())
        .build();
    let otel_trace_layer = tracing_opentelemetry::layer()
        .with_tracer(tracer_provider.tracer("nocturnal"))
        .with_filter(export_targets());

    let log_exporter = exporter!(
        opentelemetry_otlp::LogExporter::builder(),
        "OTEL_EXPORTER_OTLP_LOGS_PROTOCOL"
    )
    .context("building OTLP log exporter")?;
    let logger_provider = SdkLoggerProvider::builder()
        .with_log_processor(StampLogs(BatchLogProcessor::builder(log_exporter).build()))
        .with_resource(resource.clone())
        .build();
    let otel_log_layer =
        OpenTelemetryTracingBridge::new(&logger_provider).with_filter(export_targets());

    let metric_exporter = exporter!(
        opentelemetry_otlp::MetricExporter::builder(),
        "OTEL_EXPORTER_OTLP_METRICS_PROTOCOL"
    )
    .context("building OTLP metric exporter")?;
    let meter_provider = SdkMeterProvider::builder()
        .with_periodic_exporter(metric_exporter)
        .with_resource(resource)
        .build();
    global::set_meter_provider(meter_provider.clone());

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .with(otel_trace_layer)
        .with(otel_log_layer)
        .init();

    tracing::info!(
        endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").unwrap_or_default(),
        protocol = std::env::var("OTEL_EXPORTER_OTLP_PROTOCOL").unwrap_or_default(),
        "OTLP export enabled (configured by OTEL_* environment)"
    );

    Ok(TelemetryGuard {
        tracer: Some(tracer_provider),
        logger: Some(logger_provider),
        meter: Some(meter_provider),
    })
}

/// The process-wide instruments.
///
/// Built on first use, which is always after `init()` has installed the meter
/// provider: building them earlier would bind them to the no-op meter for the
/// life of the process, and they would silently record nothing.
pub fn metrics() -> &'static Metrics {
    static METRICS: std::sync::OnceLock<Metrics> = std::sync::OnceLock::new();
    METRICS.get_or_init(Metrics::new)
}

/// The ledger's metric instruments, names straight from the registry. With no
/// exporter installed these are no-ops — always safe to record.
pub struct Metrics {
    pub commands: Counter<u64>,
    /// Interaction creation to `defer` — the clock Discord actually enforces.
    pub ack_duration: Histogram<f64>,
    pub commit_duration: Histogram<f64>,
    pub ledger_events: Counter<u64>,
    pub ledger_seq: Gauge<u64>,
    pub wal_fsync_duration: Histogram<f64>,
    /// Bytes of WAL not yet compacted into Parquet.
    pub wal_size: Gauge<u64>,
    pub compaction_runs: Counter<u64>,
    pub auctions_active: Gauge<u64>,
    pub raids_active: Gauge<u64>,
    /// How late a derived timer fired against its due instant.
    pub scheduler_drift: Histogram<f64>,
    pub discord_reconnects: Counter<u64>,
    /// Requests delayed by the client-side rate limiter — fires BEFORE 429s.
    pub ratelimit_delays: Counter<u64>,
    pub ratelimit_delay_duration: Histogram<f64>,
    pub gateway_latency: Gauge<f64>,
}

impl Metrics {
    pub fn new() -> Metrics {
        let meter = global::meter("nocturnal");
        Metrics {
            commands: meter
                .u64_counter(metric::NOCTURNAL_COMMANDS)
                .with_unit("{interaction}")
                .build(),
            ack_duration: meter
                .f64_histogram(metric::NOCTURNAL_INTERACTION_ACK_DURATION)
                .with_unit("s")
                .build(),
            commit_duration: meter
                .f64_histogram(metric::NOCTURNAL_INTERACTION_COMMIT_DURATION)
                .with_unit("s")
                .build(),
            ledger_events: meter
                .u64_counter(metric::NOCTURNAL_LEDGER_EVENTS)
                .with_unit("{event}")
                .build(),
            ledger_seq: meter
                .u64_gauge(metric::NOCTURNAL_LEDGER_SEQ)
                .with_unit("{event}")
                .build(),
            wal_fsync_duration: meter
                .f64_histogram(metric::NOCTURNAL_WAL_FSYNC_DURATION)
                .with_unit("s")
                .build(),
            wal_size: meter
                .u64_gauge(metric::NOCTURNAL_WAL_SIZE)
                .with_unit("By")
                .build(),
            compaction_runs: meter
                .u64_counter(metric::NOCTURNAL_COMPACTION_RUNS)
                .with_unit("{run}")
                .build(),
            auctions_active: meter
                .u64_gauge(metric::NOCTURNAL_AUCTIONS_ACTIVE)
                .with_unit("{auction}")
                .build(),
            raids_active: meter
                .u64_gauge(metric::NOCTURNAL_RAIDS_ACTIVE)
                .with_unit("{raid}")
                .build(),
            scheduler_drift: meter
                .f64_histogram(metric::NOCTURNAL_SCHEDULER_DRIFT)
                .with_unit("s")
                .build(),
            discord_reconnects: meter
                .u64_counter(metric::NOCTURNAL_DISCORD_RECONNECTS)
                .with_unit("{reconnect}")
                .build(),
            ratelimit_delays: meter
                .u64_counter(metric::NOCTURNAL_DISCORD_RATELIMIT_DELAYS)
                .with_unit("{delay}")
                .build(),
            ratelimit_delay_duration: meter
                .f64_histogram(metric::NOCTURNAL_DISCORD_RATELIMIT_DELAY_DURATION)
                .with_unit("s")
                .build(),
            gateway_latency: meter
                .f64_gauge(metric::NOCTURNAL_DISCORD_GATEWAY_LATENCY)
                .with_unit("s")
                .build(),
        }
    }

    /// One command through the single writer: count + latency, tagged with
    /// registry attributes only (never player ids — cardinality rule).
    pub fn record_command(
        &self,
        command: &'static str,
        outcome: &'static str,
        rejection: Option<&'static str>,
        elapsed_s: f64,
    ) {
        let mut attrs = vec![
            KeyValue::new(crate::attr::NOCTURNAL_COMMAND, command),
            KeyValue::new(crate::attr::NOCTURNAL_DECISION_OUTCOME, outcome),
        ];
        if let Some(r) = rejection {
            attrs.push(KeyValue::new(crate::attr::NOCTURNAL_DECISION_REJECTION, r));
        }
        self.commands.add(1, &attrs);
        self.commit_duration.record(
            elapsed_s,
            &[KeyValue::new(crate::attr::NOCTURNAL_COMMAND, command)],
        );
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Metrics::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{export_targets, safe_attribute, use_http, RedactSpans, StampLogs};
    use tracing::Level;

    /// A `tracing` event has no timestamp of its own, so records reach the
    /// exporter with `Timestamp` unset and serialise as 0 — every log line
    /// lands at the Unix epoch in any backend that sorts by it. Observed in
    /// production against the log store.
    #[test]
    fn stamp_wrapper_fills_in_the_missing_timestamp() {
        use opentelemetry::logs::{Logger as _, LoggerProvider as _};
        use opentelemetry_sdk::logs::{InMemoryLogExporter, SdkLoggerProvider, SimpleLogProcessor};

        let exporter = InMemoryLogExporter::default();
        let provider = SdkLoggerProvider::builder()
            .with_log_processor(StampLogs(SimpleLogProcessor::new(exporter.clone())))
            .build();

        let logger = provider.logger("test");
        // Emitted exactly as the appender does it: observed time only.
        logger.emit(logger.create_log_record());

        let emitted = exporter.get_emitted_logs().expect("emitted logs");
        let record = &emitted.first().expect("one record").record;
        assert!(
            record.timestamp().is_some(),
            "record left the processor with no Timestamp; it would export as 0"
        );
        assert_eq!(
            record.timestamp(),
            record.observed_timestamp(),
            "the fallback must be the observed time, not a later re-stamp"
        );
    }

    /// The redaction wrapper must delegate *every* hook — a missed
    /// `set_resource` silently strips service.name from exported spans.
    #[test]
    fn redact_wrapper_forwards_set_resource() {
        use opentelemetry_sdk::error::OTelSdkResult;
        use opentelemetry_sdk::trace::{SpanData, SpanProcessor};
        use opentelemetry_sdk::Resource;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        #[derive(Debug, Clone)]
        struct Spy(Arc<AtomicBool>);
        impl SpanProcessor for Spy {
            fn on_start(&self, _: &mut opentelemetry_sdk::trace::Span, _: &opentelemetry::Context) {
            }
            fn on_end(&self, _: SpanData) {}
            fn force_flush(&self) -> OTelSdkResult {
                Ok(())
            }
            fn shutdown_with_timeout(&self, _: std::time::Duration) -> OTelSdkResult {
                Ok(())
            }
            fn set_resource(&mut self, _: &Resource) {
                self.0.store(true, Ordering::Release);
            }
        }

        let seen = Arc::new(AtomicBool::new(false));
        let mut wrapped = RedactSpans(Spy(seen.clone()));
        wrapped.set_resource(&Resource::builder().with_service_name("nocturnal").build());
        assert!(
            seen.load(Ordering::Acquire),
            "set_resource was not delegated"
        );
    }

    #[test]
    fn span_attribute_allowlist_fails_closed() {
        for ok in [
            "nocturnal.command",
            "code.line.number",
            "http.status_code",
            "server.address",
            "otel.kind",
            "busy_ns",
        ] {
            assert!(safe_attribute(ok), "{ok}");
        }
        for bad in [
            "req", "response", "event", "self", "settings", "token", "presence",
        ] {
            assert!(!safe_attribute(bad), "{bad} must be stripped");
        }
    }

    /// The allowlist is the leak barrier — pin it.
    #[test]
    fn only_our_crates_are_exported() {
        let targets = export_targets();
        for ours in [
            "nocturnal",
            "nocturnal::driver",
            "nocturnal_core::decide",
            "nocturnal_discord::commands",
        ] {
            assert!(
                targets.would_enable(ours, &Level::DEBUG),
                "{ours} must export"
            );
        }
        for theirs in [
            "serenity::gateway::shard",
            "serenity::http::client",
            "poise::dispatch",
            "hyper_util::client",
            "reqwest",
            "opentelemetry_sdk",
        ] {
            assert!(
                !targets.would_enable(theirs, &Level::ERROR),
                "{theirs} must NOT export"
            );
        }
    }

    /// Transport selection follows the standard env vars, never our own.
    #[test]
    fn transport_follows_standard_env() {
        temp_env::with_vars(
            [
                ("OTEL_EXPORTER_OTLP_PROTOCOL", Some("http/protobuf")),
                ("OTEL_EXPORTER_OTLP_TRACES_PROTOCOL", None),
            ],
            || assert!(use_http("OTEL_EXPORTER_OTLP_TRACES_PROTOCOL")),
        );
        temp_env::with_vars(
            [
                ("OTEL_EXPORTER_OTLP_PROTOCOL", Some("http/protobuf")),
                ("OTEL_EXPORTER_OTLP_TRACES_PROTOCOL", Some("grpc")),
            ],
            || {
                assert!(
                    !use_http("OTEL_EXPORTER_OTLP_TRACES_PROTOCOL"),
                    "signal var wins"
                )
            },
        );
        temp_env::with_vars(
            [
                ("OTEL_EXPORTER_OTLP_PROTOCOL", None::<&str>),
                ("OTEL_EXPORTER_OTLP_TRACES_PROTOCOL", None),
            ],
            || {
                assert!(
                    !use_http("OTEL_EXPORTER_OTLP_TRACES_PROTOCOL"),
                    "grpc default"
                )
            },
        );
    }
}
