//! Process inspection: is Steam open, is the selected game running. On Linux this reads /proc,
//! on macOS it asks `ps`, on Windows it walks the process snapshot.
//! Detection is independent of who launched the game (rig, Steam, a terminal).

use crate::engine::GameEntry;
#[cfg(target_os = "linux")]
use std::fs;
use std::path::Path;

/// `(pid, lower-case image name)` of every process, for the systems without /proc.
#[cfg(windows)]
fn processes() -> Vec<(u32, String)> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
    let mut out = Vec::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = Process32FirstW(snap, &mut e) != 0;
        while ok {
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            out.push((e.th32ProcessID, String::from_utf16_lossy(&e.szExeFile[..len]).to_lowercase()));
            ok = Process32NextW(snap, &mut e) != 0;
        }
        CloseHandle(snap);
    }
    out
}

#[cfg(target_os = "macos")]
fn processes() -> Vec<(u32, String)> {
    let Ok(out) = std::process::Command::new("ps").args(["-axo", "pid=,command="]).output() else { return Vec::new() };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let (pid, cmd) = l.trim().split_once(' ')?;
            Some((pid.parse().ok()?, cmd.trim().to_lowercase()))
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn read_comm(pid: u32) -> String {
    fs::read_to_string(format!("/proc/{}/comm", pid))
        .map(|c| c.trim().to_string())
        .unwrap_or_default()
}

#[cfg(target_os = "linux")]
fn all_pids() -> impl Iterator<Item = u32> {
    fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_string_lossy().parse::<u32>().ok())
}

/// True when a process whose kernel name (`comm`) equals `name` exists.
#[cfg(target_os = "linux")]
pub fn is_process_named(name: &str) -> bool {
    all_pids().any(|pid| read_comm(pid) == name)
}

/// True when a process called `name` (with or without `.exe`) exists.
#[cfg(windows)]
pub fn is_process_named(name: &str) -> bool {
    let n = name.to_lowercase();
    processes().iter().any(|(_, p)| *p == n || *p == format!("{n}.exe"))
}

/// True when a process whose command line starts with a program called `name` exists.
#[cfg(target_os = "macos")]
pub fn is_process_named(name: &str) -> bool {
    let n = name.to_lowercase();
    processes().iter().any(|(_, c)| c.rsplit('/').next().map(|b| b == n || b.starts_with(&format!("{n} "))).unwrap_or(false) || c.contains(&format!(".app/contents/macos/{n}")))
}

/// Steam client (any of its install flavours reports comm "steam").
pub fn steam_running() -> bool {
    is_process_named("steam")
}

/// Lower-cased executable file name used to recognise the game's process, if known.
pub fn game_needle(game: &GameEntry) -> Option<String> {
    [&game.shipping_exe, &game.exe_path]
        .iter()
        .filter(|p| !p.is_empty())
        .find_map(|p| Path::new(p.as_str()).file_name())
        .map(|n| n.to_string_lossy().to_lowercase())
        .filter(|n| n.ends_with(".exe") && n.len() > 4)
}

/// Pids whose command line mentions `needle` (already lower-case). Our own process and
/// short-lived helpers that merely carry the name as an argument are ignored.
#[cfg(windows)]
pub fn pids_with_cmdline(needle: &str) -> Vec<u32> {
    let me = std::process::id();
    processes().into_iter().filter(|(pid, name)| *pid != me && name == needle).map(|(pid, _)| pid).collect()
}

#[cfg(target_os = "macos")]
pub fn pids_with_cmdline(needle: &str) -> Vec<u32> {
    let me = std::process::id();
    processes().into_iter().filter(|(pid, cmd)| *pid != me && cmd.contains(needle) && !cmd.starts_with("ps ") && !cmd.contains("pgrep")).map(|(pid, _)| pid).collect()
}

#[cfg(target_os = "linux")]
pub fn pids_with_cmdline(needle: &str) -> Vec<u32> {
    let me = std::process::id();
    all_pids()
        .filter(|&pid| pid != me)
        .filter(|&pid| {
            let Ok(raw) = fs::read(format!("/proc/{}/cmdline", pid)) else {
                return false;
            };
            if !cmdline_names(&raw, needle) {
                return false;
            }
            !matches!(read_comm(pid).as_str(), "pkill" | "pgrep" | "rig")
        })
        .collect()
}

/// Does this command line (as `/proc/<pid>/cmdline` has it: arguments separated by NUL) start or run the
/// program `needle` (a lower-case file name)? The name has to stand as a file name or a whole word, so a shell
/// that merely has a script or a search text mentioning the game does not count.
pub fn cmdline_names(raw: &[u8], needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let text = String::from_utf8_lossy(raw).to_lowercase();
    text.split('\0').filter(|a| a.len() <= 2000 && !a.contains('\n')).any(|arg| {
        let mut from = 0;
        while let Some(i) = arg[from..].find(needle) {
            let (a, b) = (from + i, from + i + needle.len());
            let before = arg[..a].chars().next_back();
            let after = arg[b..].chars().next();
            if before.map(|c| matches!(c, '/' | '\\' | '=' | ' ' | '"' | '\'')).unwrap_or(true) && after.map(|c| matches!(c, ' ' | '"' | '\'')).unwrap_or(true) {
                return true;
            }
            from = b;
        }
        false
    })
}

pub fn game_pids(game: &GameEntry) -> Vec<u32> {
    game_needle(game).map(|n| pids_with_cmdline(&n)).unwrap_or_default()
}

/// CPU time (user + system, in clock ticks) a process has used so far.
#[cfg(not(target_os = "linux"))]
fn cpu_ticks(_pid: u32) -> Option<u64> {
    None
}

#[cfg(target_os = "linux")]
fn cpu_ticks(pid: u32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // the command name may contain spaces and parentheses: fields start after the last ')'
    let rest = stat.rsplit_once(')')?.1;
    let f: Vec<&str> = rest.split_whitespace().collect();
    Some(f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?)
}

/// Resident memory (KiB) and processor time (clock ticks) of these processes added together,
/// `None` where the system does not tell.
#[cfg(target_os = "linux")]
pub fn usage(pids: &[u32]) -> Option<(u64, u64)> {
    let (mut rss, mut ticks, mut seen) = (0u64, 0u64, false);
    for &pid in pids {
        let Some(t) = cpu_ticks(pid) else { continue };
        let kb = fs::read_to_string(format!("/proc/{pid}/status"))
            .ok()
            .and_then(|s| s.lines().find_map(|l| l.strip_prefix("VmRSS:").and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())))
            .unwrap_or(0);
        rss += kb;
        ticks += t;
        seen = true;
    }
    seen.then_some((rss, ticks))
}

#[cfg(not(target_os = "linux"))]
pub fn usage(_pids: &[u32]) -> Option<(u64, u64)> {
    None
}

/// How busy the game's processes are over a short moment, as a percentage of one core
/// (`None` when the game is not running).
pub fn cpu_percent(game: &GameEntry, over_ms: u64) -> Option<f64> {
    let pids = game_pids(game);
    if pids.is_empty() {
        return None;
    }
    let before: Vec<(u32, u64)> = pids.iter().filter_map(|&p| Some((p, cpu_ticks(p)?))).collect();
    if before.is_empty() {
        return None; // no way to measure on this system
    }
    std::thread::sleep(std::time::Duration::from_millis(over_ms));
    let used: u64 = before.iter().filter_map(|&(p, t0)| cpu_ticks(p).map(|t1| t1.saturating_sub(t0))).sum();
    let ticks_per_sec = 100.0; // USER_HZ is 100 on Linux
    Some(used as f64 / (ticks_per_sec * over_ms as f64 / 1000.0) * 100.0)
}

pub fn is_game_running(game: &GameEntry) -> bool {
    !game_pids(game).is_empty()
}

/// The Wine prefix a process runs in (its `WINEPREFIX`, without a trailing slash), when the system tells.
#[cfg(target_os = "linux")]
pub fn wine_prefix_of(pid: u32) -> Option<String> {
    let raw = fs::read(format!("/proc/{pid}/environ")).ok()?;
    raw.split(|&b| b == 0).find_map(|kv| kv.strip_prefix(b"WINEPREFIX=")).map(|v| String::from_utf8_lossy(v).trim_end_matches('/').to_string()).filter(|v| !v.is_empty())
}

#[cfg(not(target_os = "linux"))]
pub fn wine_prefix_of(_pid: u32) -> Option<String> {
    None
}

/// Is this process still there?
#[cfg(target_os = "linux")]
pub fn alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists() && !fs::read_to_string(format!("/proc/{pid}/stat")).map(|s| s.rsplit_once(')').map(|(_, r)| r.trim_start().starts_with('Z')).unwrap_or(false)).unwrap_or(true)
}

#[cfg(not(target_os = "linux"))]
pub fn alive(pid: u32) -> bool {
    processes().iter().any(|(p, _)| *p == pid)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use std::process::{Command, Stdio};

    fn game_with(exe: &str) -> GameEntry {
        GameEntry {
            appid: "1".into(), name: "T".into(), game_dir: String::new(), exe_path: String::new(),
            shipping_exe: exe.into(), bin_dir: String::new(), engine: String::new(), compatdata: String::new(),
            pfx: String::new(), has_pfx: false, is_custom: false, is_installed: true, steam_appid: String::new(),
            launch_args: String::new(), env_vars: String::new(), trainer_name: String::new(), mod_count: 0, source: String::new(), proton_path: String::new(), ue4ss: false, loader: String::new(), cover_id: String::new(),
        }
    }

    #[test]
    fn needle_comes_from_the_exe_name_only() {
        assert_eq!(game_needle(&game_with("/g/Bin/Win64/Demo-Win64-Shipping.exe")).as_deref(), Some("demo-win64-shipping.exe"));
        assert_eq!(game_needle(&game_with("")), None);
        assert_eq!(game_needle(&game_with("/g/readme.txt")), None);
    }

    /// /proc/<pid>/cmdline is briefly empty or stale while a process is inside execve, so a test
    /// must wait for the state to settle instead of sampling once.
    #[cfg(target_os = "linux")]
    fn eventually(what: &str, mut f: impl FnMut() -> bool) {
        for _ in 0..100 {
            if f() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
        panic!("never became true: {what}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn finds_a_running_process_by_its_windows_style_command_line() {
        let exe = format!("Z:\\games\\RigProbe{}-Win64-Shipping.exe", std::process::id());
        let mut child = Command::new("bash")
            .args(["-c", &format!("exec -a '{}' sleep 30", exe)])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let game = game_with(&format!("/x/RigProbe{}-Win64-Shipping.exe", std::process::id()));
        let pid = child.id();
        eventually("process detected", || game_pids(&game).contains(&pid));
        eventually("detected as running", || is_game_running(&game));
        child.kill().unwrap();
        child.wait().unwrap();
        eventually("gone after exit", || !is_game_running(&game));
    }

    #[test]
    fn only_a_command_that_runs_the_game_counts_not_one_that_talks_about_it() {
        let n = "risk of rain 2.exe";
        assert!(cmdline_names(b"Z:\\games\\Risk of Rain 2.exe", n), "a Windows path");
        assert!(cmdline_names(b"/home/u/proton\0waitforexitandrun\0/mnt/g/Risk of Rain 2.exe", n), "an argument of Proton");
        assert!(cmdline_names(b"C:\\x\\Risk of Rain 2.exe -windowed", n), "a process title with arguments");
        assert!(cmdline_names(b"wine\0--exe=Risk of Rain 2.exe", n));
        assert!(!cmdline_names(b"bash\0-c\0cd x\ntouch 'Risk of Rain 2.exe'\necho done", n), "a script");
        assert!(!cmdline_names(b"grep\0-r\0xrisk of rain 2.exey", n), "inside a longer word");
        assert!(!cmdline_names(b"", n) && !cmdline_names(b"anything", ""));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn process_name_lookup() {
        assert!(!is_process_named("definitely-not-a-process-name"));
        let me = read_comm(std::process::id());
        assert!(is_process_named(&me));
    }

    #[cfg(windows)]
    #[test]
    fn windows_sees_its_own_process() {
        let me = std::env::current_exe().unwrap().file_name().unwrap().to_string_lossy().to_lowercase();
        assert!(processes().iter().any(|(pid, name)| *pid == std::process::id() && *name == me));
        assert!(!is_process_named("definitely-not-a-process-name"));
    }
}
