//! Reads the logs that mod loaders and Unity games write and says in one word what state a
//! modded game is in: loaded fine, never started, ran without its loader, setting itself up,
//! frozen, or reporting errors. The UI turns the codes into sentences.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// A game that is running, with logs idle and almost no CPU, for this long is called frozen.
pub const FROZEN_AFTER_SECS: u64 = 240;
const BUSY_CPU_PERCENT: f64 = 3.0;

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Health {
    /// "none", "ok", "setting_up", "incomplete", "not_started", "not_loaded", "frozen", "errors".
    pub code: &'static str,
    /// "ok", "info", "warn" or "bad".
    pub level: &'static str,
    /// Plugins or mods the loader reports.
    pub count: usize,
    /// Minutes without any log activity (frozen).
    pub idle_mins: u64,
    /// The last error line, shortened.
    pub last_error: String,
    /// What the UI offers: "", "stop", "launch_option".
    pub fix: &'static str,
}

impl Health {
    fn new(code: &'static str, level: &'static str, fix: &'static str) -> Self {
        Health { code, level, count: 0, idle_mins: 0, last_error: String::new(), fix }
    }
}

pub struct Input<'a> {
    pub game_dir: &'a Path,
    /// Wine prefixes the game may have written its `Player.log` into.
    pub prefixes: Vec<PathBuf>,
    /// "bepinex", "melonloader" or "".
    pub loader: &'a str,
    pub running: bool,
    /// CPU use of the running game, when measured.
    pub cpu_percent: Option<f64>,
    pub now_ms: u64,
}

fn mtime_ms(p: &Path) -> Option<u64> {
    fs::metadata(p).ok()?.modified().ok()?.duration_since(UNIX_EPOCH).ok().map(|d| d.as_millis() as u64)
}

/// The newest Unity `Player.log` in a Wine prefix (`.../AppData/LocalLow/<company>/<game>/Player.log`).
pub fn newest_player_log(prefix: &Path) -> Option<PathBuf> {
    let users = prefix.join("drive_c/users");
    let mut best: Option<(u64, PathBuf)> = None;
    for u in fs::read_dir(users).ok()?.flatten() {
        let low = u.path().join("AppData/LocalLow");
        for company in fs::read_dir(low).into_iter().flatten().flatten() {
            for game in fs::read_dir(company.path()).into_iter().flatten().flatten() {
                let p = game.path().join("Player.log");
                if let Some(t) = mtime_ms(&p) {
                    if best.as_ref().map(|b| t > b.0).unwrap_or(true) {
                        best = Some((t, p));
                    }
                }
            }
        }
    }
    best.map(|b| b.1)
}

fn tail_lines(p: &Path, n: usize) -> Vec<String> {
    let Ok(bytes) = fs::read(p) else { return Vec::new() };
    let start = bytes.len().saturating_sub(120_000);
    let text = String::from_utf8_lossy(&bytes[start..]).to_string();
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    lines[lines.len().saturating_sub(n)..].to_vec()
}

fn count_loaded(loader: &str, log: &[String]) -> usize {
    match loader {
        "melonloader" => log
            .iter()
            .filter_map(|l| {
                // "[21:12:01.000] 3 Mods loaded." / "1 Mod loaded."
                let at = l.find(" Mods loaded").or_else(|| l.find(" Mod loaded"))?;
                l[..at].split_whitespace().next_back()?.parse::<usize>().ok()
            })
            .next_back()
            .unwrap_or(0),
        _ => log.iter().filter(|l| l.contains("Loading [")).count(),
    }
}

fn last_error(loader: &str, log: &[String]) -> Option<String> {
    let is_err = |l: &str| match loader {
        "melonloader" => l.contains("[Error]") || l.contains("Exception"),
        _ => l.contains("[Error  :") || l.contains("Exception"),
    };
    log.iter().rev().find(|l| is_err(l)).map(|l| l.trim().chars().take(180).collect())
}

/// "[21:11:39.172] [Il2CppAssemblyGenerator] [Info] Finished" → "Finished"
fn strip_tags(line: &str) -> String {
    let mut rest = line.trim();
    while rest.starts_with('[') {
        match rest.find(']') {
            Some(e) => rest = rest[e + 1..].trim_start(),
            None => break,
        }
    }
    rest.to_string()
}

/// What state is the modded game in?
pub fn diagnose(i: &Input) -> Health {
    if i.loader.is_empty() {
        return Health::new("none", "info", "");
    }
    let loader_log = match i.loader {
        "melonloader" => i.game_dir.join("MelonLoader/Latest.log"),
        _ => i.game_dir.join("BepInEx/LogOutput.log"),
    };
    let loader_dir = i.game_dir.join(if i.loader == "melonloader" { "MelonLoader" } else { "BepInEx" });
    let player_ms = i.prefixes.iter().filter_map(|p| newest_player_log(p)).filter_map(|p| mtime_ms(&p)).max();
    let log_ms = mtime_ms(&loader_log);

    let Some(log_ms) = log_ms else {
        // the loader has never written anything: either the game has not been started since, or it ran without it
        let installed_ms = mtime_ms(&loader_dir).unwrap_or(0);
        return match player_ms {
            Some(p) if p > installed_ms => Health::new("not_loaded", "bad", "launch_option"),
            _ => Health::new("not_started", "info", ""),
        };
    };

    let lines = tail_lines(&loader_log, 400);
    let idle_secs = i.now_ms.saturating_sub(log_ms.max(player_ms.unwrap_or(0))) / 1000;
    let busy = i.cpu_percent.map(|c| c >= BUSY_CPU_PERCENT).unwrap_or(true);
    if i.running && idle_secs >= FROZEN_AFTER_SECS && !busy {
        let mut h = Health::new("frozen", "bad", "stop");
        h.idle_mins = idle_secs / 60;
        return h;
    }
    let err = last_error(i.loader, &lines);
    let count = count_loaded(i.loader, &lines);
    // a loader that finished its start says so; without that line the last start did not get through
    let finished = match i.loader {
        "melonloader" => lines.iter().any(|l| l.contains(" Mods loaded") || l.contains(" Mod loaded")),
        _ => lines.iter().any(|l| l.contains("Chainloader startup complete")),
    };
    if !finished {
        if i.running {
            return Health::new("setting_up", "info", "");
        }
        let mut h = Health::new("incomplete", "warn", "");
        h.last_error = lines.iter().rev().find(|l| !l.trim().is_empty()).map(|l| strip_tags(l).chars().take(180).collect()).unwrap_or_default();
        return h;
    }
    if let Some(e) = err {
        let mut h = Health::new("errors", "warn", "");
        h.count = count;
        h.last_error = e;
        return h;
    }
    let mut h = Health::new("ok", "ok", "");
    h.count = count;
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsutil::testutil::TempDir;
    use std::time::{Duration, SystemTime};

    fn write(p: &Path, s: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, s).unwrap();
    }

    fn age(p: &Path, secs: u64) {
        let t = SystemTime::now() - Duration::from_secs(secs);
        let mut o = fs::OpenOptions::new();
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            o.write(true).custom_flags(0x0200_0000); // FILE_FLAG_BACKUP_SEMANTICS: folders too
        }
        #[cfg(not(windows))]
        o.read(true);
        o.open(p).unwrap().set_modified(t).unwrap();
    }

    fn now() -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
    }

    fn input<'a>(game: &'a Path, prefix: &Path, loader: &'a str, running: bool, cpu: Option<f64>) -> Input<'a> {
        Input { game_dir: game, prefixes: vec![prefix.to_path_buf()], loader, running, cpu_percent: cpu, now_ms: now() }
    }

    const HUNG: &str = "[21:11:34.786] [Il2CppAssemblyGenerator] [Info] [Program] Outputting as DLL output format for backwards compatibility.\n[21:11:39.172] [Il2CppAssemblyGenerator] [Info] [Program] Finished outputting in 4386.5358ms\n";

    #[test]
    fn a_game_without_a_loader_has_nothing_to_say() {
        let d = TempDir::new("ld0");
        assert_eq!(diagnose(&input(&d.join("g"), &d.join("p"), "", false, None)).code, "none");
    }

    #[test]
    fn a_loader_that_never_ran_is_told_apart_from_a_game_that_ran_without_it() {
        let d = TempDir::new("ld1");
        let game = d.join("g");
        let prefix = d.join("p");
        fs::create_dir_all(game.join("MelonLoader")).unwrap();
        age(&game.join("MelonLoader"), 0);
        assert_eq!(diagnose(&input(&game, &prefix, "melonloader", false, None)).code, "not_started", "not started since the install");
        // the game was played after the loader went in, but the loader wrote no log
        let pl = prefix.join("drive_c/users/steamuser/AppData/LocalLow/Studio/Game/Player.log");
        write(&pl, "Unity player started");
        let h = diagnose(&Input { now_ms: now() + 1000, ..input(&game, &prefix, "melonloader", false, None) });
        let installed = mtime_ms(&game.join("MelonLoader")).unwrap();
        let played = mtime_ms(&pl).unwrap();
        assert_eq!(h.code, if played > installed { "not_loaded" } else { "not_started" });
        age(&game.join("MelonLoader"), 3600);
        let h = diagnose(&input(&game, &prefix, "melonloader", false, None));
        assert_eq!((h.code, h.fix), ("not_loaded", "launch_option"), "the loader folder is older than the last game run");
    }

    #[test]
    fn a_frozen_game_is_called_frozen_only_when_everything_is_idle() {
        let d = TempDir::new("ld2");
        let game = d.join("g");
        let prefix = d.join("p");
        let log = game.join("MelonLoader/Latest.log");
        write(&log, HUNG);
        age(&log, 600);
        let pl = prefix.join("drive_c/users/steamuser/AppData/LocalLow/Studio/Game/Player.log");
        write(&pl, "<RI> Input initialized.");
        age(&pl, 600);
        let h = diagnose(&input(&game, &prefix, "melonloader", true, Some(0.4)));
        assert_eq!((h.code, h.level, h.fix), ("frozen", "bad", "stop"));
        assert!(h.idle_mins >= 9);
        // a game sitting in a menu still uses the CPU: not frozen
        assert_ne!(diagnose(&input(&game, &prefix, "melonloader", true, Some(25.0))).code, "frozen");
        // the same files with the game closed: nothing to stop
        assert_ne!(diagnose(&input(&game, &prefix, "melonloader", false, None)).code, "frozen");
        // fresh log activity: still working
        age(&log, 5);
        assert_eq!(diagnose(&input(&game, &prefix, "melonloader", true, Some(0.4))).code, "setting_up");
    }

    #[test]
    fn a_start_that_never_finished_is_reported_with_its_last_step() {
        let d = TempDir::new("ld4");
        let game = d.join("g");
        let prefix = d.join("p");
        let log = game.join("MelonLoader/Latest.log");
        write(&log, HUNG);
        age(&log, 3000);
        let h = diagnose(&input(&game, &prefix, "melonloader", false, None));
        assert_eq!(h.code, "incomplete");
        assert_eq!(h.last_error, "Finished outputting in 4386.5358ms");
    }

    #[test]
    fn loaded_mods_and_errors_are_reported() {
        let d = TempDir::new("ld3");
        let game = d.join("g");
        let prefix = d.join("p");
        let log = game.join("MelonLoader/Latest.log");
        write(&log, "[21:11:27.489] Loading UserLibs...\n[21:11:27.491] 0 UserLibs loaded.\n[21:12:01.000] 3 Mods loaded.\n");
        age(&log, 3000);
        let h = diagnose(&input(&game, &prefix, "melonloader", false, None));
        assert_eq!((h.code, h.count), ("ok", 3));
        write(&log, "[21:12:01.000] 3 Mods loaded.\n[21:12:09.000] [Error] [CoolMod] NullReferenceException: boom\n");
        age(&log, 3000);
        let h = diagnose(&input(&game, &prefix, "melonloader", false, None));
        assert_eq!((h.code, h.level), ("errors", "warn"));
        assert!(h.last_error.contains("NullReferenceException"));

        // BepInEx: plugins are counted from its "Loading [..]" lines
        let bl = game.join("BepInEx/LogOutput.log");
        write(&bl, "[Info   :   BepInEx] Loading [A 1.0.0]\n[Info   :   BepInEx] Loading [B 2.0.0]\n[Message:   BepInEx] Chainloader startup complete\n");
        age(&bl, 3000);
        let h = diagnose(&input(&game, &prefix, "bepinex", false, None));
        assert_eq!((h.code, h.count), ("ok", 2));
    }
}
