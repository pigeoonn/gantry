//! Unreal `.pak` index reader and mod-conflict finder.
//!
//! Only the file *index* of a PAK is read (never the payload), so scanning is quick even for
//! multi-GB archives. Supported: legacy indexes (versions 3 to 9) and path-hash indexes with a
//! full directory listing (versions 10 and up, which covers UE 5.x). Anything unreadable
//! (encrypted index, unknown layout, damage) is reported, never guessed at.

use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const MAGIC: [u8; 4] = [0xE1, 0x12, 0x6F, 0x5A];
const MAX_INDEX: u64 = 256 * 1024 * 1024;
const MAX_ENTRIES: usize = 4_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PakInfo {
    pub version: u32,
    /// Normalised (lower-case, forward slashes, no leading `../`) file paths inside the PAK.
    pub paths: Vec<String>,
}

struct Cur<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.p.checked_add(n).filter(|e| *e <= self.b.len()).ok_or("index is truncated")?;
        let s = &self.b[self.p..end];
        self.p = end;
        Ok(s)
    }
    fn i32(&mut self) -> Result<i32, String> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i64(&mut self) -> Result<i64, String> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    /// Unreal `FString`: i32 length including the terminator; negative = UTF-16.
    fn fstring(&mut self) -> Result<String, String> {
        let n = self.i32()?;
        if n == 0 {
            return Ok(String::new());
        }
        if n.unsigned_abs() as usize > 1 << 20 {
            return Err("implausible string length".into());
        }
        if n < 0 {
            let raw = self.take(n.unsigned_abs() as usize * 2)?;
            let units: Vec<u16> = raw.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
            Ok(String::from_utf16_lossy(&units).trim_end_matches('\0').to_string())
        } else {
            Ok(String::from_utf8_lossy(self.take(n as usize)?).trim_end_matches('\0').to_string())
        }
    }
}

/// `../../../Game/Content/A.uasset` -> `game/content/a.uasset`
pub fn normalise(path: &str) -> String {
    let p = path.replace('\\', "/").to_lowercase();
    let mut s = p.as_str();
    while let Some(rest) = s.strip_prefix("../") {
        s = rest;
    }
    s.trim_start_matches('/').to_string()
}

fn read_range(f: &mut File, len: u64, off: u64, size: u64) -> Result<Vec<u8>, String> {
    if size > MAX_INDEX || off.checked_add(size).map(|e| e > len).unwrap_or(true) {
        return Err("index lies outside the file".into());
    }
    f.seek(SeekFrom::Start(off)).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; size as usize];
    f.read_exact(&mut buf).map_err(|e| e.to_string())?;
    Ok(buf)
}

pub fn read_pak(path: &Path) -> Result<PakInfo, String> {
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    let len = f.metadata().map_err(|e| e.to_string())?.len();
    if len < 44 {
        return Err("file is too small to be a PAK".into());
    }
    let tail_len = len.min(1024);
    let tail = read_range(&mut f, len, len - tail_len, tail_len)?;
    let at = tail.windows(4).rposition(|w| w == MAGIC).ok_or("not an Unreal PAK (no footer)")?;
    let mut c = Cur { b: &tail, p: at + 4 };
    let version = c.u32()?;
    let (index_off, index_size) = (c.i64()?, c.i64()?);
    if index_off < 0 || index_size < 0 {
        return Err("corrupt footer".into());
    }
    if version >= 4 && at >= 1 && tail[at - 1] != 0 {
        return Err("the file index is encrypted".into());
    }
    let index = read_range(&mut f, len, index_off as u64, index_size as u64)?;
    let paths = match version {
        3..=9 => legacy_paths(&index, version)?,
        10..=64 => directory_paths(&index, &mut f, len)?,
        v => return Err(format!("unsupported PAK version {}", v)),
    };
    Ok(PakInfo { version, paths })
}

/// Versions 3-9: the index lists `name + FPakEntry` records one after another.
fn legacy_paths(index: &[u8], version: u32) -> Result<Vec<String>, String> {
    let mut c = Cur { b: index, p: 0 };
    let mount = c.fstring()?;
    let count = c.i32()?;
    if count < 0 || count as usize > MAX_ENTRIES {
        return Err("implausible entry count".into());
    }
    let mut out = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let name = c.fstring()?;
        c.take(24)?; // offset, size, uncompressed size
        let compression = c.u32()?; // an i32 before v8; same width
        if version <= 1 {
            c.take(8)?; // timestamp
        }
        c.take(20)?; // sha1
        if compression != 0 {
            let blocks = c.i32()?;
            if !(0..=1 << 20).contains(&blocks) {
                return Err("implausible compression block count".into());
            }
            c.take(blocks as usize * 16)?;
        }
        c.take(1)?; // encrypted flag
        c.take(4)?; // compression block size
        out.push(normalise(&format!("{}{}", mount, name)));
    }
    Ok(out)
}

/// Versions 10+: a primary index points at a full directory index holding every path.
fn directory_paths(index: &[u8], f: &mut File, len: u64) -> Result<Vec<String>, String> {
    let mut c = Cur { b: index, p: 0 };
    let mount = c.fstring()?;
    let entries = c.i32()?;
    if entries < 0 || entries as usize > MAX_ENTRIES {
        return Err("implausible entry count".into());
    }
    c.take(8)?; // path hash seed
    if c.i32()? != 0 {
        c.take(8 + 8 + 20)?; // path hash index (offset, size, hash): not needed
    }
    if c.i32()? == 0 {
        return Err("this PAK was built without a directory index".into());
    }
    let (off, size) = (c.i64()?, c.i64()?);
    if off < 0 || size < 0 {
        return Err("corrupt directory index pointer".into());
    }
    let dir = read_range(f, len, off as u64, size as u64)?;
    let mut d = Cur { b: &dir, p: 0 };
    let dirs = d.i32()?;
    if dirs < 0 || dirs as usize > MAX_ENTRIES {
        return Err("implausible directory count".into());
    }
    let mut out = Vec::new();
    for _ in 0..dirs {
        let dname = d.fstring()?;
        let files = d.i32()?;
        if files < 0 || out.len() + files as usize > MAX_ENTRIES {
            return Err("implausible file count".into());
        }
        for _ in 0..files {
            let fname = d.fstring()?;
            d.take(4)?; // encoded entry offset
            out.push(normalise(&format!("{}{}{}", mount, dname, fname)));
        }
    }
    Ok(out)
}

// ───────────────────────────── conflicts ─────────────────────────────

#[derive(Clone, Debug)]
pub struct PakMod {
    pub name: String,
    pub path: std::path::PathBuf,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Conflict {
    pub a: String,
    pub b: String,
    pub shared: usize,
    pub samples: Vec<String>,
    /// The mod whose copy of the shared files the game will most likely use.
    pub likely_winner: String,
}

#[derive(Clone, Debug, Serialize, Default, PartialEq, Eq)]
pub struct ConflictReport {
    pub scanned: usize,
    pub unreadable: Vec<String>,
    pub conflicts: Vec<Conflict>,
}

/// Unreal mounts patch PAKs (`*_P`) after ordinary ones and, among equals, later names win.
fn load_rank(name: &str) -> (u8, String) {
    let n = name.trim_start_matches("[Logic] ").to_lowercase();
    (u8::from(n.ends_with("_p")), n)
}

pub fn find_conflicts(mods: &[PakMod]) -> ConflictReport {
    let mut report = ConflictReport::default();
    let mut owners: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, m) in mods.iter().enumerate() {
        match read_pak(&m.path) {
            Ok(info) => {
                report.scanned += 1;
                for p in info.paths {
                    owners.entry(p).or_default().push(i);
                }
            }
            Err(e) => report.unreadable.push(format!("{}: {}", m.name, e)),
        }
    }
    let mut pairs: BTreeMap<(usize, usize), Vec<String>> = BTreeMap::new();
    for (path, who) in owners.iter().filter(|(_, w)| w.len() > 1) {
        let mut who = who.clone();
        who.sort_unstable();
        who.dedup();
        for (x, a) in who.iter().enumerate() {
            for b in &who[x + 1..] {
                pairs.entry((*a, *b)).or_default().push(path.clone());
            }
        }
    }
    for ((a, b), mut paths) in pairs {
        paths.sort();
        let winner = if load_rank(&mods[a].name) >= load_rank(&mods[b].name) { a } else { b };
        report.conflicts.push(Conflict {
            a: mods[a].name.clone(),
            b: mods[b].name.clone(),
            shared: paths.len(),
            samples: paths.iter().take(5).cloned().collect(),
            likely_winner: mods[winner].name.clone(),
        });
    }
    report.conflicts.sort_by(|x, y| y.shared.cmp(&x.shared).then_with(|| x.a.cmp(&y.a)));
    report
}

#[cfg(test)]
pub mod builder {
    //! Fabricates PAK files in the real on-disk layouts (v8 legacy index, v11 directory index).
    use super::*;

    fn fstr(out: &mut Vec<u8>, s: &str) {
        if s.is_empty() {
            out.extend_from_slice(&0i32.to_le_bytes());
        } else if s.is_ascii() {
            out.extend_from_slice(&((s.len() + 1) as i32).to_le_bytes());
            out.extend_from_slice(s.as_bytes());
            out.push(0);
        } else {
            let u: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
            out.extend_from_slice(&(-(u.len() as i32)).to_le_bytes());
            for x in u {
                out.extend_from_slice(&x.to_le_bytes());
            }
        }
    }

    fn footer(out: &mut Vec<u8>, version: u32, idx_off: u64, idx_size: u64, encrypted: bool, comp_names: usize) {
        out.extend_from_slice(&[0u8; 16]); // key guid
        out.push(u8::from(encrypted));
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&version.to_le_bytes());
        out.extend_from_slice(&idx_off.to_le_bytes());
        out.extend_from_slice(&idx_size.to_le_bytes());
        out.extend_from_slice(&[0u8; 20]);
        out.extend(std::iter::repeat_n(0u8, comp_names * 32));
    }

    /// Version 11: primary index + full directory index. `files` are `dir/file` strings under `mount`.
    pub fn v11(mount: &str, files: &[&str], encrypted: bool) -> Vec<u8> {
        let mut dirs: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for f in files {
            let (d, n) = f.rsplit_once('/').map(|(d, n)| (format!("{}/", d), n.to_string())).unwrap_or((String::new(), f.to_string()));
            dirs.entry(d).or_default().push(n);
        }
        let mut fd = Vec::new();
        fd.extend_from_slice(&(dirs.len() as i32).to_le_bytes());
        for (d, ns) in &dirs {
            fstr(&mut fd, d);
            fd.extend_from_slice(&(ns.len() as i32).to_le_bytes());
            for n in ns {
                fstr(&mut fd, n);
                fd.extend_from_slice(&0i32.to_le_bytes());
            }
        }
        let mut file = vec![0xEEu8; 777]; // stand-in payload
        let fd_off = file.len() as u64;
        file.extend_from_slice(&fd);
        let mut pi = Vec::new();
        fstr(&mut pi, mount);
        pi.extend_from_slice(&(files.len() as i32).to_le_bytes());
        pi.extend_from_slice(&0u64.to_le_bytes());
        pi.extend_from_slice(&0i32.to_le_bytes()); // no path hash index
        pi.extend_from_slice(&1i32.to_le_bytes());
        pi.extend_from_slice(&(fd_off as i64).to_le_bytes());
        pi.extend_from_slice(&(fd.len() as i64).to_le_bytes());
        pi.extend_from_slice(&[0u8; 20]);
        pi.extend_from_slice(&0i32.to_le_bytes()); // encoded entries blob size
        pi.extend_from_slice(&0i32.to_le_bytes()); // non-encoded entries
        let pi_off = file.len() as u64;
        file.extend_from_slice(&pi);
        footer(&mut file, 11, pi_off, pi.len() as u64, encrypted, 5);
        file
    }

    /// Version 8: one `name + FPakEntry` record per file (every other entry is "compressed").
    pub fn v8(mount: &str, files: &[&str]) -> Vec<u8> {
        let mut file = vec![0xDDu8; 300];
        let mut idx = Vec::new();
        fstr(&mut idx, mount);
        idx.extend_from_slice(&(files.len() as i32).to_le_bytes());
        for (i, f) in files.iter().enumerate() {
            fstr(&mut idx, f);
            idx.extend_from_slice(&[0u8; 24]);
            let compressed = i % 2 == 1;
            idx.extend_from_slice(&u32::from(compressed).to_le_bytes());
            idx.extend_from_slice(&[0u8; 20]);
            if compressed {
                idx.extend_from_slice(&2i32.to_le_bytes());
                idx.extend_from_slice(&[0u8; 32]);
            }
            idx.push(0);
            idx.extend_from_slice(&65536u32.to_le_bytes());
        }
        let off = file.len() as u64;
        file.extend_from_slice(&idx);
        footer(&mut file, 8, off, idx.len() as u64, false, 4);
        file
    }
}

#[cfg(test)]
mod tests {
    use super::builder::*;
    use super::*;
    use crate::fsutil::testutil::TempDir;
    use std::fs;

    #[test]
    fn reads_directory_indexes_including_utf16_names() {
        let d = TempDir::new("pak1");
        let f = d.join("a.pak");
        fs::write(&f, v11("../../../Game/", &["Content/Skin/Hero.uasset", "Content/Skin/Hero.uexp", "Config/Gîn.ini"], false)).unwrap();
        let p = read_pak(&f).unwrap();
        assert_eq!(p.version, 11);
        let mut got = p.paths.clone();
        got.sort();
        assert_eq!(got, vec!["game/config/gîn.ini", "game/content/skin/hero.uasset", "game/content/skin/hero.uexp"]);
    }

    #[test]
    fn reads_legacy_indexes_with_compression_blocks() {
        let d = TempDir::new("pak2");
        let f = d.join("old.pak");
        fs::write(&f, v8("../../../Game/", &["Content/A.uasset", "Content/B.uasset", "Content/C.uasset"])).unwrap();
        let p = read_pak(&f).unwrap();
        assert_eq!(p.version, 8);
        assert_eq!(p.paths, vec!["game/content/a.uasset", "game/content/b.uasset", "game/content/c.uasset"]);
    }

    #[test]
    fn refuses_what_it_cannot_read_instead_of_guessing() {
        let d = TempDir::new("pak3");
        let enc = d.join("enc.pak");
        fs::write(&enc, v11("../../../G/", &["a/b.x"], true)).unwrap();
        assert!(read_pak(&enc).unwrap_err().contains("encrypted"));
        let junk = d.join("junk.pak");
        fs::write(&junk, vec![7u8; 5000]).unwrap();
        assert!(read_pak(&junk).unwrap_err().contains("not an Unreal PAK"));
        fs::write(d.join("tiny.pak"), b"x").unwrap();
        assert!(read_pak(&d.join("tiny.pak")).is_err());
        assert!(read_pak(&d.join("missing.pak")).is_err());
        // a future version number is reported, not mis-parsed
        let mut v = v11("../../../G/", &["a/b.x"], false);
        let at = v.windows(4).rposition(|w| w == MAGIC).unwrap();
        v[at + 4..at + 8].copy_from_slice(&200u32.to_le_bytes());
        fs::write(d.join("future.pak"), v).unwrap();
        assert!(read_pak(&d.join("future.pak")).unwrap_err().contains("unsupported PAK version 200"));
    }

    #[test]
    fn damaged_paks_never_panic() {
        let d = TempDir::new("pak4");
        let base = [v11("../../../Game/", &["Content/A.uasset", "Content/B.uasset"], false), v8("../../../Game/", &["Content/A.uasset", "Content/B.uasset"])];
        let mut x: u64 = 0x1234_5678_9ABC_DEF1;
        let mut rnd = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let f = d.join("fuzz.pak");
        for _ in 0..3000 {
            let mut b = base[(rnd() % 2) as usize].clone();
            match rnd() % 3 {
                0 => b.truncate((rnd() as usize) % (b.len() + 1)),
                1 => {
                    for _ in 0..1 + rnd() % 6 {
                        let n = b.len();
                        b[(rnd() as usize) % n] = rnd() as u8;
                    }
                }
                _ => {
                    // aim at the footer/index fields specifically
                    let n = b.len();
                    let at = n - 1 - (rnd() as usize) % 120.min(n - 1);
                    b[at] = rnd() as u8;
                }
            }
            fs::write(&f, &b).unwrap();
            let _ = read_pak(&f);
        }
    }

    fn pm(d: &TempDir, name: &str, files: &[&str]) -> PakMod {
        let path = d.join(&format!("{}.pak", name));
        fs::write(&path, v11("../../../Game/", files, false)).unwrap();
        PakMod { name: name.into(), path }
    }

    #[test]
    fn finds_overlapping_files_and_names_the_likely_winner() {
        let d = TempDir::new("pak5");
        let mods = vec![
            pm(&d, "AAA_Skin", &["Content/Hero.uasset", "Content/Hero.uexp", "Content/Only_A.uasset"]),
            pm(&d, "ZZZ_Skin_P", &["Content/hero.UASSET", "Content/Hero.uexp", "Content/Only_Z.uasset"]),
            pm(&d, "Unrelated", &["Content/Other.uasset"]),
            pm(&d, "Third", &["Content/Hero.uexp"]),
        ];
        let r = find_conflicts(&mods);
        assert_eq!((r.scanned, r.unreadable.len()), (4, 0));
        let find = |a: &str, b: &str| {
            r.conflicts
                .iter()
                .find(|c| (c.a == a && c.b == b) || (c.a == b && c.b == a))
                .unwrap_or_else(|| panic!("{a}/{b}: {:?}", r.conflicts))
        };
        let main = find("AAA_Skin", "ZZZ_Skin_P");
        assert_eq!(main.shared, 2, "paths are compared case-insensitively");
        assert_eq!(main.likely_winner, "ZZZ_Skin_P");
        assert_eq!(find("AAA_Skin", "Third").shared, 1);
        assert_eq!(find("Third", "ZZZ_Skin_P").shared, 1);
        assert!(r.conflicts.iter().all(|c| c.a != "Unrelated" && c.b != "Unrelated"));
        assert_eq!(r.conflicts[0].shared, 2, "biggest overlap first");
        // a plain name beats nothing against a patch pak regardless of alphabet
        let m2 = vec![pm(&d, "Zeta", &["x/a.uasset"]), pm(&d, "Alpha_P", &["x/a.uasset"])];
        assert_eq!(find_conflicts(&m2).conflicts[0].likely_winner, "Alpha_P");
        // equal rank: the later name wins
        let m3 = vec![pm(&d, "Bravo", &["x/a.uasset"]), pm(&d, "Alpha", &["x/a.uasset"])];
        assert_eq!(find_conflicts(&m3).conflicts[0].likely_winner, "Bravo");
    }

    #[test]
    fn unreadable_paks_are_listed_not_fatal() {
        let d = TempDir::new("pak6");
        let good = pm(&d, "Good", &["a/b.x"]);
        let bad_path = d.join("Bad.pak");
        fs::write(&bad_path, vec![1u8; 100]).unwrap();
        let r = find_conflicts(&[good, PakMod { name: "Bad".into(), path: bad_path }]);
        assert_eq!(r.scanned, 1);
        assert!(r.unreadable[0].starts_with("Bad:"), "{:?}", r.unreadable);
        assert!(r.conflicts.is_empty());
    }

    /// Against real game archives when available (read-only): the parser must agree with the
    /// entry count stored in each PAK's own index.
    #[test]
    fn real_game_archives_parse_when_present() {
        let roots = [
            std::path::PathBuf::from("/mnt/games/SteamLibrary/steamapps/common"),
            crate::fsutil::home_dir().join(".local/share/Steam/steamapps/common"),
        ];
        let mut checked = 0;
        for r in roots.iter().filter(|r| r.is_dir()) {
            for e in walkdir::WalkDir::new(r).max_depth(7).into_iter().flatten() {
                let p = e.path();
                if p.extension().map(|x| x == "pak").unwrap_or(false) && checked < 25 {
                    if let Ok(info) = read_pak(p) {
                        assert!(!info.paths.is_empty() || e.metadata().map(|m| m.len() < 2000).unwrap_or(true), "{}", p.display());
                        checked += 1;
                    }
                }
            }
        }
        eprintln!("parsed {} real PAK(s)", checked);
    }
}
