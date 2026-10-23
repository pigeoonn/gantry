//! A small local log of play sessions (one JSON line each) and the numbers drawn from it.
//! Nothing here leaves the machine.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::Path;

const DAY_MS: u64 = 86_400_000;
/// Older lines are dropped when the file grows past this many sessions.
const KEEP: usize = 5000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub appid: String,
    pub name: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub secs: u64,
    pub crashed: bool,
    /// Mods that were switched on when the game started (0 = a plain run).
    pub mods: usize,
    /// What the session cost (memory, processor, frame times), when it could be measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub perf: Option<crate::perf::Summary>,
}

/// Add one session to the log.
pub fn record(path: &Path, s: &Session) {
    let Ok(line) = serde_json::to_string(s) else { return };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{line}");
    }
    // keep the file from growing for ever
    if let Ok(text) = fs::read_to_string(path) {
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() > KEEP + 200 {
            let _ = crate::fsutil::atomic_write(path, lines[lines.len() - KEEP..].join("\n") + "\n");
        }
    }
}

/// Every session in the log (damaged lines are skipped).
pub fn read(path: &Path) -> Vec<Session> {
    fs::read_to_string(path).map(|t| t.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()).unwrap_or_default()
}

/// Numbers for one game (`appid` given) or all of them: totals, the last 14 days, and the newest sessions.
/// `tz_minutes` is the browser's `getTimezoneOffset()` so days begin at the user's midnight.
pub fn summary(all: &[Session], appid: Option<&str>, now_ms: u64, tz_minutes: i64) -> Value {
    let mine: Vec<&Session> = all.iter().filter(|s| appid.map(|a| s.appid == a).unwrap_or(true)).collect();
    let local = |ms: u64| ((ms as i64 - tz_minutes * 60_000).max(0) as u64) / DAY_MS;
    let today = local(now_ms);
    let mut days = vec![0u64; 14];
    for s in &mine {
        let d = local(s.end_ms);
        if d <= today && today - d < 14 {
            days[(13 - (today - d)) as usize] += s.secs;
        }
    }
    let total: u64 = mine.iter().map(|s| s.secs).sum();
    let modded: u64 = mine.iter().filter(|s| s.mods > 0).map(|s| s.secs).sum();
    let crashes = mine.iter().filter(|s| s.crashed).count();
    let longest = mine.iter().map(|s| s.secs).max().unwrap_or(0);
    let recent: Vec<Value> = mine.iter().rev().take(40).map(|s| json!({ "appid": s.appid, "name": s.name, "start_ms": s.start_ms, "end_ms": s.end_ms, "secs": s.secs, "crashed": s.crashed, "mods": s.mods, "perf": s.perf })).collect();
    json!({
        "sessions": mine.len(), "total_secs": total, "modded_secs": modded, "crashes": crashes,
        "average_secs": if mine.is_empty() { 0 } else { total / mine.len() as u64 },
        "longest_secs": longest, "days": days, "recent": recent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsutil::testutil::TempDir;

    fn s(appid: &str, end: u64, secs: u64, crashed: bool, mods: usize) -> Session {
        Session { appid: appid.into(), name: appid.into(), start_ms: end - secs * 1000, end_ms: end, secs, crashed, mods, perf: None }
    }

    #[test]
    fn sessions_are_logged_read_back_and_damaged_lines_are_skipped() {
        let d = TempDir::new("stats");
        let p = d.join("sessions.jsonl");
        record(&p, &s("a", 10 * DAY_MS, 600, false, 2));
        fs::write(&p, format!("{}garbage\n", fs::read_to_string(&p).unwrap())).unwrap();
        record(&p, &s("b", 10 * DAY_MS + 5, 60, true, 0));
        let all = read(&p);
        assert_eq!(all.len(), 2);
        assert_eq!(all[1].appid, "b");
        assert!(read(&d.join("none.jsonl")).is_empty());
    }

    #[test]
    fn the_summary_counts_per_game_and_buckets_days_in_local_time() {
        let now = 60 * DAY_MS + 3_600_000; // 01:00 UTC on day 60
        let all = vec![s("a", now - 1000, 600, false, 1), s("a", now - DAY_MS, 1200, true, 0), s("b", now, 99, false, 0), s("a", now - 30 * DAY_MS, 5, false, 0)];
        let v = summary(&all, Some("a"), now, 0);
        assert_eq!((v["sessions"].as_u64(), v["total_secs"].as_u64(), v["crashes"].as_u64()), (Some(3), Some(1805), Some(1)));
        assert_eq!((v["modded_secs"].as_u64(), v["longest_secs"].as_u64()), (Some(600), Some(1200)));
        let days: Vec<u64> = v["days"].as_array().unwrap().iter().map(|x| x.as_u64().unwrap()).collect();
        assert_eq!((days[13], days[12], days.iter().sum::<u64>()), (600, 1200, 1800), "the 30-day-old session is outside the chart");
        // a user at UTC-5 (offset +300) starts the day at 05:00 UTC: a session at 02:00 UTC still belongs to the day before
        let morning = 60 * DAY_MS + 10 * 3_600_000;
        let two = vec![s("a", 60 * DAY_MS + 2 * 3_600_000, 100, false, 0), s("a", morning, 50, false, 0)];
        let utc: Vec<u64> = summary(&two, None, morning, 0)["days"].as_array().unwrap().iter().map(|x| x.as_u64().unwrap()).collect();
        let west: Vec<u64> = summary(&two, None, morning, 300)["days"].as_array().unwrap().iter().map(|x| x.as_u64().unwrap()).collect();
        assert_eq!((utc[13], utc[12]), (150, 0));
        assert_eq!((west[13], west[12]), (50, 100));
        assert_eq!(summary(&all, None, now, 0)["sessions"], 4);
        assert_eq!(summary(&[], None, now, 0)["average_secs"], 0);
    }
}
