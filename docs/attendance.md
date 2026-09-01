# Attendance

One definition, everywhere: the member page, the roster export, the Discord
DKP tables and the `nocturnal.guild.attendance.average` gauge all call
`GuildState::attendance_pct`. Since 2026-09-01 it is the rule Zig's roster
sheet has used all along, so the site and the sheet agree to the point.

## The rule

1. **Ticks** are the DKP-bearing attendance entries — `Start` and `Tick`.
   `End` entries and `/addraiddkp` awards are not ticks.
2. **Weeks** start Monday 00:00 UTC. Keep the **ten most recent weeks that
   had a raid**; the current, partial week counts.
3. **Drop the two worst weeks** by percentage attended. Among equal
   percentages (typically several 0 % weeks) the week with **more ticks
   held** is dropped first. With eight weeks or fewer nothing is dropped.
4. **Pool** the kept weeks: `floor(Σ attended / Σ held × 100)`. It is not a
   mean of weekly percentages, and it is floored, never rounded.

A member who has never raided while raids happened reads 0. A ledger with no
raids at all reads 100 (nothing was possible), as the legacy formula did.

Nothing depends on the raid-deprecation window or on a player's creation
time any more; the ten-week window bounds everything.

## How it was established

The sheet publishes only the final percentage. On 2026-09-01 the ledger's
WAL was replayed locally and candidate formulas were scored against two
snapshots of the sheet — the Aug 31 08:44 import (before that night's raid)
and the live sheet the next evening (after it), 120 members each. The
legacy 90-day formula matched about half. Weekly ticks with "best 8 of 10"
reached 80 %; floor instead of round and DKP-bearing ticks only reached 93 %,
with every remaining miss one to three points *low*; reading those members'
week tables exposed the tie-break (Krayr kept a 0/93 week over two 0/99
weeks; Sojutsu kept 29/112 while 0/124 and 0/118 went). With it: **240/240**.
The tests in `crates/nocturnal-core/tests/attendance.rs` pin each of those
details.
