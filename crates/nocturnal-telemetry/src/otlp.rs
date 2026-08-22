//! OTLP wiring: traces, logs, and metrics exporters plus the tracing
//! subscriber stack. With no endpoint configured (and none in the standard
//! `OTEL_EXPORTER_OTLP_ENDPOINT` env), only local fmt logging is installed —
//! the bot runs identically, just unexported.

use anyhow::Context as _;
use opentelemetry::global;
use opentelemetry::metrics::{Counter, Gauge, Histogram};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_sdk::logs::SdkLoggerProvider;
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
    /// Explicit OTLP endpoint; `None` falls back to the standard
    /// `OTEL_EXPORTER_OTLP_ENDPOINT` env var; neither = export disabled.
    pub endpoint: Option<String>,
    /// "grpc" or "http/protobuf".
    pub protocol: String,
    pub service_name: String,
    /// tracing filter directive (e.g. "info").
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

fn export_enabled(cfg: &TelemetryConfig) -> bool {
    cfg.endpoint.is_some() || std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").is_ok()
}

/// Export allowlist (OTel sensitive-data guidance: don't collect what you
/// didn't deliberately author). Only spans/events from our own crates leave
/// the process; library internals stay local — serenity, for one, dumps whole
/// gateway payloads (Identify incl. the bot token, InteractionCreate incl.
/// interaction tokens and member data) into its span fields.
fn export_targets() -> Targets {
    Targets::new()
        // serenity's HTTP request spans are the Discord *client spans* —
        // exported for causality/latency, with attributes stripped to the
        // safe allowlist by [`RedactSpans`] (their raw fields dump whole
        // requests, incl. auth — the B13 incident).
        .with_target("serenity::http", LevelFilter::DEBUG)
        .with_target("nocturnal", LevelFilter::TRACE)
        .with_target("nocturnal_core", LevelFilter::TRACE)
        .with_target("nocturnal_store", LevelFilter::TRACE)
        .with_target("nocturnal_telemetry", LevelFilter::TRACE)
        .with_target("nocturnal_discord", LevelFilter::TRACE)
        .with_target("nocturnal_provision", LevelFilter::TRACE)
        .with_target("nocturnal_migrate", LevelFilter::TRACE)
}

/// Attribute allowlist for exported spans (OTel redaction guidance: fail
/// closed). Only keys we deliberately author — or harmless runtime metadata —
/// survive; everything else (library field dumps like serenity's `req`)
/// is deleted before export.
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

/// Span processor decorator that applies [`safe_attribute`] to every span
/// before handing it to the wrapped (batch) processor.
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

    /// MUST delegate: the SDK hands the Resource (service.name and friends)
    /// to processors this way. Without it the wrapped exporter ships spans
    /// with an empty resource and they land under "missing-service-name".
    fn set_resource(&mut self, resource: &Resource) {
        self.0.set_resource(resource);
    }
}

/// Parse `OTEL_EXPORTER_OTLP_HEADERS` ("k=v,k=v") ourselves so bearer auth
/// works regardless of which env vars the exporter crate honors.
fn env_headers() -> std::collections::HashMap<String, String> {
    std::env::var("OTEL_EXPORTER_OTLP_HEADERS")
        .ok()
        .map(|s| {
            s.split(',')
                .filter_map(|kv| {
                    kv.split_once('=')
                        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
                })
                .collect()
        })
        .unwrap_or_default()
}

// For http/protobuf the Rust exporter treats a code-set endpoint as the FULL
// per-signal URL, so the signal path must be appended explicitly.
macro_rules! exporter {
    ($builder:expr, $cfg:expr, $path:expr) => {{
        if $cfg.protocol == "http/protobuf" {
            let mut b = opentelemetry_otlp::WithHttpConfig::with_headers(
                $builder.with_http(),
                env_headers(),
            );
            if let Some(e) = &$cfg.endpoint {
                b = opentelemetry_otlp::WithExportConfig::with_endpoint(
                    b,
                    format!("{}{}", e.trim_end_matches('/'), $path),
                );
            }
            b.build()
        } else {
            let b = $builder.with_tonic();
            match &$cfg.endpoint {
                Some(e) => {
                    opentelemetry_otlp::WithExportConfig::with_endpoint(b, e.clone()).build()
                }
                None => b.build(),
            }
        }
    }};
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

    if !export_enabled(cfg) {
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

    let resource = Resource::builder()
        .with_service_name(cfg.service_name.clone())
        .build();

    let span_exporter = exporter!(
        opentelemetry_otlp::SpanExporter::builder(),
        cfg,
        "/v1/traces"
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

    let log_exporter = exporter!(opentelemetry_otlp::LogExporter::builder(), cfg, "/v1/logs")
        .context("building OTLP log exporter")?;
    let logger_provider = SdkLoggerProvider::builder()
        .with_batch_exporter(log_exporter)
        .with_resource(resource.clone())
        .build();
    let otel_log_layer =
        OpenTelemetryTracingBridge::new(&logger_provider).with_filter(export_targets());

    let metric_exporter = exporter!(
        opentelemetry_otlp::MetricExporter::builder(),
        cfg,
        "/v1/metrics"
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
        endpoint = cfg
            .endpoint
            .as_deref()
            .unwrap_or("(from OTEL_EXPORTER_OTLP_ENDPOINT)"),
        protocol = cfg.protocol,
        "OTLP export enabled"
    );

    Ok(TelemetryGuard {
        tracer: Some(tracer_provider),
        logger: Some(logger_provider),
        meter: Some(meter_provider),
    })
}

/// The ledger's metric instruments, names straight from the registry. With no
/// exporter installed these are no-ops — always safe to record.
pub struct Metrics {
    pub commands: Counter<u64>,
    pub commit_duration: Histogram<f64>,
    pub ledger_events: Counter<u64>,
    pub ledger_seq: Gauge<u64>,
    pub wal_fsync_duration: Histogram<f64>,
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
    use super::export_targets;
    use tracing::Level;

    /// The redaction wrapper must delegate *every* hook — a missed
    /// `set_resource` silently strips service.name from exported spans.
    #[test]
    fn redact_wrapper_forwards_set_resource() {
        use super::RedactSpans;
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
        use super::safe_attribute;
        for ok in [
            "nocturnal.command",
            "code.line.number",
            "http.status_code",
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
            "serenity::http::client", // Discord client spans, redacted
        ] {
            assert!(
                targets.would_enable(ours, &Level::DEBUG),
                "{ours} must export"
            );
        }
        for theirs in [
            "serenity::gateway::shard",
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
}
