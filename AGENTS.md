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
  collector must survive for an auction to work. On boot, open auctions adopt
  their existing post (found by its buttons) and only re-post when none is found.
- Every auction embed (live short/long, closed recap, settled recap; not roll
  auctions) shows an "Item history" field: the last 3 finalized auctions won
  for the item (`GuildState::recent_won_auctions`, which excludes the auction
  being bid on), looked up from the ledger on every post and edit — never
  from memory.
- Until 2026-10-22 the field hid behind a `histon`/`histoff` toggle button.
  Members kept flipping it, so it is always shown now (#9) and the button is
  gone. Old posts still carry it: the ids still parse, and a click re-renders
  that post from the ledger, which drops the button. Keep parsing them.
- The renderers all share `build_post`, so a post, a `refresh` and a click on
  an old button produce byte-identical posts for the same ledger. Keep it
  that way when changing embed layout.
