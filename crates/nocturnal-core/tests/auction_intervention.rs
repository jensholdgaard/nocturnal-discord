//! What the ledger does when an officer steps into a running auction.
//!
//! `/endauction` and `/cancelauction` are the two ways a live auction stops
//! being the scheduler's business. Both are decided here rather than in the
//! Discord layer, so both hold for a button, a command, or anything added
//! later — and both have to survive a replay, because the answer to "who
//! pulled that auction and when" is read off the projection months later.

#![allow(clippy::unwrap_used)] // tests assert; unwrap is the assertion

use nocturnal_core::event::{Flavor, Item};
use nocturnal_core::state::AuctionStatus;
use nocturnal_core::{Actor, Command, Ctx, Ledger, Rejection};

const GUILD: u64 = 42;
const OFFICER: u64 = 7;
const BIDDER: u64 = 9;
const DEADLINE: i64 = 2_000_000;

fn ctx_at(now_ms: i64, actor: Actor) -> Ctx {
    Ctx {
        guild: GUILD,
        actor,
        now_ms,
    }
}

fn exec(l: &mut Ledger, ctx: &Ctx, cmd: Command) -> Result<(), Rejection> {
    let envelopes = l.propose(ctx, &cmd)?;
    l.commit(&envelopes);
    Ok(())
}

/// An open long auction with one bid from a player who can afford it.
fn running_auction() -> Ledger {
    let mut l = Ledger::new();
    let ctx = ctx_at(1_000_000, Actor::System);
    exec(
        &mut l,
        &ctx,
        Command::ImportPlayer {
            player: BIDDER,
            balance: 500,
            characters: vec![],
            creation_ts_ms: 1,
            log: vec![],
            legacy_id: None,
        },
    )
    .unwrap();
    exec(
        &mut l,
        &ctx,
        Command::OpenAuction {
            auction_id: "au-1".into(),
            item: Item {
                id: "1".into(),
                name: "Cloak".into(),
                url: None,
                data: None,
                image: None,
            },
            flavor: Flavor::Long,
            min_bid: 0,
            num_items: 1,
            min_bid_to_lock_for_main: 0,
            over_bid_to_win_main: 0,
            duration_ms: DEADLINE - 1_000_000,
        },
    )
    .unwrap();
    exec(
        &mut l,
        &ctx,
        Command::PlaceBid {
            auction_id: "au-1".into(),
            player: BIDDER,
            amount: 100,
            for_main: true,
            character: None,
        },
    )
    .unwrap();
    l
}

/// The whole point of rewriting the deadline: the recap has to name the
/// moment bidding actually stopped, not the time on the invitation.
#[test]
fn an_early_close_moves_the_deadline_to_the_moment_it_happened() {
    let mut l = running_auction();
    let stopped_at = 1_500_000;
    exec(
        &mut l,
        &ctx_at(stopped_at, Actor::User(OFFICER)),
        Command::CloseAuction {
            auction_id: "au-1".into(),
            ended_ts_ms: Some(stopped_at),
        },
    )
    .unwrap();

    let a = &l.state().guild(GUILD).unwrap().auctions["au-1"];
    assert_eq!(a.status, AuctionStatus::Closed);
    assert_eq!(
        a.deadline_ts_ms, stopped_at,
        "the deadline is when it stopped, not when it was going to"
    );
}

/// The scheduler's own close carries no timestamp, and must leave the
/// deadline exactly where it was — every embed and the finalize-due
/// calculation are derived from it.
#[test]
fn the_schedulers_close_leaves_the_deadline_alone() {
    let mut l = running_auction();
    exec(
        &mut l,
        &ctx_at(DEADLINE, Actor::System),
        Command::CloseAuction {
            auction_id: "au-1".into(),
            ended_ts_ms: None,
        },
    )
    .unwrap();
    assert_eq!(
        l.state().guild(GUILD).unwrap().auctions["au-1"].deadline_ts_ms,
        DEADLINE
    );
}

/// Bidding stops the instant the close lands — that is what makes
/// `/endauction` safe to run while people are still typing.
#[test]
fn a_closed_auction_takes_no_further_bids() {
    let mut l = running_auction();
    exec(
        &mut l,
        &ctx_at(1_500_000, Actor::User(OFFICER)),
        Command::CloseAuction {
            auction_id: "au-1".into(),
            ended_ts_ms: Some(1_500_000),
        },
    )
    .unwrap();

    for cmd in [
        Command::PlaceBid {
            auction_id: "au-1".into(),
            player: BIDDER,
            amount: 200,
            for_main: true,
            character: None,
        },
        Command::RetractBid {
            auction_id: "au-1".into(),
            player: BIDDER,
        },
        Command::CloseAuction {
            auction_id: "au-1".into(),
            ended_ts_ms: None,
        },
    ] {
        let err = exec(&mut l, &ctx_at(1_600_000, Actor::User(BIDDER)), cmd).unwrap_err();
        assert!(matches!(err, Rejection::AuctionNotActive), "got {err:?}");
    }
    let a = &l.state().guild(GUILD).unwrap().auctions["au-1"];
    assert_eq!(a.bids.len(), 1, "the standing bid is untouched");
    assert_eq!(a.bids[0].amount, 100);
}

/// "Who pulled that auction?" is asked long after the fact, so the answer
/// comes off the projection — taken from the envelope, which every event
/// already carries.
#[test]
fn a_cancelled_auction_remembers_who_and_when() {
    let mut l = running_auction();
    let when = 1_400_000;
    exec(
        &mut l,
        &ctx_at(when, Actor::User(OFFICER)),
        Command::CancelAuction {
            auction_id: "au-1".into(),
            reason: "officer".into(),
        },
    )
    .unwrap();

    let a = &l.state().guild(GUILD).unwrap().auctions["au-1"];
    assert_eq!(a.status, AuctionStatus::Cancelled);
    assert_eq!(a.cancelled_by, Some(OFFICER));
    assert_eq!(a.cancelled_ts_ms, Some(when));
    assert_eq!(
        a.bids.len(),
        1,
        "the bids survive — /auctiondetails reads them back"
    );
}

/// Cancelling is allowed right up until the DKP moves, and refused after.
/// In this ledger that boundary is finalize, not close: closing an auction
/// moves nothing, so one awaiting its confirmation is still safely voidable.
#[test]
fn cancelling_is_refused_only_once_the_dkp_has_moved() {
    let mut l = running_auction();
    exec(
        &mut l,
        &ctx_at(DEADLINE, Actor::System),
        Command::CloseAuction {
            auction_id: "au-1".into(),
            ended_ts_ms: None,
        },
    )
    .unwrap();
    // Closed but not paid out: still voidable.
    let mut voidable = l.clone();
    exec(
        &mut voidable,
        &ctx_at(DEADLINE + 1, Actor::User(OFFICER)),
        Command::CancelAuction {
            auction_id: "au-1".into(),
            reason: "officer".into(),
        },
    )
    .unwrap();
    assert_eq!(
        voidable.state().guild(GUILD).unwrap().auctions["au-1"].status,
        AuctionStatus::Cancelled
    );

    exec(
        &mut l,
        &ctx_at(DEADLINE + 1, Actor::User(OFFICER)),
        Command::FinalizeAuction {
            auction_id: "au-1".into(),
            seed: 1,
        },
    )
    .unwrap();
    let err = exec(
        &mut l,
        &ctx_at(DEADLINE + 2, Actor::User(OFFICER)),
        Command::CancelAuction {
            auction_id: "au-1".into(),
            reason: "too late".into(),
        },
    )
    .unwrap_err();
    assert!(matches!(err, Rejection::AuctionNotActive), "got {err:?}");
    assert_eq!(
        l.state().guild(GUILD).unwrap().balance(BIDDER),
        400,
        "the winner stayed debited"
    );
}

/// `/endauction` is close-then-finalize, and the winner is settled against
/// balances as they are at that moment — the same path the scheduler takes.
#[test]
fn ending_early_settles_exactly_as_the_scheduler_would() {
    let mut l = running_auction();
    let now = 1_500_000;
    exec(
        &mut l,
        &ctx_at(now, Actor::User(OFFICER)),
        Command::CloseAuction {
            auction_id: "au-1".into(),
            ended_ts_ms: Some(now),
        },
    )
    .unwrap();
    exec(
        &mut l,
        &ctx_at(now, Actor::User(OFFICER)),
        Command::FinalizeAuction {
            auction_id: "au-1".into(),
            seed: now as u64,
        },
    )
    .unwrap();

    let g = l.state().guild(GUILD).unwrap();
    let a = &g.auctions["au-1"];
    assert_eq!(a.status, AuctionStatus::Finalized);
    assert_eq!(a.winners.len(), 1);
    assert_eq!(a.winners[0].player, BIDDER);
    assert_eq!(g.balance(BIDDER), 400, "debited at finalize");
}

/// Everything above is projection state, so it has to come back the same way
/// after a restart — including the cancel attribution, which is read off the
/// envelope rather than the payload.
#[test]
fn an_intervention_replays_identically() {
    let mut live = running_auction();
    let mut log = Vec::new();
    for (now, cmd) in [
        (
            1_500_000,
            Command::CloseAuction {
                auction_id: "au-1".into(),
                ended_ts_ms: Some(1_500_000),
            },
        ),
        (
            1_500_001,
            Command::CancelAuction {
                auction_id: "au-1".into(),
                reason: "officer".into(),
            },
        ),
    ] {
        let ctx = ctx_at(now, Actor::User(OFFICER));
        let envelopes = live.propose(&ctx, &cmd).unwrap();
        live.commit(&envelopes);
        log.extend(envelopes);
    }

    let mut replayed = running_auction();
    for env in &log {
        replayed.replay(env);
    }
    assert_eq!(
        live.state().guild(GUILD),
        replayed.state().guild(GUILD),
        "replay diverged"
    );
    let a = &replayed.state().guild(GUILD).unwrap().auctions["au-1"];
    assert_eq!(a.cancelled_by, Some(OFFICER));
    assert_eq!(a.deadline_ts_ms, 1_500_000);
}
