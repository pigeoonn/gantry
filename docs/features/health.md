# Game health and crashes

- **The ring** on a game's page is 100 when nothing is wrong. It drops for each problem listed on the Mods tab (a problem
  that blocks the game counts most). Click it to jump to the list.
- **Fix everything** does every safe fix in order. The Steam launch-option fix still asks before it closes Steam.
- **Problems Gantry knows:** a missing library a mod needs, a loader built for the wrong kind of game, a MelonLoader
  first-start failure, a missing Windows runtime in the prefix, Vulkan not starting, a crash, a full disk, anti-cheat... each
  with what to do and the log line it found. You can add your own in `errors.json` next to Gantry's settings.
- **Crash Detective:** after a crash it names the likely cause. *Find the broken mod* turns mods off and on in rounds (a
  bisect) until it finds the one, then puts everything else back.
- **After a game update** Gantry notices that deployed files changed and puts the mods back before the next launch.
- **Check my games** (Ctrl+K) gives every game one verdict: *Ready for mods*, *Needs a mod loader*, *Needs UE4SS*, or *Protected
  by* an anti-cheat. *Set up all* installs the loaders one game at a time.
- **Problem report:** *Tools → Problem Report* copies a report for asking for help; it contains no keys and no home folder.
- **Rollback after an update:** when a game crashes and a mod was updated in the two days before that run, Crash Detective lists it
  (*Updated shortly before this run*) with *Go back to v…*, and the notification bell says so. Before any update the mod page
  shows what changed and asks first; the version you leave stays in the library.
- **Logs:** *Tools → Logs* reads every log of the game in one window (BepInEx, MelonLoader, Unity's player log, UE4SS, the engine,
  Proton): pick one, search it, errors and warnings are coloured, *Copy* takes what is shown. Only logs Gantry found itself are read.
