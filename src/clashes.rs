//! Conflicts that two mods have without sharing a file name, so the file-level conflict list cannot see them:
//!
//! - **Cyberpunk 2077 archives.** Every `.archive` in `archive/pc/mod` carries an index of the game resources it
//!   replaces (64-bit hashes of their paths). The game loads the archives in ASCII order of their file names and the
//!   first one to bring a resource wins. Two archives from different mods that bring the same resource are a real
//!   conflict even though their names differ. An `archive/pc/mod/modlist.txt` sets another order; then Gantry names
//!   the clash but not the winner.
//! - **Redscript.** Two mods that declare the same class (in the same module), or replace or add the same method, make
//!   the script compiler fail or one of them silently lose.
//!
//! Only reads files; nothing here changes anything.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

/// A file of an enabled mod: the mod's name, where it lands in the game (relative, `/`), and where it is on disk.
pub struct ModFile {
    pub mod_name: String,
    pub dest: String,
    pub src: PathBuf,
}

/// The resource hashes in an RDAR archive index (`None` when the file is not one, or is damaged).
pub fn archive_resources(path: &Path) -> Option<Vec<u64>> {
    let mut f = File::open(path).ok()?;
    let mut head = [0u8; 40];
    f.read_exact(&mut head).ok()?;
    if &head[0..4] != b"RDAR" {
        return None;
    }
    let u32_at = |b: &[u8], i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
    let u64_at = |b: &[u8], i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
    let (index_pos, index_size) = (u64_at(&head, 8), u32_at(&head, 16) as usize);
    let len = f.metadata().ok()?.len();
    // an index is a few KB to a few MB; anything past the file or absurdly big is a damaged file
    if !(28..=64 << 20).contains(&index_size) || index_pos.checked_add(index_size as u64)? > len {
        return None;
    }
    f.seek(SeekFrom::Start(index_pos)).ok()?;
    let mut idx = vec![0u8; index_size];
    f.read_exact(&mut idx).ok()?;
    let (entries, segments, deps) = (u32_at(&idx, 16) as usize, u32_at(&idx, 20) as usize, u32_at(&idx, 24) as usize);
    if 28usize.checked_add(entries.checked_mul(56)?)?.checked_add(segments.checked_mul(16)?)?.checked_add(deps.checked_mul(8)?)? != index_size {
        return None;
    }
    Some((0..entries).map(|i| u64_at(&idx, 28 + 56 * i)).collect())
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ArchiveClash {
    /// The archive that loads first (it wins), unless `ordered` is false.
    pub winner: String,
    pub winner_mod: String,
    pub loser: String,
    pub loser_mod: String,
    /// How many game resources both bring.
    pub resources: usize,
    /// The ASCII order decides; false when a `modlist.txt` sets another order.
    pub ordered: bool,
}

fn is_mod_archive(dest: &str) -> Option<&str> {
    let (dir, name) = dest.rsplit_once('/')?;
    (dir.eq_ignore_ascii_case("archive/pc/mod") && name.to_ascii_lowercase().ends_with(".archive")).then_some(name)
}

/// Archive conflicts between different mods, the biggest first. `has_modlist`: the game has `archive/pc/mod/modlist.txt`.
pub fn archive_clashes(files: &[ModFile], has_modlist: bool) -> Vec<ArchiveClash> {
    let mut archives: Vec<(&str, &ModFile)> = files.iter().filter_map(|f| is_mod_archive(&f.dest).map(|n| (n, f))).collect();
    // the order the game loads them in: plain byte order of the names
    archives.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut owner: HashMap<u64, usize> = HashMap::new();
    let mut pairs: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    for (i, (_, f)) in archives.iter().enumerate() {
        let Some(hashes) = archive_resources(&f.src) else { continue };
        for h in hashes {
            match owner.get(&h) {
                Some(&w) if archives[w].1.mod_name != f.mod_name => *pairs.entry((w, i)).or_default() += 1,
                Some(_) => {}
                None => {
                    owner.insert(h, i);
                }
            }
        }
    }
    let mut out: Vec<ArchiveClash> = pairs
        .into_iter()
        .map(|((w, l), n)| ArchiveClash {
            winner: archives[w].0.to_string(),
            winner_mod: archives[w].1.mod_name.clone(),
            loser: archives[l].0.to_string(),
            loser_mod: archives[l].1.mod_name.clone(),
            resources: n,
            ordered: !has_modlist,
        })
        .collect();
    out.sort_by(|a, b| b.resources.cmp(&a.resources).then_with(|| a.winner.cmp(&b.winner)));
    out
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ScriptClash {
    /// "class" (declared twice), "replace" (two mods replace the same method) or "add" (two mods add the same method).
    pub kind: &'static str,
    /// The class (with its module) or `Class.method`.
    pub what: String,
    pub mods: Vec<String>,
}

static MODULE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\s*module\s+([A-Za-z_][\w.]*)").unwrap());
static DECL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\s*(?:(?:public|private|protected|abstract|final|native|importonly)\s+)*(?:class|struct|enum)\s+([A-Za-z_]\w*)").unwrap());
static ANNOTATED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"@(replaceMethod|addMethod)\(\s*([A-Za-z_][\w.]*)\s*\)[^@]*?\bfunc\s+([A-Za-z_]\w*)").unwrap());
static COMMENTS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)/\*.*?\*/|//[^\n]*").unwrap());

/// What one script file declares: `(kind, key)`.
fn script_keys(text: &str) -> Vec<(&'static str, String)> {
    let text = COMMENTS.replace_all(text, "");
    let module = MODULE.captures(&text).map(|c| format!("{}.", &c[1])).unwrap_or_default();
    let mut out: Vec<(&'static str, String)> = DECL.captures_iter(&text).map(|c| ("class", format!("{module}{}", &c[1]))).collect();
    for c in ANNOTATED.captures_iter(&text) {
        out.push((if &c[1] == "replaceMethod" { "replace" } else { "add" }, format!("{}.{}", &c[2], &c[3])));
    }
    out
}

/// Redscript declarations that two or more enabled mods both make.
pub fn script_clashes(files: &[ModFile]) -> Vec<ScriptClash> {
    let mut seen: BTreeMap<(&'static str, String), Vec<String>> = BTreeMap::new();
    let mut read = 0u64;
    for f in files.iter().filter(|f| f.dest.to_ascii_lowercase().ends_with(".reds")) {
        // scripts are small; a cap keeps a strange library from being read for minutes
        let Ok(meta) = std::fs::metadata(&f.src) else { continue };
        if meta.len() > 2 << 20 || read > 64 << 20 {
            continue;
        }
        read += meta.len();
        let Ok(text) = std::fs::read_to_string(&f.src) else { continue };
        for key in script_keys(&text) {
            let mods = seen.entry(key).or_default();
            if !mods.contains(&f.mod_name) {
                mods.push(f.mod_name.clone());
            }
        }
    }
    seen.into_iter().filter(|(_, m)| m.len() > 1).map(|((kind, what), mods)| ScriptClash { kind, what, mods }).collect()
}

#[cfg(test)]
pub mod testutil {
    /// A minimal RDAR archive whose index lists these resource hashes.
    pub fn archive(hashes: &[u64]) -> Vec<u8> {
        let index_pos = 40u64;
        let index_size = 28 + 56 * hashes.len();
        let mut out = Vec::new();
        out.extend_from_slice(b"RDAR");
        out.extend_from_slice(&12u32.to_le_bytes());
        out.extend_from_slice(&index_pos.to_le_bytes());
        out.extend_from_slice(&(index_size as u32).to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&((40 + index_size) as u64).to_le_bytes());
        out.extend_from_slice(&8u32.to_le_bytes());
        out.extend_from_slice(&((56 * hashes.len()) as u32).to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&(hashes.len() as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        for h in hashes {
            out.extend_from_slice(&h.to_le_bytes());
            out.extend_from_slice(&[0u8; 48]);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsutil::testutil::TempDir;
    use std::fs;

    fn file(d: &Path, mod_name: &str, dest: &str, bytes: &[u8]) -> ModFile {
        let src = d.join(mod_name).join(dest);
        fs::create_dir_all(src.parent().unwrap()).unwrap();
        fs::write(&src, bytes).unwrap();
        ModFile { mod_name: mod_name.into(), dest: dest.into(), src }
    }

    #[test]
    fn archives_that_bring_the_same_resource_clash_and_the_first_in_byte_order_wins() {
        let d = TempDir::new("clash-archives");
        let files = vec![
            file(&d.join(""), "Panam Hangout", "archive/pc/mod/#Stay_Panam.archive", &testutil::archive(&[1, 2, 3])),
            file(&d.join(""), "Judy Hangout", "archive/pc/mod/#Stay_Judy.archive", &testutil::archive(&[2, 3, 4])),
            file(&d.join(""), "Judy Hangout", "archive/pc/mod/zz_Judy_extra.archive", &testutil::archive(&[4])),
            file(&d.join(""), "Lonely", "archive/pc/mod/Lonely.archive", &testutil::archive(&[9])),
            file(&d.join(""), "Broken", "archive/pc/mod/Broken.archive", b"RDAR garbage"),
            file(&d.join(""), "Elsewhere", "archive/pc/content/other.archive", &testutil::archive(&[1])),
        ];
        let c = archive_clashes(&files, false);
        assert_eq!(c.len(), 1, "{c:?}");
        // '#Stay_J' sorts before '#Stay_P': Judy loads first and wins both shared resources
        assert_eq!((c[0].winner.as_str(), c[0].loser_mod.as_str(), c[0].resources, c[0].ordered), ("#Stay_Judy.archive", "Panam Hangout", 2, true));
        // a mod's own archives never count against each other; a modlist.txt leaves the order open
        assert!(!archive_clashes(&files, true)[0].ordered);
        assert_eq!(archive_resources(&files[0].src), Some(vec![1, 2, 3]));
        assert_eq!(archive_resources(&files[4].src), None);
    }

    #[test]
    fn two_mods_declaring_or_replacing_the_same_script_thing_are_found() {
        let d = TempDir::new("clash-scripts");
        let files = vec![
            file(&d.join(""), "A", "r6/scripts/a/a.reds", b"module Shared.Util\npublic class Helper {}\n@replaceMethod(PlayerPuppet)\nprotected cb func OnGameAttached() -> Bool { return true; }\n// class Commented {}\n"),
            file(&d.join(""), "B", "r6/scripts/b/b.reds", b"module Shared.Util\nclass Helper extends IScriptable {}\n@replaceMethod(PlayerPuppet) protected cb func OnGameAttached() -> Bool { return false; }\n/* class Commented {} */\n@wrapMethod(PlayerPuppet)\nfunc OnDetach() {}\n"),
            file(&d.join(""), "C", "r6/scripts/c/c.reds", b"module Other\nclass Helper {}\n@wrapMethod(PlayerPuppet)\nfunc OnDetach() {}\n@addMethod(GameObject)\npublic func Ping() {}\n"),
            file(&d.join(""), "D", "r6/scripts/d.reds", b"@addMethod(GameObject)\npublic func Ping() {}\n"),
        ];
        let c = script_clashes(&files);
        let got: Vec<(&str, &str, Vec<&str>)> = c.iter().map(|x| (x.kind, x.what.as_str(), x.mods.iter().map(String::as_str).collect())).collect();
        assert_eq!(
            got,
            vec![("add", "GameObject.Ping", vec!["C", "D"]), ("class", "Shared.Util.Helper", vec!["A", "B"]), ("replace", "PlayerPuppet.OnGameAttached", vec!["A", "B"])],
            "the same name in another module, wrapped methods and comments do not count"
        );
    }
}
