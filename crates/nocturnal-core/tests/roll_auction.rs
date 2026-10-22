//! `/rollauction` (2026-09-30): a long auction won by a `/random 100`
//! instead of a bid. Each member rolls once, the rolls are public as they
//! land, the highest roll wins at the deadline, a tie across the cut is
//! rolled off, and no DKP moves at any point.
//!
//! Every roll is drawn from a seed the command carries, so the tests pick
//! the seed that lands the roll they need rather than hoping for one.

#![allow(clippy::unwrap_used)] // tests assert; unwrap is the assertion

use nocturnal_core::auction::{roll_winners, Rng};
use nocturnal_core::event::{Flavor, Item, Roll};
use nocturnal_core::state::{Auction, AuctionStatus};
use nocturnal_core::{Actor, Command, Ctx, Envelope, Event, Ledger, PlayerId, Rejection};

const GUILD: u64 = 42;
const OFFICER: u64 = 7;
const ANNA: u64 = 101;
const BORIS: u64 = 102;
const CLEO: u64 = 103;
const STRANGER: u64 = 999;
const OPENED: i64 = 1_000_000;
const DEADLINE: i64 = 2_000_000;

fn ctx_at(now_ms: i64, actor: Actor) -> Ctx {
    Ctx {
        guild: GUILD,
        actor,
        now_ms,
    }
}

fn exec(l: &mut Ledger, ctx: &Ctx, cmd: Command) -> Result<Vec<Envelope>, Rejection> {
    let envelopes = l.propose(ctx, &cmd)?;
    l.commit(&envelopes);
    Ok(envelopes)
}

/// The first seed whose d100 is `roll`.
fn seed_for(roll: u32) -> u64 {
    (0u64..).find(|s| Rng::new(*s).d100() == roll).unwrap()
}

fn cloak() -> Item {
    Item {
        id: "1".into(),
        name: "Cloak".into(),
        url: None,
        data: None,
        image: None,
    }
}

/// Three registered members with DKP to spare, and a roll auction opened
/// with bid knobs and a debit it must ignore.
fn roll_auction(num_items: u32) -> Ledger {
    roll_auction_costing(num_items, 0)
}

/// [`roll_auction`] with a price on winning (2026-10-22).
fn roll_auction_costing(num_items: u32, roll_cost: i64) -> Ledger {
    let mut l = Ledger::new();
    let ctx = ctx_at(OPENED, Actor::System);
    for player in [ANNA, BORIS, CLEO] {
        exec(
            &mut l,
            &ctx,
            Command::ImportPlayer {
                player,
                balance: 500,
                characters: vec![],
                creation_ts_ms: 1,
                log: vec![],
                legacy_id: None,
            },
        )
        .unwrap();
    }
    exec(
        &mut l,
        &ctx_at(OPENED, Actor::User(OFFICER)),
        Command::OpenAuction {
            auction_id: "au-1".into(),
            item: cloak(),
            flavor: Flavor::Roll,
            min_bid: 50,
            num_items,
            min_bid_to_lock_for_main: 10,
            over_bid_to_win_main: 100,
            duration_ms: DEADLINE - OPENED,
            debit_dkp: true,
            live: false,
            voice_channel: None,
            roll_cost,
            rollers: None,
        },
    )
    .unwrap();
    l
}

fn roll(l: &mut Ledger, player: PlayerId, roll: u32) -> Result<Vec<Envelope>, Rejection> {
    exec(
        l,
        &ctx_at(OPENED + 1, Actor::User(player)),
        Command::RollForAuction {
            auction_id: "au-1".into(),
            player,
            seed: seed_for(roll),
            voice_channel: None,
        },
    )
}

/// The scheduler's two steps at the deadline; returns the draw's events.
fn close_and_draw(l: &mut Ledger) -> Vec<Envelope> {
    let ctx = ctx_at(DEADLINE, Actor::System);
    exec(
        l,
        &ctx,
        Command::CloseAuction {
            auction_id: "au-1".into(),
            ended_ts_ms: None,
        },
    )
    .unwrap();
    exec(
        l,
        &ctx,
        Command::FinalizeAuction {
            auction_id: "au-1".into(),
        },
    )
    .unwrap()
}

fn auction(l: &Ledger) -> &Auction {
    &l.state().guild(GUILD).unwrap().auctions["au-1"]
}

#[test]
fn a_roll_auction_records_no_minimum_bid_and_no_debit() {
    let l = roll_auction(1);
    let a = auction(&l);
    assert_eq!(a.flavor, Flavor::Roll);
    assert_eq!(a.status, AuctionStatus::Open);
    assert_eq!(a.min_bid, 0);
    assert_eq!(a.min_bid_to_lock_for_main, 0);
    assert_eq!(a.over_bid_to_win_main, 0);
    assert!(
        !a.debit_dkp,
        "a free roll never debits, whatever it was asked"
    );
    assert_eq!(a.deadline_ts_ms, DEADLINE);
}

#[test]
fn a_roll_is_one_d100_recorded_with_its_seed() {
    let mut l = roll_auction(1);
    let seed = 0xDEAD_BEEF;
    let envelopes = exec(
        &mut l,
        &ctx_at(OPENED + 1, Actor::User(ANNA)),
        Command::RollForAuction {
            auction_id: "au-1".into(),
            player: ANNA,
            seed,
            voice_channel: None,
        },
    )
    .unwrap();
    assert_eq!(envelopes.len(), 1);
    let expected = Rng::new(seed).d100();
    match &envelopes[0].event {
        Event::AuctionRolled {
            player,
            roll,
            seed: recorded,
            ..
        } => {
            assert_eq!(*player, ANNA);
            assert_eq!(*roll, expected, "the roll is the seed's d100");
            assert_eq!(*recorded, seed, "and the seed is on record");
        }
        other => panic!("wrong event: {other:?}"),
    }
    assert_eq!(
        auction(&l).rolls,
        vec![Roll {
            player: ANNA,
            roll: expected
        }]
    );
}

#[test]
fn a_second_roll_is_refused_and_names_the_first() {
    let mut l = roll_auction(1);
    roll(&mut l, ANNA, 12).unwrap();
    let err = roll(&mut l, ANNA, 99).unwrap_err();
    assert_eq!(err, Rejection::AlreadyRolled { roll: 12 });
    assert_eq!(auction(&l).rolls.len(), 1, "the first roll stands");
}

#[test]
fn a_bid_on_a_roll_auction_is_refused() {
    let mut l = roll_auction(1);
    let err = exec(
        &mut l,
        &ctx_at(OPENED + 1, Actor::User(ANNA)),
        Command::PlaceBid {
            auction_id: "au-1".into(),
            player: ANNA,
            amount: 100,
            for_main: true,
            character: None,
        },
    )
    .unwrap_err();
    assert_eq!(err, Rejection::WrongAuctionFlavor);
    assert!(auction(&l).bids.is_empty());
}

#[test]
fn a_roll_on_a_bidding_auction_is_refused() {
    let mut l = roll_auction(1);
    exec(
        &mut l,
        &ctx_at(OPENED, Actor::User(OFFICER)),
        Command::OpenAuction {
            auction_id: "au-2".into(),
            item: cloak(),
            flavor: Flavor::Long,
            min_bid: 0,
            num_items: 1,
            min_bid_to_lock_for_main: 0,
            over_bid_to_win_main: 0,
            duration_ms: DEADLINE - OPENED,
            debit_dkp: true,
            live: false,
            voice_channel: None,
            roll_cost: 0,
            rollers: None,
        },
    )
    .unwrap();
    let err = exec(
        &mut l,
        &ctx_at(OPENED + 1, Actor::User(ANNA)),
        Command::RollForAuction {
            auction_id: "au-2".into(),
            player: ANNA,
            seed: 1,
            voice_channel: None,
        },
    )
    .unwrap_err();
    assert_eq!(err, Rejection::WrongAuctionFlavor);
}

#[test]
fn an_unknown_member_cannot_roll() {
    let mut l = roll_auction(1);
    let err = roll(&mut l, STRANGER, 50).unwrap_err();
    assert_eq!(err, Rejection::PlayerNotFound);
    assert!(auction(&l).rolls.is_empty());
}

#[test]
fn rolling_stops_at_the_close() {
    let mut l = roll_auction(1);
    exec(
        &mut l,
        &ctx_at(1_500_000, Actor::User(OFFICER)),
        Command::CloseAuction {
            auction_id: "au-1".into(),
            ended_ts_ms: Some(1_500_000),
        },
    )
    .unwrap();
    let err = roll(&mut l, ANNA, 50).unwrap_err();
    assert_eq!(err, Rejection::AuctionNotActive);
}

#[test]
fn the_highest_roll_wins_and_nobody_is_charged() {
    let mut l = roll_auction(1);
    roll(&mut l, ANNA, 42).unwrap();
    roll(&mut l, BORIS, 90).unwrap();
    roll(&mut l, CLEO, 7).unwrap();

    let drawn = close_and_draw(&mut l);
    assert_eq!(drawn.len(), 1, "no tie, so no roll-off: {drawn:?}");
    assert!(matches!(drawn[0].event, Event::AuctionFinalized { .. }));

    let g = l.state().guild(GUILD).unwrap();
    let a = &g.auctions["au-1"];
    assert_eq!(a.status, AuctionStatus::Finalized);
    assert_eq!(a.winners.len(), 1);
    assert_eq!(a.winners[0].player, BORIS);
    assert_eq!(a.winners[0].amount, 0);
    assert!(a.roll_offs.is_empty());
    for player in [ANNA, BORIS, CLEO] {
        assert_eq!(g.balance(player), 500, "no DKP moves on a roll auction");
    }
    let last = g.players[&BORIS].log.last().unwrap();
    assert_eq!(last.dkp, 0);
    assert_eq!(last.comment, "Cloak (roll auction: rolled 90)");
    assert!(last.item.is_some(), "the loot is still on record");
}

#[test]
fn a_tie_at_the_top_is_rolled_off_between_the_tied_only() {
    let mut l = roll_auction(1);
    roll(&mut l, ANNA, 90).unwrap();
    roll(&mut l, BORIS, 90).unwrap();
    roll(&mut l, CLEO, 50).unwrap();

    let drawn = close_and_draw(&mut l);
    assert_eq!(drawn.len(), 2, "{drawn:?}");
    let rounds = match &drawn[0].event {
        Event::AuctionRollOff { rounds, .. } => rounds.clone(),
        other => panic!("the roll-off comes first: {other:?}"),
    };
    assert!(matches!(drawn[1].event, Event::AuctionFinalized { .. }));

    assert!(!rounds.is_empty());
    for round in &rounds {
        assert!(
            round.iter().all(|r| r.player != CLEO),
            "only the tied roll again"
        );
        assert!(round.iter().all(|r| (1..=100).contains(&r.roll)));
    }
    let last = rounds.last().unwrap();
    let top = last.iter().map(|r| r.roll).max().unwrap();
    let leaders: Vec<_> = last.iter().filter(|r| r.roll == top).collect();
    assert_eq!(leaders.len(), 1, "the last round settles it");

    let g = l.state().guild(GUILD).unwrap();
    let a = &g.auctions["au-1"];
    assert_eq!(a.roll_offs, rounds, "the roll-off is on the projection");
    assert_eq!(a.winners.len(), 1);
    assert_eq!(a.winners[0].player, leaders[0].player);
    let comment = &g.players[&leaders[0].player].log.last().unwrap().comment;
    assert_eq!(
        *comment,
        format!("Cloak (roll auction: rolled 90, roll-off {top})")
    );
    for player in [ANNA, BORIS, CLEO] {
        assert_eq!(g.balance(player), 500);
    }
}

#[test]
fn a_tie_inside_the_winners_needs_no_roll_off() {
    let mut l = roll_auction(2);
    roll(&mut l, ANNA, 90).unwrap();
    roll(&mut l, BORIS, 90).unwrap();
    roll(&mut l, CLEO, 50).unwrap();

    let drawn = close_and_draw(&mut l);
    assert_eq!(drawn.len(), 1, "two 90s for two items is no tie: {drawn:?}");
    let mut won: Vec<_> = auction(&l).winners.iter().map(|w| w.player).collect();
    won.sort_unstable();
    assert_eq!(won, vec![ANNA, BORIS]);
}

#[test]
fn nobody_rolled_means_nobody_wins() {
    let mut l = roll_auction(1);
    let drawn = close_and_draw(&mut l);
    assert_eq!(drawn.len(), 1);
    let a = auction(&l);
    assert_eq!(a.status, AuctionStatus::Finalized);
    assert!(a.winners.is_empty());
}

#[test]
fn a_roll_auction_can_still_be_cancelled_before_the_draw() {
    let mut l = roll_auction(1);
    roll(&mut l, ANNA, 90).unwrap();
    exec(
        &mut l,
        &ctx_at(1_500_000, Actor::User(OFFICER)),
        Command::CancelAuction {
            auction_id: "au-1".into(),
            reason: "officer".into(),
        },
    )
    .unwrap();
    let a = auction(&l);
    assert_eq!(a.status, AuctionStatus::Cancelled);
    assert_eq!(a.rolls.len(), 1, "the rolls survive for /auctiondetails");
}

/// The rolls and the roll-off are projection state, so a restart has to
/// rebuild them exactly, down to which roll won.
#[test]
fn a_roll_auction_replays_identically() {
    let mut live = roll_auction(1);
    let mut log = Vec::new();
    log.extend(roll(&mut live, ANNA, 90).unwrap());
    log.extend(roll(&mut live, BORIS, 90).unwrap());
    log.extend(roll(&mut live, CLEO, 3).unwrap());
    let ctx = ctx_at(DEADLINE, Actor::System);
    for cmd in [
        Command::CloseAuction {
            auction_id: "au-1".into(),
            ended_ts_ms: None,
        },
        Command::FinalizeAuction {
            auction_id: "au-1".into(),
        },
    ] {
        log.extend(exec(&mut live, &ctx, cmd).unwrap());
    }
    assert!(
        log.iter()
            .any(|e| matches!(e.event, Event::AuctionRollOff { .. })),
        "the 90/90 tie is rolled off"
    );

    let mut replayed = roll_auction(1);
    for env in &log {
        replayed.replay(env);
    }
    assert_eq!(
        live.state().guild(GUILD),
        replayed.state().guild(GUILD),
        "replay diverged"
    );
}

// -- the draw itself ---------------------------------------------------------

fn rolls(pairs: &[(PlayerId, u32)]) -> Vec<Roll> {
    pairs
        .iter()
        .map(|&(player, roll)| Roll { player, roll })
        .collect()
}

#[test]
fn d100_stays_between_1_and_100_and_reaches_both() {
    let draws: Vec<u32> = (0..10_000u64).map(|s| Rng::new(s).d100()).collect();
    assert!(draws.iter().all(|r| (1..=100).contains(r)));
    assert!(draws.contains(&1));
    assert!(draws.contains(&100));
}

#[test]
fn with_items_to_spare_everyone_wins_highest_first() {
    let (won, rounds) = roll_winners(&rolls(&[(1, 20), (2, 80), (3, 50)]), 5, &mut Rng::new(1));
    assert_eq!(won, vec![2, 3, 1]);
    assert!(rounds.is_empty());
}

#[test]
fn no_rolls_no_winners() {
    let (won, rounds) = roll_winners(&[], 1, &mut Rng::new(1));
    assert!(won.is_empty());
    assert!(rounds.is_empty());
}

#[test]
fn a_tie_across_the_cut_rolls_off_for_the_slots_left() {
    let field = rolls(&[(1, 95), (2, 90), (3, 90), (4, 10)]);
    let (won, rounds) = roll_winners(&field, 2, &mut Rng::new(3));
    assert_eq!(won.len(), 2);
    assert_eq!(won[0], 1, "the clear leader wins without rolling again");
    assert!(won[1] == 2 || won[1] == 3);
    assert!(!rounds.is_empty());
    let first: Vec<PlayerId> = rounds[0].iter().map(|r| r.player).collect();
    assert_eq!(first, vec![2, 3], "only the players tied at the cut");
    assert!(rounds
        .iter()
        .flatten()
        .all(|r| r.player != 1 && r.player != 4));
}

#[test]
fn the_same_seed_draws_the_same_roll_off() {
    let field = rolls(&[(1, 77), (2, 77), (3, 77)]);
    let a = roll_winners(&field, 1, &mut Rng::new(99));
    let b = roll_winners(&field, 1, &mut Rng::new(99));
    assert_eq!(a, b);
    assert_eq!(a.0.len(), 1);
}

/// Opens `au-9` as `flavor` asking for `live`, and returns the projection.
fn opened_asking(flavor: Flavor, live: bool) -> (Vec<Envelope>, Auction) {
    let mut l = Ledger::new();
    let envelopes = exec(
        &mut l,
        &ctx_at(OPENED, Actor::User(OFFICER)),
        Command::OpenAuction {
            auction_id: "au-9".into(),
            item: cloak(),
            flavor,
            min_bid: 0,
            num_items: 1,
            min_bid_to_lock_for_main: 0,
            over_bid_to_win_main: 0,
            duration_ms: DEADLINE - OPENED,
            debit_dkp: false,
            live,
            voice_channel: None,
            roll_cost: 0,
            rollers: None,
        },
    )
    .unwrap();
    let a = l.state().guild(GUILD).unwrap().auctions["au-9"].clone();
    (envelopes, a)
}

fn recorded_live(envelopes: &[Envelope]) -> bool {
    match &envelopes[0].event {
        Event::AuctionOpened { live, .. } => *live,
        other => panic!("{other:?}"),
    }
}

/// Ziglax, 2026-10-05: a roll opened without hours runs like a short
/// auction, in the auction channel with the bell. The fact records it.
#[test]
fn a_roll_on_the_bid_time_is_live_and_the_fact_says_so() {
    let (envelopes, a) = opened_asking(Flavor::Roll, true);
    assert!(recorded_live(&envelopes));
    assert!(a.live);
}

#[test]
fn a_roll_with_hours_is_not_live() {
    let (envelopes, a) = opened_asking(Flavor::Roll, false);
    assert!(!recorded_live(&envelopes));
    assert!(!a.live);
}

/// The flavor decides for the others: a short auction is live whatever the
/// request says, a long one never is, and neither writes `live` in the fact.
#[test]
fn only_a_roll_carries_live_in_the_fact() {
    for asked in [false, true] {
        let (envelopes, a) = opened_asking(Flavor::Short, asked);
        assert!(!recorded_live(&envelopes));
        assert!(a.live, "a short auction is always live");

        let (envelopes, a) = opened_asking(Flavor::Long, asked);
        assert!(!recorded_live(&envelopes));
        assert!(!a.live, "a long auction is never live");
    }
}

const RAID: u64 = 500;
const RAID_3: u64 = 503;

/// The three members, a raid channel plus `RAID_3` as an extra one, and a
/// roll opened with `voice_channel`.
fn group_roll(
    voice_channel: Option<u64>,
    rollers: Option<Vec<PlayerId>>,
    flavor: Flavor,
) -> Result<Ledger, Rejection> {
    let mut l = roll_auction(1);
    exec(
        &mut l,
        &ctx_at(OPENED, Actor::System),
        Command::UpdateConfig {
            patch: nocturnal_core::event::ConfigPatch {
                raid_channel: Some(RAID),
                extra_raid_channels: Some(vec![RAID_3]),
                ..Default::default()
            },
        },
    )
    .unwrap();
    exec(
        &mut l,
        &ctx_at(OPENED, Actor::User(OFFICER)),
        Command::OpenAuction {
            auction_id: "au-2".into(),
            item: cloak(),
            flavor,
            min_bid: 0,
            num_items: 1,
            min_bid_to_lock_for_main: 0,
            over_bid_to_win_main: 0,
            duration_ms: DEADLINE - OPENED,
            debit_dkp: false,
            live: true,
            voice_channel,
            roll_cost: 0,
            rollers,
        },
    )?;
    Ok(l)
}

fn roll_from(l: &mut Ledger, player: PlayerId, in_channel: Option<u64>) -> Result<(), Rejection> {
    exec(
        l,
        &ctx_at(OPENED + 1, Actor::User(player)),
        Command::RollForAuction {
            auction_id: "au-2".into(),
            player,
            seed: seed_for(50),
            voice_channel: in_channel,
        },
    )
    .map(|_| ())
}

/// 2026-10-22: Plane of Time splits the raid into groups, and loot that drops
/// for the group in Raid 3 is rolled by the members who were in Raid 3 when
/// the officer opened the roll. Where anyone is at the click does not matter:
/// a member of the group who has since left still rolls, and one who joined
/// the channel afterwards does not.
#[test]
fn a_roll_for_one_raid_channel_takes_rolls_only_from_who_was_there_at_the_open() {
    let mut l = group_roll(Some(RAID_3), Some(vec![ANNA, BORIS]), Flavor::Roll).unwrap();
    let a = &l.state().guild(GUILD).unwrap().auctions["au-2"];
    assert_eq!(a.voice_channel, Some(RAID_3));
    assert_eq!(a.rollers, Some(vec![ANNA, BORIS]));

    roll_from(&mut l, ANNA, Some(RAID_3)).unwrap();
    roll_from(&mut l, BORIS, None).unwrap();
    assert_eq!(
        roll_from(&mut l, CLEO, Some(RAID_3)),
        Err(Rejection::NotInRollSnapshot { channel: RAID_3 })
    );
    let rolled: Vec<_> = l.state().guild(GUILD).unwrap().auctions["au-2"]
        .rolls
        .iter()
        .map(|r| r.player)
        .collect();
    assert_eq!(rolled, vec![ANNA, BORIS]);
}

/// An empty channel would open a roll nobody could take.
#[test]
fn a_roll_for_an_empty_channel_is_not_opened() {
    assert_eq!(
        group_roll(Some(RAID_3), Some(vec![]), Flavor::Roll).err(),
        Some(Rejection::NobodyInVoiceChannel { channel: RAID_3 })
    );
    assert_eq!(
        group_roll(Some(RAID_3), None, Flavor::Roll).err(),
        Some(Rejection::NobodyInVoiceChannel { channel: RAID_3 })
    );
}

/// The channel rolls opened between the 16:29 and the snapshot deploys on
/// 2026-10-22 carry no snapshot; they keep the rule they opened under, the
/// clicker's channel at the click.
#[test]
fn a_channel_roll_without_a_snapshot_checks_the_channel_at_the_click() {
    let mut l = group_roll(None, None, Flavor::Roll).unwrap();
    let mut envelopes = l
        .propose(
            &ctx_at(OPENED, Actor::User(OFFICER)),
            &Command::OpenAuction {
                auction_id: "au-old".into(),
                item: cloak(),
                flavor: Flavor::Roll,
                min_bid: 0,
                num_items: 1,
                min_bid_to_lock_for_main: 0,
                over_bid_to_win_main: 0,
                duration_ms: DEADLINE - OPENED,
                debit_dkp: false,
                live: true,
                voice_channel: Some(RAID_3),
                roll_cost: 0,
                rollers: Some(vec![ANNA]),
            },
        )
        .unwrap();
    if let Event::AuctionOpened { rollers, .. } = &mut envelopes[0].event {
        *rollers = None;
    }
    l.commit(&envelopes);
    let roll_old = |l: &mut Ledger, player, in_channel| {
        exec(
            l,
            &ctx_at(OPENED + 1, Actor::User(player)),
            Command::RollForAuction {
                auction_id: "au-old".into(),
                player,
                seed: seed_for(50),
                voice_channel: in_channel,
            },
        )
        .map(|_| ())
    };
    roll_old(&mut l, CLEO, Some(RAID_3)).unwrap();
    assert_eq!(
        roll_old(&mut l, BORIS, Some(RAID)),
        Err(Rejection::NotInVoiceChannel { channel: RAID_3 })
    );
}

/// Without a channel a roll is open to everyone, wherever they are, as every
/// roll auction before 2026-10-22 was.
#[test]
fn a_roll_without_a_channel_takes_anyone() {
    let mut l = group_roll(None, None, Flavor::Roll).unwrap();
    roll_from(&mut l, ANNA, Some(RAID_3)).unwrap();
    roll_from(&mut l, BORIS, None).unwrap();
}

/// The channel must be one the raid counts, and only a roll can be limited:
/// a bidding auction has no rolls to gate.
#[test]
fn only_a_roll_and_only_a_raid_channel() {
    assert_eq!(
        group_roll(Some(777), Some(vec![ANNA]), Flavor::Roll).err(),
        Some(Rejection::NotARaidChannel { channel: 777 })
    );
    assert_eq!(
        group_roll(Some(RAID_3), Some(vec![ANNA]), Flavor::Short).err(),
        Some(Rejection::WrongAuctionFlavor)
    );
}

/// Earlier `auction.opened` events carry no channel and replay as open to
/// everyone; a roll for one channel keeps it through the log's JSON.
#[test]
fn the_channel_survives_the_log_and_old_events_have_none() {
    let old: Event = serde_json::from_str(
        r#"{"kind":"auction.opened","payload":{"auction_id":"a","item":{"id":"1","name":"Cloak"},"flavor":"roll","min_bid":0,"num_items":1,"min_bid_to_lock_for_main":0,"over_bid_to_win_main":0,"deadline_ts_ms":1}}"#,
    )
    .unwrap_or_else(|e| panic!("old event no longer parses: {e}"));
    assert!(matches!(
        old,
        Event::AuctionOpened {
            voice_channel: None,
            ..
        }
    ));

    let l = group_roll(None, None, Flavor::Roll).unwrap();
    let envelopes = l
        .propose(
            &ctx_at(OPENED + 2, Actor::User(OFFICER)),
            &Command::OpenAuction {
                auction_id: "au-3".into(),
                item: cloak(),
                flavor: Flavor::Roll,
                min_bid: 0,
                num_items: 1,
                min_bid_to_lock_for_main: 0,
                over_bid_to_win_main: 0,
                duration_ms: 1,
                debit_dkp: false,
                live: true,
                voice_channel: Some(RAID_3),
                roll_cost: 0,
                rollers: Some(vec![ANNA, BORIS]),
            },
        )
        .unwrap();
    let json = serde_json::to_string(&envelopes[0].event).unwrap();
    assert!(json.contains(r#""voice_channel":503"#), "{json}");
    assert!(json.contains(r#""rollers":[101,102]"#), "{json}");
    let back: Event = serde_json::from_str(&json).unwrap();
    assert!(matches!(
        back,
        Event::AuctionOpened {
            voice_channel: Some(RAID_3),
            ..
        }
    ));
}

/// 2026-10-22: a roll can cost the winner DKP (5 for the guild's rolls). The
/// winner pays it, with the winning roll in the log line; everyone else keeps
/// their DKP.
#[test]
fn a_roll_with_a_cost_charges_the_winner_and_nobody_else() {
    let mut l = roll_auction_costing(1, 5);
    assert!(auction(&l).debit_dkp);
    roll(&mut l, ANNA, 90).unwrap();
    roll(&mut l, BORIS, 40).unwrap();
    close_and_draw(&mut l);
    let g = l.state().guild(GUILD).unwrap();
    assert_eq!(auction(&l).winners[0].player, ANNA);
    assert_eq!(auction(&l).winners[0].amount, 5);
    assert_eq!(g.balance(ANNA), 495);
    assert_eq!(g.balance(BORIS), 500);
    let line = g.players[&ANNA].log.last().unwrap();
    assert_eq!(line.dkp, -5);
    assert!(line.comment.contains("Cloak"), "{}", line.comment);
}

/// With two items each winner pays, and a roll-off loser pays nothing.
#[test]
fn every_winner_of_a_costed_roll_pays() {
    let mut l = roll_auction_costing(2, 5);
    roll(&mut l, ANNA, 90).unwrap();
    roll(&mut l, BORIS, 60).unwrap();
    roll(&mut l, CLEO, 10).unwrap();
    close_and_draw(&mut l);
    let g = l.state().guild(GUILD).unwrap();
    assert_eq!(
        (g.balance(ANNA), g.balance(BORIS), g.balance(CLEO)),
        (495, 495, 500)
    );
}

/// A member who could not pay is refused the roll, by the same reservation a
/// bid is held to, so a winner is never charged into the negative at the draw.
#[test]
fn a_member_who_cannot_pay_the_cost_cannot_roll() {
    let mut l = roll_auction_costing(1, 5);
    exec(
        &mut l,
        &ctx_at(OPENED, Actor::System),
        Command::ImportPlayer {
            player: STRANGER,
            balance: 3,
            characters: vec![],
            creation_ts_ms: 1,
            log: vec![],
            legacy_id: None,
        },
    )
    .unwrap();
    assert!(matches!(
        roll(&mut l, STRANGER, 50),
        Err(Rejection::InsufficientBalance { needed: 5, .. })
    ));
}

/// A free roll stays free, and the cost is only for rolls: a bidding auction
/// has its bids, and a negative cost would pay the winner.
#[test]
fn a_cost_is_for_rolls_and_never_negative() {
    let mut l = roll_auction(1);
    roll(&mut l, ANNA, 90).unwrap();
    close_and_draw(&mut l);
    assert_eq!(l.state().guild(GUILD).unwrap().balance(ANNA), 500);

    let open = |flavor, roll_cost| Command::OpenAuction {
        auction_id: "au-9".into(),
        item: cloak(),
        flavor,
        min_bid: 0,
        num_items: 1,
        min_bid_to_lock_for_main: 0,
        over_bid_to_win_main: 0,
        duration_ms: 1,
        debit_dkp: true,
        live: false,
        voice_channel: None,
        roll_cost,
        rollers: None,
    };
    let ctx = ctx_at(OPENED, Actor::User(OFFICER));
    assert_eq!(
        l.propose(&ctx, &open(Flavor::Long, 5)).err(),
        Some(Rejection::WrongAuctionFlavor)
    );
    assert_eq!(
        l.propose(&ctx, &open(Flavor::Roll, -5)).err(),
        Some(Rejection::InvalidAmount)
    );
    let free = l.propose(&ctx, &open(Flavor::Roll, 0)).unwrap();
    let json = serde_json::to_string(&free[0].event).unwrap();
    assert!(
        !json.contains("roll_cost"),
        "a free roll writes no cost: {json}"
    );
}
