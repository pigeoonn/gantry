# Modpacks and profile codes

**Share.** In the mod list, *More → Share as modpack* writes a `.gantrypack` (a zip). It holds the list (names, versions,
on/off, folders, groups, notes, and where each mod comes from) and, under `mods/`, the files of mods that cannot be fetched
again. Nexus Mods files are never copied into it.

**Import.** *More → Import a modpack* adds the files that came along and installs the Thunderstore mods with their
dependencies. A bar above the list shows what is still missing, with *Get all* (with a free Nexus account each page opens
and you press *Slow download*). Each mod takes its order and on/off state from the pack; the bar goes away when everything is there.

**Thunderstore / r2modman codes.** *More → Import a profile code* takes the code r2modman or Gale gives for a profile and
recreates it: mods from Thunderstore are fetched, the rest is listed as missing. Sharing as a code is not built (it needs
an upload to Thunderstore, which Gantry does not do for you).

**Profiles** are saved lists of what is on and in which order: *Load Profile…* switches between them. With
*Each profile keeps its own saves* on, switching also swaps the save files (the current ones are kept in a backup first).
