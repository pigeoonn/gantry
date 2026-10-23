//! mod.io: the mod site many newer games use. Its public API lets anyone with a free read-only key list a game's mods and
//! download them, as long as the game's makers allow that. The key is the user's own (made at mod.io/me/access) and stays
//! on this computer; it is sent only to mod.io, never printed, and kept out of the process list.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::process::Command;

pub const DEFAULT_BASE: &str = "https://api.mod.io";

/// Where a library mod came from on mod.io, so it can be found again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModioRef {
    pub game_id: u64,
    pub mod_id: u64,
    pub file_id: u64,
}

impl ModioRef {
    pub fn valid(&self) -> bool {
        self.game_id > 0 && self.mod_id > 0
    }
}

/// Keys are 32 hexadecimal characters.
pub fn valid_key(k: &str) -> bool {
    k.len() == 32 && k.chars().all(|c| c.is_ascii_hexdigit())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Game {
    pub id: u64,
    pub name: String,
    /// Whether the makers let other programs list and download this game's mods.
    pub open: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Card {
    pub id: u64,
    pub name: String,
    pub summary: String,
    pub author: String,
    pub version: String,
    pub downloads: u64,
    pub updated_ms: u64,
    pub size_kb: u64,
    pub picture: String,
    pub page_url: String,
    pub adult: bool,
    pub has_deps: bool,
    pub file_id: u64,
    pub file_name: String,
    pub download_url: String,
    /// The author's text as plain text (empty in lists).
    pub description: String,
    /// What the author wrote about the current file.
    pub changelog: String,
    pub created_ms: u64,
    pub images: Vec<String>,
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dep {
    pub mod_id: u64,
    pub name: String,
}

pub struct Query {
    pub text: String,
    pub sort: String,
    pub offset: usize,
    pub limit: usize,
}

/// The `_sort` value for one of the store's sort names.
fn sort_of(sort: &str) -> &'static str {
    match sort {
        "lately" => "-downloads_today",
        "endorsed" => "-ratings_weighted_aggregate",
        "new" => "-date_live",
        "updated" => "-date_updated",
        _ => "-downloads_total",
    }
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

fn n(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(Value::as_u64).unwrap_or(0)
}

/// A mod object of the API as a card.
pub fn card_of(v: &Value) -> Option<Card> {
    let id = n(v, "id");
    if id == 0 {
        return None;
    }
    let file = v.get("modfile").cloned().unwrap_or(Value::Null);
    Some(Card {
        id,
        name: s(v, "name"),
        summary: s(v, "summary"),
        author: v.get("submitted_by").map(|u| s(u, "username")).unwrap_or_default(),
        version: s(&file, "version"),
        downloads: v.get("stats").map(|x| n(x, "downloads_total")).unwrap_or(0),
        updated_ms: n(v, "date_updated") * 1000,
        size_kb: n(&file, "filesize") / 1024,
        picture: v.get("logo").map(|l| if s(l, "thumb_320x180").is_empty() { s(l, "original") } else { s(l, "thumb_320x180") }).unwrap_or_default(),
        page_url: s(v, "profile_url"),
        adult: n(v, "maturity_option") > 0,
        has_deps: v.get("dependencies").and_then(Value::as_bool).unwrap_or(false),
        file_id: n(&file, "id"),
        file_name: s(&file, "filename"),
        download_url: file.get("download").map(|d| s(d, "binary_url")).unwrap_or_default(),
        description: s(v, "description_plaintext"),
        changelog: s(&file, "changelog"),
        created_ms: n(v, "date_added") * 1000,
        images: v
            .get("media")
            .and_then(|m| m.get("images"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|i| Some(if s(i, "thumb_1280x720").is_empty() { s(i, "original") } else { s(i, "thumb_1280x720") }).filter(|u| !u.is_empty()))
            .take(6)
            .collect(),
        tags: v.get("tags").and_then(Value::as_array).into_iter().flatten().filter_map(|t| Some(s(t, "name")).filter(|n| !n.is_empty())).take(8).collect(),
    })
}

pub struct Modio {
    base: String,
    key: String,
    allow_http: bool,
}

impl Modio {
    /// `allow_http` exists only so tests can talk to a local fake server.
    pub fn new(base: &str, key: &str, allow_http: bool) -> Result<Self, String> {
        if !valid_key(key) {
            return Err("That does not look like a mod.io API key (32 letters and digits).".into());
        }
        Ok(Self { base: base.trim_end_matches('/').to_string(), key: key.to_string(), allow_http })
    }

    fn get(&self, path_query: &str) -> Result<Value, String> {
        let sep = if path_query.contains('?') { '&' } else { '?' };
        let url = format!("{}/v1{}{}api_key={}", self.base, path_query, sep, self.key);
        if !(url.starts_with("https://") || (self.allow_http && url.starts_with("http://"))) {
            return Err("Refusing a non-HTTPS address.".into());
        }
        // the address (which carries the key) goes in through standard input, not the command line
        let config = format!("url = \"{}\"\nuser-agent = \"Gantry/{}\"\nheader = \"Accept: application/json\"\n", url, env!("CARGO_PKG_VERSION"));
        let mut cmd = Command::new("curl");
        cmd.args(["-sS", "-L", "--max-time", "30", "-w", "\n%{http_code}", "-K", "-"]);
        let (out, err) = crate::nexus::curl_ok(&mut cmd, Some(&config))?;
        let (body, code) = out.rsplit_once('\n').unwrap_or(("", "000"));
        let code: u16 = code.trim().parse().unwrap_or(0);
        let json: Option<Value> = serde_json::from_str(body).ok();
        if (200..300).contains(&code) {
            return json.ok_or_else(|| "mod.io sent an unreadable reply.".to_string());
        }
        let said = json.as_ref().and_then(|v| v.get("error")).and_then(|e| e.get("message")).and_then(Value::as_str).map(|m| m.replace(&self.key, "…"));
        Err(match (code, said) {
            (0, _) => format!("Could not reach mod.io ({}).", err.replace(&self.key, "…").trim().lines().last().unwrap_or("network error")),
            (401, _) => "mod.io rejected the API key. Check it, or make a new one at mod.io/me/access.".into(),
            (403, m) => format!("mod.io refused this request{}. The makers of the game may only allow their mods to be used inside the game.", m.map(|m| format!(" ({m})")).unwrap_or_default()),
            (404, _) => "mod.io has no such game or mod.".into(),
            (429, _) => "mod.io rate limit reached; try again in a minute.".into(),
            (c, Some(m)) => format!("mod.io error {}: {}", c, m),
            (c, None) => format!("mod.io error {}.", c),
        })
    }

    /// Checks the key.
    pub fn check(&self) -> Result<(), String> {
        self.get("/games?_limit=1").map(|_| ())
    }

    /// Games whose name matches `name`.
    pub fn find_games(&self, name: &str) -> Result<Vec<Game>, String> {
        let v = self.get(&format!("/games?_q={}&_limit=30", crate::fsutil::url_encode(name)))?;
        Ok(v.get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|g| {
                let id = n(g, "id");
                (id > 0).then(|| {
                    let bits = n(g, "api_access_options");
                    // 1: other programs may use the API, 2: files may be downloaded directly, 4 and 8: the player must sign in
                    Game { id, name: s(g, "name"), open: bits & 1 != 0 && bits & 2 != 0 && bits & 12 == 0 }
                })
            })
            .collect())
    }

    /// One game by its number (None when mod.io does not know it).
    pub fn game(&self, id: u64) -> Result<Option<Game>, String> {
        match self.get(&format!("/games/{id}")) {
            Ok(g) => {
                let bits = n(&g, "api_access_options");
                Ok(Some(Game { id: n(&g, "id"), name: s(&g, "name"), open: bits & 1 != 0 && bits & 2 != 0 && bits & 12 == 0 }).filter(|g| g.id > 0))
            }
            Err(e) if e.contains("no such game") => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// One page of a game's mods and how many there are in all.
    pub fn mods(&self, game_id: u64, q: &Query) -> Result<(Vec<Card>, u64), String> {
        let mut path = format!("/games/{game_id}/mods?_sort={}&_limit={}&_offset={}", sort_of(&q.sort), q.limit.clamp(1, 100), q.offset);
        if !q.text.trim().is_empty() {
            path.push_str(&format!("&_q={}", crate::fsutil::url_encode(q.text.trim())));
        }
        let v = self.get(&path)?;
        let cards: Vec<Card> = v.get("data").and_then(Value::as_array).into_iter().flatten().filter_map(card_of).collect();
        Ok((cards, n(&v, "result_total")))
    }

    /// One mod with its current file.
    pub fn one(&self, game_id: u64, mod_id: u64) -> Result<Card, String> {
        card_of(&self.get(&format!("/games/{game_id}/mods/{mod_id}"))?).ok_or_else(|| "mod.io sent an unreadable mod.".to_string())
    }

    /// Mods this one needs (and what those need), nearest first.
    pub fn dependencies(&self, game_id: u64, mod_id: u64) -> Result<Vec<Dep>, String> {
        let v = self.get(&format!("/games/{game_id}/mods/{mod_id}/dependencies?recursive=true"))?;
        let mut out: Vec<(u64, Dep)> = v
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|d| {
                let id = n(d, "mod_id");
                (id > 0).then(|| (n(d, "dependency_depth"), Dep { mod_id: id, name: s(d, "name") }))
            })
            .collect();
        // the deepest first: what a dependency needs goes in before it
        out.sort_by_key(|x| std::cmp::Reverse(x.0));
        let mut seen = std::collections::HashSet::new();
        Ok(out.into_iter().map(|x| x.1).filter(|d| seen.insert(d.mod_id)).collect())
    }

    /// Downloads a file the API named (a mod.io address; no key is sent along).
    pub fn download(&self, url: &str, dest: &Path) -> Result<(), String> {
        let host = url.split("://").nth(1).and_then(|r| r.split(['/', ':']).next()).unwrap_or("");
        let ours = host == "mod.io" || host.ends_with(".mod.io") || host == "modapi.io" || host.ends_with(".modapi.io") || (self.allow_http && host == "127.0.0.1");
        if !ours || !(url.starts_with("https://") || (self.allow_http && url.starts_with("http://"))) {
            return Err("Refusing a download that is not from mod.io.".into());
        }
        crate::downloads::fetch(url, dest, None, "mod.io")
    }
}

#[cfg(test)]
pub mod fake {
    //! A tiny stand-in for mod.io, for tests.
    use std::sync::{Arc, Mutex};
    use tiny_http::{Response, Server};

    pub const KEY: &str = "0123456789abcdef0123456789abcdef";

    pub struct FakeModio {
        pub base: String,
        /// Added to every file number: raising it makes every mod look updated.
        pub bump: Arc<std::sync::atomic::AtomicU64>,
        /// Every request line the server saw (the key is replaced).
        pub requests: Arc<Mutex<Vec<String>>>,
    }

    /// Serves one open game (id 7, "Cool Racer"), one closed game, three mods (3 needs 2, 2 needs 1) and their files.
    pub fn start(zip: Vec<u8>) -> FakeModio {
        let server = Server::http("127.0.0.1:0").unwrap();
        let base = format!("http://{}", server.server_addr().to_ip().unwrap());
        let requests: Arc<Mutex<Vec<String>>> = Arc::default();
        let bump = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let (b2, r2, bump2) = (base.clone(), requests.clone(), bump.clone());
        std::thread::spawn(move || {
            for req in server.incoming_requests() {
                let url = req.url().to_string();
                r2.lock().unwrap().push(url.replace(KEY, "KEY"));
                let plus = bump2.load(std::sync::atomic::Ordering::SeqCst);
                let file = |id: u64| format!(r#"{{"id":{id},"filename":"mod{id}.zip","filesize":2048,"version":"1.{id}","changelog":"Fixed things in {id}","download":{{"binary_url":"{b2}/download/{id}.zip","date_expires":4102444800}}}}"#);
                let modv = |id: u64, deps: bool, adult: u8| {
                    format!(
                        r#"{{"id":{id},"name":"Mod {id}","summary":"About {id}","description_plaintext":"Long text about {id}","media":{{"images":[{{"original":"https://image.modcdn.io/pic{id}.png"}}]}},"tags":[{{"name":"Cars"}}],"date_added":1690000000,"profile_url":"https://mod.io/g/coolracer/m/mod-{id}","maturity_option":{adult},"dependencies":{deps},"date_updated":1700000000,"submitted_by":{{"username":"maker"}},"stats":{{"downloads_total":{}}},"logo":{{"thumb_320x180":"https://image.modcdn.io/{id}.png"}},"modfile":{file}}}"#,
                        id * 100,
                        file = file(id * 10 + plus)
                    )
                };
                let path = url.split('?').next().unwrap_or("").to_string();
                let keyed = url.contains(&format!("api_key={KEY}"));
                let (code, body): (u16, Vec<u8>) = if path.starts_with("/download/") {
                    (200, zip.clone())
                } else if !keyed {
                    (401, br#"{"error":{"code":401,"error_ref":11000,"message":"A valid api_key is required."}}"#.to_vec())
                } else {
                    let (c, t) = match path.as_str() {
                        "/v1/games" => (200, r#"{"data":[{"id":7,"name":"Cool Racer","api_access_options":3},{"id":8,"name":"Cool Racer Closed","api_access_options":1}],"result_total":2}"#.to_string()),
                        "/v1/games/7/mods" => (200, format!(r#"{{"data":[{},{},{},{}],"result_total":4,"result_offset":0,"result_limit":18}}"#, modv(1, false, 0), modv(2, true, 0), modv(3, true, 0), modv(9, false, 1))),
                        "/v1/games/7" => (200, r#"{"id":7,"name":"Cool Racer","api_access_options":3}"#.to_string()),
                        "/v1/games/8" => (200, r#"{"id":8,"name":"Cool Racer Closed","api_access_options":1}"#.to_string()),
                        "/v1/games/7/mods/1" => (200, modv(1, false, 0)),
                        "/v1/games/7/mods/2" => (200, modv(2, true, 0)),
                        "/v1/games/7/mods/3" => (200, modv(3, true, 0)),
                        "/v1/games/7/mods/3/dependencies" => (200, r#"{"data":[{"mod_id":2,"name":"Mod 2","dependency_depth":0},{"mod_id":1,"name":"Mod 1","dependency_depth":1}]}"#.to_string()),
                        "/v1/games/7/mods/2/dependencies" => (200, r#"{"data":[{"mod_id":1,"name":"Mod 1","dependency_depth":0}]}"#.to_string()),
                        p if p.ends_with("/dependencies") => (200, r#"{"data":[]}"#.to_string()),
                        _ => (404, r#"{"error":{"code":404,"message":"Not found"}}"#.to_string()),
                    };
                    (c, t.into_bytes())
                };
                let _ = req.respond(Response::from_data(body).with_status_code(code).with_header("Content-Type: application/json".parse::<tiny_http::Header>().unwrap()));
            }
        });
        FakeModio { base, bump, requests }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsutil::testutil::TempDir;

    #[test]
    fn keys_and_sorts_are_checked_and_mapped() {
        assert!(valid_key(fake::KEY) && !valid_key("short") && !valid_key(&"z".repeat(32)));
        assert_eq!((sort_of("popular"), sort_of("new"), sort_of("lately"), sort_of("anything")), ("-downloads_total", "-date_live", "-downloads_today", "-downloads_total"));
        assert!(Modio::new(DEFAULT_BASE, "nope", false).is_err());
    }

    #[test]
    fn a_mod_becomes_a_card_even_when_parts_are_missing() {
        let c = card_of(&serde_json::json!({ "id": 5, "name": "X", "stats": { "downloads_total": 9 }, "modfile": { "id": 50, "filename": "x.zip", "filesize": 4096, "download": { "binary_url": "https://api.mod.io/v1/games/1/mods/5/files/50/download" } } })).unwrap();
        assert_eq!((c.id, c.downloads, c.size_kb, c.file_id, c.file_name.as_str()), (5, 9, 4, 50, "x.zip"));
        assert!(c.download_url.ends_with("/download") && c.picture.is_empty() && !c.adult);
        assert!(card_of(&serde_json::json!({ "name": "no id" })).is_none());
    }

    #[test]
    fn the_client_lists_games_mods_and_dependencies_and_downloads_without_the_key() {
        let d = TempDir::new("modio");
        let f = fake::start(b"ZIPDATA".to_vec());
        let m = Modio::new(&f.base, fake::KEY, true).unwrap();
        m.check().unwrap();
        let games = m.find_games("Cool Racer").unwrap();
        assert_eq!(games.iter().map(|g| (g.id, g.open)).collect::<Vec<_>>(), vec![(7, true), (8, false)], "a game that wants a sign-in or forbids third parties is not open");
        let (cards, total) = m.mods(7, &Query { text: "mod".into(), sort: "popular".into(), offset: 0, limit: 18 }).unwrap();
        assert_eq!((cards.len(), total, cards[0].name.as_str(), cards[0].downloads), (4, 4, "Mod 1", 100));
        assert!(cards[3].adult && cards[1].has_deps);
        assert_eq!(m.dependencies(7, 3).unwrap().iter().map(|x| x.mod_id).collect::<Vec<_>>(), vec![1, 2], "what a dependency needs comes first");
        let one = m.one(7, 2).unwrap();
        let dest = d.join("got.zip");
        m.download(&one.download_url, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"ZIPDATA");
        assert!(m.download("https://evil.example/x.zip", &d.join("no.zip")).is_err(), "only mod.io addresses");
        // the key was used for the API and not for the file, and is not in an error message
        let seen = f.requests.lock().unwrap().clone();
        assert!(seen.iter().filter(|u| u.contains("/download/")).all(|u| !u.contains("KEY")));
        let bad = Modio::new(&f.base, "ffffffffffffffffffffffffffffffff", true).unwrap().check().unwrap_err();
        assert!(bad.contains("rejected") && !bad.contains("ffffffff"), "{bad}");
    }
}
