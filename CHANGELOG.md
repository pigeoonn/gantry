# Changes

## 5.1.0

A big release: a store for mods, modpacks, a game page that looks finished, and much more care for the things that
can go wrong. The ten interface languages are English, Romanian, Italian, French, Spanish, Russian, German,
Portuguese, Polish and Turkish.

### Finding and installing mods
- A store inside Gantry for Nexus Mods and Thunderstore: sorted by what people download most, by newest, by
  endorsements, with categories, infinite scrolling, and adult mods hidden unless you switch them on.
- Starter packs: hand-picked mods for the games people actually play, with the reason each one is there.
- Install from anywhere: from the store, the mod page or a link, with a preview of where the files will go.
- Mod pages with galleries, changes, requirements and instructions.
- Modpacks: one `.gantrypack` file with your mods inside, and import of Thunderstore / r2modman profile codes.

### Playing
- Play without mods, play with a saved profile, profiles that keep their own saves.
- A health score per game and one *Fix everything* button; the first-run welcome finds every game.
- Crash Detective and automatic bisect find the broken mod; the crash knowledge base explains what happened.
- *Why is it slow?* shows memory and processor use of each session (Linux), MangoHud frame times when a log exists,
  modded against plain sessions, and the mods most likely to cost something.
- Playtime, stats and a timeline of what changed.
- Controller / Steam Deck mode (`rig --deck`).

### Later additions
- Rollback offer: a crash soon after a mod update names the update and offers the way back; updates show what changed first.
- Theme codes to share a custom theme; soft sounds and controller rumble (off by default).
- *Tools → Logs*: every log of a game in one searchable window.
- Save Guard now protects any game whose save places are known (not only Unreal's `SaveGames`), on Windows too, and in plain
  Wine prefixes (Lutris, Heroic, Bottles) whose user folder is not `steamuser`. Save places were added to about thirty games,
  and `{saved_games}` joined the starting points. Fixed: restoring the oldest backup could delete it first.
- The crash knowledge base grew from 13 to 26 known problems (Harmony failures, missing BepInEx dependencies, a broken Wine
  prefix, out of memory, Steam not running ...) and every text is now translated into all ten languages.
- Support entries: 55 games (was 28) with Nexus domains, Thunderstore communities and loaders checked against the sites'
  own lists. Fixed: the first *Mafia: Definitive Edition* was filed as a second Mafia III with Mafia III's starter mods.
- From a read-only look at Steam (see `docs/STEAM-LESSONS.md`): Gantry notices when Steam is updating a game (health line,
  and Play waits instead of starting half-updated files); Steam Cloud games get a heads-up next to per-profile saves;
  *Tools → Workshop* lists the Steam Workshop items a game has installed; the overview shows *Trending for your games*;
  Settings can cap download speed and make new downloads wait until a game closes.
- The game list Gantry knows grew from 55 to 276 games, each checked against Nexus Mods' own list and a save-location
  database (`tools/build_recipes.py` makes them); games with anti-cheat or with mods outside the game folder are left out.
- `rig --doctor` prints the state of every installed game (also handy to attach to a bug report); Settings has
  *Report a problem* and *Ask for a game* buttons that open a pre-filled GitHub form; the window opens at the size you
  left it; the update check now sees beta releases.
- Mod Settings keeps a file's byte order mark and Windows line endings when saving, and refuses files that are not UTF-8
  instead of garbling them. GitHub links ending in `#readme` or `?tab=...` are understood and a rate limit is named.
  Fixed a possible crash in cover look-up for names with letters such as İ. Faster MD5 for large downloads.

### Managing mods
- Drag to reorder, groups, bulk actions, undo.
- A conflict viewer: who overwrites whom, mods that change nothing, one-click *let this mod win*, search by file.
- Update rules per mod, rollback to the last two versions, a live download manager.
- A notification bell for updates, failed downloads, crashes and game updates.

### Safety
- Weekly automatic backups of settings and mod lists (the newest 8 are kept), and a save snapshot after every session.
- Storage view with safe clean-up.
- Optional VirusTotal lookups (off by default, your own key; only the SHA-256 fingerprint of a program file is sent).
- Nexus checksums, program-file marks, anti-cheat games refused, Steam never edited without asking.

### Look and access
- Themes (dark, midnight, ember, forest, ocean, light, paper, mist, sand, or your own), accent colours, density,
  layout choices for the game page, motion levels from full to off.
- Screen reader names, dialogs that take and give back the focus, keyboard activation, and text contrast that is
  checked in every theme by the automatic screen test.
- What's new and empty states with actions on a first run.

### Packages
- Linux tar.gz and AppImage, Windows zip, macOS archive, an Arch package file (`gantry-git`), winget and Homebrew
  manifests. Flatpak is not offered: its sandbox cannot start games or reach their folders the way a mod manager must.

## Earlier
Versions before 5.1.0 were development builds of the same project.
