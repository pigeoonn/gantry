//! The checks and scans Gantry does on its own (health, mod updates, covers, backups...), in one list the
//! window shows. Each waits while a game runs unless the user lets them go on, and can be started at once.

use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

#[derive(Clone, Debug, PartialEq)]
pub struct Task {
    pub name: &'static str,
    /// The phrase the window shows (translated there).
    pub label: &'static str,
    pub every_ms: u64,
    pub last_ms: u64,
    pub next_ms: u64,
    pub running: bool,
    /// It is due but waiting for the game to close.
    pub held: bool,
    pub error: String,
    /// The last turn found it switched off (no sync folder, no Steam key...).
    pub off: bool,
}

static LIST: Mutex<Vec<Task>> = Mutex::new(Vec::new());
static WHILE_PLAYING: AtomicBool = AtomicBool::new(false);

fn list() -> std::sync::MutexGuard<'static, Vec<Task>> {
    LIST.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn set_while_playing(on: bool) {
    WHILE_PLAYING.store(on, Ordering::SeqCst);
}

/// Whether the checks have to wait now: a game runs and the user did not let them go on during play.
pub fn hold_for(playing: bool) -> bool {
    playing && !WHILE_PLAYING.load(Ordering::SeqCst)
}

pub fn register(name: &'static str, label: &'static str, first_ms: u64, every_ms: u64, now: u64) {
    let mut l = list();
    l.retain(|t| t.name != name);
    l.push(Task { name, label, every_ms, last_ms: 0, next_ms: now + first_ms, running: false, held: false, error: String::new(), off: false });
}

/// Whether a task may start now, and if so whether the user asked for it (Run now); a due task that has to
/// wait for the game (`hold`, from `hold_for`) is marked as held.
pub fn due(name: &str, now: u64, hold: bool) -> Option<bool> {
    let mut l = list();
    let t = l.iter_mut().find(|t| t.name == name)?;
    if t.running || now < t.next_ms {
        t.held = false;
        return None;
    }
    // next_ms 0: the user pressed Run now, which does not wait for the game
    t.held = hold && t.next_ms != 0;
    if t.held {
        return None;
    }
    t.running = true;
    Some(t.next_ms == 0)
}

/// The end of a turn: `Ok(false)` means there was nothing to do because it is switched off.
pub fn finished(name: &str, now: u64, res: Result<bool, String>) {
    let mut l = list();
    if let Some(t) = l.iter_mut().find(|t| t.name == name) {
        t.running = false;
        t.last_ms = now;
        t.next_ms = now + t.every_ms;
        t.off = matches!(res, Ok(false));
        t.error = res.err().unwrap_or_default();
    }
}

/// Start a task at its next look (within a few seconds), even while a game runs: the user asked.
pub fn run_now(name: &str) -> bool {
    let mut l = list();
    match l.iter_mut().find(|t| t.name == name) {
        Some(t) if !t.running => {
            t.next_ms = 0;
            t.held = false;
            t.error.clear();
            true
        }
        _ => false,
    }
}

pub fn status() -> Value {
    let l = list();
    json!({
        "while_playing": WHILE_PLAYING.load(Ordering::SeqCst),
        "tasks": l.iter().map(|t| json!({
            "name": t.name, "label": t.label, "every_ms": t.every_ms, "last_ms": t.last_ms, "next_ms": t.next_ms,
            "running": t.running, "held": t.held, "error": t.error, "off": t.off,
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_check_waits_for_its_time_and_for_the_game_unless_asked_and_then_comes_back_later() {
        register("t-wait", "Test", 1000, 5000, 0);
        assert_eq!(due("t-wait", 500, false), None, "not yet");
        assert_eq!(due("t-wait", 1500, true), None, "a game is running");
        assert!(status()["tasks"].as_array().unwrap().iter().any(|t| t["name"] == "t-wait" && t["held"] == true));
        assert_eq!(due("t-wait", 1600, false), Some(false), "the game closed");
        assert_eq!(due("t-wait", 1700, false), None, "it is already running");
        assert!(!run_now("t-wait"), "a running task is not started twice");
        finished("t-wait", 2000, Err("offline".into()));
        let row = status()["tasks"].as_array().unwrap().iter().find(|t| t["name"] == "t-wait").cloned().unwrap();
        assert_eq!((row["next_ms"].as_u64(), row["running"].as_bool(), row["error"].as_str()), (Some(7000), Some(false), Some("offline")));
        assert_eq!(due("t-wait", 3000, false), None);
        assert!(run_now("t-wait"));
        assert_eq!(due("t-wait", 3000, true), Some(true), "asked to run now, so it does not wait for the game");
        finished("t-wait", 3100, Ok(false));
        assert!(status()["tasks"].as_array().unwrap().iter().any(|t| t["name"] == "t-wait" && t["off"] == true && t["error"] == ""));
        assert!(!run_now("nope"));
    }
}
