#!/usr/bin/env python3
"""Adds games to data/recipes.json from two public lists, only where both agree.

    python3 tools/build_recipes.py [--write] [--min-mods 250] [--work DIR]

* Nexus Mods' own list of games (domain names, how many mods each has). Needs your Nexus API key in
  ~/.config/protonrig/nexus_api_key (or NEXUS_API_KEY_FILE); it is only used for that one request and never printed.
* The Ludusavi manifest (https://github.com/mtkennerly/ludusavi-manifest), which knows each game's Steam id and where
  it keeps its saves.

A game is added when its name is the same in both lists (letters and digits only, one match), it is not in
recipes.json yet, and Nexus has at least --min-mods mods for it. Its save places are the Windows paths of the manifest,
turned into Gantry's starting points ({documents}, {appdata_local}, {appdata_locallow}, {appdata_roaming},
{saved_games}, {game}); anything that cannot be a plain folder is left out. Existing entries are never changed, except
that a missing `saves` list is filled in. Without --write it only prints what it would do.
"""
import argparse
import json
import os
import re
import subprocess
import sys
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MANIFEST = "https://raw.githubusercontent.com/mtkennerly/ludusavi-manifest/master/data/manifest.yaml"
BASES = {
    "<winDocuments>": "{documents}",
    "<winLocalAppData>": "{appdata_local}",
    "<winAppData>": "{appdata_roaming}",
    "<base>": "{game}",
}
# games Gantry should not offer mods for: online games with anti-cheat, and games whose mods live outside the game folder
# (Gantry installs into the game folder, so those would land in the wrong place)
SKIP_NAMES = {"fortnite", "valorant", "counterstrike2", "apexlegends", "rainbowsixsiege", "pubgbattlegrounds", "rust"}
SKIP_DOMAINS = {
    "marvelrivals", "fallout76", "helldivers2", "halothemasterchiefcollection", "warthunder", "yugiohmasterduel", "easportsfc26",
    "mortalkombat", "tekken8", "streetfighter6", "forzahorizon6", "dragonageorigins",
    "baldursgate3", "thesims4", "dragonage", "thesims3", "thesims2", "dragonagetheveilguard",
}


def norm(s):
    return re.sub(r"[^a-z0-9]", "", s.lower().replace("&", "and"))


def nexus_games():
    keyfile = Path(os.environ.get("NEXUS_API_KEY_FILE", Path.home() / ".config/protonrig/nexus_api_key"))
    key = keyfile.read_text().strip()
    config = 'url = "https://api.nexusmods.com/v1/games.json"\nheader = "apikey: %s"\nheader = "User-Agent: Gantry-tools"\n' % key
    out = subprocess.run(["curl", "-sS", "--max-time", "60", "-K", "-"], input=config, capture_output=True, text=True).stdout
    return [{"d": g["domain_name"], "n": g["name"], "mods": g.get("mods", 0)} for g in json.loads(out)]


def save_places(entry):
    """Windows save folders of a manifest entry as Gantry starting points, at most 4, plain folders only."""
    out = []
    for path, info in (entry.get("files") or {}).items():
        if "save" not in info.get("tags", []):
            continue
        when = info.get("when") or [{}]
        if not any(w.get("os") in (None, "windows") and w.get("store") in (None, "steam") for w in when):
            continue
        p = path.replace("\\", "/")
        if p.startswith("<home>/"):
            rest = p[len("<home>/"):]
            for prefix, token in (("AppData/LocalLow/", "{appdata_locallow}/"), ("Saved Games/", "{saved_games}/"), ("Documents/", "{documents}/"), ("AppData/Roaming/", "{appdata_roaming}/"), ("AppData/Local/", "{appdata_local}/")):
                if rest.startswith(prefix):
                    p = token + rest[len(prefix):]
                    break
            else:
                continue
        else:
            for k, v in BASES.items():
                if p.startswith(k + "/"):
                    p = v + p[len(k):]
                    break
            else:
                continue
        # cut at the first part that is a pattern or a per-account folder: the folder above it is the save place
        parts = []
        for seg in p.split("/"):
            if "<" in seg or "*" in seg or "?" in seg or "[" in seg:
                break
            parts.append(seg)
        p = "/".join(parts)
        # a file is not a folder: drop a trailing name that has an extension
        if re.search(r"\.[A-Za-z0-9]{1,5}$", p.split("/")[-1]):
            p = "/".join(p.split("/")[:-1])
        if p.count("/") < 1 or ".." in p or ":" in p or len(p) > 150:
            continue
        # the base alone would be the whole Documents folder or the whole game: too much
        if p in ("{documents}", "{game}", "{appdata_local}", "{appdata_roaming}", "{appdata_locallow}", "{saved_games}"):
            continue
        if p not in out:
            out.append(p)
    # a folder that holds another listed folder is enough on its own
    out = [p for p in out if not any(q != p and p.startswith(q + "/") for q in out)]
    return out[:4]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--write", action="store_true")
    ap.add_argument("--min-mods", type=int, default=250)
    ap.add_argument("--work", default=str(Path(os.environ.get("TMPDIR", "/tmp")) / "gantry-recipes-work"))
    a = ap.parse_args()
    import yaml  # PyYAML

    work = Path(a.work)
    work.mkdir(parents=True, exist_ok=True)
    mf = work / "manifest.yaml"
    if not mf.exists():
        print("downloading the Ludusavi manifest ...", file=sys.stderr)
        urllib.request.urlretrieve(MANIFEST, mf)
    manifest = yaml.safe_load(mf.read_text())
    by = {}
    for name, e in manifest.items():
        sid = (e.get("steam") or {}).get("id")
        if sid:
            by.setdefault(norm(name), []).append((name, str(sid), e))

    path = ROOT / "data" / "recipes.json"
    data = json.loads(path.read_text())
    games = data["games"]
    have = {g["appid"]: g for g in games}
    known_domains = {g.get("nexus") for g in games if g.get("nexus")}

    added, filled = [], []
    for g in sorted(nexus_games(), key=lambda g: -g["mods"]):
        if g["mods"] < a.min_mods or norm(g["n"]) in SKIP_NAMES or g["d"] in SKIP_DOMAINS:
            continue
        c = by.get(norm(g["n"]))
        if not c or len(c) != 1:
            continue
        name, sid, e = c[0]
        saves = save_places(e)
        if sid in have:
            if saves and not have[sid].get("saves"):
                have[sid]["saves"] = saves
                filled.append(have[sid]["name"])
            continue
        if g["d"] in known_domains:
            continue
        entry = {"appid": sid, "name": name, "nexus": g["d"]}
        if saves:
            entry["saves"] = saves
        games.append(entry)
        have[sid] = entry
        known_domains.add(g["d"])
        added.append((g["mods"], name, g["d"], saves))
    for mods, name, domain, saves in added:
        print(f"+ {name} [{domain}] {mods} mods  saves: {saves}")
    print(f"{len(added)} games added, {len(filled)} got save places: {filled}", file=sys.stderr)
    if a.write:
        path.write_text(json.dumps(data, indent=2, ensure_ascii=False) + "\n")
        print("written", path, file=sys.stderr)


if __name__ == "__main__":
    main()
