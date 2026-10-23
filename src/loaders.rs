//! Mod loaders for Unity games (BepInEx, MelonLoader): what a game has, what it needs, and where
//! to get it. Unity mods do nothing until one of these is in the game folder, and the right build
//! depends on how the game was made: Mono or IL2CPP, 32 or 64 bit.
//!
//! Downloads are looked up through the projects' own release pages (names change with every
//! build), only accepted from their official download areas, and kept in a cache so a loader
//! fetched once keeps working offline.

use regex::Regex;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// What kind of Unity game a folder holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnityInfo {
    /// Compiled to native code (`GameAssembly.dll`): needs the IL2CPP flavour of a loader.
    pub il2cpp: bool,
    pub x64: bool,
    /// "bepinex", "melonloader" or empty.
    pub loader: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Loader {
    /// BepInEx 5: the stable release for Mono games.
    Bepinex5,
    /// BepInEx 6 build: the one that supports IL2CPP games (and Mono ones).
    Bepinex6,
    MelonLoader,
}

impl Loader {
    pub fn parse(s: &str) -> Option<Loader> {
        match s {
            "bepinex5" => Some(Loader::Bepinex5),
            "bepinex6" => Some(Loader::Bepinex6),
            "melonloader" => Some(Loader::MelonLoader),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Loader::Bepinex5 => "BepInEx 5",
            Loader::Bepinex6 => "BepInEx 6",
            Loader::MelonLoader => "MelonLoader",
        }
    }
}

/// CPU type of a Windows program or library from its PE header: `Some(true)` for 64-bit.
pub fn pe_is_64bit(path: &Path) -> Option<bool> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = fs::File::open(path).ok()?;
    let mut dos = [0u8; 64];
    f.read_exact(&mut dos).ok()?;
    if &dos[..2] != b"MZ" {
        return None;
    }
    let off = u32::from_le_bytes([dos[60], dos[61], dos[62], dos[63]]) as u64;
    f.seek(SeekFrom::Start(off)).ok()?;
    let mut pe = [0u8; 6];
    f.read_exact(&mut pe).ok()?;
    if &pe[..4] != b"PE\0\0" {
        return None;
    }
    match u16::from_le_bytes([pe[4], pe[5]]) {
        0x8664 | 0xAA64 => Some(true),
        0x014c => Some(false),
        _ => None,
    }
}

fn exists_ci(dir: &Path, name: &str) -> Option<PathBuf> {
    fs::read_dir(dir).ok()?.flatten().find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(name)).map(|e| e.path())
}

/// What the Unity game in `dir` is like, or `None` when it is not a Unity game.
pub fn detect(dir: &Path) -> Option<UnityInfo> {
    let player = exists_ci(dir, "UnityPlayer.dll");
    let data = fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .find(|e| e.file_name().to_string_lossy().to_lowercase().ends_with("_data") && (e.path().join("globalgamemanagers").is_file() || e.path().join("data.unity3d").is_file() || e.path().join("Managed").is_dir() || e.path().join("il2cpp_data").is_dir()));
    if player.is_none() && data.is_none() {
        return None;
    }
    let il2cpp = exists_ci(dir, "GameAssembly.dll").is_some() || data.as_ref().map(|d| d.path().join("il2cpp_data").is_dir()).unwrap_or(false);
    let exe64 = || {
        fs::read_dir(dir)
            .ok()?
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().to_lowercase().ends_with(".exe") && !e.file_name().to_string_lossy().to_lowercase().contains("crash"))
            .find_map(|e| pe_is_64bit(&e.path()))
    };
    let x64 = player.as_ref().and_then(|p| pe_is_64bit(p)).or_else(exe64).unwrap_or(true);
    let loader = if exists_ci(dir, "MelonLoader").map(|p| p.is_dir()).unwrap_or(false) {
        "melonloader"
    } else if exists_ci(dir, "BepInEx").map(|p| p.is_dir()).unwrap_or(false) {
        "bepinex"
    } else {
        ""
    };
    Some(UnityInfo { il2cpp, x64, loader: loader.into() })
}

// ───────────────────────────── where to get them ─────────────────────────────

#[derive(Clone, Debug)]
pub struct Sources {
    pub bepinex_api: String,
    pub melon_api: String,
    /// The BepInEx build server's page for the 6.x builds.
    pub be_listing: String,
    pub be_base: String,
    /// Download addresses must start with one of these (the projects' own areas).
    pub allowed: Vec<String>,
}

impl Default for Sources {
    fn default() -> Self {
        Self {
            bepinex_api: "https://api.github.com/repos/BepInEx/BepInEx/releases/latest".into(),
            melon_api: "https://api.github.com/repos/LavaGang/MelonLoader/releases/latest".into(),
            be_listing: "https://builds.bepinex.dev/projects/bepinex_be".into(),
            be_base: "https://builds.bepinex.dev".into(),
            allowed: vec![
                "https://github.com/BepInEx/BepInEx/releases/download/".into(),
                "https://github.com/LavaGang/MelonLoader/releases/download/".into(),
                "https://builds.bepinex.dev/".into(),
            ],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub version: String,
}

fn name_ok(n: &str) -> bool {
    n.ends_with(".zip") && !n.contains('/') && !n.contains("..") && n.chars().all(|c| c.is_ascii_alphanumeric() || "._-+".contains(c))
}

/// The release asset whose name starts with one of `prefixes` and ends in `.zip`.
pub fn pick_release_asset(release: &Value, prefixes: &[String], allowed: &[String]) -> Option<Asset> {
    let tag = release.get("tag_name").and_then(Value::as_str).unwrap_or("").trim_start_matches('v').to_string();
    release.get("assets")?.as_array()?.iter().find_map(|a| {
        let name = a.get("name")?.as_str()?;
        let url = a.get("browser_download_url")?.as_str()?;
        (prefixes.iter().any(|p| name.starts_with(p.as_str())) && name_ok(name) && allowed.iter().any(|al| url.starts_with(al.as_str())))
            .then(|| Asset { name: name.to_string(), url: url.to_string(), version: tag.clone() })
    })
}

/// The newest BepInEx 6 build of the wanted kind out of the build server's page.
pub fn pick_be_build(html: &str, base: &str, il2cpp: bool, x64: bool) -> Option<Asset> {
    let re = Regex::new(r#"href="(/projects/bepinex_be/(\d+)/(BepInEx-Unity\.(IL2CPP|Mono)-win-(x64|x86)-6\.0\.0-be\.\d+(?:%2B|\+)[0-9a-f]+\.zip))""#).ok()?;
    re.captures_iter(html)
        .filter(|c| (&c[4] == "IL2CPP") == il2cpp && (&c[5] == "x64") == x64)
        .max_by_key(|c| c[2].parse::<u64>().unwrap_or(0))
        .map(|c| {
            let name = c[3].replace("%2B", "+");
            Asset { version: format!("6.0.0-be.{}", &c[2]), url: format!("{}{}", base.trim_end_matches('/'), &c[1]), name }
        })
}

fn http_get(url: &str) -> Result<Vec<u8>, String> {
    let out = Command::new("curl")
        .args(["-sSL", "--max-time", "30", "-H", "Accept: application/vnd.github+json", "-H", "User-Agent: Gantry"])
        .arg(url)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("Could not run curl ({e})."))?;
    if out.stdout.is_empty() {
        return Err("Could not reach the download site.".into());
    }
    Ok(out.stdout)
}

/// Look up the download for `loader` that fits the game.
pub fn find_asset(loader: Loader, info: &UnityInfo, src: &Sources) -> Result<Asset, String> {
    let arch = if info.x64 { "x64" } else { "x86" };
    match loader {
        Loader::Bepinex5 => {
            let v: Value = serde_json::from_slice(&http_get(&src.bepinex_api)?).map_err(|_| "Could not read the BepInEx release list.".to_string())?;
            pick_release_asset(&v, &[format!("BepInEx_win_{arch}_"), format!("BepInEx_{arch}_")], &src.allowed).ok_or_else(|| "No BepInEx 5 download was listed for this game.".to_string())
        }
        Loader::MelonLoader => {
            let v: Value = serde_json::from_slice(&http_get(&src.melon_api)?).map_err(|_| "Could not read the MelonLoader release list.".to_string())?;
            pick_release_asset(&v, &[format!("MelonLoader.{arch}.zip")], &src.allowed).ok_or_else(|| "No MelonLoader download was listed for this game.".to_string())
        }
        Loader::Bepinex6 => {
            let html = String::from_utf8_lossy(&http_get(&src.be_listing)?).to_string();
            let a = pick_be_build(&html, &src.be_base, info.il2cpp, info.x64).ok_or_else(|| "No BepInEx 6 build was listed for this game.".to_string())?;
            if src.allowed.iter().any(|al| a.url.starts_with(al.as_str())) {
                Ok(a)
            } else {
                Err("Refusing a download address outside the BepInEx build server.".into())
            }
        }
    }
}

/// The archive on disk: cached by name, downloaded when missing. `say` reports progress.
pub fn ensure_zip(cache_dir: &Path, asset: &Asset, say: &dyn Fn(&str)) -> Result<PathBuf, String> {
    fs::create_dir_all(cache_dir).map_err(|e| e.to_string())?;
    let dest = cache_dir.join(&asset.name);
    if dest.is_file() {
        return Ok(dest);
    }
    say(&format!("Downloading {}...", asset.name));
    let part = dest.with_extension(format!("zip{}-{}", crate::fsutil::TMP_MARK, crate::fsutil::unique_token()));
    let ok = Command::new("curl")
        .args(["-fL", "--retry", "2", "--connect-timeout", "15", "--max-time", "1800", "-sS", "-o"])
        .arg(&part)
        .arg(&asset.url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok || fs::metadata(&part).map(|m| m.len()).unwrap_or(0) == 0 {
        let _ = fs::remove_file(&part);
        return Err("The download failed (check your connection).".into());
    }
    fs::rename(&part, &dest).map_err(|e| {
        let _ = fs::remove_file(&part);
        format!("Could not store the download: {e}")
    })?;
    Ok(dest)
}

#[cfg(test)]
pub mod fake {
    use super::*;

    /// A local stand-in for GitHub's release API, the BepInEx build server and the downloads.
    pub struct Fake {
        pub src: Sources,
        pub hits: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    pub fn start(zip: Vec<u8>) -> Fake {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let base = format!("http://{}", server.server_addr().to_ip().unwrap());
        let hits = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (h2, b2) = (hits.clone(), base.clone());
        std::thread::spawn(move || {
            for req in server.incoming_requests() {
                let url = req.url().to_string();
                h2.lock().unwrap().push(url.clone());
                let body = if url == "/bepinex" {
                    format!(r#"{{"tag_name":"v5.4.23.5","assets":[{{"name":"BepInEx_win_x64_5.4.23.5.zip","browser_download_url":"{b2}/dl/BepInEx_win_x64_5.4.23.5.zip"}},{{"name":"BepInEx_win_x86_5.4.23.5.zip","browser_download_url":"{b2}/dl/BepInEx_win_x86_5.4.23.5.zip"}}]}}"#).into_bytes()
                } else if url == "/melon" {
                    format!(r#"{{"tag_name":"v0.7.3","assets":[{{"name":"MelonLoader.x64.zip","browser_download_url":"{b2}/dl/MelonLoader.x64.zip"}}]}}"#).into_bytes()
                } else if url == "/be" {
                    r#"<a href="/projects/bepinex_be/788/BepInEx-Unity.IL2CPP-win-x64-6.0.0-be.788%2B5b766a3.zip">x</a><a href="/projects/bepinex_be/789/BepInEx-Unity.IL2CPP-win-x64-6.0.0-be.789%2B6cea286.zip">y</a>"#.to_string().into_bytes()
                } else if url.starts_with("/dl/") || url.starts_with("/projects/") {
                    zip.clone()
                } else {
                    let _ = req.respond(tiny_http::Response::from_data(Vec::new()).with_status_code(404));
                    continue;
                };
                let _ = req.respond(tiny_http::Response::from_data(body));
            }
        });
        let src = Sources {
            bepinex_api: format!("{base}/bepinex"),
            melon_api: format!("{base}/melon"),
            be_listing: format!("{base}/be"),
            be_base: base.clone(),
            allowed: vec![format!("{base}/")],
        };
        Fake { src, hits }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsutil::testutil::TempDir;

    fn pe(machine: u16) -> Vec<u8> {
        let mut b = vec![0u8; 128];
        b[0] = b'M';
        b[1] = b'Z';
        b[60] = 64;
        b[64..68].copy_from_slice(b"PE\0\0");
        b[68..70].copy_from_slice(&machine.to_le_bytes());
        b
    }

    #[test]
    fn the_cpu_type_comes_from_the_pe_header() {
        let d = TempDir::new("pe");
        fs::write(d.join("a64.dll"), pe(0x8664)).unwrap();
        fs::write(d.join("a32.dll"), pe(0x014c)).unwrap();
        fs::write(d.join("junk.dll"), b"nope").unwrap();
        assert_eq!((pe_is_64bit(&d.join("a64.dll")), pe_is_64bit(&d.join("a32.dll")), pe_is_64bit(&d.join("junk.dll"))), (Some(true), Some(false), None));
    }

    #[test]
    fn unity_games_are_recognised_with_their_flavour_and_loader() {
        let d = TempDir::new("unity");
        let mono = d.join("Mono Game");
        fs::create_dir_all(mono.join("Game_Data/Managed")).unwrap();
        fs::write(mono.join("UnityPlayer.dll"), pe(0x8664)).unwrap();
        assert_eq!(detect(&mono), Some(UnityInfo { il2cpp: false, x64: true, loader: String::new() }));
        fs::create_dir_all(mono.join("BepInEx/plugins")).unwrap();
        assert_eq!(detect(&mono).unwrap().loader, "bepinex");

        let il = d.join("Il Game");
        fs::create_dir_all(il.join("Il_Data")).unwrap();
        fs::write(il.join("Il_Data/globalgamemanagers"), "x").unwrap();
        fs::write(il.join("UnityPlayer.dll"), pe(0x014c)).unwrap();
        fs::write(il.join("GameAssembly.dll"), "x").unwrap();
        fs::create_dir_all(il.join("MelonLoader")).unwrap();
        assert_eq!(detect(&il), Some(UnityInfo { il2cpp: true, x64: false, loader: "melonloader".into() }));

        let other = d.join("Other");
        fs::create_dir_all(other.join("Data")).unwrap();
        assert_eq!(detect(&other), None);
    }

    const RELEASE: &str = r#"{"tag_name":"v5.4.23.5","assets":[
      {"name":"BepInEx_linux_x64_5.4.23.5.zip","browser_download_url":"https://github.com/BepInEx/BepInEx/releases/download/v5.4.23.5/BepInEx_linux_x64_5.4.23.5.zip"},
      {"name":"BepInEx_win_x64_5.4.23.5.zip","browser_download_url":"https://github.com/BepInEx/BepInEx/releases/download/v5.4.23.5/BepInEx_win_x64_5.4.23.5.zip"},
      {"name":"BepInEx_win_x86_5.4.23.5.zip","browser_download_url":"https://github.com/BepInEx/BepInEx/releases/download/v5.4.23.5/BepInEx_win_x86_5.4.23.5.zip"}]}"#;

    #[test]
    fn the_right_windows_build_is_picked_from_the_real_release_list() {
        let v: Value = serde_json::from_str(RELEASE).unwrap();
        let allowed = Sources::default().allowed;
        let a = pick_release_asset(&v, &["BepInEx_win_x64_".into()], &allowed).unwrap();
        assert_eq!((a.name.as_str(), a.version.as_str()), ("BepInEx_win_x64_5.4.23.5.zip", "5.4.23.5"));
        assert!(pick_release_asset(&v, &["BepInEx_win_x86_".into()], &allowed).unwrap().name.contains("x86"));
        assert!(pick_release_asset(&v, &["BepInEx_win_x64_".into()], &["https://elsewhere.example/".into()]).is_none(), "only official download areas");
    }

    #[test]
    fn the_newest_bepinex_6_build_of_the_right_kind_is_found_on_the_build_server_page() {
        // as listed on builds.bepinex.dev, newest first
        let html = r#"<a href="/projects/bepinex_be/789/BepInEx-Unity.Mono-win-x64-6.0.0-be.789%2B6cea286.zip">m</a>
          <a href="/projects/bepinex_be/789/BepInEx-Unity.IL2CPP-win-x86-6.0.0-be.789%2B6cea286.zip">i86</a>
          <a href="/projects/bepinex_be/789/BepInEx-Unity.IL2CPP-win-x64-6.0.0-be.789%2B6cea286.zip">i64</a>
          <a href="/projects/bepinex_be/788/BepInEx-Unity.IL2CPP-win-x64-6.0.0-be.788%2B5b766a3.zip">old</a>
          <a href="/projects/bepinex_be/789/BepInEx-NET.CoreCLR-net6.0-win-x64-6.0.0-be.789%2B6cea286.zip">net</a>"#;
        let a = pick_be_build(html, "https://builds.bepinex.dev", true, true).unwrap();
        assert_eq!(a.name, "BepInEx-Unity.IL2CPP-win-x64-6.0.0-be.789+6cea286.zip");
        assert_eq!(a.url, "https://builds.bepinex.dev/projects/bepinex_be/789/BepInEx-Unity.IL2CPP-win-x64-6.0.0-be.789%2B6cea286.zip");
        assert_eq!(a.version, "6.0.0-be.789");
        assert!(pick_be_build(html, "https://builds.bepinex.dev", false, false).is_none(), "no 32-bit Mono build listed");
        assert!(name_ok(&a.name));
    }
}
