//! Keeps Steam's per-game launch options in step with what rig knows the game needs
//! (WINEDLLOVERRIDES for UE4SS, gamemode, MangoHud, custom env vars and arguments).
//!
//! Steam rewrites `localconfig.vdf` when it exits, so the file is only edited while Steam
//! is closed. Edits are textual and surgical (the rest of the file is byte-identical),
//! backed up first and written atomically. Launch options the user typed themselves are
//! never replaced unless explicitly forced.

use crate::engine::{GameEntry, RigPaths};
use crate::fsutil::{self, atomic_write};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

// ───────────────────────────── minimal VDF reader/editor ─────────────────────────────

#[derive(Debug)]
enum Tok {
    Str(String, usize, usize),
    Open,
    Close(usize),
}

fn tokenize(txt: &str) -> Result<Vec<Tok>, String> {
    let b = txt.as_bytes();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b' ' | b'\t' | b'\r' | b'\n' => i += 1,
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'{' => {
                toks.push(Tok::Open);
                i += 1;
            }
            b'}' => {
                toks.push(Tok::Close(i));
                i += 1;
            }
            b'"' => {
                let start = i;
                i += 1;
                let mut out: Vec<u8> = Vec::new();
                loop {
                    match b.get(i) {
                        None => return Err("unterminated string".into()),
                        Some(b'"') => {
                            i += 1;
                            break;
                        }
                        Some(b'\\') => {
                            match b.get(i + 1) {
                                Some(b'n') => out.push(b'\n'),
                                Some(b't') => out.push(b'\t'),
                                Some(&c) => out.push(c),
                                None => return Err("dangling escape".into()),
                            }
                            i += 2;
                        }
                        Some(&c) => {
                            out.push(c);
                            i += 1;
                        }
                    }
                }
                toks.push(Tok::Str(String::from_utf8_lossy(&out).into_owned(), start, i));
            }
            c => return Err(format!("unexpected byte {:?} at {}", c as char, i)),
        }
    }
    Ok(toks)
}

#[derive(Debug)]
enum Val {
    Str { start: usize, end: usize, text: String },
    Obj { children: Vec<Entry>, close: usize },
}

#[derive(Debug)]
struct Entry {
    key: String,
    val: Val,
}

fn parse_entries(toks: &[Tok], pos: &mut usize, top: bool) -> Result<(Vec<Entry>, usize), String> {
    let mut out = Vec::new();
    loop {
        match toks.get(*pos) {
            None if top => return Ok((out, usize::MAX)),
            None => return Err("missing closing brace".into()),
            Some(Tok::Close(at)) if !top => {
                *pos += 1;
                return Ok((out, *at));
            }
            Some(Tok::Close(_)) => return Err("unbalanced closing brace".into()),
            Some(Tok::Open) => return Err("brace without key".into()),
            Some(Tok::Str(key, _, _)) => {
                *pos += 1;
                match toks.get(*pos) {
                    Some(Tok::Str(text, s, e)) => {
                        *pos += 1;
                        out.push(Entry { key: key.clone(), val: Val::Str { start: *s, end: *e, text: text.clone() } });
                    }
                    Some(Tok::Open) => {
                        *pos += 1;
                        let (children, close) = parse_entries(toks, pos, false)?;
                        out.push(Entry { key: key.clone(), val: Val::Obj { children, close } });
                    }
                    _ => return Err(format!("key '{}' has no value", key)),
                }
            }
        }
    }
}

fn parse(txt: &str) -> Result<Vec<Entry>, String> {
    let toks = tokenize(txt)?;
    let mut pos = 0;
    parse_entries(&toks, &mut pos, true).map(|(e, _)| e)
}

fn child<'a>(entries: &'a [Entry], key: &str) -> Option<&'a Entry> {
    entries.iter().find(|e| e.key.eq_ignore_ascii_case(key))
}

fn children_of(e: &Entry) -> Option<(&[Entry], usize)> {
    match &e.val {
        Val::Obj { children, close } => Some((children, *close)),
        _ => None,
    }
}

/// `UserLocalConfigStore/Software/Valve/Steam/apps`
fn apps_obj(root: &[Entry]) -> Result<(&[Entry], usize), String> {
    let mut cur = children_of(child(root, "UserLocalConfigStore").ok_or("no UserLocalConfigStore")?)
        .ok_or("UserLocalConfigStore is not a block")?;
    for k in ["Software", "Valve", "Steam", "apps"] {
        cur = children_of(child(cur.0, k).ok_or_else(|| format!("no '{}' block", k))?)
            .ok_or_else(|| format!("'{}' is not a block", k))?;
    }
    Ok(cur)
}

fn vdf_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Current LaunchOptions for `appid`: `Ok(None)` when the app block or key is absent.
pub fn get_launch_options(txt: &str, appid: &str) -> Result<Option<String>, String> {
    let root = parse(txt)?;
    let (apps, _) = apps_obj(&root)?;
    let Some(app) = apps.iter().find(|e| e.key == appid) else {
        return Ok(None);
    };
    let Some((kids, _)) = children_of(app) else {
        return Ok(None);
    };
    Ok(child(kids, "LaunchOptions").and_then(|e| match &e.val {
        Val::Str { text, .. } => Some(text.clone()),
        _ => None,
    }))
}

/// Return `txt` with `appid`'s LaunchOptions set to `value`; everything else is untouched.
pub fn set_launch_options(txt: &str, appid: &str, value: &str) -> Result<String, String> {
    let root = parse(txt)?;
    let (apps, apps_close) = apps_obj(&root)?;
    let quoted = format!("\"{}\"", vdf_escape(value));

    let indent_of = |close: usize| -> String {
        let ls = txt[..close].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let lead: String = txt[ls..close].chars().take_while(|c| *c == '\t' || *c == ' ').collect();
        if txt[ls..close].trim().is_empty() {
            lead
        } else {
            String::new()
        }
    };
    // insert `block` (lines, each ending in \n) just before the line holding `close`
    let insert_before_close = |close: usize, block: String| -> String {
        let ls = txt[..close].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let mut out = String::with_capacity(txt.len() + block.len() + 2);
        if txt[ls..close].trim().is_empty() {
            out.push_str(&txt[..ls]);
            out.push_str(&block);
            out.push_str(&txt[ls..]);
        } else {
            out.push_str(&txt[..close]);
            out.push('\n');
            out.push_str(&block);
            out.push_str(&txt[close..]);
        }
        out
    };

    match apps.iter().find(|e| e.key == appid) {
        Some(app) => {
            let (kids, close) = children_of(app).ok_or("app entry is not a block")?;
            match child(kids, "LaunchOptions").map(|e| &e.val) {
                Some(Val::Str { start, end, .. }) => {
                    Ok(format!("{}{}{}", &txt[..*start], quoted, &txt[*end..]))
                }
                _ => {
                    let ind = format!("{}\t", indent_of(close));
                    Ok(insert_before_close(close, format!("{}\"LaunchOptions\"\t\t{}\n", ind, quoted)))
                }
            }
        }
        None => {
            let ind = format!("{}\t", indent_of(apps_close));
            let block = format!(
                "{ind}\"{id}\"\n{ind}{{\n{ind}\t\"LaunchOptions\"\t\t{q}\n{ind}}}\n",
                ind = ind,
                id = appid,
                q = quoted
            );
            Ok(insert_before_close(apps_close, block))
        }
    }
}

// ───────────────────────────── compatibility tool (Proton) per game ─────────────────────────────

/// `InstallConfigStore/Software/Valve/Steam/CompatToolMapping` of Steam's `config.vdf`.
fn compat_obj(root: &[Entry]) -> Result<(&[Entry], usize), String> {
    let mut cur = children_of(child(root, "InstallConfigStore").ok_or("no InstallConfigStore")?).ok_or("InstallConfigStore is not a block")?;
    for k in ["Software", "Valve", "Steam", "CompatToolMapping"] {
        cur = children_of(child(cur.0, k).ok_or_else(|| format!("no '{}' block", k))?).ok_or_else(|| format!("'{}' is not a block", k))?;
    }
    Ok(cur)
}

/// The Proton Steam uses for `appid`: its own choice, else the default for all games ("0").
pub fn get_compat_tool(txt: &str, appid: &str) -> Result<Option<String>, String> {
    let root = parse(txt)?;
    let (map, _) = compat_obj(&root)?;
    let name_of = |key: &str| {
        map.iter().find(|e| e.key == key).and_then(children_of).and_then(|(kids, _)| child(kids, "name")).and_then(|e| match &e.val {
            Val::Str { text, .. } if !text.is_empty() => Some(text.clone()),
            _ => None,
        })
    };
    Ok(name_of(appid).or_else(|| name_of("0")))
}

/// Return `txt` with `appid` set to use the compatibility tool `tool`; everything else is untouched.
pub fn set_compat_tool(txt: &str, appid: &str, tool: &str) -> Result<String, String> {
    if tool.is_empty() || !tool.chars().all(|c| c.is_ascii_alphanumeric() || "._-+ ".contains(c)) {
        return Err("That is not a valid compatibility tool name.".into());
    }
    if appid.is_empty() || !appid.chars().all(|c| c.is_ascii_digit()) {
        return Err("Only Steam games can be given a compatibility tool.".into());
    }
    let root = parse(txt)?;
    let (map, close) = compat_obj(&root)?;
    let quoted = format!("\"{}\"", tool);
    let indent_of = |close: usize| -> String {
        let ls = txt[..close].rfind('\n').map(|i| i + 1).unwrap_or(0);
        txt[ls..close].chars().take_while(|c| *c == '\t' || *c == ' ').collect()
    };
    match map.iter().find(|e| e.key == appid) {
        Some(entry) => {
            let (kids, _) = children_of(entry).ok_or("the entry is not a block")?;
            match child(kids, "name").map(|e| &e.val) {
                Some(Val::Str { start, end, .. }) => Ok(format!("{}{}{}", &txt[..*start], quoted, &txt[*end..])),
                _ => Err("The compatibility entry has no name.".into()),
            }
        }
        None => {
            let ind = format!("{}\t", indent_of(close));
            let block = format!("{ind}\"{appid}\"\n{ind}{{\n{ind}\t\"name\"\t\t{quoted}\n{ind}\t\"config\"\t\t\"\"\n{ind}\t\"priority\"\t\t\"250\"\n{ind}}}\n");
            let ls = txt[..close].rfind('\n').map(|i| i + 1).unwrap_or(0);
            Ok(format!("{}{}{}", &txt[..ls], block, &txt[ls..]))
        }
    }
}

/// Write the tool choice into Steam's `config.vdf` (Steam must be closed; a backup is made first).
pub fn apply_compat_tool(config_vdf: &Path, appid: &str, tool: &str, steam_running: bool, backup_dir: &Path) -> Result<(), String> {
    if steam_running {
        return Err("Steam is running; close it first (it would overwrite the change on exit).".into());
    }
    let txt = fs::read_to_string(config_vdf).map_err(|e| format!("Cannot read config.vdf: {}", e))?;
    let new_txt = set_compat_tool(&txt, appid, tool)?;
    if get_compat_tool(&new_txt, appid)?.as_deref() != Some(tool) {
        return Err("Safety check failed: edited file does not read back correctly; nothing was written.".into());
    }
    fs::create_dir_all(backup_dir).map_err(|e| e.to_string())?;
    let backup = backup_dir.join(format!("config_steam_{:013}.vdf", fsutil::now_ms()));
    fs::copy(config_vdf, &backup).map_err(|e| format!("Backup failed, nothing written: {}", e))?;
    // keep the five newest backups of this file
    let mut old: Vec<PathBuf> = fs::read_dir(backup_dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.file_name().map(|n| n.to_string_lossy().starts_with("config_steam_")).unwrap_or(false)).collect();
    old.sort();
    let excess = old.len().saturating_sub(5);
    for o in old.into_iter().take(excess) {
        let _ = fs::remove_file(o);
    }
    atomic_write(config_vdf, new_txt).map_err(|e| format!("Could not write config.vdf: {}", e))
}

/// `config.vdf` of the Steam install in use.
pub fn find_config_vdf() -> Option<PathBuf> {
    RigPaths::steam_roots().into_iter().map(|r| r.join("config/config.vdf")).find(|p| p.is_file())
}

// ───────────────────────────── launch string ─────────────────────────────

fn shell_quote(v: &str) -> String {
    if !v.is_empty() && v.chars().all(|c| c.is_ascii_alphanumeric() || "_,=./:@%+-".contains(c)) {
        v.to_string()
    } else {
        let mut s = String::from("\"");
        for c in v.chars() {
            if matches!(c, '"' | '\\' | '$' | '`') {
                s.push('\\');
            }
            s.push(c);
        }
        s.push('"');
        s
    }
}

pub struct LaunchInputs<'a> {
    pub game: &'a GameEntry,
    pub gamemode: bool,
    pub mangohud: bool,
    /// WINEDLLOVERRIDES value to export, when the game needs one.
    pub dll_overrides: Option<String>,
    pub have_gamemoderun: bool,
}

/// The exact Steam launch-options string rig wants for the game.
pub fn wanted_launch_options(i: &LaunchInputs) -> String {
    let mut parts: Vec<String> = Vec::new();
    for line in i.game.env_vars.lines() {
        if let Some((k, v)) = line.split_once('=') {
            let k = k.trim();
            if !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                parts.push(format!("{}={}", k, shell_quote(v.trim().trim_matches('"'))));
            }
        }
    }
    if let Some(o) = &i.dll_overrides {
        if !i.game.env_vars.lines().any(|l| l.trim_start().starts_with("WINEDLLOVERRIDES=")) {
            parts.push(format!("WINEDLLOVERRIDES={}", shell_quote(o)));
        }
    }
    if i.mangohud {
        parts.push("MANGOHUD=1".into());
    }
    if i.gamemode && i.have_gamemoderun {
        parts.push("gamemoderun".into());
    }
    parts.push("%command%".into());
    let args = i.game.launch_args.split_whitespace().collect::<Vec<_>>().join(" ");
    if !args.is_empty() {
        parts.push(args);
    }
    parts.join(" ")
}

// ───────────────────────────── status / apply ─────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SteamLaunchInfo {
    /// ok | missing | stale | custom | n/a | unavailable
    pub status: String,
    pub current: String,
    pub wanted: String,
    pub steam_running: bool,
    pub detail: String,
}

impl SteamLaunchInfo {
    fn simple(status: &str, wanted: &str, detail: &str, steam_running: bool) -> Self {
        Self { status: status.into(), current: String::new(), wanted: wanted.into(), steam_running, detail: detail.into() }
    }
}

fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

const STEAMID64_BASE: u64 = 76_561_197_960_265_728;

/// Name of the account Steam logs in as ("AutoLoginUser" in its registry file).
fn auto_login_user(registry: &str) -> Option<String> {
    let re = regex::Regex::new(r#""AutoLoginUser"\s+"([^"]+)""#).ok()?;
    re.captures(registry).map(|c| c[1].to_string())
}

/// `localconfig.vdf` of the account that is logged in (or logs in next) in the Steam under `root`.
/// Launch options belong to one account; writing them into another one's file changes nothing.
pub fn active_localconfig(root: &Path, registry: &str) -> Option<PathBuf> {
    let name = auto_login_user(registry)?;
    let acct = crate::steamlib::accounts(&[root.to_path_buf()]).into_iter().find(|a| a.name.eq_ignore_ascii_case(&name))?;
    let id = acct.steamid.parse::<u64>().ok()?.checked_sub(STEAMID64_BASE)?;
    let p = root.join("userdata").join(id.to_string()).join("config/localconfig.vdf");
    p.is_file().then_some(p)
}

/// Every account's `localconfig.vdf` in the Steam under `root`.
fn localconfigs_in(root: &Path) -> Vec<PathBuf> {
    fs::read_dir(root.join("userdata"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name() != "0" && e.file_name() != "anonymous")
        .map(|e| e.path().join("config/localconfig.vdf"))
        .filter(|p| p.is_file())
        .collect()
}

/// All accounts' `localconfig.vdf`, the logged-in account's first.
pub fn find_all_localconfigs() -> Vec<PathBuf> {
    let registry = crate::platform::steam_registry_text();
    let mut out: Vec<PathBuf> = Vec::new();
    for root in RigPaths::steam_roots() {
        let active = active_localconfig(&root, &registry);
        let mut all = localconfigs_in(&root);
        all.sort_by_key(|p| std::cmp::Reverse(fs::metadata(p).and_then(|m| m.modified()).ok()));
        for p in active.into_iter().chain(all) {
            let canon = crate::fsutil::canonical(&p).unwrap_or_else(|_| p.clone());
            if !out.iter().any(|o| crate::fsutil::canonical(o).unwrap_or_else(|_| o.clone()) == canon) {
                out.push(p);
            }
        }
    }
    out
}

/// The `localconfig.vdf` to read launch options from: the logged-in account's, else the newest one.
pub fn find_localconfig() -> Option<PathBuf> {
    find_all_localconfigs().into_iter().next().or_else(find_localconfig_newest)
}

/// Most recently written `localconfig.vdf` across all Steam roots and users.
fn find_localconfig_newest() -> Option<PathBuf> {
    RigPaths::steam_roots()
        .into_iter()
        .flat_map(|root| {
            fs::read_dir(root.join("userdata"))
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| e.file_name() != "0" && e.file_name() != "anonymous")
                .map(|e| e.path().join("config/localconfig.vdf"))
                .collect::<Vec<_>>()
        })
        .filter_map(|p| {
            let m = fs::metadata(&p).ok()?;
            let t = m.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
            Some((t, p))
        })
        .max_by_key(|(t, _)| *t)
        .map(|(_, p)| p)
}

/// Compare Steam's current launch options with what rig wants.
/// `last_applied` is the value rig itself wrote last time (so it can tell its own,
/// now outdated, options from ones the user typed).
pub fn inspect_file(
    localconfig: Option<&Path>,
    game: &GameEntry,
    wanted: &str,
    last_applied: Option<&str>,
    steam_running: bool,
) -> SteamLaunchInfo {
    if game.launches_directly() {
        return SteamLaunchInfo::simple("n/a", wanted, "This game is launched by rig directly (not through Steam).", steam_running);
    }
    let Some(path) = localconfig else {
        return SteamLaunchInfo::simple("unavailable", wanted, "Steam's localconfig.vdf was not found.", steam_running);
    };
    let txt = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => return SteamLaunchInfo::simple("unavailable", wanted, &format!("Cannot read localconfig.vdf: {}", e), steam_running),
    };
    let current = match get_launch_options(&txt, &game.appid) {
        Ok(c) => c.unwrap_or_default(),
        Err(e) => return SteamLaunchInfo::simple("unavailable", wanted, &format!("Unexpected localconfig.vdf layout: {}", e), steam_running),
    };
    let (cn, wn) = (norm(&current), norm(wanted));
    let status = if cn == wn || (cn.is_empty() && wn == "%command%") {
        "ok"
    } else if cn.is_empty() {
        "missing"
    } else if last_applied.map(norm).as_deref() == Some(cn.as_str()) {
        "stale"
    } else {
        "custom"
    };
    SteamLaunchInfo { status: status.into(), current, wanted: wanted.into(), steam_running, detail: String::new() }
}

pub fn inspect(game: &GameEntry, wanted: &str, last_applied: Option<&str>) -> SteamLaunchInfo {
    inspect_file(find_localconfig().as_deref(), game, wanted, last_applied, crate::procs::steam_running())
}

/// Write `wanted` into `localconfig` for `game`. Refuses while Steam is running (it would
/// overwrite the change on exit). Keeps the five most recent backups in `backup_dir`.
pub fn apply_to_file(
    localconfig: &Path,
    game: &GameEntry,
    wanted: &str,
    steam_running: bool,
    backup_dir: &Path,
) -> Result<(), String> {
    if steam_running {
        return Err("Steam is running; close it first (it would overwrite the change on exit).".into());
    }
    let txt = fs::read_to_string(localconfig).map_err(|e| format!("Cannot read localconfig.vdf: {}", e))?;
    let new_txt = set_launch_options(&txt, &game.appid, wanted)?;
    // The result must still be a well-formed document that reads back exactly as intended.
    let back = get_launch_options(&new_txt, &game.appid)?;
    if back.as_deref() != Some(wanted) {
        return Err("Safety check failed: edited file does not read back correctly; nothing was written.".into());
    }
    fs::create_dir_all(backup_dir).map_err(|e| e.to_string())?;
    let user = localconfig
        .parent()
        .and_then(Path::parent)
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let backup = backup_dir.join(format!("localconfig_{}_{:013}.vdf", user, fsutil::now_ms()));
    fs::copy(localconfig, &backup).map_err(|e| format!("Backup failed, nothing written: {}", e))?;
    prune_backups(backup_dir, 5);
    atomic_write(localconfig, new_txt).map_err(|e| format!("Could not write localconfig.vdf: {}", e))
}

/// Keep the newest `keep` backups of each Steam account (names are `localconfig_<account>_<ms>.vdf`).
fn prune_backups(dir: &Path, keep: usize) {
    let mut by_user: std::collections::BTreeMap<String, Vec<(u64, PathBuf)>> = Default::default();
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(rest) = name.strip_prefix("localconfig_").and_then(|r| r.strip_suffix(".vdf")) else { continue };
        let (user, ms) = rest.rsplit_once('_').unwrap_or((rest, "0"));
        by_user.entry(user.to_string()).or_default().push((ms.parse().unwrap_or(0), e.path()));
    }
    for mut files in by_user.into_values() {
        files.sort();
        let excess = files.len().saturating_sub(keep);
        for (_, old) in files.into_iter().take(excess) {
            let _ = fs::remove_file(old);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsutil::testutil::TempDir;

    const SAMPLE: &str = "\"UserLocalConfigStore\"\n{\n\t\"Software\"\n\t{\n\t\t\"valve\"\n\t\t{\n\t\t\t\"Steam\"\n\t\t\t{\n\t\t\t\t\"apps\"\n\t\t\t\t{\n\t\t\t\t\t\"7\"\n\t\t\t\t\t{\n\t\t\t\t\t\t\"LastPlayed\"\t\t\"1\"\n\t\t\t\t\t}\n\t\t\t\t\t\"2397300\"\n\t\t\t\t\t{\n\t\t\t\t\t\t\"LastPlayed\"\t\t\"1700000000\"\n\t\t\t\t\t\t\"LaunchOptions\"\t\t\"-old \\\"quoted\\\" \\\\path\"\n\t\t\t\t\t\t\"Playtime\"\t\t\"5\"\n\t\t\t\t\t}\n\t\t\t\t}\n\t\t\t}\n\t\t}\n\t}\n\t\"friends\"\n\t{\n\t\t\"x\"\t\t\"y\" // trailing comment\n\t}\n}\n";

    const CONFIG: &str = "\"InstallConfigStore\"\n{\n\t\"Software\"\n\t{\n\t\t\"Valve\"\n\t\t{\n\t\t\t\"Steam\"\n\t\t\t{\n\t\t\t\t\"CompatToolMapping\"\n\t\t\t\t{\n\t\t\t\t\t\"0\"\n\t\t\t\t\t{\n\t\t\t\t\t\t\"name\"\t\t\"proton-cachyos-slr\"\n\t\t\t\t\t\t\"config\"\t\t\"\"\n\t\t\t\t\t\t\"priority\"\t\t\"75\"\n\t\t\t\t\t}\n\t\t\t\t\t\"440\"\n\t\t\t\t\t{\n\t\t\t\t\t\t\"name\"\t\t\"proton_9\"\n\t\t\t\t\t\t\"config\"\t\t\"\"\n\t\t\t\t\t\t\"priority\"\t\t\"250\"\n\t\t\t\t\t}\n\t\t\t\t}\n\t\t\t\t\"other\"\t\t\"keep me\"\n\t\t\t}\n\t\t}\n\t}\n}\n";

    #[test]
    fn the_proton_of_a_steam_game_is_read_and_set_without_touching_anything_else() {
        assert_eq!(get_compat_tool(CONFIG, "440").unwrap().as_deref(), Some("proton_9"));
        assert_eq!(get_compat_tool(CONFIG, "999").unwrap().as_deref(), Some("proton-cachyos-slr"), "games without a choice use the default");
        // change a game that already has a choice, and add one that has none
        let a = set_compat_tool(CONFIG, "440", "GE-Proton10-1").unwrap();
        assert_eq!(get_compat_tool(&a, "440").unwrap().as_deref(), Some("GE-Proton10-1"));
        let b = set_compat_tool(CONFIG, "3164500", "proton_experimental").unwrap();
        assert_eq!(get_compat_tool(&b, "3164500").unwrap().as_deref(), Some("proton_experimental"));
        assert_eq!(get_compat_tool(&b, "440").unwrap().as_deref(), Some("proton_9"));
        assert!(b.contains("\"other\"\t\t\"keep me\""), "the rest of the file is left alone");
        assert!(b.contains("\"priority\"\t\t\"250\""));
        assert!(set_compat_tool(CONFIG, "440", "evil\" \"x").is_err() && set_compat_tool(CONFIG, "abc", "proton_9").is_err() && set_compat_tool(CONFIG, "440", "").is_err());
        assert!(set_compat_tool("\"Other\"\n{\n}\n", "440", "proton_9").is_err(), "an unexpected layout is refused");
    }

    #[test]
    fn the_tool_choice_is_written_only_while_steam_is_closed_and_with_a_backup() {
        let d = TempDir::new("compat");
        let cfg = d.join("config.vdf");
        fs::write(&cfg, CONFIG).unwrap();
        let backups = d.join("bak");
        assert!(apply_compat_tool(&cfg, "440", "proton_8", true, &backups).is_err());
        assert_eq!(fs::read_to_string(&cfg).unwrap(), CONFIG);
        apply_compat_tool(&cfg, "440", "proton_8", false, &backups).unwrap();
        assert_eq!(get_compat_tool(&fs::read_to_string(&cfg).unwrap(), "440").unwrap().as_deref(), Some("proton_8"));
        assert_eq!(fs::read_dir(&backups).unwrap().count(), 1);
        assert_eq!(fs::read_to_string(fs::read_dir(&backups).unwrap().next().unwrap().unwrap().path()).unwrap(), CONFIG, "the backup is the file as it was");
    }

    #[test]
    fn launch_options_go_to_the_account_that_is_logged_in() {
        let d = TempDir::new("accounts");
        let root = d.join("Steam");
        fs::create_dir_all(root.join("config")).unwrap();
        // two accounts of one person: account_two (id 39734274) logs in automatically, account_one (39734273) is the newer file
        fs::write(
            root.join("config/loginusers.vdf"),
            "\"users\"\n{\n\t\"76561198000000002\"\n\t{\n\t\t\"AccountName\"\t\t\"account_two\"\n\t\t\"timestamp\"\t\t\"200\"\n\t}\n\t\"76561198000000001\"\n\t{\n\t\t\"AccountName\"\t\t\"account_one\"\n\t\t\"timestamp\"\t\t\"100\"\n\t}\n}\n",
        )
        .unwrap();
        for id in ["39734274", "39734273"] {
            fs::create_dir_all(root.join("userdata").join(id).join("config")).unwrap();
            fs::write(root.join("userdata").join(id).join("config/localconfig.vdf"), SAMPLE).unwrap();
        }
        let registry = "\"Registry\"\n{\n\t\"HKCU\"\n\t{\n\t\t\"Software\"\n\t\t{\n\t\t\t\"AutoLoginUser\"\t\t\"account_two\"\n\t\t}\n\t}\n}\n";
        let active = active_localconfig(&root, registry).unwrap();
        assert!(crate::fsutil::slashed(&active).contains("userdata/39734274/"), "{active:?}");
        assert_eq!(localconfigs_in(&root).len(), 2);
        assert!(active_localconfig(&root, "no registry").is_none());
        assert!(active_localconfig(&root, "\"AutoLoginUser\"\t\t\"someone-else\"").is_none());
    }

    #[test]
    fn old_backups_are_pruned_per_account_and_the_new_one_always_stays() {
        let d = TempDir::new("prune");
        for (user, ms) in [("1111", 1), ("1111", 2), ("1111", 3), ("9999", 10), ("9999", 11), ("9999", 12), ("9999", 13)] {
            fs::write(d.join(&format!("localconfig_{user}_{ms:013}.vdf")), "x").unwrap();
        }
        // a backup of the account whose number sorts first, taken after all the others
        fs::write(d.join("localconfig_0001_0000000000099.vdf"), "x").unwrap();
        prune_backups(d.path(), 2);
        let mut left: Vec<String> = fs::read_dir(d.path()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        left.sort();
        assert_eq!(left, vec!["localconfig_0001_0000000000099.vdf", "localconfig_1111_0000000000002.vdf", "localconfig_1111_0000000000003.vdf", "localconfig_9999_0000000000012.vdf", "localconfig_9999_0000000000013.vdf"]);
    }

    fn game(id: &str) -> GameEntry {
        GameEntry {
            appid: id.into(), name: "T".into(), game_dir: String::new(), exe_path: String::new(), shipping_exe: String::new(),
            bin_dir: String::new(), engine: "Unreal Engine 5.4".into(), compatdata: String::new(), pfx: String::new(),
            has_pfx: false, is_custom: false, is_installed: true, steam_appid: String::new(), launch_args: String::new(),
            env_vars: String::new(), trainer_name: String::new(), mod_count: 0, source: String::new(), proton_path: String::new(), ue4ss: false, loader: String::new(), cover_id: String::new(),
        }
    }

    #[test]
    fn mangled_vdf_never_panics() {
        let mut x: u64 = 0xD1B5_4A32_D192_ED03;
        let mut rnd = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..3000 {
            let mut b = SAMPLE.as_bytes().to_vec();
            for _ in 0..1 + rnd() % 8 {
                let n = b.len();
                match rnd() % 3 {
                    0 => b.truncate((rnd() as usize) % (n + 1)),
                    1 if n > 0 => b[(rnd() as usize) % n] = b"\"{}\\\n\t x"[(rnd() as usize) % 8],
                    _ => b.insert((rnd() as usize) % (n + 1), rnd() as u8),
                }
            }
            let txt = String::from_utf8_lossy(&b).into_owned();
            let _ = get_launch_options(&txt, "2397300");
            let _ = set_launch_options(&txt, "2397300", "x \"y\" %command%");
            let _ = set_launch_options(&txt, "999", "%command%");
        }
    }

    #[test]
    fn reads_escaped_values_and_absent_keys() {
        assert_eq!(get_launch_options(SAMPLE, "2397300").unwrap().as_deref(), Some("-old \"quoted\" \\path"));
        assert_eq!(get_launch_options(SAMPLE, "7").unwrap(), None);
        assert_eq!(get_launch_options(SAMPLE, "999").unwrap(), None);
        assert!(get_launch_options("garbage {", "1").is_err());
        assert!(get_launch_options("\"Other\"\n{\n}\n", "1").is_err());
    }

    #[test]
    fn replaces_in_place_and_leaves_the_rest_byte_identical() {
        let out = set_launch_options(SAMPLE, "2397300", "WINEDLLOVERRIDES=dwmapi=n,b %command%").unwrap();
        assert_eq!(get_launch_options(&out, "2397300").unwrap().as_deref(), Some("WINEDLLOVERRIDES=dwmapi=n,b %command%"));
        let before = SAMPLE.replace("-old \\\"quoted\\\" \\\\path", "X");
        let after = out.replace("WINEDLLOVERRIDES=dwmapi=n,b %command%", "X");
        assert_eq!(before, after, "only the value changed");
    }

    #[test]
    fn inserts_key_into_existing_app_and_creates_missing_app() {
        let out = set_launch_options(SAMPLE, "7", "a \"b\" %command%").unwrap();
        assert_eq!(get_launch_options(&out, "7").unwrap().as_deref(), Some("a \"b\" %command%"));
        assert!(out.contains("\t\t\t\t\t\t\"LaunchOptions\"\t\t\"a \\\"b\\\" %command%\"\n\t\t\t\t\t}"), "{}", out);
        let out = set_launch_options(SAMPLE, "555", "%command%").unwrap();
        assert_eq!(get_launch_options(&out, "555").unwrap().as_deref(), Some("%command%"));
        assert!(out.contains("\t\t\t\t\t\"555\"\n\t\t\t\t\t{\n\t\t\t\t\t\t\"LaunchOptions\"\t\t\"%command%\"\n\t\t\t\t\t}\n"), "{}", out);
        // other apps and sections survive
        assert_eq!(get_launch_options(&out, "2397300").unwrap().as_deref(), Some("-old \"quoted\" \\path"));
        assert!(out.contains("\"friends\""));
        assert!(set_launch_options("\"UserLocalConfigStore\"\n{\n}\n", "1", "x").is_err());
    }

    #[test]
    fn launch_string_composition_and_quoting() {
        let mut g = game("1");
        g.env_vars = "PROTON_ENABLE_NVAPI=1\nBAD KEY=2\nNAME=\"has space\"\nCMD=$(x)\n".into();
        g.launch_args = " -dx12   -windowed ".into();
        let s = wanted_launch_options(&LaunchInputs { game: &g, gamemode: true, mangohud: true, dll_overrides: Some("dwmapi=n,b".into()), have_gamemoderun: true });
        assert_eq!(s, "PROTON_ENABLE_NVAPI=1 NAME=\"has space\" CMD=\"\\$(x)\" WINEDLLOVERRIDES=dwmapi=n,b MANGOHUD=1 gamemoderun %command% -dx12 -windowed");
        let plain = wanted_launch_options(&LaunchInputs { game: &game("1"), gamemode: false, mangohud: false, dll_overrides: None, have_gamemoderun: false });
        assert_eq!(plain, "%command%");
        // gamemode requested but binary missing: not added
        let s = wanted_launch_options(&LaunchInputs { game: &game("1"), gamemode: true, mangohud: false, dll_overrides: None, have_gamemoderun: false });
        assert_eq!(s, "%command%");
        // user-defined WINEDLLOVERRIDES wins over rig's
        let mut g = game("1");
        g.env_vars = "WINEDLLOVERRIDES=winmm=n".into();
        let s = wanted_launch_options(&LaunchInputs { game: &g, gamemode: false, mangohud: false, dll_overrides: Some("dwmapi=n,b".into()), have_gamemoderun: false });
        assert_eq!(s.matches("WINEDLLOVERRIDES").count(), 1);
    }

    fn setup(tag: &str) -> (TempDir, PathBuf) {
        let d = TempDir::new(tag);
        let f = d.join("userdata/12345/config/localconfig.vdf");
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(&f, SAMPLE).unwrap();
        (d, f)
    }

    #[test]
    fn status_matrix() {
        let (_d, f) = setup("status");
        let g = game("2397300");
        let wanted = "WINEDLLOVERRIDES=dwmapi=n,b %command%";
        // the sample holds the user's own options
        assert_eq!(inspect_file(Some(&f), &g, wanted, None, false).status, "custom");
        // ...unless rig itself wrote them earlier: then they are merely outdated
        assert_eq!(inspect_file(Some(&f), &g, wanted, Some("-old \"quoted\"   \\path"), false).status, "stale");
        assert_eq!(inspect_file(Some(&f), &game("7"), wanted, None, false).status, "missing");
        assert_eq!(inspect_file(Some(&f), &game("7"), "%command%", None, false).status, "ok");
        let mut c = game("2397300");
        c.is_custom = true;
        assert_eq!(inspect_file(Some(&f), &c, wanted, None, false).status, "n/a");
        assert_eq!(inspect_file(None, &g, wanted, None, false).status, "unavailable");
        fs::write(&f, "not vdf {{{").unwrap();
        assert_eq!(inspect_file(Some(&f), &g, wanted, None, false).status, "unavailable");
    }

    #[test]
    fn apply_backs_up_refuses_while_steam_runs_and_roundtrips() {
        let (d, f) = setup("apply");
        let g = game("7");
        let backups = d.join("bk");
        let wanted = "WINEDLLOVERRIDES=dwmapi=n,b gamemoderun %command%";

        let err = apply_to_file(&f, &g, wanted, true, &backups).unwrap_err();
        assert!(err.contains("Steam is running"));
        assert_eq!(fs::read_to_string(&f).unwrap(), SAMPLE, "untouched while Steam runs");
        assert!(!backups.exists());

        apply_to_file(&f, &g, wanted, false, &backups).unwrap();
        assert_eq!(inspect_file(Some(&f), &g, wanted, None, false).status, "ok");
        let bk: Vec<_> = fs::read_dir(&backups).unwrap().flatten().collect();
        assert_eq!(bk.len(), 1);
        assert_eq!(fs::read_to_string(bk[0].path()).unwrap(), SAMPLE, "backup is the original");
        // no temp files left beside the config
        assert!(fs::read_dir(f.parent().unwrap()).unwrap().flatten().all(|e| !fsutil::is_tmp_path(&e.path())));

        // backups are pruned to the five most recent
        for i in 0..8 {
            apply_to_file(&f, &g, &format!("%command% -n{}", i), false, &backups).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(fs::read_dir(&backups).unwrap().count(), 5);
    }

    #[test]
    fn newest_localconfig_across_users_is_chosen_and_zero_skipped() {
        // exercised through the public helper on a fake root via the same selection logic
        let d = TempDir::new("pick");
        for (u, body) in [("111", "a"), ("0", "b"), ("222", "c")] {
            let f = d.join(&format!("userdata/{}/config/localconfig.vdf", u));
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(&f, body).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        let best = fs::read_dir(d.join("userdata"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name() != "0")
            .map(|e| e.path().join("config/localconfig.vdf"))
            .max_by_key(|p| fs::metadata(p).unwrap().modified().unwrap())
            .unwrap();
        assert!(crate::fsutil::slashed(&best).contains("/222/"));
    }
}
