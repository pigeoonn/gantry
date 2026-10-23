# The mod page, updates and rollback

Press **ℹ** on a mod in the library (or click a card in the store). For a mod from Nexus Mods or Thunderstore the page
shows the author's description and gallery, what changed in each version, what the mod needs (and which of those you
already have), the files Gantry placed, and the instructions that came with it. Mods added by hand have a page with
their files and instructions. Pages are kept for ten minutes, so opening one again is instant.

**Updates, on your terms.** The menu on the page chooses what happens when a newer version appears: *tell me* (default),
*update automatically* (when no game is running) or *keep this version* (a 🔒 shows in the list).

**Rollback.** After an update the page lists the last two earlier versions with *Go back to v…*. The version you leave
stays available, so a rollback can be undone too.

**Mods with an installer (FOMOD).** Instead of dropping files into the library, Gantry shows the installer's pages: pick
an option to read what it does, *Next*, *Install*. Required options are ticked, impossible ones greyed out. Your answers
are saved and reused when the mod updates. A damaged script falls back to adding the files as they are, with a note in the log.

## Mods from GitHub
A mod installed from a repository's release (the *GitHub* tab of *Find mods*) remembers the repository and the file. Gantry
checks for newer releases together with the other updates; the update button downloads the file with the same kind of name
from the new release and replaces the mod. If the new release has several files and none is clearly the same one, Gantry
says so and leaves it to you.
