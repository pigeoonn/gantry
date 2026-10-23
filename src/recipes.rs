//! What is known about particular games: which mod loader the community uses and which
//! Thunderstore community they belong to. A small list ships with Gantry, and entries in
//! `recipes.json` next to Gantry's settings add to it or replace one (same game id).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{LazyLock, RwLock};

const BUILT_IN: &str = include_str!("../data/recipes.json");

/// "Files with these extensions go into this folder of the game."
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct FolderRule {
    pub ext: Vec<String>,
    pub folder: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Recipe {
    pub appid: String,
    #[serde(default)]
    pub name: String,
    /// "bepinex5", "bepinex6" or "melonloader": the loader its mods are made for.
    #[serde(default)]
    pub loader: String,
    /// Identifier of the game's Thunderstore community.
    #[serde(default)]
    pub thunderstore: String,
    /// The game's Nexus Mods domain (the part after nexusmods.com/).
    #[serde(default)]
    pub nexus: String,
    /// Where loose files of a kind go when nothing else says (a mod without folders, no readme hint).
    #[serde(default)]
    pub folders: Vec<FolderRule>,
    /// A tip shown with the game's health.
    #[serde(default)]
    pub notes: String,
    /// Where the game keeps its saves (`{documents}/My Game` and the like), for per-profile saves.
    #[serde(default)]
    pub saves: Vec<String>,
    /// A short list of mods worth starting with (the store's *Starter pack*).
    #[serde(default)]
    pub starter: Vec<StarterPick>,
}

/// One mod of a starter pack.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StarterPick {
    /// "nexus" (the mod's number) or "thunderstore" (`Owner-Name`)
    pub source: String,
    pub id: String,
    /// One line on why it is on the list.
    #[serde(default)]
    pub why: String,
}

fn plain_word(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A folder inside the game: no `..`, no drive, no leading slash.
fn folder_ok(f: &str) -> bool {
    let f = f.trim();
    !f.is_empty() && f.len() <= 120 && !f.starts_with('/') && !f.contains('\\') && !f.contains(':') && f.split('/').all(|p| !p.is_empty() && p != "." && p != "..")
}

/// One recipe checked and tidied, or nothing when it cannot be trusted.
pub fn validated(mut r: Recipe) -> Option<Recipe> {
    let ok = !r.appid.is_empty()
        && r.appid.len() < 40
        && r.appid.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
        && ["", "bepinex5", "bepinex6", "melonloader"].contains(&r.loader.as_str())
        && (r.thunderstore.is_empty() || plain_word(&r.thunderstore))
        && (r.nexus.is_empty() || plain_word(&r.nexus));
    if !ok {
        return None;
    }
    r.name = r.name.chars().filter(|c| !c.is_control()).take(80).collect();
    r.notes = r.notes.chars().filter(|c| !c.is_control() || *c == '\n').take(1000).collect();
    for f in &mut r.folders {
        f.folder = f.folder.trim().trim_end_matches('/').to_string();
        f.ext = f.ext.iter().map(|e| e.to_lowercase()).collect();
    }
    r.saves.retain(|s| s.len() <= 200 && s.starts_with('{') && !s.contains("..") && ["{appdata_local}", "{appdata_locallow}", "{appdata_roaming}", "{documents}", "{saved_games}", "{game}", "{home}"].iter().any(|p| s.starts_with(p)));
    r.saves.truncate(8);
    r.starter.retain(|p| (p.source == "nexus" && !p.id.is_empty() && p.id.len() <= 12 && p.id.bytes().all(|b| b.is_ascii_digit())) || (p.source == "thunderstore" && p.id.len() <= 100 && plain_word(&p.id)));
    for p in &mut r.starter {
        p.why = p.why.chars().filter(|c| !c.is_control()).take(200).collect();
    }
    r.starter.truncate(12);
    r.folders.retain(|f| folder_ok(&f.folder) && !f.ext.is_empty() && f.ext.iter().all(|e| plain_word(e)));
    r.folders.truncate(20);
    Some(r)
}

static RULES: LazyLock<RwLock<HashMap<String, Vec<FolderRule>>>> = LazyLock::new(|| RwLock::new(HashMap::new()));

/// Make the folder rules of these recipes known to the mod library.
pub fn publish(all: &[Recipe]) {
    {
        let mut s = SAVES.write().unwrap_or_else(|e| e.into_inner());
        s.clear();
        for r in all.iter().filter(|r| !r.saves.is_empty()) {
            s.insert(r.appid.clone(), r.saves.clone());
        }
    }
    let mut m = RULES.write().unwrap_or_else(|e| e.into_inner());
    m.clear();
    for r in all.iter().filter(|r| !r.folders.is_empty()) {
        m.insert(r.appid.clone(), r.folders.clone());
    }
}

/// The save folders the support entry of a game names.
pub fn rules_saves(appid: &str, steam_appid: &str) -> Vec<String> {
    let m = SAVES.read().unwrap_or_else(|e| e.into_inner());
    m.get(appid).or_else(|| (!steam_appid.is_empty()).then(|| m.get(steam_appid)).flatten()).cloned().unwrap_or_default()
}

static SAVES: LazyLock<RwLock<HashMap<String, Vec<String>>>> = LazyLock::new(|| RwLock::new(HashMap::new()));

/// The folder rules of a game (by its id or its Steam id).
pub fn rules_for(appid: &str, steam_appid: &str) -> Vec<FolderRule> {
    let m = RULES.read().unwrap_or_else(|e| e.into_inner());
    m.get(appid).or_else(|| (!steam_appid.is_empty()).then(|| m.get(steam_appid)).flatten()).cloned().unwrap_or_default()
}

/// Add or replace a recipe in the user's file (other entries stay as they are).
pub fn save_user(user_file: &Path, r: Recipe) -> Result<(), String> {
    let r = validated(r).ok_or("That is not a valid game entry.")?;
    let mut doc: serde_json::Value = std::fs::read_to_string(user_file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_else(|| serde_json::json!({ "games": [] }));
    let games = doc.get_mut("games").and_then(|g| g.as_array_mut());
    let entry = serde_json::to_value(&r).map_err(|e| e.to_string())?;
    match games {
        Some(list) => {
            list.retain(|g| g.get("appid").and_then(|a| a.as_str()) != Some(r.appid.as_str()));
            list.push(entry);
        }
        None => doc = serde_json::json!({ "games": [entry] }),
    }
    crate::fsutil::atomic_write(user_file, serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

#[derive(Deserialize)]
struct File {
    #[serde(default)]
    games: Vec<Recipe>,
}

fn parse(text: &str) -> Vec<Recipe> {
    serde_json::from_str::<File>(text)
        .map(|f| f.games)
        .unwrap_or_default()
        .into_iter()
        .filter_map(validated)
        .collect()
}

/// The built-in recipes with the user's own file applied over them.
pub fn load(user_file: &Path) -> Vec<Recipe> {
    let mut all = parse(BUILT_IN);
    for r in std::fs::read_to_string(user_file).map(|t| parse(&t)).unwrap_or_default() {
        all.retain(|x| x.appid != r.appid);
        all.push(r);
    }
    all
}

pub fn find<'a>(all: &'a [Recipe], appid: &str, steam_appid: &str) -> Option<&'a Recipe> {
    all.iter().find(|r| r.appid == appid || (!steam_appid.is_empty() && r.appid == steam_appid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsutil::testutil::TempDir;

    #[test]
    fn the_built_in_list_is_well_formed() {
        let all = parse(BUILT_IN);
        assert!(all.len() >= 10);
        let mut ids: Vec<&String> = all.iter().map(|r| &r.appid).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), all.len(), "one entry per game");
        assert!(all.iter().all(|r| !r.thunderstore.is_empty() || !r.nexus.is_empty() || !r.folders.is_empty() || !r.notes.is_empty()), "every entry says something useful");
        assert!(all.iter().all(|r| r.starter.iter().all(|p| !p.why.is_empty())), "every starter mod says why");
        assert!(all.iter().filter(|r| !r.loader.is_empty()).all(|r| !r.thunderstore.is_empty()));
        assert_eq!(find(&all, "3164500", "").unwrap().loader, "melonloader");
        assert_eq!(find(&all, "632360", "").unwrap().thunderstore, "riskofrain2");
        assert!(find(&all, "1", "").is_none());
        let nfs = find(&all, "1262560", "").unwrap();
        assert_eq!((nfs.nexus.as_str(), nfs.folders[0].folder.as_str()), ("needforspeedmostwanted2012", "VEHICLES"), "the car folder is written down as data now");
    }

    #[test]
    fn built_in_entries_are_consistent_and_their_save_places_survive_checking() {
        let all = parse(BUILT_IN);
        // two entries with the same Nexus game or Thunderstore community are a copy-and-paste slip (one was once filed under the wrong game)
        for key in [|r: &Recipe| r.nexus.clone(), |r: &Recipe| r.thunderstore.clone()] {
            let mut seen = std::collections::HashSet::new();
            for r in all.iter().map(key).filter(|k| !k.is_empty()) {
                assert!(seen.insert(r.clone()), "{r} is used by two entries");
            }
        }
        let b = crate::profsaves::bases(&crate::repair::fixtures::make_game("recipes_saves").1);
        for r in &all {
            let raw = BUILT_IN.matches(&format!("\"appid\": \"{}\"", r.appid)).count();
            assert_eq!(raw, 1, "{} appears once", r.appid);
            for s in &r.saves {
                assert!(crate::profsaves::resolve(s, &b).is_some(), "{} ({}): '{s}' does not resolve", r.name, r.appid);
            }
            // checking must not throw away anything the data says
            let v = validated(r.clone()).unwrap_or_else(|| panic!("{} is refused by validation", r.name));
            assert_eq!(v.saves, r.saves, "{}: a save place was dropped by validation", r.name);
        }
        let cp = find(&all, "1091500", "").unwrap();
        assert_eq!((cp.nexus.as_str(), cp.saves.as_slice()), ("cyberpunk2077", &["{saved_games}/CD Projekt Red/Cyberpunk 2077".to_string()][..]));
        assert_eq!(find(&all, "1030840", "").unwrap().nexus, "mafiadefinitiveedition", "the first Mafia is not Mafia III");
        assert!(all.iter().filter(|r| !r.saves.is_empty()).count() >= 30);
    }

    #[test]
    fn the_users_file_adds_games_replaces_entries_and_cannot_break_anything() {
        let d = TempDir::new("recipes");
        let f = d.join("recipes.json");
        std::fs::write(&f, r#"{"games":[{"appid":"3164500","loader":"bepinex6"},{"appid":"42","name":"Mine","loader":"melonloader","thunderstore":"mine"},{"appid":"7","loader":"evil"},{"appid":"8","thunderstore":"../x"}]}"#).unwrap();
        let all = load(&f);
        assert_eq!(find(&all, "3164500", "").unwrap().loader, "bepinex6", "the user's entry wins");
        assert_eq!(find(&all, "42", "").unwrap().thunderstore, "mine");
        assert!(find(&all, "7", "").is_none() && find(&all, "8", "").is_none(), "invalid entries are ignored");
        std::fs::write(&f, "not json").unwrap();
        assert_eq!(load(&f).len(), parse(BUILT_IN).len(), "a broken file changes nothing");
        assert_eq!(load(&d.join("missing.json")).len(), parse(BUILT_IN).len());
    }

    #[test]
    fn folder_rules_and_names_are_checked_and_the_users_entry_is_saved_next_to_the_others() {
        let r = |json: &str| validated(serde_json::from_str::<Recipe>(json).unwrap());
        let ok = r(r#"{"appid":"hydra_steam_1","nexus":"cool-game","folders":[{"ext":["BNDL","Bin"],"folder":"VEHICLES/"},{"ext":["x"],"folder":"../up"},{"ext":["y"],"folder":"/abs"},{"ext":["z"],"folder":"a\\b"},{"ext":["w"],"folder":"C:/x"},{"ext":["bad ext"],"folder":"ok"},{"ext":[],"folder":"ok"}],"notes":"Close\u0007 the launcher.\nThanks"}"#).unwrap();
        assert_eq!(ok.folders, vec![FolderRule { ext: vec!["bndl".into(), "bin".into()], folder: "VEHICLES".into() }]);
        assert_eq!(ok.notes, "Close the launcher.\nThanks", "control characters go, line breaks stay");
        for bad in [r#"{"appid":""}"#, r#"{"appid":"a b"}"#, r#"{"appid":"1","nexus":"../x"}"#, r#"{"appid":"1","thunderstore":"a b"}"#, r#"{"appid":"1","loader":"evil"}"#] {
            assert!(r(bad).is_none(), "{bad}");
        }
        let d = TempDir::new("recipes_save");
        let f = d.join("recipes.json");
        save_user(&f, Recipe { appid: "10".into(), name: "Ten".into(), nexus: "ten".into(), ..Default::default() }).unwrap();
        save_user(&f, Recipe { appid: "11".into(), ..Default::default() }).unwrap();
        save_user(&f, Recipe { appid: "10".into(), name: "Ten again".into(), notes: "hi".into(), ..Default::default() }).unwrap();
        let all = load(&f);
        assert_eq!(find(&all, "10", "").unwrap().name, "Ten again");
        assert!(find(&all, "11", "").is_some(), "the other entry stays");
        assert_eq!(all.iter().filter(|x| x.appid == "10").count(), 1);
        assert!(save_user(&f, Recipe { appid: "../x".into(), ..Default::default() }).is_err());
        // published rules are found by id or by Steam id
        publish(&[Recipe { appid: "777".into(), folders: ok.folders.clone(), ..Default::default() }]);
        assert_eq!(rules_for("777", "").len(), 1);
        assert_eq!(rules_for("other", "777").len(), 1);
        assert!(rules_for("other", "").is_empty());
        publish(&[]);
    }

    #[test]
    fn a_starter_list_keeps_only_sensible_entries() {
        let pick = |source: &str, id: &str| StarterPick { source: source.into(), id: id.into(), why: "why\n".into() };
        let r = validated(Recipe { appid: "1".into(), starter: vec![pick("nexus", "55"), pick("nexus", "x; drop"), pick("thunderstore", "Own-Mod"), pick("thunderstore", "../x"), pick("other", "1")], ..Default::default() }).unwrap();
        let ids: Vec<&str> = r.starter.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["55", "Own-Mod"]);
        assert_eq!(r.starter[0].why, "why", "control characters go");
    }
}
