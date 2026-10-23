//! Settings files of mods (BepInEx `.cfg`, MelonLoader `UserData`, plain `.ini`/`.json`) that the
//! user edits inside the app. Only text files in the usual settings folders of the selected
//! game can be read or written, never anything that leads outside the game folder, and the first
//! version of a file is kept next to it so a bad edit can be undone.

use std::fs;
use std::path::{Component, Path, PathBuf};

/// Folders (inside the game) where mods keep their settings.
const CONFIG_DIRS: &[&str] = &["BepInEx/config", "UserData", "config", "Config", "Settings"];
const EXTS: &[&str] = &["cfg", "ini", "json", "toml", "yaml", "yml", "xml", "txt", "properties"];
const MAX_SIZE: u64 = 1_000_000;
const BACKUP_EXT: &str = "gantry-orig";

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ConfigFile {
    pub path: String,
    pub size: u64,
    /// The first version is kept, so "restore" is possible.
    pub has_backup: bool,
}

fn ext_ok(p: &Path) -> bool {
    p.extension().map(|e| EXTS.contains(&e.to_string_lossy().to_lowercase().as_str())).unwrap_or(false)
}

/// The settings files of the game, sorted by path.
pub fn list(game_dir: &Path) -> Vec<ConfigFile> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(game_dir) else { return out };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        // BepInEx/config is two levels deep: match each configured folder against the real names
        for cfg in CONFIG_DIRS {
            let first = cfg.split('/').next().unwrap_or("");
            if !name.eq_ignore_ascii_case(first) {
                continue;
            }
            let mut dir = e.path();
            let mut ok = true;
            for part in cfg.split('/').skip(1) {
                match fs::read_dir(&dir).ok().and_then(|rd| rd.flatten().find(|x| x.file_name().to_string_lossy().eq_ignore_ascii_case(part))) {
                    Some(x) => dir = x.path(),
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                continue;
            }
            for w in walkdir::WalkDir::new(&dir).max_depth(3).follow_links(false).into_iter().flatten().take(2000) {
                if !w.file_type().is_file() || !ext_ok(w.path()) {
                    continue;
                }
                let Ok(md) = w.metadata() else { continue };
                if md.len() > MAX_SIZE {
                    continue;
                }
                if let Ok(rel) = w.path().strip_prefix(game_dir) {
                    let backup = backup_of(w.path());
                    out.push(ConfigFile { path: rel.to_string_lossy().replace('\\', "/"), size: md.len(), has_backup: backup.is_file() });
                }
            }
        }
    }
    out.sort_by_key(|f| f.path.to_lowercase());
    out.dedup_by(|a, b| a.path == b.path);
    out.truncate(300);
    out
}

fn backup_of(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_os_string();
    s.push(".");
    s.push(BACKUP_EXT);
    PathBuf::from(s)
}

/// The real path of a settings file asked for by the UI, or why it is refused.
fn resolve(game_dir: &Path, rel: &str) -> Result<PathBuf, String> {
    let r = Path::new(rel);
    if rel.is_empty() || r.is_absolute() || r.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err("That is not a settings file of this game.".into());
    }
    if !list(game_dir).iter().any(|f| f.path == rel) {
        return Err("That is not a settings file of this game.".into());
    }
    let full = game_dir.join(r);
    let meta = fs::symlink_metadata(&full).map_err(|_| "That file no longer exists.".to_string())?;
    if !meta.is_file() {
        return Err("That is not a settings file of this game.".into());
    }
    let (canon, root) = (full.canonicalize().map_err(|e| e.to_string())?, game_dir.canonicalize().map_err(|e| e.to_string())?);
    if !canon.starts_with(&root) {
        return Err("That is not a settings file of this game.".into());
    }
    Ok(full)
}

const BOM: &str = "\u{feff}";
const NOT_TEXT: &str = "This file is not plain UTF-8 text, so it cannot be edited here without damaging it.";

/// The text of file bytes, or `None` when they are not UTF-8 (UTF-16 and old code pages would be garbled by a save).
fn utf8_text(bytes: &[u8]) -> Option<&str> {
    let t = std::str::from_utf8(bytes).ok()?;
    (!t.contains('\0')).then_some(t)
}

pub fn read(game_dir: &Path, rel: &str) -> Result<String, String> {
    let full = resolve(game_dir, rel)?;
    let bytes = fs::read(&full).map_err(|e| e.to_string())?;
    let text = utf8_text(&bytes).ok_or(NOT_TEXT)?;
    Ok(text.strip_prefix(BOM).unwrap_or(text).to_string())
}

/// Save new text. The very first version is copied aside once. The file is rewritten in place:
/// a mod's file that is linked into the game from the library stays linked, so the edit is kept
/// in the library and survives the next deploy. A byte order mark and Windows line endings the
/// file had are kept, because some mods' readers care.
pub fn write(game_dir: &Path, rel: &str, text: &str) -> Result<(), String> {
    let full = resolve(game_dir, rel)?;
    if text.len() as u64 > MAX_SIZE {
        return Err("That file is too large to edit here.".into());
    }
    let old = fs::read(&full).map_err(|e| format!("Could not save: {e}"))?;
    let old_text = utf8_text(&old).ok_or(NOT_TEXT)?;
    let backup = backup_of(&full);
    if !backup.exists() {
        fs::copy(&full, &backup).map_err(|e| format!("Could not keep the original: {e}"))?;
    }
    let crlf = old_text.matches("\r\n").count() > old_text.replace("\r\n", "").matches('\n').count();
    let mut out = if crlf && !text.contains('\r') { text.replace('\n', "\r\n") } else { text.to_string() };
    if old_text.starts_with(BOM) && !out.starts_with(BOM) {
        out.insert_str(0, BOM);
    }
    fs::write(&full, out).map_err(|e| format!("Could not save: {e}"))
}

/// Put the original version back (the backup stays).
pub fn restore(game_dir: &Path, rel: &str) -> Result<(), String> {
    let full = resolve(game_dir, rel)?;
    let backup = backup_of(&full);
    if !backup.is_file() {
        return Err("There is no original to go back to.".into());
    }
    fs::copy(&backup, &full).map(|_| ()).map_err(|e| format!("Could not restore: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsutil::testutil::TempDir;

    fn game() -> (TempDir, PathBuf) {
        let d = TempDir::new("cfgs");
        let g = d.join("game");
        fs::create_dir_all(g.join("BepInEx/config")).unwrap();
        fs::create_dir_all(g.join("BepInEx/plugins")).unwrap();
        fs::write(g.join("BepInEx/config/Cool.cfg"), "[General]\nSpeed = 1\n").unwrap();
        fs::write(g.join("BepInEx/plugins/Cool.dll"), "MZ").unwrap();
        fs::write(g.join("game.ini"), "x").unwrap();
        fs::create_dir_all(g.join("userdata")).unwrap();
        fs::write(g.join("userdata/Mel.json"), "{}").unwrap();
        (d, g)
    }

    #[test]
    fn only_settings_files_in_the_usual_folders_are_listed() {
        let (_d, g) = game();
        let l = list(&g);
        assert_eq!(l.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), vec!["BepInEx/config/Cool.cfg", "userdata/Mel.json"]);
    }

    #[test]
    fn edits_keep_the_first_version_and_can_be_undone() {
        let (_d, g) = game();
        assert_eq!(read(&g, "BepInEx/config/Cool.cfg").unwrap(), "[General]\nSpeed = 1\n");
        write(&g, "BepInEx/config/Cool.cfg", "[General]\nSpeed = 5\n").unwrap();
        write(&g, "BepInEx/config/Cool.cfg", "[General]\nSpeed = 9\n").unwrap();
        assert_eq!(read(&g, "BepInEx/config/Cool.cfg").unwrap(), "[General]\nSpeed = 9\n");
        assert!(list(&g)[0].has_backup);
        restore(&g, "BepInEx/config/Cool.cfg").unwrap();
        assert_eq!(read(&g, "BepInEx/config/Cool.cfg").unwrap(), "[General]\nSpeed = 1\n", "the very first version, not the previous edit");
        assert!(restore(&g, "userdata/Mel.json").is_err(), "nothing to go back to without an edit");
        // the backup file itself is not a settings file
        assert!(!list(&g).iter().any(|f| f.path.ends_with(".gantry-orig")));
    }

    #[test]
    fn a_files_byte_order_mark_and_line_endings_survive_an_edit() {
        let (_d, g) = game();
        let f = g.join("BepInEx/config/Cool.cfg");
        fs::write(&f, "\u{feff}[General]\r\nSpeed = 1\r\n").unwrap();
        assert_eq!(read(&g, "BepInEx/config/Cool.cfg").unwrap(), "[General]\r\nSpeed = 1\r\n", "the mark is not shown");
        write(&g, "BepInEx/config/Cool.cfg", "[General]\nSpeed = 2\n").unwrap();
        assert_eq!(fs::read(&f).unwrap(), "\u{feff}[General]\r\nSpeed = 2\r\n".as_bytes());
        // a file with plain line endings stays that way
        fs::write(g.join("userdata/Mel.json"), "{\n}\n").unwrap();
        write(&g, "userdata/Mel.json", "{\n \"a\": 1\n}\n").unwrap();
        assert_eq!(fs::read_to_string(g.join("userdata/Mel.json")).unwrap(), "{\n \"a\": 1\n}\n");
    }

    #[test]
    fn files_that_are_not_utf8_are_left_alone_instead_of_being_garbled() {
        let (_d, g) = game();
        let f = g.join("BepInEx/config/Cool.cfg");
        let utf16: Vec<u8> = [0xFFu8, 0xFE].into_iter().chain("[A]\r\nx=1".encode_utf16().flat_map(|u| u.to_le_bytes())).collect();
        fs::write(&f, &utf16).unwrap();
        assert!(read(&g, "BepInEx/config/Cool.cfg").unwrap_err().contains("not plain UTF-8"));
        assert!(write(&g, "BepInEx/config/Cool.cfg", "[A]\nx=2").is_err());
        assert_eq!(fs::read(&f).unwrap(), utf16, "untouched");
        assert!(!backup_of(&f).exists(), "no pointless backup of a file that was refused");
        fs::write(&f, [b'a', b'=', 0xE9]).unwrap(); // Latin-1 e-acute
        assert!(read(&g, "BepInEx/config/Cool.cfg").is_err());
    }

    #[test]
    fn linked_files_keep_their_link_so_the_edit_lives_in_the_library() {
        let (d, g) = game();
        let staged = d.join("staged.cfg");
        fs::write(&staged, "A=1").unwrap();
        fs::remove_file(g.join("BepInEx/config/Cool.cfg")).unwrap();
        fs::hard_link(&staged, g.join("BepInEx/config/Cool.cfg")).unwrap();
        write(&g, "BepInEx/config/Cool.cfg", "A=2").unwrap();
        assert_eq!(fs::read_to_string(&staged).unwrap(), "A=2");
    }

    #[test]
    fn nothing_outside_the_settings_folders_can_be_touched() {
        let (d, g) = game();
        fs::write(d.join("secret.cfg"), "S").unwrap();
        for bad in ["../secret.cfg", "/etc/passwd", "game.ini", "BepInEx/plugins/Cool.dll", "BepInEx/config/../../game.ini", "", "BepInEx/config/missing.cfg"] {
            assert!(read(&g, bad).is_err(), "{bad}");
            assert!(write(&g, bad, "x").is_err(), "{bad}");
        }
        assert_eq!(fs::read_to_string(g.join("game.ini")).unwrap(), "x");
        // a link that leads out of the game is refused
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(d.join("secret.cfg"), g.join("BepInEx/config/Evil.cfg")).unwrap();
            assert!(read(&g, "BepInEx/config/Evil.cfg").is_err());
            assert!(write(&g, "BepInEx/config/Evil.cfg", "x").is_err());
            assert_eq!(fs::read_to_string(d.join("secret.cfg")).unwrap(), "S");
        }
    }
}
