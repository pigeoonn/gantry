//! Optional VirusTotal lookups. Off unless the user switches it on and gives their own key. What leaves the
//! computer is the SHA-256 fingerprint of a program file (never the file, its name, or which game it belongs
//! to); the key travels to curl on stdin so it never appears in a process list.

use crate::nexus::curl_ok;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

pub const BASE: &str = "https://www.virustotal.com";

/// What VirusTotal says about one file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    /// VirusTotal has seen the file before.
    pub known: bool,
    pub malicious: u32,
    pub suspicious: u32,
    pub harmless: u32,
    pub undetected: u32,
}

impl Verdict {
    /// Engines that said something other than "clean".
    pub fn flagged(&self) -> u32 {
        self.malicious + self.suspicious
    }

    pub fn engines(&self) -> u32 {
        self.malicious + self.suspicious + self.harmless + self.undetected
    }
}

/// VirusTotal keys are 64 hexadecimal characters.
pub fn key_ok(key: &str) -> bool {
    key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn sha_ok(sha: &str) -> bool {
    sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The reply to `GET /api/v3/files/<sha256>`.
pub fn parse_reply(code: u16, body: &str) -> Result<Verdict, String> {
    match code {
        200..=299 => {
            let v: Value = serde_json::from_str(body).map_err(|_| "VirusTotal sent an unreadable reply.".to_string())?;
            let stats = &v["data"]["attributes"]["last_analysis_stats"];
            let n = |k: &str| stats[k].as_u64().unwrap_or(0) as u32;
            Ok(Verdict { known: true, malicious: n("malicious"), suspicious: n("suspicious"), harmless: n("harmless"), undetected: n("undetected") })
        }
        404 => Ok(Verdict::default()),
        401 | 403 => Err("VirusTotal rejected the key. Check it in Settings.".into()),
        429 => Err("VirusTotal's limit for free keys is reached (4 a minute, 500 a day). Try again later.".into()),
        0 => Err("Could not reach VirusTotal.".into()),
        c => Err(format!("VirusTotal error {}.", c)),
    }
}

pub struct Client {
    base: String,
    key: String,
    allow_http: bool,
}

impl Client {
    pub fn new(base: &str, key: &str, allow_http: bool) -> Result<Self, String> {
        if !key_ok(key) {
            return Err("That does not look like a VirusTotal key.".into());
        }
        Ok(Self { base: base.trim_end_matches('/').to_string(), key: key.to_string(), allow_http })
    }

    /// Ask about one fingerprint.
    pub fn lookup(&self, sha256: &str) -> Result<Verdict, String> {
        if !sha_ok(sha256) {
            return Err("Not a SHA-256 fingerprint.".into());
        }
        let url = format!("{}/api/v3/files/{}", self.base, sha256.to_lowercase());
        if !(url.starts_with("https://") || (self.allow_http && url.starts_with("http://"))) {
            return Err("Refusing a non-HTTPS address.".into());
        }
        let config = format!("header = \"x-apikey: {}\"\nuser-agent = \"Gantry/{}\"\n", self.key, env!("CARGO_PKG_VERSION"));
        let mut cmd = Command::new("curl");
        cmd.args(["-sS", "--max-time", "30", "-w", "\n%{http_code}", "-K", "-"]);
        let (out, _err) = curl_ok(cmd.arg(&url), Some(&config))?;
        let (body, code) = out.rsplit_once('\n').unwrap_or(("", "000"));
        parse_reply(code.trim().parse().unwrap_or(0), body)
    }
}

/// Answers already known, so a file is asked about once in a while and not on every look.
#[derive(Default, Serialize, Deserialize)]
pub struct Cache {
    #[serde(default)]
    pub items: BTreeMap<String, (u64, Verdict)>,
}

/// How long an answer is trusted: a file VirusTotal knows for a few days, one it does not know for a day.
const KNOWN_TTL_MS: u64 = 3 * 24 * 3600 * 1000;
const UNKNOWN_TTL_MS: u64 = 24 * 3600 * 1000;

impl Cache {
    pub fn load(path: &Path) -> Cache {
        fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) {
        if let Ok(j) = serde_json::to_string(self) {
            let _ = crate::fsutil::atomic_write(path, j);
        }
    }

    pub fn get(&self, sha: &str, now_ms: u64) -> Option<Verdict> {
        let (at, v) = self.items.get(&sha.to_lowercase())?;
        let ttl = if v.known { KNOWN_TTL_MS } else { UNKNOWN_TTL_MS };
        (now_ms.saturating_sub(*at) < ttl).then(|| v.clone())
    }

    pub fn put(&mut self, sha: &str, v: Verdict, now_ms: u64) {
        self.items.insert(sha.to_lowercase(), (now_ms, v));
        if self.items.len() > 2000 {
            let mut by_age: Vec<(u64, String)> = self.items.iter().map(|(k, (t, _))| (*t, k.clone())).collect();
            by_age.sort();
            for (_, k) in by_age.into_iter().take(500) {
                self.items.remove(&k);
            }
        }
    }
}

#[cfg(test)]
pub mod fake {
    //! A stand-in for VirusTotal: which fingerprints it knows, and what was asked.
    use std::sync::{Arc, Mutex};
    use tiny_http::{Response, Server};

    pub const KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    /// "path" and "x-apikey" of a request
    pub type Asked = (String, Option<String>);

    pub struct FakeVt {
        pub base: String,
        /// "path" and "x-apikey" of every request
        pub seen: Arc<Mutex<Vec<Asked>>>,
        /// fingerprint -> (malicious, harmless)
        pub known: Arc<Mutex<std::collections::HashMap<String, (u32, u32)>>>,
    }

    pub fn start() -> FakeVt {
        let server = Server::http("127.0.0.1:0").unwrap();
        let base = format!("http://{}", server.server_addr().to_ip().unwrap());
        let seen: Arc<Mutex<Vec<Asked>>> = Arc::default();
        let known: Arc<Mutex<std::collections::HashMap<String, (u32, u32)>>> = Arc::default();
        let (s2, k2) = (seen.clone(), known.clone());
        std::thread::spawn(move || {
            for req in server.incoming_requests() {
                let url = req.url().to_string();
                let key = req.headers().iter().find(|h| h.field.equiv("x-apikey")).map(|h| h.value.as_str().to_string());
                s2.lock().unwrap().push((url.clone(), key.clone()));
                if key.as_deref() != Some(KEY) {
                    let _ = req.respond(Response::from_string("{}").with_status_code(401));
                    continue;
                }
                let sha = url.rsplit('/').next().unwrap_or("").to_string();
                let resp = match k2.lock().unwrap().get(&sha) {
                    Some((bad, good)) => Response::from_string(format!(r#"{{"data":{{"attributes":{{"last_analysis_stats":{{"malicious":{bad},"suspicious":0,"harmless":{good},"undetected":3}}}}}}}}"#)),
                    None => Response::from_string(r#"{"error":{"code":"NotFoundError"}}"#).with_status_code(404),
                };
                let _ = req.respond(resp);
            }
        });
        FakeVt { base, seen, known }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsutil::testutil::TempDir;

    #[test]
    fn replies_become_verdicts_and_errors_say_what_to_do() {
        let ok = r#"{"data":{"attributes":{"last_analysis_stats":{"malicious":2,"suspicious":1,"harmless":60,"undetected":9}}}}"#;
        let v = parse_reply(200, ok).unwrap();
        assert_eq!((v.known, v.flagged(), v.engines()), (true, 3, 72));
        assert_eq!(parse_reply(404, "{}").unwrap(), Verdict::default(), "unknown to VirusTotal is not an error");
        assert!(parse_reply(200, "nope").is_err());
        assert!(parse_reply(401, "").unwrap_err().contains("key"));
        assert!(parse_reply(429, "").unwrap_err().contains("limit"));
        assert!(parse_reply(0, "").unwrap_err().contains("reach"));
        assert!(parse_reply(500, "").is_err());
    }

    #[test]
    fn keys_and_fingerprints_are_checked_before_anything_is_sent() {
        assert!(key_ok(fake::KEY));
        for bad in ["", "short", &"g".repeat(64), &"a".repeat(63), &"a".repeat(65)] {
            assert!(!key_ok(bad), "{bad}");
            assert!(Client::new("https://x", bad, false).is_err());
        }
        let c = Client::new("https://www.virustotal.com", fake::KEY, false).unwrap();
        assert!(c.lookup("../../etc/passwd").is_err());
        assert!(c.lookup(&"a".repeat(63)).is_err());
        let plain = Client::new("http://example.invalid", fake::KEY, false).unwrap();
        assert!(plain.lookup(&"a".repeat(64)).unwrap_err().contains("non-HTTPS"), "no key over plain http");
    }

    #[test]
    fn a_lookup_sends_the_key_in_a_header_and_only_the_fingerprint_in_the_path() {
        let f = fake::start();
        let (bad, clean) = ("b".repeat(64), "c".repeat(64));
        f.known.lock().unwrap().insert(bad.clone(), (5, 50));
        let c = Client::new(&f.base, fake::KEY, true).unwrap();
        let v = c.lookup(&bad).unwrap();
        assert_eq!((v.known, v.malicious, v.harmless), (true, 5, 50));
        assert!(!c.lookup(&clean).unwrap().known);
        let seen = f.seen.lock().unwrap();
        assert_eq!(seen[0], (format!("/api/v3/files/{bad}"), Some(fake::KEY.to_string())));
        assert!(!seen.iter().any(|(u, _)| u.contains(fake::KEY)), "the key is never part of an address");
        drop(seen);
        let wrong = Client::new(&f.base, &"1".repeat(64), true).unwrap();
        assert!(wrong.lookup(&bad).unwrap_err().contains("key"));
    }

    #[test]
    fn the_cache_keeps_answers_for_a_while_and_stays_small() {
        let d = TempDir::new("vtcache");
        let path = d.join("vt.json");
        let mut c = Cache::default();
        let sha = "AB".repeat(32);
        c.put(&sha, Verdict { known: true, malicious: 1, ..Default::default() }, 1_000);
        c.put(&"cd".repeat(32), Verdict::default(), 1_000);
        c.save(&path);
        let c = Cache::load(&path);
        assert_eq!(c.get(&sha.to_lowercase(), 2_000).map(|v| v.malicious), Some(1), "fingerprints are compared in lower case");
        assert!(c.get(&sha, 1_000 + KNOWN_TTL_MS).is_none());
        assert!(c.get(&"cd".repeat(32), 1_000 + UNKNOWN_TTL_MS).is_none(), "an unknown file is asked about again sooner");
        assert!(Cache::load(&d.join("none.json")).items.is_empty());
        let mut big = Cache::default();
        for i in 0..2100u64 {
            big.put(&format!("{:064x}", i), Verdict::default(), i);
        }
        assert!(big.items.len() <= 1700);
        assert!(big.get(&format!("{:064x}", 2099), 2100).is_some(), "the newest are kept");
    }
}
