//! The welcome list: for every game Gantry found, one plain verdict and what to do about it.

use crate::loaders::UnityInfo;
use serde_json::{json, Value};

/// What to tell the user about one game.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Mods can go in as they are.
    Ready,
    /// A Unity game without a mod loader (`kind` says which one fits).
    NeedsLoader(&'static str),
    /// An Unreal game without UE4SS.
    NeedsUe4ss,
    /// A game with core mods (Cyberpunk's RED4ext and the others, a script extender) that are not there yet: how many.
    NeedsCore(usize),
    /// The game has anti-cheat; Gantry will not put mods into it.
    Protected(String),
}

/// Decide from what is on disk: the Unity facts (if any), the engine name, whether UE4SS is there,
/// the loader a support entry recommends (may be empty) and the anti-cheat found in the folder.
pub fn verdict(unity: Option<&UnityInfo>, engine: &str, ue4ss: bool, recommended: &str, anti_cheat: Option<&str>) -> Verdict {
    verdict_with_cores(unity, engine, ue4ss, recommended, anti_cheat, 0)
}

/// The same, knowing how many of the game's core mods are missing.
pub fn verdict_with_cores(unity: Option<&UnityInfo>, engine: &str, ue4ss: bool, recommended: &str, anti_cheat: Option<&str>, missing_cores: usize) -> Verdict {
    if let Some(ac) = anti_cheat {
        return Verdict::Protected(ac.to_string());
    }
    if let Some(u) = unity {
        if !u.loader.is_empty() {
            return Verdict::Ready;
        }
        let kind = match recommended {
            "melonloader" => "melonloader",
            "bepinex5" => "bepinex5",
            "bepinex6" => "bepinex6",
            _ if u.il2cpp => "bepinex6",
            _ => "bepinex5",
        };
        return Verdict::NeedsLoader(kind);
    }
    if engine.to_lowercase().contains("unreal") && !ue4ss {
        return Verdict::NeedsUe4ss;
    }
    if missing_cores > 0 {
        return Verdict::NeedsCore(missing_cores);
    }
    Verdict::Ready
}

/// JSON for one row.
pub fn row(appid: &str, name: &str, engine: &str, mods: usize, v: &Verdict) -> Value {
    let (code, kind, extra) = match v {
        Verdict::Ready => ("ready", "", String::new()),
        Verdict::NeedsLoader(k) => ("needs_loader", *k, String::new()),
        Verdict::NeedsUe4ss => ("needs_ue4ss", "", String::new()),
        Verdict::NeedsCore(n) => ("needs_core", "", n.to_string()),
        Verdict::Protected(ac) => ("protected", "", ac.clone()),
    };
    json!({ "appid": appid, "name": name, "engine": engine, "mods": mods, "verdict": code, "kind": kind, "detail": extra })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unity(il2cpp: bool, loader: &str) -> UnityInfo {
        UnityInfo { il2cpp, x64: true, loader: loader.into() }
    }

    #[test]
    fn unity_games_get_the_loader_that_fits_them() {
        assert_eq!(verdict(Some(&unity(true, "")), "Unity", false, "", None), Verdict::NeedsLoader("bepinex6"));
        assert_eq!(verdict(Some(&unity(false, "")), "Unity", false, "", None), Verdict::NeedsLoader("bepinex5"));
        assert_eq!(verdict(Some(&unity(true, "")), "Unity", false, "melonloader", None), Verdict::NeedsLoader("melonloader"));
        assert_eq!(verdict(Some(&unity(true, "bepinex")), "Unity", false, "melonloader", None), Verdict::Ready);
    }

    #[test]
    fn unreal_games_need_ue4ss_and_other_games_are_ready() {
        assert_eq!(verdict(None, "Unreal Engine 5.4", false, "", None), Verdict::NeedsUe4ss);
        assert_eq!(verdict(None, "Unreal Engine 5.4", true, "", None), Verdict::Ready);
        assert_eq!(verdict(None, "Standard / Non-UE", false, "", None), Verdict::Ready);
        assert_eq!(verdict_with_cores(None, "Standard / Non-UE", false, "", None, 2), Verdict::NeedsCore(2));
        assert_eq!(verdict_with_cores(None, "Standard / Non-UE", false, "", Some("EAC"), 2), Verdict::Protected("EAC".into()), "anti-cheat still wins");
        assert_eq!(row("1", "G", "x", 0, &Verdict::NeedsCore(3))["detail"], "3");
    }

    #[test]
    fn anti_cheat_beats_everything() {
        assert_eq!(verdict(Some(&unity(true, "")), "Unity", false, "", Some("EasyAntiCheat")), Verdict::Protected("EasyAntiCheat".into()));
        let r = row("1", "G", "Unity", 0, &Verdict::Protected("EAC".into()));
        assert_eq!(r["verdict"], "protected");
        assert_eq!(r["detail"], "EAC");
    }
}
