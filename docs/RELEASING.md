# Making a release

## Checklist (nothing here is done by an assistant: publishing is a person's decision)

- [ ] GitHub Actions can start jobs (*Settings → Billing*: the account must have a valid payment method or a spending
      limit above 0; a run that fails in a few seconds with no steps is this).
- [ ] The last run on `main` is green on Linux, Windows and macOS.
- [ ] `Cargo.toml`, `packaging/arch/PKGBUILD`, `packaging/arch/aur/PKGBUILD`, the winget and Homebrew files all say the same version.
- [ ] `CHANGELOG.md` has the version at the top; the *What's new* window of the app (`NEWS_VERSION` in `src/ui.html`) matches.
- [ ] `docs/TESTING.md` was tried by hand on Windows and macOS, if you have them.
- [ ] The README was read once as a stranger; the pictures in `docs/img` are current.
- [ ] `git log` shows nothing that should not be public (keys, private paths); the repository is made public only now.
- [ ] Tag and push (step 3 below), then fill in the checksums (step 4).

## Steps

1. Make sure the last run on `main` is green (Linux, Windows and macOS) and `Cargo.toml` has the version you want.
2. Update `docs/ROADMAP.md` and the version in `packaging/arch/PKGBUILD`.
3. Tag and push: `git tag v5.1.0 && git push origin v5.1.0`. The *Release* workflow builds the Linux tar.gz and
   AppImage, the Windows zip and the macOS archive, with a `.sha256` for each, and publishes them as a GitHub release.
   (*Actions → Release → Run workflow* builds the same files without publishing, to try it.)
4. Update the checksums in `packaging/winget/` and `packaging/homebrew/gantry.rb` and submit them; refresh the
   AUR package from `packaging/arch/PKGBUILD`.
5. Gantry checks for newer releases once a day and tells its users; nothing else is needed.

Before the repository is made public: read the README once as a stranger, check the screenshots in `docs/img`
are current (`node tests/ui_smoke.mjs <rig> <browser> <folder>` writes a picture of every screen), and run
`git log` for anything that should not be published.
