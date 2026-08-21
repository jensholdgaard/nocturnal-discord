//! The single writer. One dedicated OS thread owns the `Ledger` and the
//! `Store`; everything else talks to it through a channel. Writes follow
//! decide → append (fsync) → apply, strictly one at a time — the property
//! that makes the audit's whole race-condition track unrepresentable.
//! Reads are closures executed on the same thread: always consistent,
//! never locking.

use anyhow::Context as _;
use nocturnal_core::{Actor, Command, Envelope, GuildId, Ledger, Rejection};
use nocturnal_store::Store;
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
    // Wired up by the raid/auction commands in M4; until then only queries flow.
    #[allow(dead_code)]
    Execute {
        guild: GuildId,
        actor: Actor,
        cmd: Box<Command>,
        reply: oneshot::Sender<Result<Vec<Envelope>, ExecError>>,
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
    #[allow(dead_code)] // first writer commands land in M4
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
pub fn start(data_dir: &std::path::Path) -> anyhow::Result<(DriverHandle, usize)> {
    let t0 = std::time::Instant::now();
    let (mut store, envelopes) = Store::open(data_dir)
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
            while let Some(req) = rx.blocking_recv() {
                match req {
                    Request::Query(f) => f(&ledger),
                    Request::Execute { guild, actor, cmd, reply } => {
                        let ctx = nocturnal_core::Ctx { guild, actor, now_ms: now_ms() };
                        let cmd = *cmd;
                        let result = match ledger.propose(&ctx, &cmd) {

                            Err(rej) => Err(ExecError::Rejected(rej)),
                            Ok(envelopes) => match store.append(&envelopes) {
                                Ok(()) => {
                                    ledger.commit(&envelopes);
                                    Ok(envelopes)
                                }
                                Err(e) => {
                                    tracing::error!(error = %e, "WAL append failed; command dropped");
                                    Err(ExecError::Storage(e.to_string()))
                                }
                            },
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
