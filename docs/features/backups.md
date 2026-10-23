# Backups and storage

- **Automatic backups:** once a week Gantry copies your settings and every game's mod list (no keys, no mod files). *Tools →
  Backup & Disk* lists the newest 8 and restores one; the current settings are kept in the cache folder first. Switch it off in
  *Settings → Back up Gantry every week*.
- **Your own backup:** *Save a backup* writes one file you can keep anywhere; *Restore a backup* puts it back.
- **Saves:** see [playing](play.md). Copies of replaced saves older than 30 days can be deleted from *Backup & Disk*.
- **Storage:** a bar shows where the space goes (mods, trash, profile saves, downloads) and how much hard links save. *Empty the
  trash* and *Remove leftover folders* only touch Gantry's own files.
- **Verify:** *Backup & Disk* checks that every staged file is still there.

Backups of mod *files* are not made: the library is the copy, and mods can be fetched again from where they came from.

## Sync through a folder
*Settings → Sync folder* takes the full path of a folder that something else keeps in step between your computers
(Syncthing, Dropbox, a USB stick). Gantry copies the **portable** part of your setup into `Gantry-sync` inside it: profiles,
notes, tags, favourites, saved mods, the look and each game's mod list. It copies again when something changed (every
twenty minutes, or press *Send now*). On the other computer press *Bring in*: the portable settings come in, mod lists
are applied wherever the mods are in that computer's library, and the settings it replaces are kept first.
Never copied: folder paths, trainers, launch options, play time, window size and API keys.
