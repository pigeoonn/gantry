//! A growing list of known problems: what a line in a log means, why it happens and what to do.
//! The list ships inside Gantry (`data/errors.json`) and can be extended or corrected by the user
//! with an `errors.json` next to Gantry's settings, entries with the same `id` replace the built-in ones.
//! Texts may be a plain string or a map of language code to text.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const BUILT_IN: &str = include_str!("../data/errors.json");
const MAX_PATTERNS: usize = 200;
const MAX_TEXT: usize = 2000;
/// Logs older than this say nothing about the game as it is now.
const FRESH: Duration = Duration::from_secs(14 * 24 * 3600);

#[derive(Clone, Debug, Deserialize)]
pub struct Pattern {
    pub id: String,
    /// Which logs to look in: "bepinex", "melonloader", "player", "ue4ss" or "proton".
    #[serde(default)]
    pub logs: Vec<String>,
    /// A line matches when it contains any of these (letter case does not matter) ...
    pub contains: Vec<String>,
    /// ... and, when this is not empty, also any of these.
    #[serde(default)]
    pub also: Vec<String>,
    #[serde(default = "warn")]
    pub level: String,
    pub title: Value,
    #[serde(default)]
    pub cause: Value,
    #[serde(default)]
    pub fix: Value,
}

fn warn() -> String {
    "warn".into()
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Finding {
    pub id: String,
    pub level: String,
    pub title: Value,
    pub cause: Value,
    pub fix: Value,
    /// The log line that matched, shortened.
    pub evidence: String,
    /// Which log it came from.
    pub source: String,
}

fn text_ok(v: &Value) -> bool {
    match v {
        Value::String(s) => s.len() <= MAX_TEXT,
        Value::Object(m) => m.values().all(|x| x.as_str().map(|s| s.len() <= MAX_TEXT).unwrap_or(false)),
        Value::Null => true,
        _ => false,
    }
}

fn parse(text: &str) -> Vec<Pattern> {
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(text) else { return Vec::new() };
    items
        .into_iter()
        .filter_map(|v| serde_json::from_value::<Pattern>(v).ok())
        .filter(|p| !p.id.is_empty() && !p.contains.is_empty() && p.contains.iter().all(|c| !c.trim().is_empty()) && ["warn", "bad", "info"].contains(&p.level.as_str()) && text_ok(&p.title) && text_ok(&p.cause) && text_ok(&p.fix))
        .collect()
}

/// The built-in patterns, with the user's file (if any) applied on top.
pub fn patterns(user_file: Option<&Path>) -> Vec<Pattern> {
    let mut all = parse(BUILT_IN);
    if let Some(extra) = user_file.and_then(|p| fs::read_to_string(p).ok()) {
        for p in parse(&extra) {
            match all.iter().position(|x| x.id == p.id) {
                Some(i) => all[i] = p,
                None => all.push(p),
            }
        }
    }
    all.truncate(MAX_PATTERNS);
    all
}

/// Look for known problems in logs (`(label, lines)`); each pattern reports its most recent matching line once.
pub fn scan(patterns: &[Pattern], logs: &[(String, Vec<String>)]) -> Vec<Finding> {
    let mut out = Vec::new();
    for p in patterns {
        let needles: Vec<String> = p.contains.iter().map(|c| c.to_lowercase()).collect();
        let also: Vec<String> = p.also.iter().map(|c| c.to_lowercase()).collect();
        let hit = logs.iter().filter(|(label, _)| p.logs.is_empty() || p.logs.iter().any(|l| l == label)).find_map(|(label, lines)| {
            lines.iter().rev().find(|line| {
                let l = line.to_lowercase();
                needles.iter().any(|n| l.contains(n)) && (also.is_empty() || also.iter().any(|a| l.contains(a)))
            }).map(|line| (label.clone(), line.trim().chars().take(200).collect::<String>()))
        });
        if let Some((source, evidence)) = hit {
            out.push(Finding { id: p.id.clone(), level: p.level.clone(), title: p.title.clone(), cause: p.cause.clone(), fix: p.fix.clone(), evidence, source });
        }
    }
    out
}

fn tail(p: &Path, n: usize) -> Option<Vec<String>> {
    let meta = fs::metadata(p).ok()?;
    if SystemTime::now().duration_since(meta.modified().ok()?).map(|a| a > FRESH).unwrap_or(false) {
        return None;
    }
    let bytes = fs::read(p).ok()?;
    let start = bytes.len().saturating_sub(150_000);
    let text = String::from_utf8_lossy(&bytes[start..]).to_string();
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    Some(lines[lines.len().saturating_sub(n)..].to_vec())
}

/// The logs a game and its loaders write, as `(label, last lines)`.
pub fn collect_logs(game_dir: &Path, prefixes: &[PathBuf], ue4ss_log: &str, proton_log: Option<&Path>) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    let mut add = |label: &str, lines: Option<Vec<String>>| {
        if let Some(l) = lines.filter(|l| !l.is_empty()) {
            out.push((label.to_string(), l));
        }
    };
    add("bepinex", tail(&game_dir.join("BepInEx/LogOutput.log"), 800));
    add("melonloader", tail(&game_dir.join("MelonLoader/Latest.log"), 800));
    for p in prefixes {
        if let Some(log) = crate::logdoctor::newest_player_log(p) {
            add("player", tail(&log, 800));
            break;
        }
    }
    if !ue4ss_log.trim().is_empty() {
        add("ue4ss", Some(ue4ss_log.lines().map(str::to_string).collect()));
    }
    if let Some(p) = proton_log {
        add("proton", tail(p, 1500));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsutil::testutil::TempDir;

    fn log(label: &str, lines: &[&str]) -> (String, Vec<String>) {
        (label.to_string(), lines.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn the_built_in_list_is_valid_and_every_entry_can_match_its_own_example() {
        let all = patterns(None);
        assert!(all.len() >= 26, "{}", all.len());
        let mut ids: Vec<&str> = all.iter().map(|p| p.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), all.len(), "ids are unique");
        for p in &all {
            // every text is there in every language the interface has, and says something
            for (what, v) in [("title", &p.title), ("cause", &p.cause), ("fix", &p.fix)] {
                for lang in ["en", "ro", "it", "fr", "es", "ru", "de", "pt", "pl", "tr"] {
                    let t = v.get(lang).and_then(Value::as_str).unwrap_or("");
                    assert!(t.len() > 8, "{} {what} has no {lang} text", p.id);
                    assert!(!t.contains('{') || lang == "en", "{} {what} {lang} still has a placeholder: {t}", p.id);
                }
                assert!(v.get("en").and_then(Value::as_str).map(|e| e.len() <= 400).unwrap_or(false), "{} {what}: keep it short", p.id);
            }
            // a line built from the first needle (and the first 'also') is found in a log it is meant for
            let line = format!("{} {}", p.contains[0], p.also.first().cloned().unwrap_or_default());
            let label = p.logs.first().cloned().unwrap_or_else(|| "player".into());
            let found = scan(std::slice::from_ref(p), &[(label, vec![line])]);
            assert_eq!(found.len(), 1, "{}", p.id);
        }
    }

    #[test]
    fn real_looking_log_lines_are_recognised_and_only_in_the_right_logs() {
        let all = patterns(None);
        let ids = |logs: &[(String, Vec<String>)]| scan(&all, logs).into_iter().map(|f| f.id).collect::<Vec<_>>();
        assert_eq!(ids(&[log("bepinex", &["[Info   :   BepInEx] Loading [A 1.0]", "[Error  :   BepInEx] Error loading [Cool 1.0.0] : Could not load file or assembly 'Lib, Version=1.0' or one of its dependencies"])]), vec!["bepinex_missing_assembly", "bepinex_plugin_failed"]);
        assert_eq!(ids(&[log("melonloader", &["[21:11:34] [Il2CppAssemblyGenerator] [Error] Cpp2IL failed with exit code 1"])]), vec!["melon_il2cpp_failed"]);
        // the same words in a log the pattern does not read do nothing
        assert!(ids(&[log("melonloader", &["Could not load file or assembly"])]).is_empty());
        // nothing wrong, nothing reported
        assert!(ids(&[log("player", &["Initialize engine version: 2022.3.5f1", "Loaded scene 'Main'"]), log("bepinex", &["[Message:   BepInEx] Chainloader started"])]).is_empty());
        assert_eq!(ids(&[log("proton", &["err:module:import_dll Library MSVCP140.dll (which is needed by L\"C:\\\\game.exe\") not found"])]), vec!["vcrun_missing"]);
        assert_eq!(ids(&[log("player", &["No space left on device"])]), vec!["disk_full"]);
        assert_eq!(ids(&[log("ue4ss", &["[2024-01-01] Fatal error! EXCEPTION_ACCESS_VIOLATION reading address 0x0"])]), vec!["game_crashed"]);
    }

    #[test]
    fn the_newer_patterns_recognise_what_the_loaders_and_wine_really_print() {
        let all = patterns(None);
        let ids = |logs: &[(String, Vec<String>)]| scan(&all, logs).into_iter().map(|f| f.id).collect::<Vec<_>>();
        let one = |label: &str, line: &str| ids(&[log(label, &[line])]);
        assert_eq!(one("bepinex", "[Error  :   BepInEx] Could not load [Cool 1.0] because it has missing dependencies: Lib (v2.0)"), vec!["bepinex_missing_dependency"]);
        assert_eq!(one("bepinex", "[Warning:   BepInEx] Plugin [Old 0.1] targets a wrong version of BepInEx (4.0) and might not work until you update"), vec!["bepinex_wrong_version"]);
        assert_eq!(one("player", "HarmonyLib.HarmonyException: Patching exception in method null"), vec!["harmony_failed"]);
        assert_eq!(one("melonloader", "System.MissingMethodException: Method not found: Void Game.Player.Jump()"), vec!["missing_method"]);
        assert_eq!(one("bepinex", "System.Reflection.ReflectionTypeLoadException: The classes in the module cannot be loaded."), vec!["type_load"]);
        assert_eq!(one("player", "System.DllNotFoundException: native.dll"), vec!["dll_not_found"]);
        assert_eq!(one("bepinex", "Newtonsoft.Json.JsonReaderException: Unexpected character encountered while parsing value: }."), vec!["config_syntax"]);
        assert_eq!(one("player", "Could not allocate memory: System out of memory!"), vec!["out_of_memory"]);
        assert_eq!(one("proton", "err:virtual:virtual_setup_exception stack overflow 1296 bytes in thread 0024"), vec!["stack_overflow"]);
        assert_eq!(one("player", "SteamAPI_Init() failed; no Steam client running"), vec!["steam_not_running"]);
        assert_eq!(one("proton", "wine: Unhandled exception: unimplemented function ntdll.dll.RtlFoo called in 64-bit code"), vec!["proton_unimplemented"]);
        assert_eq!(one("proton", "wine: could not load kernel32.dll, status c0000135"), vec!["proton_kernel32"]);
        assert_eq!(one("ue4ss", "[PS] Failed to find FName::ToString: expect broken things"), vec!["ue4ss_scan_failed"]);
        // ordinary lines do not trip them
        for (label, line) in [("proton", "fixme:heap:RtlSetHeapInformation"), ("ue4ss", "Failed to find the mod folder"), ("player", "Steam initialised"), ("bepinex", "[Info   :   BepInEx] 3 plugins to load")] {
            assert!(one(label, line).is_empty(), "{line}");
        }
    }

    #[test]
    fn a_finding_names_the_most_recent_line_and_where_it_was() {
        let all = patterns(None);
        let f = scan(&all, &[log("player", &["No space left on device /a", "ok", "No space left on device /b", "later line"])]);
        assert_eq!(f.len(), 1);
        assert_eq!((f[0].evidence.as_str(), f[0].source.as_str(), f[0].level.as_str()), ("No space left on device /b", "player", "bad"));
        let long = format!("No space left on device {}", "x".repeat(500));
        assert!(scan(&all, &[log("player", &[&long])])[0].evidence.chars().count() <= 200);
    }

    #[test]
    fn the_users_file_adds_and_replaces_entries_and_bad_ones_are_ignored() {
        let d = TempDir::new("kb_user");
        let f = d.join("errors.json");
        fs::write(&f, r#"[
          {"id":"disk_full","contains":["disk is full"],"logs":["player"],"level":"warn","title":{"en":"Mine","ro":"A mea"},"cause":"c","fix":"f"},
          {"id":"mine","contains":["weird noise"],"title":"Weird","cause":"","fix":""},
          {"id":"","contains":["x"],"title":"no id"},
          {"id":"empty","contains":[],"title":"no needle"},
          {"id":"badlevel","contains":["x"],"level":"fatal","title":"t"},
          {"id":"huge","contains":["x"],"title":"HUGE"},
          "not an object"
        ]"#.replace("\"HUGE\"", &format!("\"{}\"", "y".repeat(5000)))).unwrap();
        let all = patterns(Some(&f));
        let disk = all.iter().find(|p| p.id == "disk_full").unwrap();
        assert_eq!((disk.level.as_str(), disk.contains.as_slice()), ("warn", &["disk is full".to_string()][..]), "replaced");
        assert_eq!(disk.title["ro"], "A mea");
        assert!(all.iter().any(|p| p.id == "mine"));
        assert!(!all.iter().any(|p| ["", "empty", "badlevel", "huge"].contains(&p.id.as_str())));
        // garbage in the file leaves the built-in list alone
        fs::write(&f, "not json").unwrap();
        assert_eq!(patterns(Some(&f)).len(), patterns(None).len());
        assert_eq!(patterns(Some(&d.join("missing.json"))).len(), patterns(None).len());
    }

    #[test]
    fn logs_are_collected_from_the_known_places_and_old_ones_are_ignored() {
        let d = TempDir::new("kb_logs");
        let game = d.join("game");
        fs::create_dir_all(game.join("BepInEx")).unwrap();
        fs::write(game.join("BepInEx/LogOutput.log"), "line1\nBadImageFormatException here\n").unwrap();
        fs::create_dir_all(game.join("MelonLoader")).unwrap();
        fs::write(game.join("MelonLoader/Latest.log"), "").unwrap();
        let proton = d.join("steam-1.log");
        fs::write(&proton, "vkCreateInstance failed\n").unwrap();
        let logs = collect_logs(&game, &[], "ue line\n", Some(&proton));
        let labels: Vec<&str> = logs.iter().map(|l| l.0.as_str()).collect();
        assert_eq!(labels, vec!["bepinex", "ue4ss", "proton"], "an empty log is left out");
        let found: Vec<String> = scan(&patterns(None), &logs).into_iter().map(|f| f.id).collect();
        assert!(found.contains(&"bad_image".to_string()) && found.contains(&"vulkan_failed".to_string()), "{found:?}");
        // a log nobody has touched for a month says nothing about today
        let old = fs::File::options().write(true).open(&proton).unwrap();
        old.set_modified(SystemTime::now() - Duration::from_secs(40 * 24 * 3600)).unwrap();
        assert!(!collect_logs(&game, &[], "", Some(&proton)).iter().any(|l| l.0 == "proton"));
    }
}
