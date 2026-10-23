# Safety

- **Everything is reversible.** Mods live in a library and are linked into the game; *Back to vanilla* removes them and
  restores the originals. Gantry writes nothing outside the game folder and its own settings.
- **Anti-cheat games are refused** (for example THE FINALS): no mods are put there, and Gantry says why.
- **Steam is never edited without asking**, and only while Steam is closed, with a backup.
- **Downloads from Nexus Mods** are compared with the checksum Nexus published; a file that differs is thrown away. The mod
  page shows *Checksum verified* when it matched. Thunderstore and GitHub do not publish a checksum, so those show none.
- **Program files:** a mod that contains `.exe`, `.bat`, `.cmd`, `.ps1`... gets a *Contains programs* mark, because a mod rarely
  needs one.
- **VirusTotal (optional, off by default):** with your own free key, *Check with VirusTotal* on a mod's *Files* tab asks about
  its program files. Only the SHA-256 fingerprint of each file is sent: not the file, not its name, not your game. Answers are
  kept for a few days; a free key may ask four times a minute (Gantry waits between questions). "Not known" is not "safe".
- **Keys** (Nexus, Steam, VirusTotal) are stored in private files in Gantry's settings folder and are never shown or logged.
- **Links from websites** (`nxm://`) always ask before downloading.
