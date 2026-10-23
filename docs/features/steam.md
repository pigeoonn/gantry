# Steam, Workshop and downloads

Gantry only **reads** what Steam keeps (its game records, its Workshop folder, its Cloud cache); it never changes them.
Steam has to be running for the update check to apply.

## Steam is updating a game
While Steam downloads, unpacks or checks a game, the game page shows *Steam is updating this game (42%)* and **Play
waits**: starting then would run half-updated files, and putting mods in would fight Steam's own writes. When Steam
has an update waiting but has not started it, a quiet note says your mods may need to be deployed again afterwards.
If Steam crashed in the middle of an update and left its record behind, Gantry ignores it as long as Steam is closed.

## Workshop items
For games whose mods come from the Steam Workshop (RimWorld, Project Zomboid, Cities: Skylines and others), a
**Workshop** button appears under *Tools* when the game has items installed. The window lists each one with name,
picture, size, last update and how many people subscribed (looked up on Steam's public item page, then kept on your
computer), with buttons for its Steam page and its folder. Steam still downloads and updates them; Gantry only shows
what is there. An item that has been removed from the Workshop is marked.

## Steam Cloud
When Steam Cloud syncs a game's saves and you use *Each profile keeps its own saves*, a note appears: after a profile
switch Steam may ask which copy to keep, or put its own copy back. Choose the one on this computer, or turn Steam
Cloud off for that game. Gantry also keeps its own copies (see [saves](play.md)).

## Trending for your games
The overview shows *Trending for your games*: the most downloaded recently changed mods of the three games you played
last, without the ones you already have. It needs a Nexus key or a Thunderstore community, and is kept for ten minutes.

## Download limits
*Settings → Download speed limit* caps each download (1 to 25 MB/s). *Wait for the game to close before downloading*
holds new downloads while a game runs, so they cannot make it stutter; a download that has already begun is left alone,
and a waiting one can be cancelled in the download list.
