#!/usr/bin/env python3
"""Build flags/guide.json (what /flags renders) from research/*.json.

research/tier*.json were extracted from Quarm's quest scripts and server
(SecretsOTheP/quests 2843cb4, SecretsOTheP/EQMacEmu 50f4f78) and the Quarm
database, following research/BRIEF.md. This script only normalises them:

- which step wins where two research parts overlap (the part whose zone it is),
- the plane heading each step sits under,
- `done_when` / `alt_done` / `requires` as conditions the page can evaluate
  against a character's `#popflags` output; anything #popflags cannot show
  (zone flags, items, levels, upper bounds) becomes a printed `note`,
- NPC positions as `/loc` prints them (Y, X, Z).

Re-run after re-extracting: python3 build_guide.py
"""
import json
import os
import re

HERE = os.path.dirname(os.path.abspath(__file__))
QUESTS_COMMIT = "2843cb4"
SERVER_COMMIT = "50f4f78"
QGLOBALS = set(
    "mavuin seventh thelin poi_door zeks fuirstel grummus aerindar bertox_key hohtrials "
    "mmarr mmarr_book tylis saryrn karana cipher sol_room zebuxoruk pofire time earthb_key".split()
)
BITSTRINGS = {"hohtrials", "sol_room"}

# Where research parts overlap, the part that owns the zone wins.
PREFER = {
    "fuirstel-3": "tier1",
    "fuirstel-5": "tier1",
    "zeks-2": "tier1",
    "karana-3": "tier2",
    "seer-cl_karana": "tier3",
    "zebuxoruk-2": "tier3",
}

ZONE_PLANE = {
    "pojustice": "Plane of Justice",
    "podisease": "Plane of Disease",
    "ponightmare": "Plane of Nightmare",
    "nightmareb": "Plane of Nightmare",
    "poinnovation": "Plane of Innovation",
    "povalor": "Plane of Valor",
    "postorms": "Plane of Storms",
    "codecay": "Crypt of Decay",
    "potorment": "Plane of Torment",
    "hohonora": "Halls of Honor",
    "hohonorb": "Halls of Honor",
    "bothunder": "Bastion of Thunder",
    "potactics": "Plane of Tactics",
    "solrotower": "Tower of Solusek Ro",
    "pofire": "Elemental Planes",
    "poair": "Elemental Planes",
    "powater": "Elemental Planes",
    "poeartha": "Elemental Planes",
    "poearthb": "Elemental Planes",
    "potimea": "Plane of Time",
    "potimeb": "Plane of Time",
}


def flag_plane(flag, tier):
    f = (flag or "").removeprefix("cl_")
    table = {
        "mavuin": "Plane of Justice",
        "seventh": "Plane of Justice",
        "grummus": "Plane of Disease",
        "thelin": "Plane of Nightmare",
        "maze": "Plane of Nightmare",
        "terris": "Plane of Nightmare",
        "poi_door": "Plane of Innovation",
        "behemoth": "Plane of Innovation",
        "aerindar": "Plane of Valor",
        "bertox_key": "Crypt of Decay",
        "bertox": "Crypt of Decay",
        "tylis": "Plane of Torment",
        "keeper": "Plane of Torment",
        "saryrn": "Plane of Torment",
        "hohtrials": "Halls of Honor",
        "mmarr": "Halls of Honor",
        "mmarr_book": "Halls of Honor",
        "vallon": "Plane of Tactics",
        "tallon": "Plane of Tactics",
        "rallos": "Plane of Tactics",
        "cipher": "Grand Librarian Maelin",
        "zebuxoruk": "Grand Librarian Maelin",
        "sol_room": "Tower of Solusek Ro",
        "pofire": "Tower of Solusek Ro",
        "solusek": "Tower of Solusek Ro",
        "earthb_key": "Elemental Planes",
        "time": "Plane of Time",
    }
    if f == "fuirstel":
        return "Plane of Disease" if tier == 1 else "Crypt of Decay"
    if f == "zeks":
        return "Plane of Innovation" if tier == 1 else "Plane of Tactics"
    if f == "karana":
        return "Plane of Storms" if tier == 2 else "Bastion of Thunder"
    return table.get(f)


# Steps that set no flag of their own are done once what they lead to is.
DONE_BY = {
    "poj-trial-mark": ([("mavuin", ">=", "2")], []),
    "seventh-hammer": ([("seventh", "present", None)], []),
    "maze-enter": ([("thelin", ">=", "2")], [("cl_maze", "present", None)]),
    "tier1-portals": ([("mavuin", ">=", "3"), ("fuirstel", ">=", "5"), ("thelin", ">=", "4")], []),
    "access-valor-storms": ([("mavuin", ">=", "3")], []),
    "askr-bag": ([("karana", ">=", "1")], []),
    "askr-meld": ([("karana", ">=", "2")], []),
    "access-codecay": ([("grummus", "present", None)], []),
    "lower-crypt": ([("fuirstel", ">=", "4")], [("cl_bertox", "present", None)]),
    "access-potorment": ([("fuirstel", ">=", "5"), ("thelin", ">=", "4")], []),
    "keeper-access": ([("tylis", ">=", "2")], [("cl_keeper", "present", None)]),
    "hohonorb-access": ([("mmarr", "present", None)], [("mmarr_book", "present", None), ("cl_mmarr", "present", None)]),
    "agnarr-access": ([("karana", ">=", "4")], [("cl_karana", "present", None)]),
    "drunder-giwin": ([("zeks", ">=", "3")], []),
    "solrotower-access": ([("pofire", ">=", "1")], []),
    "solusek-chamber": ([("pofire", ">=", "2")], [("cl_solusek", "present", None)]),
    "pofire-access": ([("time", "present", None)], []),
    "elemental-access": ([("time", "present", None)], []),
    "poair-xegony-key": ([("time", "present", None)], []),
    "essence-pofire": ([("time", "present", None)], []),
    "essence-powater": ([("time", "present", None)], []),
    "essence-poearthb": ([("time", "present", None)], []),
    "essence-poair": ([("time", "present", None)], []),
    "quintessence": ([("time", "present", None)], []),
    "potime-access": ([("time", "present", None)], []),
    "poearthb-access": ([("time", "present", None)], []),
}


# What each plane's portal or door checks before you can be there at all
# (potranquility/player.lua doors, the solrotower and elemental zone-ins).
# Added to every step in the plane except the access steps themselves and the
# Seer, who stands in the Plane of Knowledge.
PLANE_ACCESS = {
    "Plane of Valor": [("mavuin", ">=", "3", "You finished Mavuin's case in the Plane of Justice, which opens the Plane of Valor.")],
    "Plane of Storms": [("mavuin", ">=", "3", "You finished Mavuin's case in the Plane of Justice, which opens the Plane of Storms.")],
    "Crypt of Decay": [("grummus", "present", None, "You killed Grummus in the Plane of Disease, which opens the Crypt of Decay.")],
    "Plane of Torment": [
        ("fuirstel", ">=", "5", "You finished the Plane of Disease story with the Fuirstel brothers."),
        ("thelin", ">=", "4", "You freed Thelin from his nightmare."),
    ],
    "Halls of Honor": [("aerindar", ">=", "2", "You beat Aerin`Dar in the Plane of Valor and stepped through to the Halls of Honor.")],
    "Bastion of Thunder": [("karana", ">=", "3", "You used the shrine in the Plane of Storms that opens the Bastion of Thunder.")],
    "Plane of Tactics": [("zeks", ">=", "2", "You beat the Manaetic Behemoth in the Plane of Innovation and talked to Giwin afterwards.")],
    "Tower of Solusek Ro": [
        ("cipher", "present", None, "You have the Cipher of the Divine Language from Grand Librarian Maelin."),
        ("zeks", ">=", "6", "You told Maelin about both Vallon and Tallon Zek."),
    ],
    "Elemental Planes": [("zebuxoruk", ">=", "2", "Grand Librarian Maelin has told you to gather the four elemental essences.")],
}
OPEN_STEPS = {
    "access-valor-storms", "access-codecay", "access-potorment", "solrotower-access",
    "elemental-access", "potime-access", "poearthb-access", "pofire-access", "tier1-portals",
}


# Never "do next": side routes (a boss killed before its preflag leaves a
# pending memory instead of the flag) and extras the tiers do not need.
def optional(sid):
    return sid.startswith("cl_") or sid in {"seventh-1", "seventh-hammer", "tier1-portals", "drunder-giwin"}


def cond(flag, op, value=None, why=None):
    c = {"flag": flag, "op": op}
    if value is not None:
        c["value"] = str(value)
    if why:
        c["why"] = why
    return c


def requirement(r):
    """One research requirement → an evaluable condition, or a printed note."""
    flag, value, why = r.get("flag"), str(r.get("value")), r.get("why")
    known = flag in QGLOBALS or (flag or "").startswith("cl_")
    label = why or f"{flag} {value}"
    if not known:
        text = value if flag in (None, "item") else f"{flag}: {value}"
        return cond("note", "note", None, f"{text} ({why})" if why and why not in text else text)
    v = value.strip()
    if v == "absent":
        return cond(flag, "absent", None, label)
    if v == "present":
        return cond(flag, "present", None, label)
    m = re.fullmatch(r">=\s*(\d+)", v)
    if m:
        return cond(flag, ">=", m.group(1), label)
    if flag in BITSTRINGS and re.fullmatch(r"1+", v):
        return [cond(flag, "bit", str(i), label) for i in range(len(v))]
    if re.fullmatch(r"\d+", v) and flag not in BITSTRINGS:
        return cond(flag, ">=", v, label)
    # Upper bounds ("absent or 1", "not 2", "< 6", "bit0 = 0") only say an NPC
    # stops answering once you are past the step; the step is done by then.
    return cond("note", "note", None, f"{flag} {v}: {why}" if why else f"{flag} {v}")


def done_conds(s, seer_targets):
    sid, flag, value = s["id"], s.get("flag"), str(s.get("value"))
    if sid in DONE_BY:
        d, alt = DONE_BY[sid]
        return [cond(*c) for c in d], [cond(*c) for c in alt]
    if flag and flag.startswith("cl_"):
        alt = seer_targets.get(flag)
        return [cond(flag, "present")], ([cond(*alt)] if alt else [])
    m = re.fullmatch(r"bit(\d)", value)
    if m:
        return [cond(flag, "bit", m.group(1))], []
    if re.fullmatch(r"\d+", value):
        return [cond(flag, ">=", value)], []
    return [cond(flag, "present")], []


def loc(n):
    x, y, z = n.get("x"), n.get("y"), n.get("z")
    if x is None or y is None or (x == 0 and y == 0 and (z or 0) == 0):
        return None
    return f"{round(y)}, {round(x)}, {round(z or 0)}"


def source(src):
    src = src.strip()
    for repo in ("EQMacEmu/", "quests/"):
        if src.startswith(repo):
            return src
    if src.startswith(("zone/", "common/", "world/")):
        return "EQMacEmu/" + src
    return "quests/" + src


def main():
    parts = {}
    for p in ("tier1", "tier2", "tier3", "tier45"):
        with open(os.path.join(HERE, "research", f"{p}.json")) as fh:
            parts[p] = json.load(fh)["steps"]
    zones = {}
    with open(os.path.join(HERE, "research", "zones.tsv")) as fh:
        for line in fh:
            short, long_ = line.rstrip("\n").split("\t")
            zones[short] = long_

    # Seer conversions: cl_x → (target flag, >=, value).
    seer_targets = {}
    for steps in parts.values():
        for s in steps:
            if s["id"].startswith("seer-cl_") and s.get("flag"):
                seer_targets[s["id"].removeprefix("seer-")] = (s["flag"], ">=", str(s["value"]))

    out, seen = [], set()
    for p, steps in parts.items():
        for s in steps:
            sid = s["id"]
            if sid in PREFER and PREFER[sid] != p:
                continue
            if sid in seen:
                raise SystemExit(f"duplicate step id {sid} not covered by PREFER")
            seen.add(sid)
            tier = int(s["tier"])
            plane = ZONE_PLANE.get(s.get("zone")) or flag_plane(s.get("flag"), tier)
            if s.get("zone") in ("potranquility", "poknowledge", "poinnovation"):
                plane = flag_plane(s.get("flag"), tier) or ZONE_PLANE.get(s.get("zone")) or plane
            if sid == "tier1-portals" or (not plane and s.get("zone") == "potranquility"):
                plane = "Plane of Tranquility portals"
            if sid == "access-codecay":
                plane = "Crypt of Decay"
            if not plane and sid == "quintessence":
                plane = "Elemental Planes"
            if not plane:
                raise SystemExit(f"no plane for {sid}")
            done, alt = done_conds(s, seer_targets)
            requires = []
            if sid not in OPEN_STEPS and not sid.startswith("seer-"):
                for f, op, v, why in PLANE_ACCESS.get(plane, []):
                    if f != s.get("flag"):
                        requires.append(cond(f, op, v, why))
            for r in s.get("requires", []):
                c = requirement(r)
                requires.extend(c if isinstance(c, list) else [c])
            out.append({
                "id": sid,
                "tier": tier,
                "plane": plane,
                "title": s["title"],
                "optional": optional(sid),
                "done_when": done,
                "alt_done": alt,
                "requires": requires,
                "actions": [
                    {k: v for k, v in {
                        "kind": a.get("type", "do"),
                        "npc": a.get("npc"),
                        "zone": a.get("zone"),
                        "say": a.get("say") or None,
                        "items": [{"id": int(i["id"]), "name": i["name"]} for i in a.get("items", []) if i.get("id")],
                        "detail": a.get("detail", ""),
                    }.items() if v not in (None, [])}
                    for a in s.get("actions", [])
                ],
                "npcs": [
                    {k: v for k, v in {
                        "name": n["name"],
                        "zone": n.get("zone", s.get("zone")),
                        "loc": loc(n),
                        "spawn": n.get("spawn"),
                        "notes": n.get("notes") or None,
                    }.items() if v is not None}
                    for n in s.get("npcs", [])
                ],
                "credit": s.get("credit") or None,
                "message": s.get("message") or None,
                "debug": s.get("debug", []),
                "notes": s.get("notes") or None,
                "source": [source(x) for x in s.get("source", [])],
            })

    # Plain-language text (research/plain_*.json), written from the built
    # steps for members new to the expansion: it replaces every text field,
    # pairing requirements and NPCs by position.
    plain = {}
    for fn in sorted(os.listdir(os.path.join(HERE, "research"))):
        if fn.startswith("plain_") and fn.endswith(".json"):
            with open(os.path.join(HERE, "research", fn)) as fh:
                plain.update(json.load(fh)["steps"])
    for st in out:
        pl = plain.get(st["id"])
        if not pl:
            raise SystemExit(f"no plain text for {st['id']}")
        items = {i["id"]: i for a in st["actions"] for i in a.get("items", [])}
        st["title"] = pl["title"]
        st["why"] = pl.get("why") or None
        st["actions"] = [
            {k: v for k, v in {
                "detail": a["text"],
                "say": a.get("say") or None,
                "items": [items[i] for i in a.get("items", []) if i in items],
            }.items() if v not in (None, [])}
            for a in pl["actions"]
        ]
        if len(pl["npcs"]) != len(st["npcs"]) or len(pl["needs"]) != len(st["requires"]):
            raise SystemExit(f"plain text for {st['id']} does not line up")
        for n, pn in zip(st["npcs"], pl["npcs"]):
            n["name"] = pn["name"]
            n.pop("notes", None)
            if pn.get("where"):
                n["notes"] = pn["where"]
            n.pop("spawn", None)
        last = None
        for c, need in zip(st["requires"], pl["needs"]):
            if need == last:
                c.pop("why", None)  # one sentence for an expanded bit string
            else:
                c["why"] = need
            last = need
        st["message"] = pl.get("works_when") or None
        st["credit"] = pl.get("credit") or None
        st["debug"] = pl.get("if_it_failed", [])
        st["notes"] = pl.get("tips") or None

    # Tier order, then the research order within a tier.
    out.sort(key=lambda st: st["tier"])
    guide = {
        "quests_commit": QUESTS_COMMIT,
        "server_commit": SERVER_COMMIT,
        "zones": zones,
        "steps": [{k: v for k, v in st.items() if v is not None} for st in out],
    }
    with open(os.path.join(HERE, "guide.json"), "w") as fh:
        json.dump(guide, fh, indent=1, ensure_ascii=False)
        fh.write("\n")
    print(f"{len(out)} steps written")


if __name__ == "__main__":
    main()
