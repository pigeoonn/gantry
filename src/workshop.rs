//! Steam Workshop items a game has installed. Steam downloads and updates them itself; Gantry only lists what is
//! there (from `steamapps/workshop/appworkshop_<app>.acf` and the content folder) and looks up each item's name and
//! picture through Steam's public item lookup. Nothing is changed, and nothing here needs a key.

use crate::steamlib::{parse_vdf, Vdf};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Sites Steam serves Workshop pictures from; a picture from anywhere else is not shown.
const PREVIEW_HOSTS: &[&str] = &["images.steamusercontent.com", "steamuserimages-a.akamaihd.net", "cdn.akamai.steamstatic.com", "shared.fastly.steamstatic.com"];

/// Items looked up in one request (Steam accepts many; this keeps the request small).
const BATCH: usize = 50;

/// One installed item, as Steam recorded it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    pub id: String,
    pub size: u64,
    /// When the installed version was published (Unix seconds).
    pub updated: u64,
}

/// What the Workshop says about an item.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Detail {
    pub title: String,
    pub preview: String,
    pub subscribers: u64,
    /// The `updated` of the installed copy this was looked up for; a different one means look again.
    pub for_updated: u64,
    /// The Workshop no longer has the item (removed by its author or by Valve).
    pub removed: bool,
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit())
}

/// `<library>/workshop` for a game installed in `<library>/common/<folder>`.
pub fn workshop_dir(game_dir: &Path) -> Option<PathBuf> {
    let lib = game_dir.parent().filter(|p| p.file_name().map(|n| n == "common").unwrap_or(false))?.parent()?;
    Some(lib.join("workshop"))
}

/// The items listed in an `appworkshop_<app>.acf`.
pub fn parse_installed(acf: &str) -> Vec<Installed> {
    let root = parse_vdf(acf);
    let Some(items) = root.get("AppWorkshop").and_then(|a| a.get("WorkshopItemsInstalled")) else { return Vec::new() };
    let num = |v: &Vdf, k: &str| v.get(k).and_then(Vdf::str).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
    items.entries().iter().filter(|(id, v)| digits(id) && matches!(v, Vdf::Map(_))).map(|(id, v)| Installed { id: id.clone(), size: num(v, "size"), updated: num(v, "timeupdated") }).collect()
}

/// The Workshop items of `appid` that are installed for the game in `game_dir` (their folder must exist), newest first.
pub fn installed(game_dir: &Path, appid: &str) -> Vec<Installed> {
    if !digits(appid) {
        return Vec::new();
    }
    let Some(dir) = workshop_dir(game_dir) else { return Vec::new() };
    let Ok(acf) = std::fs::read_to_string(dir.join(format!("appworkshop_{appid}.acf"))) else { return Vec::new() };
    let mut items: Vec<Installed> = parse_installed(&acf).into_iter().filter(|i| dir.join("content").join(appid).join(&i.id).is_dir()).collect();
    items.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| a.id.cmp(&b.id)));
    items
}

/// The folder of one installed item.
pub fn item_folder(game_dir: &Path, appid: &str, id: &str) -> Option<PathBuf> {
    if !digits(appid) || !digits(id) {
        return None;
    }
    let p = workshop_dir(game_dir)?.join("content").join(appid).join(id);
    p.is_dir().then_some(p)
}

/// Details out of the answer to `GetPublishedFileDetails`, by item id. An item the Workshop does not know any more is
/// marked removed.
pub fn parse_details(body: &Value) -> HashMap<String, Detail> {
    let mut out = HashMap::new();
    for d in body["response"]["publishedfiledetails"].as_array().into_iter().flatten() {
        let Some(id) = d.get("publishedfileid").and_then(Value::as_str).filter(|s| digits(s)) else { continue };
        let ok = d.get("result").and_then(Value::as_u64) == Some(1) && d.get("banned").and_then(Value::as_u64).unwrap_or(0) == 0;
        let text = |k: &str, max: usize| d.get(k).and_then(Value::as_str).unwrap_or("").chars().filter(|c| !c.is_control()).take(max).collect::<String>();
        let preview = text("preview_url", 400);
        out.insert(
            id.to_string(),
            Detail {
                title: text("title", 160),
                // only a picture from Steam's own hosts is ever shown
                preview: if preview.strip_prefix("https://").and_then(|r| r.split('/').next()).map(|h| PREVIEW_HOSTS.contains(&h)).unwrap_or(false) { preview } else { String::new() },
                subscribers: d.get("subscriptions").and_then(Value::as_u64).unwrap_or(0),
                for_updated: 0,
                removed: !ok,
            },
        );
    }
    out
}

/// Ask Steam about these items (a public question; no key). `allow_http` exists for tests with a local fake.
pub fn lookup(ids: &[String], base: &str, allow_http: bool) -> Result<HashMap<String, Detail>, String> {
    if !(base.starts_with("https://") || (allow_http && base.starts_with("http://"))) {
        return Err("Refusing a non-HTTPS Steam address.".into());
    }
    let mut all = HashMap::new();
    for chunk in ids.chunks(BATCH) {
        let mut cmd = Command::new("curl");
        cmd.args(["-sS", "-L", "--max-time", "30"]).arg("-d").arg(format!("itemcount={}", chunk.len()));
        for (n, id) in chunk.iter().enumerate().filter(|(_, id)| digits(id)) {
            cmd.arg("-d").arg(format!("publishedfileids[{n}]={id}"));
        }
        let url = format!("{}/ISteamRemoteStorage/GetPublishedFileDetails/v1/", base.trim_end_matches('/'));
        let out = cmd.arg(url).stdin(Stdio::null()).output().map_err(|e| format!("Could not run curl ({e})."))?;
        let body: Value = serde_json::from_slice(&out.stdout).map_err(|_| "Could not reach Steam.".to_string())?;
        all.extend(parse_details(&body));
    }
    Ok(all)
}

fn read_cache(path: &Path) -> HashMap<String, Detail> {
    std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// The listing for the screen: every installed item with what is known about it. Names come from the local cache; the
/// ones missing (or of an older version) are looked up and cached. A failed lookup still gives the list, with ids
/// only, and says why.
pub fn listing(game_dir: &Path, appid: &str, cache: &Path, base: &str, allow_http: bool) -> Value {
    let items = installed(game_dir, appid);
    let mut known = read_cache(cache);
    let stale: Vec<String> = items.iter().filter(|i| known.get(&i.id).map(|d| d.for_updated != i.updated).unwrap_or(true)).map(|i| i.id.clone()).collect();
    let mut error = Value::Null;
    if !stale.is_empty() {
        match lookup(&stale, base, allow_http) {
            Ok(got) => {
                for (id, mut d) in got {
                    d.for_updated = items.iter().find(|i| i.id == id).map(|i| i.updated).unwrap_or(0);
                    known.insert(id, d);
                }
                if let Ok(t) = serde_json::to_string(&known) {
                    let _ = crate::fsutil::atomic_write(cache, t);
                }
            }
            Err(e) => error = json!(e),
        }
    }
    let rows: Vec<Value> = items
        .iter()
        .map(|i| {
            let d = known.get(&i.id).cloned().unwrap_or_default();
            json!({
                "id": i.id, "size": i.size, "updated": i.updated, "title": d.title, "preview": d.preview,
                "subscribers": d.subscribers, "removed": d.removed,
                "url": format!("https://steamcommunity.com/sharedfiles/filedetails/?id={}", i.id),
            })
        })
        .collect();
    json!({ "appid": appid, "items": rows, "error": error })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsutil::testutil::TempDir;
    use std::sync::{Arc, Mutex};

    /// The shape Steam writes (taken from a real file; ids and numbers changed).
    const ACF: &str = "\"AppWorkshop\"\n{\n\t\"appid\"\t\t\"294100\"\n\t\"SizeOnDisk\"\t\t\"297466\"\n\t\"NeedsUpdate\"\t\t\"0\"\n\t\"WorkshopItemsInstalled\"\n\t{\n\t\t\"111\"\n\t\t{\n\t\t\t\"size\"\t\t\"11044\"\n\t\t\t\"timeupdated\"\t\t\"1484311267\"\n\t\t\t\"manifest\"\t\t\"-1\"\n\t\t}\n\t\t\"222\"\n\t\t{\n\t\t\t\"size\"\t\t\"500\"\n\t\t\t\"timeupdated\"\t\t\"1706909235\"\n\t\t}\n\t\t\"333\"\n\t\t{\n\t\t\t\"size\"\t\t\"5\"\n\t\t\t\"timeupdated\"\t\t\"1\"\n\t\t}\n\t}\n\t\"WorkshopItemDetails\"\n\t{\n\t\t\"111\"\n\t\t{\n\t\t\t\"manifest\"\t\t\"-1\"\n\t\t}\n\t}\n}\n";

    fn library(d: &TempDir) -> PathBuf {
        let game = d.join("steamapps/common/Rim");
        std::fs::create_dir_all(&game).unwrap();
        let ws = d.join("steamapps/workshop");
        std::fs::create_dir_all(ws.join("content/294100/111")).unwrap();
        std::fs::create_dir_all(ws.join("content/294100/222")).unwrap(); // 333 is listed but gone from disk
        std::fs::write(ws.join("appworkshop_294100.acf"), ACF).unwrap();
        game
    }

    #[test]
    fn installed_items_are_read_from_steams_record_and_must_exist_on_disk() {
        let d = TempDir::new("ws_installed");
        let game = library(&d);
        let ids: Vec<(String, u64)> = installed(&game, "294100").into_iter().map(|i| (i.id, i.size)).collect();
        assert_eq!(ids, vec![("222".to_string(), 500), ("111".to_string(), 11044)], "newest first; the one without a folder is not shown");
        assert!(installed(&game, "999").is_empty() && installed(&game, "../294100").is_empty() && installed(&d.join("elsewhere"), "294100").is_empty());
        assert_eq!(item_folder(&game, "294100", "222"), Some(d.join("steamapps/workshop/content/294100/222")));
        assert_eq!(item_folder(&game, "294100", "../222"), None);
        assert_eq!(item_folder(&game, "294100", "333"), None);
        assert!(parse_installed("garbage {{{").is_empty());
    }

    fn fake() -> (String, Arc<Mutex<Vec<String>>>) {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let base = format!("http://{}", server.server_addr().to_ip().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        std::thread::spawn(move || {
            for mut req in server.incoming_requests() {
                let mut body = String::new();
                let _ = std::io::Read::read_to_string(req.as_reader(), &mut body);
                s2.lock().unwrap().push(format!("{} {}", req.url(), body));
                let reply = r#"{"response":{"result":1,"resultcount":2,"publishedfiledetails":[
                    {"publishedfileid":"111","result":1,"title":"Cool Mod","preview_url":"https://images.steamusercontent.com/ugc/1/cool.jpg","subscriptions":1234,"banned":0},
                    {"publishedfileid":"222","result":9},
                    {"publishedfileid":"x","result":1,"title":"junk"}]}}"#;
                let _ = req.respond(tiny_http::Response::from_string(reply));
            }
        });
        (base, seen)
    }

    #[test]
    fn names_are_looked_up_once_cached_and_looked_up_again_only_for_a_new_version() {
        if crate::fsutil::find_in_path("curl").is_none() {
            return;
        }
        let d = TempDir::new("ws_listing");
        let game = library(&d);
        let (base, seen) = fake();
        let cache = d.join("cache/ws.json");
        let v = listing(&game, "294100", &cache, &base, true);
        let items = v["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        let cool = items.iter().find(|i| i["id"] == "111").unwrap();
        assert_eq!((cool["title"].as_str(), cool["subscribers"].as_u64(), cool["removed"].as_bool()), (Some("Cool Mod"), Some(1234), Some(false)));
        assert_eq!(cool["preview"], "https://images.steamusercontent.com/ugc/1/cool.jpg");
        assert_eq!(cool["url"], "https://steamcommunity.com/sharedfiles/filedetails/?id=111");
        assert_eq!(items.iter().find(|i| i["id"] == "222").unwrap()["removed"], true, "the Workshop no longer has it");
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert!(seen.lock().unwrap()[0].contains("publishedfileids[0]=") && seen.lock().unwrap()[0].contains("itemcount=2"));
        // a second look uses the cache
        let again = listing(&game, "294100", &cache, &base, true);
        assert_eq!(again["items"], v["items"]);
        assert_eq!(seen.lock().unwrap().len(), 1, "nothing asked twice");
        // Steam updates item 111: only that is looked up again
        std::fs::write(d.join("steamapps/workshop/appworkshop_294100.acf"), ACF.replace("1484311267", "1790000000")).unwrap();
        listing(&game, "294100", &cache, &base, true);
        let reqs = seen.lock().unwrap().clone();
        assert_eq!(reqs.len(), 2);
        assert!(reqs[1].contains("itemcount=1") && reqs[1].contains("=111"), "{}", reqs[1]);
    }

    #[test]
    fn without_a_connection_the_list_still_appears_with_the_reason() {
        let d = TempDir::new("ws_offline");
        let game = library(&d);
        let v = listing(&game, "294100", &d.join("ws.json"), "http://127.0.0.1:1", true);
        assert_eq!(v["items"].as_array().unwrap().len(), 2);
        assert!(v["error"].is_string());
        assert_eq!(v["items"][0]["title"], "");
        // a plain http address is never used for real
        assert!(lookup(&["1".into()], "http://api.example", false).unwrap_err().contains("HTTPS"));
    }

    #[test]
    fn replies_with_odd_content_never_become_unsafe_pictures_or_ids() {
        let body = json!({"response":{"publishedfiledetails":[
            {"publishedfileid":"5","result":1,"title":"A\u{7}B","preview_url":"javascript:alert(1)"},
            {"publishedfileid":"6","result":1,"title":"banned","banned":1,"preview_url":"https://evil.example/x.png"},
            {"publishedfileid":"../7","result":1,"title":"x"},
            {"result":1}]}});
        let d = parse_details(&body);
        assert_eq!(d.len(), 2);
        assert_eq!((d["5"].title.as_str(), d["5"].preview.as_str(), d["5"].removed), ("AB", "", false));
        assert!(d["6"].removed && d["6"].preview.is_empty(), "a picture from a foreign host is dropped");
        assert!(parse_details(&json!({})).is_empty());
    }
}
