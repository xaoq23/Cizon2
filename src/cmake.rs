//! CMake-built git dependencies (v0.8 route).
//!
//! When a dependency checkout has a top-level `CMakeLists.txt`, cizon2 builds it with CMake
//! instead of compiling its flat `src/*.cpp` (the regular layout still covers header-only and
//! flat-source libraries; CMake's territory starts where that layout ends):
//!
//! 1. configure into `deps/.build/<name>` with the File API query `codemodel-v2` active,
//! 2. `cmake --build` and `cmake --install` into `deps/.install/<name>`,
//! 3. read the codemodel reply to discover library targets and their transitive link inputs,
//! 4. record the result in `deps/.build/<name>/cizon-cmake.toml` (the sidecar).
//!
//! The build is skipped when the sidecar fingerprint (dep HEAD + compiler + mode + cmake
//! version) still matches and every recorded artifact exists in the install tree. Consumers
//! get `-I<install>/include` + `-L<install>/lib` automatically; the libraries themselves are
//! picked with a target's `extern_links` (extended to bins in v0.8) and expanded with the
//! transitive link inputs from the sidecar, so `extern_links = ["gtest_main", "gtest"]` links
//! exactly what googletest needs, including its own link dependencies.
//!
//! Known v1 limits (documented, not silent): the dependency must have `install()` rules for
//! its libraries (otherwise there is no install tree to consume); public compile definitions
//! from the codemodel are not forwarded (most libraries carry them in headers; user flags
//! cover the rest); shared-library targets cannot link cmake dependencies yet.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::compiler::Cxx;

/// A library target discovered in a dependency's codemodel.
#[derive(Debug, Clone)]
pub struct CmakeTarget {
    pub name: String,
    pub kind: CmakeTargetKind,
    /// File name of the installed artifact (e.g. `libgtest.a`), in `<install>/lib`.
    pub artifact: String,
    /// Transitive link inputs, in link order: the target's own link libraries, resolved
    /// through the codemodel's target graph (already flattened, duplicates removed).
    pub links: Vec<LinkInput>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmakeTargetKind {
    Static,
    Shared,
}

/// One link input for the consumer's link line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkInput {
    /// A library name: `-l<name>` for GCC/Clang, `<name>.lib` for MSVC.
    Lib(String),
    /// An absolute path (e.g. a system archive), passed verbatim.
    Path(PathBuf),
}

impl LinkInput {
    /// Render for the consumer's link line under the given compiler.
    pub fn render(&self, cxx: &Cxx) -> String {
        match self {
            LinkInput::Lib(name) => cxx.link_lib_flag(name),
            LinkInput::Path(p) => p.display().to_string(),
        }
    }
}

/// Everything a build needs to consume one CMake-built dependency.
#[derive(Debug, Clone)]
pub struct CmakeDep {
    pub install_dir: PathBuf,
    pub targets: Vec<CmakeTarget>,
}

impl CmakeDep {

    /// Library directory for consumers (`<install>/lib`, falling back to `lib64`).
    pub fn lib_dir(&self) -> Option<PathBuf> {
        for cand in ["lib", "lib64"] {
            let dir = self.install_dir.join(cand);
            if dir.is_dir() {
                return Some(dir);
            }
        }
        None
    }

    /// Expand wanted library names (as written in `extern_links`) to the full link inputs:
    /// each name that matches a discovered target contributes the target itself plus its
    /// transitive links; unknown names pass through as plain `-l<name>` entries.
    pub fn expand_links(&self, wanted: &[String]) -> Vec<LinkInput> {
        let mut out: Vec<LinkInput> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for name in wanted {
            let key = name.to_lowercase();
            match self.targets.iter().find(|t| t.name.to_lowercase() == key) {
                Some(target) => {
                    let own = LinkInput::Lib(link_name(&target.artifact));
                    push_unique(&mut out, &mut seen, own);
                    for input in &target.links {
                        push_unique(&mut out, &mut seen, input.clone());
                    }
                }
                None => push_unique(&mut out, &mut seen, LinkInput::Lib(name.clone())),
            }
        }
        out
    }
}

fn push_unique(out: &mut Vec<LinkInput>, seen: &mut HashSet<String>, input: LinkInput) {
    let key = match &input {
        LinkInput::Lib(n) => format!("lib:{n}"),
        LinkInput::Path(p) => format!("path:{}", p.display()),
    };
    if seen.insert(key) {
        out.push(input);
    }
}

/// A dependency is CMake-built when its checkout has a top-level CMakeLists.txt.
pub fn is_cmake_dep(dep_dir: &Path) -> bool {
    dep_dir.join("CMakeLists.txt").is_file()
}

/// CMake build tree for a dependency (a sibling of the checkout, never inside it).
pub fn build_dir(dir: &Path, name: &str) -> PathBuf {
    dir.join("deps").join(".build").join(name)
}

/// CMake install tree for a dependency (a sibling of the checkout, never inside it).
pub fn install_dir(dir: &Path, name: &str) -> PathBuf {
    dir.join("deps").join(".install").join(name)
}

/// Probe the cmake version (`cmake --version`); a clear error when cmake is missing.
pub fn cmake_version() -> Result<String, String> {
    let out = Command::new("cmake")
        .arg("--version")
        .output()
        .map_err(|e| format!("CMake is required to build this dependency (it has a CMakeLists.txt), but cmake was not found: {e}. Install CMake and retry."))?;
    if !out.status.success() {
        return Err("CMake is required to build this dependency (it has a CMakeLists.txt), but `cmake --version` failed. Install CMake and retry.".into());
    }
    let first = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .to_string();
    Ok(first
        .strip_prefix("cmake version ")
        .unwrap_or(&first)
        .trim()
        .to_string())
}

fn mode_name(release: bool) -> &'static str {
    if release {
        "Release"
    } else {
        "Debug"
    }
}

/// The C compiler matching a C++ compiler spec: g++ → gcc, clang++ → clang, cl → cl.
/// A full path keeps its directory (only the file stem is swapped).
fn c_compiler_for(cxx_name: &str) -> String {
    let path = Path::new(cxx_name);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(cxx_name);
    let cc_stem = if stem.contains("clang") {
        stem.replace("clang++", "clang")
    } else if stem.contains("g++") {
        stem.replace("g++", "gcc")
    } else {
        stem.to_string()
    };
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent
            .join(cc_stem)
            .to_string_lossy()
            .into_owned(),
        _ => cc_stem,
    }
}

/// Generator for the dependency build: MSVC uses cmake's default (Visual Studio); otherwise
/// Ninja when available (fast, cross-platform), MinGW Makefiles on Windows, Unix Makefiles.
fn pick_generator(cxx: &Cxx) -> Option<String> {
    if cxx.family().is_msvc() {
        return None;
    }
    let ninja = Command::new("ninja")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if ninja {
        return Some("Ninja".into());
    }
    if cfg!(windows) {
        Some("MinGW Makefiles".into())
    } else {
        Some("Unix Makefiles".into())
    }
}

/// Fingerprint of a dependency build: dep HEAD + compiler version + mode + cmake version.
/// Any change rebuilds the dependency; an unchanged fingerprint with all artifacts present
/// skips cmake entirely.
fn fingerprint(
    dir: &Path,
    name: &str,
    cxx: &Cxx,
    release: bool,
    cmake_ver: &str,
    target: Option<&str>,
    sysroot: Option<&str>,
) -> Result<String, String> {
    let out = Command::new("git")
        .args(["-C", &format!("deps/{name}"), "rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .map_err(|e| format!("failed to read deps/{name} HEAD: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "failed to read deps/{name} HEAD: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let head = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(format!(
        "commit={head}\ncompiler={}\nmode={}\ncmake={cmake_ver}\ntarget={}\nsysroot={}",
        cxx.version,
        mode_name(release),
        target.unwrap_or(""),
        sysroot.unwrap_or(""),
    ))
}

#[derive(Debug, Serialize, Deserialize)]
struct Sidecar {
    fingerprint: String,
    targets: Vec<SidecarTarget>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SidecarTarget {
    name: String,
    kind: String,
    artifact: String,
    /// Link inputs as plain strings: a bare name is a `-l<name>` library, a string containing
    /// a path separator is an absolute path.
    links: Vec<String>,
}

fn sidecar_path(build_dir: &Path) -> PathBuf {
    build_dir.join("cizon-cmake.toml")
}

fn read_sidecar(build_dir: &Path) -> Option<Sidecar> {
    let text = std::fs::read_to_string(sidecar_path(build_dir)).ok()?;
    toml::from_str(&text).ok()
}

fn write_sidecar(build_dir: &Path, sidecar: &Sidecar) -> Result<(), String> {
    let text = toml::to_string(sidecar)
        .map_err(|e| format!("failed to serialize cmake sidecar: {e}"))?;
    std::fs::write(sidecar_path(build_dir), text)
        .map_err(|e| format!("failed to write {}: {e}", sidecar_path(build_dir).display()))
}

fn link_input_from_str(s: &str) -> LinkInput {
    if s.contains('/') || s.contains('\\') {
        LinkInput::Path(PathBuf::from(s))
    } else {
        LinkInput::Lib(s.to_string())
    }
}

fn link_input_to_str(input: &LinkInput) -> String {
    match input {
        LinkInput::Lib(n) => n.clone(),
        LinkInput::Path(p) => p.display().to_string(),
    }
}

/// Ensure a CMake dependency is built and installed, and return its discovered targets.
/// Skips cmake entirely when the sidecar fingerprint matches and all artifacts exist.
pub fn ensure_built(
    dir: &Path,
    name: &str,
    cxx: &Cxx,
    release: bool,
    target: Option<&str>,
    sysroot: Option<&str>,
) -> Result<CmakeDep, String> {
    let dep_dir = dir.join("deps").join(name);
    let bdir = build_dir(dir, name);
    let idir = install_dir(dir, name);
    let cmake_ver = cmake_version()?;
    let fp = fingerprint(dir, name, cxx, release, &cmake_ver, target, sysroot)?;
    if let Some(sidecar) = read_sidecar(&bdir) {
        if sidecar.fingerprint == fp {
            let dep = sidecar_to_dep(&idir, &sidecar);
            if dep
                .targets
                .iter()
                .all(|t| installed_artifact(&idir, &t.artifact).is_some())
            {
                return Ok(dep);
            }
        }
    }
    build_dep(dir, name, &dep_dir, &bdir, &idir, cxx, release, &fp, target, sysroot)
}

fn sidecar_to_dep(install_dir: &Path, sidecar: &Sidecar) -> CmakeDep {
    CmakeDep {
        install_dir: install_dir.to_path_buf(),
        targets: sidecar
            .targets
            .iter()
            .map(|t| CmakeTarget {
                name: t.name.clone(),
                kind: if t.kind == "shared" {
                    CmakeTargetKind::Shared
                } else {
                    CmakeTargetKind::Static
                },
                artifact: t.artifact.clone(),
                links: t.links.iter().map(|s| link_input_from_str(s)).collect(),
            })
            .collect(),
    }
}

/// Where an installed artifact lives: `<install>/lib/<artifact>` first, then `lib64`, then
/// `bin` (Windows DLLs install to bin/).
fn installed_artifact(install_dir: &Path, artifact: &str) -> Option<PathBuf> {
    for sub in ["lib", "lib64", "bin"] {
        let cand = install_dir.join(sub).join(artifact);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

/// Map a target triple to the CMake system name/processor pair used for cross builds.
/// Returns None for triples we do not recognize; CMake then configures without a system
/// file, which still works for compilers that carry the target in their name.
fn cmake_system_for(triple: &str) -> Option<(&'static str, String)> {
    let lower = triple.to_lowercase();
    let arch = lower.split('-').next().unwrap_or("");
    // Unknown arches pass through verbatim (owned, because `arch` borrows the local `lower`).
    let processor = match arch {
        "aarch64" | "arm64" => "aarch64".to_string(),
        "x86_64" | "amd64" => "x86_64".to_string(),
        "i686" | "i386" => "x86".to_string(),
        "arm" | "armv7" | "armv7l" => "arm".to_string(),
        "riscv64" => "riscv64".to_string(),
        other => other.to_string(),
    };
    if lower.contains("linux") {
        Some(("Linux", processor))
    } else if lower.contains("windows") || lower.contains("mingw") {
        Some(("Windows", processor))
    } else if lower.contains("darwin") || lower.contains("apple") {
        Some(("Darwin", processor))
    } else {
        None
    }
}

/// Run the full cmake flow: configure (with the File API query), build, install, then parse
/// the codemodel and write the sidecar.
#[allow(clippy::too_many_arguments)]
fn build_dep(
    dir: &Path,
    name: &str,
    dep_dir: &Path,
    bdir: &Path,
    idir: &Path,
    cxx: &Cxx,
    release: bool,
    fingerprint: &str,
    target: Option<&str>,
    sysroot: Option<&str>,
) -> Result<CmakeDep, String> {
    let mode = mode_name(release);
    let query_dir = bdir.join(".cmake").join("api").join("v1").join("query");
    std::fs::create_dir_all(&query_dir)
        .map_err(|e| format!("failed to create {}: {e}", query_dir.display()))?;
    std::fs::write(query_dir.join("codemodel-v2"), "")
        .map_err(|e| format!("failed to write the CMake File API query: {e}"))?;

    let mut configure = Command::new("cmake");
    configure
        .arg("-S")
        .arg(dep_dir)
        .arg("-B")
        .arg(bdir)
        .arg(format!("-DCMAKE_BUILD_TYPE={mode}"))
        .arg(format!("-DCMAKE_INSTALL_PREFIX={}", idir.display()))
        .arg(format!("-DCMAKE_CXX_COMPILER={}", cxx.name))
        .arg(format!("-DCMAKE_C_COMPILER={}", c_compiler_for(&cxx.name)))
        .arg("-DCMAKE_POSITION_INDEPENDENT_CODE=ON");
    // Cross-compiling a dependency: tell CMake the target system so it does not probe the
    // host (try_run, find_library paths) as if the dep were built for the host.
    if let Some(t) = target {
        if let Some((system, processor)) = cmake_system_for(t) {
            configure.arg(format!("-DCMAKE_SYSTEM_NAME={system}"));
            configure.arg(format!("-DCMAKE_SYSTEM_PROCESSOR={processor}"));
        }
    }
    if let Some(root) = sysroot {
        configure.arg(format!("-DCMAKE_SYSROOT={root}"));
    }
    if let Some(gen) = pick_generator(cxx) {
        configure.arg("-G").arg(gen);
    }
    run_cmake(configure, "configure", name)?;

    let mut build = Command::new("cmake");
    build
        .arg("--build")
        .arg(bdir)
        .arg("--config")
        .arg(mode)
        .arg("--parallel");
    run_cmake(build, "build", name)?;

    let mut install = Command::new("cmake");
    install
        .arg("--install")
        .arg(bdir)
        .arg("--config")
        .arg(mode)
        .arg("--prefix")
        .arg(idir);
    run_cmake(install, "install", name)?;

    let targets = parse_codemodel(bdir, mode)?;
    if targets.is_empty() {
        return Err(format!(
            "CMake dependency {name} has a CMakeLists.txt but no installable library targets (none of its libraries has install(TARGETS ...) rules). cizon2 can only consume libraries that CMake installs; add install rules to the dependency, or use the regular layout (include/ + flat src/*.cpp) for header-only code."
        ));
    }
    // Every discovered library must actually be installed, otherwise the dependency's
    // install() rules are incomplete and the consumer would link against nothing.
    for target in &targets {
        if installed_artifact(idir, &target.artifact).is_none() {
            return Err(format!(
                "CMake dependency {name} built target {} ({}) but its install rules did not install it under {}/lib. Add an install(TARGETS ...) rule for it, or remove the target from the build.",
                target.name,
                target.artifact,
                idir.display()
            ));
        }
    }
    let dep = CmakeDep {
        install_dir: idir.to_path_buf(),
        targets: targets.clone(),
    };
    let sidecar = Sidecar {
        fingerprint: fingerprint.to_string(),
        targets: targets
            .iter()
            .map(|t| SidecarTarget {
                name: t.name.clone(),
                kind: if t.kind == CmakeTargetKind::Shared {
                    "shared".to_string()
                } else {
                    "static".to_string()
                },
                artifact: t.artifact.clone(),
                links: t.links.iter().map(link_input_to_str).collect(),
            })
            .collect(),
    };
    write_sidecar(bdir, &sidecar)?;
    let _ = dir;
    Ok(dep)
}

/// Run one cmake step; on failure report the tail of its combined output (the whole log is
/// usually hundreds of lines of compiler chatter).
fn run_cmake(mut cmd: Command, step: &str, dep: &str) -> Result<(), String> {
    let out = cmd
        .output()
        .map_err(|e| format!("failed to run cmake {step} for dependency {dep}: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<&str> = combined.lines().collect();
    let start = lines.len().saturating_sub(30);
    let tail = &lines[start..];
    Err(format!(
        "cmake {step} failed for dependency {dep}:\n{}",
        tail.join("\n")
    ))
}

/// Library file name (e.g. `libgtest.a`, `gtest.lib`, `libbar.so`) → link name (`gtest`, `bar`).
fn link_name(artifact: &str) -> String {
    let mut name = artifact;
    if let Some(rest) = name.strip_prefix("lib") {
        name = rest;
    }
    // Longest first: ".dll.a" must be tried before ".a", or libfoo.dll.a loses only ".a".
    for ext in [".dll.a", ".a", ".so", ".dylib", ".lib", ".dll"] {
        if let Some(rest) = name.strip_suffix(ext) {
            name = rest;
            break;
        }
    }
    // Versioned .so files (libfoo.so.1.2): strip the version tail too.
    if let Some((base, _)) = name.split_once(".so.") {
        name = base;
    }
    name.to_string()
}

/// Parse the codemodel reply: newest `index-*.json` → codemodel file → per-target files for
/// the matching configuration. Returns the library targets with resolved transitive links.
fn parse_codemodel(build_dir: &Path, mode: &str) -> Result<Vec<CmakeTarget>, String> {
    let reply_dir = build_dir.join(".cmake").join("api").join("v1").join("reply");
    let mut indexes: Vec<PathBuf> = std::fs::read_dir(&reply_dir)
        .map_err(|e| format!("CMake File API reply not found under {}: {e}", reply_dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("index-") && n.ends_with(".json"))
        })
        .collect();
    indexes.sort_by_key(|p| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
    });
    let Some(index_path) = indexes.pop() else {
        return Err(format!(
            "CMake File API reply has no index file under {} (is the codemodel-v2 query active?)",
            reply_dir.display()
        ));
    };
    let index_text = std::fs::read_to_string(&index_path)
        .map_err(|e| format!("failed to read {}: {e}", index_path.display()))?;
    let index = crate::json::parse(&index_text)
        .map_err(|e| format!("failed to parse {}: {e}", index_path.display()))?;
    let codemodel_file = index
        .get("reply")
        .and_then(|r| r.get("codemodel-v2"))
        .and_then(|c| c.get("jsonFile"))
        .and_then(|j| j.as_str())
        .ok_or_else(|| format!("File API index {} has no codemodel-v2 reply", index_path.display()))?;
    let codemodel_text = std::fs::read_to_string(reply_dir.join(codemodel_file))
        .map_err(|e| format!("failed to read the codemodel reply: {e}"))?;
    let codemodel = crate::json::parse(&codemodel_text)
        .map_err(|e| format!("failed to parse the codemodel reply: {e}"))?;
    let configurations = codemodel
        .get("configurations")
        .and_then(|c| c.as_arr())
        .ok_or("codemodel reply has no configurations")?;
    let config = configurations
        .iter()
        .find(|c| c.get("name").and_then(|n| n.as_str()) == Some(mode))
        .or_else(|| configurations.first())
        .ok_or("codemodel reply has no configurations")?;
    let targets = config
        .get("targets")
        .and_then(|t| t.as_arr())
        .ok_or("codemodel configuration has no targets")?;

    // First pass: read every library target's own facts (name, type, artifact).
    let mut raw: Vec<RawTarget> = Vec::new();
    let mut by_artifact: HashMap<String, String> = HashMap::new();
    for entry in targets {
        let Some(json_file) = entry.get("jsonFile").and_then(|j| j.as_str()) else {
            continue;
        };
        let text = std::fs::read_to_string(reply_dir.join(json_file))
            .map_err(|e| format!("failed to read target reply {json_file}: {e}"))?;
        let t = crate::json::parse(&text)
            .map_err(|e| format!("failed to parse target reply {json_file}: {e}"))?;
        let Some(name) = t.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        let kind = match t.get("type").and_then(|x| x.as_str()) {
            Some("STATIC_LIBRARY") => CmakeTargetKind::Static,
            Some("SHARED_LIBRARY") => CmakeTargetKind::Shared,
            _ => continue, // executables, interface and module libraries are not link inputs
        };
        // Only targets with install rules are consumable: a dependency often builds internal
        // helpers for its own tests (e.g. fmt builds googletest under test/) that are never
        // installed. The codemodel carries an `install` section only for targets that have
        // install rules, so its absence is the filter.
        let has_install = t
            .get("install")
            .and_then(|i| i.get("destinations"))
            .and_then(|d| d.as_arr())
            .is_some_and(|d| !d.is_empty());
        if !has_install {
            continue;
        }
        let Some(artifact) = t
            .get("nameOnDisk")
            .and_then(|n| n.as_str())
            .map(str::to_string)
            .or_else(|| {
                t.get("artifacts")
                    .and_then(|a| a.as_arr())
                    .and_then(|a| a.first())
                    .and_then(|a| a.get("path"))
                    .and_then(|p| p.as_str())
                    .map(|p| {
                        Path::new(p)
                            .file_name()
                            .map(|f| f.to_string_lossy().into_owned())
                            .unwrap_or_else(|| p.to_string())
                    })
            })
        else {
            continue;
        };
        let mut fragments = Vec::new();
        if let Some(link) = t.get("link") {
            if let Some(parts) = link.get("commandFragments").and_then(|c| c.as_arr()) {
                for part in parts {
                    if part.get("role").and_then(|r| r.as_str()) != Some("libraries") {
                        continue;
                    }
                    if let Some(frag) = part.get("fragment").and_then(|f| f.as_str()) {
                        if !frag.is_empty() {
                            fragments.push(frag.to_string());
                        }
                    }
                }
            }
        }
        by_artifact.insert(artifact.clone(), name.to_string());
        raw.push(RawTarget {
            name: name.to_string(),
            kind,
            artifact,
            fragments,
        });
    }

    // Second pass: resolve each target's link fragments through the target graph, transitively.
    let mut out = Vec::new();
    for target in &raw {
        let mut links = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut visited: HashSet<String> = HashSet::new();
        resolve_links(target, &raw, &by_artifact, &mut links, &mut seen, &mut visited);
        out.push(CmakeTarget {
            name: target.name.clone(),
            kind: target.kind,
            artifact: target.artifact.clone(),
            links,
        });
    }
    Ok(out)
}

/// One library target's own facts, before link resolution.
struct RawTarget {
    name: String,
    kind: CmakeTargetKind,
    artifact: String,
    fragments: Vec<String>,
}

/// Resolve one target's link fragments: a fragment naming another target's artifact recurses
/// into that target (transitive, cycle-safe); `-l<name>` and bare names become Lib entries;
/// absolute paths pass through; bare library file names become Lib entries by their link name.
fn resolve_links(
    target: &RawTarget,
    all: &[RawTarget],
    by_artifact: &HashMap<String, String>,
    out: &mut Vec<LinkInput>,
    seen: &mut HashSet<String>,
    visited: &mut HashSet<String>,
) {
    if !visited.insert(target.name.clone()) {
        return;
    }
    for frag in &target.fragments {
        let file = Path::new(frag)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_else(|| frag.clone());
        if let Some(other_name) = by_artifact.get(&file) {
            if let Some(other) = all.iter().find(|t| &t.name == other_name) {
                push_unique(out, seen, LinkInput::Lib(link_name(&other.artifact)));
                resolve_links(other, all, by_artifact, out, seen, visited);
                continue;
            }
        }
        if let Some(lib) = frag.strip_prefix("-l") {
            push_unique(out, seen, LinkInput::Lib(lib.to_string()));
        } else if Path::new(frag).is_absolute() {
            push_unique(out, seen, LinkInput::Path(PathBuf::from(frag)));
        } else if looks_like_lib_file(frag) {
            push_unique(out, seen, LinkInput::Lib(link_name(frag)));
        } else {
            push_unique(out, seen, LinkInput::Lib(frag.clone()));
        }
    }
}

fn looks_like_lib_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    (lower.starts_with("lib") || lower.ends_with(".lib"))
        && (lower.ends_with(".a")
            || lower.ends_with(".so")
            || lower.ends_with(".dylib")
            || lower.ends_with(".lib")
            || lower.ends_with(".dll")
            || lower.contains(".so."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmake_system_for_maps_common_triples() {
        assert_eq!(
            cmake_system_for("aarch64-linux-gnu"),
            Some(("Linux", "aarch64".to_string()))
        );
        assert_eq!(
            cmake_system_for("x86_64-w64-mingw32"),
            Some(("Windows", "x86_64".to_string()))
        );
        assert_eq!(
            cmake_system_for("x86_64-apple-darwin"),
            Some(("Darwin", "x86_64".to_string()))
        );
        assert_eq!(cmake_system_for("weird-unknown"), None);
    }

    #[test]
    fn link_name_strips_prefix_and_suffix() {
        assert_eq!(link_name("libgtest.a"), "gtest");
        assert_eq!(link_name("libbar.so"), "bar");
        assert_eq!(link_name("libfoo.so.1.2"), "foo");
        assert_eq!(link_name("gtest.lib"), "gtest");
        assert_eq!(link_name("libfoo.dll.a"), "foo");
        assert_eq!(link_name("pthread"), "pthread");
    }

    #[test]
    fn c_compiler_for_swaps_stem_only() {
        assert_eq!(c_compiler_for("g++"), "gcc");
        assert_eq!(c_compiler_for("clang++"), "clang");
        assert_eq!(c_compiler_for("cl"), "cl");
        assert_eq!(c_compiler_for("/opt/gcc-14/bin/g++"), "/opt/gcc-14/bin/gcc");
        assert_eq!(c_compiler_for("aarch64-linux-gnu-g++"), "aarch64-linux-gnu-gcc");
    }

    #[test]
    fn expand_links_resolves_transitively_and_dedupes() {
        let dep = CmakeDep {
            install_dir: PathBuf::from("/x"),
            targets: vec![
                CmakeTarget {
                    name: "bar".into(),
                    kind: CmakeTargetKind::Shared,
                    artifact: "libbar.so".into(),
                    links: vec![LinkInput::Lib("foo".into()), LinkInput::Lib("pthread".into())],
                },
                CmakeTarget {
                    name: "foo".into(),
                    kind: CmakeTargetKind::Static,
                    artifact: "libfoo.a".into(),
                    links: vec![LinkInput::Lib("pthread".into())],
                },
            ],
        };
        let expanded = dep.expand_links(&["bar".to_string()]);
        assert_eq!(
            expanded,
            vec![
                LinkInput::Lib("bar".into()),
                LinkInput::Lib("foo".into()),
                LinkInput::Lib("pthread".into()),
            ]
        );
        // Unknown names pass through untouched; matching is case-insensitive.
        let expanded = dep.expand_links(&["BAR".to_string(), "other".to_string()]);
        assert!(expanded.iter().any(|l| *l == LinkInput::Lib("other".into())));
    }

    #[test]
    fn link_input_render_uses_compiler_spelling() {
        let cl = Cxx {
            name: "cl".into(),
            version: "Microsoft (R) C/C++ Optimizing Compiler Version 19.44.35213 for x64".into(),
        };
        assert_eq!(LinkInput::Lib("gtest".into()).render(&cl), "gtest.lib");
        let gxx = Cxx {
            name: "g++".into(),
            version: "g++ 14.2.0".into(),
        };
        assert_eq!(LinkInput::Lib("gtest".into()).render(&gxx), "-lgtest");
        assert_eq!(
            LinkInput::Path(PathBuf::from("/usr/lib/libx.a")).render(&gxx),
            "/usr/lib/libx.a"
        );
    }

    fn has_cmake() -> bool {
        Command::new("cmake")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn has_git() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        assert!(ok, "git {args:?} failed in {}", dir.display());
    }

    /// End-to-end with real cmake: a toy dependency (static lib `foo` + shared lib `bar`
    /// linking `foo`, both with install rules) is configured via the File API, built,
    /// installed, and its targets + transitive links are discovered from the codemodel.
    #[test]
    fn ensure_built_discovers_targets_from_real_cmake() {
        if !has_cmake() || !has_git() {
            return;
        }
        let Ok(cxx) = crate::compiler::pick(None, None) else {
            return;
        };
        let base = std::env::temp_dir().join(format!(
            "cizon2-cmake-e2e-{}",
            std::process::id()
        ));
        let dep_dir = base.join("deps").join("toydep");
        std::fs::create_dir_all(dep_dir.join("src")).unwrap();
        std::fs::create_dir_all(dep_dir.join("include").join("toy")).unwrap();
        std::fs::write(
            dep_dir.join("CMakeLists.txt"),
            "cmake_minimum_required(VERSION 3.20)\nproject(toydep CXX)\n\
             add_library(foo STATIC src/foo.cpp)\n\
             target_include_directories(foo PUBLIC $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include> $<INSTALL_INTERFACE:include>)\n\
             add_library(bar SHARED src/bar.cpp)\n\
             target_link_libraries(bar PUBLIC foo)\n\
             install(TARGETS foo bar ARCHIVE DESTINATION lib LIBRARY DESTINATION lib RUNTIME DESTINATION bin)\n\
             install(DIRECTORY include/ DESTINATION include)\n",
        )
        .unwrap();
        std::fs::write(dep_dir.join("include/toy/foo.h"), "int foo();\n").unwrap();
        std::fs::write(dep_dir.join("src/foo.cpp"), "#include \"toy/foo.h\"\nint foo() { return 1; }\n").unwrap();
        std::fs::write(
            dep_dir.join("src/bar.cpp"),
            "#include \"toy/foo.h\"\nint bar() { return foo(); }\n",
        )
        .unwrap();
        git(&dep_dir, &["init"]);
        git(&dep_dir, &["add", "-A"]);
        git(&dep_dir, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-m", "init", "--no-gpg-sign"]);

        let dep = ensure_built(&base, "toydep", &cxx, false, None, None)
            .expect("the toy cmake dependency must build");
        assert_eq!(dep.targets.len(), 2, "targets: {:?}", dep.targets);
        let foo = dep.targets.iter().find(|t| t.name == "foo").expect("foo");
        assert_eq!(foo.kind, CmakeTargetKind::Static);
        assert_eq!(foo.artifact, "libfoo.a");
        let bar = dep.targets.iter().find(|t| t.name == "bar").expect("bar");
        assert_eq!(bar.kind, CmakeTargetKind::Shared);
        // bar links foo: the codemodel fragment resolves through the target graph.
        assert!(
            bar.links.iter().any(|l| *l == LinkInput::Lib("foo".into())),
            "bar links: {:?}",
            bar.links
        );
        assert!(dep.install_dir.join("include").is_dir());
        assert!(dep.lib_dir().is_some());
        assert!(installed_artifact(&dep.install_dir, "libfoo.a").is_some());
        assert!(installed_artifact(&dep.install_dir, "libbar.so").is_some());
        // Second call hits the fingerprint cache: same result, no rebuild.
        let dep2 = ensure_built(&base, "toydep", &cxx, false, None, None).unwrap();
        assert_eq!(dep2.targets.len(), 2);

        std::fs::remove_dir_all(base).ok();
    }

    /// A dependency without install() rules fails loudly at add/build time instead of
    /// producing an empty install tree that links against nothing.
    #[test]
    fn ensure_built_rejects_missing_install_rules() {
        if !has_cmake() || !has_git() {
            return;
        }
        let Ok(cxx) = crate::compiler::pick(None, None) else {
            return;
        };
        let base = std::env::temp_dir().join(format!(
            "cizon2-cmake-noinstall-{}",
            std::process::id()
        ));
        let dep_dir = base.join("deps").join("nolib");
        std::fs::create_dir_all(dep_dir.join("src")).unwrap();
        std::fs::write(
            dep_dir.join("CMakeLists.txt"),
            "cmake_minimum_required(VERSION 3.20)\nproject(nolib CXX)\n\
             add_library(nolib STATIC src/nolib.cpp)\n",
        )
        .unwrap();
        std::fs::write(dep_dir.join("src/nolib.cpp"), "int nolib() { return 0; }\n").unwrap();
        git(&dep_dir, &["init"]);
        git(&dep_dir, &["add", "-A"]);
        git(&dep_dir, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-m", "init", "--no-gpg-sign"]);

        let err = ensure_built(&base, "nolib", &cxx, false, None, None).unwrap_err();
        assert!(err.contains("install"), "got: {err}");
        std::fs::remove_dir_all(base).ok();
    }
}
