//! Hydra Launcher integration: reads Hydra's own library database (read-only) so the games
//! it manages show up in rig with their real install folder, Wine prefix, Proton build and
//! launch options, and rig's mod / trainer / repair tooling works on them.

use crate::engine::{GameEntry, RigEngine};
use crate::leveldb;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Hydra's data directory (`~/.config/hydralauncher`, or its Flatpak twin) when present.
pub fn find_dir(home: &Path) -> Option<PathBuf> {
    [
        home.join(".config/hydralauncher"),
        home.join(".var/app/io.github.hydralauncher.hydra/config/hydralauncher"),
        home.join("AppData/Roaming/hydralauncher"),
        home.join("Library/Application Support/hydralauncher"),
    ]
    .into_iter()
    .find(|d| d.join("hydra-db").is_dir())
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HydraGame {
    pub shop: String,
    pub object_id: String,
    pub title: String,
    pub executable_path: String,
    pub wine_prefix_path: String,
    pub proton_path: String,
    pub launch_options: String,
    pub is_deleted: bool,
    pub download_path: String,
    pub folder_name: String,
}

fn s(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").trim().to_string()
}

/// Parse the library out of Hydra's key/value store. Pure over the map, so it is testable
/// without a database on disk.
pub fn parse_library(db: &HashMap<Vec<u8>, Vec<u8>>) -> Vec<HydraGame> {
    let json_of = |prefix: &str, k: &[u8]| -> Option<(String, Value)> {
        let key = std::str::from_utf8(k).ok()?;
        let id = key.strip_prefix(prefix)?;
        let v: Value = serde_json::from_slice(db.get(k)?).ok()?;
        Some((id.to_string(), v))
    };
    let downloads: HashMap<String, Value> =
        db.keys().filter_map(|k| json_of("!downloads!", k)).collect();

    let mut out: Vec<HydraGame> = db
        .keys()
        .filter_map(|k| json_of("!games!", k))
        .map(|(id, v)| {
            let dl = downloads.get(&id);
            let (shop, object_id) = id.split_once(':').map(|(a, b)| (a.to_string(), b.to_string())).unwrap_or_default();
            HydraGame {
                shop: if s(&v, "shop").is_empty() { shop } else { s(&v, "shop") },
                object_id: if s(&v, "objectId").is_empty() { object_id } else { s(&v, "objectId") },
                title: s(&v, "title"),
                executable_path: s(&v, "executablePath"),
                wine_prefix_path: s(&v, "winePrefixPath"),
                proton_path: s(&v, "protonPath"),
                launch_options: s(&v, "launchOptions"),
                is_deleted: v.get("isDeleted").and_then(Value::as_bool).unwrap_or(false),
                download_path: dl.map(|d| s(d, "downloadPath")).unwrap_or_default(),
                folder_name: dl.map(|d| s(d, "folderName")).unwrap_or_default(),
            }
        })
        .filter(|g| !g.object_id.is_empty())
        .collect();
    out.sort_by(|a, b| (a.shop.as_str(), a.object_id.as_str()).cmp(&(b.shop.as_str(), b.object_id.as_str())));
    out
}

/// Wine prefix layout Hydra uses: `<base>/pfx` inside a compatdata-style folder.
pub fn prefix_for(g: &HydraGame, hydra_dir: &Path) -> (PathBuf, PathBuf) {
    let base = if g.wine_prefix_path.is_empty() {
        hydra_dir.join("wine-prefixes").join(&g.object_id)
    } else {
        PathBuf::from(&g.wine_prefix_path)
    };
    if base.join("pfx").is_dir() || !base.join("drive_c").is_dir() {
        let pfx = base.join("pfx");
        (base, pfx)
    } else {
        (base.clone(), base) // a bare Wine prefix: the folder itself holds drive_c
    }
}

fn safe_part(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect()
}

/// Turn a Hydra record into a rig game, or `None` when it is deleted or not installed.
pub fn to_entry(h: &HydraGame, hydra_dir: &Path) -> Option<GameEntry> {
    if h.is_deleted || h.executable_path.is_empty() {
        return None;
    }
    let exe = PathBuf::from(&h.executable_path);
    if !exe.is_file() {
        return None;
    }
    let mut e = RigEngine::resolve_executable(&exe)?;

    // Prefer the folder Hydra downloaded the game into when it contains the executable.
    if !h.download_path.is_empty() && !h.folder_name.is_empty() {
        let root = Path::new(&h.download_path).join(&h.folder_name);
        if root.is_dir() && exe.starts_with(&root) {
            e = RigEngine::entry_for_root(&exe, &root)?;
        }
    }

    let (compat, pfx) = prefix_for(h, hydra_dir);
    e.appid = format!("hydra_{}_{}", safe_part(&h.shop), safe_part(&h.object_id));
    if !h.title.is_empty() {
        e.name = h.title.clone();
    }
    e.is_custom = false;
    e.source = "hydra".into();
    e.steam_appid = if h.shop == "steam" && h.object_id.bytes().all(|b| b.is_ascii_digit()) { h.object_id.clone() } else { String::new() };
    e.proton_path = h.proton_path.clone();
    e.launch_args = if h.launch_options.contains("%command%") { String::new() } else { h.launch_options.clone() };
    e.has_pfx = pfx.is_dir();
    e.compatdata = compat.to_string_lossy().to_string();
    e.pfx = pfx.to_string_lossy().to_string();
    Some(e)
}

/// A library entry for a Hydra game whose files are not on disk (yet), so the library overview can
/// show everything you own and mark what is not installed. `None` for deleted or untitled records.
pub fn to_missing_entry(h: &HydraGame) -> Option<GameEntry> {
    if h.is_deleted || h.title.is_empty() {
        return None;
    }
    Some(GameEntry {
        appid: format!("hydra_{}_{}", safe_part(&h.shop), safe_part(&h.object_id)),
        name: h.title.clone(),
        game_dir: String::new(),
        exe_path: String::new(),
        shipping_exe: String::new(),
        bin_dir: String::new(),
        engine: "Not installed".into(),
        compatdata: String::new(),
        pfx: String::new(),
        has_pfx: false,
        is_custom: false,
        is_installed: false,
        steam_appid: if h.shop == "steam" && h.object_id.bytes().all(|b| b.is_ascii_digit()) { h.object_id.clone() } else { String::new() },
        launch_args: String::new(),
        env_vars: String::new(),
        trainer_name: String::new(),
        mod_count: 0,
        source: "hydra".into(),
        proton_path: String::new(),
        ue4ss: false, loader: String::new(), cover_id: String::new(),
    })
}

/// Every game in Hydra's library, installed or not; empty (never an error) when Hydra is absent or
/// its database cannot be read.
pub fn library_games(hydra_dir: &Path) -> Vec<GameEntry> {
    let Ok((db, _warnings)) = leveldb::read_db(&hydra_dir.join("hydra-db")) else {
        return Vec::new();
    };
    parse_library(&db)
        .iter()
        .filter_map(|h| to_entry(h, hydra_dir).or_else(|| to_missing_entry(h)))
        .collect()
}

/// `(object id, executable if installed)` of every non-deleted game; cheap enough to poll.
pub fn installed_signature(hydra_dir: &Path) -> Vec<(String, String)> {
    let Ok((db, _)) = leveldb::read_db(&hydra_dir.join("hydra-db")) else {
        return Vec::new();
    };
    let mut sig: Vec<(String, String)> = parse_library(&db)
        .into_iter()
        .filter(|g| !g.is_deleted && !g.title.is_empty())
        .map(|g| {
            let exe = if !g.executable_path.is_empty() && Path::new(&g.executable_path).is_file() { g.executable_path } else { String::new() };
            (format!("{}:{}", g.shop, g.object_id), exe)
        })
        .collect();
    sig.sort();
    sig
}

/// Proton `proton` script for a Hydra game's own build, if it exists on disk.
pub fn proton_script(g: &GameEntry) -> Option<PathBuf> {
    if g.proton_path.is_empty() {
        return None;
    }
    let p = Path::new(&g.proton_path);
    let script = if p.file_name().map(|n| n == "proton").unwrap_or(false) { p.to_path_buf() } else { p.join("proton") };
    script.is_file().then_some(script)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsutil::testutil::TempDir;
    use crate::leveldb::builder::{table, wal};
    use std::fs;

    fn fixture(tag: &str) -> (TempDir, PathBuf) {
        let d = TempDir::new(tag);
        let hd = d.join("hydralauncher");
        fs::create_dir_all(hd.join("hydra-db")).unwrap();
        // game install folders
        let ue = d.join("games/Half Sword Game/Half Sword");
        fs::create_dir_all(ue.join("HalfSwordUE5/Binaries/Win64")).unwrap();
        fs::write(ue.join("HalfSwordUE5.exe"), b"MZ").unwrap();
        let mut shipping = vec![0u8; 1024];
        shipping.extend_from_slice(b"++UE5+Release-5.4 ");
        fs::write(ue.join("HalfSwordUE5/Binaries/Win64/HalfSwordUE5-Win64-Shipping.exe"), shipping).unwrap();
        fs::create_dir_all(d.join("games/F1 2020")).unwrap();
        fs::write(d.join("games/F1 2020/F1_2020.exe"), b"MZ").unwrap();
        fs::create_dir_all(hd.join("wine-prefixes/2397300/pfx")).unwrap();
        fs::create_dir_all(d.join("proton/GE-Test")).unwrap();
        fs::write(d.join("proton/GE-Test/proton"), b"#!/bin/sh").unwrap();

        // paths go into JSON text: backslashes (Windows) must be escaped
        fn jp(p: &Path) -> String {
            p.to_string_lossy().replace('\\', "\\\\")
        }
        let g = |title: &str, id: &str, exe: &str, extra: &str| {
            format!(r#"{{"title":"{title}","objectId":"{id}","shop":"steam","isDeleted":false,"executablePath":"{exe}"{extra}}}"#)
        };
        let half = g("Half Sword", "2397300", &jp(&ue.join("HalfSwordUE5.exe")), &format!(r#","protonPath":"{}""#, jp(&d.join("proton/GE-Test"))));
        let f1 = g("F1 2020", "1080110", &jp(&d.join("games/F1 2020/F1_2020.exe")), r#","launchOptions":"-windowed","winePrefixPath":"/nonexistent/pfx""#);
        let gone = format!(r#"{{"title":"Removed","objectId":"3","shop":"steam","isDeleted":true,"executablePath":"{}"}}"#, jp(&d.join("games/F1 2020/F1_2020.exe")));
        let missing = g("Uninstalled", "4", "/no/such/game.exe", "");
        let no_exe = r#"{"title":"Never installed","objectId":"5","shop":"steam","isDeleted":false}"#.to_string();
        let dl = format!(r#"{{"downloadPath":"{}","folderName":"Half Sword"}}"#, jp(&d.join("games/Half Sword Game")));
        let old_title = g("Old title", "1080110", "/old/path.exe", "");

        // old state lives in a compressed table, newer edits and a deletion in the log
        fs::write(hd.join("hydra-db/000010.ldb"), table(
            &[(b"!games!steam:1080110", 5, 1, old_title.as_bytes()), (b"!games!steam:3", 6, 1, gone.as_bytes())],
            1, true)).unwrap();
        let ops: Vec<(Vec<u8>, Option<Vec<u8>>)> = vec![
            (b"!games!steam:2397300".to_vec(), Some(half.into_bytes())),
            (b"!games!steam:1080110".to_vec(), Some(f1.into_bytes())),
            (b"!games!steam:4".to_vec(), Some(missing.into_bytes())),
            (b"!games!steam:5".to_vec(), Some(no_exe.into_bytes())),
            (b"!downloads!steam:2397300".to_vec(), Some(dl.into_bytes())),
            (b"!gameShopCache!steam:2397300:english".to_vec(), Some(b"{}".to_vec())),
        ];
        let borrowed: Vec<(&[u8], Option<&[u8]>)> = ops.iter().map(|(k, v)| (&k[..], v.as_deref())).collect();
        fs::write(hd.join("hydra-db/000011.log"), wal(&[(100, borrowed)])).unwrap();
        (d, hd)
    }

    #[test]
    fn reads_the_library_and_resolves_installed_games() {
        let (d, hd) = fixture("hy1");
        let lib = {
            let (db, warn) = leveldb::read_db(&hd.join("hydra-db")).unwrap();
            assert!(warn.is_empty(), "{warn:?}");
            parse_library(&db)
        };
        assert_eq!(lib.len(), 5, "{:?}", lib.iter().map(|g| &g.title).collect::<Vec<_>>());
        let f1 = lib.iter().find(|g| g.object_id == "1080110").unwrap();
        assert_eq!(f1.title, "F1 2020", "log entry replaced the older table entry");
        assert_eq!(f1.launch_options, "-windowed");

        let games = library_games(&hd);
        let names: Vec<_> = games.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names.len(), 4, "deleted games are skipped, uninstalled ones are listed as such: {names:?}");
        for n in ["Uninstalled", "Never installed"] {
            let g = games.iter().find(|g| g.name == n).unwrap();
            assert!(!g.is_installed && g.game_dir.is_empty() && g.engine == "Not installed", "{n}");
        }
        assert!(games.iter().filter(|g| g.is_installed).count() == 2);

        let hs = games.iter().find(|g| g.name == "Half Sword").unwrap();
        assert_eq!(hs.appid, "hydra_steam_2397300");
        assert_eq!(hs.source, "hydra");
        assert!(hs.launches_directly() && !hs.is_custom);
        assert_eq!(hs.steam_appid, "2397300");
        assert!(hs.engine.starts_with("Unreal Engine 5.4"), "{}", hs.engine);
        assert_eq!(Path::new(&hs.game_dir), d.join("games/Half Sword Game/Half Sword"));
        assert!(Path::new(&hs.bin_dir).ends_with("HalfSwordUE5/Binaries/Win64"));
        assert!(hs.has_pfx && Path::new(&hs.pfx).ends_with("wine-prefixes/2397300/pfx") && Path::new(&hs.compatdata).ends_with("wine-prefixes/2397300"));
        assert_eq!(proton_script(hs).unwrap(), d.join("proton/GE-Test/proton"));

        let f1 = games.iter().find(|g| g.name == "F1 2020").unwrap();
        assert_eq!(f1.launch_args, "-windowed");
        assert!(!f1.has_pfx && Path::new(&f1.pfx) == Path::new("/nonexistent/pfx/pfx"));
        assert!(proton_script(f1).is_none());
    }

    #[test]
    fn percent_command_launch_options_are_not_treated_as_arguments() {
        let (_d, hd) = fixture("hy2");
        let mut lib = parse_library(&leveldb::read_db(&hd.join("hydra-db")).unwrap().0);
        let g = lib.iter_mut().find(|g| g.object_id == "1080110").unwrap();
        g.launch_options = "FOO=1 %command% -x".into();
        assert_eq!(to_entry(g, &hd).unwrap().launch_args, "");
    }

    #[test]
    fn absent_or_unreadable_hydra_is_harmless() {
        let d = TempDir::new("hy3");
        assert!(library_games(&d.join("none")).is_empty());
        fs::create_dir_all(d.join("h/hydra-db")).unwrap();
        fs::write(d.join("h/hydra-db/1.ldb"), b"garbage").unwrap();
        assert!(library_games(&d.join("h")).is_empty());
        assert_eq!(find_dir(d.path()), None);
        fs::create_dir_all(d.join(".config/hydralauncher/hydra-db")).unwrap();
        assert_eq!(find_dir(d.path()), Some(d.join(".config/hydralauncher")));
    }

    #[test]
    fn prefix_layouts() {
        let d = TempDir::new("hy4");
        let hd = d.join("hd");
        let mut g = HydraGame { object_id: "9".into(), ..Default::default() };
        // not created yet: Hydra's default location, Proton will create pfx
        let (c, p) = prefix_for(&g, &hd);
        assert_eq!((c, p), (hd.join("wine-prefixes/9"), hd.join("wine-prefixes/9/pfx")));
        // a bare Wine prefix keeps its drive_c at the top
        fs::create_dir_all(d.join("bare/drive_c")).unwrap();
        g.wine_prefix_path = d.join("bare").to_string_lossy().to_string();
        let (c, p) = prefix_for(&g, &hd);
        assert_eq!(c, p);
        // compatdata style
        fs::create_dir_all(d.join("compat/pfx/drive_c")).unwrap();
        g.wine_prefix_path = d.join("compat").to_string_lossy().to_string();
        assert_eq!(prefix_for(&g, &hd).1, d.join("compat/pfx"));
    }

    #[test]
    fn scan_merges_hydra_without_duplicating_known_games() {
        use crate::engine::RigPaths;
        use crate::repair::RigConfigData;
        let (d, hd) = fixture("hy5");
        let paths = RigPaths {
            custom_games_file: d.join("custom.json"),
            config_file: d.join("cfg.json"),
            cache_dir: d.join("cache/injectors"),
            engine_cache_file: d.join("cache/engine.json"),
            webview_profile_dir: d.join("cache/wv"),
            hydra_dir: Some(hd.clone()),
            launchers: Default::default(),
        };
        let mut cfg = RigConfigData::default();
        cfg.launch_options.insert("hydra_steam_1080110".into(), crate::repair::LaunchOpts { launch_args: "-saved".into(), env_vars: "A=1".into() });
        let games = RigEngine::scan_libraries(&[], &paths, &cfg);
        assert_eq!(games.iter().filter(|g| g.is_installed).count(), 2);
        assert_eq!(games.len(), 4, "plus the two Hydra games that are not installed");
        assert_eq!(games.iter().find(|g| g.name == "F1 2020").unwrap().launch_args, "-saved", "rig's own settings win");
        assert_eq!(games.iter().find(|g| g.name == "F1 2020").unwrap().env_vars, "A=1");

        // a custom entry for the same executable suppresses the Hydra duplicate
        let custom = RigEngine::resolve_executable(&d.join("games/F1 2020/F1_2020.exe")).unwrap();
        let mut custom = custom;
        custom.name = "F1 (mine)".into();
        fs::write(&paths.custom_games_file, serde_json::to_string(&vec![custom]).unwrap()).unwrap();
        let games = RigEngine::scan_libraries(&[], &paths, &cfg);
        assert_eq!(games.iter().filter(|g| g.is_installed).count(), 2);
        assert!(games.iter().any(|g| g.name == "F1 (mine)") && !games.iter().any(|g| g.name == "F1 2020"));
    }
}

#[cfg(test)]
mod real_world {
    use super::*;

    /// Runs against the developer's own Hydra install when there is one (read-only).
    #[test]
    fn the_local_hydra_database_is_readable() {
        let Some(hd) = find_dir(&crate::fsutil::home_dir()) else {
            eprintln!("no Hydra install here; skipping");
            return;
        };
        let (db, warnings) = leveldb::read_db(&hd.join("hydra-db")).expect("hydra-db readable");
        let lib = parse_library(&db);
        eprintln!("hydra: {} keys, {} games, warnings: {:?}", db.len(), lib.len(), warnings);
        for g in &lib {
            eprintln!("  {:<40} {}:{} deleted={} exe={}", g.title, g.shop, g.object_id, g.is_deleted, g.executable_path);
        }
        for g in library_games(&hd) {
            eprintln!("  -> rig entry: {} [{}] {} | {} | pfx={} proton={}", g.name, g.appid, g.engine, g.game_dir, g.pfx, g.proton_path);
        }
        assert!(db.keys().any(|k| k.starts_with(b"!games!")) || lib.is_empty());
    }
}
