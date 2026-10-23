//! Games installed through other launchers: Heroic (Epic, GOG, Amazon, sideloaded) and Lutris.
//!
//! Both are read-only: Gantry lists what they have installed, with the Wine prefix and Proton build
//! they chose when there is one, so the games can be modded and launched like any other. The file
//! formats are those the launchers write (Heroic's JSON stores, Lutris' `pga.db` and game YAMLs).

use crate::engine::{GameEntry, RigEngine};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A game as a launcher describes it, before it becomes a `GameEntry`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LauncherGame {
    /// `heroic` or `lutris`
    pub source: &'static str,
    /// Store inside the launcher (`epic`, `gog`, `sideload`, `amazon`, or the Lutris runner).
    pub shop: String,
    pub id: String,
    pub title: String,
    pub install_dir: PathBuf,
    pub exe: PathBuf,
    pub wine_prefix: String,
    /// Proton build (a folder holding `proton`), when the launcher uses one.
    pub proton: String,
}

/// Where the other launchers keep their data (each `None` when not installed).
#[derive(Clone, Debug, Default)]
pub struct LauncherPaths {
    pub heroic: Option<PathBuf>,
    /// `(data dir with pga.db, config dir with games/*.yml)`
    pub lutris: Option<(PathBuf, PathBuf)>,
}

impl LauncherPaths {
    pub fn detect(home: &Path) -> Self {
        Self { heroic: heroic_dir(home), lutris: lutris_dir(home).map(|d| (d, home.join(".config/lutris"))) }
    }

    pub fn games(&self) -> Vec<GameEntry> {
        library_games(self.heroic.as_deref(), self.lutris.as_ref().map(|(a, b)| (a.as_path(), b.as_path())))
    }

    pub fn signature(&self) -> Vec<(String, String)> {
        signature(self.heroic.as_deref(), self.lutris.as_ref().map(|(a, b)| (a.as_path(), b.as_path())))
    }
}

pub fn heroic_dir(home: &Path) -> Option<PathBuf> {
    [
        home.join(".config/heroic"),
        home.join(".var/app/com.heroicgameslauncher.hgl/config/heroic"),
        home.join("AppData/Roaming/heroic"),
        home.join("Library/Application Support/heroic"),
    ]
        .into_iter()
        .find(|d| d.is_dir())
}

pub fn lutris_dir(home: &Path) -> Option<PathBuf> {
    let d = home.join(".local/share/lutris");
    d.join("pga.db").is_file().then_some(d)
}

fn read_json(p: &Path) -> Option<Value> {
    serde_json::from_slice(&fs::read(p).ok()?).ok()
}

fn s(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").trim().to_string()
}

/// Wine prefix and Proton build Heroic stored for a game (`GamesConfig/<app>.json`).
fn heroic_game_config(dir: &Path, app: &str) -> (String, String) {
    let Some(v) = read_json(&dir.join("GamesConfig").join(format!("{app}.json"))) else { return (String::new(), String::new()) };
    let c = v.get(app).unwrap_or(&v);
    let prefix = s(c, "winePrefix");
    let wine = c.get("wineVersion").cloned().unwrap_or(Value::Null);
    let proton = if s(&wine, "type") == "proton" {
        let bin = PathBuf::from(s(&wine, "bin"));
        bin.parent().filter(|p| p.join("proton").is_file()).map(|p| p.to_string_lossy().to_string()).unwrap_or_default()
    } else {
        String::new()
    };
    (prefix, proton)
}

/// Everything Heroic has installed whose executable still exists.
pub fn heroic_games(dir: &Path) -> Vec<LauncherGame> {
    let mut out = Vec::new();
    let mut push = |shop: &str, id: String, title: String, install: PathBuf, exe: PathBuf| {
        if id.is_empty() || !exe.is_file() {
            return;
        }
        let (wine_prefix, proton) = heroic_game_config(dir, &id);
        let title = if title.is_empty() { install.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| id.clone()) } else { title };
        out.push(LauncherGame { source: "heroic", shop: shop.into(), id, title, install_dir: install, exe, wine_prefix, proton });
    };

    // Epic (legendary): { "<app>": { title, install_path, executable, ... } }
    if let Some(Value::Object(m)) = read_json(&dir.join("legendaryConfig/legendary/installed.json")) {
        for (app, g) in m {
            let install = PathBuf::from(s(&g, "install_path"));
            let exe = install.join(s(&g, "executable").replace('\\', "/"));
            push("epic", app, s(&g, "title"), install, exe);
        }
    }
    // GOG: { "installed": [ { appName, install_path, ... } ] } with titles in the library cache
    if let Some(v) = read_json(&dir.join("gog_store/installed.json")) {
        let titles: std::collections::HashMap<String, String> = read_json(&dir.join("store_cache/gog_library.json"))
            .and_then(|l| l.get("games").and_then(Value::as_array).cloned())
            .map(|a| a.iter().map(|g| (s(g, "app_name"), s(g, "title"))).collect())
            .unwrap_or_default();
        for g in v.get("installed").and_then(Value::as_array).cloned().unwrap_or_default() {
            let app = s(&g, "appName");
            let install = PathBuf::from(s(&g, "install_path"));
            let exe = first_exe(&install, &s(&g, "executable"));
            push("gog", app.clone(), titles.get(&app).cloned().unwrap_or_default(), install, exe);
        }
    }
    // Games the user added by hand
    if let Some(v) = read_json(&dir.join("sideload_apps/library.json")) {
        for g in v.get("games").and_then(Value::as_array).cloned().unwrap_or_default() {
            let exe = PathBuf::from(g.get("install").map(|i| s(i, "executable")).unwrap_or_default());
            let install = exe.parent().map(Path::to_path_buf).unwrap_or_default();
            push("sideload", s(&g, "app_name"), s(&g, "title"), install, exe);
        }
    }
    out.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()).then_with(|| a.id.cmp(&b.id)));
    out
}

/// A GOG game's executable: the recorded one, else the only `.exe` at the top of the install folder.
fn first_exe(install: &Path, recorded: &str) -> PathBuf {
    if !recorded.is_empty() {
        let p = install.join(recorded.replace('\\', "/"));
        if p.is_file() {
            return p;
        }
    }
    let mut exes: Vec<PathBuf> = fs::read_dir(install)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e.eq_ignore_ascii_case("exe")).unwrap_or(false))
        .filter(|p| !crate::engine::is_auxiliary_exe(&p.file_name().unwrap_or_default().to_string_lossy().to_lowercase()))
        .collect();
    exes.sort();
    exes.into_iter().next().unwrap_or_default()
}

// ───────────────────────────── Lutris ─────────────────────────────

/// `key: value` inside the top-level `section:` of a Lutris game YAML (just what is needed).
pub fn yaml_value(yaml: &str, section: &str, key: &str) -> Option<String> {
    let mut inside = false;
    for line in yaml.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if indent == 0 {
            inside = line.trim_end().trim_end_matches(':') == section && line.trim_end().ends_with(':');
            continue;
        }
        if inside {
            if let Some((k, v)) = line.trim().split_once(':') {
                if k.trim() == key {
                    let v = v.trim().trim_matches('"').trim_matches('\'').trim();
                    return (!v.is_empty()).then(|| v.to_string());
                }
            }
        }
    }
    None
}

/// Installed Lutris games (needs the `sqlite3` tool; without it Lutris games are simply not listed).
pub fn lutris_games(data_dir: &Path, config_dir: &Path) -> Vec<LauncherGame> {
    let out = Command::new("sqlite3")
        .arg("-readonly")
        .arg("-json")
        .arg(data_dir.join("pga.db"))
        .arg("SELECT id, slug, name, runner, directory, configpath FROM games WHERE installed = 1")
        .output();
    let Ok(out) = out else { return Vec::new() };
    let Ok(Value::Array(rows)) = serde_json::from_slice::<Value>(&out.stdout) else { return Vec::new() };
    let mut games = Vec::new();
    for r in rows {
        let cfg = s(&r, "configpath");
        let yaml = fs::read_to_string(config_dir.join("games").join(format!("{cfg}.yml"))).unwrap_or_default();
        let dir = PathBuf::from(s(&r, "directory"));
        let exe_raw = yaml_value(&yaml, "game", "exe").unwrap_or_default();
        let exe = if Path::new(&exe_raw).is_absolute() { PathBuf::from(&exe_raw) } else { dir.join(&exe_raw) };
        if exe_raw.is_empty() || !exe.is_file() {
            continue;
        }
        let id = r.get("id").map(|v| v.to_string().trim_matches('"').to_string()).unwrap_or_default();
        games.push(LauncherGame {
            source: "lutris",
            shop: s(&r, "runner"),
            id,
            title: s(&r, "name"),
            install_dir: if dir.as_os_str().is_empty() { exe.parent().map(Path::to_path_buf).unwrap_or_default() } else { dir },
            exe,
            wine_prefix: yaml_value(&yaml, "game", "prefix").unwrap_or_default(),
            proton: String::new(),
        });
    }
    games.sort_by_key(|a| a.title.to_lowercase());
    games
}

// ───────────────────────────── to a Gantry game ─────────────────────────────

fn safe_part(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect()
}

pub fn to_entry(g: &LauncherGame) -> Option<GameEntry> {
    let mut e = if g.exe.starts_with(&g.install_dir) && g.install_dir.is_dir() {
        RigEngine::entry_for_root(&g.exe, &g.install_dir)?
    } else {
        RigEngine::resolve_executable(&g.exe)?
    };
    e.appid = format!("{}_{}_{}", g.source, safe_part(&g.shop), safe_part(&g.id));
    if !g.title.is_empty() {
        e.name = g.title.clone();
    }
    e.is_custom = false;
    e.source = g.source.into();
    e.steam_appid = String::new();
    e.proton_path = g.proton.clone();
    if !g.wine_prefix.is_empty() {
        let base = PathBuf::from(&g.wine_prefix);
        let (compat, pfx) = if base.join("pfx").is_dir() || !base.join("drive_c").is_dir() { (base.clone(), base.join("pfx")) } else { (base.clone(), base) };
        e.has_pfx = pfx.is_dir();
        e.compatdata = compat.to_string_lossy().to_string();
        e.pfx = pfx.to_string_lossy().to_string();
    }
    Some(e)
}

/// Every installed game of the launchers found, as Gantry games.
pub fn library_games(heroic: Option<&Path>, lutris: Option<(&Path, &Path)>) -> Vec<GameEntry> {
    let mut all: Vec<LauncherGame> = heroic.map(heroic_games).unwrap_or_default();
    if let Some((data, config)) = lutris {
        all.extend(lutris_games(data, config));
    }
    all.iter().filter_map(to_entry).collect()
}

/// `(game id, executable)` of everything installed; cheap enough to poll.
pub fn signature(heroic: Option<&Path>, lutris: Option<(&Path, &Path)>) -> Vec<(String, String)> {
    let mut all: Vec<LauncherGame> = heroic.map(heroic_games).unwrap_or_default();
    if let Some((data, config)) = lutris {
        all.extend(lutris_games(data, config));
    }
    let mut sig: Vec<(String, String)> = all.iter().map(|g| (format!("{}:{}:{}", g.source, g.shop, g.id), g.exe.to_string_lossy().to_string())).collect();
    sig.sort();
    sig
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsutil::testutil::TempDir;

    fn write(p: &Path, s: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, s).unwrap();
    }

    /// A path as it must appear inside JSON text (backslashes escaped).
    fn jp(p: &Path) -> String {
        p.to_string_lossy().replace('\\', "\\\\")
    }

    fn heroic_fixture(tag: &str) -> (TempDir, PathBuf) {
        let d = TempDir::new(tag);
        let h = d.join("heroic");
        // Epic: executable relative to the install path
        write(&d.join("games/Epic Game/bin/game.exe"), "MZ");
        write(
            &h.join("legendaryConfig/legendary/installed.json"),
            &format!(r#"{{"Fortnite_App":{{"title":"Epic Game","install_path":"{}","executable":"bin\\game.exe","platform":"Windows"}},"Gone":{{"title":"Missing","install_path":"/nope","executable":"x.exe"}}}}"#, jp(&d.join("games/Epic Game"))),
        );
        // GOG: title from the library cache, exe guessed (the only top-level exe)
        write(&d.join("games/GOG Game/launcher.exe"), "MZ");
        write(&d.join("games/GOG Game/unins000.exe"), "MZ");
        write(&h.join("gog_store/installed.json"), &format!(r#"{{"installed":[{{"appName":"1207658930","install_path":"{}","platform":"windows"}}]}}"#, jp(&d.join("games/GOG Game"))));
        write(&h.join("store_cache/gog_library.json"), r#"{"games":[{"app_name":"1207658930","title":"The GOG Title"}]}"#);
        // sideloaded, with a Proton build and prefix
        write(&d.join("side/Side Game/side.exe"), "MZ");
        write(&h.join("sideload_apps/library.json"), &format!(r#"{{"games":[{{"app_name":"abc123","title":"Side Game","runner":"sideload","install":{{"executable":"{}","platform":"Windows"}}}}]}}"#, jp(&d.join("side/Side Game/side.exe"))));
        write(&d.join("tools/GE-Proton9/proton"), "#!/bin/sh");
        fs::create_dir_all(d.join("prefixes/side/pfx")).unwrap();
        write(
            &h.join("GamesConfig/abc123.json"),
            &format!(r#"{{"abc123":{{"winePrefix":"{}","wineVersion":{{"bin":"{}","name":"GE-Proton9","type":"proton"}}}}}}"#, jp(&d.join("prefixes/side")), jp(&d.join("tools/GE-Proton9/proton"))),
        );
        (d, h)
    }

    #[test]
    fn heroic_epic_gog_and_sideloaded_games_are_listed_with_their_prefix_and_proton() {
        let (d, h) = heroic_fixture("heroic");
        let g = heroic_games(&h);
        assert_eq!(g.iter().map(|x| x.title.as_str()).collect::<Vec<_>>(), vec!["Epic Game", "Side Game", "The GOG Title"], "uninstalled/missing games are skipped");
        let epic = &g[0];
        assert_eq!((epic.shop.as_str(), Path::new(&epic.exe).ends_with("bin/game.exe")), ("epic", true));
        let gog = &g[2];
        assert!(Path::new(&gog.exe).ends_with("launcher.exe"), "installer helpers are not the game: {:?}", gog.exe);
        let side = &g[1];
        assert_eq!(Path::new(&side.wine_prefix), d.join("prefixes/side"));
        assert_eq!(Path::new(&side.proton), d.join("tools/GE-Proton9"));

        let e = library_games(Some(&h), None);
        assert_eq!(e.len(), 3);
        let side = e.iter().find(|x| x.name == "Side Game").unwrap();
        assert_eq!((side.source.as_str(), side.appid.as_str(), side.launches_directly(), side.has_pfx), ("heroic", "heroic_sideload_abc123", true, true));
        assert!(Path::new(&side.pfx).ends_with("prefixes/side/pfx") && Path::new(&side.proton_path).ends_with("GE-Proton9"));
        assert!(e.iter().all(|x| x.is_installed && !x.is_custom));
        assert_eq!(signature(Some(&h), None).len(), 3);
    }

    #[test]
    fn missing_or_broken_launcher_data_is_never_an_error() {
        let d = TempDir::new("heroic-empty");
        assert!(heroic_games(&d.join("none")).is_empty());
        write(&d.join("h/legendaryConfig/legendary/installed.json"), "{ broken");
        write(&d.join("h/gog_store/installed.json"), "[]");
        assert!(heroic_games(&d.join("h")).is_empty());
        assert!(library_games(None, None).is_empty());
        assert!(heroic_dir(&d.join("home")).is_none() && lutris_dir(&d.join("home")).is_none());
        fs::create_dir_all(d.join("home/.config/heroic")).unwrap();
        assert!(heroic_dir(&d.join("home")).is_some());
    }

    #[test]
    fn lutris_yaml_is_read_by_section_and_a_real_database_by_sqlite() {
        let yaml = "game:\n  exe: /games/x/x.exe\n  prefix: \"/pfx/x\"\n  working_dir: /games/x\nwine:\n  version: lutris-9\nsystem:\n  exe: nope\n";
        assert_eq!(yaml_value(yaml, "game", "exe").as_deref(), Some("/games/x/x.exe"));
        assert_eq!(yaml_value(yaml, "game", "prefix").as_deref(), Some("/pfx/x"));
        assert_eq!(yaml_value(yaml, "wine", "version").as_deref(), Some("lutris-9"));
        assert_eq!(yaml_value(yaml, "system", "prefix"), None);
        assert_eq!(yaml_value("", "game", "exe"), None);

        if crate::fsutil::find_in_path("sqlite3").is_none() {
            eprintln!("skipping the database part: no sqlite3");
            return;
        }
        let d = TempDir::new("lutris");
        write(&d.join("games/Lu Game/lu.exe"), "MZ");
        let db = d.join("data/pga.db");
        fs::create_dir_all(db.parent().unwrap()).unwrap();
        let sql = format!(
            "CREATE TABLE games (id INTEGER PRIMARY KEY, slug TEXT, name TEXT, runner TEXT, installed INTEGER, directory TEXT, configpath TEXT); \
             INSERT INTO games VALUES (7,'lu-game','Lu Game','wine',1,'{dir}','lu-game-1'); \
             INSERT INTO games VALUES (8,'other','Not Installed','wine',0,'/x','other-2'); \
             INSERT INTO games VALUES (9,'ghost','Ghost','wine',1,'/nowhere','ghost-3');",
            dir = d.join("games/Lu Game").display()
        );
        assert!(Command::new("sqlite3").arg(&db).arg(sql).status().unwrap().success());
        write(&d.join("config/games/lu-game-1.yml"), "game:\n  exe: lu.exe\n  prefix: /pfx/lu\n");
        let g = lutris_games(&d.join("data"), &d.join("config"));
        assert_eq!(g.len(), 1, "{g:?}");
        assert_eq!((g[0].title.as_str(), g[0].id.as_str(), g[0].wine_prefix.as_str()), ("Lu Game", "7", "/pfx/lu"));
        let e = library_games(None, Some((&d.join("data"), &d.join("config"))));
        assert_eq!((e[0].appid.as_str(), e[0].source.as_str()), ("lutris_wine_7", "lutris"));
    }
}
