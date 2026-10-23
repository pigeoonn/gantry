//! Mods published as GitHub releases: paste a repository link, pick a file of its newest
//! release, and it is installed like any other mod. Downloads are only taken from the
//! repository's own release area.

use serde_json::Value;
use std::process::{Command, Stdio};

pub const API: &str = "https://api.github.com";

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Release {
    pub owner: String,
    pub repo: String,
    pub tag: String,
    pub title: String,
    pub assets: Vec<Asset>,
}

fn part_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 100 && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) && !s.starts_with('.')
}

/// `owner/repo` out of a link or the short form.
pub fn parse_repo(input: &str) -> Option<(String, String)> {
    // a link copied from a page often ends in "#readme" or "?tab=readme-ov-file"
    let t = input.trim().split(['#', '?']).next().unwrap_or("").trim_end_matches('/');
    let t = t.strip_prefix("https://").or_else(|| t.strip_prefix("http://")).unwrap_or(t);
    let t = t.strip_prefix("www.").unwrap_or(t);
    let path = match t.strip_prefix("github.com/") {
        Some(p) => p,
        None if !t.contains('.') || t.matches('/').count() == 1 && !t.contains("://") && !t.split('/').next()?.contains('.') => t,
        None => return None,
    };
    let mut it = path.split('/');
    let (o, r) = (it.next()?, it.next()?);
    let r = r.strip_suffix(".git").unwrap_or(r);
    (part_ok(o) && part_ok(r)).then(|| (o.to_string(), r.to_string()))
}

/// Files of a release that are mods or contain mods.
pub fn mod_files(assets: &[Asset]) -> Vec<Asset> {
    let ok = |n: &str| {
        let l = n.to_lowercase();
        [".zip", ".7z", ".rar", ".dll", ".pak"].iter().any(|e| l.ends_with(e)) && !l.contains("source") && !l.contains("linux") && !l.contains("macos") && !l.contains("-osx")
    };
    assets.iter().filter(|a| ok(&a.name)).cloned().collect()
}

pub fn parse_release(v: &Value, owner: &str, repo: &str, allowed: Option<&str>) -> Option<Release> {
    let prefix = allowed.map(str::to_string).unwrap_or_else(|| format!("https://github.com/{owner}/{repo}/releases/download/"));
    let assets = v
        .get("assets")?
        .as_array()?
        .iter()
        .filter_map(|a| {
            let url = a.get("browser_download_url")?.as_str()?.to_string();
            let name = a.get("name")?.as_str()?.to_string();
            (url.starts_with(&prefix) && !name.contains('/') && !name.contains("..")).then(|| Asset { name, url, size: a.get("size").and_then(Value::as_u64).unwrap_or(0) })
        })
        .collect();
    Some(Release { owner: owner.into(), repo: repo.into(), tag: v.get("tag_name").and_then(Value::as_str).unwrap_or("").to_string(), title: v.get("name").and_then(Value::as_str).unwrap_or("").to_string(), assets })
}

/// Is `url` a download from the release area of `owner/repo`?
pub fn download_ok(url: &str, owner: &str, repo: &str, allow_prefix: Option<&str>) -> bool {
    let std_prefix = format!("https://github.com/{owner}/{repo}/releases/download/");
    url.starts_with(&std_prefix) || allow_prefix.map(|p| url.starts_with(p)).unwrap_or(false)
}

/// What GitHub's own error text means for the person, if it is an error reply.
fn explain(v: &Value) -> Option<String> {
    let m = v.get("message")?.as_str()?.to_lowercase();
    if m.contains("rate limit") {
        Some("GitHub is limiting requests from this connection; try again in a while.".into())
    } else if m.contains("not found") {
        Some("GitHub does not know that repository (it may be private or the name is wrong).".into())
    } else {
        None
    }
}

fn get(url: &str) -> Result<Value, String> {
    let out = Command::new("curl")
        .args(["-sSL", "--max-time", "30", "-H", "Accept: application/vnd.github+json", "-H", "User-Agent: Gantry"])
        .arg(url)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("Could not run curl ({e})."))?;
    serde_json::from_slice(&out.stdout).map_err(|_| "Could not reach GitHub.".to_string())
}

/// The newest release of a repository (pre-releases count when there is no full release).
pub fn latest(owner: &str, repo: &str, api: &str, allowed: Option<&str>) -> Result<Release, String> {
    let v = get(&format!("{api}/repos/{owner}/{repo}/releases/latest"))?;
    if v.get("tag_name").is_none() {
        // "no latest release" is a 404 too, so only a rate limit ends it here
        if let Some(e) = explain(&v).filter(|e| e.contains("limiting")) {
            return Err(e);
        }
        let list = get(&format!("{api}/repos/{owner}/{repo}/releases"))?;
        if let Some(e) = explain(&list) {
            return Err(e);
        }
        let first = list.as_array().and_then(|a| a.first()).cloned();
        return first.and_then(|f| parse_release(&f, owner, repo, allowed)).ok_or_else(|| "That repository has no releases.".to_string());
    }
    parse_release(&v, owner, repo, allowed).ok_or_else(|| "That repository has no releases.".to_string())
}

#[cfg(test)]
pub mod fake {
    /// A GitHub API stand-in: one repository with one release and its files.
    pub fn start(owner: &str, repo: &str, files: Vec<(&str, Vec<u8>)>) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        start_tagged(owner, repo, "v1", files)
    }

    pub fn start_tagged(owner: &str, repo: &str, tag: &str, files: Vec<(&str, Vec<u8>)>) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let base = format!("http://{}", server.server_addr().to_ip().unwrap());
        let hits = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (h2, b2, o, r, tag) = (hits.clone(), base.clone(), owner.to_string(), repo.to_string(), tag.to_string());
        let files: Vec<(String, Vec<u8>)> = files.into_iter().map(|(n, d)| (n.to_string(), d)).collect();
        std::thread::spawn(move || {
            for req in server.incoming_requests() {
                let url = req.url().to_string();
                h2.lock().unwrap().push(url.clone());
                let body = if url == format!("/repos/{o}/{r}/releases/latest") {
                    let assets: Vec<String> = files.iter().map(|(n, d)| format!(r#"{{"name":"{n}","size":{},"browser_download_url":"{b2}/dl/{n}"}}"#, d.len())).collect();
                    format!(r#"{{"tag_name":"{tag}","name":"First","html_url":"https://github.com/{o}/{r}/releases/tag/{tag}","assets":[{}]}}"#, assets.join(",")).into_bytes()
                } else if let Some(f) = files.iter().find(|(n, _)| url == format!("/dl/{n}")) {
                    f.1.clone()
                } else {
                    let _ = req.respond(tiny_http::Response::from_data(Vec::new()).with_status_code(404));
                    continue;
                };
                let _ = req.respond(tiny_http::Response::from_data(body));
            }
        });
        (base, hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repositories_are_understood_from_links_and_the_short_form() {
        let want = Some(("owner".to_string(), "Cool-Mod".to_string()));
        for input in ["https://github.com/owner/Cool-Mod", "https://github.com/owner/Cool-Mod/", "github.com/owner/Cool-Mod.git", "https://github.com/owner/Cool-Mod/releases/tag/v1", "owner/Cool-Mod", " https://www.github.com/owner/Cool-Mod "] {
            assert_eq!(parse_repo(input), want, "{input}");
        }
        for input in ["https://github.com/owner/Cool-Mod#readme", "https://github.com/owner/Cool-Mod?tab=readme-ov-file", "https://github.com/owner/Cool-Mod/releases?q=1#top"] {
            assert_eq!(parse_repo(input), want, "{input}");
        }
        for bad in ["", "owner", "https://evil.example/owner/repo", "https://github.com/owner", "https://github.com/../x", "a b/c", "https://github.com/ow ner/repo", "x.y/z"] {
            assert_eq!(parse_repo(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn only_mod_files_of_the_repositorys_own_release_area_are_offered() {
        let v = serde_json::json!({"tag_name":"v2.1","name":"Big update","assets":[
            {"name":"CoolMod-2.1.zip","size":10,"browser_download_url":"https://github.com/owner/repo/releases/download/v2.1/CoolMod-2.1.zip"},
            {"name":"CoolMod-linux.tar.gz","size":10,"browser_download_url":"https://github.com/owner/repo/releases/download/v2.1/CoolMod-linux.tar.gz"},
            {"name":"source.zip","size":10,"browser_download_url":"https://github.com/owner/repo/releases/download/v2.1/source.zip"},
            {"name":"plugin.dll","size":10,"browser_download_url":"https://github.com/owner/repo/releases/download/v2.1/plugin.dll"},
            {"name":"sneaky.zip","size":10,"browser_download_url":"https://evil.example/sneaky.zip"}]});
        let rel = parse_release(&v, "owner", "repo", None).unwrap();
        assert_eq!((rel.tag.as_str(), rel.assets.len()), ("v2.1", 4), "the foreign address is dropped");
        let names: Vec<String> = mod_files(&rel.assets).into_iter().map(|a| a.name).collect();
        assert_eq!(names, vec!["CoolMod-2.1.zip", "plugin.dll"]);
        assert!(download_ok("https://github.com/owner/repo/releases/download/v2.1/x.zip", "owner", "repo", None));
        assert!(!download_ok("https://github.com/owner/other/releases/download/v2.1/x.zip", "owner", "repo", None));
        assert!(!download_ok("https://evil.example/x.zip", "owner", "repo", None));
    }
}
