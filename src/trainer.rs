//! Starting a trainer next to a running game. A trainer only works when it lives in the same Wine world as the game:
//! same prefix, same Proton build and same settings. So it is started the way Steam starts the game (`proton run`, with
//! the game's compatibility data), and only when that fails right away, through the build's own Wine as a second try.
//! Whatever the trainer prints goes to a log file, so a trainer that does not show up can be explained.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
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

/// Starts the trainer. `Ok` names the way that worked; `Err` says what every way printed or why it stopped.
pub fn start(l: &Launch, settle: Duration) -> Result<&'static str, String> {
    if let Some(parent) = l.log_file.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let mut why: Vec<String> = Vec::new();
    for (i, (name, cmd)) in commands(l).into_iter().enumerate() {
        match start_one(cmd, l.log_file, i == 0, name, settle) {
            Ok(mut child) => {
                thread::spawn(move || {
                    let _ = child.wait();
                });
                return Ok(name);
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
        assert_eq!(start(&launch(Some(&good), &trainer, &compat, &steam, &log), Duration::from_millis(300)), Ok("proton run"));
        assert!(fs::read_to_string(&log).unwrap().contains("started run"));
        // proton refuses, its Wine works
        let bad = d.join("Bad/proton");
        script(&bad, "echo 'wineserver: version mismatch' >&2; exit 3");
        script(&d.join("Bad/files/bin/wine64"), "echo wine got \"$@\"; sleep 2");
        assert_eq!(start(&launch(Some(&bad), &trainer, &compat, &steam, &log), Duration::from_millis(300)), Ok("wine"));
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
    fn the_tail_keeps_the_last_lines_and_skips_the_markers() {
        let d = TempDir::new("trainer-tail");
        let p = d.join("l.log");
        fs::write(&p, "-- proton run --\none\n\ntwo\n-- wine --\nthree\nfour\n").unwrap();
        assert_eq!(tail(&p, 2), "three | four");
        assert_eq!(tail(&p, 10), "one | two | three | four");
        assert_eq!(tail(&d.join("missing"), 3), "");
    }
}
