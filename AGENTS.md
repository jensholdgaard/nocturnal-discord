# AGENTS.md

Nocturnal: a Discord DKP/raid bot for Project Quarm / TAKP, split into a
no-event-sourcing core (`nocturnal-core`: projections + commands), a WAL/Parquet
store, telemetry, provisioning, migration tooling, and the Discord bot itself
(`nocturnal`). Rust 2021, edition-2021 workspace (rust-version 1.80).

## Workflow notes

- Source layout: `crates/nocturnal` is the bot (poise + serenity via
  `poise::serenity_prelude`), `crates/nocturnal-core` holds the ledger
  projections and commands, `crates/nocturnal-store` is persistence.
- The ledger is an append-only event log replayed into projections
  (`nocturnal-core/src/state.rs`). Mutations go through `Command`s executed on
  the `DriverHandle`; reads are `driver.query(...)` closures over the
  projection. Do not hand-roll a second copy of a rule the ledger already
  projects — add it to the projection and query it.
- Code carries its own rationale in comments (audit ids, hazards, legacy
  behaviours that are deliberately kept). Preserve that voice: when you change
  behaviour, update or extend the nearby comment rather than deleting history.
- Tests live beside the code and in `crates/*/tests/`. Run
  `cargo test -p nocturnal-core -p nocturnal` and `cargo clippy --workspace`
  before finishing. No lints beyond the workspace defaults
  (`unsafe_code = forbid`, `clippy::unwrap_used = warn`).

## Auction embeds (`crates/nocturnal/src/auctions.rs`)

- Buttons are stateless: custom ids are `nb:<action>:<auction id>`, and
  handling is a pure function of (custom id, ledger state). No in-memory
  collector must survive for an auction to work; open auctions re-post on boot.
- Every auction embed (live short/long, closed recap, settled recap) ends with
  an "Item history" toggle. The button's custom id encodes the current
  presentation state (`histon` expands, `histoff` collapses), and a click
  re-renders the clicked message from the ledger — never from memory. The
  expanded field lists the last 3 finalized auctions won for the item
  (`GuildState::recent_won_auctions`, which excludes the auction being bid
  on). `refresh` re-renders collapsed after every ledger change; that is
  intended and stateless.
- Discord has no native accordion; the toggle approach is deliberate and the
  renderers all share `build_post` so expand/collapse/refresh produce
  byte-identical posts for the same ledger + history state. Keep it that way
  when changing embed layout.
