//! Every file Gantry downloads goes through here, so one list can show what is coming in:
//! progress, speed, errors, and a way to stop it.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Clone)]
struct Item {
    id: u64,
    name: String,
    source: String,
    total: u64,
    done: u64,
    speed: u64,
    /// "active", "done", "failed" or "cancelled"
    state: &'static str,
    error: String,
    started_ms: u64,
    finished_ms: u64,
    cancel: bool,
    /// Held back until the game that is running has closed.
    waiting: bool,
}

/// How downloads behave: a speed cap per download (0 = none) and whether to hold new ones while a game runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Limits {
    pub kb_per_sec: u64,
    pub wait_for_game: bool,
}

static LIMITS: Mutex<Limits> = Mutex::new(Limits { kb_per_sec: 0, wait_for_game: false });
static GAME: OnceLock<Arc<AtomicBool>> = OnceLock::new();

pub fn set_limits(l: Limits) {
    *LIMITS.lock().unwrap_or_else(|e| e.into_inner()) = l;
}

/// The flag that says a game is running (the app's own), consulted when `wait_for_game` is on.
pub fn watch_game(flag: Arc<AtomicBool>) {
    let _ = GAME.set(flag);
}

fn game_running() -> bool {
    GAME.get().map(|f| f.load(Ordering::SeqCst)).unwrap_or(false)
}

/// curl's `--limit-rate` value for a cap in KB/s (nothing when there is no cap; never below 16 KB/s, which would stall).
fn rate_arg(kb: u64) -> Option<String> {
    (kb > 0).then(|| format!("{}k", kb.max(16)))
}

/// The curl arguments that cap the speed (none without a cap).
fn limit_args(l: Limits) -> Vec<String> {
    rate_arg(l.kb_per_sec).map(|r| vec!["--limit-rate".to_string(), r]).unwrap_or_default()
}

static LIST: Mutex<Vec<Item>> = Mutex::new(Vec::new());
static NEXT: AtomicU64 = AtomicU64::new(1);
const KEEP: usize = 40;

fn list() -> std::sync::MutexGuard<'static, Vec<Item>> {
    LIST.lock().unwrap_or_else(|e| e.into_inner())
}

fn update(id: u64, f: impl FnOnce(&mut Item)) {
    if let Some(i) = list().iter_mut().find(|i| i.id == id) {
        f(i);
    }
}

/// The size the server announced, from the response headers curl saved (the last one wins: redirects come first).
fn announced_size(headers: &Path) -> u64 {
    std::fs::read_to_string(headers)
        .ok()
        .and_then(|t| t.lines().rev().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").and_then(|v| v.trim().parse().ok())))
        .unwrap_or(0)
}

/// Download `url` to `dest` with curl and report it in the list. `source` is a short label ("Nexus Mods").
pub fn fetch(url: &str, dest: &Path, user_agent: Option<&str>, source: &str) -> Result<(), String> {
    let limits = *LIMITS.lock().unwrap_or_else(|e| e.into_inner());
    fetch_with(url, dest, user_agent, source, limits, &game_running)
}

/// `fetch` with the limits and the "is a game running" check given.
pub fn fetch_with(url: &str, dest: &Path, user_agent: Option<&str>, source: &str, limits: Limits, game_on: &dyn Fn() -> bool) -> Result<(), String> {
    // an address that starts with a dash would be read by curl as an option
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("The download address is not a web address.".into());
    }
    let id = NEXT.fetch_add(1, Ordering::SeqCst);
    let name = dest.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| url.to_string());
    {
        let mut l = list();
        l.push(Item { id, name, source: source.to_string(), total: 0, done: 0, speed: 0, state: "active", error: String::new(), started_ms: crate::fsutil::now_ms(), finished_ms: 0, cancel: false, waiting: false });
    }
    // while a game runs a new download waits (it can be cancelled meanwhile); one that has begun is left alone
    if limits.wait_for_game && game_on() {
        update(id, |i| i.waiting = true);
        while game_on() {
            if list().iter().any(|i| i.id == id && i.cancel) {
                update(id, |i| {
                    i.waiting = false;
                    i.state = "cancelled";
                    i.finished_ms = crate::fsutil::now_ms();
                });
                return Err("Cancelled.".to_string());
            }
            std::thread::sleep(Duration::from_millis(400));
        }
        update(id, |i| i.waiting = false);
    }
    let headers: PathBuf = dest.with_extension("headers");
    let mut cmd = Command::new("curl");
    cmd.args(["-fL", "--retry", "2", "--connect-timeout", "15", "--max-time", "3600", "-sS"]);
    cmd.args(limit_args(limits));
    if let Some(ua) = user_agent {
        cmd.args(["-A", ua]);
    }
    cmd.arg("-D").arg(&headers).arg("-o").arg(dest).arg(url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let result = match cmd.spawn() {
        Err(_) => Err("curl could not be started.".to_string()),
        Ok(mut child) => {
            let mut last = (Instant::now(), 0u64);
            let outcome = loop {
                match child.try_wait() {
                    Ok(Some(s)) => break if s.success() { Ok(()) } else { Err("The download failed.".to_string()) },
                    Ok(None) => {}
                    Err(e) => break Err(e.to_string()),
                }
                if list().iter().any(|i| i.id == id && i.cancel) {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err("Cancelled.".to_string());
                }
                let done = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
                let total = announced_size(&headers);
                // a file that cannot fit is stopped at once, not when the drive is full
                if total > 0 {
                    if let Some(free) = dest.parent().and_then(crate::fsutil::free_bytes) {
                        if total.saturating_add(256 << 20) > free.saturating_add(done) {
                            let _ = child.kill();
                            let _ = child.wait();
                            break Err(format!("Not enough free space for {}: it needs about {} and only {} is free there. Free some space and try again.", dest.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(), crate::fsutil::size_text(total + (256 << 20)), crate::fsutil::size_text(free)));
                        }
                    }
                }
                let dt = last.0.elapsed();
                let speed = if dt >= Duration::from_millis(700) {
                    let s = (done.saturating_sub(last.1) as f64 / dt.as_secs_f64()) as u64;
                    last = (Instant::now(), done);
                    Some(s)
                } else {
                    None
                };
                update(id, |i| {
                    i.done = done;
                    if total > 0 {
                        i.total = total;
                    }
                    if let Some(s) = speed {
                        i.speed = s;
                    }
                });
                std::thread::sleep(Duration::from_millis(120));
            };
            outcome
        }
    };
    let _ = std::fs::remove_file(&headers);
    let size = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
    let result = match result {
        Ok(()) if size == 0 => Err("The download was empty.".to_string()),
        r => r,
    };
    if result.is_err() {
        let _ = std::fs::remove_file(dest);
    }
    update(id, |i| {
        i.finished_ms = crate::fsutil::now_ms();
        i.speed = 0;
        match &result {
            Ok(()) => {
                i.state = "done";
                i.done = size;
                i.total = size;
            }
            Err(e) if e == "Cancelled." => i.state = "cancelled",
            Err(e) => {
                i.state = "failed";
                i.error = e.clone();
            }
        }
    });
    // only the newest are kept
    let mut l = list();
    let finished = l.iter().filter(|i| i.state != "active").count();
    if finished > KEEP {
        let mut drop = finished - KEEP;
        l.retain(|i| {
            if i.state != "active" && drop > 0 {
                drop -= 1;
                false
            } else {
                true
            }
        });
    }
    result
}

/// Ask a running download to stop.
pub fn cancel(id: u64) -> bool {
    let mut l = list();
    match l.iter_mut().find(|i| i.id == id && i.state == "active") {
        Some(i) => {
            i.cancel = true;
            true
        }
        None => false,
    }
}

/// Forget the finished entries.
pub fn clear_finished() {
    list().retain(|i| i.state == "active");
}

/// `{ active, items }`, newest first.
pub fn status() -> Value {
    let l = list();
    json!({
        "active": l.iter().filter(|i| i.state == "active").count(),
        "items": l.iter().rev().map(|i| json!({
            "id": i.id, "name": i.name, "source": i.source, "total": i.total, "done": i.done, "speed": i.speed,
            "state": i.state, "waiting": i.waiting, "error": i.error, "started_ms": i.started_ms, "finished_ms": i.finished_ms,
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsutil::testutil::TempDir;
    use std::io::Read;

    #[test]
    fn an_address_that_is_not_a_web_address_never_reaches_curl() {
        let d = TempDir::new("dlbad");
        for bad in ["--output=/tmp/x", "-K/etc/passwd", "file:///etc/passwd", "ftp://x/y", ""] {
            assert!(fetch(bad, &d.join("f.zip"), None, "test").is_err(), "{bad}");
        }
        assert!(!d.join("f.zip").exists());
    }

    /// A body that hands out its bytes slowly, so a download can be watched and stopped.
    struct Slow(usize);
    impl Read for Slow {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.0 == 0 {
                return Ok(0);
            }
            std::thread::sleep(Duration::from_millis(40));
            let n = buf.len().min(self.0).min(8 * 1024);
            buf[..n].fill(b'x');
            self.0 -= n;
            Ok(n)
        }
    }

    fn serve(len: usize, slow: bool) -> String {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}/f.bin", server.server_addr().to_ip().unwrap().port());
        std::thread::spawn(move || {
            for req in server.incoming_requests() {
                let body: Box<dyn Read + Send> = if slow { Box::new(Slow(len)) } else { Box::new(std::io::Cursor::new(vec![b'x'; len])) };
                let _ = req.respond(tiny_http::Response::new(tiny_http::StatusCode(200), vec![], body, Some(len), None));
            }
        });
        url
    }

    fn mine(name: &str) -> Value {
        status()["items"].as_array().unwrap().iter().find(|i| i["name"] == name).cloned().unwrap_or(Value::Null)
    }

    #[test]
    fn a_download_is_listed_with_its_size_and_ends_as_done() {
        if crate::fsutil::find_in_path("curl").is_none() {
            return;
        }
        let d = TempDir::new("dl-ok");
        let dest = d.path().join("done-one.bin");
        fetch(&serve(50_000, false), &dest, None, "Test").unwrap();
        let i = mine("done-one.bin");
        assert_eq!(i["state"], "done");
        assert_eq!((i["done"].as_u64(), i["total"].as_u64()), (Some(50_000), Some(50_000)));
        assert_eq!(i["source"], "Test");
        assert_eq!(std::fs::metadata(&dest).unwrap().len(), 50_000);
    }

    #[test]
    fn a_running_download_can_be_stopped_and_leaves_nothing_behind() {
        if crate::fsutil::find_in_path("curl").is_none() {
            return;
        }
        let d = TempDir::new("dl-cancel");
        let dest = d.path().join("stop-me.bin");
        let url = serve(4_000_000, true);
        let (dest2, d2) = (dest.clone(), d.path().to_path_buf());
        let t = std::thread::spawn(move || {
            let _keep = d2;
            fetch(&url, &dest2, None, "Test")
        });
        let mut id = 0;
        for _ in 0..100 {
            let i = mine("stop-me.bin");
            if i["done"].as_u64().unwrap_or(0) > 0 {
                id = i["id"].as_u64().unwrap();
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(id > 0, "the download never showed progress");
        assert!(cancel(id));
        assert_eq!(t.join().unwrap(), Err("Cancelled.".to_string()));
        assert_eq!(mine("stop-me.bin")["state"], "cancelled");
        assert!(!dest.exists());
    }

    #[test]
    fn the_speed_cap_becomes_curls_limit_and_never_stalls_a_download() {
        assert_eq!(rate_arg(0), None);
        assert_eq!(rate_arg(1024).as_deref(), Some("1024k"));
        assert_eq!(rate_arg(3).as_deref(), Some("16k"));
    }

    #[test]
    fn a_capped_download_passes_the_cap_to_curl_and_still_arrives_whole() {
        // how strictly curl holds a cap on a fast local connection differs between versions, so the argument is checked, not the clock
        assert_eq!(limit_args(Limits { kb_per_sec: 100, wait_for_game: false }), vec!["--limit-rate".to_string(), "100k".to_string()]);
        assert!(limit_args(Limits::default()).is_empty());
        if crate::fsutil::find_in_path("curl").is_none() {
            return;
        }
        let d = TempDir::new("dl-cap");
        let dest = d.path().join("capped-one.bin");
        fetch_with(&serve(300_000, false), &dest, None, "Test", Limits { kb_per_sec: 4000, wait_for_game: false }, &|| false).unwrap();
        assert_eq!(std::fs::metadata(&dest).unwrap().len(), 300_000);
    }

    #[test]
    fn a_new_download_waits_for_the_game_to_close_and_can_be_cancelled_while_waiting() {
        if crate::fsutil::find_in_path("curl").is_none() {
            return;
        }
        let d = TempDir::new("dl-wait");
        let playing = Arc::new(AtomicBool::new(true));
        let (p2, url, dest) = (playing.clone(), serve(10_000, false), d.path().join("waits-for-game.bin"));
        let dest2 = dest.clone();
        let t = std::thread::spawn(move || fetch_with(&url, &dest2, None, "Test", Limits { kb_per_sec: 0, wait_for_game: true }, &|| p2.load(Ordering::SeqCst)));
        std::thread::sleep(Duration::from_millis(900));
        let i = mine("waits-for-game.bin");
        assert_eq!((i["state"].as_str(), i["waiting"].as_bool()), (Some("active"), Some(true)), "{i}");
        assert!(!dest.exists(), "nothing is fetched while the game runs");
        playing.store(false, Ordering::SeqCst);
        t.join().unwrap().unwrap();
        let i = mine("waits-for-game.bin");
        assert_eq!((i["state"].as_str(), i["waiting"].as_bool()), (Some("done"), Some(false)));
        // and a waiting one can be given up on
        playing.store(true, Ordering::SeqCst);
        let (p3, url2, dest3) = (playing.clone(), serve(10_000, false), d.path().join("give-up.bin"));
        let t2 = std::thread::spawn(move || fetch_with(&url2, &dest3, None, "Test", Limits { kb_per_sec: 0, wait_for_game: true }, &|| p3.load(Ordering::SeqCst)));
        let mut id = 0;
        for _ in 0..40 {
            let i = mine("give-up.bin");
            if i["waiting"] == true {
                id = i["id"].as_u64().unwrap();
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(id > 0 && cancel(id));
        assert_eq!(t2.join().unwrap(), Err("Cancelled.".to_string()));
        assert_eq!(mine("give-up.bin")["state"], "cancelled");
    }

    #[test]
    fn a_failing_download_says_so() {
        if crate::fsutil::find_in_path("curl").is_none() {
            return;
        }
        let d = TempDir::new("dl-fail");
        let dest = d.path().join("nothing-here.bin");
        assert!(fetch("http://127.0.0.1:1/x", &dest, None, "Test").is_err());
        let i = mine("nothing-here.bin");
        assert_eq!(i["state"], "failed");
        assert!(!i["error"].as_str().unwrap_or("").is_empty());
    }
}
