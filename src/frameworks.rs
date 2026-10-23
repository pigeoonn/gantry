//! Core mods that other mods need before they do anything: Cyberpunk 2077's RED4ext, ArchiveXL, TweakXL, Redscript and
//! Cyber Engine Tweaks, and the script extenders of Skyrim and Fallout 4. They are found from the files, so no website
//! or key is needed: a mod that brings `.archive.xl` files needs ArchiveXL, and so on.

use std::path::Path;

/// A core mod that mods ask for but the game does not have.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Missing {
    pub name: &'static str,
    /// Where to get it (a mod page or the search for one).
    pub page: &'static str,
    pub needed_by: usize,
}

struct Framework {
    name: &'static str,
    page: &'static str,
    /// A file whose presence means it is installed (paths from the game folder, any letter case).
    present: &'static [&'static str],
    /// Does an enabled mod file (lower-case, forward slashes) need it?
    needs: fn(&str) -> bool,
}

fn has(game: &Path, rel: &str) -> bool {
    let mut cur = game.to_path_buf();
    for part in rel.split('/') {
        let want = part.to_lowercase();
        let found = std::fs::read_dir(&cur).ok().and_then(|rd| rd.flatten().find(|e| e.file_name().to_string_lossy().to_lowercase() == want).map(|e| e.path()));
        match found {
            Some(p) => cur = p,
            None => return false,
        }
    }
    true
}

const CYBERPUNK: &[Framework] = &[
    Framework { name: "RED4ext", page: "https://www.nexusmods.com/cyberpunk2077/mods/2380", present: &["red4ext/RED4ext.dll"], needs: |f| f.starts_with("red4ext/plugins/") },
    Framework { name: "ArchiveXL", page: "https://www.nexusmods.com/cyberpunk2077/mods/4198", present: &["red4ext/plugins/ArchiveXL/ArchiveXL.dll"], needs: |f| f.ends_with(".archive.xl") },
    Framework {
        name: "TweakXL",
        page: "https://www.nexusmods.com/cyberpunk2077/search/?gsearch=TweakXL&gsearchtype=mods",
        present: &["red4ext/plugins/TweakXL/TweakXL.dll"],
        needs: |f| f.starts_with("r6/tweaks/") && (f.ends_with(".yaml") || f.ends_with(".yml") || f.ends_with(".xl")),
    },
    Framework { name: "Redscript", page: "https://www.nexusmods.com/cyberpunk2077/mods/1511", present: &["engine/tools/scc.exe"], needs: |f| f.starts_with("r6/scripts/") && f.ends_with(".reds") },
    Framework { name: "Cyber Engine Tweaks", page: "https://www.nexusmods.com/cyberpunk2077/mods/107", present: &["bin/x64/plugins/cyber_engine_tweaks.asi"], needs: |f| f.starts_with("bin/x64/plugins/cyber_engine_tweaks/mods/") },
];

const SKYRIM_SE: &[Framework] = &[Framework { name: "SKSE64", page: "https://skse.silverlock.org/", present: &["skse64_loader.exe"], needs: |f| f.starts_with("data/skse/plugins/") && f.ends_with(".dll") }];

const FALLOUT_4: &[Framework] = &[Framework { name: "F4SE", page: "https://f4se.silverlock.org/", present: &["f4se_loader.exe"], needs: |f| f.starts_with("data/f4se/plugins/") && f.ends_with(".dll") }];

/// Which table fits the game in `game`, by files only that game has.
fn table_for(game: &Path) -> Option<&'static [Framework]> {
    if has(game, "bin/x64/Cyberpunk2077.exe") {
        Some(CYBERPUNK)
    } else if has(game, "SkyrimSE.exe") {
        Some(SKYRIM_SE)
    } else if has(game, "Fallout4.exe") {
        Some(FALLOUT_4)
    } else {
        None
    }
}

/// The core mods that the enabled mods ask for and the game does not have, with how many files ask.
pub fn missing(game: &Path, enabled_files: &[String]) -> Vec<Missing> {
    let Some(table) = table_for(game) else { return Vec::new() };
    let lower: Vec<String> = enabled_files.iter().map(|f| f.replace('\\', "/").to_lowercase()).collect();
    table
        .iter()
        .filter_map(|fw| {
            let n = lower.iter().filter(|f| (fw.needs)(f)).count();
            (n > 0 && !fw.present.iter().any(|p| has(game, p))).then_some(Missing { name: fw.name, page: fw.page, needed_by: n })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsutil::testutil::TempDir;
    use std::fs;

    fn touch(p: &Path) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, "x").unwrap();
    }

    #[test]
    fn cyberpunk_mods_are_matched_with_the_core_mods_they_need() {
        let d = TempDir::new("fw");
        let game = d.join("Cyberpunk 2077");
        touch(&game.join("bin/x64/Cyberpunk2077.exe"));
        touch(&game.join("engine/tools/scc.exe"));
        touch(&game.join("bin/x64/Plugins/Cyber_Engine_Tweaks.asi"));
        let files: Vec<String> = ["archive/pc/mod/Cat.archive", "archive/pc/mod/Cat.archive.xl", "r6/tweaks/Cat.yaml", "r6/scripts/Cat.reds", "red4ext/plugins/Thing/Thing.dll", "bin/x64/plugins/cyber_engine_tweaks/mods/Foo/init.lua"].iter().map(|s| s.to_string()).collect();
        let m = missing(&game, &files);
        assert_eq!(m.iter().map(|x| (x.name, x.needed_by)).collect::<Vec<_>>(), vec![("RED4ext", 1), ("ArchiveXL", 1), ("TweakXL", 1)], "Redscript and CET are there (any letter case), the rest is not");
        // once they are installed nothing is reported
        touch(&game.join("red4ext/RED4ext.dll"));
        touch(&game.join("red4ext/plugins/ArchiveXL/ArchiveXL.dll"));
        touch(&game.join("red4ext/plugins/TweakXL/TweakXL.dll"));
        assert!(missing(&game, &files).is_empty());
        // a mod that asks for nothing special is fine without any of them
        assert!(missing(&game, &["archive/pc/mod/Plain.archive".to_string()]).is_empty());
    }

    #[test]
    fn other_games_only_get_the_script_extender_they_use() {
        let d = TempDir::new("fw2");
        let sky = d.join("Skyrim");
        touch(&sky.join("SkyrimSE.exe"));
        let plugin = vec!["Data\\SKSE\\Plugins\\Foo.dll".to_string()];
        assert_eq!(missing(&sky, &plugin).iter().map(|m| m.name).collect::<Vec<_>>(), vec!["SKSE64"]);
        touch(&sky.join("skse64_loader.exe"));
        assert!(missing(&sky, &plugin).is_empty());
        let plain = d.join("Plain");
        touch(&plain.join("plain.exe"));
        assert!(missing(&plain, &plugin).is_empty(), "a game Gantry knows nothing about is never nagged");
    }
}
