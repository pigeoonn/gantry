//! Saves that belong to a mod profile. A modded run and a plain run (or two sets of mods) often cannot
//! share saves: a mod changes what a save holds, and the game then refuses or breaks it. When this is on,
//! switching profile puts the current saves away under the old profile's name and brings back the ones
//! the new profile had (a profile that has none yet simply starts from what is there). Nothing is ever
//! deleted: what is replaced is first copied to a backup folder.
//!
//! Where saves live is the game's business: Unreal's `SaveGames` folders are found by themselves, every
//! other game names its folders in its support entry (`saves`), with these starting points:
//! `{appdata_local}`, `{appdata_locallow}`, `{appdata_roaming}`, `{documents}`, `{saved_games}`, `{game}` and `{home}`.

use crate::engine::GameEntry;
use crate::fsutil;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// The folders the starting points stand for, for one game.
pub struct Bases {
    pub local: PathBuf,
    pub locallow: PathBuf,
    pub roaming: PathBuf,
    pub documents: PathBuf,
    /// Windows' "Saved Games" folder (CD Projekt, Sony ports and others keep saves there).
    pub saved_games: PathBuf,
    pub game: PathBuf,
    pub home: PathBuf,
}

pub fn bases(g: &GameEntry) -> Bases {
    let home = fsutil::home_dir();
    let (local, locallow, roaming, documents, saved_games) = if cfg!(windows) {
        let env = |v: &str, rel: &str| std::env::var_os(v).filter(|x| !x.is_empty()).map(PathBuf::from).unwrap_or_else(|| home.join(rel));
        (env("LOCALAPPDATA", "AppData/Local"), home.join("AppData/LocalLow"), env("APPDATA", "AppData/Roaming"), home.join("Documents"), home.join("Saved Games"))
    } else {
        // Proton and Wine keep the Windows folders inside the game's prefix, under the prefix's user
        // (a game without a prefix has no such folders; a path that cannot exist keeps everything away from the current folder)
        let user = fsutil::wine_user_dir(&if g.pfx.is_empty() { PathBuf::from("/nonexistent-gantry-prefix") } else { PathBuf::from(&g.pfx) });
        (user.join("AppData/Local"), user.join("AppData/LocalLow"), user.join("AppData/Roaming"), user.join("Documents"), user.join("Saved Games"))
    };
    Bases { local, locallow, roaming, documents, saved_games, game: PathBuf::from(&g.game_dir), home }
}

/// `{documents}/My Game` -> the folder, or nothing when the text is not of that shape or leaves its base.
pub fn resolve(token_path: &str, b: &Bases) -> Option<PathBuf> {
    let t = token_path.trim().replace('\\', "/");
    let (base, rest) = match t.split_once('}') {
        Some((open, rest)) if open.starts_with('{') => (&open[1..], rest.trim_start_matches('/')),
        _ => return None,
    };
    let root = match base {
        "appdata_local" => &b.local,
        "appdata_locallow" => &b.locallow,
        "appdata_roaming" => &b.roaming,
        "documents" => &b.documents,
        "saved_games" => &b.saved_games,
        "game" => &b.game,
        "home" => &b.home,
        _ => return None,
    };
    let mut out = root.clone();
    let mut any = false;
    for c in Path::new(rest).components() {
        match c {
            Component::Normal(n) if !n.to_string_lossy().contains(':') => {
                out.push(n);
                any = true;
            }
            Component::CurDir => {}
            _ => return None,
        }
    }
    any.then_some(out)
}

/// One folder of saves: a name for the store and where it is now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Root {
    pub key: String,
    pub live: PathBuf,
    /// The same place as a starting point plus a path (`{documents}/My Game`): it means the same on
    /// another day, whatever the prefix looks like then, so a stored copy can find its way back.
    pub token: String,
}

fn slug(s: &str) -> String {
    let t: String = s.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    t.trim_matches('_').chars().take(80).collect()
}

fn local_token(rel: &str) -> String {
    format!("{{appdata_local}}/{}", rel.replace('\\', "/"))
}

/// The save folders of a game: Unreal's `SaveGames` (found in the prefix) and those its support entry names.
pub fn roots(g: &GameEntry, custom: &[String]) -> Vec<Root> {
    let b = bases(g);
    let mut out: Vec<Root> = Vec::new();
    if b.local.is_dir() && cfg!(windows) {
        // the real AppData\Local is big: look only where Unreal puts them (<project>\Saved\SaveGames)
        if g.engine.starts_with("Unreal") {
            for e in fs::read_dir(&b.local).into_iter().flatten().flatten() {
                let d = e.path().join("Saved/SaveGames");
                if d.is_dir() {
                    let rel = d.strip_prefix(&b.local).unwrap_or(&d).to_string_lossy().to_string();
                    out.push(Root { key: format!("ue_{}", slug(&rel)), token: local_token(&rel), live: d });
                }
            }
        }
    } else if b.local.is_dir() {
        for d in crate::repair::RepairEngine::find_save_dirs(&b.local) {
            let rel = d.strip_prefix(&b.local).unwrap_or(&d).to_string_lossy().to_string();
            out.push(Root { key: format!("ue_{}", slug(&rel)), token: local_token(&rel), live: d });
        }
    }
    for c in custom {
        if let Some(live) = resolve(c, &b) {
            if live.is_dir() && !out.iter().any(|r| r.live == live) {
                out.push(Root { key: format!("p_{}", slug(c)), token: c.trim().replace('\\', "/"), live });
            }
        }
    }
    out
}

fn profile_dir(store: &Path, profile: &str) -> PathBuf {
    store.join(slug(profile))
}

/// Does this profile have saves put away?
pub fn has_saves(store: &Path, profile: &str) -> bool {
    fs::read_dir(profile_dir(store, profile)).map(|mut d| d.next().is_some()).unwrap_or(false)
}

/// Copy the live saves into the profile's store, replacing what was there.
pub fn put_away(store: &Path, profile: &str, roots: &[Root]) -> Result<usize, String> {
    let mut n = 0;
    for r in roots {
        let dest = profile_dir(store, profile).join(&r.key);
        let tmp = dest.with_extension(format!("tmp-{}", fsutil::unique_token()));
        let _ = fs::remove_dir_all(&tmp);
        n += fsutil::copy_dir_all(&r.live, &tmp).map_err(|e| format!("Could not keep the saves of '{}': {}", profile, e))?;
        fsutil::swap_dir(&tmp, &dest).map_err(|e| format!("Could not keep the saves of '{}': {}", profile, e))?;
    }
    Ok(n)
}

/// Replace the live saves with the profile's stored ones (only for folders it has stored). What was
/// there is copied into `backup` first. Returns how many folders were replaced.
pub fn bring_back(store: &Path, backup: &Path, profile: &str, roots: &[Root]) -> Result<usize, String> {
    let mut replaced = 0;
    for r in roots {
        let from = profile_dir(store, profile).join(&r.key);
        if !from.is_dir() {
            continue;
        }
        let keep = backup.join(&r.key);
        fsutil::copy_dir_all(&r.live, &keep).map_err(|e| format!("Could not back up the current saves: {e}"))?;
        let staged = r.live.with_extension(format!("gantry-tmp-{}", fsutil::unique_token()));
        fsutil::copy_dir_all(&from, &staged).map_err(|e| {
            let _ = fs::remove_dir_all(&staged);
            format!("Could not bring the saves of '{}' back: {}", profile, e)
        })?;
        fsutil::swap_dir(&staged, &r.live).map_err(|e| format!("Could not bring the saves of '{}' back: {}", profile, e))?;
        replaced += 1;
    }
    Ok(replaced)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsutil::testutil::TempDir;

    fn game(d: &TempDir) -> GameEntry {
        let mut g = crate::repair::fixtures::make_game("profsaves").1;
        g.pfx = d.join("pfx").to_string_lossy().to_string();
        g.game_dir = d.join("game").to_string_lossy().to_string();
        g
    }

    fn write(p: &Path, s: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, s).unwrap();
    }

    #[test]
    fn starting_points_resolve_inside_their_base_and_nowhere_else() {
        let d = TempDir::new("ps_resolve");
        let g = game(&d);
        let b = bases(&g);
        assert_eq!(resolve("{documents}/My Game", &b), Some(b.documents.join("My Game")));
        assert_eq!(resolve("{appdata_locallow}\\\\Co\\\\Game", &b), Some(b.locallow.join("Co").join("Game")));
        assert_eq!(resolve("{game}/saves", &b), Some(b.game.join("saves")));
        assert_eq!(resolve("{saved_games}/CD Projekt Red/Cyberpunk 2077", &b), Some(b.saved_games.join("CD Projekt Red").join("Cyberpunk 2077")));
        for bad in ["", "documents/x", "{documents}", "{documents}/", "{nothing}/x", "{documents}/../x", "{documents}/C:/x", "{home}/../.."] {
            assert_eq!(resolve(bad, &b), None, "{bad}");
        }
    }

    #[test]
    fn unreal_save_folders_and_named_ones_are_found() {
        let d = TempDir::new("ps_roots");
        let g = game(&d);
        let b = bases(&g);
        write(&b.local.join("Demo/Saved/SaveGames/slot1.sav"), "A");
        write(&b.documents.join("My Game/save.dat"), "B");
        let r = roots(&g, &["{documents}/My Game".into(), "{documents}/Missing".into(), "{documents}/../bad".into()]);
        assert_eq!(r.len(), 2, "{r:?}");
        assert!(r.iter().any(|x| x.key.starts_with("ue_") && x.live.ends_with("SaveGames")));
        assert!(r.iter().any(|x| x.key.starts_with("p_") && x.live == b.documents.join("My Game")));
    }

    #[test]
    fn saves_follow_the_profile_and_what_is_replaced_is_kept() {
        let d = TempDir::new("ps_flow");
        let g = game(&d);
        let b = bases(&g);
        let live = b.documents.join("My Game");
        write(&live.join("slot.sav"), "modded 1");
        write(&live.join("sub/extra.sav"), "x");
        let r = roots(&g, &["{documents}/My Game".into()]);
        let store = d.join("store");
        let backup = d.join("backup");
        assert!(!has_saves(&store, "Modded"));
        // leaving "Modded": its saves are put away
        assert_eq!(put_away(&store, "Modded", &r).unwrap(), 2);
        assert!(has_saves(&store, "Modded"));
        // "Plain" has nothing yet, so nothing is replaced
        assert_eq!(bring_back(&store, &backup, "Plain", &r).unwrap(), 0);
        assert_eq!(fs::read_to_string(live.join("slot.sav")).unwrap(), "modded 1");
        // playing Plain changes the saves; leaving it puts them away too
        fs::write(live.join("slot.sav"), "plain 1").unwrap();
        fs::remove_file(live.join("sub/extra.sav")).unwrap();
        put_away(&store, "Plain", &r).unwrap();
        // back to Modded: its saves return exactly, and the plain ones are in the backup folder
        assert_eq!(bring_back(&store, &backup, "Modded", &r).unwrap(), 1);
        assert_eq!(fs::read_to_string(live.join("slot.sav")).unwrap(), "modded 1");
        assert_eq!(fs::read_to_string(live.join("sub/extra.sav")).unwrap(), "x");
        let kept: Vec<_> = walkdir::WalkDir::new(&backup).into_iter().flatten().filter(|e| e.file_type().is_file()).collect();
        assert_eq!(fs::read_to_string(kept.iter().find(|e| e.file_name() == "slot.sav").unwrap().path()).unwrap(), "plain 1");
        // and Plain again
        assert_eq!(bring_back(&store, &d.join("backup2"), "Plain", &r).unwrap(), 1);
        assert_eq!(fs::read_to_string(live.join("slot.sav")).unwrap(), "plain 1");
        assert!(!live.join("sub/extra.sav").exists(), "the folder is the stored one, not a mix");
        // a profile name cannot reach outside the store
        put_away(&store, "../../evil", &r).unwrap();
        assert!(!d.join("evil").exists() && fs::read_dir(&store).unwrap().count() == 3);
    }
}
