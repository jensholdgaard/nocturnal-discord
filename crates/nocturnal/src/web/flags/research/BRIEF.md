# Brief: PoP flag steps from Quarm source

You are extracting the Planes of Power character-flag progression for Project Quarm (TAKP/EQMac emulator), so a guild web page can tell each player exactly what to do next and how to debug a step that didn't work. Accuracy matters more than prose: every fact must come from the source below, never from memory of live EverQuest or wikis. Where the source is ambiguous, say so in `notes` instead of guessing.

## Sources (read-only)
- Quest scripts (Lua, Quarm's live repo): `~/quarm-src/quests/<zone_short_name>/...` (NPC files are `Name_With_Underscores.lua`; `#` prefix = hidden/controller NPC; `encounters/` hold event logic; `global/` and `lua_modules/` hold shared code).
- Server source: `~/quarm-src/EQMacEmu/` (C++). `zone/gm_commands/popflags.cpp` is the player command `#popflags` whose stage texts define the canonical stages. `zone/lua_*` define what Lua calls do.
- Seer: `~/quarm-src/quests/poknowledge/Seer_Mal_Nae-Shi.lua` documents every flag (header comment) and the checklist ("cl_*") memory mechanism.
- Local Quarm database (MariaDB, read-only queries only):
  `podman exec eqmac-maria mariadb -uroot -p<password> alkabor -B -e "<SQL>"`
  Tables: `npc_types(id,name,level,...)`, `spawn2(id,spawngroupID,zone,x,y,z,heading,respawntime)`, `spawnentry(spawngroupID,npcID,chance)`, `items(id,Name)`, `zone(short_name,zoneidnumber,long_name)`. NPC ids are zoneidnumber*1000+n. Item names: `select id,Name from items where id in (...)`.
  NPC location: `select n.name,s.zone,s.x,s.y,s.z from npc_types n join spawnentry se on se.npcID=n.id join spawn2 s on s.spawngroupID=se.spawngroupID where n.name like 'Seer_Mal%';`
  Event-spawned NPCs have no spawn2 row: find the `eq.spawn2(npc_id, grid, 0, x, y, z, h)` call in Lua and report those coordinates with spawn="event".

## The flags (qglobals, per character)
mavuin seventh thelin poi_door zeks fuirstel grummus aerindar bertox_key hohtrials(3-char bitstring Rydda`Dar,Village,Nomad) mmarr mmarr_book tylis saryrn karana cipher sol_room(5-char bitstring Xuzl,Arlyxir,Dresolik,Rizlona,Jiva) zebuxoruk pofire time earthb_key, plus checklist flags cl_* that the Seer converts ("unlock memories").

## What to do
For EVERY place in your assigned zones that sets one of these flags (`eq.set_global("<flag>", ...)`, including cl_* checklist flags and computed values), plus every NPC interaction the player must perform to reach it (preflag hails, item hand-ins, keyword says, event triggers), produce one step object. Follow the logic: read the surrounding event_say/event_trade/event_death/event_signal/event_timer code, the conditions (qglobals checks, items, raid/group checks, distance checks, zone checks, level checks, timers), and every message the player sees.

Write ONE JSON file: `~/quarm-src/flagguide/<PART>.json` = `{"part": "<PART>", "steps": [ ... ]}` where each step is:

```json
{
  "id": "fuirstel-1",                       // flag-value, or a short slug for a non-flag prerequisite step
  "flag": "fuirstel", "value": "1",          // value after this step; for bitstrings say e.g. "bit0" ; null for pure prerequisite steps
  "tier": 1,
  "zone": "potranquility",                   // where the player performs the step
  "title": "Get the Ward request from Milyk", // short, player-facing
  "requires": [ {"flag": "fuirstel", "value": "absent", "why": "script only grants when no fuirstel global"} ],
  "actions": [                               // in order
    {"type": "say|give|kill|event|click|loot|zone|seer", "npc": "Elder_Fuirstel", "zone": "potranquility",
     "say": "exact keyword the script matches (findi text)", "items": [{"id": 12345, "name": "..."}],
     "detail": "one plain sentence of what to do"}
  ],
  "npcs": [ {"name": "Elder_Fuirstel", "zone": "potranquility", "x": 0, "y": 0, "z": 0, "spawn": "static|event|roam", "notes": ""} ],
  "credit": "who receives the flag: the hand-in player / every client in the zone / raid members within N units / anyone on the hate list ... (quote the loop/condition)",
  "message": "exact text the player sees when it works (usually 'You have received a character flag!' plus any purple/emote text)",
  "debug": [ "concrete check derived from a condition in the code, e.g. 'Milyk only answers 'ward' if you do NOT already have fuirstel'", "..." ],
  "notes": "ambiguities, era/expansion gates, timers, anything a player could trip on",
  "source": ["poknowledge/Seer_Mal_Nae-Shi.lua:140", "..."]
}
```

Rules:
- Quote `say` keywords exactly as the script matches them (the `findi(...)` argument). If several keywords chain, make one action per say.
- Coordinates: give raw DB/Lua x,y,z numbers; do not convert.
- `debug` entries must each trace to a specific condition, timer, distance, group/raid check, item check, or flag check in the code. Include "what you see if it failed" when the script prints something on the failure path.
- Cover the Seer's "unlock memories" conversion for every cl_* flag your zones set (which flag/value it needs first).
- Skip GM/staff-only paths (Admin() checks), but note them if they could confuse a player.
- Keep each string short and factual. No markdown in strings.
- When done, reply with: the file path, the step count, and a list of anything you could not resolve.
