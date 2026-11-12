//! Parsing of cizon.toml.
//!
//! Current boundary: only the four sections `[project]`, `[dependencies]`, `[compiler]` and
//! `[[target]]` are recognized. Any other section is a loud error and is never silently ignored
//! — a typo or a "supported in a future version" must surface on the spot, instead of producing
//! a build that looks successful while the dependency was never added.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Deserialize, Default)]
pub struct Config {
    pub project: Option<Project>,
    pub dependencies: Option<BTreeMap<String, DepInfo>>,
    pub compiler: Option<CompilerSection>,
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    /// Required for static_lib/shared_lib; optional for bin, defaulting to [project].name.
    #[serde(default)]
    pub name: Option<String>,
    pub kind: TargetKind,
    /// A file or a directory; directories are scanned recursively. Defaults to src/ for bin,
    /// required for static_lib/shared_lib.
    #[serde(default)]
    pub sources: Option<Vec<String>>,
    /// Bin: static/shared library target names, linked in this order within each artifact kind.
    /// Shared library: static library target names, built into the DLL.
    /// Test: same as bin.
    #[serde(default)]
    pub links: Vec<String>,
    /// Test targets only: names of system-installed libraries, mapped verbatim to `-l<name>` at
    /// link time. cizon2 neither downloads nor locates them; headers must already be on the
    /// compiler's default include path (non-default prefixes go through [compiler] flags).
    #[serde(default)]
    pub extern_links: Vec<String>,
    /// Optional installed-package prefix (project-relative directory). cizon2 expands it to
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
    "cizon2 only recognizes [project], [dependencies], [compiler] and [[target]]";

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

    // Tests are executable targets too: a test-only project is valid (build still builds its
    // libraries; only the test command produces an exe), so the "at least one" rule counts both.
    let bin_count = targets.iter().filter(|t| t.kind == TargetKind::Bin).count();
    let exe_count = targets
        .iter()
        .filter(|t| matches!(t.kind, TargetKind::Bin | TargetKind::Test))
        .count();
    if exe_count == 0 {
        return Err(
            "[[target]] must contain at least one kind = \"bin\" or kind = \"test\".".into(),
        );
    }
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
    let mut libs = HashSet::new();
    let mut shared = HashSet::new();
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
                libs.insert(name.to_lowercase());
                if !target.links.is_empty() {
                    return Err(format!(
                        "static library target {name} does not support links; put implementation dependencies in the bin or a shared library."
                    ));
                }
                if target.sources.as_ref().is_none_or(|s| s.is_empty()) {
                    return Err(format!("static library target {name} must provide sources."));
                }
            }
            TargetKind::SharedLib => {
                shared.insert(name.to_lowercase());
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
    for target in targets.iter() {
        let name = target.name.as_deref().expect("name resolved");
        let mut seen = HashSet::new();
        match target.kind {
            TargetKind::StaticLib => continue,
            TargetKind::SharedLib => {
                for link in &target.links {
                    let lower = link.to_lowercase();
                    if !libs.contains(&lower) {
                        if shared.contains(&lower) {
                            return Err(format!(
                                "shared library target {name} cannot link shared library {link}."
                            ));
                        }
                        return Err(format!(
                            "shared library target {name} links a nonexistent static library {link}."
                        ));
                    }
                    if !seen.insert(lower) {
                        return Err(format!(
                            "duplicate entry {link} in the links of shared library target {name}."
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
                            "{kind} target {name} links a nonexistent library {link}."
                        ));
                    }
                    if !seen.insert(lower) {
                        return Err(format!(
                            "duplicate entry {link} in the links of {kind} target {name}."
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

pub fn load(dir: &Path) -> Result<Config, String> {
    let path = dir.join("cizon.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "not found {}: this directory is not a cizon2 project. Run `cizon2 new <name>` first or write cizon.toml by hand.",
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
        if key == "project" || key == "dependencies" || key == "compiler" || key == "target" {
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
    if let Some(deps) = &cfg.dependencies {
        for name in deps.keys() {
            if !is_valid_dep_name(name) {
                return Err(format!(
                    "dependency name {name:?} is invalid: only letters, digits, - and _ are allowed (it is also the directory name under deps/)."
                ));
            }
        }
    }
    validate_targets(&mut cfg, &effective_name)?;
    Ok(cfg)
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn write_config(text: &str) -> (PathBuf, Config) {
        let dir = temp_dir("parse");
        std::fs::write(dir.join("cizon.toml"), text).unwrap();
        let cfg = load(&dir).unwrap();
        (dir, cfg)
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cizon2-cfg-{tag}-{}",
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
        assert!(load(&dir).unwrap_err().contains("must not be empty"));
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
        let err = load(&dir).unwrap_err();
        assert!(err.contains("[workspace]"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn typo_section_rejected_loudly() {
        let dir = temp_dir("typo-rej");
        std::fs::write(dir.join("cizon.toml"), "[dependancies]\n").unwrap();
        let err = load(&dir).unwrap_err();
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
        let err = load(&dir).unwrap_err();
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
        let err = load(&dir).unwrap_err();
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
        let err = load(&dir).unwrap_err();
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
        let err = load(&dir).unwrap_err();
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
        assert!(load(&dir).unwrap_err().contains("must provide name"));
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
        assert!(load(&dir).unwrap_err().contains("must provide sources"));
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
        assert!(load(&dir).unwrap_err().contains("must provide name"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn targets_require_at_least_one_bin_and_names() {
        let dir = temp_dir("no-bin");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config("[[target]]\nname = \"foo\"\nkind = \"static_lib\"\nsources = [\"lib/foo.cpp\"]\n"),
        )
        .unwrap();
        let err = load(&dir).unwrap_err();
        assert!(err.contains("at least one"), "got: {err}");
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
        assert!(load(&dir).is_ok(), "multiple named bins must be accepted");
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
        let err = load(&dir).unwrap_err();
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
        assert!(load(&dir).unwrap_err().contains("nonexistent"));
        std::fs::remove_dir_all(dir).ok();

        let dir = temp_dir("lib-links");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config(
                "[[target]]\nname = \"foo\"\nkind = \"static_lib\"\nsources = [\"lib/foo.cpp\"]\nlinks = [\"x\"]\n\n\
                 [[target]]\nname = \"app\"\nkind = \"bin\"\n",
            ),
        )
        .unwrap();
        assert!(load(&dir).unwrap_err().contains("does not support links"));
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
        assert!(load(&dir).unwrap_err().contains("must provide sources"));
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
        assert!(load(&dir).unwrap_err().contains("duplicate"));
        std::fs::remove_dir_all(dir).ok();

        let dir = temp_dir("targets-sources");
        std::fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\nsources = [\"src/main.cpp\"]\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\n",
        )
        .unwrap();
        assert!(load(&dir).unwrap_err().contains("cannot be used together"));
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
        let err = load(&dir).unwrap_err();
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
        let err = load(&dir).unwrap_err();
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
        assert!(load(&dir).unwrap_err().contains("duplicate"));
        std::fs::remove_dir_all(dir).ok();

        let dir = temp_dir("reserved-target");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config("[[target]]\nname = \"obj\"\nkind = \"bin\"\n"),
        )
        .unwrap();
        assert!(load(&dir).unwrap_err().contains("internal"));
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
        assert!(load(&dir).is_ok());
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
        assert!(load(&dir).unwrap_err().contains("duplicate"));
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
    fn parse_strict_werror_and_prefix() {
        let (dir, cfg) = write_config(
            "[project]\nname = \"t\"\n\n[compiler]\nstrict = true\nwerror = true\n\n\
             [[target]]\nname = \"t_test\"\nkind = \"test\"\nsources = [\"tests\"]\nprefix = \"third_party/pkg\"\n",
        );
        assert!(cfg.strict());
        assert!(cfg.werror());
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
        assert!(load(&dir).unwrap_err().contains("a test target must provide name"));
        std::fs::remove_dir_all(dir).ok();

        let dir = temp_dir("test-no-sources");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config("[[target]]\nname = \"t_test\"\nkind = \"test\"\n"),
        )
        .unwrap();
        assert!(load(&dir).unwrap_err().contains("must provide sources"));
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
        let err = load(&dir).unwrap_err();
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
            let err = load(&dir).unwrap_err();
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
            let err = load(&dir).unwrap_err();
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
        assert!(load(&dir).is_ok());
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
        let err = load(&dir).unwrap_err();
        assert!(err.contains("duplicate"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn missing_file_gives_actionable_message() {
        let dir = temp_dir("missing-proj");
        let err = load(&dir).unwrap_err();
        assert!(err.contains("not found"), "got: {err}");
        assert!(err.contains("not a cizon2 project"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn read_error_is_not_reported_as_missing() {
        let dir = temp_dir("bad-utf8");
        // Non-UTF-8 content: reading the file fails, and that must not be disguised as
        // "cizon.toml not found".
        std::fs::write(dir.join("cizon.toml"), [0xFFu8, 0xFE]).unwrap();
        let err = load(&dir).unwrap_err();
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
            let err = load(&dir).unwrap_err();
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
            let err = load(&dir).unwrap_err();
            assert!(err.contains("dependency name"), "dep={bad:?} got: {err}");
        }
        std::fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"a\"\n\n[dependencies]\nmy-dep_1 = { git = \"x\" }\n",
        )
        .unwrap();
        assert!(load(&dir).is_ok(), "a valid dependency name should be accepted");
        std::fs::remove_dir_all(dir).ok();
    }
}
