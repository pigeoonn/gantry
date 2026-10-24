//! Starting a trainer next to a running game. A trainer only works when it lives in the same Wine world as the game:
//! same prefix, same Proton build and same settings. So it is started the way Steam starts the game (`proton run`, with
//! the game's compatibility data), and only when that fails right away, through the build's own Wine as a second try.
//! Whatever the trainer prints goes to a log file, so a trainer that does not show up can be explained.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::engine::RigEngine;

/// Everything needed to start one trainer for one game.
pub struct Launch<'a> {
    pub trainer: &'a Path,
    /// The game's Wine prefix (empty on Windows).
    pub pfx: &'a str,
    /// The game's compatibility data folder (the parent of `pfx` for Steam games).
    pub compat: &'a Path,
    /// The `proton` script of the build the game runs with.
    pub proton: Option<&'a Path>,
    pub steam_root: &'a Path,
    pub app_id: &'a str,
    /// `NAME=value` lines the game itself is started with.
    pub env_lines: &'a str,
    pub log_file: &'a Path,
}

fn base(mut c: Command, l: &Launch) -> Command {
    if let Some(parent) = l.trainer.parent() {
        c.current_dir(parent);
    }
    if !l.pfx.is_empty() {
        c.env("WINEPREFIX", l.pfx);
    }
    for line in l.env_lines.lines() {
        if let Some((k, v)) = line.split_once('=') {
            let k = k.trim();
            if !k.is_empty() && !k.contains(char::is_whitespace) {
                c.env(k, v.trim().trim_matches('"'));
            }
        }
    }
    c.stdin(Stdio::null());
    c
}

/// The ways to start the trainer, best first. Each has a short name for the log.
pub fn commands(l: &Launch) -> Vec<(&'static str, Command)> {
    if crate::platform::IS_WINDOWS {
        return vec![("directly", base(Command::new(l.trainer), l))];
    }
    let mut out = Vec::new();
    if let Some(script) = l.proton.filter(|p| p.is_file()) {
        let mut c = Command::new(script);
        c.arg("run").arg(l.trainer);
        let mut c = base(c, l);
        c.env("STEAM_COMPAT_DATA_PATH", l.compat).env("STEAM_COMPAT_CLIENT_INSTALL_PATH", l.steam_root).env("STEAM_COMPAT_APP_ID", l.app_id);
        out.push(("proton run", c));
    }
    let wine = RigEngine::resolve_proton_wine_bin(l.proton);
    let mut c = Command::new(wine);
    c.arg(l.trainer);
    out.push(("wine", base(c, l)));
    out
}

/// The last lines of a log, trimmed, for a message.
pub fn tail(path: &Path, lines: usize) -> String {
    let text = fs::read(path).map(|b| String::from_utf8_lossy(&b).to_string()).unwrap_or_default();
    let keep: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("-- ")).collect();
    let from = keep.len().saturating_sub(lines);
    keep[from..].iter().map(|l| l.chars().take(200).collect::<String>()).collect::<Vec<_>>().join(" | ")
}

fn start_one(mut cmd: Command, log: &Path, first: bool, name: &str, settle: Duration) -> Result<Child, String> {
    let open = |append: bool| OpenOptions::new().create(true).write(true).append(append).truncate(!append).open(log);
    let mut file: File = open(!first).map_err(|e| e.to_string())?;
    let _ = writeln!(file, "-- {} --", name);
    cmd.stdout(Stdio::from(file.try_clone().map_err(|e| e.to_string())?)).stderr(Stdio::from(file));
    let mut child = cmd.spawn().map_err(|e| format!("{}: {}", name, e))?;
    let until = Instant::now() + settle;
    while Instant::now() < until {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(child),
            Ok(Some(status)) => return Err(format!("{} stopped with {}", name, status)),
            Ok(None) => thread::sleep(Duration::from_millis(40)),
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(child)
}

/// A started trainer: the way that worked, the pid Gantry started, and when that process ended (with its exit status).
#[derive(Debug, Clone)]
pub struct Started {
    pub how: &'static str,
    pub pid: u32,
    pub ended: Arc<Mutex<Option<(Instant, String)>>>,
}

/// Starts the trainer. `Ok` names the way that worked; `Err` says what every way printed or why it stopped.
pub fn start(l: &Launch, settle: Duration) -> Result<Started, String> {
    if let Some(parent) = l.log_file.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let mut why: Vec<String> = Vec::new();
    for (i, (name, cmd)) in commands(l).into_iter().enumerate() {
        match start_one(cmd, l.log_file, i == 0, name, settle) {
            Ok(mut child) => {
                let ended = Arc::new(Mutex::new(None));
                let (pid, mark) = (child.id(), Arc::clone(&ended));
                thread::spawn(move || {
                    let status = child.wait().map(|s| s.to_string()).unwrap_or_default();
                    *mark.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), status));
                });
                return Ok(Started { how: name, pid, ended });
            }
            Err(e) => why.push(e),
        }
    }
    let said = tail(l.log_file, 4);
    Err(if said.is_empty() { why.join("; ") } else { format!("{} ({})", why.join("; "), said) })
}

/// What a trainer's output says about why it did not start, when it is one of the usual causes.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Cause {
    /// The Visual C++ runtime is not in the prefix.
    CppRuntime,
    /// .NET (or Mono) is not in the prefix.
    DotNet,
    /// The game's Wine server and the trainer's are different builds.
    OtherBuild,
}

pub fn cause_of(output: &str) -> Option<Cause> {
    let low = output.to_lowercase();
    if low.contains("version mismatch") || low.contains("wineserver: ") && low.contains("protocol version") {
        Some(Cause::OtherBuild)
    } else if low.contains("mscoree") || low.contains("wine-mono") || low.contains("system.windows.forms") || low.contains("could not load file or assembly") {
        Some(Cause::DotNet)
    } else if ["vcruntime", "msvcp", "msvcr", "concrt140", "ucrtbase"].iter().any(|d| low.contains(d)) && (low.contains("not found") || low.contains("failed") || low.contains("could not")) {
        Some(Cause::CppRuntime)
    } else {
        None
    }
}

/// The game versions a trainer says it is made for, from its file name ("Cyberpunk 2077 v2.0-v2.5 Plus 46 Trainer.exe"
/// gives 2.0 to 2.5; "v1.100.0 Plus 18" gives exactly 1.100.0). `None` when the name does not say.
pub fn made_for(file_name: &str) -> Option<(Vec<u32>, Vec<u32>)> {
    static RANGE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)(?:^|[\s_(\[-])v(\d+(?:\.\d+){1,3})(?:\s*[-~]\s*v?(\d+(?:\.\d+){1,3}))?(?:$|[\s_)\].+-])").unwrap());
    let c = RANGE.captures(file_name)?;
    let parse = |t: &str| t.split('.').map(|n| n.parse::<u32>().ok()).collect::<Option<Vec<u32>>>();
    let from = parse(c.get(1)?.as_str())?;
    let to = c.get(2).and_then(|m| parse(m.as_str())).unwrap_or_else(|| from.clone());
    Some((from, to))
}

fn cmp_version(a: &[u32], b: &[u32]) -> std::cmp::Ordering {
    let n = a.len().max(b.len());
    (0..n).map(|i| a.get(i).unwrap_or(&0).cmp(b.get(i).unwrap_or(&0))).find(|o| o.is_ne()).unwrap_or(std::cmp::Ordering::Equal)
}

/// How a game version sits against what the trainer is made for: `Less` (older), `Greater` (newer) or `Equal` (fits).
/// `None` when the two cannot be compared: different first numbers usually mean the game's file carries some other
/// number (an engine version, a build), and a wrong warning is worse than none.
pub fn fits(game: &[u32], made_for: &(Vec<u32>, Vec<u32>)) -> Option<std::cmp::Ordering> {
    let (from, to) = made_for;
    if game.is_empty() || game.first() != from.first() || game.iter().all(|&n| n == 0) {
        return None;
    }
    // compare only as many parts as the trainer names: v2.5 covers 2.5.0.1 too
    let cut = |v: &[u32], n: usize| v.iter().copied().take(n).collect::<Vec<u32>>();
    if cmp_version(&cut(game, from.len()), from).is_lt() {
        Some(std::cmp::Ordering::Less)
    } else if cmp_version(&cut(game, to.len()), to).is_gt() {
        Some(std::cmp::Ordering::Greater)
    } else {
        Some(std::cmp::Ordering::Equal)
    }
}

/// The product version a Windows executable declares (`VS_FIXEDFILEINFO`), trailing zero parts dropped.
pub fn product_version(data: &[u8]) -> Option<Vec<u32>> {
    let sig = [0xBDu8, 0x04, 0xEF, 0xFE];
    let pos = data.windows(4).position(|w| w == sig)?;
    let b = data.get(pos..pos + 24)?;
    let word = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
    if word(4) != 0x0001_0000 {
        return None;
    }
    let (ms, ls) = (word(16), word(20));
    let mut v = vec![ms >> 16, ms & 0xffff, ls >> 16, ls & 0xffff];
    while v.len() > 1 && v.last() == Some(&0) {
        v.pop();
    }
    Some(v)
}

pub fn version_text(v: &[u32]) -> String {
    v.iter().map(u32::to_string).collect::<Vec<_>>().join(".")
}

/// Where the trainer's output goes.
pub fn log_path(cache_root: &Path) -> PathBuf {
    cache_root.join("trainer.log")
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use crate::fsutil::testutil::TempDir;
    use std::collections::HashMap;
    use std::os::unix::fs::PermissionsExt;

    fn script(path: &Path, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, format!("#!/bin/sh\n{}\n", body)).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn env_of(c: &Command) -> HashMap<String, String> {
        c.get_envs().filter_map(|(k, v)| Some((k.to_string_lossy().to_string(), v?.to_string_lossy().to_string()))).collect()
    }

    fn launch<'a>(proton: Option<&'a Path>, trainer: &'a Path, compat: &'a Path, steam: &'a Path, log: &'a Path) -> Launch<'a> {
        Launch { trainer, pfx: "/compat/1091500/pfx", compat, proton, steam_root: steam, app_id: "1091500", env_lines: "PROTON_LOG=1\nbad line\n", log_file: log }
    }

    #[test]
    fn the_trainer_is_started_the_way_steam_starts_the_game() {
        let d = TempDir::new("trainer-cmd");
        let proton = d.join("Proton-X/proton");
        script(&proton, "exit 0");
        let trainer = d.join("t/FLiNG.exe");
        let (compat, steam, log) = (d.join("compat/1091500"), d.join("Steam"), d.join("trainer.log"));
        let l = launch(Some(&proton), &trainer, &compat, &steam, &log);
        let cmds = commands(&l);
        assert_eq!(cmds.iter().map(|c| c.0).collect::<Vec<_>>(), vec!["proton run", "wine"], "Proton first, its own Wine as the second try");
        let (_, run) = &cmds[0];
        assert_eq!(run.get_program(), proton.as_os_str());
        assert_eq!(run.get_args().map(|a| a.to_string_lossy().to_string()).collect::<Vec<_>>(), vec!["run".to_string(), trainer.to_string_lossy().to_string()]);
        let e = env_of(run);
        assert_eq!(e["STEAM_COMPAT_DATA_PATH"], compat.to_string_lossy());
        assert_eq!(e["STEAM_COMPAT_CLIENT_INSTALL_PATH"], steam.to_string_lossy());
        assert_eq!(e["STEAM_COMPAT_APP_ID"], "1091500");
        assert_eq!(e["WINEPREFIX"], "/compat/1091500/pfx");
        assert_eq!(e["PROTON_LOG"], "1", "the game's own settings travel with it");
        assert_eq!(run.get_current_dir(), trainer.parent());
        // without a Proton there is only plain Wine
        let none = commands(&launch(None, &trainer, &compat, &steam, &log));
        assert_eq!(none.iter().map(|c| c.0).collect::<Vec<_>>(), vec!["wine"]);
    }

    #[test]
    fn a_trainer_that_keeps_running_counts_as_started_and_one_that_dies_falls_back_to_wine() {
        let d = TempDir::new("trainer-start");
        let (compat, steam, log) = (d.join("compat"), d.join("Steam"), d.join("out/trainer.log"));
        let trainer = d.join("t/x.exe");
        fs::create_dir_all(d.join("t")).unwrap();
        // proton works: it stays up
        let good = d.join("Good/proton");
        script(&good, "echo started \"$@\"; sleep 2");
        assert_eq!(start(&launch(Some(&good), &trainer, &compat, &steam, &log), Duration::from_millis(300)).unwrap().how, "proton run");
        assert!(fs::read_to_string(&log).unwrap().contains("started run"));
        // proton refuses, its Wine works
        let bad = d.join("Bad/proton");
        script(&bad, "echo 'wineserver: version mismatch' >&2; exit 3");
        script(&d.join("Bad/files/bin/wine64"), "echo wine got \"$@\"; sleep 2");
        assert_eq!(start(&launch(Some(&bad), &trainer, &compat, &steam, &log), Duration::from_millis(300)).unwrap().how, "wine");
        let said = fs::read_to_string(&log).unwrap();
        assert!(said.contains("version mismatch") && said.contains("wine got"), "both tries are in the log: {said}");
        // nothing works: the message carries what was printed
        let worse = d.join("Worse/proton");
        script(&worse, "echo 'no such prefix' >&2; exit 2");
        script(&d.join("Worse/files/bin/wine64"), "echo 'bad exe format' >&2; exit 1");
        let err = start(&launch(Some(&worse), &trainer, &compat, &steam, &log), Duration::from_millis(300)).unwrap_err();
        assert!(err.contains("no such prefix") && err.contains("bad exe format"), "{err}");
    }

    #[test]
    fn the_usual_causes_are_recognised_from_what_the_trainer_printed() {
        assert_eq!(cause_of("err:module:import_dll Library MSVCP140.dll (which is needed by L\"Z:\\t.exe\") not found"), Some(Cause::CppRuntime));
        assert_eq!(cause_of("err:mscoree:CLRRuntimeInfo_GetRuntimeHost Wine Mono is not installed"), Some(Cause::DotNet));
        assert_eq!(cause_of("wineserver: version mismatch 811/815 - wineserver binary is from a different version"), Some(Cause::OtherBuild));
        assert_eq!(cause_of("fixme:ntdll:something unimportant"), None);
        assert_eq!(cause_of("the vcruntime was mentioned, nothing is wrong"), None);
    }

    #[test]
    fn the_end_of_a_started_trainer_is_noticed() {
        let d = TempDir::new("trainer-end");
        let (compat, steam, log) = (d.join("compat"), d.join("Steam"), d.join("trainer.log"));
        let trainer = d.join("t/x.exe");
        fs::create_dir_all(d.join("t")).unwrap();
        let quick = d.join("Quick/proton");
        script(&quick, "sleep 0.4; exit 5");
        let s = start(&launch(Some(&quick), &trainer, &compat, &steam, &log), Duration::from_millis(100)).unwrap();
        assert!(s.pid > 0);
        let until = Instant::now() + Duration::from_secs(5);
        while s.ended.lock().unwrap().is_none() && Instant::now() < until {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(s.ended.lock().unwrap().as_ref().is_some_and(|(_, st)| st.contains('5')), "the exit status is kept");
    }

    #[test]
    fn the_versions_a_trainer_is_made_for_come_from_its_name() {
        assert_eq!(made_for("Cyberpunk 2077 v2.0-v2.5 Plus 46 Trainer.exe"), Some((vec![2, 0], vec![2, 5])));
        assert_eq!(made_for("hydra_steam_360430_Mafia III Definitive Edition v1.100.0 Plus 18 Trainer.exe"), Some((vec![1, 100, 0], vec![1, 100, 0])));
        assert_eq!(made_for("Game v1.0.3 - v1.2 Trainer.exe"), Some((vec![1, 0, 3], vec![1, 2])));
        assert_eq!(made_for("MPayne3+11Tr-LNG.exe"), None);
        assert_eq!(made_for("WeMod Setup.exe"), None);
        assert_eq!(made_for("Elden Ring v1 Trainer.exe"), None, "a lone number says too little");
        assert_eq!(made_for("dev2.5 build.exe"), None);
        let r = made_for("Cyberpunk 2077 v2.0-v2.5 Plus 46 Trainer.exe").unwrap();
        use std::cmp::Ordering::*;
        assert_eq!(fits(&[2, 5], &r), Some(Equal));
        assert_eq!(fits(&[2, 5, 0, 1], &r), Some(Equal), "v2.5 covers its hotfixes");
        assert_eq!(fits(&[2, 31], &r), Some(Greater));
        assert_eq!(fits(&[2, 0], &r), Some(Equal));
        assert_eq!(fits(&[1, 6], &r), None, "another first number: not comparable");
        assert_eq!(fits(&[2021, 3, 1], &r), None, "a Unity version is not a game version");
        let one = made_for("Mafia III v1.100.0 Trainer.exe").unwrap();
        assert_eq!(fits(&[1, 100], &one), Some(Equal));
        assert_eq!(fits(&[1, 110], &one), Some(Greater));
        assert_eq!(fits(&[1, 90], &one), Some(Less));
    }

    #[test]
    fn the_product_version_is_read_from_the_version_resource() {
        let mut exe = b"MZ.....".to_vec();
        exe.extend_from_slice(&[0xBD, 0x04, 0xEF, 0xFE, 0, 0, 1, 0]);
        // file version 3.0.88.46235, product version 2.5.0.0, like Cyberpunk 2077 2.5
        exe.extend_from_slice(&((3u32 << 16).to_le_bytes()));
        exe.extend_from_slice(&((88u32 << 16 | 46235).to_le_bytes()));
        exe.extend_from_slice(&((2u32 << 16 | 5).to_le_bytes()));
        exe.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(product_version(&exe), Some(vec![2, 5]));
        assert_eq!(version_text(&[2, 5]), "2.5");
        assert_eq!(product_version(b"MZ no resource"), None);
    }

    #[test]
    fn the_tail_keeps_the_last_lines_and_skips_the_markers() {
        let d = TempDir::new("trainer-tail");
        let p = d.join("l.log");
        fs::write(&p, "-- proton run --\none\n\ntwo\n-- wine --\nthree\nfour\n").unwrap();
        assert_eq!(tail(&p, 2), "three | four");
        assert_eq!(tail(&p, 10), "one | two | three | four");
        assert_eq!(tail(&d.join("missing"), 3), "");
    }
}
