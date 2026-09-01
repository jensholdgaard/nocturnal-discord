# The feedback channel, mirrored into Ourios

Members post feedback in one Discord channel. The bot mirrors every message
there into Ourios as a `discord.feedback` log record — the same pipeline and
tenant as its own logs and the members' `everquest.character.profile`
events — so feedback can be read, searched and (eventually) shown on the
site without anyone scrolling Discord history or a second bot in the guild.

## Turning it on

1. **Developer portal → the bot's application → Bot → Privileged Gateway
   Intents → Message Content Intent: ON.** Without it Discord hands the bot
   empty message bodies, and — worse — a bot that *requests* the intent
   without the toggle is refused by the gateway and stays down. This is why
   the intent is requested only when step 2 is set.
2. In `nocturnal.yaml`:
   ```yaml
   discord:
     feedback_channel_id: 1544068940349579404
   ```
   Deploy the binary that knows the key **before** the config carrying it
   (`deny_unknown_fields`: a rollback binary would otherwise crash-loop).
3. The bot needs *View Channel* and *Read Message History* on that channel.

## What is recorded

| | |
|---|---|
| body | the message text (edits re-emit; newest record for an id wins) |
| `nocturnal.feedback.message_id` | snowflake |
| `nocturnal.feedback.kind` | `posted` (live) · `edited` · `backfill` (read from history at boot) |
| `nocturnal.feedback.posted_ms` | Discord's timestamp of the message |
| `nocturnal.feedback.reply_to` | the message it answers, or empty |
| `nocturnal.feedback.attachments` | count only; bytes are not mirrored |
| `nocturnal.discord.user.name` / `.id`, `nocturnal.discord.channel.id` | who and where |

Bots (this one included) are never mirrored. At boot the channel is read
back to the last mirrored id (`<data>/feedback.cursor`), capped at 2000
messages, so a fresh deploy backfills the whole channel and a restart loses
nothing. Attachments stay on Discord.

## Reading it back

```
event_name == "discord.feedback" | range(-30d, now) | limit 500
```

against the Ourios query endpoint with `x-ourios-tenant: nocturnal`.
