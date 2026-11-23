//! Parsing of cizon.toml.
//!
//! Current boundary: only the five sections `[project]`, `[dependencies]`, `[compiler]`,
//! `[subproject]` and `[[target]]` are recognized. Any other section is a loud error and is
//! never silently ignored — a typo or a "supported in a future version" must surface on the
//! spot, instead of producing a build that looks successful while the dependency was never added.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize, Default)]
pub struct Config {
    pub project: Option<Project>,
    pub dependencies: Option<BTreeMap<String, DepInfo>>,
    pub compiler: Option<CompilerSection>,
    /// `[subproject]`: name → project-relative directory of a nested cizon project whose
    /// targets are merged into this project's build plan (see `load_resolved`).
    #[serde(default, rename = "subproject")]
    pub subprojects: Option<BTreeMap<String, String>>,
    /// `[[target]]`: one bin plus multiple static libraries; when absent the project stays
    /// the implicit single-executable one.
    #[serde(default, rename = "target")]
    pub targets: Option<Vec<Target>>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub name: Option<String>,
    /// Project version (metadata only); currently used neither for compilation nor for artifact
    /// naming, reserved for install/export.
    #[serde(default)]
    pub version: Option<String>,
    pub edition: Option<String>,
    /// Explicit source file list (relative to the project root). When absent, src/ is scanned
    /// recursively for .cpp/.cc/.cxx.
    #[serde(default)]
    pub sources: Option<Vec<String>>,
    /// Whether to generate compile_commands.json in the project root (and fill in a missing
    /// VS Code IntelliSense configuration).
    #[serde(default)]
    pub compile_commands: Option<bool>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    Bin,
    StaticLib,
    SharedLib,
    /// Built and run only by `cizon test`; `build` / `run` ignore it. The pass contract is the
    /// process exit code (0 = pass) — no framework parsing.
    Test,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Target {
    /// Required for static_lib/shared_lib; optional for bin, defaulting to [project].name.
    #[serde(default)]
    pub name: Option<String>,
    pub kind: TargetKind,
    /// The directory of the cizon.toml that declared this target; sources, include and prefix
    /// resolve against it, not against the consuming project's root. Set by `parse` (the root
    /// project's own directory for its own targets); never read from the file.
    #[serde(skip)]
    pub base_dir: PathBuf,
    /// A file or a directory; directories are scanned recursively. Defaults to src/ for bin,
    /// required for static_lib/shared_lib.
    #[serde(default)]
    pub sources: Option<Vec<String>>,
    /// Bin/test: static/shared library target names, linked in declaration order.
    /// Shared library: static library target names, built into the DLL.
    /// Static library: public static dependencies; their includes and archives propagate.
    #[serde(default)]
    pub links: Vec<String>,
    /// Test targets only: names of system-installed libraries, mapped verbatim to `-l<name>` at
    /// link time. cizon neither downloads nor locates them; headers must already be on the
    /// compiler's default include path (non-default prefixes go through [compiler] flags).
    #[serde(default)]
    pub extern_links: Vec<String>,
    /// Optional project-relative public include directory. For libraries it is part of the
    /// target's public interface and is inherited through `links`; bins/tests use it only for
    /// their own sources. The project-root include/ remains automatic for every target.
    #[serde(default)]
    pub include: Option<String>,
    /// Optional installed-package prefix (project-relative directory). cizon expands it to
    /// `-I<prefix>/include` for this target's sources and `-L<prefix>/lib` at link time, so
    /// consuming an installed library never requires hand-written search-path flags.
    #[serde(default)]
    pub prefix: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct CompilerSection {
    pub cpp: Option<String>,
    /// Convenience switch: expand to `-Wall -Wextra -Wpedantic` for project sources (not deps).
    #[serde(default)]
    pub strict: bool,
    /// Convenience switch: append `-Werror` (warnings become errors) for project sources.
    #[serde(default)]
    pub werror: bool,
    #[serde(default)]
    pub flags: Vec<String>,
    #[serde(default)]
    pub link_flags: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct DepInfo {
    pub git: String,
    pub tag: Option<String>,
}

/// [dependencies] has been implemented since v0.2; every section outside this table is a loud
/// error.
const FUTURE_SECTIONS: &[&str] = &[
    "dev-dependencies",
    "workspace",
    "override",
    "patch",
    "features",
    "pch",
    "build",
];

/// Configuration sections that were never supported — their presence counts as a typo.
const UNKNOWN_HINT: &str =
    "cizon only recognizes [project], [dependencies], [compiler], [subproject] and [[target]]";

/// A dependency name is also the directory name under `deps/`, so only characters that are safe
/// in the intersection with TOML bare keys are allowed.
pub(crate) fn is_valid_dep_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Both the project name and a target name end up in an artifact file name: each must be a single
/// file name that cannot escape the build scope.
fn validate_file_component(name: &str, what: &str) -> Result<(), String> {
    const RESERVED: &[&str] = &[
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7",
        "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    const INTERNAL: &[&str] = &["obj", "lib", "deps-obj"];
    let bad_char = name
        .chars()
        .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'));
    let stem = name.split('.').next().unwrap_or("").to_ascii_lowercase();
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.starts_with('.')
        || bad_char
        || name.ends_with(' ')
        || name.ends_with('.')
        || RESERVED.contains(&stem.as_str())
        || INTERNAL.iter().any(|n| name.eq_ignore_ascii_case(n))
    {
        return Err(format!(
            "{what} {name:?} cannot be used as a file name: empty names, path separators, a leading dot and Windows reserved characters/device names are not allowed, and it must not conflict with internal directories such as obj/lib/deps-obj."
        ));
    }
    Ok(())
}

pub(crate) fn validate_project_name(name: &str) -> Result<(), String> {
    validate_file_component(name, "project name")
}

fn validate_target_name(name: &str) -> Result<(), String> {
    validate_file_component(name, "target name")
}

/// A extern_links entry is appended to the link command verbatim as `-l<name>`, so it must not be
/// able to smuggle flags or path pieces into that command line.
fn validate_system_link_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "extern_links entry {name:?} is invalid: it is appended to the link command as -l<name>, so only letters, digits and . - _ + are allowed (no leading dash, no whitespace, no path separators)."
        ))
    }
}

/// Validate and resolve `[[target]]`: a single bin without a name uses the project name; with
/// multiple bins every bin must be named explicitly; libraries must be named explicitly.
fn validate_targets(cfg: &mut Config, project_name: &str) -> Result<(), String> {
    let Some(targets) = &mut cfg.targets else {
        return Ok(());
    };
    if targets.is_empty() {
        return Err("[[target]] must not be empty.".into());
    }
    if cfg
        .project
        .as_ref()
        .is_some_and(|p| p.sources.is_some())
    {
        return Err("[project].sources and [[target]] cannot be used together: put the source files in the corresponding target.".into());
    }

    // Library-only projects are valid: `build` produces the declared libraries, `run` reports
    // that there is no bin, and `test` reports that there is no test target.
    let bin_count = targets.iter().filter(|t| t.kind == TargetKind::Bin).count();
    for target in targets.iter_mut() {
        match target.kind {
            TargetKind::Bin => {
                if target.name.is_none() {
                    if bin_count == 1 {
                        target.name = Some(project_name.to_string());
                    } else {
                        return Err(
                            "with multiple bin targets, every bin must provide name.".into()
                        );
                    }
                }
            }
            TargetKind::StaticLib | TargetKind::SharedLib => {
                if target.name.is_none() {
                    return Err("a library target must provide name.".into());
                }
            }
            TargetKind::Test => {
                if target.name.is_none() {
                    return Err("a test target must provide name.".into());
                }
            }
        }
        validate_target_name(target.name.as_deref().expect("name resolved"))?;
    }

    let mut names = HashSet::new();
    for target in targets.iter() {
        let name = target.name.as_deref().expect("name resolved");
        if !names.insert(name.to_lowercase()) {
            return Err(format!("duplicate target name {name} (case-insensitive)."));
        }
        if target.kind != TargetKind::Test && !target.extern_links.is_empty() {
            return Err(format!(
                "only test targets can declare extern_links; target {name} cannot (it is a {kind} target).",
                kind = match target.kind {
                    TargetKind::Bin => "bin",
                    TargetKind::StaticLib => "static_lib",
                    TargetKind::SharedLib => "shared_lib",
                    TargetKind::Test => "test",
                }
            ));
        }
        match target.kind {
            TargetKind::Bin => {}
            TargetKind::StaticLib => {
                if target.sources.as_ref().is_none_or(|s| s.is_empty()) {
                    return Err(format!("static library target {name} must provide sources."));
                }
            }
            TargetKind::SharedLib => {
                if target.sources.as_ref().is_none_or(|s| s.is_empty()) {
                    return Err(format!("shared library target {name} must provide sources."));
                }
            }
            TargetKind::Test => {
                if target.sources.as_ref().is_none_or(|s| s.is_empty()) {
                    return Err(format!("test target {name} must provide sources."));
                }
                let mut seen_system = HashSet::new();
                for link in &target.extern_links {
                    validate_system_link_name(link)?;
                    if !seen_system.insert(link.to_lowercase()) {
                        return Err(format!(
                            "duplicate entry {link} in the extern_links of test target {name}."
                        ));
                    }
                }
            }
        }
        if let Some(sources) = &target.sources {
            if matches!(target.kind, TargetKind::Bin | TargetKind::Test) && sources.is_empty() {
                return Err(format!(
                    "the sources of {kind} target {name} must not be empty.",
                    kind = if target.kind == TargetKind::Bin {
                        "bin"
                    } else {
                        "test"
                    }
                ));
            }
        }
    }
    Ok(())
}

/// Validate `links` against the given target set: static libraries link only static
/// libraries, shared libraries link only static libraries, bins/tests link libraries.
/// `root` is the project root; a target declared in another file gets a source suffix naming
/// its cizon.toml, so the error points at the file to fix. `load_resolved` runs this on the
/// merged target set, where a link may resolve to a subproject's library.
fn validate_target_links(targets: &[Target], root: &Path) -> Result<(), String> {
    let mut libs = HashSet::new();
    let mut shared = HashSet::new();
    for target in targets {
        let name = target.name.as_deref().expect("config resolves target names");
        match target.kind {
            TargetKind::StaticLib => {
                libs.insert(name.to_lowercase());
            }
            TargetKind::SharedLib => {
                shared.insert(name.to_lowercase());
            }
            _ => {}
        }
    }
    for target in targets.iter() {
        let name = target.name.as_deref().expect("config resolves target names");
        let src = target_source_suffix(root, target);
        let mut seen = HashSet::new();
        match target.kind {
            TargetKind::StaticLib => {
                for link in &target.links {
                    let lower = link.to_lowercase();
                    if !libs.contains(&lower) {
                        if shared.contains(&lower) {
                            return Err(format!(
                                "static library target {name}{src} cannot link shared library {link}; static libraries can only depend on static libraries."
                            ));
                        }
                        return Err(format!(
                            "static library target {name}{src} links a nonexistent static library {link}."
                        ));
                    }
                    if !seen.insert(lower) {
                        return Err(format!(
                            "duplicate entry {link} in the links of static library target {name}{src}."
                        ));
                    }
                }
            }
            TargetKind::SharedLib => {
                for link in &target.links {
                    let lower = link.to_lowercase();
                    if !libs.contains(&lower) {
                        if shared.contains(&lower) {
                            return Err(format!(
                                "shared library target {name}{src} cannot link shared library {link}."
                            ));
                        }
                        return Err(format!(
                            "shared library target {name}{src} links a nonexistent static library {link}."
                        ));
                    }
                    if !seen.insert(lower) {
                        return Err(format!(
                            "duplicate entry {link} in the links of shared library target {name}{src}."
                        ));
                    }
                }
            }
            TargetKind::Bin | TargetKind::Test => {
                let kind = if target.kind == TargetKind::Bin { "bin" } else { "test" };
                for link in &target.links {
                    let lower = link.to_lowercase();
                    if !libs.contains(&lower) && !shared.contains(&lower) {
                        return Err(format!(
                            "{kind} target {name}{src} links a nonexistent library {link}."
                        ));
                    }
                    if !seen.insert(lower) {
                        return Err(format!(
                            "duplicate entry {link} in the links of {kind} target {name}{src}."
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

/// The cizon.toml that declared a target, relative to the project root; empty for the root
/// project's own targets, so single-project error messages are unchanged.
fn target_source_suffix(root: &Path, target: &Target) -> String {
    if target.base_dir == *root {
        String::new()
    } else {
        format!(" (declared in {})", toml_label(root, &target.base_dir))
    }
}

/// A cizon.toml path for error messages, relative to the project root.
fn toml_label(root: &Path, dir: &Path) -> String {
    match dir.strip_prefix(root) {
        Ok(rel) if !rel.as_os_str().is_empty() => format!(
            "{}/cizon.toml",
            rel.to_string_lossy().replace('\\', "/")
        ),
        _ => "cizon.toml".into(),
    }
}

/// A directory path for error messages, relative to the project root and slash-normalized.
fn dir_label(root: &Path, dir: &Path) -> String {
    match dir.strip_prefix(root) {
        Ok(rel) if !rel.as_os_str().is_empty() => rel.to_string_lossy().replace('\\', "/"),
        _ => dir.display().to_string(),
    }
}

fn validate_edition(edition: &str) -> Result<(), String> {
    let version = edition
        .strip_prefix("c++")
        .or_else(|| edition.strip_prefix("gnu++"))
        .unwrap_or(edition);
    const VALID: &[&str] = &[
        "98", "03", "11", "14", "17", "20", "23", "26", "0x", "1y", "1z", "2a", "2b", "2c",
    ];
    if VALID.contains(&version) {
        return Ok(());
    }
    Err(format!(
        "invalid [project].edition \"{edition}\". Use a C++ standard such as \"17\", \"c++20\" or \"gnu++23\" \
         (versions: 98, 03, 11, 14, 17, 20, 23, 26; aliases: 0x, 1y, 1z, 2a, 2b, 2c)."
    ))
}

/// Parse one cizon.toml with per-file validation only. Link existence is NOT checked here:
/// a target may link a target declared in another file (a subproject), so links are
/// validated on the merged set by `load_resolved` (which is also what single-project
/// callers use: for a project without subprojects it is exactly parse plus link checks).
/// Every target records the directory of the file that declared it (`base_dir`); the plan
/// resolves that target's sources, include and prefix against it.
fn parse(dir: &Path) -> Result<Config, String> {
    let path = dir.join("cizon.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "not found {}: this directory is not a cizon project. Run `cizon new <name>` first or write cizon.toml by hand.",
                path.display()
            ));
        }
        Err(e) => return Err(format!("failed to read {}: {e}", path.display())),
    };

    let table: toml::Table = text
        .parse()
        .map_err(|e| format!("failed to parse cizon.toml:\n{e}"))?;

    if table.get("target").is_some_and(|v| v.is_table()) {
        return Err(
            "target in cizon.toml must be an array: use [[target]] (double brackets), not [target]."
                .into(),
        );
    }

    for key in table.keys() {
        if key == "project"
            || key == "dependencies"
            || key == "compiler"
            || key == "subproject"
            || key == "target"
        {
            continue;
        }
        if FUTURE_SECTIONS.contains(&key.as_str()) {
            return Err(format!(
                "cizon.toml contains the configuration section [{key}], which is outside the current boundary.\n\
                 The matching capability (PCH / features / workspace and the like) is not implemented yet; remove that section or comment it out.",
                key = key
            ));
        }
        return Err(format!(
            "cizon.toml contains the unknown configuration section [{key}]. {UNKNOWN_HINT}; check the spelling.",
            key = key
        ));
    }

    let mut cfg: Config = toml::Value::Table(table)
        .try_into::<Config>()
        .map_err(|e| format!("cizon.toml has a structural error:\n{e}"))?;

    let effective_name = match cfg.project.as_ref().and_then(|p| p.name.as_deref()) {
        Some(name) => name.to_string(),
        None => dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "app".into()),
    };
    validate_project_name(&effective_name)?;
    if let Some(version) = cfg.project.as_ref().and_then(|p| p.version.as_deref()) {
        if version.trim().is_empty() {
            return Err("[project].version must not be empty.".into());
        }
        if version.chars().any(|c| c.is_control()) {
            return Err("[project].version must not contain control characters.".into());
        }
    }
    if let Some(edition) = cfg.edition() {
        validate_edition(edition)?;
    }
    if let Some(deps) = &cfg.dependencies {
        for name in deps.keys() {
            if !is_valid_dep_name(name) {
                return Err(format!(
                    "dependency name {name:?} is invalid: only letters, digits, - and _ are allowed (it is also the directory name under deps/)."
                ));
            }
        }
    }
    if let Some(subs) = &cfg.subprojects {
        for (name, raw) in subs {
            if !is_valid_dep_name(name) {
                return Err(format!(
                    "subproject name {name:?} is invalid: only letters, digits, - and _ are allowed."
                ));
            }
            if raw.trim().is_empty() {
                return Err(format!("subproject {name} path must not be empty."));
            }
            // Unlike sources/include/prefix, a subproject path may climb with `..` (a
            // subproject includes a sibling this way: B and C both include ../d). The real
            // guard is collection time: the resolved directory must sit inside the root
            // project, checked by canonicalization in `collect_subproject`.
            let rel = Path::new(raw);
            let bad_component = rel.components().any(|c| {
                !matches!(
                    c,
                    std::path::Component::Normal(_)
                        | std::path::Component::CurDir
                        | std::path::Component::ParentDir
                )
            });
            if rel.is_absolute() || bad_component {
                return Err(format!(
                    "subproject {name} path must be a project-relative path (no absolute path): {raw}"
                ));
            }
        }
    }
    validate_targets(&mut cfg, &effective_name)?;
    if let Some(targets) = &mut cfg.targets {
        for target in targets.iter_mut() {
            target.base_dir = dir.to_path_buf();
        }
    }
    Ok(cfg)
}

/// One parsed project file in the subproject graph.
struct ResolvedFile {
    /// Canonical directory of this project's cizon.toml.
    dir: PathBuf,
    /// The `[subproject]` key that included it; None for the root project.
    name: Option<String>,
    cfg: Config,
}

impl ResolvedFile {
    /// The file's cizon.toml, relative to the project root, for error messages.
    fn label(&self, root: &Path) -> String {
        toml_label(root, &self.dir)
    }
}

/// Load the root project and recursively merge its `[subproject]` graph into one Config:
/// every subproject's targets join the root's target list (each keeping its own `base_dir`),
/// and every subproject's `[dependencies]` join the root's dependency set. The root
/// project's `[compiler]` and `[project].edition` win; a subproject's `[compiler]` or a
/// differing `[project].edition` is ignored (with a warning).
/// This is what `build` / `run` / `test` / `sync` plan from, so a broken subproject graph
/// fails every command before any compilation starts, with the offending file named.
/// For a project without subprojects this is exactly parse plus link validation.
///
/// Rules enforced here — they are the feature, not an afterthought:
/// - a target name must be unique across the whole merged graph (case-insensitive); a clash
///   names both files.
/// - a subproject may not declare a bin target; bins live only in the root project.
/// - inclusion cycles (A includes B, B includes A, transitively) are errors; a diamond
///   (A includes B and C, B and C include D) includes D once, by canonical directory.
/// - the same directory reached through different path spellings is included once.
/// - a dependency name used with a different git URL or tag is an error; identical
///   declarations deduplicate.
/// - a subproject's `[project].edition` that differs from the root's effective edition
///   is ignored, with a warning; the root's edition applies to every target.
/// - `[subproject]` requires `[[target]]` in the declaring file, and a subproject must
///   declare at least one target; a subproject directory must sit inside the root project.
pub fn load_resolved(dir: &Path) -> Result<Config, String> {
    // Keep the caller's path spelling for target base_dir. Canonical paths are used only for
    // graph identity/containment checks; on Windows they carry a `\\?\` prefix that does not
    // prefix-match the ordinary project root used by the pipeline.
    let root_input = dir.to_path_buf();
    let root = std::fs::canonicalize(dir)
        .map_err(|e| format!("Cannot resolve project root {}: {e}", dir.display()))?;
    let mut files: Vec<ResolvedFile> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut stack: Vec<PathBuf> = Vec::new();
    collect_subproject(&root, &root, None, &mut seen, &mut stack, &mut files)?;

    // A project that declares subprojects must be explicit: the implicit single-executable
    // project has no links field, so it could never consume a subproject library. A
    // subproject that declares nothing contributes nothing and would be silently ignored.
    for file in &files {
        let has_targets = file.cfg.targets().is_some_and(|t| !t.is_empty());
        let has_subs = file.cfg.subprojects().is_some_and(|m| !m.is_empty());
        if has_subs && !has_targets {
            return Err(format!(
                "[subproject] in {} requires [[target]]: the implicit single-executable project cannot link subproject libraries.",
                file.label(&root)
            ));
        }
        if file.name.is_some() && !has_targets {
            return Err(format!(
                "subproject {} ({}) declares no [[target]]; a subproject must provide at least one library or test target.",
                file.name.as_deref().unwrap_or("?"),
                file.label(&root)
            ));
        }
    }

    let mut iter = files.into_iter();
    let root_file = iter.next().expect("collect_subproject always yields the root first");
    let mut merged = root_file.cfg;
    let mut targets = merged.targets.take().unwrap_or_default();
    let mut deps = merged.dependencies.take().unwrap_or_default();
    let mut dep_source: HashMap<String, String> = deps
        .keys()
        .map(|k| (k.clone(), "cizon.toml".to_string()))
        .collect();
    // The root's effective edition (c++23 when undeclared) is what every target
    // compiles with; a subproject's differing edition is warned about, not applied.
    let root_edition = merged.edition().unwrap_or("c++23").to_string();
    for file in iter {
        let label = file.label(&root);
        if file.cfg.compiler.is_some() {
            eprintln!(
                "Warning: subproject {} ({label}) declares [compiler], which is ignored; the root project's compiler and flags apply.",
                file.name.as_deref().unwrap_or("?")
            );
        }
        if let Some(warning) = subproject_edition_warning(
            file.name.as_deref().unwrap_or("?"),
            &label,
            &file.cfg,
            &root_edition,
        ) {
            eprintln!("{warning}");
        }
        if let Some(list) = file.cfg.targets() {
            for target in list {
                if target.kind == TargetKind::Bin {
                    return Err(format!(
                        "subproject {} ({label}) declares a bin target {}; bins live only in the root project — a subproject provides static_lib, shared_lib and test targets.",
                        file.name.as_deref().unwrap_or("?"),
                        target.name.as_deref().unwrap_or("?")
                    ));
                }
            }
            targets.extend(list.iter().cloned());
        }
        if let Some(map) = file.cfg.dependencies() {
            for (name, info) in map {
                match deps.get(name) {
                    Some(existing) if existing.git != info.git || existing.tag != info.tag => {
                        let first = dep_source
                            .get(name)
                            .map(|s| s.as_str())
                            .unwrap_or("cizon.toml");
                        return Err(format!(
                            "dependency {name} is declared with different sources: {} (from {first}) and {} (from {label}). A dependency name must resolve to one git URL and tag across the project and its subprojects.",
                            describe_dep(existing),
                            describe_dep(info)
                        ));
                    }
                    Some(_) => {} // Identical declaration: deduplicate.
                    None => {
                        dep_source.insert(name.clone(), label.clone());
                        deps.insert(name.clone(), info.clone());
                    }
                }
            }
        }
    }

    // Target names must be unique across the whole merged graph; within-file duplicates
    // were already rejected by parse, so a clash here is always across files.
    {
        let mut seen_names: HashMap<String, (String, String)> = HashMap::new();
        for target in &targets {
            let name = target.name.as_deref().expect("config resolves target names");
            let label = toml_label(&root, &target.base_dir);
            if let Some((_, first_label)) =
                seen_names.insert(name.to_lowercase(), (name.to_string(), label.clone()))
            {
                return Err(format!(
                    "duplicate target name {name:?}: declared in {first_label} and in {label}. Target names must be unique across the root project and its subprojects (case-insensitive)."
                ));
            }
        }
    }

    validate_target_links(&targets, &root)?;
    // Error labels above use canonical paths; the plan below must use the caller's spelling.
    for target in &mut targets {
        let name = target.name.as_deref().expect("config resolves target names");
        let rel = target
            .base_dir
            .strip_prefix(&root)
            .map_err(|_| format!("internal: target {name} base path is outside the project root"))?;
        target.base_dir = if rel.as_os_str().is_empty() {
            root_input.clone()
        } else {
            root_input.join(rel)
        };
    }
    if !targets.is_empty() {
        merged.targets = Some(targets);
    }
    if !deps.is_empty() {
        merged.dependencies = Some(deps);
    }
    Ok(merged)
}

fn describe_dep(info: &DepInfo) -> String {
    match &info.tag {
        Some(tag) => format!("git = {:?}, tag = {tag:?}", info.git),
        None => format!("git = {:?}", info.git),
    }
}

/// Canonical C++ version for an edition spelling: "c++17"/"17" -> "17", and the
/// pre-release aliases map to their standard ("2b" -> "23"). Used only to decide
/// whether a subproject's edition differs from the root's effective edition.
fn edition_version(edition: &str) -> &str {
    let version = edition
        .strip_prefix("c++")
        .or_else(|| edition.strip_prefix("gnu++"))
        .unwrap_or(edition);
    match version {
        "0x" => "11",
        "1y" => "14",
        "1z" => "17",
        "2a" => "20",
        "2b" => "23",
        "2c" => "26",
        other => other,
    }
}

/// Warning for a subproject that declares [project].edition: the merged build
/// compiles every target with the root project's edition, so a differing
/// subproject edition would otherwise be silently dropped. None when the
/// subproject declares no edition, or one equal to the root's effective edition
/// (spelling- and alias-insensitive: "17" == "c++17", "2b" == "c++23").
fn subproject_edition_warning(
    name: &str,
    label: &str,
    cfg: &Config,
    root_edition: &str,
) -> Option<String> {
    let edition = cfg.edition()?;
    if edition_version(edition) == edition_version(root_edition) {
        return None;
    }
    Some(format!(
        "Warning: subproject {name} ({label}) declares [project].edition = {edition:?}, which is ignored; the root project's edition ({root_edition:?}) applies."
    ))
}

/// Depth-first walk of the subproject graph, in inclusion order (a file is recorded before
/// its own subprojects). `seen` deduplicates by canonical directory (diamond); `stack`
/// holds the current inclusion path for cycle detection.
fn collect_subproject(
    dir: &Path,
    root: &Path,
    name: Option<String>,
    seen: &mut HashSet<PathBuf>,
    stack: &mut Vec<PathBuf>,
    out: &mut Vec<ResolvedFile>,
) -> Result<(), String> {
    let canonical = std::fs::canonicalize(dir).map_err(|e| match &name {
        Some(n) => format!(
            "subproject {n} directory {} does not exist or cannot be read: {e}",
            dir.display()
        ),
        None => format!("Cannot resolve project root {}: {e}", dir.display()),
    })?;
    if stack.contains(&canonical) {
        let mut chain: Vec<String> = stack.iter().map(|d| toml_label(root, d)).collect();
        chain.push(toml_label(root, &canonical));
        return Err(format!(
            "subproject cycle: {}. A subproject must not (transitively) include itself or an ancestor.",
            chain.join(" -> ")
        ));
    }
    if !seen.insert(canonical.clone()) {
        return Ok(()); // Diamond: already collected through another path.
    }
    // A malformed subproject file must name the subproject, or the error is unactionable.
    let cfg = parse(&canonical).map_err(|e| match &name {
        Some(n) => format!("subproject {n} ({}): {e}", toml_label(root, &canonical)),
        None => e,
    })?;
    let subs: Vec<(String, PathBuf)> = match cfg.subprojects() {
        Some(map) => map
            .iter()
            .map(|(n, rel)| (n.clone(), canonical.join(rel)))
            .collect(),
        None => Vec::new(),
    };
    stack.push(canonical.clone());
    out.push(ResolvedFile { dir: canonical.clone(), name, cfg });
    for (sub_name, sub_dir) in subs {
        // `sub_dir.join("cizon.toml").is_file()` is also false when the path is a file
        // (ENOTDIR), so name that case first instead of blaming a missing cizon.toml.
        if sub_dir.is_file() {
            return Err(format!(
                "subproject {sub_name} at {} is a file, not a directory (declared in {}).",
                dir_label(root, &sub_dir),
                toml_label(root, &canonical)
            ));
        }
        if !sub_dir.join("cizon.toml").is_file() {
            return Err(format!(
                "subproject {sub_name} at {} has no cizon.toml (declared in {}).",
                dir_label(root, &sub_dir),
                toml_label(root, &canonical)
            ));
        }
        let sub_canonical = std::fs::canonicalize(&sub_dir).map_err(|e| {
            format!(
                "Cannot resolve subproject {sub_name} directory {}: {e}",
                dir_label(root, &sub_dir)
            )
        })?;
        if !sub_canonical.starts_with(root) {
            return Err(format!(
                "subproject {sub_name} at {} resolves outside the project ({}). Subprojects must live inside the project directory.",
                dir_label(root, &sub_dir),
                dir_label(root, &sub_canonical)
            ));
        }
        collect_subproject(&sub_dir, root, Some(sub_name), seen, stack, out)?;
    }
    stack.pop();
    Ok(())
}

impl Config {
    /// Project name: [project].name wins, the directory name is the default.
    pub fn name(&self, dir: &Path) -> String {
        if let Some(name) = self.project.as_ref().and_then(|p| p.name.as_deref()) {
            return name.to_string();
        }
        dir.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "app".into())
    }

    pub fn edition(&self) -> Option<&str> {
        self.project.as_ref().and_then(|p| p.edition.as_deref())
    }

    pub fn explicit_sources(&self) -> Option<&[String]> {
        self.project.as_ref().and_then(|p| p.sources.as_deref())
    }

    /// The `[[target]]` list; None means the traditional implicit single-executable project.
    pub fn targets(&self) -> Option<&[Target]> {
        self.targets.as_deref()
    }

    /// Off by default: not every developer needs a compilation database.
    pub fn compile_commands(&self) -> bool {
        self.project
            .as_ref()
            .and_then(|p| p.compile_commands)
            .unwrap_or(false)
    }

    pub fn flags(&self) -> &[String] {
        self.compiler.as_ref().map(|c| c.flags.as_slice()).unwrap_or(&[])
    }

    pub fn link_flags(&self) -> &[String] {
        self.compiler.as_ref().map(|c| c.link_flags.as_slice()).unwrap_or(&[])
    }

    pub fn strict(&self) -> bool {
        self.compiler.as_ref().map(|c| c.strict).unwrap_or(false)
    }

    pub fn werror(&self) -> bool {
        self.compiler.as_ref().map(|c| c.werror).unwrap_or(false)
    }

    pub fn explicit_compiler(&self) -> Option<&str> {
        self.compiler.as_ref().and_then(|c| c.cpp.as_deref())
    }

    /// The declared git dependencies ([dependencies]); BTreeMap keeps the iteration order stable.
    pub fn dependencies(&self) -> Option<&BTreeMap<String, DepInfo>> {
        self.dependencies.as_ref()
    }

    /// The `[subproject]` table (name → project-relative directory); BTreeMap keeps the
    /// iteration order stable, so the merge order is deterministic.
    pub fn subprojects(&self) -> Option<&BTreeMap<String, String>> {
        self.subprojects.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn write_config(text: &str) -> (PathBuf, Config) {
        let dir = temp_dir("parse");
        std::fs::write(dir.join("cizon.toml"), text).unwrap();
        // Single-file parse only: a [subproject] entry names directories that need not
        // exist for a parse test; resolution is load_resolved's job, tested separately.
        let cfg = parse(&dir).unwrap();
        (dir, cfg)
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cizon-cfg-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parse_minimal() {
        let (dir, cfg) = write_config("[project]\nname = \"hello\"\n");
        assert_eq!(cfg.name(&dir), "hello");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn project_version_is_metadata_only() {
        let (dir, cfg) = write_config("[project]\nname = \"a\"\nversion = \"1.2.3\"\n");
        assert_eq!(
            cfg.project.as_ref().and_then(|p| p.version.as_deref()),
            Some("1.2.3")
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn legacy_project_sources_load_without_targets() {
        let (dir, cfg) = write_config("[project]\nname = \"a\"\nsources = [\"app\"]\n");
        assert!(cfg.targets().is_none());
        assert_eq!(
            cfg.explicit_sources(),
            Some(&["app".to_string()][..])
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn empty_project_version_rejected() {
        let dir = temp_dir("empty-version");
        std::fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"a\"\nversion = \"  \"\n",
        )
        .unwrap();
        assert!(load_resolved(&dir).unwrap_err().contains("must not be empty"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn name_defaults_to_dir_name() {
        let (dir, cfg) = write_config("[project]\n");
        let fallback = Path::new(&dir);
        assert_eq!(cfg.name(fallback), dir.file_name().unwrap().to_string_lossy());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn parse_compiler_section() {
        let (dir, cfg) = write_config(
            "[project]\nname = \"a\"\nedition = \"c++17\"\n\n\
             [compiler]\ncpp = \"g++\"\nflags = [\"-O2\"]\nlink_flags = [\"-lm\"]\n",
        );
        assert_eq!(cfg.edition(), Some("c++17"));
        assert_eq!(cfg.explicit_compiler(), Some("g++"));
        assert_eq!(cfg.flags(), &["-O2"]);
        assert_eq!(cfg.link_flags(), &["-lm"]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn parse_dependencies_section() {
        let (dir, cfg) = write_config(
            "[project]\nname = \"a\"\n\n\
             [dependencies]\nfmt = { git = \"https://example.com/fmtlib/fmt\", tag = \"11.0.2\" }\n",
        );
        let deps = cfg.dependencies().unwrap();
        assert_eq!(
            deps.get("fmt").map(|d| (d.git.as_str(), d.tag.as_deref())),
            Some(("https://example.com/fmtlib/fmt", Some("11.0.2")))
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn future_section_rejected_loudly() {
        let dir = temp_dir("future-rej");
        std::fs::write(dir.join("cizon.toml"), "[project]\nname=\"a\"\n\n[workspace]\nmembers=[]\n")
            .unwrap();
        let err = load_resolved(&dir).unwrap_err();
        assert!(err.contains("[workspace]"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn typo_section_rejected_loudly() {
        let dir = temp_dir("typo-rej");
        std::fs::write(dir.join("cizon.toml"), "[dependancies]\n").unwrap();
        let err = load_resolved(&dir).unwrap_err();
        assert!(err.contains("[dependancies]"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();
    }

    /// MossKV case: the real key is cpp, and writing compiler was silently ignored by serde.
    #[test]
    fn unknown_key_in_compiler_section_rejected() {
        let dir = temp_dir("unknown-compiler");
        std::fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"a\"\n\n[compiler]\ncompiler = \"clang++\"\n",
        )
        .unwrap();
        let err = load_resolved(&dir).unwrap_err();
        assert!(err.contains("unknown field"), "got: {err}");
        assert!(err.contains("cpp"), "the error should name the correct key cpp: {err}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unknown_key_in_project_section_rejected() {
        let dir = temp_dir("unknown-project");
        std::fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"a\"\nlicence = \"MIT\"\n",
        )
        .unwrap();
        let err = load_resolved(&dir).unwrap_err();
        assert!(err.contains("licence"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unknown_key_in_dependency_entry_rejected() {
        let dir = temp_dir("unknown-dep-key");
        std::fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"a\"\n\n[dependencies]\n\
             fmt = { git = \"https://example.com/fmt\", branch = \"main\" }\n",
        )
        .unwrap();
        let err = load_resolved(&dir).unwrap_err();
        assert!(err.contains("branch"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn compile_commands_defaults_off_and_can_be_enabled() {
        let (dir, cfg) = write_config("[project]\nname = \"a\"\n");
        assert!(!cfg.compile_commands());
        std::fs::remove_dir_all(dir).ok();

        let (dir, cfg) = write_config("[project]\nname = \"a\"\ncompile_commands = true\n");
        assert!(cfg.compile_commands());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn parse_targets_single_bin_and_libs() {
        let (dir, cfg) = write_config(
            "[project]\nname = \"t\"\n\n\
             [[target]]\nname = \"foo\"\nkind = \"static_lib\"\nsources = [\"lib/foo.cpp\"]\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"foo\"]\n",
        );
        let targets = cfg.targets().expect("targets");
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].name.as_deref(), Some("foo"));
        assert_eq!(targets[0].kind, TargetKind::StaticLib);
        assert_eq!(targets[0].sources.as_deref(), Some(&["lib/foo.cpp".to_string()][..]));
        assert_eq!(targets[1].kind, TargetKind::Bin);
        assert_eq!(targets[1].links, ["foo"]);
        std::fs::remove_dir_all(dir).ok();
    }

    fn target_config(targets: &str) -> String {
        format!("[project]\nname = \"t\"\n\n{targets}")
    }

    #[test]
    fn parse_shared_lib_target_and_bin_links() {
        let (dir, cfg) = write_config(&target_config(
            "[[target]]\nname = \"core\"\nkind = \"static_lib\"\nsources = [\"lib\"]\n\n\
             [[target]]\nname = \"core_dll\"\nkind = \"shared_lib\"\nsources = [\"capi\"]\nlinks = [\"core\"]\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"core_dll\", \"core\"]\n",
        ));
        let targets = cfg.targets().expect("targets");
        assert_eq!(targets[1].kind, TargetKind::SharedLib);
        assert_eq!(targets[1].links, ["core"]);
        assert_eq!(targets[2].links, ["core_dll", "core"]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn multiple_bins_can_link_different_libraries() {
        let (dir, cfg) = write_config(&target_config(
            "[[target]]\nname = \"foo\"\nkind = \"static_lib\"\nsources = [\"foo\"]\n\n\
             [[target]]\nname = \"bar\"\nkind = \"static_lib\"\nsources = [\"bar\"]\n\n\
             [[target]]\nname = \"a\"\nkind = \"bin\"\nsources = [\"a\"]\nlinks = [\"foo\"]\n\n\
             [[target]]\nname = \"b\"\nkind = \"bin\"\nsources = [\"b\"]\nlinks = [\"bar\"]\n",
        ));
        let targets = cfg.targets().expect("targets");
        assert_eq!(targets[2].links, ["foo"]);
        assert_eq!(targets[3].links, ["bar"]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn shared_lib_links_only_static_libraries() {
        let dir = temp_dir("shared-links-shared");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config(
                "[[target]]\nname = \"other\"\nkind = \"shared_lib\"\nsources = [\"other\"]\n\n\
                 [[target]]\nname = \"core_dll\"\nkind = \"shared_lib\"\nsources = [\"capi\"]\nlinks = [\"other\"]\n\n\
                 [[target]]\nname = \"app\"\nkind = \"bin\"\n",
            ),
        )
        .unwrap();
        let err = load_resolved(&dir).unwrap_err();
        assert!(err.contains("cannot link shared library"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn shared_lib_requires_name_and_sources() {
        let dir = temp_dir("shared-no-name");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config(
                "[[target]]\nkind = \"shared_lib\"\nsources = [\"capi\"]\n\n\
                 [[target]]\nname = \"app\"\nkind = \"bin\"\n",
            ),
        )
        .unwrap();
        assert!(load_resolved(&dir).unwrap_err().contains("must provide name"));
        std::fs::remove_dir_all(dir).ok();

        let dir = temp_dir("shared-no-sources");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config(
                "[[target]]\nname = \"core_dll\"\nkind = \"shared_lib\"\n\n\
                 [[target]]\nname = \"app\"\nkind = \"bin\"\n",
            ),
        )
        .unwrap();
        assert!(load_resolved(&dir).unwrap_err().contains("must provide sources"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn target_named_shared_is_allowed() {
        let (dir, cfg) = write_config(&target_config(
            "[[target]]\nname = \"shared\"\nkind = \"shared_lib\"\nsources = [\"capi\"]\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"shared\"]\n",
        ));
        assert!(cfg.targets().is_some());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn bin_target_name_defaults_to_project_name() {
        let (dir, cfg) = write_config(
            "[project]\nname = \"myapp\"\n\n\
             [[target]]\nkind = \"bin\"\nsources = [\"src\"]\n",
        );
        let bin = cfg
            .targets()
            .unwrap()
            .iter()
            .find(|t| t.kind == TargetKind::Bin)
            .unwrap();
        assert_eq!(bin.name.as_deref(), Some("myapp"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn static_lib_target_requires_name() {
        let dir = temp_dir("lib-no-name");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config(
                "[[target]]\nkind = \"static_lib\"\nsources = [\"lib/x.cpp\"]\n\n\
                 [[target]]\nname = \"app\"\nkind = \"bin\"\n",
            ),
        )
        .unwrap();
        assert!(load_resolved(&dir).unwrap_err().contains("must provide name"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn targets_allow_library_only_and_require_bin_names() {
        let dir = temp_dir("lib-only");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config("[[target]]\nname = \"foo\"\nkind = \"static_lib\"\nsources = [\"lib/foo.cpp\"]\n"),
        )
        .unwrap();
        let cfg = load_resolved(&dir).unwrap();
        assert_eq!(cfg.targets().unwrap()[0].kind, TargetKind::StaticLib);
        std::fs::remove_dir_all(dir).ok();

        let dir = temp_dir("shared-only");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config("[[target]]\nname = \"dll\"\nkind = \"shared_lib\"\nsources = [\"capi\"]\n"),
        )
        .unwrap();
        assert!(load_resolved(&dir).is_ok(), "a shared-library-only project is valid");
        std::fs::remove_dir_all(dir).ok();

        let dir = temp_dir("two-bin");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config(
                "[[target]]\nname = \"a\"\nkind = \"bin\"\n\n\
                 [[target]]\nname = \"b\"\nkind = \"bin\"\n",
            ),
        )
        .unwrap();
        assert!(load_resolved(&dir).is_ok(), "multiple named bins must be accepted");
        std::fs::remove_dir_all(dir).ok();

        let dir = temp_dir("two-bin-unnamed");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config(
                "[[target]]\nname = \"a\"\nkind = \"bin\"\n\n\
                 [[target]]\nkind = \"bin\"\n",
            ),
        )
        .unwrap();
        let err = load_resolved(&dir).unwrap_err();
        assert!(err.contains("every bin must provide name"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn target_links_and_sources_are_validated() {
        let dir = temp_dir("bad-links");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config(
                "[[target]]\nname = \"foo\"\nkind = \"static_lib\"\nsources = [\"lib/foo.cpp\"]\n\n\
                 [[target]]\nname = \"app\"\nkind = \"bin\"\nlinks = [\"missing\"]\n",
            ),
        )
        .unwrap();
        assert!(load_resolved(&dir).unwrap_err().contains("nonexistent"));
        std::fs::remove_dir_all(dir).ok();

        let dir = temp_dir("lib-links-missing");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config(
                "[[target]]\nname = \"foo\"\nkind = \"static_lib\"\nsources = [\"lib/foo.cpp\"]\nlinks = [\"x\"]\n\n\
                 [[target]]\nname = \"app\"\nkind = \"bin\"\n",
            ),
        )
        .unwrap();
        assert!(load_resolved(&dir).unwrap_err().contains("nonexistent static library"));
        std::fs::remove_dir_all(dir).ok();

        let dir = temp_dir("lib-links");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config(
                "[[target]]\nname = \"foo\"\nkind = \"static_lib\"\nsources = [\"lib/foo.cpp\"]\n\n\
                 [[target]]\nname = \"bar\"\nkind = \"static_lib\"\nsources = [\"lib/bar.cpp\"]\nlinks = [\"foo\"]\n\n\
                 [[target]]\nname = \"app\"\nkind = \"bin\"\n",
            ),
        )
        .unwrap();
        assert!(load_resolved(&dir).is_ok(), "static libraries may depend on static libraries");
        std::fs::remove_dir_all(dir).ok();

        let dir = temp_dir("lib-links-shared");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config(
                "[[target]]\nname = \"foo\"\nkind = \"static_lib\"\nsources = [\"lib/foo.cpp\"]\nlinks = [\"dll\"]\n\n\
                 [[target]]\nname = \"dll\"\nkind = \"shared_lib\"\nsources = [\"capi\"]\n\n\
                 [[target]]\nname = \"app\"\nkind = \"bin\"\n",
            ),
        )
        .unwrap();
        assert!(load_resolved(&dir).unwrap_err().contains("cannot link shared library"));
        std::fs::remove_dir_all(dir).ok();

        let dir = temp_dir("lib-no-sources");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config(
                "[[target]]\nname = \"foo\"\nkind = \"static_lib\"\n\n\
                 [[target]]\nname = \"app\"\nkind = \"bin\"\n",
            ),
        )
        .unwrap();
        assert!(load_resolved(&dir).unwrap_err().contains("must provide sources"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn targets_reject_duplicate_names_and_project_sources() {
        let dir = temp_dir("dup-targets");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config(
                "[[target]]\nname = \"x\"\nkind = \"static_lib\"\nsources = [\"lib/x.cpp\"]\n\n\
                 [[target]]\nname = \"x\"\nkind = \"bin\"\n",
            ),
        )
        .unwrap();
        assert!(load_resolved(&dir).unwrap_err().contains("duplicate"));
        std::fs::remove_dir_all(dir).ok();

        let dir = temp_dir("targets-sources");
        std::fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\nsources = [\"src/main.cpp\"]\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\n",
        )
        .unwrap();
        assert!(load_resolved(&dir).unwrap_err().contains("cannot be used together"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unknown_key_in_target_rejected() {
        let dir = temp_dir("unknown-target-key");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config("[[target]]\nname = \"app\"\nkind = \"bin\"\nextra = true\n"),
        )
        .unwrap();
        let err = load_resolved(&dir).unwrap_err();
        assert!(err.contains("extra"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn single_table_target_rejected_with_hint() {
        let dir = temp_dir("single-target-table");
        std::fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n[target]\nname = \"app\"\nkind = \"bin\"\n",
        )
        .unwrap();
        let err = load_resolved(&dir).unwrap_err();
        assert!(err.contains("[[target]]"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn target_names_are_case_insensitive_and_reserved_checked() {
        let dir = temp_dir("case-targets");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config(
                "[[target]]\nname = \"Foo\"\nkind = \"static_lib\"\nsources = [\"lib/foo.cpp\"]\n\n\
                 [[target]]\nname = \"foo\"\nkind = \"bin\"\n",
            ),
        )
        .unwrap();
        assert!(load_resolved(&dir).unwrap_err().contains("duplicate"));
        std::fs::remove_dir_all(dir).ok();

        let dir = temp_dir("reserved-target");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config("[[target]]\nname = \"obj\"\nkind = \"bin\"\n"),
        )
        .unwrap();
        assert!(load_resolved(&dir).unwrap_err().contains("internal"));
        std::fs::remove_dir_all(dir).ok();

        // links are matched case-insensitively
        let dir = temp_dir("case-links");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config(
                "[[target]]\nname = \"Foo\"\nkind = \"static_lib\"\nsources = [\"lib/foo.cpp\"]\n\n\
                 [[target]]\nname = \"app\"\nkind = \"bin\"\nlinks = [\"FOO\"]\n",
            ),
        )
        .unwrap();
        assert!(load_resolved(&dir).is_ok());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn target_names_use_unicode_case_folding() {
        let dir = temp_dir("unicode-targets");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config(
                "[[target]]\nname = \"Ä\"\nkind = \"static_lib\"\nsources = [\"lib/a.cpp\"]\n\n\
                 [[target]]\nname = \"ä\"\nkind = \"static_lib\"\nsources = [\"lib/b.cpp\"]\n\n\
                 [[target]]\nname = \"app\"\nkind = \"bin\"\n",
            ),
        )
        .unwrap();
        assert!(load_resolved(&dir).unwrap_err().contains("duplicate"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn parse_test_target_with_extern_links() {
        let (dir, cfg) = write_config(&target_config(
            "[[target]]\nname = \"core\"\nkind = \"static_lib\"\nsources = [\"lib\"]\n\n\
             [[target]]\nname = \"calc_test\"\nkind = \"test\"\nsources = [\"tests\"]\nlinks = [\"core\"]\nextern_links = [\"gtest\", \"gtest_main\"]\n",
        ));
        let targets = cfg.targets().unwrap();
        assert_eq!(targets[1].kind, TargetKind::Test);
        assert_eq!(targets[1].links, ["core"]);
        assert_eq!(targets[1].extern_links, ["gtest", "gtest_main"]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn invalid_edition_is_rejected_before_the_compiler() {
        let dir = temp_dir("bad-edition");
        std::fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\nedition = \"23x\"\n",
        )
        .unwrap();
        let err = load_resolved(&dir).unwrap_err();
        assert!(err.contains("[project].edition"), "got: {err}");
        assert!(err.contains("invalid"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();

        let (dir, cfg) = write_config("[project]\nname = \"t\"\nedition = \"2b\"\n");
        assert_eq!(cfg.edition(), Some("2b"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn parse_strict_werror_and_prefix() {
        let (dir, cfg) = write_config(
            "[project]\nname = \"t\"\n\n[compiler]\nstrict = true\nwerror = true\n\n\
             [[target]]\nname = \"t_test\"\nkind = \"test\"\nsources = [\"tests\"]\ninclude = \"libs/test/include\"\nprefix = \"third_party/pkg\"\n",
        );
        assert!(cfg.strict());
        assert!(cfg.werror());
        assert_eq!(
            cfg.targets().unwrap()[0].include.as_deref(),
            Some("libs/test/include")
        );
        assert_eq!(
            cfg.targets().unwrap()[0].prefix.as_deref(),
            Some("third_party/pkg")
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn test_only_project_is_allowed() {
        // Test targets are executable targets: a test-only project parses fine (build still
        // builds libraries; the test command produces and runs the exe).
        let (dir, cfg) = write_config(&target_config(
            "[[target]]\nname = \"t_test\"\nkind = \"test\"\nsources = [\"tests\"]\n",
        ));
        let targets = cfg.targets().unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].kind, TargetKind::Test);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn test_target_requires_name_and_sources() {
        let dir = temp_dir("test-no-name");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config("[[target]]\nkind = \"test\"\nsources = [\"tests\"]\n"),
        )
        .unwrap();
        assert!(load_resolved(&dir).unwrap_err().contains("a test target must provide name"));
        std::fs::remove_dir_all(dir).ok();

        let dir = temp_dir("test-no-sources");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config("[[target]]\nname = \"t_test\"\nkind = \"test\"\n"),
        )
        .unwrap();
        assert!(load_resolved(&dir).unwrap_err().contains("must provide sources"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn test_target_links_are_validated_like_bin() {
        let dir = temp_dir("test-bad-links");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config(
                "[[target]]\nname = \"t_test\"\nkind = \"test\"\nsources = [\"tests\"]\nlinks = [\"missing\"]\n",
            ),
        )
        .unwrap();
        let err = load_resolved(&dir).unwrap_err();
        assert!(err.contains("test target"), "got: {err}");
        assert!(err.contains("nonexistent"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn extern_links_rejected_outside_test_targets() {
        for targets in [
            "[[target]]\nname = \"app\"\nkind = \"bin\"\nextern_links = [\"gtest\"]\n",
            "[[target]]\nname = \"core\"\nkind = \"static_lib\"\nsources = [\"lib\"]\nextern_links = [\"gtest\"]\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\n",
            "[[target]]\nname = \"core_dll\"\nkind = \"shared_lib\"\nsources = [\"capi\"]\nextern_links = [\"gtest\"]\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\n",
        ] {
            let dir = temp_dir("system-non-test");
            std::fs::write(dir.join("cizon.toml"), target_config(targets)).unwrap();
            let err = load_resolved(&dir).unwrap_err();
            assert!(
                err.contains("only test targets can declare extern_links"),
                "targets={targets} got: {err}"
            );
            std::fs::remove_dir_all(dir).ok();
        }
    }

    #[test]
    fn extern_links_rejects_flag_and_path_smuggling() {
        for bad in ["", "-lm", "g test", "a/b", "a\\b", "gtest;rm"] {
            let dir = temp_dir("system-bad-name");
            std::fs::write(
                dir.join("cizon.toml"),
                target_config(&format!(
                    "[[target]]\nname = \"t_test\"\nkind = \"test\"\nsources = [\"tests\"]\nextern_links = [{bad:?}]\n"
                )),
            )
            .unwrap();
            let err = load_resolved(&dir).unwrap_err();
            assert!(
                err.contains("extern_links entry"),
                "bad={bad:?} got: {err}"
            );
            std::fs::remove_dir_all(dir).ok();
        }

        // Legitimate names must pass: dots, dashes, underscores and plus signs are common in
        // real external library names.
        let dir = temp_dir("system-good-name");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config(
                "[[target]]\nname = \"t_test\"\nkind = \"test\"\nsources = [\"tests\"]\nextern_links = [\"gtest_main\", \"Catch2\", \"libstdc++\"]\n",
            ),
        )
        .unwrap();
        assert!(load_resolved(&dir).is_ok());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn extern_links_rejects_duplicates() {
        let dir = temp_dir("system-dup");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config(
                "[[target]]\nname = \"t_test\"\nkind = \"test\"\nsources = [\"tests\"]\nextern_links = [\"gtest\", \"GTEST\"]\n",
            ),
        )
        .unwrap();
        let err = load_resolved(&dir).unwrap_err();
        assert!(err.contains("duplicate"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn missing_file_gives_actionable_message() {
        let dir = temp_dir("missing-proj");
        let err = load_resolved(&dir).unwrap_err();
        assert!(err.contains("not found"), "got: {err}");
        assert!(err.contains("not a cizon project"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn read_error_is_not_reported_as_missing() {
        let dir = temp_dir("bad-utf8");
        // Non-UTF-8 content: reading the file fails, and that must not be disguised as
        // "cizon.toml not found".
        std::fs::write(dir.join("cizon.toml"), [0xFFu8, 0xFE]).unwrap();
        let err = load_resolved(&dir).unwrap_err();
        assert!(err.contains("failed to read"), "got: {err}");
        assert!(!err.contains("not found"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn project_name_rejects_path_escape() {
        let dir = temp_dir("name-escape");
        for bad in ["../evil", "a/b", "a\\b", "C:\\x", "aux", ""] {
            std::fs::write(
                dir.join("cizon.toml"),
                format!("[project]\nname = {bad:?}\n"),
            )
            .unwrap();
            let err = load_resolved(&dir).unwrap_err();
            assert!(err.contains("file name"), "name={bad:?} got: {err}");
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn dependency_name_rejects_path_traversal() {
        let dir = temp_dir("dep-name-escape");
        for bad in ["..", "a/b", "a\\b", ""] {
            std::fs::write(
                dir.join("cizon.toml"),
                format!("[project]\nname = \"a\"\n\n[dependencies]\n{bad:?} = {{ git = \"x\" }}\n"),
            )
            .unwrap();
            let err = load_resolved(&dir).unwrap_err();
            assert!(err.contains("dependency name"), "dep={bad:?} got: {err}");
        }
        std::fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"a\"\n\n[dependencies]\nmy-dep_1 = { git = \"x\" }\n",
        )
        .unwrap();
        assert!(load_resolved(&dir).is_ok(), "a valid dependency name should be accepted");
        std::fs::remove_dir_all(dir).ok();
    }

    // ── [subproject] ──────────────────────────────────────────────────────────

    fn write_file(dir: &Path, rel: &str, text: &str) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, text).unwrap();
    }

    fn lib_target(name: &str) -> String {
        format!(
            "[[target]]\nname = \"{name}\"\nkind = \"static_lib\"\nsources = [\"src\"]\n"
        )
    }

    fn lib_toml(name: &str) -> String {
        format!("[project]\nname = \"{name}\"\n\n{}", lib_target(name))
    }

    fn bin_target(links: &[&str]) -> String {
        format!(
            "[[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [{}]\n",
            links
                .iter()
                .map(|l| format!("\"{l}\""))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }

    #[test]
    fn parse_subproject_table() {
        let (dir, cfg) = write_config(
            "[project]\nname = \"t\"\n\n[subproject]\nnet = \"libs/net\"\nstorage = \"libs/storage\"\n",
        );
        let subs = cfg.subprojects().unwrap();
        assert_eq!(subs.get("net").map(|s| s.as_str()), Some("libs/net"));
        assert_eq!(subs.get("storage").map(|s| s.as_str()), Some("libs/storage"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn subproject_name_and_path_validation() {
        // Invalid names are rejected.
        let dir = temp_dir("sub-bad-name");
        std::fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"a\"\n\n[subproject]\n\"my net\" = \"libs/net\"\n",
        )
        .unwrap();
        let err = load_resolved(&dir).unwrap_err();
        assert!(err.contains("subproject name"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();

        // Empty and absolute paths are rejected; `..` is allowed (a subproject includes a
        // sibling that way) — the inside-project guard runs at collection time.
        for (raw, needle) in [("", "must not be empty"), ("/etc", "project-relative")] {
            let dir = temp_dir("sub-bad-path");
            std::fs::write(
                dir.join("cizon.toml"),
                format!("[project]\nname = \"a\"\n\n[subproject]\nnet = {raw:?}\n"),
            )
            .unwrap();
            let err = load_resolved(&dir).unwrap_err();
            assert!(err.contains(needle), "raw={raw:?} got: {err}");
            std::fs::remove_dir_all(dir).ok();
        }
        let (dir, _) = write_config("[project]\nname = \"a\"\n\n[subproject]\nnet = \"../shared\"\n");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn load_resolved_matches_load_without_subprojects() {
        let (dir, cfg) = write_config(&target_config(
            "[[target]]\nname = \"foo\"\nkind = \"static_lib\"\nsources = [\"lib/foo.cpp\"]\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"foo\"]\n",
        ));
        let resolved = load_resolved(&dir).unwrap();
        assert_eq!(
            resolved.targets().unwrap().len(),
            cfg.targets().unwrap().len()
        );
        assert_eq!(
            resolved.targets().unwrap()[0].name.as_deref(),
            Some("foo")
        );
        assert!(resolved.subprojects().is_none());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn load_resolved_keeps_caller_spelling_for_base_dirs() {
        let dir = temp_dir("sub-base-spelling");
        write_file(
            &dir,
            "cizon.toml",
            "[project]\nname = \"app\"\n\n[subproject]\nnet = \"libs/net\"\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"net\"]\n",
        );
        write_file(
            &dir,
            "libs/net/cizon.toml",
            "[project]\nname = \"net\"\n\n\
             [[target]]\nname = \"net\"\nkind = \"static_lib\"\nsources = [\"src\"]\n",
        );
        let cfg = load_resolved(&dir).unwrap();
        let by_name = |name: &str| {
            cfg.targets()
                .unwrap()
                .iter()
                .find(|t| t.name.as_deref() == Some(name))
                .unwrap()
                .base_dir
                .clone()
        };
        assert_eq!(by_name("app"), dir);
        assert_eq!(by_name("net"), dir.join("libs/net"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn load_resolved_rejects_duplicate_target_names_across_files() {
        let dir = temp_dir("sub-dup");
        write_file(&dir, "cizon.toml", &format!(
            "[project]\nname = \"app\"\n\n[subproject]\nother = \"libs/other\"\n\n{}",
            "[[target]]\nname = \"net\"\nkind = \"static_lib\"\nsources = [\"lib\"]\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"net\"]\n"
        ));
        write_file(&dir, "libs/other/cizon.toml", &lib_toml("net"));
        let err = load_resolved(&dir).unwrap_err();
        assert!(err.contains("duplicate target name"), "got: {err}");
        assert!(err.contains("libs/other/cizon.toml"), "the error must name both files: {err}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn load_resolved_rejects_cycle() {
        let dir = temp_dir("sub-cycle");
        write_file(&dir, "cizon.toml", &format!(
            "[project]\nname = \"app\"\n\n[subproject]\na = \"libs/a\"\n\n{}",
            bin_target(&["a_lib"])
        ));
        write_file(&dir, "libs/a/cizon.toml", &format!(
            "[project]\nname = \"a\"\n\n[subproject]\nb = \"b\"\n\n{}",
            lib_target("a_lib")
        ));
        // b includes its parent a via ".." — a cycle.
        write_file(&dir, "libs/a/b/cizon.toml", &format!(
            "[project]\nname = \"b\"\n\n[subproject]\na = \"..\"\n\n{}",
            lib_target("b_lib")
        ));
        let err = load_resolved(&dir).unwrap_err();
        assert!(err.contains("cycle"), "got: {err}");
        assert!(err.contains("libs/a/cizon.toml"), "the chain must name the files: {err}");
        std::fs::remove_dir_all(dir).ok();

        // A project including itself is a cycle too.
        let dir = temp_dir("sub-self-cycle");
        write_file(&dir, "cizon.toml", &format!(
            "[project]\nname = \"app\"\n\n[subproject]\nself = \".\"\n\n{}",
            bin_target(&[])
        ));
        let err = load_resolved(&dir).unwrap_err();
        assert!(err.contains("cycle"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn load_resolved_diamond_includes_shared_once() {
        // A includes B and C; B and C both include D (as a sibling via ".."). D's target
        // must appear exactly once in the merged set.
        let dir = temp_dir("sub-diamond");
        write_file(&dir, "cizon.toml", &format!(
            "[project]\nname = \"app\"\n\n[subproject]\nb = \"libs/b\"\nc = \"libs/c\"\nd = \"libs/d\"\n\n{}",
            bin_target(&["b_lib", "c_lib", "d_lib"])
        ));
        write_file(&dir, "libs/b/cizon.toml", &format!(
            "[project]\nname = \"b\"\n\n[subproject]\nd = \"../d\"\n\n{}",
            lib_target("b_lib")
        ));
        write_file(&dir, "libs/c/cizon.toml", &format!(
            "[project]\nname = \"c\"\n\n[subproject]\nd = \"../d\"\n\n{}",
            lib_target("c_lib")
        ));
        write_file(&dir, "libs/d/cizon.toml", &lib_toml("d_lib"));
        let resolved = load_resolved(&dir).unwrap();
        let mut names: Vec<_> = resolved
            .targets()
            .unwrap()
            .iter()
            .map(|t| t.name.clone().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["app", "b_lib", "c_lib", "d_lib"]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn load_resolved_dedups_same_dir_by_spelling() {
        let dir = temp_dir("sub-spelling");
        write_file(&dir, "cizon.toml", &format!(
            "[project]\nname = \"app\"\n\n[subproject]\nnet = \"libs/net\"\nnet2 = \"./libs/net\"\n\n{}",
            bin_target(&["net"])
        ));
        write_file(&dir, "libs/net/cizon.toml", &lib_toml("net"));
        let resolved = load_resolved(&dir).unwrap();
        let count = resolved
            .targets()
            .unwrap()
            .iter()
            .filter(|t| t.name.as_deref() == Some("net"))
            .count();
        assert_eq!(count, 1, "the same directory must be included once");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    #[cfg(unix)]
    fn load_resolved_dedups_same_dir_by_symlink() {
        // The same directory reached through a symlink inside the project is
        // included once: canonicalize feeds the `seen` set, so the second entry
        // is a diamond, not a second copy of the target.
        let dir = temp_dir("sub-symlink-dedup");
        write_file(&dir, "cizon.toml", &format!(
            "[project]\nname = \"app\"\n\n[subproject]\nnet = \"libs/net\"\nnet2 = \"libs/alias\"\n\n{}",
            bin_target(&["net"])
        ));
        write_file(&dir, "libs/net/cizon.toml", &lib_toml("net"));
        std::os::unix::fs::symlink(dir.join("libs/net"), dir.join("libs/alias")).unwrap();
        let resolved = load_resolved(&dir).unwrap();
        let count = resolved
            .targets()
            .unwrap()
            .iter()
            .filter(|t| t.name.as_deref() == Some("net"))
            .count();
        assert_eq!(count, 1, "the same directory must be included once");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn load_resolved_rejects_bin_in_subproject() {
        let dir = temp_dir("sub-bin");
        write_file(&dir, "cizon.toml", &format!(
            "[project]\nname = \"app\"\n\n[subproject]\nother = \"libs/other\"\n\n{}",
            bin_target(&[])
        ));
        write_file(&dir, "libs/other/cizon.toml",
            "[project]\nname = \"other\"\n\n[[target]]\nname = \"other\"\nkind = \"bin\"\nsources = [\"src\"]\n");
        let err = load_resolved(&dir).unwrap_err();
        assert!(err.contains("bin target"), "got: {err}");
        assert!(err.contains("other"), "got: {err}");
        assert!(err.contains("libs/other/cizon.toml"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn load_resolved_ignores_subproject_compiler() {
        // A subproject naming a compiler that does not exist must not fail loading: the
        // root project's compiler applies. (The build-level test proves the build itself
        // uses the root compiler.)
        let dir = temp_dir("sub-compiler");
        write_file(&dir, "cizon.toml", &format!(
            "[project]\nname = \"app\"\n\n[subproject]\nnet = \"libs/net\"\n\n{}",
            bin_target(&["net"])
        ));
        write_file(&dir, "libs/net/cizon.toml", &format!(
            "[project]\nname = \"net\"\n\n[compiler]\ncpp = \"definitely-not-a-compiler\"\nflags = [\"-DNOPE=1\"]\n\n{}",
            lib_target("net")
        ));
        let resolved = load_resolved(&dir).unwrap();
        assert_eq!(resolved.explicit_compiler(), None);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn subproject_edition_warning_fires_only_on_difference() {
        let (dir, cfg) = write_config("[project]\nname = \"net\"\nedition = \"c++17\"\n");
        let warning =
            subproject_edition_warning("net", "libs/net/cizon.toml", &cfg, "c++23").unwrap();
        assert!(warning.contains("[project].edition"), "got: {warning}");
        assert!(warning.contains("\"c++17\""), "got: {warning}");
        assert!(warning.contains("\"c++23\""), "got: {warning}");
        assert!(warning.contains("libs/net/cizon.toml"), "got: {warning}");
        // Same edition under a different spelling: no warning.
        assert!(subproject_edition_warning("net", "libs/net/cizon.toml", &cfg, "17").is_none());
        std::fs::remove_dir_all(dir).ok();

        let (dir, cfg) = write_config("[project]\nname = \"net\"\nedition = \"2b\"\n");
        assert!(
            subproject_edition_warning("net", "libs/net/cizon.toml", &cfg, "c++23").is_none(),
            "2b is the pre-release name for C++23"
        );
        std::fs::remove_dir_all(dir).ok();

        let (dir, cfg) = write_config("[project]\nname = \"net\"\n");
        assert!(subproject_edition_warning("net", "libs/net/cizon.toml", &cfg, "c++23").is_none());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn load_resolved_ignores_subproject_edition() {
        // The subproject declares c++17; the merged config keeps the root's c++23.
        // (The warning text is covered by subproject_edition_warning_fires_only_on_
        // difference; the build-level test proves the root edition is what compiles.)
        let dir = temp_dir("sub-edition");
        write_file(&dir, "cizon.toml", &format!(
            "[project]\nname = \"app\"\nedition = \"c++23\"\n\n[subproject]\nnet = \"libs/net\"\n\n{}",
            bin_target(&["net"])
        ));
        write_file(&dir, "libs/net/cizon.toml", &format!(
            "[project]\nname = \"net\"\nedition = \"c++17\"\n\n{}",
            lib_target("net")
        ));
        let resolved = load_resolved(&dir).unwrap();
        assert_eq!(resolved.edition(), Some("c++23"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn load_resolved_merges_subproject_dependencies() {
        // Identical declarations deduplicate.
        let dir = temp_dir("sub-deps-same");
        write_file(&dir, "cizon.toml",
            "[project]\nname = \"app\"\n\n[subproject]\nnet = \"libs/net\"\n\n[dependencies]\nfmt = { git = \"https://example.com/fmt\" }\n\n[[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"net\"]\n");
        write_file(&dir, "libs/net/cizon.toml",
            "[project]\nname = \"net\"\n\n[dependencies]\nfmt = { git = \"https://example.com/fmt\" }\n\n[[target]]\nname = \"net\"\nkind = \"static_lib\"\nsources = [\"src\"]\n");
        let resolved = load_resolved(&dir).unwrap();
        assert_eq!(resolved.dependencies().unwrap().len(), 1);
        std::fs::remove_dir_all(dir).ok();

        // The same name with a different URL is a loud error naming the dependency.
        let dir = temp_dir("sub-deps-conflict");
        write_file(&dir, "cizon.toml",
            "[project]\nname = \"app\"\n\n[subproject]\nnet = \"libs/net\"\n\n[dependencies]\nfmt = { git = \"https://example.com/fmt\" }\n\n[[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"net\"]\n");
        write_file(&dir, "libs/net/cizon.toml",
            "[project]\nname = \"net\"\n\n[dependencies]\nfmt = { git = \"https://example.com/other-fmt\" }\n\n[[target]]\nname = \"net\"\nkind = \"static_lib\"\nsources = [\"src\"]\n");
        let err = load_resolved(&dir).unwrap_err();
        assert!(err.contains("fmt"), "got: {err}");
        assert!(err.contains("different sources"), "got: {err}");
        assert!(err.contains("libs/net/cizon.toml"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();

        // A subproject-only dependency joins the set.
        let dir = temp_dir("sub-deps-only");
        write_file(&dir, "cizon.toml", &format!(
            "[project]\nname = \"app\"\n\n[subproject]\nnet = \"libs/net\"\n\n{}",
            bin_target(&["net"])
        ));
        write_file(&dir, "libs/net/cizon.toml",
            "[project]\nname = \"net\"\n\n[dependencies]\ntiny = { git = \"https://example.com/tiny\", tag = \"v1\" }\n\n[[target]]\nname = \"net\"\nkind = \"static_lib\"\nsources = [\"src\"]\n");
        let resolved = load_resolved(&dir).unwrap();
        assert_eq!(
            resolved.dependencies().unwrap().get("tiny").map(|d| d.git.as_str()),
            Some("https://example.com/tiny")
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn load_resolved_nested_subproject() {
        // Root -> mid -> leaf; the leaf's target is usable by the root bin.
        let dir = temp_dir("sub-nested");
        write_file(&dir, "cizon.toml", &format!(
            "[project]\nname = \"app\"\n\n[subproject]\nmid = \"libs/mid\"\n\n{}",
            bin_target(&["leaf"])
        ));
        write_file(&dir, "libs/mid/cizon.toml", &format!(
            "[project]\nname = \"mid\"\n\n[subproject]\nleaf = \"leaf\"\n\n{}",
            lib_target("mid")
        ));
        write_file(&dir, "libs/mid/leaf/cizon.toml", &lib_toml("leaf"));
        let resolved = load_resolved(&dir).unwrap();
        let mut names: Vec<_> = resolved
            .targets()
            .unwrap()
            .iter()
            .map(|t| t.name.clone().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["app", "leaf", "mid"]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn load_resolved_requires_targets_with_subprojects() {
        // The root with [subproject] but no [[target]] cannot link anything.
        let dir = temp_dir("sub-no-targets-root");
        write_file(
            &dir,
            "cizon.toml",
            "[project]\nname = \"app\"\n\n[subproject]\nnet = \"libs/net\"\n",
        );
        write_file(&dir, "libs/net/cizon.toml", &lib_toml("net"));
        let err = load_resolved(&dir).unwrap_err();
        assert!(err.contains("requires [[target]]"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();

        // A subproject declaring no targets contributes nothing and must not be silent.
        let dir = temp_dir("sub-no-targets-sub");
        write_file(&dir, "cizon.toml", &format!(
            "[project]\nname = \"app\"\n\n[subproject]\nempty = \"libs/empty\"\n\n{}",
            bin_target(&[])
        ));
        write_file(&dir, "libs/empty/cizon.toml", "[project]\nname = \"empty\"\n");
        let err = load_resolved(&dir).unwrap_err();
        assert!(err.contains("declares no [[target]]"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn load_resolved_rejects_missing_subproject() {
        // No cizon.toml in the named directory.
        let dir = temp_dir("sub-missing-toml");
        write_file(&dir, "cizon.toml", &format!(
            "[project]\nname = \"app\"\n\n[subproject]\nempty = \"libs/empty\"\n\n{}",
            bin_target(&[])
        ));
        std::fs::create_dir_all(dir.join("libs/empty")).unwrap();
        let err = load_resolved(&dir).unwrap_err();
        assert!(err.contains("has no cizon.toml"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();

        // No such directory at all.
        let dir = temp_dir("sub-missing-dir");
        write_file(&dir, "cizon.toml", &format!(
            "[project]\nname = \"app\"\n\n[subproject]\nghost = \"libs/ghost\"\n\n{}",
            bin_target(&[])
        ));
        let err = load_resolved(&dir).unwrap_err();
        assert!(err.contains("has no cizon.toml"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn load_resolved_rejects_subproject_path_that_is_a_file() {
        // A subproject path naming a file must say so, not blame a missing cizon.toml:
        // `path.join("cizon.toml").is_file()` is false for a file too (ENOTDIR).
        let dir = temp_dir("sub-file-path");
        write_file(&dir, "cizon.toml", &format!(
            "[project]\nname = \"app\"\n\n[subproject]\nnet = \"libs/net\"\n\n{}",
            bin_target(&[])
        ));
        std::fs::create_dir_all(dir.join("libs")).unwrap();
        std::fs::write(dir.join("libs/net"), "not a directory").unwrap();
        let err = load_resolved(&dir).unwrap_err();
        assert!(err.contains("is a file, not a directory"), "got: {err}");
        assert!(!err.contains("has no cizon.toml"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    #[cfg(unix)]
    fn load_resolved_rejects_subproject_outside_project() {
        // A symlinked subproject directory pointing outside the project must be refused:
        // the build never compiles sources outside the project.
        let dir = temp_dir("sub-outside");
        let outside = temp_dir("sub-outside-target");
        write_file(&outside, "cizon.toml", &lib_toml("evil"));
        std::fs::create_dir_all(dir.join("libs")).unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("libs/evil")).unwrap();
        write_file(&dir, "cizon.toml", &format!(
            "[project]\nname = \"app\"\n\n[subproject]\nevil = \"libs/evil\"\n\n{}",
            bin_target(&[])
        ));
        let err = load_resolved(&dir).unwrap_err();
        assert!(err.contains("outside the project"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();
        std::fs::remove_dir_all(outside).ok();
    }

    #[test]
    fn load_resolved_validates_merged_links() {
        // A subproject lib linking a library that exists only in another subproject is
        // fine; linking a nonexistent one names the declaring file.
        let dir = temp_dir("sub-links");
        write_file(&dir, "cizon.toml", &format!(
            "[project]\nname = \"app\"\n\n[subproject]\nnet = \"libs/net\"\nstorage = \"libs/storage\"\n\n{}",
            bin_target(&["net", "storage"])
        ));
        write_file(&dir, "libs/net/cizon.toml", &lib_toml("net"));
        write_file(&dir, "libs/storage/cizon.toml",
            "[project]\nname = \"storage\"\n\n[[target]]\nname = \"storage\"\nkind = \"static_lib\"\nsources = [\"src\"]\nlinks = [\"net\"]\n");
        let resolved = load_resolved(&dir).unwrap();
        assert_eq!(resolved.targets().unwrap().len(), 3);
        std::fs::remove_dir_all(dir).ok();

        let dir = temp_dir("sub-links-bad");
        write_file(&dir, "cizon.toml", &format!(
            "[project]\nname = \"app\"\n\n[subproject]\nstorage = \"libs/storage\"\n\n{}",
            bin_target(&["storage"])
        ));
        write_file(&dir, "libs/storage/cizon.toml",
            "[project]\nname = \"storage\"\n\n[[target]]\nname = \"storage\"\nkind = \"static_lib\"\nsources = [\"src\"]\nlinks = [\"ghost\"]\n");
        let err = load_resolved(&dir).unwrap_err();
        assert!(err.contains("nonexistent static library ghost"), "got: {err}");
        assert!(err.contains("libs/storage/cizon.toml"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();
    }
}
