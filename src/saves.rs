//! Save Guard: timestamped snapshots of a game's save folders.
//!
//! A snapshot is taken automatically before every launch (skipped when nothing changed since
//! the last one), on demand, and before any restore, so a mod that corrupts a save never costs
//! more than the last session. The newest few are kept; restoring never deletes anything and
//! first snapshots what is there now.
//!
//! Which folders count is `profsaves::roots`: Unreal's `SaveGames` (found by themselves) and the
//! places a game's support entry names (Documents, AppData, the game folder ...). That works the
//! same in a Wine prefix and on Windows. A snapshot remembers where each folder came from, so it
//! goes back to the right place even if the folder is gone. Snapshots made by older versions (a
//! copy of the prefix's AppData/Local tree, or the `backups` folder) still restore.

use crate::engine::GameEntry;
use crate::fsutil;
use crate::profsaves;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

const KEEP_MANUAL: usize = 8;
const KEEP_AUTO: usize = 8;
const META: &str = "meta.json";
const DATA: &str = "data";
const LEGACY_MARKER: &str = ".rig_backup_v2";
const NO_FOLDERS: &str = "No save folder found for this game yet.";
const EMPTY: &str = "The save folders are empty.";
/// A copy of more than this is not a save folder (a wrong entry would otherwise fill the disk before every launch).
const MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const TOO_BIG: &str = "The save folders are unusually large (over 2 GB), so they are not copied automatically.";

/// Where one saved folder belongs: its name in the snapshot and its place as a starting point plus path.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredRoot {
    pub key: String,
    pub token: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Snapshot {
    /// Folder name under `saves/<game>/` (or `legacy…` for backups made by older versions).
    pub id: String,
    /// manual, auto, setup, before-restore or legacy
    pub label: String,
    pub at_ms: u64,
    pub files: usize,
    pub bytes: u64,
    #[serde(default)]
    pub signature: String,
    /// The folders inside; empty for a snapshot made by an older version (those mirror AppData/Local).
    #[serde(default)]
    pub roots: Vec<StoredRoot>,
}

fn safe_id(raw: &str) -> String {
    let s: String = raw.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
    if s.is_empty() {
        "game".into()
    } else {
        s
    }
}

fn game_dir(cache_root: &Path, game: &GameEntry) -> PathBuf {
    cache_root.join("saves").join(safe_id(&game.appid))
}

fn label_ok(l: &str) -> String {
    let s: String = l.chars().filter(|c| c.is_ascii_lowercase() || *c == '-').collect();
    if s.is_empty() {
        "manual".into()
    } else {
        s
    }
}

/// FNV-1a over (folder, relative path, size, mtime) of every file: changes when any save changes.
fn signature(roots: &[(PathBuf, String)]) -> (String, usize, u64) {
    let mut entries: Vec<(String, u64, u64)> = Vec::new();
    for (dir, key) in roots {
        for e in walkdir::WalkDir::new(dir).into_iter().flatten().filter(|e| e.file_type().is_file()) {
            let Ok(m) = e.metadata() else { continue };
            let mt = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
            let sub = e.path().strip_prefix(dir).unwrap_or(e.path());
            entries.push((format!("{}/{}", key, fsutil::slashed(sub)), m.len(), mt));
        }
    }
    entries.sort();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut bytes = 0;
    for (p, size, mt) in &entries {
        for b in p.bytes().chain(size.to_le_bytes()).chain(mt.to_le_bytes()) {
            h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        bytes += size;
    }
    (format!("{:016x}", h), entries.len(), bytes)
}

fn live_roots(game: &GameEntry, custom: &[String]) -> Result<Vec<profsaves::Root>, String> {
    let roots = profsaves::roots(game, custom);
    if roots.is_empty() {
        return Err(NO_FOLDERS.into());
    }
    Ok(roots)
}

fn read_meta(dir: &Path) -> Option<Snapshot> {
    let mut s: Snapshot = serde_json::from_str(&fs::read_to_string(dir.join(META)).ok()?).ok()?;
    s.id = dir.file_name()?.to_string_lossy().to_string();
    Some(s)
}

/// All snapshots of `game`, newest first (including backups made by older versions).
pub fn list(game: &GameEntry, cache_root: &Path) -> Vec<Snapshot> {
    let mut out: Vec<Snapshot> = fs::read_dir(game_dir(cache_root, game))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir() && !fsutil::is_tmp_path(&e.path()))
        .filter_map(|e| read_meta(&e.path()))
        .collect();
    let id = safe_id(&game.appid);
    for (suffix, sid) in [("", "legacy"), (".pre-restore", "legacy-pre-restore")] {
        let d = cache_root.join("backups").join(format!("{}{}", id, suffix));
        if d.is_dir() {
            let at = fs::metadata(&d).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64).unwrap_or(0);
            let (_, mut files, mut bytes) = signature(&[(d.clone(), String::new())]);
            if d.join(LEGACY_MARKER).is_file() {
                files = files.saturating_sub(1); // the marker is bookkeeping, not a save
                bytes = bytes.saturating_sub(2);
            }
            out.push(Snapshot { id: sid.into(), label: "legacy".into(), at_ms: at, files, bytes, signature: String::new(), roots: Vec::new() });
        }
    }
    out.sort_by(|a, b| b.at_ms.cmp(&a.at_ms).then_with(|| b.id.cmp(&a.id)));
    out
}

/// Drop the oldest snapshots beyond the allowance, except `protect` (the one a restore is about to use).
fn prune(game: &GameEntry, cache_root: &Path, protect: Option<&str>) {
    let snaps: Vec<Snapshot> = list(game, cache_root).into_iter().filter(|s| s.label != "legacy").collect();
    for (manual, keep) in [(true, KEEP_MANUAL), (false, KEEP_AUTO)] {
        for old in snaps.iter().filter(|s| (s.label == "manual") == manual).skip(keep) {
            if Some(old.id.as_str()) != protect {
                let _ = fs::remove_dir_all(game_dir(cache_root, game).join(&old.id));
            }
        }
    }
}

/// Take a snapshot of the folders in `profsaves::roots(game, custom)`. `Ok(None)` means nothing
/// changed since the newest one, so none was needed.
pub fn snapshot(game: &GameEntry, cache_root: &Path, label: &str, custom: &[String]) -> Result<Option<Snapshot>, String> {
    snapshot_keeping(game, cache_root, label, custom, None)
}

fn snapshot_keeping(game: &GameEntry, cache_root: &Path, label: &str, custom: &[String], protect: Option<&str>) -> Result<Option<Snapshot>, String> {
    let roots = live_roots(game, custom)?;
    let places: Vec<(PathBuf, String)> = roots.iter().map(|r| (r.live.clone(), r.key.clone())).collect();
    let (sig, files, bytes) = signature(&places);
    if files == 0 {
        return Err(EMPTY.into());
    }
    if bytes > MAX_BYTES {
        return Err(TOO_BIG.into());
    }
    if list(game, cache_root).iter().find(|s| s.label != "legacy").map(|s| s.signature == sig).unwrap_or(false) {
        return Ok(None);
    }
    let at_ms = fsutil::now_ms();
    let label = label_ok(label);
    let id = format!("{:013}-{}", at_ms, label);
    let base = game_dir(cache_root, game);
    let stage = base.join(format!("{}{}-{}", id, fsutil::TMP_MARK, fsutil::unique_token()));
    let stored: Vec<StoredRoot> = roots.iter().map(|r| StoredRoot { key: r.key.clone(), token: r.token.clone() }).collect();
    let result = (|| -> std::io::Result<()> {
        for r in &roots {
            fsutil::copy_dir_all(&r.live, &stage.join(DATA).join(&r.key))?;
        }
        let meta = Snapshot { id: id.clone(), label: label.clone(), at_ms, files, bytes, signature: sig.clone(), roots: stored.clone() };
        fs::write(stage.join(META), serde_json::to_vec_pretty(&meta).unwrap_or_default())?;
        fs::rename(&stage, base.join(&id))
    })();
    if let Err(e) = result {
        let _ = fs::remove_dir_all(&stage);
        return Err(format!("Backup failed: {}", e));
    }
    prune(game, cache_root, protect);
    Ok(Some(Snapshot { id, label, at_ms, files, bytes, signature: sig, roots: stored }))
}

/// Put a snapshot back. Files that exist now are overwritten, nothing else is deleted, and the
/// current state is snapshotted first so the restore itself can be undone.
pub fn restore(game: &GameEntry, cache_root: &Path, id: &str, custom: &[String]) -> Result<String, String> {
    let snaps = list(game, cache_root);
    let snap = snaps.iter().find(|s| s.id == id).ok_or("That backup does not exist (any more).")?;
    let b = profsaves::bases(game);
    let legacy = snap.label == "legacy" || snap.roots.is_empty();
    if legacy && !b.local.is_dir() {
        return Err("Wine prefix AppData/Local not found.".into());
    }
    // the safety net: abort if it cannot be taken while there is something to protect. It must not
    // push the backup that is being restored out of the allowance.
    match snapshot_keeping(game, cache_root, "before-restore", custom, Some(id)) {
        Ok(_) => {}
        Err(e) if e == NO_FOLDERS || e == EMPTY || e == TOO_BIG => {}
        Err(e) => return Err(format!("Could not snapshot the current saves, restore aborted: {}", e)),
    }
    let copied: std::io::Result<usize> = if snap.label == "legacy" {
        let src = cache_root.join("backups").join(format!(
            "{}{}",
            safe_id(&game.appid),
            if id == "legacy-pre-restore" { ".pre-restore" } else { "" }
        ));
        if src.join(LEGACY_MARKER).is_file() || id == "legacy-pre-restore" {
            crate::repair::RepairEngine::find_save_dirs(&src).iter().try_fold(0, |acc, d| {
                let rel = d.strip_prefix(&src).unwrap_or(d);
                fsutil::copy_dir_all(d, &b.local.join(rel)).map(|n| acc + n)
            })
        } else {
            crate::repair::RepairEngine::find_save_dirs(&b.local).iter().try_fold(0, |acc, d| fsutil::copy_dir_all(&src, d).map(|n| acc + n))
        }
    } else if snap.roots.is_empty() {
        // made by an older version: a mirror of AppData/Local
        let data = game_dir(cache_root, game).join(id).join(DATA);
        crate::repair::RepairEngine::find_save_dirs(&data).iter().try_fold(0, |acc, d| {
            let rel = d.strip_prefix(&data).unwrap_or(d);
            fsutil::copy_dir_all(d, &b.local.join(rel)).map(|n| acc + n)
        })
    } else {
        let data = game_dir(cache_root, game).join(id).join(DATA);
        snap.roots.iter().try_fold(0, |acc, r| {
            // the names come from a file on disk: only plain ones, and only places inside the starting points
            let src = data.join(&r.key);
            match (safe_id(&r.key) == r.key && src.is_dir(), profsaves::resolve(&r.token, &b)) {
                (true, Some(dest)) => fsutil::copy_dir_all(&src, &dest).map(|n| acc + n),
                _ => Ok(acc),
            }
        })
    };
    match copied {
        Ok(0) => Err("Nothing could be restored (the target SaveGames folder was not found).".into()),
        Ok(n) => Ok(format!("Restored {} save file(s) for '{}'. The previous state was kept as a \"before-restore\" backup.", n, game.name)),
        Err(e) => Err(format!("Restore failed: {}", e)),
    }
}

#[cfg(test)]
#[cfg(not(windows))] // these tests build Wine prefixes; on Windows the saves are in the real AppData
mod tests {
    use super::*;
    use crate::repair::fixtures::make_game;
    use std::time::Duration;

    fn saves_dir(game: &GameEntry) -> PathBuf {
        PathBuf::from(&game.pfx).join("drive_c/users/steamuser/AppData/Local/Demo/Saved/SaveGames")
    }

    fn docs(game: &GameEntry) -> PathBuf {
        PathBuf::from(&game.pfx).join("drive_c/users/steamuser/Documents/My Game")
    }

    const NONE: &[String] = &[];

    #[test]
    fn snapshots_are_skipped_when_nothing_changed_and_listed_newest_first() {
        let (d, game) = make_game("sg1");
        let cache = d.join("cache");
        fs::write(saves_dir(&game).join("slot1.sav"), "ONE").unwrap();
        let a = snapshot(&game, &cache, "auto", NONE).unwrap().expect("first snapshot");
        assert_eq!((a.files, a.bytes, a.label.as_str()), (1, 3, "auto"));
        assert_eq!(snapshot(&game, &cache, "auto", NONE).unwrap(), None, "unchanged saves are not copied again");
        assert_eq!(snapshot(&game, &cache, "manual", NONE).unwrap(), None, "not even on request: nothing new to protect");

        std::thread::sleep(Duration::from_millis(1100)); // mtime has 1 s resolution
        fs::write(saves_dir(&game).join("slot1.sav"), "TWO!").unwrap();
        let b = snapshot(&game, &cache, "manual", NONE).unwrap().expect("changed saves are captured");
        let l = list(&game, &cache);
        assert_eq!(l.iter().map(|s| s.id.clone()).collect::<Vec<_>>(), vec![b.id.clone(), a.id.clone()]);
        assert!(l[0].at_ms >= l[1].at_ms);
        // nothing is left half-written
        assert!(fs::read_dir(cache.join("saves/42")).unwrap().flatten().all(|e| !fsutil::is_tmp_path(&e.path())));
    }

    #[test]
    fn restore_brings_back_old_content_and_protects_the_current_state_first() {
        let (d, game) = make_game("sg2");
        let cache = d.join("cache");
        let slot = saves_dir(&game).join("slot1.sav");
        fs::write(&slot, "GOOD").unwrap();
        let good = snapshot(&game, &cache, "auto", NONE).unwrap().unwrap();
        std::thread::sleep(Duration::from_millis(1100));
        fs::write(&slot, "CORRUPTED BY A MOD").unwrap();
        fs::write(saves_dir(&game).join("new_slot.sav"), "created later").unwrap();

        let msg = restore(&game, &cache, &good.id, NONE).unwrap();
        assert!(msg.contains("Restored 1 save file"), "{msg}");
        assert_eq!(fs::read_to_string(&slot).unwrap(), "GOOD");
        assert!(saves_dir(&game).join("new_slot.sav").is_file(), "restore never deletes");
        let before = list(&game, &cache).into_iter().find(|s| s.label == "before-restore").expect("safety snapshot");
        // the corrupted state can itself be brought back
        std::thread::sleep(Duration::from_millis(5));
        restore(&game, &cache, &before.id, NONE).unwrap();
        assert_eq!(fs::read_to_string(&slot).unwrap(), "CORRUPTED BY A MOD");
    }

    #[test]
    fn restoring_the_oldest_backup_does_not_delete_it_before_it_is_used() {
        let (d, game) = make_game("sg_oldest");
        let cache = d.join("cache");
        let f = saves_dir(&game).join("a.sav");
        let mut first = None;
        for i in 0..KEEP_AUTO {
            fs::write(&f, format!("v{}", "x".repeat(i + 1))).unwrap();
            let s = snapshot(&game, &cache, "auto", NONE).unwrap().unwrap();
            first.get_or_insert(s.id);
            std::thread::sleep(Duration::from_millis(3));
        }
        let oldest = first.unwrap();
        assert_eq!(list(&game, &cache).len(), KEEP_AUTO, "the allowance is full");
        // the saves changed since the last backup, so the safety backup is really taken (and would push the oldest out)
        fs::write(&f, "this is what the mod made of it").unwrap();
        let msg = restore(&game, &cache, &oldest, NONE).expect("the oldest backup still restores");
        assert!(msg.contains("Restored 1 save file"), "{msg}");
        assert_eq!(fs::read_to_string(&f).unwrap(), "vx");
        assert!(list(&game, &cache).iter().any(|s| s.label == "before-restore"));
        // once it is used, the normal allowance applies again
        std::thread::sleep(Duration::from_millis(3));
        fs::write(&f, "later").unwrap();
        snapshot(&game, &cache, "auto", NONE).unwrap().unwrap();
        assert!(list(&game, &cache).iter().all(|s| s.id != oldest));
    }

    #[test]
    fn bad_ids_and_missing_prefixes_are_refused_cleanly() {
        let (d, game) = make_game("sg3");
        let cache = d.join("cache");
        fs::write(saves_dir(&game).join("a.sav"), "x").unwrap();
        snapshot(&game, &cache, "auto", NONE).unwrap();
        for bad in ["../../etc", "nope", "", "legacy", "/tmp"] {
            assert!(restore(&game, &cache, bad, NONE).is_err(), "{bad:?}");
        }
        let mut no_prefix = game.clone();
        no_prefix.pfx = d.join("missing").to_string_lossy().to_string();
        assert!(snapshot(&no_prefix, &cache, "auto", NONE).unwrap_err().contains("No save folder"));
        fs::remove_file(saves_dir(&game).join("a.sav")).unwrap();
        assert!(snapshot(&game, &cache, "auto", NONE).unwrap_err().contains("empty"));
        assert_eq!(label_ok("../../x"), "x");
        assert_eq!(label_ok("123"), "manual");
    }

    #[test]
    fn only_the_newest_snapshots_are_kept() {
        let (d, game) = make_game("sg4");
        let cache = d.join("cache");
        let f = saves_dir(&game).join("a.sav");
        for i in 0..(KEEP_MANUAL + 4) {
            fs::write(&f, format!("v{:03}", i)).unwrap();
            // identical size, different mtime is not guaranteed within a second, so vary the size too
            fs::write(saves_dir(&game).join(format!("pad{}.sav", i)), "x").unwrap();
            snapshot(&game, &cache, "manual", NONE).unwrap().unwrap();
            std::thread::sleep(Duration::from_millis(3));
        }
        let l = list(&game, &cache);
        assert_eq!(l.len(), KEEP_MANUAL);
        assert!(l.iter().all(|s| s.label == "manual"));
        // auto snapshots have their own allowance and do not evict manual ones
        fs::write(saves_dir(&game).join("extra.sav"), "e").unwrap();
        snapshot(&game, &cache, "auto", NONE).unwrap().unwrap();
        assert_eq!(list(&game, &cache).len(), KEEP_MANUAL + 1);
    }

    #[test]
    fn backups_made_by_older_versions_remain_restorable() {
        let (d, game) = make_game("sg5");
        let cache = d.join("cache");
        // v2 legacy layout: mirrors AppData/Local, marked
        let legacy = cache.join("backups/42/Demo/Saved/SaveGames");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("old.sav"), "LEGACY").unwrap();
        fs::write(cache.join("backups/42").join(LEGACY_MARKER), "v2").unwrap();
        let l = list(&game, &cache);
        assert_eq!(l.len(), 1);
        assert_eq!((l[0].id.as_str(), l[0].label.as_str(), l[0].files), ("legacy", "legacy", 1));
        restore(&game, &cache, "legacy", NONE).unwrap();
        assert_eq!(fs::read_to_string(saves_dir(&game).join("old.sav")).unwrap(), "LEGACY");
    }

    #[test]
    fn snapshots_from_the_previous_version_still_restore() {
        let (d, game) = make_game("sg_v2");
        let cache = d.join("cache");
        // what the last version wrote: data/ mirrors AppData/Local, and meta.json has no folders listed
        let snap = cache.join("saves/42/0000000000001-auto");
        fs::create_dir_all(snap.join("data/Demo/Saved/SaveGames")).unwrap();
        fs::write(snap.join("data/Demo/Saved/SaveGames/old.sav"), "FROM V5.1").unwrap();
        fs::write(snap.join(META), r#"{"id":"x","label":"auto","at_ms":1,"files":1,"bytes":9,"signature":"abc"}"#).unwrap();
        let l = list(&game, &cache);
        assert_eq!((l.len(), l[0].roots.len()), (1, 0));
        restore(&game, &cache, "0000000000001-auto", NONE).unwrap();
        assert_eq!(fs::read_to_string(saves_dir(&game).join("old.sav")).unwrap(), "FROM V5.1");
    }

    #[test]
    fn saves_a_support_entry_names_are_guarded_too_and_come_back_even_when_the_folder_is_gone() {
        let (d, game) = make_game("sg_docs");
        let cache = d.join("cache");
        let custom = vec!["{documents}/My Game".to_string(), "{documents}/Not There".to_string()];
        fs::create_dir_all(docs(&game).join("profile")).unwrap();
        fs::write(docs(&game).join("profile/career.sav"), "LEVEL 9").unwrap();
        fs::write(saves_dir(&game).join("ue.sav"), "U").unwrap();
        let s = snapshot(&game, &cache, "auto", &custom).unwrap().expect("both folders are captured");
        assert_eq!((s.files, s.roots.len()), (2, 2));
        assert!(s.roots.iter().any(|r| r.token == "{documents}/My Game") && s.roots.iter().any(|r| r.token.starts_with("{appdata_local}/")), "{:?}", s.roots);
        assert_eq!(snapshot(&game, &cache, "auto", &custom).unwrap(), None);
        // without the entry only the Unreal folder is seen; the other one is simply not part of it
        assert_eq!(list(&game, &cache)[0].files, 2);

        // the whole folder is lost (a reinstall, a clean-up) and comes back where it was
        fs::remove_dir_all(docs(&game)).unwrap();
        fs::write(saves_dir(&game).join("ue.sav"), "changed").unwrap();
        let msg = restore(&game, &cache, &s.id, &custom).unwrap();
        assert!(msg.contains("Restored 2 save file"), "{msg}");
        assert_eq!(fs::read_to_string(docs(&game).join("profile/career.sav")).unwrap(), "LEVEL 9");
        assert_eq!(fs::read_to_string(saves_dir(&game).join("ue.sav")).unwrap(), "U");
    }

    #[test]
    fn a_game_in_a_plain_wine_prefix_is_guarded_like_any_other() {
        let (d, mut game) = make_game("sg_wine");
        let cache = d.join("cache");
        // Lutris / Heroic / Bottles: the user folder carries the Linux account's name
        let pfx = d.join("lutris/prefix");
        let account = std::env::var("USER").or_else(|_| std::env::var("LOGNAME")).unwrap_or_else(|_| "tester".into());
        let user = pfx.join("drive_c/users").join(&account);
        fs::create_dir_all(pfx.join("drive_c/users/Public")).unwrap();
        fs::create_dir_all(user.join("Documents/Wine Game")).unwrap();
        fs::write(user.join("Documents/Wine Game/slot.sav"), "WINE").unwrap();
        game.appid = "lutris_wine_7".into();
        game.pfx = pfx.to_string_lossy().to_string();
        let custom = vec!["{documents}/Wine Game".to_string()];
        let s = snapshot(&game, &cache, "auto", &custom).unwrap().expect("found in the account's folder");
        assert_eq!(s.files, 1);
        fs::write(user.join("Documents/Wine Game/slot.sav"), "BROKEN").unwrap();
        restore(&game, &cache, &s.id, &custom).unwrap();
        assert_eq!(fs::read_to_string(user.join("Documents/Wine Game/slot.sav")).unwrap(), "WINE");
    }

    #[test]
    fn a_hand_edited_snapshot_cannot_write_outside_the_save_places() {
        let (d, game) = make_game("sg_evil");
        let cache = d.join("cache");
        fs::write(saves_dir(&game).join("a.sav"), "x").unwrap();
        let s = snapshot(&game, &cache, "auto", NONE).unwrap().unwrap();
        let meta_path = cache.join("saves/42").join(&s.id).join(META);
        let mut meta: Snapshot = serde_json::from_str(&fs::read_to_string(&meta_path).unwrap()).unwrap();
        fs::create_dir_all(cache.join("saves/42").join(&s.id).join("data/x")).unwrap();
        fs::write(cache.join("saves/42").join(&s.id).join("data/x/pwn.txt"), "p").unwrap();
        meta.roots = vec![
            StoredRoot { key: "../../../../x".into(), token: "{documents}/ok".into() },
            StoredRoot { key: "x".into(), token: "{documents}/../../escape".into() },
            StoredRoot { key: "x".into(), token: "/etc".into() },
        ];
        fs::write(&meta_path, serde_json::to_vec(&meta).unwrap()).unwrap();
        let r = restore(&game, &cache, &s.id, NONE);
        assert!(r.is_err(), "nothing valid was left to restore: {r:?}");
        assert!(!d.join("escape").exists() && !Path::new("/etc/pwn.txt").exists());
    }
}
