#!/usr/bin/env python3
"""Generate games.json from NVIDIA's official RTX games list.

Source of truth
---------------
NVIDIA publishes its RTX games/apps table as JSON, fetched by the page's own
front-end script. The rendered HTML contains no table rows and the .md variant
strips the table entirely, so this is the only machine-readable form:

  page:  https://www.nvidia.com/en-us/geforce/news/nvidia-rtx-games-engines-apps
  data:  .../nvidia-rtx-games-engines-apps/dlss-rt-games-apps-overrides.json

We keep only rows where the "dlss frame generation" column is non-empty, i.e.
the game actually ships Frame Generation. That answers "does this game have
Frame Generation", which is the prerequisite the mod needs. It says nothing
about whether the patch will work -- that also depends on which proxy DLL the
game loads, which no public list covers.

Steam AppIDs are resolved so the runtime can match on appid (exact) rather than
folder name (fuzzy). Every entry keeps its display name as a fallback key, so a
missing appid degrades instead of failing.

Usage
-----
  python tools/gen_games_json.py                # uses tools/.cache
  python tools/gen_games_json.py --refresh      # ignore cached downloads
  python tools/gen_games_json.py --limit 25     # spot-check
"""

import argparse
import difflib
import hashlib
import json
import os
import re
import sys
import time
import unicodedata
import urllib.error
import urllib.parse
import urllib.request

NVIDIA_PAGE = "https://www.nvidia.com/en-us/geforce/news/nvidia-rtx-games-engines-apps"
NVIDIA_DATA = (
    "https://www.nvidia.com/content/dam/en-zz/Solutions/geforce/news/"
    "nvidia-rtx-games-engines-apps/dlss-rt-games-apps-overrides.json"
)
NVIDIA_UPDATED = "2026-07-28"

# The endpoint the storefront itself uses. Better recall than the documented
# storesearch API, which returns nothing at all for some titles.
STEAM_UI_SEARCH = (
    "https://store.steampowered.com/search/results/?query&term={term}"
    "&start=0&count=25&dynamic_data=&sort_by=_ASC&snr=1_7_7_230_7&infinite=1"
)
STEAM_STORE_SEARCH = "https://store.steampowered.com/api/storesearch/?cc=US&l=en&v=1&term="

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
CACHE = os.path.join(HERE, ".cache")

# Store results that are never the base game. Single words must match a whole
# token: substring matching would reject "Contest" (test) and "Compass" (pass).
NOISE_WORDS = frozenset((
    "soundtrack", "dlc", "demo", "playtest", "benchmark", "artbook",
    "upgrade", "pass", "expansion", "test",
))
NOISE_PHRASES = (
    "digital deluxe", "digital extra", "deluxe edition upgrade",
    "bonus content", "gameplay pack", "game pack",
)

# Words that may differ between a title and its store entry without making them
# different games: "Hellblade II Enhanced" and "Hellblade II" are one product.
# Anything outside this set means the titles genuinely differ, so matching them
# would be guessing -- e.g. "Sim Racing Telemetry - F1 22" must never match
# "F1 22".
QUALIFIERS = frozenset((
    "enhanced", "remastered", "remaster", "edition", "definitive",
    "complete", "ultimate", "deluxe", "goty", "game", "of", "the",
    "plus", "final", "anthology", "collection", "trilogy",
))

FUZZY_CUTOFF = 0.90

# Steam rate-limits bursts hard, so pace real requests.
THROTTLE_SECONDS = 1.60
_last_request = [0.0]
_refresh_cache = [False]


def throttle():
    gap = time.time() - _last_request[0]
    if gap < THROTTLE_SECONDS:
        time.sleep(THROTTLE_SECONDS - gap)
    _last_request[0] = time.time()


def cache_path_for(kind, term):
    digest = hashlib.sha1(term.lower().encode("utf-8")).hexdigest()[:16]
    return os.path.join(CACHE, kind, digest + ".json")


def fetch(url, cache_path):
    """Return decoded text, or None if unobtainable. Cached on success only."""
    if not _refresh_cache[0] and os.path.exists(cache_path):
        with open(cache_path, "r", encoding="utf-8") as f:
            return f.read()

    req = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0"})
    for attempt in range(4):
        try:
            throttle()
            with urllib.request.urlopen(req, timeout=30) as resp:
                raw = resp.read()
            break
        except urllib.error.HTTPError as e:
            if e.code == 429 and attempt < 3:
                wait = 45 * (attempt + 1)
                print("  429 rate limited, sleeping %ds" % wait, file=sys.stderr)
                time.sleep(wait)
                continue
            print("  giving up (%s) on %s" % (e, url[:80]), file=sys.stderr)
            return None
        except (urllib.error.URLError, TimeoutError, OSError) as e:
            if attempt == 3:
                print("  giving up on %s: %s" % (url[:80], e), file=sys.stderr)
                return None
            time.sleep(2 * (attempt + 1))

    os.makedirs(os.path.dirname(cache_path), exist_ok=True)
    with open(cache_path, "wb") as f:
        f.write(raw)
    return raw.decode("utf-8", "replace")


def norm(s):
    """Fold a title to a comparable key.

    The store returns U+FFFD where en/em dashes belong, so those become plain
    separators rather than poisoning the comparison.
    """
    s = (s or "").lower().replace("\ufffd", " ")
    s = unicodedata.normalize("NFKD", s)
    s = "".join(c for c in s if not unicodedata.combining(c))
    s = re.sub(r"[^a-z0-9]+", " ", s)
    return re.sub(r"\s+", " ", s).strip()


def is_noise(norm_name):
    if any(p in norm_name for p in NOISE_PHRASES):
        return True
    return any(t in NOISE_WORDS for t in norm_name.split())


def clean_title(name):
    """Tidy NVIDIA's own encoding artefacts for display."""
    name = str(name).replace("\ufffd", " -")
    name = re.sub(r"\s+", " ", name)
    return name.strip(" -").strip()


def nvidia_fg_games():
    raw = fetch(NVIDIA_DATA, os.path.join(CACHE, "nvidia.json"))
    if raw is None:
        raise SystemExit("could not fetch the NVIDIA list; delete the cache to retry")

    rows = json.loads(raw)["data"]
    out = []
    for r in rows:
        if r.get("type") != "Game":
            continue
        fg = (r.get("dlss frame generation") or "").strip()
        if not fg:
            continue
        name = clean_title(r.get("name"))
        if not name:
            continue
        out.append({
            "name": name,
            # "Yes" is native Frame Generation. "NV, U" is native Frame
            # Generation that the NVIDIA app can upgrade to a newer model.
            # Both mean the game ships the feature.
            "fg": "override" if "U" in fg else "yes",
        })
    out.sort(key=lambda g: g["name"].lower())
    return out


def steam_search(term):
    """Search the Steam store. Returns a list of (appid, store_title)."""
    url = STEAM_UI_SEARCH.format(term=urllib.parse.quote(term))
    body = fetch(url, cache_path_for("ui", term))

    pairs = []
    if body:
        try:
            html = json.loads(body).get("results_html") or ""
        except ValueError:
            html = ""
        ids = re.findall(r'data-ds-appid="(\d+)"', html)
        titles = [re.sub(r"<[^>]+>", "", t)
                  for t in re.findall(r'<span class="title">(.*?)</span>', html)]
        for appid, title in zip(ids, titles):
            if appid.isdigit() and int(appid) > 0:
                pairs.append((int(appid), title))

    if pairs:
        return pairs

    # Fallback for when the storefront endpoint yields nothing.
    body = fetch(STEAM_STORE_SEARCH + urllib.parse.quote(term), cache_path_for("api", term))
    if not body:
        return []
    try:
        items = json.loads(body).get("items", [])
    except ValueError:
        return []
    return [(int(it["id"]), it["name"]) for it in items if it.get("id") and it.get("name")]


def resolve_appid(name):
    """Return (appid, confidence) for a display name."""
    target = norm(name)
    cands = []
    for appid, title in steam_search(name):
        key = norm(title)
        if key and not is_noise(key) and (appid, key) not in cands:
            cands.append((appid, key))
    if not cands:
        return None, "none"

    # Prefer an exact match anywhere in the list, so an early loose hit cannot
    # shadow a later exact one.
    for appid, key in cands:
        if key == target:
            return appid, "exact"

    for appid, key in cands:
        want, have = set(target.split()), set(key.split())
        if not want or not have:
            continue
        differing = (have - want) | (want - have)
        if differing and differing <= QUALIFIERS:
            return appid, "qualifier"

    best, score = None, 0.0
    for appid, key in cands:
        ratio = difflib.SequenceMatcher(None, target, key).ratio()
        if key.startswith(target + " ") or target.startswith(key + " "):
            extra = (set(key.split()) ^ set(target.split()))
            if extra and extra <= QUALIFIERS:
                ratio = max(ratio, 0.95)
        if ratio > score:
            best, score = appid, ratio

    if best is not None and score >= FUZZY_CUTOFF:
        return best, "fuzzy"
    return None, "none"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=os.path.join(ROOT, "games.json"))
    ap.add_argument("--refresh", action="store_true", help="ignore cached downloads")
    ap.add_argument("--limit", type=int, default=0, help="only resolve the first N")
    args = ap.parse_args()
    _refresh_cache[0] = bool(args.refresh)

    games = nvidia_fg_games()
    if args.limit:
        games = games[: args.limit]
    print("NVIDIA lists %d games with Frame Generation" % len(games))

    seen = {}
    out = []
    stats = {"exact": 0, "qualifier": 0, "fuzzy": 0, "none": 0}
    unresolved = []

    for i, g in enumerate(games, 1):
        appid, conf = resolve_appid(g["name"])
        stats[conf] += 1
        entry = {"name": g["name"], "fg": g["fg"]}

        if appid:
            # Several NVIDIA rows can point at one Steam app (a base title and
            # its remaster, say). Keep the first; duplicates carry the same
            # Frame Generation answer, so nothing is lost.
            if appid in seen:
                print("  dup   %s -> %s (already used by %s)"
                      % (g["name"], appid, seen[appid]))
                continue
            seen[appid] = g["name"]
            entry["steam_appid"] = appid
            entry["match"] = conf
        else:
            unresolved.append(g["name"])

        out.append(entry)
        if i % 50 == 0:
            print("  %d/%d" % (i, len(games)))

    doc = {
        "$comment": (
            "Games with NVIDIA DLSS Frame Generation. Generated by "
            "tools/gen_games_json.py -- do not edit by hand. The presence of "
            "Frame Generation says the game ships the feature; it does not "
            "promise the mod will work, which also depends on which proxy DLL "
            "the game loads."
        ),
        "source": "NVIDIA GeForce RTX Games & Apps List",
        "source_page": NVIDIA_PAGE,
        "source_data": NVIDIA_DATA,
        "source_updated": NVIDIA_UPDATED,
        "match_notes": (
            "steam_appid confidence: 'exact' = store name matches the NVIDIA "
            "name; 'qualifier' = differs only by words like enhanced/remastered; "
            "'fuzzy' = within FUZZY_CUTOFF. Entries without steam_appid fall back "
            "to matching on the install folder name."
        ),
        "games": out,
    }

    with open(args.out, "w", encoding="utf-8") as f:
        json.dump(doc, f, indent=2, ensure_ascii=False)
        f.write("\n")

    print("\nwrote %s" % args.out)
    print("  exact=%(exact)d qualifier=%(qualifier)d fuzzy=%(fuzzy)d unresolved=%(none)d" % stats)
    if unresolved:
        print("\nno appid (%d) - these rely on folder-name matching:" % len(unresolved))
        for name in unresolved:
            print("  - %s" % name)


if __name__ == "__main__":
    main()