# Why is it slow?

The *Activity* card has a **Why is it slow?** button.

- **While a game runs (Linux)** Gantry notes the most memory it used and how busy the processor was. After the session this
  is kept with it. On Windows and macOS these are not measured.
- **Frame times** come from MangoHud. Press Shift+F2 in the game to save a log; Gantry looks in the folder named in
  `MangoHud.conf`, your home folder, `~/MangoHud` and `/tmp/MangoHud`, and adds average frame rate, the slowest 1 % and
  0.1 %, stutters (frames over three times the usual and over 40 ms) and the slowest frame. *Open a MangoHud log* reads any
  log by hand.
- **With mods and without:** sessions are averaged apart, so you can see what the mods cost.
- **Suspects:** enabled mods that are big (400 MB and more) or run code (`.dll`, `.asi`, `.lua`, `.py`, `.cs`...) are listed
  with a *Switch off* button. Switch one off, play again, and compare.

These are hints to compare against, not a diagnosis. Nothing leaves your computer.
