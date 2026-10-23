# What Steam does well, and what Gantry can learn from it

A look at how Steam handles the same problems Gantry has (games, updates, saves, background work), taken from its
**own files and logs on a Linux install** (read-only; nothing was changed, no account data copied) and from its public
documentation. The Steam program itself was not taken apart: its subscriber agreement forbids that, and everything
below can be seen from outside.

## What was observed

| Area | What Steam does | Where it shows |
|---|---|---|
| Library | One `libraryfolders.vdf` lists every library with a label, total size and the apps in it; each game has an `appmanifest_<id>.acf`. | `steamapps/` |
| Updates | An explicit state machine per game: *Update Required → Queued → Running → Reconfiguring → Verifying Installed*, with byte counters for downloading **and** staging, and a "validate after the next update" flag. | `content_log.txt`, `appmanifest` fields `StateFlags`, `BytesToDownload`, `BytesToStage`, `FullValidateAfterNextUpdate` |
| Update policy per game | `AutoUpdateBehavior` (always / on launch / high priority), `AllowOtherDownloadsWhileRunning`, `ScheduledAutoUpdate`. | `appmanifest` |
| Running games | Every game starts inside a small wrapper (`reaper`); each process it spawns is logged as "tracked", so a launcher that starts the real game and exits does not make Steam think the game stopped. | `gameprocess_log.txt` |
| Cloud saves | A per-game cache lists every synced file with its hash, local time, remote time and sync state; conflicts become one question ("which copy?") at launch. | `userdata/<account>/<app>/remotecache.vdf`, `cloud_log.txt` |
| Logs | Every log has a size cap and keeps exactly one `.previous` copy (512 KB, 1 MB, 4 MB, 8 MB), so nothing grows forever. | `logs/` |
| Shaders | Compiled in the background and kept per game; the biggest folder in the install (1.9 GB here). | `steamapps/shadercache`, `shader_log.txt` |
| Interface memory | Window layouts are remembered per screen size. | `config/DialogConfigOverlay_<w>x<h>.vdf` |
| Per-game extras | Screenshots, game recordings and Workshop items live in their own folders per game. | `userdata/<account>/`, `steamapps/workshop` |

## What was already the same in Gantry
Library detection from `libraryfolders.vdf`, playtime from `LastPlayed`, shader-cache clean-up, a download manager, per-mod
update rules, a journal with undo for every deploy, bounded activity (newest 300) and session (newest 5000) logs. Game
updates are noticed afterwards, by deployed files that changed under Gantry (drift), not from Steam's own record.

## Ideas, best first
Value and effort are a judgement; every item is something Steam already proved people use.

1. **Steam Cloud heads-up (done in this round).** When Steam Cloud syncs a game, switching a profile's saves can start a
   "which copy?" question or have Steam put its own copy back. Gantry now says so next to *Each profile keeps its own saves*
   (it only looks for the cache file; it never reads or changes it).
2. **Know when Steam is updating a game (done).** The game's manifest says so (`StateFlags`, `BytesToDownload`, `TargetBuildID`
   against `buildid`). Gantry could hold back a deploy or a launch with mods while an update is running, and say "Steam is
   updating this game" instead of discovering the damage afterwards. Built: the game page says so, and Play refuses while
   Steam's own record shows an update running. Read-only.
3. **Workshop games (done).** For games whose mods come from the Steam Workshop (RimWorld, Cities: Skylines, Project Zomboid,
   Garry's Mod ...), list the subscribed items found under `steamapps/workshop/content/<app>` next to the mods, with title
   and picture from Steam's public item lookup. Built as *Tools → Workshop*, read-only; managing them stays Steam's job.
4. **A "what's new" shelf (done, as trending).** Steam's home page opens on what changed. Gantry could open on: updates for your mods, and new
   popular mods for the games you play since your last visit. Built as *Trending for your games* on the overview: the most
   downloaded recently changed mods of the three games played last, minus what you already have.
5. **Download limits (done).** Steam can cap speed and pause downloads while a game runs. Gantry downloads with curl, so a speed
   limit and "wait while a game is running" are small. Built in Settings: a cap per download, and new downloads wait for the
   game to close (one that already started is left alone).
6. **Smart collections.** "Games with updates", "games with conflicts", "games without a backup" as one-click filters on the
   game list. *Value medium, effort low.*
7. **Screenshots per profile.** Show a game's screenshots next to the profile they were taken under, for before/after
   comparisons of mods. Read-only from the screenshot folder. *Value medium, effort medium.*
8. **Sync through a folder you choose.** Cloud-style sync without a server: mirror profiles, modpacks, themes and settings
   into a folder that Syncthing, Dropbox or a USB stick already syncs. *Value medium, effort medium.*
9. **Update states in the download panel.** Show *Queued → Downloading → Checking → Deploying* the way Steam names its steps,
   instead of one progress bar. *Value low, effort low.*
10. **Process tracking by environment.** Steam and Proton put `SteamGameId` into the environment of every process of a game;
   Gantry finds games by executable name. Matching the variable would also find games whose executable is unknown.
   Not built: Wine helper processes such as `wineserver` carry the same variable and may outlive the game, so this needs a
   test with a real running game first.
11. **Remember window size per screen size.** Cosmetic.

## What not to copy
The storefront, the in-game overlay (injecting into games is exactly what anti-cheat dislikes), forced updates, and
anything that sends data away by default.
