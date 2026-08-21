# Event taxonomy (draft)

Status: **drafted** — becomes *specified* in M1 when the Rust types land with
serde round-trip + replay-determinism tests. Names and payloads below will shift
as the legacy bot's exact behaviour is extracted in M0; the envelope and rules
should not.

## Envelope

Every event shares:

| Field | Type | Notes |
|---|---|---|
| `seq` | u64 | Contiguous, assigned by the single writer. The clock of the system. |
| `ts` | RFC 3339 UTC | Wall time at append |
| `guild_id` | u64 | Discord guild (single-guild today; multi-guild stays possible) |
| `actor` | u64 \| `system` | Discord user id of the officer/member, or `system` for scheduler-driven events |
| `kind` | string | Discriminant, e.g. `dkp.adjusted`, `auction.finalized` |
| `v` | u8 | Payload schema version for this kind (starts at 1) |
| `correlation_id` | uuid, optional | Ties bids/closes to their auction, ticks to their raid |
| `payload` | object | Kind-specific, below |

Rules:

- Events are **facts, never commands** — past tense, already validated.
  Invalid requests are rejected before append and never appear in the log.
- Payload schemas are **append-only**: new optional fields are fine; changing
  a field's meaning requires a new `v` and an explicit upgrade in the reader.
- The `apply` fold must handle every historical `(kind, v)` forever.

## Kinds

### Players & characters
| Kind | Payload (sketch) | Notes |
|---|---|---|
| `player.registered` | discord_id, main_character | Self or officer registration |
| `player.character_linked` | character, class, level | From registration or `/who` parse |
| `player.character_unlinked` | character | |
| `player.imported` | balance, lifetime_earned, lifetime_spent, characters[] | **Genesis only** (migration) |

### DKP ledger
| Kind | Payload | Notes |
|---|---|---|
| `dkp.adjusted` | player, delta (non-zero i64), reason, source | Covers legacy add/remove; bounds enforced at decide time (`/removedkp -5` class of bug is unrepresentable) |
| `dkp.decayed` | rules, per-player deltas | If/when decay exists — batch event |

### Raids
| Kind | Payload | Notes |
|---|---|---|
| `raid.started` | raid_id, name, tick_interval, tick_value | Decide step refuses if one is active |
| `raid.roster_updated` | raid_id, joined[], left[] | From `/who` log parsing |
| `raid.tick` | raid_id, tick_no, awarded[{player, amount}] | One event per tick — the "41 writes per tick" pattern becomes one append; `tick_no` makes double-award unrepresentable |
| `raid.ended` | raid_id, reason (officer \| catchup) | Transient Discord errors can no longer end a raid — only this event can |
| `raid.imported` | … | Genesis only |

### Auctions (one unified model; `flavor: short | long`)
| Kind | Payload | Notes |
|---|---|---|
| `auction.opened` | auction_id, item, flavor, min_bid, deadline, quantity | The legacy ~80 % duplicated short/long code paths collapse into one state machine |
| `auction.bid_placed` | auction_id, player, amount, channel (button \| dm) | Re-bid by same player replaces; decide step enforces bid ≤ current balance − committed bids on *other* open auctions (kills cross-auction double-spend at bid time too) |
| `auction.bid_retracted` | auction_id, player | |
| `auction.closed` | auction_id | Deadline reached — bidding ends deterministically at this seq; "bid during close window" ambiguity gone |
| `auction.tie_broken` | auction_id, candidates[], seed, winner | The draw is auditable; candidate set is the *correct* array |
| `auction.finalized` | auction_id, winners[{player, amount}] | **Is** the debit — fold decrements balances here. No separate charge step to forget |
| `auction.cancelled` | auction_id, reason | |

### Config & ops
| Kind | Payload | Notes |
|---|---|---|
| `config.updated` | key, value | Admin roles, defaults, tick settings — replaces the 1 h Mongo polling loop |
| `ops.note` | text | Officer-visible annotation ("corrected per dispute…"), keeps the log the single narrative |

## What is deliberately *not* an event

- Debug logging → `tracing`/OTLP, not the ledger (the legacy `debuglog`
  collection eating Atlas quota has no successor in the log).
- Discord message/embed ids and other presentation state → ephemeral,
  reconstructed or re-sent on boot.
- Balance snapshots → derived, never stored.
