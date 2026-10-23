//! FOMOD installers: the `fomod/ModuleConfig.xml` many big mods ship instead of a plain folder. It asks
//! questions (pages of groups of options), remembers the answers as flags, and installs the files the
//! answers lead to. This module reads the script, works out what to show and what to install, and lays
//! the chosen files out in a fresh folder that the library then takes like any other mod. It never
//! runs anything: a script can only name files inside its own archive.

use roxmltree::{Document, Node};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Module {
    pub name: String,
    pub image: String,
    pub required: Vec<FileOp>,
    pub steps: Vec<Step>,
    pub conditional: Vec<(Dep, Vec<FileOp>)>,
    pub depends: Option<Dep>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Step {
    pub name: String,
    pub visible: Option<Dep>,
    pub groups: Vec<Group>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    ExactlyOne,
    AtMostOne,
    #[default]
    Any,
    AtLeastOne,
    All,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Group {
    pub name: String,
    pub kind: Kind,
    pub plugins: Vec<Plugin>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Usable {
    Required,
    Recommended,
    #[default]
    Optional,
    CouldBeUsable,
    NotUsable,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plugin {
    pub name: String,
    pub description: String,
    pub image: String,
    pub flags: Vec<(String, String)>,
    pub files: Vec<FileOp>,
    pub default: Usable,
    pub patterns: Vec<(Dep, Usable)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dep {
    And(Vec<Dep>),
    Or(Vec<Dep>),
    Flag { name: String, value: String },
    File { path: String, state: String },
    /// Game or mod-manager version checks: Gantry cannot tell, so they count as met.
    Version,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileOp {
    pub source: String,
    pub dest: String,
    pub priority: i32,
    pub folder: bool,
    pub always: bool,
    pub if_usable: bool,
}

// ───────────────────────────── reading the script ─────────────────────────────

/// The script's text: UTF-8 or UTF-16 (with or without a byte order mark), as the mods ship it.
pub fn decode(bytes: &[u8]) -> String {
    let utf16 = |b: &[u8], le: bool| -> String {
        let (pairs, _) = b.as_chunks::<2>();
        let units: Vec<u16> = pairs.iter().map(|c| if le { u16::from_le_bytes(*c) } else { u16::from_be_bytes(*c) }).collect();
        String::from_utf16_lossy(&units)
    };
    let s = if bytes.starts_with(&[0xFF, 0xFE]) {
        utf16(&bytes[2..], true)
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        utf16(&bytes[2..], false)
    } else if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8_lossy(&bytes[3..]).to_string()
    } else if bytes.len() > 4 && bytes[1] == 0 && bytes[3] == 0 {
        utf16(bytes, true) // no mark, but every other byte is empty
    } else if bytes.len() > 4 && bytes[0] == 0 && bytes[2] == 0 {
        utf16(bytes, false)
    } else {
        String::from_utf8_lossy(bytes).to_string()
    };
    s.trim_start_matches('\u{feff}').to_string()
}

fn name_is(n: &Node, want: &str) -> bool {
    n.is_element() && n.tag_name().name().eq_ignore_ascii_case(want)
}

fn kids<'a, 'b: 'a>(n: Node<'a, 'b>, want: &'static str) -> impl Iterator<Item = Node<'a, 'b>> + 'a {
    n.children().filter(move |c| name_is(c, want))
}

fn kid<'a, 'b: 'a>(n: Node<'a, 'b>, want: &'static str) -> Option<Node<'a, 'b>> {
    kids(n, want).next()
}

/// An attribute by name, ignoring case.
fn attr<'a>(n: &Node<'a, '_>, name: &str) -> &'a str {
    n.attributes().find(|a| a.name().eq_ignore_ascii_case(name)).map(|a| a.value()).unwrap_or("")
}

fn text_of(n: Node) -> String {
    n.descendants().filter(|d| d.is_text()).filter_map(|d| d.text()).collect::<String>().trim().to_string()
}

fn deps_of(n: Node) -> Dep {
    let or = attr(&n, "operator").eq_ignore_ascii_case("or");
    let mut parts = Vec::new();
    for c in n.children().filter(|c| c.is_element()) {
        let tag = c.tag_name().name().to_lowercase();
        match tag.as_str() {
            "filedependency" => parts.push(Dep::File { path: attr(&c, "file").replace('\\', "/"), state: attr(&c, "state").to_lowercase() }),
            "flagdependency" => parts.push(Dep::Flag { name: attr(&c, "flag").to_string(), value: attr(&c, "value").to_string() }),
            "gamedependency" | "fommdependency" | "foseversion" | "gameversion" => parts.push(Dep::Version),
            "dependencies" => parts.push(deps_of(c)),
            _ => {}
        }
    }
    if or {
        Dep::Or(parts)
    } else {
        Dep::And(parts)
    }
}

/// The condition inside a container (`visible`, `pattern`, `moduleDependencies`): a nested `dependencies`
/// element, or the conditions written straight into the container.
fn condition(n: Node) -> Option<Dep> {
    if let Some(d) = kid(n, "dependencies") {
        return Some(deps_of(d));
    }
    let direct = n.children().any(|c| c.is_element() && ["filedependency", "flagdependency", "gamedependency", "fommdependency"].contains(&c.tag_name().name().to_lowercase().as_str()));
    direct.then(|| deps_of(n))
}

fn file_ops(n: Node) -> Vec<FileOp> {
    n.children()
        .filter(|c| name_is(c, "file") || name_is(c, "folder"))
        .map(|c| FileOp {
            source: attr(&c, "source").replace('\\', "/"),
            dest: if c.attribute("destination").is_some() || c.attributes().any(|a| a.name().eq_ignore_ascii_case("destination")) { attr(&c, "destination").replace('\\', "/") } else { attr(&c, "source").replace('\\', "/") },
            priority: attr(&c, "priority").trim().parse().unwrap_or(0),
            folder: name_is(&c, "folder"),
            always: attr(&c, "alwaysInstall").eq_ignore_ascii_case("true"),
            if_usable: attr(&c, "installIfUsable").eq_ignore_ascii_case("true"),
        })
        .filter(|f| !f.source.trim().is_empty())
        .collect()
}

fn usable(name: &str) -> Usable {
    match name.to_lowercase().as_str() {
        "required" => Usable::Required,
        "recommended" => Usable::Recommended,
        "notusable" => Usable::NotUsable,
        "couldbeusable" => Usable::CouldBeUsable,
        _ => Usable::Optional,
    }
}

fn parse_plugin(p: Node) -> Plugin {
    let mut out = Plugin { name: attr(&p, "name").to_string(), ..Default::default() };
    if let Some(d) = kid(p, "description") {
        out.description = text_of(d);
    }
    if let Some(i) = kid(p, "image") {
        out.image = attr(&i, "path").replace('\\', "/");
    }
    if let Some(cf) = kid(p, "conditionFlags") {
        out.flags = kids(cf, "flag").map(|f| (attr(&f, "name").to_string(), text_of(f))).filter(|(n, _)| !n.is_empty()).collect();
    }
    if let Some(f) = kid(p, "files") {
        out.files = file_ops(f);
    }
    if let Some(td) = kid(p, "typeDescriptor") {
        if let Some(t) = kid(td, "type") {
            out.default = usable(attr(&t, "name"));
        } else if let Some(dt) = kid(td, "dependencyType") {
            if let Some(d) = kid(dt, "defaultType") {
                out.default = usable(attr(&d, "name"));
            }
            if let Some(ps) = kid(dt, "patterns") {
                for pat in kids(ps, "pattern") {
                    let ty = kid(pat, "type").map(|t| usable(attr(&t, "name"))).unwrap_or_default();
                    if let Some(c) = condition(pat) {
                        out.patterns.push((c, ty));
                    }
                }
            }
        }
    }
    out
}

fn ordered<T>(mut items: Vec<T>, order: &str, name: impl Fn(&T) -> String) -> Vec<T> {
    match order.to_lowercase().as_str() {
        "ascending" => items.sort_by_key(|i| name(i).to_lowercase()),
        "descending" => items.sort_by_key(|i| std::cmp::Reverse(name(i).to_lowercase())),
        _ => {}
    }
    items
}

/// Read a `ModuleConfig.xml`.
pub fn parse(text: &str) -> Result<Module, String> {
    let fixed: String;
    let doc = match Document::parse(text) {
        Ok(d) => d,
        Err(_) => {
            // scripts written by hand often have a bare "&"; make those entities and try once more
            static AMP: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"&(?:#[0-9]+;|#x[0-9a-fA-F]+;|[A-Za-z]+;)?").expect("regex"));
            fixed = AMP.replace_all(text, |c: &regex::Captures| if c[0].len() > 1 { c[0].to_string() } else { "&amp;".to_string() }).to_string();
            Document::parse(&fixed).map_err(|e| format!("The installer script could not be read: {}", e))?
        }
    };
    let root = doc.root_element();
    if !name_is(&root, "config") {
        return Err("This is not an installer script (no <config> at the top).".into());
    }
    let mut m = Module::default();
    if let Some(n) = kid(root, "moduleName") {
        m.name = text_of(n);
    }
    if let Some(i) = kid(root, "moduleImage") {
        m.image = attr(&i, "path").replace('\\', "/");
    }
    if let Some(d) = kid(root, "moduleDependencies") {
        m.depends = condition(d).or_else(|| Some(deps_of(d)));
    }
    if let Some(r) = kid(root, "requiredInstallFiles") {
        m.required = file_ops(r);
    }
    if let Some(steps) = kid(root, "installSteps") {
        let mut v = Vec::new();
        for s in kids(steps, "installStep") {
            let mut step = Step { name: attr(&s, "name").to_string(), ..Default::default() };
            step.visible = kid(s, "visible").and_then(condition);
            if let Some(gs) = kid(s, "optionalFileGroups") {
                let mut groups = Vec::new();
                for g in kids(gs, "group") {
                    let kind = match attr(&g, "type").to_lowercase().as_str() {
                        "selectexactlyone" => Kind::ExactlyOne,
                        "selectatmostone" => Kind::AtMostOne,
                        "selectatleastone" => Kind::AtLeastOne,
                        "selectall" => Kind::All,
                        _ => Kind::Any,
                    };
                    let plugins = kid(g, "plugins").map(|ps| ordered(kids(ps, "plugin").map(parse_plugin).collect(), attr(&ps, "order"), |p| p.name.clone())).unwrap_or_default();
                    groups.push(Group { name: attr(&g, "name").to_string(), kind, plugins });
                }
                step.groups = ordered(groups, attr(&gs, "order"), |g| g.name.clone());
            }
            v.push(step);
        }
        m.steps = ordered(v, attr(&steps, "order"), |s| s.name.clone());
    }
    if let Some(c) = kid(root, "conditionalFileInstalls") {
        if let Some(ps) = kid(c, "patterns") {
            for pat in kids(ps, "pattern") {
                let files = kid(pat, "files").map(file_ops).unwrap_or_default();
                m.conditional.push((condition(pat).unwrap_or(Dep::And(vec![])), files));
            }
        }
    }
    if m.steps.is_empty() && m.required.is_empty() && m.conditional.is_empty() {
        return Err("The installer script does not install anything.".into());
    }
    Ok(m)
}

// ───────────────────────────── finding it in an archive ─────────────────────────────

/// The script of an extracted archive and the folder its file paths are relative to (the folder that
/// holds `fomod/`; archives often wrap everything in one more folder).
pub fn find_module(stage: &Path) -> Option<(PathBuf, PathBuf)> {
    fn child_ci(dir: &Path, name: &str) -> Option<PathBuf> {
        fs::read_dir(dir).ok()?.flatten().find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(name)).map(|e| e.path())
    }
    let mut roots = vec![stage.to_path_buf()];
    for _ in 0..2 {
        let more: Vec<PathBuf> = roots.iter().flat_map(|r| fs::read_dir(r).into_iter().flatten().flatten().filter(|e| e.path().is_dir()).map(|e| e.path())).take(40).collect();
        roots.extend(more);
    }
    roots.into_iter().find_map(|root| {
        let dir = child_ci(&root, "fomod").filter(|d| d.is_dir())?;
        let script = child_ci(&dir, "ModuleConfig.xml").filter(|p| p.is_file())?;
        Some((script, root))
    })
}

pub fn load(script: &Path) -> Result<Module, String> {
    let bytes = fs::read(script).map_err(|e| format!("The installer script could not be read: {}", e))?;
    if bytes.len() > 4_000_000 {
        return Err("The installer script is unreasonably large.".into());
    }
    parse(&decode(&bytes))
}

// ───────────────────────────── what to show ─────────────────────────────

/// What the user has picked: `"step:group"` -> indexes of the chosen plugins. Anything missing is
/// filled in with the script's own defaults.
pub type Selection = BTreeMap<String, Vec<usize>>;

fn key(step: usize, group: usize) -> String {
    format!("{step}:{group}")
}

/// What the script may look at in the game: does this file exist?
pub trait World {
    fn has_file(&self, path: &str) -> bool;
}

impl<F: Fn(&str) -> bool> World for F {
    fn has_file(&self, path: &str) -> bool {
        self(path)
    }
}

fn holds(dep: &Dep, flags: &HashMap<String, String>, world: &dyn World) -> bool {
    match dep {
        Dep::And(v) => v.iter().all(|d| holds(d, flags, world)),
        Dep::Or(v) => v.is_empty() || v.iter().any(|d| holds(d, flags, world)),
        Dep::Flag { name, value } => flags.get(name).map(|v| v == value).unwrap_or(value.is_empty()),
        Dep::File { path, state } => {
            let there = world.has_file(path);
            match state.as_str() {
                "missing" => !there,
                _ => there, // "active" and "inactive" both mean "it is there" to a program that has no load order
            }
        }
        Dep::Version => true,
    }
}

fn plugin_type(p: &Plugin, flags: &HashMap<String, String>, world: &dyn World) -> Usable {
    p.patterns.iter().find(|(d, _)| holds(d, flags, world)).map(|(_, t)| *t).unwrap_or(p.default)
}

#[derive(Clone, Debug, Serialize)]
pub struct PluginView {
    pub name: String,
    pub description: String,
    pub image: String,
    pub usable: Usable,
    pub selected: bool,
    /// Cannot be changed (required, or the only way to satisfy the group).
    pub locked: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct GroupView {
    pub name: String,
    pub kind: Kind,
    pub plugins: Vec<PluginView>,
    /// Why the choice is not valid yet ("" when it is).
    pub problem: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct StepView {
    pub name: String,
    pub groups: Vec<GroupView>,
    /// Index of the step in the script (the answers are stored per step and group).
    pub index: usize,
}

/// The pages the user sees for these answers, the final answers (defaults filled in, impossible ones
/// dropped) and the flags they set.
pub struct Resolved {
    pub steps: Vec<StepView>,
    pub selection: Selection,
    pub flags: HashMap<String, String>,
}

pub fn resolve(m: &Module, chosen: &Selection, world: &dyn World) -> Resolved {
    let mut flags: HashMap<String, String> = HashMap::new();
    let mut steps = Vec::new();
    let mut selection = Selection::new();
    for (si, step) in m.steps.iter().enumerate() {
        if let Some(v) = &step.visible {
            if !holds(v, &flags, world) {
                continue;
            }
        }
        let mut groups = Vec::new();
        for (gi, g) in step.groups.iter().enumerate() {
            let types: Vec<Usable> = g.plugins.iter().map(|p| plugin_type(p, &flags, world)).collect();
            let k = key(si, gi);
            let mut picked: Vec<usize> = match chosen.get(&k) {
                Some(c) => c.iter().copied().filter(|&i| i < g.plugins.len()).collect(),
                None => default_pick(g.kind, &types),
            };
            // what the script forces
            picked.retain(|&i| types[i] != Usable::NotUsable);
            for (i, t) in types.iter().enumerate() {
                if (*t == Usable::Required || g.kind == Kind::All) && *t != Usable::NotUsable && !picked.contains(&i) {
                    picked.push(i);
                }
            }
            if matches!(g.kind, Kind::ExactlyOne | Kind::AtMostOne) && picked.len() > 1 {
                // keep a required one, else the last one chosen
                let keep = picked.iter().copied().find(|&i| types[i] == Usable::Required).unwrap_or(*picked.last().expect("not empty"));
                picked = vec![keep];
            }
            picked.sort_unstable();
            picked.dedup();
            let problem = match g.kind {
                Kind::ExactlyOne if picked.len() != 1 => "Pick exactly one.",
                Kind::AtLeastOne if picked.is_empty() => "Pick at least one.",
                _ => "",
            };
            for &i in &picked {
                for (n, v) in &g.plugins[i].flags {
                    flags.insert(n.clone(), v.clone());
                }
            }
            let plugins = g
                .plugins
                .iter()
                .enumerate()
                .map(|(i, p)| PluginView {
                    name: p.name.clone(),
                    description: p.description.clone(),
                    image: p.image.clone(),
                    usable: types[i],
                    selected: picked.contains(&i),
                    locked: types[i] == Usable::Required || types[i] == Usable::NotUsable || g.kind == Kind::All,
                })
                .collect();
            selection.insert(k, picked);
            groups.push(GroupView { name: g.name.clone(), kind: g.kind, plugins, problem: problem.to_string() });
        }
        steps.push(StepView { name: step.name.clone(), groups, index: si });
    }
    Resolved { steps, selection, flags }
}

fn default_pick(kind: Kind, types: &[Usable]) -> Vec<usize> {
    let good: Vec<usize> = types.iter().enumerate().filter(|(_, t)| matches!(t, Usable::Required | Usable::Recommended)).map(|(i, _)| i).collect();
    let usable_first = types.iter().position(|t| *t != Usable::NotUsable);
    match kind {
        Kind::All => (0..types.len()).filter(|&i| types[i] != Usable::NotUsable).collect(),
        Kind::ExactlyOne => good.first().copied().or(usable_first).into_iter().collect(),
        Kind::AtMostOne => good.first().copied().into_iter().collect(),
        Kind::AtLeastOne => if good.is_empty() { usable_first.into_iter().collect() } else { good },
        Kind::Any => good,
    }
}

/// Is every visible group answered validly?
pub fn complete(r: &Resolved) -> bool {
    r.steps.iter().all(|s| s.groups.iter().all(|g| g.problem.is_empty()))
}

// ───────────────────────────── what to install ─────────────────────────────

/// The files the answers lead to, in the order they are laid down (later ones replace earlier ones;
/// a higher priority is laid down later).
pub fn install_files(m: &Module, r: &Resolved, world: &dyn World) -> Vec<FileOp> {
    let mut ops: Vec<FileOp> = m.required.clone();
    for sv in &r.steps {
        let step = &m.steps[sv.index];
        for (gi, g) in step.groups.iter().enumerate() {
            for &pi in r.selection.get(&key(sv.index, gi)).into_iter().flatten() {
                ops.extend(g.plugins[pi].files.iter().cloned());
            }
        }
    }
    for (dep, files) in &m.conditional {
        if holds(dep, &r.flags, world) {
            ops.extend(files.iter().cloned());
        }
    }
    let mut indexed: Vec<(usize, FileOp)> = ops.into_iter().enumerate().collect();
    indexed.sort_by_key(|(i, f)| (f.priority, *i));
    indexed.into_iter().map(|(_, f)| f).collect()
}

/// A path from a script, made safe: no drive, no `..`, no leading slash.
fn clean(p: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    for c in Path::new(&p.replace('\\', "/")).components() {
        match c {
            // a colon would be a drive letter or a hidden stream on Windows
            Component::Normal(n) if n.to_string_lossy().contains(':') => return None,
            Component::Normal(n) => out.push(n.to_string_lossy().to_string()),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(out)
}

/// `root/<rel>` where each part may differ in letter case from what is on disk.
pub fn resolve_ci(root: &Path, rel: &str) -> Option<PathBuf> {
    let mut cur = root.to_path_buf();
    for part in clean(rel)? {
        let exact = cur.join(&part);
        if exact.symlink_metadata().is_ok() {
            cur = exact;
            continue;
        }
        let hit = fs::read_dir(&cur).ok()?.flatten().find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(&part))?;
        cur = hit.path();
    }
    Some(cur)
}

fn copy_tree(src: &Path, dst: &Path, count: &mut usize) -> Result<(), String> {
    for e in walkdir::WalkDir::new(src).follow_links(false).into_iter().flatten() {
        let rel = e.path().strip_prefix(src).map_err(|e| e.to_string())?;
        let target = dst.join(rel);
        if e.file_type().is_dir() {
            fs::create_dir_all(&target).map_err(|e| e.to_string())?;
        } else if e.file_type().is_file() {
            if let Some(p) = target.parent() {
                fs::create_dir_all(p).map_err(|e| e.to_string())?;
            }
            let _ = fs::remove_file(&target);
            fs::copy(e.path(), &target).map_err(|er| format!("Could not copy {}: {}", e.path().display(), er))?;
            *count += 1;
        }
    }
    Ok(())
}

/// Lay the files out in `out` (a new, empty folder) the way the game folder wants them. Returns the
/// number of files and the sources the archive does not have.
pub fn materialize(ops: &[FileOp], root: &Path, out: &Path) -> Result<(usize, Vec<String>), String> {
    let mut count = 0;
    let mut missing = Vec::new();
    for op in ops {
        let Some(src) = resolve_ci(root, &op.source).filter(|p| p.exists()) else {
            missing.push(op.source.clone());
            continue;
        };
        // a script can only read inside its archive (links that lead out are not followed)
        if !src.canonicalize().map(|c| c.starts_with(root.canonicalize().unwrap_or_else(|_| root.to_path_buf()))).unwrap_or(false) {
            missing.push(op.source.clone());
            continue;
        }
        let dest = clean(&op.dest).ok_or_else(|| format!("The script wants a file placed outside the game folder: {}", op.dest))?;
        let mut target = out.to_path_buf();
        target.extend(&dest);
        if op.folder {
            fs::create_dir_all(&target).map_err(|e| e.to_string())?;
            copy_tree(&src, &target, &mut count)?;
        } else {
            // a file whose destination ends in a folder name (or is empty) keeps its own name
            let into_dir = op.dest.is_empty() || op.dest.ends_with('/') || op.dest.ends_with('\\');
            if into_dir {
                target.push(src.file_name().ok_or("A file without a name")?);
            }
            if let Some(p) = target.parent() {
                fs::create_dir_all(p).map_err(|e| e.to_string())?;
            }
            let _ = fs::remove_file(&target);
            fs::copy(&src, &target).map_err(|e| format!("Could not copy {}: {}", src.display(), e))?;
            count += 1;
        }
    }
    Ok((count, missing))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsutil::testutil::TempDir;

    const SCRIPT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<config xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:noNamespaceSchemaLocation="http://qconsulting.ca/fo3/ModConfig5.0.xsd">
  <moduleName>Cool Overhaul</moduleName>
  <moduleImage path="fomod\banner.png" />
  <requiredInstallFiles>
    <file source="core\base.esp" destination="base.esp" />
    <folder source="common" destination="Data\common" />
  </requiredInstallFiles>
  <installSteps order="Explicit">
    <installStep name="Look">
      <optionalFileGroups order="Explicit">
        <group name="Style" type="SelectExactlyOne">
          <plugins order="Explicit">
            <plugin name="Dark">
              <description>Dark and moody.</description>
              <image path="fomod\dark.png"/>
              <conditionFlags><flag name="style">dark</flag></conditionFlags>
              <files><file source="styles\dark.dds" destination="Data\tex.dds" priority="1"/></files>
              <typeDescriptor><type name="Recommended"/></typeDescriptor>
            </plugin>
            <plugin name="Light">
              <description>Bright.</description>
              <conditionFlags><flag name="style">light</flag></conditionFlags>
              <files><file source="styles\light.dds" destination="Data\tex.dds" priority="1"/></files>
              <typeDescriptor><type name="Optional"/></typeDescriptor>
            </plugin>
          </plugins>
        </group>
        <group name="Extras" type="SelectAny">
          <plugins order="Explicit">
            <plugin name="Sounds">
              <description>New sounds.</description>
              <files><folder source="extras\sounds" destination="Data\sounds"/></files>
              <typeDescriptor><type name="Optional"/></typeDescriptor>
            </plugin>
            <plugin name="Patch for X">
              <description>Only when X is installed.</description>
              <files><file source="extras\patch.esp" destination="patch.esp"/></files>
              <typeDescriptor>
                <dependencyType>
                  <defaultType name="NotUsable"/>
                  <patterns><pattern><dependencies><fileDependency file="X.esm" state="Active"/></dependencies><type name="Recommended"/></pattern></patterns>
                </dependencyType>
              </typeDescriptor>
            </plugin>
          </plugins>
        </group>
      </optionalFileGroups>
    </installStep>
    <installStep name="Dark options">
      <visible><dependencies operator="And"><flagDependency flag="style" value="dark"/></dependencies></visible>
      <optionalFileGroups>
        <group name="Glow" type="SelectAtMostOne">
          <plugins>
            <plugin name="Glow on"><description>Glow.</description><files><file source="extras\glow.esp" destination="glow.esp"/></files><typeDescriptor><type name="Optional"/></typeDescriptor></plugin>
          </plugins>
        </group>
      </optionalFileGroups>
    </installStep>
  </installSteps>
  <conditionalFileInstalls>
    <patterns>
      <pattern>
        <dependencies operator="And"><flagDependency flag="style" value="dark"/></dependencies>
        <files><file source="styles\dark_extra.dds" destination="Data\tex2.dds"/></files>
      </pattern>
    </patterns>
  </conditionalFileInstalls>
</config>"#;

    fn world(files: &'static [&'static str]) -> impl Fn(&str) -> bool {
        move |p: &str| files.iter().any(|f| f.eq_ignore_ascii_case(p))
    }

    #[test]
    fn a_script_is_read_with_steps_groups_flags_and_conditions() {
        let m = parse(SCRIPT).unwrap();
        assert_eq!(m.name, "Cool Overhaul");
        assert_eq!(m.required.len(), 2);
        assert!(m.required[1].folder && m.required[1].dest == "Data/common", "backslashes become slashes");
        assert_eq!(m.steps.len(), 2);
        assert_eq!(m.steps[0].groups[0].kind, Kind::ExactlyOne);
        assert_eq!(m.steps[0].groups[0].plugins[0].flags, vec![("style".to_string(), "dark".to_string())]);
        assert_eq!(m.steps[0].groups[0].plugins[0].default, Usable::Recommended);
        assert_eq!(m.steps[0].groups[1].plugins[1].patterns.len(), 1);
        assert!(m.steps[1].visible.is_some());
        assert_eq!(m.conditional.len(), 1);
        assert_eq!(m.steps[0].groups[0].plugins[0].files[0].priority, 1);
    }

    #[test]
    fn defaults_follow_the_script_and_flags_open_later_steps() {
        let m = parse(SCRIPT).unwrap();
        let w = world(&[]);
        let r = resolve(&m, &Selection::new(), &w);
        // "Dark" is recommended, so it is chosen; that opens the second step
        assert_eq!(r.steps.len(), 2, "the dark options step is visible");
        assert!(r.steps[0].groups[0].plugins[0].selected && !r.steps[0].groups[0].plugins[1].selected);
        assert_eq!(r.flags.get("style").map(String::as_str), Some("dark"));
        // the patch plugin is not usable without X.esm
        let patch = &r.steps[0].groups[1].plugins[1];
        assert_eq!(patch.usable, Usable::NotUsable);
        assert!(!patch.selected && patch.locked);
        assert!(complete(&r));
        // choosing Light hides the second step
        let mut sel = r.selection.clone();
        sel.insert("0:0".into(), vec![1]);
        let r2 = resolve(&m, &sel, &w);
        assert_eq!(r2.steps.len(), 1);
        assert_eq!(r2.flags.get("style").map(String::as_str), Some("light"));
        // with X.esm present the patch is recommended and picked by default
        let w2 = world(&["X.esm"]);
        let r3 = resolve(&m, &Selection::new(), &w2);
        assert_eq!(r3.steps[0].groups[1].plugins[1].usable, Usable::Recommended);
        assert!(r3.steps[0].groups[1].plugins[1].selected);
    }

    #[test]
    fn a_group_that_allows_one_never_keeps_two_and_not_usable_is_never_picked() {
        let m = parse(SCRIPT).unwrap();
        let w = world(&[]);
        let mut sel = Selection::new();
        sel.insert("0:0".into(), vec![0, 1]);
        sel.insert("0:1".into(), vec![1, 0]);
        let r = resolve(&m, &sel, &w);
        assert_eq!(r.selection["0:0"].len(), 1, "exactly one of the style options");
        assert_eq!(r.selection["0:1"], vec![0], "the patch cannot be chosen without X");
        // an out-of-range answer is ignored rather than trusted
        let mut bad = Selection::new();
        bad.insert("0:0".into(), vec![9]);
        let r = resolve(&m, &bad, &w);
        assert!(!complete(&r), "nothing valid chosen for the exactly-one group: {:?}", r.selection);
        assert_eq!(r.steps[0].groups[0].problem, "Pick exactly one.");
    }

    #[test]
    fn the_install_list_follows_the_answers_and_priorities() {
        let m = parse(SCRIPT).unwrap();
        let w = world(&[]);
        let r = resolve(&m, &Selection::new(), &w);
        let files = install_files(&m, &r, &w);
        let dests: Vec<&str> = files.iter().map(|f| f.dest.as_str()).collect();
        assert!(dests.contains(&"base.esp") && dests.contains(&"Data/common") && dests.contains(&"Data/tex.dds") && dests.contains(&"Data/tex2.dds"), "{dests:?}");
        assert!(!dests.contains(&"patch.esp"));
        // priority 1 files come after priority 0 ones
        let first_pri = files.iter().position(|f| f.priority == 1).unwrap();
        assert!(files[..first_pri].iter().all(|f| f.priority == 0));
        // light: no dark extras
        let mut sel = r.selection.clone();
        sel.insert("0:0".into(), vec![1]);
        let r2 = resolve(&m, &sel, &w);
        let d2: Vec<String> = install_files(&m, &r2, &w).into_iter().map(|f| f.dest).collect();
        assert!(d2.contains(&"Data/tex.dds".to_string()) && !d2.contains(&"Data/tex2.dds".to_string()) && !d2.contains(&"glow.esp".to_string()));
    }

    #[test]
    fn files_are_laid_out_with_the_right_names_and_unsafe_scripts_are_stopped() {
        let d = TempDir::new("fomod_mat");
        let root = d.join("arc");
        for (p, c) in [("core/base.esp", "B"), ("common/a.txt", "A"), ("common/sub/b.txt", "Bb"), ("styles/dark.dds", "D"), ("styles/dark_extra.dds", "DX")] {
            fs::create_dir_all(root.join(p).parent().unwrap()).unwrap();
            fs::write(root.join(p), c).unwrap();
        }
        let m = parse(SCRIPT).unwrap();
        let w = world(&[]);
        let r = resolve(&m, &Selection::new(), &w);
        let ops = install_files(&m, &r, &w);
        let out = d.join("out");
        fs::create_dir_all(&out).unwrap();
        let (n, missing) = materialize(&ops, &root, &out).unwrap();
        assert_eq!(fs::read_to_string(out.join("base.esp")).unwrap(), "B");
        assert_eq!(fs::read_to_string(out.join("Data/common/sub/b.txt")).unwrap(), "Bb");
        assert_eq!(fs::read_to_string(out.join("Data/tex.dds")).unwrap(), "D");
        assert!(n >= 5 && missing.is_empty(), "{n} {missing:?}");
        // a source the archive does not have is reported, not fatal; letter case does not matter
        let more = vec![FileOp { source: "CORE\\BASE.ESP".into(), dest: "x/y.esp".into(), ..Default::default() }, FileOp { source: "nope.esp".into(), dest: "n.esp".into(), ..Default::default() }];
        let (n2, miss2) = materialize(&more, &root, &out).unwrap();
        assert_eq!((n2, miss2), (1, vec!["nope.esp".to_string()]));
        assert!(out.join("x/y.esp").is_file());
        // a destination that leaves the game folder is refused
        for evil in ["../../outside.txt", "/etc/cron.d/x", "C:/Windows/x.dll"] {
            let bad = vec![FileOp { source: "core/base.esp".into(), dest: evil.into(), ..Default::default() }];
            assert!(materialize(&bad, &root, &out).is_err(), "{evil}");
        }
        assert!(!d.join("outside.txt").exists());
        // a source that leaves the archive is not read
        fs::write(d.join("secret.txt"), "S").unwrap();
        let sneak = vec![FileOp { source: "../secret.txt".into(), dest: "s.txt".into(), ..Default::default() }];
        let (n3, miss3) = materialize(&sneak, &root, &out).unwrap();
        assert_eq!(n3, 0);
        assert_eq!(miss3.len(), 1);
        assert!(!out.join("s.txt").exists());
    }

    #[test]
    fn the_script_is_found_whatever_the_case_or_wrapping_folder() {
        let d = TempDir::new("fomod_find");
        fs::create_dir_all(d.join("Mod Name 1.0/FOMOD")).unwrap();
        fs::write(d.join("Mod Name 1.0/FOMOD/moduleconfig.XML"), SCRIPT).unwrap();
        let (script, root) = find_module(d.path()).unwrap();
        assert!(script.ends_with("moduleconfig.XML") && root.ends_with("Mod Name 1.0"));
        assert!(load(&script).is_ok());
        let e = TempDir::new("fomod_none");
        fs::create_dir_all(e.join("Data")).unwrap();
        assert!(find_module(e.path()).is_none());
    }

    #[test]
    fn utf16_scripts_and_sloppy_xml_are_read() {
        let utf16le: Vec<u8> = [0xFFu8, 0xFE].into_iter().chain(SCRIPT.encode_utf16().flat_map(|u| u.to_le_bytes())).collect();
        assert!(parse(&decode(&utf16le)).is_ok());
        let utf16be: Vec<u8> = [0xFEu8, 0xFF].into_iter().chain(SCRIPT.encode_utf16().flat_map(|u| u.to_be_bytes())).collect();
        assert!(parse(&decode(&utf16be)).is_ok());
        let bom: Vec<u8> = [0xEFu8, 0xBB, 0xBF].into_iter().chain(SCRIPT.bytes()).collect();
        assert!(parse(&decode(&bom)).is_ok());
        // a bare ampersand in a name
        let sloppy = SCRIPT.replace("Cool Overhaul", "Rock & Roll");
        assert_eq!(parse(&sloppy).unwrap().name, "Rock & Roll");
        // different letter case in tags and attributes
        let shouting = SCRIPT.replace("<plugin ", "<PLUGIN ").replace("</plugin>", "</PLUGIN>").replace("source=", "SOURCE=");
        assert_eq!(parse(&shouting).unwrap().steps[0].groups[0].plugins.len(), 2);
    }

    #[test]
    fn scripts_that_make_no_sense_are_refused_and_nothing_panics() {
        for bad in ["", "not xml", "<config/>", "<other><x/></other>", "<config><installSteps/></config>", "<config><moduleName>x</moduleName></config>"] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
        // only required files, no questions: fine
        let m = parse("<config><requiredInstallFiles><file source=\"a\"/></requiredInstallFiles></config>").unwrap();
        assert!(m.steps.is_empty());
        assert_eq!(m.required[0].dest, "a", "no destination means the same path");
        let _ = decode(&[0xFF, 0xFE, 0x00]);
        let _ = decode(&[]);
        // a deeply nested dependency tree does not recurse forever
        let deep = format!("<config><installSteps><installStep name=\"s\"><visible>{}<flagDependency flag=\"a\" value=\"b\"/>{}</visible><optionalFileGroups/></installStep></installSteps></config>", "<dependencies>".repeat(200), "</dependencies>".repeat(200));
        let _ = parse(&deep);
    }

    #[test]
    fn damaged_scripts_never_panic() {
        let w = world(&[]);
        let run = |text: &str| {
            if let Ok(m) = parse(text) {
                let r = resolve(&m, &Selection::new(), &w);
                let _ = install_files(&m, &r, &w);
                let mut odd = Selection::new();
                odd.insert("9:9".into(), vec![0, 99]);
                odd.insert("0:0".into(), vec![usize::MAX]);
                let _ = complete(&resolve(&m, &odd, &w));
            }
        };
        // cut off anywhere
        for i in (0..SCRIPT.len()).step_by(5) {
            if SCRIPT.is_char_boundary(i) {
                run(&SCRIPT[..i]);
            }
        }
        // one piece taken out anywhere
        let chars: Vec<char> = SCRIPT.chars().collect();
        for start in (0..chars.len()).step_by(37) {
            let cut: String = chars[..start].iter().chain(chars[(start + 23).min(chars.len())..].iter()).collect();
            run(&cut);
        }
        // tags swapped for each other
        for (a, b) in [("plugin", "group"), ("group", "plugins"), ("installStep", "plugin"), ("file ", "folder ")] {
            run(&SCRIPT.replace(a, b));
        }
    }

    #[test]
    fn select_all_and_required_plugins_cannot_be_unticked() {
        let script = r#"<config><installSteps><installStep name="s"><optionalFileGroups>
          <group name="All" type="SelectAll"><plugins><plugin name="a"><files><file source="a"/></files></plugin><plugin name="b"><files><file source="b"/></files></plugin></plugins></group>
          <group name="Req" type="SelectAny"><plugins><plugin name="r"><typeDescriptor><type name="Required"/></typeDescriptor><files><file source="r"/></files></plugin><plugin name="o"><files><file source="o"/></files></plugin></plugins></group>
          <group name="One" type="SelectAtLeastOne"><plugins><plugin name="x"/><plugin name="y"/></plugins></group>
          </optionalFileGroups></installStep></installSteps></config>"#;
        let m = parse(script).unwrap();
        let w = world(&[]);
        let mut sel = Selection::new();
        sel.insert("0:0".into(), vec![]);
        sel.insert("0:1".into(), vec![1]);
        sel.insert("0:2".into(), vec![]);
        let r = resolve(&m, &sel, &w);
        assert_eq!(r.selection["0:0"], vec![0, 1]);
        assert_eq!(r.selection["0:1"], vec![0, 1], "the required one is added to the user's pick");
        assert!(!complete(&r), "at least one is needed in the last group");
        assert_eq!(r.steps[0].groups[2].problem, "Pick at least one.");
    }
}
