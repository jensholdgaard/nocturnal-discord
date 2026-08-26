//! The ledger: commands, events, decide/apply folds, projections.
//!
//! Pure — no I/O, no tokio, no Discord, no clock, no ambient randomness.
//! Every rule of the DKP economy lives here and nowhere else. Behaviour is
//! specified by `docs/commands.md`; the legacy bot's jest fixtures are ported
//! into this crate's tests.

pub mod apply;
pub mod auction;
pub mod command;
pub mod decide;
pub mod event;
pub mod reject;
pub mod state;
pub mod who;

pub use apply::apply;
pub use command::{Command, Ctx};
pub use decide::{compute_winners, decide};
pub use event::{Actor, Envelope, Event, Flavor, GuildId, Item, PlayerId, Secret};
pub use reject::Rejection;
pub use state::State;

/// The single-writer ledger: state plus the next sequence number.
///
/// The driver loop uses it in three strict phases per command:
/// 1. [`Ledger::propose`] — pure decide, envelopes staged (nothing mutated),
/// 2. persist the envelopes durably (WAL fsync),
/// 3. [`Ledger::commit`] — fold them in and advance the sequence.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Ledger {
    state: State,
    next_seq: u64,
}

impl Ledger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Decide and stage: returns fully-formed envelopes without mutating
    /// anything. Call [`Ledger::commit`] with them after they are durable.
    pub fn propose(&self, ctx: &Ctx, cmd: &Command) -> Result<Vec<Envelope>, Rejection> {
        let events = decide(&self.state, ctx, cmd)?;
        Ok(events
            .into_iter()
            .enumerate()
            .map(|(i, event)| Envelope {
                seq: self.next_seq + i as u64,
                ts_ms: ctx.now_ms,
                guild: ctx.guild,
                actor: ctx.actor,
                v: 1,
                correlation_id: None,
                event,
            })
            .collect())
    }

    /// Fold staged (now durable) envelopes into state.
    ///
    /// # Panics
    /// Panics on a sequence gap — that is a driver bug, never valid data.
    pub fn commit(&mut self, envelopes: &[Envelope]) {
        for env in envelopes {
            assert_eq!(env.seq, self.next_seq, "sequence gap: driver bug");
            apply(&mut self.state, env);
            self.next_seq += 1;
        }
    }

    /// Propose + commit in one step (tests and non-durable use).
    pub fn execute(&mut self, ctx: &Ctx, cmd: &Command) -> Result<Vec<Envelope>, Rejection> {
        let envelopes = self.propose(ctx, cmd)?;
        self.commit(&envelopes);
        Ok(envelopes)
    }

    /// Rebuild from a persisted event (boot-time replay).
    pub fn replay(&mut self, env: &Envelope) {
        assert_eq!(env.seq, self.next_seq, "replay out of order: corrupt log");
        apply(&mut self.state, env);
        self.next_seq += 1;
    }
}
