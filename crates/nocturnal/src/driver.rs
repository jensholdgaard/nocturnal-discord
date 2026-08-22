//! The single writer. One dedicated OS thread owns the `Ledger` and the
//! `Store`; everything else talks to it through a channel. Writes follow
//! decide → append (fsync) → apply, strictly one at a time — the property
//! that makes the audit's whole race-condition track unrepresentable.
//! Reads are closures executed on the same thread: always consistent,
//! never locking.

use anyhow::Context as _;
use nocturnal_core::{Actor, Command, Envelope, GuildId, Ledger, Rejection};
use nocturnal_store::Store;
use nocturnal_telemetry::{attr, Metrics};
use tokio::sync::{mpsc, oneshot};

#[derive(Debug)]
pub enum ExecError {
    Rejected(Rejection),
    /// WAL/storage failure — the command did NOT happen.
    Storage(String),
}

impl std::fmt::Display for ExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecError::Rejected(r) => write!(f, "rejected: {r}"),
            ExecError::Storage(e) => write!(f, "storage: {e}"),
        }
    }
}

enum Request {
    Execute {
        guild: GuildId,
        actor: Actor,
        cmd: Box<Command>,
        reply: oneshot::Sender<Result<Vec<Envelope>, ExecError>>,
        /// In-process trace context: the caller's span, so the ledger work
        /// done on the writer thread nests under the interaction that caused
        /// it instead of starting its own orphan trace. (OTel guidance:
        /// propagate context explicitly across threads you start yourself.)
        parent: tracing::Span,
    },
    Query(Box<dyn FnOnce(&Ledger) + Send>),
}

#[derive(Clone)]
pub struct DriverHandle {
    tx: mpsc::Sender<Request>,
}

impl DriverHandle {
    /// Execute a command through the single writer. `Ok` means the events are
    /// fsynced and applied.
    pub async fn execute(
        &self,
        guild: GuildId,
        actor: Actor,
        cmd: Command,
    ) -> Result<Vec<Envelope>, ExecError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Request::Execute {
                guild,
                actor,
                cmd: Box::new(cmd),
                reply,
                parent: tracing::Span::current(),
            })
            .await
            .map_err(|_| ExecError::Storage("driver gone".into()))?;
        rx.await
            .map_err(|_| ExecError::Storage("driver gone".into()))?
    }

    /// Run a read against the live projections on the writer thread.
    pub async fn query<R, F>(&self, f: F) -> R
    where
        R: Send + 'static,
        F: FnOnce(&Ledger) -> R + Send + 'static,
    {
        let (reply, rx) = oneshot::channel::<R>();
        let req = Request::Query(Box::new(move |ledger| {
            let _ = reply.send(f(ledger));
        }));
        // Both failure modes mean the driver thread is dead; nothing sane to
        // do for a read but propagate the panic.
        #[allow(clippy::expect_used)]
        {
            self.tx.send(req).await.expect("driver alive");
            rx.await.expect("driver alive")
        }
    }
}

fn now_ms() -> i64 {
    #[allow(clippy::expect_used)]
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after 1970");
    d.as_millis() as i64
}

/// Boot: lock is already held; open the store, replay, start the writer
/// thread. Returns the handle and the replayed event count.
/// Boot without an archive (tests, and any deployment that keeps history
/// only on local disk).
#[cfg(test)]
pub fn start(data_dir: &std::path::Path) -> anyhow::Result<(DriverHandle, usize)> {
    start_with_archive(data_dir, None)
}

/// Boot with an off-site archive: Parquet partitions the local disk lacks are
/// restored from object storage before replay.
pub fn start_with_archive(
    data_dir: &std::path::Path,
    archive: Option<nocturnal_store::Archive>,
) -> anyhow::Result<(DriverHandle, usize)> {
    let t0 = std::time::Instant::now();
    let (mut store, envelopes) = Store::open_with_archive(data_dir, archive)
        .with_context(|| format!("opening store in {}", data_dir.display()))?;
    let mut ledger = Ledger::new();
    for env in &envelopes {
        ledger.replay(env);
    }
    let replayed = envelopes.len();
    tracing::info!(events = replayed, elapsed = ?t0.elapsed(), "ledger replayed");

    let (tx, mut rx) = mpsc::channel::<Request>(256);
    std::thread::Builder::new()
        .name("ledger-writer".into())
        .spawn(move || {
            let metrics = Metrics::new();
            // Seed the gauge so dashboards show the ledger head from boot,
            // not only after the first write.
            metrics.ledger_seq.record(ledger.next_seq(), &[]);
            while let Some(req) = rx.blocking_recv() {
                match req {
                    Request::Query(f) => f(&ledger),
                    Request::Execute { guild, actor, cmd, reply, parent } => {
                        let started = std::time::Instant::now();
                        let ctx = nocturnal_core::Ctx { guild, actor, now_ms: now_ms() };
                        let cmd = *cmd;
                        let command_kind = cmd.kind();
                        // Child of the interaction span (context propagated
                        // across the channel), INTERNAL: no process boundary.
                        let span = tracing::info_span!(
                            parent: &parent,
                            "ledger.execute",
                            otel.kind = "internal",
                            otel.status_code = tracing::field::Empty,
                            { attr::NOCTURNAL_GUILD_ID } = %guild,
                            { attr::NOCTURNAL_COMMAND } = command_kind,
                            { attr::NOCTURNAL_DECISION_OUTCOME } = tracing::field::Empty,
                            { attr::NOCTURNAL_EVENT_SEQ } = tracing::field::Empty,
                        );
                        let _entered = span.enter();

                        // decide → append(fsync) → apply, each its own span so
                        // a slow command says *which phase* was slow.
                        let decided = tracing::info_span!("ledger.decide", otel.kind = "internal")
                            .in_scope(|| ledger.propose(&ctx, &cmd));

                        let result = match decided {
                            Err(rej) => {
                                span.record(attr::NOCTURNAL_DECISION_OUTCOME, "rejected");
                                span.record("otel.status_code", "OK"); // a refusal is a valid outcome
                                metrics.record_command(
                                    command_kind,
                                    "rejected",
                                    Some(rej.slug()),
                                    started.elapsed().as_secs_f64(),
                                );
                                Err(ExecError::Rejected(rej))
                            }
                            Ok(envelopes) => {
                                let fsync_started = std::time::Instant::now();
                                let append_span = tracing::info_span!(
                                    "wal.append",
                                    otel.kind = "internal",
                                    events = envelopes.len(),
                                );
                                let appended = append_span.in_scope(|| store.append(&envelopes));
                                match appended {
                                    Ok(()) => {
                                        metrics
                                            .wal_fsync_duration
                                            .record(fsync_started.elapsed().as_secs_f64(), &[]);
                                        tracing::info_span!("ledger.apply", otel.kind = "internal")
                                            .in_scope(|| ledger.commit(&envelopes));
                                        span.record(attr::NOCTURNAL_DECISION_OUTCOME, "accepted");
                                        span.record("otel.status_code", "OK");
                                        if let Some(last) = envelopes.last() {
                                            span.record(attr::NOCTURNAL_EVENT_SEQ, last.seq);
                                        }
                                        for env in &envelopes {
                                            metrics.ledger_events.add(
                                                1,
                                                &[opentelemetry::KeyValue::new(
                                                    attr::NOCTURNAL_EVENT_KIND,
                                                    env.event.kind(),
                                                )],
                                            );
                                        }
                                        metrics.ledger_seq.record(ledger.next_seq(), &[]);
                                        metrics.record_command(
                                            command_kind,
                                            "accepted",
                                            None,
                                            started.elapsed().as_secs_f64(),
                                        );
                                        Ok(envelopes)
                                    }
                                    Err(e) => {
                                        span.record(attr::NOCTURNAL_DECISION_OUTCOME, "error");
                                        span.record("otel.status_code", "ERROR");
                                        tracing::error!(error = %e, "WAL append failed; command dropped");
                                        metrics.record_command(
                                            command_kind,
                                            "error",
                                            None,
                                            started.elapsed().as_secs_f64(),
                                        );
                                        Err(ExecError::Storage(e.to_string()))
                                    }
                                }
                            }
                        };
                        let _ = reply.send(result);
                    }
                }
            }
            tracing::info!("ledger writer stopped");
        })
        .context("spawning ledger-writer thread")?;

    Ok((DriverHandle { tx }, replayed))
}
