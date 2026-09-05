# Character bids

*Added 2026-09-05. Off by default; an officer turns it on with
`/feature characterbids on` and off again the same way.*

A bid names one of the member's roster characters. When the feature is on,
the **Main bid** and **Alt bid** buttons do three things before the amount
modal opens:

1. **Filter by rank.** Main bid offers the member's Main-ranked character;
   Alt bid offers every other character on their row (Second-ranked and
   unranked). Officers set ranks with `/roster rank`; members can still
   type one into `/roster add|edit` until the officers decide otherwise.
2. **Filter by the item.** The mirrored item row (pqdi's EQEmu `items`
   row, cached on disk when the auction opened) carries the class, race and
   slot bitmasks. A character whose class the item excludes is not offered;
   race is checked when the character has sent a profile, since the roster
   does not store race. **Only equipment is gated**: a row with no
   equipment slot — tradeskill drops, quest pieces, spells — offers every
   character on the side, because the class bits on such rows mean nothing
   to a bidder (Ziglax, 2026-09-05).
3. **Show the upgrade.** For each offered character: the slot the item
   would go in, what is worn there now (from the character's last Zeal
   profile, via the site snapshot) and the stat delta. Paired slots (ears,
   wrists, rings) compare against the weaker of the two. A character with
   no profile is offered with "no gear on record".

Then:

- **one** eligible character → the modal opens straight away, titled
  `Main bid · Vexira`, with the upgrade line as the field's placeholder;
- **several** → an ephemeral select lists them with the upgrade as each
  option's description, and the choice opens the modal;
- **none** → an ephemeral refusal naming the item's class line and the
  characters that were excluded, and no modal.

Nothing about this waits on the network: the click has three seconds and a
modal cannot follow a deferred acknowledgement, so the picker reads the
ledger projection, the mirror's disk cache and the site's last snapshot.
Without an item row (TAKP items, or pqdi unreachable when the auction
opened) every character on the side is offered and the upgrade line says
"no item data to compare" — the auction is never blocked by a lookup.

## What the ledger records

`auction.bid_placed` gained an optional `character`; so did each entry in
`auction.finalized`'s `winners`. Bids from before the feature, and bids
placed while it is off, have none and load exactly as before (pinned in
`serde_pinning.rs`). The ledger checks two things about a named character,
independent of the buttons: it is on the bidder's own row, and it is on the
side the bid claims (`character_not_eligible`). Usability is the Discord
layer's filter only — the item row is not in the ledger.

Winner lines become `@member (Vexira) for 12 dkp`; bid lists stay
anonymous. The upgrade comparison is shown to the bidder only — never in
the channel.

## The officer's safety net at close

When a short auction closes, the bot checks each proposed winner's
character against the item once more, from the roster's class. A winner
who cannot use it turns the closed embed **orange**, adds a "Check before
confirming" field naming the character, its class and the item's class
line, and relabels the button **Confirm anyway** (Discord's four button
colours have no orange; red already means cancelled). The custom id is
unchanged, so confirming works exactly as before. This catches what the
picker cannot: a rank changed after the bid, an item row that arrived
after the click, or a bid placed while the feature was off and turned on
before close.

## Attendance requirements

Separately from the toggle, `/configure mainbidminra:50` (and
`altbidminra`) refuses a bid on that side when the member's attendance —
the same figure the roster page shows, see [attendance.md](attendance.md)
— is under the percentage. The refusal states both numbers. Zero, the
default, means no requirement. Both apply whether or not character bids
are on.

## Turning it off

`/feature characterbids off`. The next click opens the plain modal; bids
already placed keep their character. No data is lost either way, which is
the point of keeping the toggle in config rather than in a build.
