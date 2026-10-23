//! Everything you own on Steam, installed or not: the Steam accounts known to this computer
//! (`config/loginusers.vdf`) and, through a free Steam Web API key, the list of owned games.
//!
//! Steam no longer lists a library to anonymous visitors (not even for a public profile), so a key
//! is the one dependable way. A key can always read the library of the account that created it. It
//! travels to curl on stdin, never in the process list, and is stored in a file only you can read.

use crate::nexus::curl_ok;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::LazyLock;

pub const DEFAULT_BASE: &str = "https://api.steampowered.com";

/// A Steam account that has logged in on this computer.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Account {
    /// 17-digit SteamID64.
    pub steamid: String,
    /// The login name.
    pub name: String,
    /// The display name.
    pub persona: String,
    /// When it last logged in (Unix seconds).
    pub last_login: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Owned {
    pub appid: u64,
    pub name: String,
    /// Total playtime in minutes.
    #[serde(default)]
    pub minutes: u64,
    /// Unix seconds, 0 = never.
    #[serde(default)]
    pub last_played: u64,
}

static USER_BLOCK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""(\d{17})"\s*\{([^}]*)\}"#).unwrap());
static KV: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""([^"]+)"\s+"([^"]*)""#).unwrap());

/// Accounts listed in a `loginusers.vdf`, newest login first.
pub fn parse_accounts(vdf: &str) -> Vec<Account> {
    let mut out: Vec<Account> = USER_BLOCK
        .captures_iter(vdf)
        .map(|c| {
            let kv: HashMap<String, String> = KV.captures_iter(&c[2]).map(|m| (m[1].to_lowercase(), m[2].to_string())).collect();
            let get = |k: &str| kv.get(k).cloned().unwrap_or_default();
            let recent = get("mostrecent") == "1";
            Account {
                steamid: c[1].to_string(),
                name: get("accountname"),
                persona: get("personaname"),
                // "most recent" outranks any timestamp
                last_login: if recent { u64::MAX } else { get("timestamp").parse().unwrap_or(0) },
            }
        })
        .collect();
    out.sort_by(|a, b| b.last_login.cmp(&a.last_login).then_with(|| a.steamid.cmp(&b.steamid)));
    for a in &mut out {
        if a.last_login == u64::MAX {
            a.last_login = 0;
        }
    }
    out
}

/// Accounts of every Steam install found (de-duplicated by id).
pub fn accounts(roots: &[PathBuf]) -> Vec<Account> {
    let mut seen = std::collections::HashSet::new();
    let mut all = Vec::new();
    for r in roots {
        if let Ok(t) = std::fs::read_to_string(r.join("config/loginusers.vdf")) {
            for a in parse_accounts(&t) {
                if seen.insert(a.steamid.clone()) {
                    all.push(a);
                }
            }
        }
    }
    all
}

/// The account whose library to show: the chosen one when it still exists, else the newest login.
pub fn pick<'a>(accts: &'a [Account], chosen: &str) -> Option<&'a Account> {
    accts.iter().find(|a| !chosen.is_empty() && a.steamid == chosen).or_else(|| accts.first())
}

/// `LastPlayed` (Unix seconds) of every app in a `localconfig.vdf`, by appid.
pub fn last_played(vdf: &str) -> HashMap<String, u64> {
    static TOKEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""((?:[^"\\]|\\.)*)"|([{}])"#).unwrap());
    let mut out = HashMap::new();
    let mut path: Vec<String> = Vec::new();
    let mut pending: Option<String> = None;
    let mut prev_key: Option<String> = None;
    for c in TOKEN.captures_iter(vdf) {
        if let Some(m) = c.get(1) {
            let t = m.as_str().to_string();
            match prev_key.take() {
                Some(k) => {
                    // key + value
                    if k == "LastPlayed" && path.len() >= 2 && path[path.len() - 2] == "apps" {
                        if let Ok(ts) = t.parse::<u64>() {
                            out.insert(path[path.len() - 1].clone(), ts);
                        }
                    }
                }
                None => prev_key = Some(t),
            }
        } else if &c[2] == "{" {
            if let Some(k) = prev_key.take() {
                pending = Some(k);
            }
            path.push(pending.take().unwrap_or_default());
        } else {
            path.pop();
            prev_key = None;
        }
    }
    out
}

/// Does Steam Cloud keep copies of this game's files? Steam writes `userdata/<account>/<app>/remotecache.vdf`
/// once a game has synced something (an empty one is a few bytes). Only looks, never reads the content.
pub fn cloud_synced(roots: &[PathBuf], appid: &str) -> bool {
    if appid.is_empty() || !appid.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    roots.iter().any(|r| {
        std::fs::read_dir(r.join("userdata")).into_iter().flatten().flatten().any(|u| std::fs::metadata(u.path().join(appid).join("remotecache.vdf")).map(|m| m.len() > 200).unwrap_or(false))
    })
}

/// A value in a Valve text file: a string, or a block of named values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Vdf {
    Str(String),
    Map(Vec<(String, Vdf)>),
}

impl Vdf {
    pub fn get(&self, key: &str) -> Option<&Vdf> {
        match self {
            Vdf::Map(m) => m.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v),
            Vdf::Str(_) => None,
        }
    }
    pub fn str(&self) -> Option<&str> {
        match self {
            Vdf::Str(s) => Some(s),
            Vdf::Map(_) => None,
        }
    }
    pub fn entries(&self) -> &[(String, Vdf)] {
        match self {
            Vdf::Map(m) => m,
            Vdf::Str(_) => &[],
        }
    }
}

/// Valve's text format (`"key" "value"` and `"key" { ... }`) read into a tree. Damaged text gives what could be read,
/// and nesting is limited so a hostile file cannot take the stack.
pub fn parse_vdf(text: &str) -> Vdf {
    fn tokens(text: &str) -> Vec<Option<String>> {
        // Some(string) for a quoted or bare word, None for a brace; braces are told apart below by their own markers
        let mut out = Vec::new();
        let mut it = text.chars().peekable();
        while let Some(c) = it.next() {
            match c {
                '{' => out.push(Some("\u{1}{".into())),
                '}' => out.push(Some("\u{1}}".into())),
                '"' => {
                    let mut s = String::new();
                    while let Some(c) = it.next() {
                        match c {
                            '"' => break,
                            '\\' => match it.next() {
                                Some('n') => s.push('\n'),
                                Some('t') => s.push('\t'),
                                Some(o) => s.push(o),
                                None => break,
                            },
                            c => s.push(c),
                        }
                    }
                    out.push(Some(s));
                }
                '/' if it.peek() == Some(&'/') => {
                    for c in it.by_ref() {
                        if c == '\n' {
                            break;
                        }
                    }
                }
                c if c.is_whitespace() => {}
                _ => {}
            }
        }
        out
    }
    fn block(t: &[Option<String>], i: &mut usize, depth: usize) -> Vec<(String, Vdf)> {
        let mut out = Vec::new();
        while *i < t.len() {
            let Some(key) = t[*i].clone() else { break };
            if key == "\u{1}}" {
                *i += 1;
                return out;
            }
            *i += 1;
            if key == "\u{1}{" {
                continue;
            }
            match t.get(*i).and_then(|v| v.as_deref()) {
                Some("\u{1}{") => {
                    *i += 1;
                    let inner = if depth < 32 { block(t, i, depth + 1) } else { Vec::new() };
                    out.push((key, Vdf::Map(inner)));
                }
                Some(v) if v != "\u{1}}" => {
                    out.push((key, Vdf::Str(v.to_string())));
                    *i += 1;
                }
                _ => {}
            }
        }
        out
    }
    let t = tokens(text);
    let mut i = 0;
    Vdf::Map(block(&t, &mut i, 0))
}

/// What Steam is doing with one game's files right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Nothing; the game is as installed.
    Idle,
    /// Steam knows of an update and has not started it.
    Pending,
    /// The update was paused.
    Paused,
    /// Files are being downloaded, unpacked, checked or put in place.
    Updating,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UpdateState {
    pub phase: Phase,
    /// How far along the update is, when Steam says how many bytes it needs.
    pub percent: Option<u8>,
}

impl UpdateState {
    pub const IDLE: UpdateState = UpdateState { phase: Phase::Idle, percent: None };
}

/// A number stored in an `.acf` file (`"Key"  "123"`).
fn acf_number(text: &str, key: &str) -> Option<u64> {
    let pat = format!("\"{key}\"");
    text.lines().find_map(|l| {
        let l = l.trim();
        let rest = l.strip_prefix(&pat)?.trim();
        rest.trim_matches('"').parse::<u64>().ok()
    })
}

/// Steam's own record of a game (`appmanifest_<id>.acf`): `StateFlags` is a set of bits, 4 meaning installed.
/// Bits 2 (update required), 512 (paused) and 256, 1024, 65536 ... 8388608 (update, validation, reconfiguring,
/// preallocating, downloading, staging, committing, stopping) say what is going on.
pub fn update_state(acf: &str) -> UpdateState {
    const BUSY: u64 = 256 | 1024 | 65_536 | 131_072 | 262_144 | 524_288 | 1_048_576 | 2_097_152 | 4_194_304 | 8_388_608;
    let Some(flags) = acf_number(acf, "StateFlags") else { return UpdateState::IDLE };
    let total = acf_number(acf, "BytesToDownload").unwrap_or(0) + acf_number(acf, "BytesToStage").unwrap_or(0);
    let done = acf_number(acf, "BytesDownloaded").unwrap_or(0) + acf_number(acf, "BytesStaged").unwrap_or(0);
    let percent = (total > 0).then(|| (done.min(total) * 100 / total) as u8);
    let phase = if flags & 512 != 0 {
        Phase::Paused
    } else if flags & BUSY != 0 {
        Phase::Updating
    } else if flags & 2 != 0 {
        Phase::Pending
    } else {
        return UpdateState::IDLE;
    };
    UpdateState { phase, percent: percent.filter(|_| phase != Phase::Pending) }
}

/// The manifest of the game installed in `game_dir` (`<library>/common/<folder>`), if Steam has one.
pub fn manifest_for(game_dir: &std::path::Path, appid: &str) -> Option<PathBuf> {
    if appid.is_empty() || !appid.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let lib = game_dir.parent().filter(|p| p.file_name().map(|n| n == "common").unwrap_or(false))?.parent()?;
    let m = lib.join(format!("appmanifest_{appid}.acf"));
    m.is_file().then_some(m)
}

/// What Steam is doing with the game in `game_dir` (idle when it is not a Steam game or there is no record).
pub fn game_update(game_dir: &std::path::Path, appid: &str) -> UpdateState {
    manifest_for(game_dir, appid).and_then(|m| std::fs::read_to_string(m).ok()).map(|t| update_state(&t)).unwrap_or(UpdateState::IDLE)
}

/// Web API keys are 32 hexadecimal characters.
pub fn valid_key(k: &str) -> bool {
    k.len() == 32 && k.bytes().all(|b| b.is_ascii_hexdigit())
}

pub struct Client {
    base: String,
    key: String,
    allow_http: bool,
}

/// Names that are not games: runtimes and redistributables Steam lists as apps.
fn is_tool(name: &str) -> bool {
    let low = name.to_lowercase();
    low.starts_with("proton ")
        || low == "proton"
        || low.contains("steam linux runtime")
        || low.contains("steamworks common")
        || low.contains("redistributable")
}

pub fn parse_owned(body: &Value) -> Result<Vec<Owned>, String> {
    let resp = body.get("response").ok_or("Steam sent an unreadable reply.")?;
    let Some(games) = resp.get("games").and_then(Value::as_array) else {
        return Err("Steam returned no games. Check that the key was created by this Steam account.".into());
    };
    let mut out: Vec<Owned> = games
        .iter()
        .filter_map(|g| {
            let name = g.get("name").and_then(Value::as_str)?.trim().to_string();
            Some(Owned {
                appid: g.get("appid").and_then(Value::as_u64)?,
                minutes: g.get("playtime_forever").and_then(Value::as_u64).unwrap_or(0),
                last_played: g.get("rtime_last_played").and_then(Value::as_u64).unwrap_or(0),
                name,
            })
        })
        .filter(|o| !o.name.is_empty() && !is_tool(&o.name))
        .collect();
    out.sort_by_key(|o| o.name.to_lowercase());
    Ok(out)
}

impl Client {
    /// `allow_http` exists only so tests can talk to a local fake server.
    pub fn new(base: &str, key: &str, allow_http: bool) -> Result<Self, String> {
        if !valid_key(key) {
            return Err("That does not look like a Steam Web API key (32 letters and digits).".into());
        }
        Ok(Self { base: base.trim_end_matches('/').to_string(), key: key.to_string(), allow_http })
    }

    /// Owned games of `steamid` with names and playtime.
    pub fn owned_games(&self, steamid: &str) -> Result<Vec<Owned>, String> {
        if steamid.len() != 17 || !steamid.bytes().all(|b| b.is_ascii_digit()) {
            return Err("Unrecognised Steam account.".into());
        }
        let url = format!(
            "{}/IPlayerService/GetOwnedGames/v1/?key={}&steamid={}&include_appinfo=1&include_played_free_games=1&format=json",
            self.base, self.key, steamid
        );
        if !(url.starts_with("https://") || (self.allow_http && url.starts_with("http://"))) {
            return Err("Refusing a non-HTTPS Steam address.".into());
        }
        // the key is part of the URL, so the URL goes to curl on stdin, not on the command line
        let config = format!("url = \"{url}\"\n");
        let (out, err) = curl_ok(Command::new("curl").args(["-sS", "-L", "--max-time", "30", "-w", "\n%{http_code}", "-K", "-"]), Some(&config))?;
        let (body, code) = out.rsplit_once('\n').unwrap_or(("", "000"));
        let code: u16 = code.trim().parse().unwrap_or(0);
        match code {
            200..=299 => {}
            0 => return Err(format!("Could not reach Steam ({}).", err.trim().lines().last().unwrap_or("network error"))),
            401 | 403 => return Err("Steam rejected the Web API key. Check it, or create a new one.".into()),
            429 => return Err("Steam rate limit reached; try again in a while.".into()),
            c => return Err(format!("Steam error {c}.")),
        }
        let json: Value = serde_json::from_str(body).map_err(|_| "Steam sent an unreadable reply.".to_string())?;
        parse_owned(&json)
    }
}

#[cfg(test)]
pub mod fake {
    //! A stand-in for api.steampowered.com, for tests.
    use std::sync::{Arc, Mutex};
    use tiny_http::{Response, Server};

    pub const KEY: &str = "0123456789ABCDEF0123456789abcdef";
    pub const STEAMID: &str = "76561198000000001";

    pub struct FakeSteam {
        pub base: String,
        /// Body for `GetOwnedGames` (replaceable between calls).
        pub games_json: Arc<Mutex<String>>,
        pub requests: Arc<Mutex<Vec<String>>>,
    }

    pub fn start() -> FakeSteam {
        let server = Server::http("127.0.0.1:0").unwrap();
        let base = format!("http://{}", server.server_addr().to_ip().unwrap());
        let games_json = Arc::new(Mutex::new(
            r#"{"response":{"game_count":4,"games":[
                {"appid":1962700,"name":"Subnautica 2","playtime_forever":600,"rtime_last_played":1700000000},
                {"appid":620,"name":"Portal 2","playtime_forever":125},
                {"appid":1493710,"name":"Proton Experimental","playtime_forever":0},
                {"appid":730,"name":"Counter-Strike 2","playtime_forever":0}]}}"#
                .to_string(),
        ));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (g2, r2) = (games_json.clone(), requests.clone());
        std::thread::spawn(move || {
            for req in server.incoming_requests() {
                let url = req.url().to_string();
                r2.lock().unwrap().push(url.clone());
                let ok = url.contains(&format!("key={KEY}")) && url.contains(&format!("steamid={STEAMID}"));
                let resp = if !url.starts_with("/IPlayerService/GetOwnedGames/v1/") {
                    Response::from_string("not found").with_status_code(404)
                } else if !ok {
                    Response::from_string("<html>Unauthorized</html>").with_status_code(401)
                } else {
                    Response::from_string(g2.lock().unwrap().clone())
                };
                let _ = req.respond(resp);
            }
        });
        FakeSteam { base, games_json, requests }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::*;
    use super::*;

    const LOGINUSERS: &str = "\"users\"\n{\n\t\"76561198000000001\"\n\t{\n\t\t\"AccountName\"\t\t\"account_one\"\n\t\t\"PersonaName\"\t\t\"Alpha\"\n\t\t\"AutoLogin\"\t\t\"1\"\n\t\t\"timestamp\"\t\t\"1792480834\"\n\t}\n\t\"76561198000000002\"\n\t{\n\t\t\"AccountName\"\t\t\"account_two\"\n\t\t\"PersonaName\"\t\t\"Beta\"\n\t\t\"timestamp\"\t\t\"1792252518\"\n\t}\n}\n";

    #[test]
    fn accounts_are_read_newest_login_first_and_the_chosen_one_wins() {
        let a = parse_accounts(LOGINUSERS);
        assert_eq!(a.iter().map(|x| x.persona.as_str()).collect::<Vec<_>>(), vec!["Alpha", "Beta"]);
        assert_eq!((a[0].steamid.as_str(), a[0].name.as_str()), ("76561198000000001", "account_one"));
        assert_eq!(pick(&a, "").unwrap().persona, "Alpha");
        assert_eq!(pick(&a, "76561198000000002").unwrap().persona, "Beta");
        assert_eq!(pick(&a, "99999999999999999").unwrap().persona, "Alpha", "a vanished choice falls back to the newest");
        assert!(pick(&[], "x").is_none());
        // an explicit MostRecent flag beats timestamps
        let t = LOGINUSERS.replace("\"PersonaName\"\t\t\"Beta\"", "\"PersonaName\"\t\t\"Beta\"\n\t\t\"MostRecent\"\t\t\"1\"");
        assert_eq!(parse_accounts(&t)[0].persona, "Beta");
        assert!(parse_accounts("garbage").is_empty());
    }

    #[test]
    fn last_played_is_read_per_app_from_localconfig() {
        let vdf = "\"UserLocalConfigStore\"\n{\n\t\"Software\"\n\t{\n\t\t\"Valve\"\n\t\t{\n\t\t\t\"Steam\"\n\t\t\t{\n\t\t\t\t\"apps\"\n\t\t\t\t{\n\t\t\t\t\t\"620\"\n\t\t\t\t\t{\n\t\t\t\t\t\t\"cloud\"\n\t\t\t\t\t\t{\n\t\t\t\t\t\t\t\"LastPlayed\"\t\t\"5\"\n\t\t\t\t\t\t}\n\t\t\t\t\t\t\"LastPlayed\"\t\t\"1700000000\"\n\t\t\t\t\t}\n\t\t\t\t\t\"730\"\n\t\t\t\t\t{\n\t\t\t\t\t\t\"LaunchOptions\"\t\t\"-novid\"\n\t\t\t\t\t}\n\t\t\t\t\t\"1245620\"\n\t\t\t\t\t{\n\t\t\t\t\t\t\"LastPlayed\"\t\t\"1790000000\"\n\t\t\t\t\t}\n\t\t\t\t}\n\t\t\t}\n\t\t}\n\t}\n}\n";
        let m = last_played(vdf);
        assert_eq!(m.get("620"), Some(&1_700_000_000), "the cloud sub-block's own LastPlayed is not the game's");
        assert_eq!(m.get("1245620"), Some(&1_790_000_000));
        assert!(!m.contains_key("730"), "no LastPlayed, no entry");
        assert!(last_played("garbage {{{ \"x\"").is_empty());
    }

    #[test]
    fn steam_cloud_is_recognised_from_the_cache_steam_keeps_per_game() {
        let d = crate::fsutil::testutil::TempDir::new("cloud");
        let root = d.join("Steam");
        let cache = root.join("userdata/123456/620/remotecache.vdf");
        std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
        std::fs::write(&cache, format!("\"620\"\n{{\n\t\"ChangeNumber\"\t\t\"1\"\n{}}}\n", "\t\"saves/slot.sav\"\n\t{\n\t\t\"root\"\t\t\"0\"\n\t\t\"size\"\t\t\"9\"\n\t\t\"localtime\"\t\t\"1\"\n\t\t\"time\"\t\t\"1\"\n\t\t\"remotetime\"\t\t\"1\"\n\t\t\"sha\"\t\t\"0123456789abcdef0123456789abcdef01234567\"\n\t\t\"syncstate\"\t\t\"1\"\n\t\t\"persiststate\"\t\t\"0\"\n\t}\n")).unwrap();
        std::fs::create_dir_all(root.join("userdata/123456/730")).unwrap();
        std::fs::write(root.join("userdata/123456/730/remotecache.vdf"), "\"730\"\n{\n}\n").unwrap();
        let roots = [d.join("nothing"), root];
        assert!(cloud_synced(&roots, "620"));
        assert!(!cloud_synced(&roots, "730"), "an empty cache means nothing is synced");
        assert!(!cloud_synced(&roots, "999") && !cloud_synced(&roots, "") && !cloud_synced(&roots, "../620"));
    }

    #[test]
    fn valves_text_format_is_read_into_a_tree_and_damage_never_panics() {
        let v = parse_vdf("\"AppWorkshop\"\n{\n\t\"appid\"\t\t\"241100\"\n\t// a comment\n\t\"Items\"\n\t{\n\t\t\"42\"\n\t\t{\n\t\t\t\"size\"\t\t\"9\"\n\t\t}\n\t}\n}\n");
        let w = v.get("AppWorkshop").unwrap();
        assert_eq!(w.get("appid").and_then(Vdf::str), Some("241100"));
        assert_eq!(w.get("items").unwrap().get("42").unwrap().get("size").and_then(Vdf::str), Some("9"), "keys ignore case");
        assert_eq!(w.get("Items").unwrap().entries().len(), 1);
        for bad in ["", "{", "}}}", "\"a\" {", "\"a\"", "\"a", "\"a\" \"b", "\u{0}\u{1}{{{", &"{".repeat(5000), &"\"k\" {".repeat(5000)] {
            let _ = parse_vdf(bad);
        }
        // a half-written file still gives what came before the damage
        let half = parse_vdf("\"A\" { \"x\" \"1\" \"B\" { \"y\" \"2\"");
        assert_eq!(half.get("A").and_then(|a| a.get("x")).and_then(Vdf::str), Some("1"));
    }

    #[test]
    fn steams_record_of_a_game_says_when_it_is_being_updated() {
        let acf = |flags: u64, extra: &str| format!("\"AppState\"\n{{\n\t\"appid\"\t\t\"620\"\n\t\"StateFlags\"\t\t\"{flags}\"\n{extra}}}\n");
        assert_eq!(update_state(&acf(4, "")), UpdateState::IDLE, "installed and quiet");
        assert_eq!(update_state("garbage"), UpdateState::IDLE);
        assert_eq!(update_state(&acf(6, "")).phase, Phase::Pending, "an update is waiting");
        // downloading: installed bit stays set, the downloading bit joins it
        let dl = "\t\"BytesToDownload\"\t\t\"1000\"\n\t\"BytesDownloaded\"\t\t\"250\"\n\t\"BytesToStage\"\t\t\"1000\"\n\t\"BytesStaged\"\t\t\"250\"\n";
        let u = update_state(&acf(4 | 1_048_576 | 256, dl));
        assert_eq!((u.phase, u.percent), (Phase::Updating, Some(25)));
        assert_eq!(update_state(&acf(4 | 2_097_152, "")).phase, Phase::Updating, "staging counts");
        assert_eq!(update_state(&acf(4 | 131_072, "")).phase, Phase::Updating, "so does Steam verifying the files");
        assert_eq!(update_state(&acf(4 | 256 | 512, dl)).phase, Phase::Paused);
        // nonsense byte counts never give more than 100
        assert_eq!(update_state(&acf(4 | 256, "\t\"BytesToDownload\"\t\t\"10\"\n\t\"BytesDownloaded\"\t\t\"99\"\n")).percent, Some(100));
    }

    #[test]
    fn the_manifest_is_found_next_to_the_common_folder_and_nowhere_else() {
        let d = crate::fsutil::testutil::TempDir::new("manifest");
        let game = d.join("steamapps/common/Cool Game");
        std::fs::create_dir_all(&game).unwrap();
        assert_eq!(manifest_for(&game, "620"), None, "no manifest, no answer");
        std::fs::write(d.join("steamapps/appmanifest_620.acf"), "\"AppState\"\n{\n\"StateFlags\" \"4\"\n}\n").unwrap();
        assert_eq!(manifest_for(&game, "620"), Some(d.join("steamapps/appmanifest_620.acf")));
        assert_eq!(manifest_for(&game, "../620"), None);
        assert_eq!(manifest_for(&d.join("elsewhere/Cool Game"), "620"), None, "only games that live in a Steam library");
        std::fs::write(d.join("steamapps/appmanifest_620.acf"), "\"AppState\"\n{\n\"StateFlags\" \"1048580\"\n}\n").unwrap();
        assert_eq!(game_update(&game, "620").phase, Phase::Updating);
        assert_eq!(game_update(&game, "").phase, Phase::Idle);
    }

    #[test]
    fn key_shape_and_account_ids_are_checked_before_any_request() {
        assert!(valid_key(KEY));
        for bad in ["", "short", "0123456789ABCDEF0123456789abcdeg", "0123456789ABCDEF0123456789abcdef0", "01234567 9ABCDEF0123456789abcdef"] {
            assert!(!valid_key(bad), "{bad:?}");
        }
        let c = Client::new("http://127.0.0.1:1", KEY, true).unwrap();
        assert!(c.owned_games("123").is_err() && c.owned_games("7656119958841407x").is_err());
        assert!(Client::new("http://x", "nope", true).is_err());
        assert!(Client::new("http://api.example", KEY, false).unwrap().owned_games(STEAMID).unwrap_err().contains("HTTPS"));
    }

    #[test]
    fn owned_games_come_back_sorted_without_runtimes_and_with_playtime() {
        let f = start();
        let c = Client::new(&f.base, KEY, true).unwrap();
        let g = c.owned_games(STEAMID).unwrap();
        assert_eq!(g.iter().map(|o| o.name.as_str()).collect::<Vec<_>>(), vec!["Counter-Strike 2", "Portal 2", "Subnautica 2"]);
        let s2 = g.iter().find(|o| o.appid == 1962700).unwrap();
        assert_eq!((s2.minutes, s2.last_played), (600, 1_700_000_000));
        assert_eq!(f.requests.lock().unwrap().len(), 1);
        // wrong key / dead server / empty answer are explained, never panics
        let bad = Client::new(&f.base, "ffffffffffffffffffffffffffffffff", true).unwrap();
        assert!(bad.owned_games(STEAMID).unwrap_err().contains("rejected the Web API key"));
        assert!(Client::new("http://127.0.0.1:1", KEY, true).unwrap().owned_games(STEAMID).unwrap_err().contains("Could not reach"));
        *f.games_json.lock().unwrap() = r#"{"response":{}}"#.into();
        assert!(c.owned_games(STEAMID).unwrap_err().contains("no games"));
        *f.games_json.lock().unwrap() = "not json".into();
        assert!(c.owned_games(STEAMID).unwrap_err().contains("unreadable"));
    }
}
