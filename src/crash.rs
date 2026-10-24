//! Crash Detective: decide whether the last run of a game crashed and which mods the evidence
//! points at. The scoring is a pure function over the collected logs (`analyze`), so it is
//! tested on synthetic logs; `gather` does the file reading.

use crate::engine::{GameEntry, RigEngine};
use crate::fsutil;
use crate::repair::{ModEntry, ModManager};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

const LOG_TAIL: u64 = 512 * 1024;
const RECENT_LINES: usize = 40;
const SCORE_CAP: u32 = 20;
const DAY_MS: u64 = 86_400_000;

#[derive(Clone, Debug)]
pub struct ModInfo {
    pub id: String,
    pub name: String,
    pub mod_type: String,
    pub enabled: bool,
    pub is_system: bool,
    /// Last time the mod's files changed (ms since epoch), 0 when unknown.
    pub changed_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Suspect {
    pub id: String,
    pub name: String,
    pub mod_type: String,
    pub score: u32,
    pub reasons: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CrashReport {
    /// "self_heal" when pressing Self-Heal should fix the cause (e.g. wrong engine version).
    #[serde(default)]
    pub fix: Option<String>,
    pub appid: String,
    pub game: String,
    pub at_ms: u64,
    /// True when the run left crash artifacts or fatal markers behind.
    pub crashed: bool,
    pub headline: String,
    pub suspects: Vec<Suspect>,
    pub evidence: Vec<String>,
    pub artifacts: usize,
    /// Mods updated shortly before this run, with the version they could go back to.
    #[serde(default)]
    pub recent_updates: Vec<crate::deploy::RecentUpdate>,
    /// Up to three things to try, most promising first (`{ "kind": ..., ... }`, filled in by the app).
    #[serde(default)]
    pub tries: Vec<serde_json::Value>,
    /// How many runs in a row crashed, this one included.
    #[serde(default)]
    pub streak: u32,
}

pub struct AnalysisInput<'a> {
    pub mods: &'a [ModInfo],
    pub ue4ss_log: &'a str,
    pub ue_log: &'a str,
    /// Crash dumps / crash-context files produced since the run started.
    pub artifacts: usize,
    pub now_ms: u64,
    /// `(what UE4SS is configured for, what the game itself reports)` when they disagree.
    pub engine_mismatch: Option<(String, String)>,
}

/// 0 = not a problem line, 1 = weak (failed, warning), 2 = error, 3 = strong (fatal, exception…).
fn severity(line_lower: &str) -> u32 {
    const STRONG: [&str; 9] = [
        "fatal", "exception", "access violation", "assertion failed", "unhandled", "crash",
        "stack overflow", "segmentation", "attempt to ",
    ];
    if STRONG.iter().any(|k| line_lower.contains(k)) {
        3
    } else if line_lower.contains("error") {
        2
    } else if line_lower.contains("failed") || line_lower.contains("warning") {
        1
    } else {
        0
    }
}

/// Markers that mean the game process itself went down (UE's own log).
fn is_fatal_marker(line_lower: &str) -> bool {
    ["fatal error", "appcrash", "unhandled exception", "assertion failed", "access violation", "appError called", "crashing"]
        .iter()
        .any(|k| line_lower.contains(&k.to_lowercase()))
}

fn norm_path(line_lower: &str) -> String {
    line_lower.replace('\\', "/")
}

/// How strongly a log line points at a mod: 2 = its folder path, 1 = its name, 0 = no.
fn mention(line_norm: &str, m: &ModInfo) -> u32 {
    let name = m.name.trim_start_matches("[Logic] ").to_lowercase();
    let stem = name.trim_end_matches("_p").to_string();
    if m.mod_type == "LUA" {
        if line_norm.contains(&format!("mods/{}/", name)) || line_norm.contains(&format!("mods/{}\"", name)) {
            return 2;
        }
        if name.len() >= 4 && line_norm.contains(&name) {
            return 1;
        }
        0
    } else {
        let key = if stem.len() >= 4 { stem } else { name };
        if key.len() >= 4 && line_norm.contains(&key) {
            1
        } else {
            0
        }
    }
}

fn clip(s: &str) -> String {
    let t = s.trim();
    if t.chars().count() > 140 {
        format!("{}…", t.chars().take(140).collect::<String>())
    } else {
        t.to_string()
    }
}

pub fn analyze(appid: &str, game: &str, inp: &AnalysisInput) -> CrashReport {
    let fatal_lines: Vec<String> = inp
        .ue_log
        .lines()
        .filter(|l| is_fatal_marker(&l.to_lowercase()))
        .map(clip)
        .collect();
    let crashed = inp.artifacts > 0 || !fatal_lines.is_empty();

    let candidates: Vec<&ModInfo> = inp.mods.iter().filter(|m| m.enabled && !m.is_system).collect();
    let mut score = vec![0u32; candidates.len()];
    let mut reasons: Vec<Vec<String>> = vec![Vec::new(); candidates.len()];
    let mut strong_lines: Vec<String> = Vec::new();

    for (log, tag) in [(inp.ue4ss_log, "UE4SS.log"), (inp.ue_log, "game log")] {
        let lines: Vec<&str> = log.lines().collect();
        let total = lines.len();
        for (i, line) in lines.iter().enumerate() {
            let low = line.to_lowercase();
            let sev = severity(&low);
            if sev == 0 {
                continue;
            }
            if sev >= 2 && strong_lines.len() < 40 {
                strong_lines.push(clip(line));
            }
            let norm = norm_path(&low);
            let recent = i + RECENT_LINES >= total;
            for (k, m) in candidates.iter().enumerate() {
                let w = mention(&norm, m);
                if w == 0 {
                    continue;
                }
                let pts = sev * w * if recent { 2 } else { 1 };
                score[k] = (score[k] + pts).min(SCORE_CAP);
                if reasons[k].len() < 3 && sev >= 2 {
                    reasons[k].push(format!("{}: {}", tag, clip(line)));
                }
            }
        }
    }

    // Recently added or changed mods are classic suspects.
    let newest = candidates.iter().enumerate().filter(|(_, m)| m.changed_ms > 0).max_by_key(|(_, m)| m.changed_ms).map(|(k, _)| k);
    for (k, m) in candidates.iter().enumerate() {
        if m.changed_ms > 0 && inp.now_ms.saturating_sub(m.changed_ms) < 2 * DAY_MS {
            score[k] += 1;
            reasons[k].push("changed within the last 2 days".into());
        }
        if Some(k) == newest && candidates.len() > 1 && m.changed_ms > 0 {
            score[k] += 2;
            reasons[k].push("the most recently added or changed mod".into());
        }
    }

    let mut suspects: Vec<Suspect> = candidates
        .iter()
        .enumerate()
        .filter(|(k, _)| score[*k] > 0)
        .map(|(k, m)| Suspect {
            id: m.id.clone(),
            name: m.name.clone(),
            mod_type: m.mod_type.clone(),
            score: score[k],
            reasons: std::mem::take(&mut reasons[k]),
        })
        .collect();
    suspects.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.name.cmp(&b.name)));
    suspects.truncate(8);

    let mut evidence: Vec<String> = fatal_lines.into_iter().rev().take(4).collect();
    evidence.extend(strong_lines.into_iter().rev().take(6));
    evidence.dedup();
    evidence.truncate(8);

    let mut fix = None;
    if let (true, Some((configured, actual))) = (crashed, &inp.engine_mismatch) {
        evidence.insert(0, format!("UE4SS is configured for Unreal Engine {} but the game reports {}.", configured, actual));
        evidence.truncate(8);
        fix = Some("self_heal".to_string());
    }
    let headline = match (crashed, suspects.first()) {
        (true, _) if fix.is_some() => {
            let (c, a) = inp.engine_mismatch.clone().unwrap_or_default();
            format!("Crash detected, and UE4SS is set up for Unreal Engine {} while the game is {}. Press Self-Heal to correct it.", c, a)
        }
        (true, Some(s)) if s.score >= 6 => format!("Crash detected. Most likely culprit: {}.", s.name),
        (true, Some(s)) => format!("Crash detected. Weak lead: {}. \"Find the culprit\" will settle it.", s.name),
        (true, None) => "Crash detected, but no mod stands out in the logs. \"Find the culprit\" can still isolate it.".into(),
        (false, Some(s)) if s.score >= 6 => format!("No crash recorded, but {} logged errors.", s.name),
        (false, _) => "No crash evidence in the last run.".into(),
    };

    CrashReport {
        fix,
        appid: appid.into(),
        game: game.into(),
        at_ms: inp.now_ms,
        crashed,
        headline,
        suspects,
        evidence,
        artifacts: inp.artifacts,
        recent_updates: Vec::new(),
        tries: Vec::new(),
        streak: 0,
    }
}

// ───────────────────────────── gathering ─────────────────────────────

fn mtime_ms(p: &Path) -> u64 {
    fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Last change of a mod's files (directory contents for Lua mods).
pub fn mod_changed_ms(m: &ModEntry) -> u64 {
    let mut t = mtime_ms(&m.path);
    if m.path.is_dir() {
        for e in WalkDir::new(&m.path).max_depth(3).into_iter().flatten() {
            if e.file_type().is_file() {
                t = t.max(mtime_ms(e.path()));
            }
        }
    }
    t
}

pub fn mod_infos(mods: &[ModEntry]) -> Vec<ModInfo> {
    mods.iter()
        .map(|m| ModInfo {
            id: m.id.clone(),
            name: m.name.clone(),
            mod_type: m.mod_type.clone(),
            enabled: m.enabled,
            is_system: m.is_system,
            changed_ms: mod_changed_ms(m),
        })
        .collect()
}

fn appdata_local(game: &GameEntry) -> PathBuf {
    crate::profsaves::bases(game).local
}

/// Directories UE writes `Saved/<sub>` into for this game.
fn saved_dirs(game: &GameEntry, sub: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(ub) = RigEngine::find_ue_bin_dir(Path::new(&game.game_dir)) {
        if let Some(proj) = ub.parent().and_then(Path::parent) {
            out.push(proj.join("Saved").join(sub));
        }
    }
    if let Ok(rd) = fs::read_dir(appdata_local(game)) {
        for e in rd.flatten() {
            out.push(e.path().join("Saved").join(sub));
        }
    }
    out.into_iter().filter(|p| p.is_dir()).collect()
}

/// Crash dumps and crash-context files written after `since_ms`.
pub fn artifacts_since(game: &GameEntry, since_ms: u64) -> usize {
    let mut roots = saved_dirs(game, "Crashes");
    if let Some(ub) = RigEngine::find_ue_bin_dir(Path::new(&game.game_dir)) {
        roots.push(ub);
    }
    roots
        .iter()
        .flat_map(|r| WalkDir::new(r).max_depth(3).into_iter().flatten())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            let n = e.file_name().to_string_lossy().to_lowercase();
            n.ends_with(".dmp") || n.ends_with(".runtime-xml") || n == "crashcontext.runtime-xml"
        })
        .filter(|e| mtime_ms(e.path()) >= since_ms)
        .count()
}

/// Tail of the newest `*.log` among the game's `Saved/Logs` folders, if written since `since_ms`.
fn newest_ue_log(game: &GameEntry, since_ms: u64) -> String {
    saved_dirs(game, "Logs")
        .iter()
        .flat_map(|d| fs::read_dir(d).into_iter().flatten().flatten())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "log").unwrap_or(false))
        .filter(|p| mtime_ms(p) >= since_ms)
        .max_by_key(|p| mtime_ms(p))
        .and_then(|p| fsutil::read_tail(&p, LOG_TAIL).ok())
        .unwrap_or_default()
}

/// The engine version the game itself printed in its newest log ("LogInit: Engine Version:
/// 5.6.1-…"), unless the log is older than the executable (a game update since then).
pub fn logged_engine_version(game: &GameEntry) -> Option<(String, String)> {
    let exe_ms = [&game.shipping_exe, &game.exe_path]
        .iter()
        .filter(|p| !p.is_empty())
        .map(|p| mtime_ms(Path::new(p.as_str())))
        .max()
        .unwrap_or(0);
    let log = saved_dirs(game, "Logs")
        .iter()
        .flat_map(|d| fs::read_dir(d).into_iter().flatten().flatten())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "log").unwrap_or(false))
        .filter(|p| mtime_ms(p) >= exe_ms)
        .max_by_key(|p| mtime_ms(p))?;
    let mut head = vec![0u8; 64 * 1024];
    let n = fs::File::open(&log).and_then(|mut f| std::io::Read::read(&mut f, &mut head)).ok()?;
    parse_engine_version_line(&String::from_utf8_lossy(&head[..n]))
}

/// `LogInit: Engine Version: 5.6.1-131439+++Project+SN2-Release` -> ("5", "6")
pub fn parse_engine_version_line(log: &str) -> Option<(String, String)> {
    let line = log.lines().find(|l| l.contains("Engine Version:") && !l.contains("Compatible"))?;
    let v = line.split("Engine Version:").nth(1)?.trim();
    let mut it = v.split(|c: char| !c.is_ascii_digit());
    let (maj, min) = (it.next()?, it.next()?);
    (matches!(maj, "4" | "5") && !min.is_empty() && min.len() <= 2).then(|| (maj.to_string(), min.to_string()))
}

/// Where UE4SS and the engine keep their logs for this game: `(UE4SS.log, newest log in Saved/Logs)`.
pub fn unreal_log_files(game: &GameEntry) -> (Option<PathBuf>, Option<PathBuf>) {
    let ue4ss = RigEngine::find_ue_bin_dir(Path::new(&game.game_dir)).and_then(|ub| {
        let root = ModManager::get_ue4ss_root(&ub);
        [root.join("UE4SS.log"), ub.join("UE4SS.log")].into_iter().find(|p| p.is_file())
    });
    let engine = saved_dirs(game, "Logs")
        .iter()
        .flat_map(|d| fs::read_dir(d).into_iter().flatten().flatten())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "log").unwrap_or(false))
        .max_by_key(|p| mtime_ms(p));
    (ue4ss, engine)
}

fn ue4ss_log_text(game: &GameEntry) -> String {
    let Some(ub) = RigEngine::find_ue_bin_dir(Path::new(&game.game_dir)) else {
        return String::new();
    };
    let root = ModManager::get_ue4ss_root(&ub);
    [root.join("UE4SS.log"), ub.join("UE4SS.log")]
        .iter()
        .find(|p| p.is_file())
        .and_then(|p| fsutil::read_tail(p, LOG_TAIL).ok())
        .unwrap_or_default()
}

/// Collect evidence for a run that started at `since_ms` and analyse it.
pub fn analyze_run(game: &GameEntry, mods: &[ModEntry], since_ms: u64, now_ms: u64) -> CrashReport {
    let infos = mod_infos(mods);
    let ue4ss = ue4ss_log_text(game);
    let engine_mismatch = RigEngine::find_ue_bin_dir(Path::new(&game.game_dir)).and_then(|ub| {
        let ini = fs::read_to_string(ModManager::get_ue4ss_root(&ub).join("UE4SS-settings.ini")).ok()?.replace("\r\n", "\n");
        let (cm, cn) = (crate::repair::ini_get(&ini, "MajorVersion")?, crate::repair::ini_get(&ini, "MinorVersion")?);
        if cm.is_empty() || cn.is_empty() {
            return None; // automatic detection: nothing of ours to be wrong
        }
        let (am, an) = logged_engine_version(game).or_else(|| RigEngine::detect_ue_version_opt(&ub))?;
        ((cm.as_str(), cn.as_str()) != (am.as_str(), an.as_str())).then(|| (format!("{}.{}", cm, cn), format!("{}.{}", am, an)))
    });
    let ue = newest_ue_log(game, since_ms);
    analyze(
        &game.appid,
        &game.name,
        &AnalysisInput { mods: &infos, ue4ss_log: &ue4ss, ue_log: &ue, artifacts: artifacts_since(game, since_ms), now_ms, engine_mismatch },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(id: &str, name: &str, ty: &str, enabled: bool, changed_days_ago: f64) -> ModInfo {
        ModInfo {
            id: id.into(),
            name: name.into(),
            mod_type: ty.into(),
            enabled,
            is_system: false,
            changed_ms: if changed_days_ago < 0.0 { 0 } else { 100 * DAY_MS - (changed_days_ago * DAY_MS as f64) as u64 },
        }
    }

    fn input<'a>(mods: &'a [ModInfo], u: &'a str, g: &'a str, artifacts: usize) -> AnalysisInput<'a> {
        AnalysisInput { mods, ue4ss_log: u, ue_log: g, artifacts, now_ms: 100 * DAY_MS, engine_mismatch: None }
    }

    const UE4SS: &str = "[12:00:01] Starting Lua mod 'GoodMod'\n\
[12:00:02] Starting Lua mod 'BadMod'\n\
[12:00:05] [Lua] Error: ...\\Mods\\BadMod\\Scripts\\main.lua:42: attempt to index a nil value (field 'x')\n\
[12:00:06] [Lua] BadMod: stack traceback: Mods/BadMod/Scripts/main.lua:42\n\
[12:00:07] [Lua] GoodMod: all fine, warning: minor thing\n";

    #[test]
    fn lua_errors_point_at_the_mod_whose_path_appears() {
        let mods = vec![m("LUA:GoodMod", "GoodMod", "LUA", true, 30.0), m("LUA:BadMod", "BadMod", "LUA", true, 30.0)];
        let r = analyze("1", "G", &input(&mods, UE4SS, "", 1));
        assert!(r.crashed && r.artifacts == 1);
        assert_eq!(r.suspects[0].id, "LUA:BadMod");
        assert!(r.suspects[0].score >= 6, "{:?}", r.suspects);
        assert!(r.suspects[0].reasons.iter().any(|x| x.contains("attempt to index")));
        assert!(r.headline.contains("BadMod") && r.headline.starts_with("Crash detected"));
        let good = r.suspects.iter().find(|s| s.id == "LUA:GoodMod").map(|s| s.score).unwrap_or(0);
        assert!(good < r.suspects[0].score);
    }

    #[test]
    fn fatal_markers_in_the_game_log_count_as_a_crash_even_without_a_dump() {
        let ue = "LogInit: Display: ok\nLogWindows: Error: appError called: Fatal error: [File:Foo.cpp] [Line: 9]\nLogPakFile: Error: Failed to read Skin_P.pak\n";
        let mods = vec![m("PAK:Skin_P", "Skin_P", "PAK", true, 20.0), m("PAK:Other", "Other", "PAK", true, 20.0)];
        let r = analyze("1", "G", &input(&mods, "", ue, 0));
        assert!(r.crashed);
        assert_eq!(r.suspects[0].id, "PAK:Skin_P");
        assert!(r.evidence.iter().any(|e| e.to_lowercase().contains("fatal error")));
    }

    #[test]
    fn a_wrong_engine_version_is_named_as_the_cause_of_a_startup_crash() {
        let mods: Vec<ModInfo> = vec![];
        let ue = "LogWindows: Error: appError called: Fatal error: [File:MallocBinned2.cpp] [Line: 643]\n";
        let mut inp = input(&mods, "[1] UE4SS loaded\n", ue, 0);
        inp.engine_mismatch = Some(("5.4".into(), "5.6".into()));
        let r = analyze("1", "G", &inp);
        assert!(r.crashed);
        assert_eq!(r.fix.as_deref(), Some("self_heal"));
        assert!(r.headline.contains("5.4") && r.headline.contains("5.6") && r.headline.contains("Self-Heal"), "{}", r.headline);
        assert!(r.evidence[0].contains("configured for Unreal Engine 5.4"), "{:?}", r.evidence);
        // no crash, no accusation; no mismatch, no fix hint
        let mut calm = input(&mods, "", "", 0);
        calm.engine_mismatch = Some(("5.4".into(), "5.6".into()));
        assert_eq!(analyze("1", "G", &calm).fix, None);
        assert_eq!(analyze("1", "G", &input(&mods, "", ue, 0)).fix, None);
    }

    #[test]
    fn engine_version_is_read_from_the_games_own_log_line() {
        let log = "LogInit: Build: X-CL-1\nLogInit: Engine Version: 5.6.1-131439+++Project+SN2-Release\nLogInit: Compatible Engine Version: 5.5.0-1\n";
        assert_eq!(parse_engine_version_line(log), Some(("5".into(), "6".into())));
        assert_eq!(parse_engine_version_line("LogInit: Engine Version: 4.27.2-1+++UE4+Release-4.27\n"), Some(("4".into(), "27".into())));
        assert_eq!(parse_engine_version_line("LogInit: Compatible Engine Version: 5.5.0-1\n"), None);
        assert_eq!(parse_engine_version_line("LogInit: Engine Version: garbage\n"), None);
        assert_eq!(parse_engine_version_line(""), None);
    }

    #[test]
    fn clean_runs_do_not_raise_alarms() {
        let mods = vec![m("LUA:A", "Alpha", "LUA", true, 30.0)];
        let r = analyze("1", "G", &input(&mods, "[1] Starting Lua mod 'Alpha'\n[2] all good\n", "LogInit: ok\n", 0));
        assert!(!r.crashed);
        assert!(r.suspects.is_empty());
        assert_eq!(r.headline, "No crash evidence in the last run.");
    }

    #[test]
    fn recently_changed_mods_are_ranked_up_but_disabled_and_system_mods_never_blamed() {
        let mut mods = vec![m("LUA:Old", "OldMod", "LUA", true, 40.0), m("LUA:New", "NewMod", "LUA", true, 0.5), m("LUA:Off", "OffMod", "LUA", false, 0.1)];
        let mut sys = m("LUA:Keybinds", "Keybinds", "LUA", true, 0.1);
        sys.is_system = true;
        mods.push(sys);
        let log = "[1] Error in Mods/OffMod/Scripts/main.lua: boom\n[2] Error in Mods/Keybinds/Scripts/main.lua: boom\n";
        let r = analyze("1", "G", &input(&mods, log, "", 1));
        assert_eq!(r.suspects.len(), 1, "{:?}", r.suspects);
        assert_eq!(r.suspects[0].id, "LUA:New");
        assert!(r.suspects[0].reasons.iter().any(|x| x.contains("most recently")));
    }

    #[test]
    fn short_names_need_a_path_match_and_logic_prefix_is_ignored() {
        let mods = vec![m("LUA:AB", "AB", "LUA", true, -1.0), m("PAK:[Logic] HSTM_UI", "[Logic] HSTM_UI", "PAK", true, -1.0)];
        let log = "[1] error: cab fell off\n[2] fatal: Failed to load /Game/Mods/HSTM_UI/Widget\n";
        let r = analyze("1", "G", &input(&mods, log, "", 0));
        assert_eq!(r.suspects.len(), 1);
        assert_eq!(r.suspects[0].name, "[Logic] HSTM_UI");
    }

    #[test]
    fn scores_are_capped_and_output_is_bounded() {
        let mods: Vec<ModInfo> = (0..20).map(|i| m(&format!("LUA:M{i}"), &format!("Module{i:02}"), "LUA", true, -1.0)).collect();
        let log: String = (0..5000).map(|i| format!("[{i}] error in Mods/Module{:02}/Scripts/x.lua: attempt to call\n", i % 20)).collect();
        let r = analyze("1", "G", &input(&mods, &log, "", 0));
        assert_eq!(r.suspects.len(), 8);
        assert!(r.suspects.iter().all(|s| s.score <= SCORE_CAP && s.reasons.len() <= 3));
        assert!(r.evidence.len() <= 8);
    }

    #[test]
    fn gathers_artifacts_and_logs_from_disk() {
        use crate::repair::fixtures::make_game;
        let (d, game) = make_game("crashgather");
        let bin = PathBuf::from(&game.bin_dir);
        assert_eq!(artifacts_since(&game, 0), 0);
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(bin.join("crash.dmp"), b"x").unwrap();
        let crashes = PathBuf::from(&game.game_dir).join("Demo/Saved/Crashes/UE4CC-1");
        fs::create_dir_all(&crashes).unwrap();
        fs::write(crashes.join("CrashContext.runtime-xml"), b"<x/>").unwrap();
        assert_eq!(artifacts_since(&game, 0), 2);
        assert_eq!(artifacts_since(&game, mtime_ms(&bin.join("crash.dmp")) + 5000), 0, "older artifacts are ignored");

        let logs = PathBuf::from(&game.pfx).join("drive_c/users/steamuser/AppData/Local/Demo/Saved/Logs");
        fs::create_dir_all(&logs).unwrap();
        fs::write(logs.join("Demo.log"), "LogWindows: Error: Fatal error: [File:a.cpp]\n").unwrap();
        fs::write(bin.join("ue4ss/UE4SS.log"), "[1] Error: Mods/CoolMod/Scripts/main.lua:3: attempt to call a nil value\n").unwrap();
        let mods = ModManager::list_mods(&game);
        let r = analyze_run(&game, &mods, 0, mtime_ms(&logs) + 1000);
        assert!(r.crashed);
        assert_eq!(r.suspects.first().map(|s| s.id.as_str()), Some("LUA:CoolMod"), "{:?}", r.suspects);
        assert!(newest_ue_log(&game, mtime_ms(&logs) + 60_000).is_empty(), "stale logs are not reused");
        drop(d);
    }
}
