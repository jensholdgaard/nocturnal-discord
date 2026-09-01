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
| `player.imported` | balance, lifetime_earned, lifetime_spent, characters[], `legacy_id`? | **Genesis only** (migration). `legacy_id` (added 2026-08-26) is the Mongo `_id`, carried so `/backup` can reproduce the document |

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
| `raid.merged` | from, into | Officer correction (2026-09-01): a false `/startraid` folded into the real raid. Attendance entries and every log line are re-labelled to `into`, balances untouched, `from` ceases to exist. Both must be ended |
| `raid.imported` | …, `tick_interval_ms`, `dkp_per_tick`, `event_id`? | Genesis only. The three trailing fields were added 2026-08-26 (defaulted, so older events replay unchanged) because `/backup` has to give them back |

### Auctions (one unified model; `flavor: short | long`)
| Kind | Payload | Notes |
|---|---|---|
| `auction.opened` | auction_id, item, flavor, min_bid, deadline, quantity | The legacy ~80 % duplicated short/long code paths collapse into one state machine |
| `auction.bid_placed` | auction_id, player, amount, channel (button \| dm) | Re-bid by same player replaces; decide step enforces bid ≤ current balance − committed bids on *other* open auctions (kills cross-auction double-spend at bid time too) |
| `auction.bid_retracted` | auction_id, player | |
| `auction.closed` | auction_id, `ended_ts_ms`? | Deadline reached — bidding ends deterministically at this seq; "bid during close window" ambiguity gone. `ended_ts_ms` (added 2026-08-26) is set only by `/endauction` and becomes the deadline, so the recap names when bidding actually stopped |
| `auction.tie_broken` | auction_id, candidates[], seed, winner | The draw is auditable; candidate set is the *correct* array |
| `auction.finalized` | auction_id, winners[{player, amount}] | **Is** the debit — fold decrements balances here. No separate charge step to forget |
| `auction.cancelled` | auction_id, reason | |

### Telemetry provisioning (dpsbot absorbed)
| Kind | Payload | Notes |
|---|---|---|
| `telemetry.token.issued` | username, **token_fp**, perses_role | Only the sha256 **fingerprint** — never the token. See below |
| `telemetry.token.revoked` | username, actor | Removes token + all dashboard access in the fold |
| `telemetry.access.updated` | username, perses_role | Role refresh on re-run of `/dpstoken` after a rank change |

The projection (`telemetry` map: user → fingerprint, role) is materialized to
`tokens.txt` + the Perses provisioning YAMLs after each event and on boot.
The YAMLs are fully derived; the token *line* is preserved, never regenerated.

#### Why the token secret is not in the log

An earlier draft put the token value in the payload, reasoning that the ledger
and `tokens.txt` share a host and an access boundary. That reasoning was
wrong, in three compounding ways:

- **The blast radii are not the same.** `deploy/backup.sh` tars `events` and
  `wal`; it does not touch `/etc/eq-otel`. So the ledger is copied nightly
  into `/var/backups` with a retention chain, and `tokens.txt` is not copied
  at all. The log's reach is *larger* than the file it was compared to.
- **The log is append-only, so revocation cannot reach backwards.**
  `/dpsrevoke` removes the line from `tokens.txt`, but the issuing event keeps
  the secret in the WAL, in every Parquet partition it compacts into, and in
  every backup taken since — permanently.
- **It travels.** The off-site archive uploads compacted partitions to object
  storage, and `docs/runbook.md` resolves disputes by grepping the ledger,
  which is exactly the kind of output that gets pasted into a thread.

So the log records `sha256(token)` and the secret exists in exactly one place,
like a password file. A plain hash rather than a password KDF is deliberate:
the input is 96 bits of `getrandom` output, not something a human chose.

The consequence is intentional. A ledger restored onto a fresh VM rebuilds
every member's *access* — roles, projects, bindings — but cannot rebuild their
token, because it never had it. Materialization reports any such grant so an
officer can `/dpsrevoke` and the member can re-run `/dpstoken`. A secret you
did not store is a secret that cannot leak from a backup.

### Config & ops
| Kind | Payload | Notes |
|---|---|---|
| `roster.character.set` | player, character{name, class, level, aa?, profile_url?, access[], main?} | The whole record, not a patch: replay never merges. Absorbed from nocturnal-roster-bot 2026-08-31 |
| `roster.character.removed` | player, name | |
| `config.updated` | key, value | Admin roles, defaults, tick settings — replaces the 1 h Mongo polling loop |
| `ops.note` | text | Officer-visible annotation ("corrected per dispute…"), keeps the log the single narrative |

## What is deliberately *not* an event

- Debug logging → `tracing`/OTLP, not the ledger (the legacy `debuglog`
  collection eating Atlas quota has no successor in the log).
- Discord message/embed ids and other presentation state → ephemeral,
  reconstructed or re-sent on boot.
- Balance snapshots → derived, never stored.
