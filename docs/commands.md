# Command inventory & behavioural spec

Status: **specified** (M0). Extracted from the legacy bot at `main@8ec128e`
(the `docker-deployment` merge — the audited revision plus the first hardening
commits). The guildie's `phase1-stability` branch is landing audit Phase 1 on
top; it changes robustness, not behaviour, so this spec tracks `main`.

This is the rewrite's contract: same commands, same options, same embeds and
flows — except the items in [Deliberate changes](#deliberate-changes), which
need officer sign-off.

## Environment & data (legacy, for migration)

- Env: `DISCORD_TOKEN`, `DISCORD_CLIENT_ID`, `MONGO_URL`, `LOG_LEVEL`
  (`DEBUG` gates debuglog writes). Gateway intents: Guilds, GuildVoiceStates,
  DirectMessages.
- Mongo db `DKP`, collections and shapes (source of the migration tool):
  - `players`: `{player (discord id), guild, current (int), characters[],
    creationDate, log: [{dkp, comment, date, raid: {_id, name}|null, item?}]}`
  - `raids`: `{guild, name, date, attendance: [{players[], comment, date,
    dkps}], tickDuration (ms), dkpsPerTick, active, deprecated, eventId}`
    — the `attendance` array doubles as the tick record.
  - `auctions` (long + stored-at-end short): `{guild, item, minBid,
    numberOfItems, minBidToLockForMain, overBidtoWinMain, bids: [{player,
    amount, bidForMain}], auctionActive, createdAt, auctionEnd, messageId,
    winners[]}`
  - `options` (guild config): see Configuration.
  - `shortAuctions`: declared, never used. `debuglog`: debug events, no successor.

## Guild configuration (`/configure`, `/showconfig`)

`/configure` — restricted + Discord admin default-perms. Options (all channels
by picker, role by picker):

| Option | Req | Meaning | Default / notes |
|---|---|---|---|
| role | ✔ | Officer role for `restricted` commands | Guild Administrators always bypass |
| raidchannel | ✔ | Voice channel counted for ticks | |
| logchannel | ✔ | Text channel for DKP movement embeds | |
| auctionchannel | ✔ | Text channel for short auctions | |
| longauctionchannel | | Text channel for long auctions | falls back to auctionchannel |
| secondraidchannel | | Second voice channel for attendance | must differ from raidchannel |
| tickduration | | Minutes between ticks (0.5 = 30 s) | 6 min if unset at raid start |
| raiddeprecationtime | | Days before raids stop counting for attendance | **legacy default bug: 90 ms, not 90 days** |
| bidtime | | Short-auction duration, seconds (30–1000) | 60 |
| minbid | | Default minimum bid | 0 |
| minbidtolockformain | | Min bid for a MAIN bid to lock priority | 0 |
| overbidtowinmain | | Amount an ALT must overbid the top MAIN to win | 0 |
| raidhelperapikey | | RaidHelper API key (enables event integration) | never echoed; `/showconfig` shows presence only |

`/showconfig` — admin-only; embed listing every setting with human units
("Not set" when absent).

## Command inventory

Access: **all** = any member · **officer** = `restricted` flag (officer role or
guild Administrator) · **admin** = Discord Administrator default-perms.

| Command | Access | Options | One-liner |
|---|---|---|---|
| `/playerdkp` | all | player? | Show a player's balance (ephemeral, `\`N\` DKP`) |
| `/dkphistory` | all | player? | Paginated personal history, ticks aggregated per raid |
| `/listplayersdkps` | all | — | Paginated DKP + attendance table; **refused during an active raid** |
| `/adddkp` | officer | player, dkp ≥1, comment | Credit one player |
| `/removedkp` | officer | player, dkp, comment | Debit one player (**no lower bound — legacy bug**) |
| `/addraiddkp` | officer | dkp, comment | Credit everyone in the raid channel; needs active raid; logs attendance entry + embed |
| `/registercharacter` | all | name | Link an EQ character to the caller |
| `/startraid` | officer | name?, dkpspertick?, tickduration? | Start the (single) raid; awards a `Start` tick to those present |
| `/endraid` | officer | — | End raid; final `End` attendance entry (0 DKP); posts full movement log; RaidHelper auto-award if event-linked |
| `/startbid` | officer | search, minbid?, numitems?, database? | Short auction flow (below) |
| `/startlongbid` | officer | search, minbid?, numitems?, duration? (h, default 48), database? | Long auction; bids via `/bid` |
| `/auctiondetails` | officer | auctionid | Dump bids/winners of a **settled** auction; refused while it is still running; publicly announces the peek in the auction channel (only when it actually showed something) |
| `/cancelauction` | officer role | auctionid | Void a running auction: no winner, no DKP. Bids stay readable, not republished |
| `/endauction` | officer role | auctionid | Close and settle now, skipping the wait; the deadline becomes that moment |
| `/searchitem` | all | search, database? | Item lookup without an auction |
| `/searchlogs` | all | search | Paginated ledger search by comment; searches matching `/tick/i` are refused with flavor text |
| `/backup` | admin | — | `backup.zip` containing `players.json` + `raids.json`, attached to the reply (ephemeral); the roster page reads these, so the names are a contract |
| `/addraideventdkp` | officer | dkp, raidid, eventid | Manually run the RaidHelper award for a past raid |
| `/configure` `/showconfig` | see above | | |

Flavor: rejections use `:prohibited:` + "DKP Bot scowls at you…" phrasing —
part of the bot's personality; keep the tone (render the emoji properly, E13).

## Raid lifecycle

- One active raid per guild (checked, not enforced — race in legacy).
- `/startraid`: RaidHelper (if key set) is queried for an event starting ±10
  min; its title/id auto-name and link the raid when no name given. Fallback
  name: current date. Members of raidchannel (+ secondraidchannel) get
  `dkpsPerTick` with comment `Start`.
- **Tick** (worker, every 10 s): due when `last attendance entry date +
  tickDuration < now`. Members of both voice channels get `dkpsPerTick`
  (comment `Tick`), one attendance entry appended, blue embed to logchannel
  listing everyone present.
- `/endraid`: 0-DKP `End` attendance entry; movement log (gains, losses, loot
  with item links) rendered chronologically in chunked embeds (35 lines/embed).
  If event-linked: RaidHelper award runs automatically with **5 DKP** (hardcoded).
- Deprecation (worker, hourly): raids older than `raidDeprecationTime` are
  flagged `deprecated` and stop counting toward attendance.
- **Attendance** = player's attended entries / entries possible since their
  `creationDate`, over non-deprecated raids, as % (2 dp); no possible entries
  → 100 %.

## Auctions

**Item search** (shared by `/startbid`, `/startlongbid`, `/searchitem`):
`database` = `quarm` (default; pqdi.cc JSON API + stat-table scrape) or `takp`
(HTML scrape). Results: 1 → item embed directly; 2–25 → one button per item
(30 s picker); 26–40 → plain text list ("refine"); >40 → refused. Item embed:
name + #id, stat block, thumbnail icon, link. Officer confirms with a
**Start Auction** button (30 s window).

**Short auction** (`/startbid`): posted to auctionchannel with countdown
(`ends <t:…:R>`), min-bid line, and buttons **I want to bid** / **Bid for
Alter** / **Cancel** (cancel = officer-role only). Bid buttons DM the clicker;
the amount is typed in DM (60 s collector; `0` cancels). Runs `bidTime`
seconds; bell sound plays in the raid channel(s) at start. At close: bids
revalidated against current balances (invalid ones dropped), winners computed,
auction stored to the `auctions` collection, result embed shows winners,
anonymized bid amounts, and the auction id, with a **Confirm Winner/s** button
(6 min, starter-officer only — the role check is commented out in legacy).
Confirmation is what debits winners.

**Long auction** (`/startlongbid`): document created up front; embed to
longauctionchannel with auction id + relative end time. Bids arrive via `/bid`
(ephemeral confirmation `Bid N DKPs as MAIN/ALT on <item>`; re-bid replaces;
`0` retracts). The worker (60 s cycle) finalizes auctions **20 minutes after**
`auctionEnd` (grace period), computes winners, marks finished, edits the embed
green with winners + anonymized bids. **Legacy never debits these winners** —
the rewrite does (deliberate change).

**Winner rules** (from `Auction.getWinners`/`getTopBids` — the tested logic):
1. Bids are MAIN or ALT. A MAIN bid "locks" if `amount ≥ minBidToLockForMain`.
   An ALT bid competes with MAINs only if it exceeds the top MAIN bid by
   ≥ `overBidtoWinMain` (when configured).
2. Top `numberOfItems` bids win, MAIN-qualified bids first; ALT winners fill
   remaining slots.
3. Ties on amount: higher attendance wins; still tied → random draw.
   *Intended*, but see deliberate changes — legacy attendance is effectively
   never populated (short auctions pass `checkAttendance: false`; long bids
   never store it), and the draw indexes the wrong array (E3). Effective
   legacy behaviour: pure random among tied bids.
4. Bid validation (both flows): integer, > 0, ≥ minBid, ≤ current balance at
   bid time; re-checked at close for short auctions.

## Ledger presentation

- `/dkphistory`: newest-first; consecutive ticks for the same raid collapse to
  one `**N** *raid* aggregated ticks` line; loot lines show item name; 30
  entries/page, button pagination (2 min lifetime, then disabled).
- `/listplayersdkps`: two-column embed table (rank/name | DKP/attendance %),
  10/page, caller's own row appended below; only players with log activity
  within `raidDeprecationTime`; page buttons as above.
- `/searchlogs`: case-insensitive comment search, 20/page, resolves display
  names, item links inline.
- Embed colors: start/green 5763719 · tick/blue 3447003 · adjust/orange
  15105570 · raid-end/pink 15277667 · long-auction result green.

## RaidHelper integration

- `GET raid-helper.dev/api/v4/servers/{guild}/events` (auth: API key) at
  `/startraid`; `GET /api/v4/events/{eventId}` for awards.
- Award rule: signups not `Absence`/`Bench`, with raid attendance count ≥
  `min(10, ⌊distinct attendees / 2⌋, ≥1)` ticks, get the DKP. Report embed
  lists: Rewarded / NOT enough attendance / NOT subscribed / NOT attended.
- `/endraid` auto-award is hardcoded 5 DKP → make configurable (deliberate change).

## `/who` log parsing (parser retained, no command)

Input: pasted EQ `/who` output. Legacy parse: timestamp from `[…]`; character
names = first word after each `]`, filtering literals `Players`/`There`.
Awards `addByCharacter` per name (registered characters only; unregistered
names reported in the errors field). Public embed: comment, DKP, sorted
character list, errors. Note legacy quirks: `raid` boolean is passed where a
raid object is expected → `{_id: null}` history entries (E10); character
lookup is case-sensitive (E11) and not guild-scoped on the first check.

## Telemetry provisioning (`/dpstoken`, `/dpsrevoke`)

Absorbed from `everquest-observability/bot/dpsbot.py` (Python, retired at
cutover) so one bot serves the guild. Same UX, ledger-backed internals.

| Command | Access | Behaviour |
|---|---|---|
| `/dpstoken` | member with a mapped guild rank | Issue (or refresh) the caller's personal OTLP ingest token + Perses dashboard access |
| `/dpsrevoke member` | Administrator or Manage Guild | Revoke a member's token and dashboard access |

Contract (from the legacy implementation):

- Username must match `^[a-z0-9._]{2,32}$`; guild-only commands.
- **Role mapping**: Discord roles → Perses role (`editor` > `viewer`), from a
  live-editable `roles.yaml` (path configurable; re-read per command). No
  mapped rank → friendly refusal.
- **Issue**: 48-hex-char token; DM with the token, the Windows PowerShell
  one-liner installer, and dashboard URL; closed DMs → ephemeral fallback with
  the token in a spoiler. Already has a token → refresh dashboard role +
  personal project only ("ask an officer to /dpsrevoke first").
- **Materialized files** (paths configurable, legacy-compatible formats):
  - `tokens.txt`: `{token} # {username}` per line (gateway auth).
  - Perses provisioning dir: `rb-{user}.yaml` (RoleBinding in project
    `everquest`), `50-project-{user}.yaml` (personal project `u-{user}`),
    `51-ds-{user}.yaml` (its Prometheus datasource), `52-rb-own-{user}.yaml`
    (owner binding). Root-owned systemd `.path` units still watch these files;
    the bot stays unprivileged.
- **Revoke**: remove the token line, delete all four provisioning files
  (plus legacy `user-{user}.yaml`).

What changes inside: issue/refresh/revoke are ledger events
(`telemetry.token.issued/.access_updated/.revoked` — see `events.md`), and the
files are **derived state**: rewritten idempotently from the projection after
every change *and on boot*. A half-written file, a lost VM, or a manual edit
gone wrong heals itself at the next startup — and "who had a token last
March?" is a ledger query, which today has no answer at all.

Deployment note: these commands need the observability VM's filesystem, so the
unified bot deploys **on that VM** (it is a small static binary; the DKP side
doesn't care where it runs). Paths and the dashboard URL are config
(`operations.md`); both commands disable cleanly when unconfigured.

## Deliberate changes (officer sign-off — all fixes, no feature changes)

1. Long-auction winners are debited at finalization.
2. Tie-break works as intended: real attendance, correct candidate pool, and
   the draw is recorded (auditable) — replacing the de-facto coin flip.
3. No negative balances; debits are conditional at debit time; `/removedkp`
   rejects amounts < 1.
4. Bids during the close window resolve deterministically (close seq wins).
5. Raids never end from transient Discord errors; raid-deprecation default is
   90 days, not 90 ms.
6. Character names matched case-insensitively and guild-scoped.
7. `/searchlogs` input treated as literal text (no regex injection); the Tick
   guard actually guards.
8. `/registercharacter` reports what actually happened.
9. `/endraid` RaidHelper award amount configurable (default 5).
10. Ephemeral/public visibility per current *intent* (several legacy ephemeral
    flags are silent no-ops); `:prohibited:` renders as the emoji.
11. Stale buttons (pre-restart) answer "this auction has ended" instead of dying.
12. `/auctiondetails` is refused while the auction is still running — officers
    bid too, and the standing bids of a live auction are worth an item to
    whoever reads them. Settled and cancelled auctions read as before, and the
    public peek notice is posted only when something was actually shown.
13. `/cancelauction` and `/endauction` exist (upstream, 2026-08-24): there was
    no way to pull or force-close a long auction. Both require the officer
    **role** itself — an Administrator who was never given it is refused,
    because these move DKP and the guild already said who may do that. With no
    officer role configured they fall back to Administrator.
14. Bids are typed into a **modal**, not a DM, and long auctions carry the same
    *Main bid* / *Alt bid* buttons as short ones — so `/bid` is gone. Upstream's
    reason was a `MessageCollector` race we never had (one pending prompt per
    bidder); the reasons that carry over are that a modal needs no open DMs and
    that both auction kinds now behave the same way. Confirmations name the
    item and the side.
15. `/parsedkps` is removed, as upstream removed it — nothing used it. The
    `/who` parser stays in `nocturnal-core` with its tests, so restoring the
    command is a command wrapper away.
16. The item stat block is trimmed: leading and trailing padding gone, runs of
    spaces collapsed, blank rows dropped, and the 56-dash rule slimmed. It is
    read on a phone during a raid.
17. `/configure` refuses values that used to be accepted and break later: a
    tick duration of zero, a deprecation window of zero, a bid time outside
    30–1000 s, negative bid floors, a blank RaidHelper key, and a second raid
    channel equal to the first (which would double everyone's tick). Nothing
    is applied when a value is refused.

## Resolved decisions (2026-08-21: keep current behaviour throughout)

Per the maintainer: current UX stays exactly as officers know it; changes can
be requested later. Concretely:

- **Bell sound**: kept — played at short-auction start in the raid channel(s),
  strictly fire-and-forget (a voice failure can never touch an auction).
- **Bid entry**: the DM-typed-amount flow stays, hardened — the collector is
  bound to its specific auction, closed DMs get the ephemeral fallback, and
  nothing in the path can crash the bot. (A modal remains a cheap later
  upgrade if officers ask.)
- **`/auctiondetails` public callout**: kept as-is.

The [deliberate changes](#deliberate-changes-officer-sign-off--all-fixes-no-feature-changes)
above are bug fixes, not UX changes — they ship with the rewrite; officers can
veto any individually later.
