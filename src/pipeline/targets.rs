use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::compiler::{self, Cxx};
use crate::config::{Config, Target, TargetKind};

use super::output::rel_display;
use super::{BinTarget, LibTarget, LinkTarget, Plan, SharedTarget};

const SOURCE_EXTS: &[&str] = &["cpp", "cc", "cxx"];

pub fn plan(dir: &Path, cfg: &Config, cxx: Cxx, release: bool) -> Result<Plan, String> {
    let (mut bins, mut tests, mut libs, mut shared) = if let Some(targets) = cfg.targets() {
        let static_names: HashMap<String, String> = targets
            .iter()
            .filter(|t| t.kind == TargetKind::StaticLib)
            .map(|t| {
                let name = t.name.as_deref().expect("config resolves lib name");
                (name.to_lowercase(), name.to_string())
            })
            .collect();
        let mut libs = Vec::new();
        for target in targets.iter().filter(|t| t.kind == TargetKind::StaticLib) {
            let name = target.name.as_deref().expect("config resolves lib name");
            let lib_sources = collect_target_sources(
                &target.base_dir,
                target.sources.as_deref(),
                &format!("static library target {name}"),
            )?;
            if lib_sources.is_empty() {
                return Err(format!("static library target {name} has no source file to compile."));
            }
            let owner = format!("static library target {name}");
            libs.push(LibTarget {
                name: name.to_string(),
                sources: lib_sources,
                include: resolve_include(&target.base_dir, target.include.as_deref(), &owner)?,
                links: target
                    .links
                    .iter()
                    .map(|link| {
                        static_names
                            .get(&link.to_lowercase())
                            .cloned()
                            .ok_or_else(|| format!("{owner} links a nonexistent static library {link}."))
                    })
                    .collect::<Result<_, _>>()?,
                prefix: resolve_prefix(&target.base_dir, target.prefix.as_deref(), &owner)?,
                compile_flags: Vec::new(),
            });
        }
        let mut shared = Vec::new();
        for target in targets.iter().filter(|t| t.kind == TargetKind::SharedLib) {
            let name = target.name.as_deref().expect("config resolves shared library name");
            let shared_sources = collect_target_sources(
                &target.base_dir,
                target.sources.as_deref(),
                &format!("shared library target {name}"),
            )?;
            if shared_sources.is_empty() {
                return Err(format!(
                    "shared library target {name} has no source file to compile."
                ));
            }
            let owner = format!("shared library target {name}");
            let shared_links = target
                .links
                .iter()
                .map(|link| {
                    static_names
                        .get(&link.to_lowercase())
                        .cloned()
                        .ok_or_else(|| format!("{owner} links a nonexistent static library {link}."))
                })
                .collect::<Result<_, _>>()?;
            shared.push(SharedTarget {
                name: name.to_string(),
                sources: shared_sources,
                include: resolve_include(&target.base_dir, target.include.as_deref(), &owner)?,
                links: shared_links,
                prefix: resolve_prefix(&target.base_dir, target.prefix.as_deref(), &owner)?,
                compile_flags: Vec::new(),
            });
        }
        let mut bins = Vec::new();
        for target in targets.iter().filter(|t| t.kind == TargetKind::Bin) {
            let name = target.name.as_deref().expect("config resolves bin name");
            let sources = collect_target_sources(
                &target.base_dir,
                target.sources.as_deref(),
                &format!("bin target {name}"),
            )?;
            if sources.is_empty() {
                return Err(format!("bin target {name} has no source file to compile."));
            }
            let links = resolve_exec_links(target.links.as_slice(), &libs, &shared);
            let owner = format!("bin target {name}");
            bins.push(BinTarget {
                name: name.to_string(),
                sources,
                links,
                out_name: exe_name(name),
                extern_links: Vec::new(),
                include: resolve_include(&target.base_dir, target.include.as_deref(), &owner)?,
                prefix: resolve_prefix(&target.base_dir, target.prefix.as_deref(), &owner)?,
                compile_flags: Vec::new(),
            });
        }
        let mut tests = Vec::new();
        for target in targets.iter().filter(|t| t.kind == TargetKind::Test) {
            let name = target.name.as_deref().expect("config resolves test name");
            let sources = collect_target_sources(
                &target.base_dir,
                target.sources.as_deref(),
                &format!("test target {name}"),
            )?;
            if sources.is_empty() {
                return Err(format!("test target {name} has no source file to compile."));
            }
            let links = resolve_exec_links(target.links.as_slice(), &libs, &shared);
            let owner = format!("test target {name}");
            tests.push(BinTarget {
                name: name.to_string(),
                sources,
                links,
                out_name: exe_name(name),
                extern_links: target.extern_links.clone(),
                include: resolve_include(&target.base_dir, target.include.as_deref(), &owner)?,
                prefix: resolve_prefix(&target.base_dir, target.prefix.as_deref(), &owner)?,
                compile_flags: Vec::new(),
            });
        }
        // Every target's base dir gets its src/ scanned for uncompiled files: a
        // subproject's src/ is as much a source root as the project's own.
        let base_dirs = target_base_dirs(dir, targets);
        warn_unused_src(dir, &base_dirs, &bins, &tests, &libs, &shared);
        (bins, tests, libs, shared)
    } else {
        let explicit = cfg.explicit_sources();
        let sources = collect_sources(dir, explicit)?;
        if sources.is_empty() {
            return Err(match explicit {
                Some(_) => "No .cpp file found under the paths listed in [project] sources. Check those paths, or remove sources to scan src/.".into(),
                None => format!(
                    "No .cpp file under {}. Put source code in src/, or list them explicitly with [project] sources = [...].",
                    rel_display(dir, &dir.join("src"))
                ),
            });
        }
        let name = cfg.name(dir);
        let bins = vec![BinTarget {
            out_name: exe_name(&name),
            name,
            sources,
            links: Vec::new(),
            extern_links: Vec::new(),
            include: None,
            prefix: None,
            compile_flags: Vec::new(),
        }];
        (bins, Vec::new(), Vec::new(), Vec::new())
    };

    let edition = compiler::edition_flag(cfg.edition());
    let optimization = if release { "-O2" } else { "-g" };
    let mut base = vec![edition, optimization.into()];
    // Release is a real C++ profile: asserts and NDEBUG-gated code follow the standard switch.
    // Tests strip this definition again so assert-based tests stay live.
    if release {
        base.push("-DNDEBUG".into());
    }
    let mut flags = base.clone();
    // Convenience warning switches expand to fixed flag bundles; user flags come after so they
    // can still override (e.g. -Wno-... or -UNDEBUG). These apply to project sources only, never deps.
    if cfg.strict() {
        flags.extend(["-Wall", "-Wextra", "-Wpedantic"].map(String::from));
    }
    if cfg.werror() {
        flags.push("-Werror".into());
    }
    flags.extend(cfg.flags().iter().cloned());
    // Public header directory: modern-layout include/ is added with -I automatically; when the
    // directory is missing the compiler safely ignores it
    flags.push(format!("-I{}", dir.join("include").display()));
    // Dependency header paths enter flags unconditionally: when the directory is missing the
    // compiler safely ignores it, so planning needs no network operation
    let mut dep_includes: BTreeMap<String, Vec<String>> = BTreeMap::new();
    if let Some(map) = cfg.dependencies() {
        for name in map.keys() {
            let inc = dir.join("deps").join(name);
            let include = format!("-I{}", inc.join("include").display());
            let root = format!("-I{}", inc.display());
            flags.push(include.clone());
            flags.push(root.clone());
            dep_includes.insert(name.clone(), vec![include, root]);
        }
    }
    let dep_base = base;

    let mut automatic_includes: Vec<(String, PathBuf)> = Vec::new();
    let project_include = dir.join("include");
    if project_include.is_dir() {
        automatic_includes.push(("project include/".to_string(), project_include));
    }
    if let Some(map) = cfg.dependencies() {
        for name in map.keys() {
            let dep_root = dir.join("deps").join(name);
            let dep_include = dep_root.join("include");
            if dep_include.is_dir() {
                automatic_includes.push((format!("dependency {name}"), dep_include));
            }
            if dep_root.is_dir() {
                automatic_includes.push((format!("dependency {name}"), dep_root));
            }
        }
    }

    let link_flags: Vec<String> = cfg.link_flags().to_vec();
    let test_flags = without_ndebug(&flags);
    finalize_target_graph(
        dir,
        &mut bins,
        &mut tests,
        &mut libs,
        &mut shared,
        &automatic_includes,
        &flags,
        &test_flags,
    )?;

    for (src, names) in overlapping_exec_sources(&bins, &tests) {
        eprintln!(
            "Warning: source {} is compiled into multiple executable targets ({}). Recursive sources can pull in another target's files; multiple main() definitions will fail to link. Use explicit file paths or separate directories.",
            rel_display(dir, &src),
            names.join(", ")
        );
    }

    Ok(Plan {
        cxx,
        bins,
        tests,
        flags,
        link_flags,
        dep_base,
        dep_includes,
        libs,
        shared,
        mode: if release { "release" } else { "debug" },
    })
}

/// Resolve the include interface and static-library order, then materialize each target's
/// effective compile flags. Root `include/` and dependency includes are already in `flags`.
fn finalize_target_graph(
    dir: &Path,
    bins: &mut [BinTarget],
    tests: &mut [BinTarget],
    libs: &mut [LibTarget],
    shared: &mut [SharedTarget],
    automatic_includes: &[(String, PathBuf)],
    flags: &[String],
    test_flags: &[String],
) -> Result<(), String> {
    let lib_index: HashMap<String, usize> = libs
        .iter()
        .enumerate()
        .map(|(i, lib)| (lib.name.to_lowercase(), i))
        .collect();
    let shared_index: HashMap<String, usize> = shared
        .iter()
        .enumerate()
        .map(|(i, sh)| (sh.name.to_lowercase(), i))
        .collect();
    validate_static_graph(libs, &lib_index)?;
    validate_include_uniqueness(
        dir,
        bins,
        tests,
        libs,
        shared,
        automatic_includes,
        &lib_index,
        &shared_index,
    )?;

    for i in 0..libs.len() {
        let includes = lib_includes(&libs[i].name, libs, &lib_index);
        let prefix = libs[i].prefix.clone();
        libs[i].compile_flags = flags_with_includes(flags, &includes, &prefix);
    }
    for i in 0..shared.len() {
        let includes = shared_includes(&shared[i], libs, &lib_index);
        let prefix = shared[i].prefix.clone();
        shared[i].compile_flags = flags_with_includes(flags, &includes, &prefix);
        let ordered = ordered_static_roots(&shared[i].links, libs, &lib_index)?;
        shared[i].links = ordered;
    }
    for bin in bins.iter_mut() {
        let includes = exec_includes(
            bin.include.as_deref(),
            &bin.links,
            libs,
            shared,
            &lib_index,
            &shared_index,
        );
        let prefix = bin.prefix.clone();
        bin.compile_flags = flags_with_includes(flags, &includes, &prefix);
        let ordered = ordered_exec_links(&bin.links, libs, &lib_index)?;
        bin.links = ordered;
    }
    for test in tests.iter_mut() {
        let includes = exec_includes(
            test.include.as_deref(),
            &test.links,
            libs,
            shared,
            &lib_index,
            &shared_index,
        );
        let prefix = test.prefix.clone();
        test.compile_flags = flags_with_includes(test_flags, &includes, &prefix);
        let ordered = ordered_exec_links(&test.links, libs, &lib_index)?;
        test.links = ordered;
    }
    Ok(())
}

fn flags_with_includes(base: &[String], includes: &[PathBuf], prefix: &Option<PathBuf>) -> Vec<String> {
    let mut flags = base.to_vec();
    for include in includes {
        flags.push(format!("-I{}", include.display()));
    }
    if let Some(p) = prefix {
        flags.push(format!("-I{}/include", p.display()));
    }
    flags
}

fn push_include(include: &Option<PathBuf>, out: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>) {
    if let Some(path) = include {
        if seen.insert(path.clone()) {
            out.push(path.clone());
        }
    }
}

fn lib_includes(
    start: &str,
    libs: &[LibTarget],
    index: &HashMap<String, usize>,
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut visited = HashSet::new();
    collect_lib_includes(start, libs, index, &mut visited, &mut out, &mut seen);
    out
}

fn collect_lib_includes(
    name: &str,
    libs: &[LibTarget],
    index: &HashMap<String, usize>,
    visited: &mut HashSet<String>,
    out: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
) {
    let lower = name.to_lowercase();
    if !visited.insert(lower.clone()) {
        return;
    }
    let Some(&i) = index.get(&lower) else {
        return;
    };
    push_include(&libs[i].include, out, seen);
    for dep in &libs[i].links {
        collect_lib_includes(dep, libs, index, visited, out, seen);
    }
}

fn shared_includes(
    shared: &SharedTarget,
    libs: &[LibTarget],
    index: &HashMap<String, usize>,
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut visited = HashSet::new();
    push_include(&shared.include, &mut out, &mut seen);
    for dep in &shared.links {
        collect_lib_includes(dep, libs, index, &mut visited, &mut out, &mut seen);
    }
    out
}

fn exec_includes(
    own: Option<&Path>,
    links: &[LinkTarget],
    libs: &[LibTarget],
    shared: &[SharedTarget],
    lib_index: &HashMap<String, usize>,
    shared_index: &HashMap<String, usize>,
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut visited_libs = HashSet::new();
    let mut visited_shared = HashSet::new();
    if let Some(path) = own {
        if seen.insert(path.to_path_buf()) {
            out.push(path.to_path_buf());
        }
    }
    for link in links {
        match link {
            LinkTarget::Static(name) => {
                collect_lib_includes(name, libs, lib_index, &mut visited_libs, &mut out, &mut seen);
            }
            LinkTarget::Shared(name) => {
                let lower = name.to_lowercase();
                if !visited_shared.insert(lower.clone()) {
                    continue;
                }
                if let Some(&i) = shared_index.get(&lower) {
                    push_include(&shared[i].include, &mut out, &mut seen);
                    for dep in &shared[i].links {
                        collect_lib_includes(
                            dep,
                            libs,
                            lib_index,
                            &mut visited_libs,
                            &mut out,
                            &mut seen,
                        );
                    }
                }
            }
        }
    }
    out
}

/// Reject static-library cycles globally, including libraries that no executable consumes.
fn validate_static_graph(
    libs: &[LibTarget],
    index: &HashMap<String, usize>,
) -> Result<(), String> {
    fn visit(
        name: &str,
        libs: &[LibTarget],
        index: &HashMap<String, usize>,
        state: &mut HashMap<String, u8>,
        stack: &mut Vec<String>,
    ) -> Result<(), String> {
        let lower = name.to_lowercase();
        match state.get(&lower).copied() {
            Some(2) => return Ok(()),
            Some(1) => {
                let start = stack
                    .iter()
                    .position(|n| n.eq_ignore_ascii_case(name))
                    .unwrap_or(0);
                let mut chain = stack[start..].to_vec();
                chain.push(name.to_string());
                return Err(format!(
                    "static library link cycle: {}. Break the cycle; cizon does not add --start-group.",
                    chain.join(" -> ")
                ));
            }
            _ => {}
        }
        let Some(&i) = index.get(&lower) else {
            return Err(format!("static library target {name} links a nonexistent static library."));
        };
        state.insert(lower.clone(), 1);
        stack.push(libs[i].name.clone());
        for dep in &libs[i].links {
            visit(dep, libs, index, state, stack)?;
        }
        stack.pop();
        state.insert(lower, 2);
        Ok(())
    }

    let mut state = HashMap::new();
    let mut stack = Vec::new();
    for lib in libs {
        visit(&lib.name, libs, index, &mut state, &mut stack)?;
    }
    Ok(())
}

/// Reject duplicate public headers across each target's effective include closure.
fn validate_include_uniqueness(
    dir: &Path,
    bins: &[BinTarget],
    tests: &[BinTarget],
    libs: &[LibTarget],
    shared: &[SharedTarget],
    automatic_includes: &[(String, PathBuf)],
    lib_index: &HashMap<String, usize>,
    shared_index: &HashMap<String, usize>,
) -> Result<(), String> {
    // Every target's own compilation sees the same -I list as its consumers, so each
    // closure is checked: libraries first, then shared libraries, bins and tests.
    // Whether two headers differing only by case count as duplicates follows the
    // host filesystem: Windows lookups are case-insensitive, so Util.hpp and
    // util.hpp shadow each other there; on case-sensitive filesystems they are
    // different files.
    let case_insensitive = cfg!(windows);
    for lib in libs {
        let mut pairs = automatic_includes.to_vec();
        pairs.extend(lib_closure_includes(
            &lib.name,
            lib.include.as_ref(),
            &lib.links,
            libs,
            lib_index,
        ));
        check_closure_includes(dir, &pairs, case_insensitive)?;
    }
    for sh in shared {
        let mut pairs = automatic_includes.to_vec();
        pairs.extend(lib_closure_includes(
            &sh.name,
            sh.include.as_ref(),
            &sh.links,
            libs,
            lib_index,
        ));
        check_closure_includes(dir, &pairs, case_insensitive)?;
    }
    for target in bins.iter().chain(tests.iter()) {
        let mut pairs = automatic_includes.to_vec();
        pairs.extend(exec_closure_includes(
            target, libs, shared, lib_index, shared_index,
        ));
        check_closure_includes(dir, &pairs, case_insensitive)?;
    }
    Ok(())
}

/// (owning target, include directory) pairs in a static library's link closure, in
/// declaration order, starting with the library's own include directory. Mirrors
/// `collect_lib_includes` but keeps the owner of each directory so a duplicate header
/// can name both libraries.
fn lib_closure_includes(
    name: &str,
    own_include: Option<&PathBuf>,
    links: &[String],
    libs: &[LibTarget],
    index: &HashMap<String, usize>,
) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    if let Some(include) = own_include {
        out.push((name.to_string(), include.clone()));
    }
    let mut visited = HashSet::new();
    for dep in links {
        collect_closure_includes(dep, libs, index, &mut visited, &mut out);
    }
    out
}

/// (owning target, include directory) pairs in a bin/test target's link closure, in
/// declaration order, starting with the target's own include directory. Mirrors
/// `exec_includes`.
fn exec_closure_includes(
    target: &BinTarget,
    libs: &[LibTarget],
    shared: &[SharedTarget],
    lib_index: &HashMap<String, usize>,
    shared_index: &HashMap<String, usize>,
) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    if let Some(include) = &target.include {
        out.push((target.name.clone(), include.clone()));
    }
    let mut visited_libs = HashSet::new();
    let mut visited_shared = HashSet::new();
    for link in &target.links {
        match link {
            LinkTarget::Static(name) => {
                collect_closure_includes(name, libs, lib_index, &mut visited_libs, &mut out);
            }
            LinkTarget::Shared(name) => {
                let lower = name.to_lowercase();
                if !visited_shared.insert(lower.clone()) {
                    continue;
                }
                if let Some(&i) = shared_index.get(&lower) {
                    if let Some(include) = &shared[i].include {
                        out.push((shared[i].name.clone(), include.clone()));
                    }
                    for dep in &shared[i].links {
                        collect_closure_includes(dep, libs, lib_index, &mut visited_libs, &mut out);
                    }
                }
            }
        }
    }
    out
}

/// Recursive half of `lib_closure_includes` / `exec_closure_includes`: one static
/// library's include directory plus its transitive dependencies, in declaration
/// order. Mirrors `collect_lib_includes`.
fn collect_closure_includes(
    name: &str,
    libs: &[LibTarget],
    index: &HashMap<String, usize>,
    visited: &mut HashSet<String>,
    out: &mut Vec<(String, PathBuf)>,
) {
    let lower = name.to_lowercase();
    if !visited.insert(lower.clone()) {
        return;
    }
    let Some(&i) = index.get(&lower) else {
        return;
    };
    if let Some(include) = &libs[i].include {
        out.push((libs[i].name.clone(), include.clone()));
    }
    for dep in &libs[i].links {
        collect_closure_includes(dep, libs, index, visited, out);
    }
}

/// Map key for a header's path relative to its include directory: separators
/// normalized, and lowercased when the host filesystem is case-insensitive so
/// Util.hpp and util.hpp count as the same header there. The error message
/// always shows the original paths, never the key.
fn header_key(rel: &Path, case_insensitive: bool) -> String {
    let key = rel.to_string_lossy().replace('\\', "/");
    if case_insensitive {
        key.to_lowercase()
    } else {
        key
    }
}

/// Scan one link closure's include directories for duplicate relative header paths.
pub(crate) fn check_closure_includes(
    dir: &Path,
    pairs: &[(String, PathBuf)],
    case_insensitive: bool,
) -> Result<(), String> {
    let canonical_root = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let mut scanned: HashSet<PathBuf> = HashSet::new();
    let mut seen: HashMap<String, (String, PathBuf)> = HashMap::new();
    for (owner, include) in pairs {
        let identity = std::fs::canonicalize(include).unwrap_or_else(|_| include.clone());
        if !scanned.insert(identity) {
            continue;
        }
        let mut files = Vec::new();
        collect_include_files(dir, &canonical_root, include, &mut files, &mut HashSet::new())?;
        files.sort();
        for file in files {
            let rel = file.strip_prefix(include).unwrap_or(&file).to_path_buf();
            let key = header_key(&rel, case_insensitive);
            if let Some((first_owner, first_file)) = seen.get(&key) {
                // Dependencies may reuse generic headers; preserve their existing search order.
                if first_owner.starts_with("dependency ") && owner.starts_with("dependency ") {
                    continue;
                }
                return Err(format!(
                    "duplicate header \"{}\" provided by \"{}\" ({}) and \"{}\" ({}); move one under include/<project>/ or rename it.",
                    rel.to_string_lossy().replace('\\', "/"),
                    first_owner,
                    rel_display(dir, first_file).replace('\\', "/"),
                    owner,
                    rel_display(dir, &file).replace('\\', "/")
                ));
            }
            seen.insert(key, (owner.clone(), file));
        }
    }
    Ok(())
}

/// Collect includable files, following directory links with containment and cycle guards.
fn collect_include_files(
    project_root: &Path,
    canonical_root: &Path,
    dir: &Path,
    out: &mut Vec<PathBuf>,
    visited: &mut HashSet<PathBuf>,
) -> Result<(), String> {
    let label = rel_display(project_root, dir).replace('\\', "/");
    let canonical = std::fs::canonicalize(dir)
        .map_err(|e| format!("Cannot resolve include directory {label}: {e}"))?;
    if !canonical.starts_with(canonical_root) {
        return Err(format!("include directory {label} resolves outside the project."));
    }
    if !visited.insert(canonical) {
        return Ok(());
    }
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("Cannot read {label}: {e}"))?;
    for entry in entries {
        let entry = entry
            .map_err(|e| format!("Cannot read directory entries of {label}: {e}"))?;
        let path = entry.path();
        let path_label = rel_display(project_root, &path).replace('\\', "/");
        let file_type = entry
            .file_type()
            .map_err(|e| format!("Cannot read type of {path_label}: {e}"))?;
        if file_type.is_dir() {
            collect_include_files(project_root, canonical_root, &path, out, visited)?;
        } else if file_type.is_symlink() {
            let target = std::fs::canonicalize(&path)
                .map_err(|e| format!("Cannot resolve include link {path_label}: {e}"))?;
            if target.is_dir() {
                collect_include_files(project_root, canonical_root, &path, out, visited)?;
            } else if path.is_file() {
                if !target.starts_with(canonical_root) {
                    return Err(format!("include file {path_label} resolves outside the project."));
                }
                out.push(path);
            }
        } else if path.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

/// Dependency-first order for static archives: dependents before dependencies. Independent
/// libraries keep their first encounter order.
fn ordered_static_roots(
    roots: &[String],
    libs: &[LibTarget],
    index: &HashMap<String, usize>,
) -> Result<Vec<String>, String> {
    fn collect(
        name: &str,
        libs: &[LibTarget],
        index: &HashMap<String, usize>,
        names: &mut Vec<String>,
        positions: &mut HashMap<String, usize>,
        edges: &mut Vec<Vec<usize>>,
    ) -> Result<usize, String> {
        let lower = name.to_lowercase();
        if let Some(&i) = positions.get(&lower) {
            return Ok(i);
        }
        let Some(&lib_i) = index.get(&lower) else {
            return Err(format!("static library target {name} links a nonexistent static library."));
        };
        let i = names.len();
        names.push(libs[lib_i].name.clone());
        positions.insert(lower, i);
        edges.push(Vec::new());
        for dep in &libs[lib_i].links {
            let d = collect(dep, libs, index, names, positions, edges)?;
            edges[i].push(d);
        }
        Ok(i)
    }

    let mut names = Vec::new();
    let mut positions = HashMap::new();
    let mut edges = Vec::new();
    for root in roots {
        collect(root, libs, index, &mut names, &mut positions, &mut edges)?;
    }
    let mut indegree = vec![0usize; names.len()];
    for targets in &edges {
        for &target in targets {
            indegree[target] += 1;
        }
    }
    let mut ready: Vec<usize> = (0..names.len()).filter(|&i| indegree[i] == 0).collect();
    let mut ordered = Vec::with_capacity(names.len());
    while !ready.is_empty() {
        ready.sort_unstable();
        let i = ready.remove(0);
        ordered.push(names[i].clone());
        for &target in &edges[i] {
            indegree[target] -= 1;
            if indegree[target] == 0 {
                ready.push(target);
            }
        }
    }
    if ordered.len() != names.len() {
        return Err(
            "static library link cycle detected; break the cycle (cizon does not add --start-group)."
                .into(),
        );
    }
    Ok(ordered)
}

fn ordered_exec_links(
    links: &[LinkTarget],
    libs: &[LibTarget],
    index: &HashMap<String, usize>,
) -> Result<Vec<LinkTarget>, String> {
    let roots: Vec<String> = links
        .iter()
        .filter_map(|link| match link {
            LinkTarget::Static(name) => Some(name.clone()),
            LinkTarget::Shared(_) => None,
        })
        .collect();
    let mut ordered: Vec<LinkTarget> = ordered_static_roots(&roots, libs, index)?
        .into_iter()
        .map(LinkTarget::Static)
        .collect();
    ordered.extend(
        links
            .iter()
            .filter(|link| matches!(link, LinkTarget::Shared(_)))
            .cloned(),
    );
    Ok(ordered)
}

/// Remove every `-DNDEBUG` spelling from test-source flags. Tests must not silently compile out
/// their own asserts: the exit-code contract is only meaningful while `assert` is live.
fn without_ndebug(flags: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(flags.len());
    let mut i = 0;
    while i < flags.len() {
        let flag = &flags[i];
        if flag == "-D"
            && flags
                .get(i + 1)
                .is_some_and(|v| v == "NDEBUG" || v.starts_with("NDEBUG="))
        {
            i += 2;
            continue;
        }
        if flag == "-DNDEBUG" || flag.starts_with("-DNDEBUG=") {
            i += 1;
            continue;
        }
        out.push(flag.clone());
        i += 1;
    }
    out
}

/// Resolve an optional installed-package prefix to an absolute directory inside the project.
/// cizon later expands it to -I<prefix>/include (compile) and -L<prefix>/lib (link) for the
/// owning target, so consuming an installed library needs no hand-written search-path flags.
fn resolve_prefix(dir: &Path, raw: Option<&str>, owner: &str) -> Result<Option<PathBuf>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    if raw.trim().is_empty() {
        return Err(format!("{owner} prefix path must not be empty."));
    }
    let path = resolve_project_path(dir, raw, owner, "prefix path")?;
    if !path.is_dir() {
        return Err(format!(
            "{owner} prefix {} is not a directory. Install the package there first (e.g. cmake --install), or remove the prefix.",
            rel_display(dir, &path)
        ));
    }
    ensure_dir_inside(dir, &path, &format!("{owner} prefix path"))?;
    Ok(Some(path))
}

/// Resolve an optional target include root at plan time: a missing or misplaced directory is a
/// configuration typo and must fail before the compiler ever sees an include.
fn resolve_include(dir: &Path, raw: Option<&str>, owner: &str) -> Result<Option<PathBuf>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    if raw.trim().is_empty() {
        return Err(format!("{owner} include path must not be empty."));
    }
    let path = resolve_project_path(dir, raw, owner, "include path")?;
    if !path.is_dir() {
        return Err(format!(
            "{owner} include path {} is not a directory. Create it or fix the path.",
            rel_display(dir, &path)
        ));
    }
    ensure_dir_inside(dir, &path, &format!("{owner} include path"))?;
    let root = std::fs::canonicalize(dir)
        .map_err(|e| format!("Cannot resolve project root {}: {e}", dir.display()))?;
    let real = std::fs::canonicalize(&path)
        .map_err(|e| format!("Cannot resolve include directory {}: {e}", path.display()))?;
    if real == root {
        return Err(format!(
            "{owner} include path must not be the project root; use include/ or a library-specific public directory."
        ));
    }
    Ok(Some(path))
}

/// Explicit target mode drops the implicit single-executable project, so src/ files that no
/// target compiles would silently stop being built. Warn once with a concrete fix.
pub(super) fn target_base_dirs(dir: &Path, targets: &[Target]) -> Vec<PathBuf> {
    let mut base_dirs = vec![dir.to_path_buf()];
    for target in targets {
        if !base_dirs.contains(&target.base_dir) {
            base_dirs.push(target.base_dir.clone());
        }
    }
    base_dirs
}

fn warn_unused_src(
    dir: &Path,
    base_dirs: &[PathBuf],
    bins: &[BinTarget],
    tests: &[BinTarget],
    libs: &[LibTarget],
    shared: &[SharedTarget],
) {
    for base in base_dirs {
        let unused = unused_sources(dir, std::slice::from_ref(base), bins, tests, libs, shared);
        if unused.is_empty() {
            continue;
        }
        let shown = unused
            .iter()
            .take(3)
            .map(|src| rel_display(dir, src))
            .collect::<Vec<_>>()
            .join(", ");
        let more = unused.len().saturating_sub(3);
        let suffix = if more > 0 {
            format!(" (+{more} more)")
        } else {
            String::new()
        };
        if base.as_path() == dir {
            eprintln!(
                "Warning: source files under src/ are not compiled by any [[target]]: {shown}{suffix}. \
Declaring [[target]] disables the implicit single-executable project; add them to a bin/test target \
(e.g. sources = [\"src\"]), or remove [[target]]."
            );
        } else {
            let label = rel_display(dir, base).replace('\\', "/");
            eprintln!(
                "Warning: source files under {label}/src/ are not compiled by any target in subproject {label}: \
{shown}{suffix}. Add them to a target in {label}/cizon.toml, or remove them."
            );
        }
    }
}

/// Project-relative source files under any target base dir's src/ that no [[target]]
/// compiles. Every base dir is scanned — a subproject's src/ as well as the root's —
/// and files are relativized against the project root so they match the `used` set.
/// Pure: `warn_unused_src` prints it, tests assert on it.
pub(super) fn unused_sources(
    dir: &Path,
    base_dirs: &[PathBuf],
    bins: &[BinTarget],
    tests: &[BinTarget],
    libs: &[LibTarget],
    shared: &[SharedTarget],
) -> Vec<PathBuf> {
    let mut used = HashSet::new();
    for source in bins
        .iter()
        .chain(tests.iter())
        .flat_map(|t| t.sources.iter())
        .chain(libs.iter().flat_map(|l| l.sources.iter()))
        .chain(shared.iter().flat_map(|s| s.sources.iter()))
    {
        if let Ok(rel) = project_relative(dir, source) {
            used.insert(rel);
        }
    }
    let mut unused = Vec::new();
    for base in base_dirs {
        unused.extend(unused_src_sources(&base.join("src"), dir, &used));
    }
    unused.sort();
    unused.dedup();
    unused
}

pub(super) fn unused_src_sources(
    src_dir: &Path,
    root: &Path,
    used: &HashSet<PathBuf>,
) -> Vec<PathBuf> {
    if !src_dir.is_dir() {
        return Vec::new();
    }
    let mut scanned = Vec::new();
    if collect_recursive(src_dir, &mut scanned).is_err() {
        return Vec::new();
    }
    let mut unused: Vec<PathBuf> = scanned
        .into_iter()
        .filter(|path| project_relative(root, path).map_or(true, |rel| !used.contains(&rel)))
        .collect();
    unused.sort();
    unused
}

/// Source files compiled into more than one executable target (bin/test). Recursive `sources`
/// directories can silently overlap; multiple main() definitions then fail to link.
pub(super) fn overlapping_exec_sources(
    bins: &[BinTarget],
    tests: &[BinTarget],
) -> Vec<(PathBuf, Vec<String>)> {
    let mut map: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    for target in bins.iter().chain(tests.iter()) {
        for src in &target.sources {
            map.entry(src.clone()).or_default().push(target.name.clone());
        }
    }
    map.into_iter().filter(|(_, names)| names.len() > 1).collect()
}

/// Resolve bin/test `links` entries against the planned libraries (bin and test targets link the
/// same artifact kinds: static archives and shared import libraries).
fn resolve_exec_links(
    links: &[String],
    libs: &[LibTarget],
    shared: &[SharedTarget],
) -> Vec<LinkTarget> {
    links
        .iter()
        .map(|link| {
            let lower = link.to_lowercase();
            if let Some(lib) = libs.iter().find(|lib| lib.name.to_lowercase() == lower) {
                LinkTarget::Static(lib.name.clone())
            } else if let Some(sh) = shared.iter().find(|sh| sh.name.to_lowercase() == lower) {
                LinkTarget::Shared(sh.name.clone())
            } else {
                LinkTarget::Static(link.clone())
            }
        })
        .collect()
}

pub(super) fn exe_name(name: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// Output path: relative to the project root; falls back to the original path when outside it.

pub(super) fn resolve_source_path(dir: &Path, raw: &str, owner: &str) -> Result<PathBuf, String> {
    if raw.trim().is_empty() {
        return Err(format!("{owner} sources must not contain an empty path."));
    }
    resolve_project_path(dir, raw, owner, "sources")
}

fn resolve_project_path(
    dir: &Path,
    raw: &str,
    owner: &str,
    what: &str,
) -> Result<PathBuf, String> {
    let rel = Path::new(raw);
    let bad_component = rel.components().any(|c| {
        !matches!(
            c,
            std::path::Component::Normal(_) | std::path::Component::CurDir
        )
    });
    if rel.is_absolute() || bad_component {
        return Err(format!(
            "{owner} {what} must be a project-relative path (no absolute path, no ..): {raw}"
        ));
    }
    Ok(dir.join(rel))
}

/// Source directories must be inside the project; reject symlink/junction directories pointing
/// outside the project or at the project root.
pub(super) fn ensure_dir_inside(dir: &Path, path: &Path, owner: &str) -> Result<(), String> {
    let root = std::fs::canonicalize(dir)
        .map_err(|e| format!("Cannot resolve project root {}: {e}", dir.display()))?;
    let canonical = std::fs::canonicalize(path)
        .map_err(|e| format!("Cannot resolve {owner} directory {}: {e}", path.display()))?;
    if canonical == root {
        return Err(format!("{owner} must not point at the project root."));
    }
    if !canonical.starts_with(&root) {
        return Err(format!(
            "Directory of {owner} is outside the project (symlink/junction?): {}",
            path.display()
        ));
    }
    Ok(())
}

/// Explicit sources or recursive scan of src/. Same-name stem conflicts in the same directory
/// error out immediately (object files are named by path mirror).
pub(super) fn collect_sources(dir: &Path, explicit: Option<&[String]>) -> Result<Vec<PathBuf>, String> {
    let mut out: Vec<PathBuf> = Vec::new();
    match explicit {
        Some(list) => {
            for raw in list {
                let path = resolve_source_path(dir, raw, "[project].sources")?;
                if path.is_dir() {
                    ensure_dir_inside(dir, &path, "[project].sources")?;
                    collect_recursive(&path, &mut out)?;
                } else if path.is_file() {
                    check_source_ext(&path)?;
                    out.push(path);
                } else {
                    return Err(format!(
                        "Path listed in [project].sources does not exist: {}",
                        path.display()
                    ));
                }
            }
            out.sort();
        }
        None => {
            let src = dir.join("src");
            if !src.is_dir() {
                return Err(format!("src/ directory not found (or not readable): {}", src.display()));
            }
            ensure_dir_inside(dir, &src, "src/")?;
            collect_recursive(&src, &mut out)?;
            out.sort();
        }
    }
    let owner = if explicit.is_some() {
        "[project].sources"
    } else {
        "src/"
    };
    check_object_paths(dir, &out, owner)?;
    Ok(out)
}

/// A target's sources: files or directories (directories scanned recursively); bin defaults to
/// scanning src/.
pub(super) fn collect_target_sources(
    dir: &Path,
    list: Option<&[String]>,
    owner: &str,
) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    match list {
        None => {
            let src = dir.join("src");
            if !src.is_dir() {
                return Err(format!("src/ directory not found (or not readable): {}", src.display()));
            }
            ensure_dir_inside(dir, &src, owner)?;
            collect_recursive(&src, &mut out)?;
        }
        Some(entries) => {
            for raw in entries {
                let path = resolve_source_path(dir, raw, owner)?;
                if path.is_dir() {
                    ensure_dir_inside(dir, &path, owner)?;
                    collect_recursive(&path, &mut out)?;
                } else if path.is_file() {
                    check_source_ext(&path)?;
                    out.push(path);
                } else {
                    return Err(format!("Path listed in {owner} sources does not exist: {}", path.display()));
                }
            }
        }
    }
    out.sort();
    check_object_paths(dir, &out, owner)?;
    Ok(out)
}

/// Object paths within one target must be unique; same stem across directories is allowed,
/// same stem within the same directory errors out.
pub(super) fn check_object_paths(dir: &Path, sources: &[PathBuf], owner: &str) -> Result<(), String> {
    let mut seen: HashMap<String, PathBuf> = HashMap::new();
    for src in sources {
        let rel = project_relative(dir, src)?;
        let mut obj_rel = rel;
        obj_rel.set_extension("o");
        let key = obj_rel.to_string_lossy().to_lowercase();
        if let Some(prev) = seen.insert(key, src.clone()) {
            if prev == *src {
                return Err(format!("{owner} lists the same source more than once: {}", src.display()));
            }
            return Err(format!(
                "{owner} object path conflict:\n  {}\n  {}\nSource file stems within the same directory must be unique (same name across directories is allowed).",
                prev.display(),
                src.display()
            ));
        }
    }
    // An object file could also be treated as a parent directory by another source directory
    // (e.g. src/foo.o/bar.cpp).
    let mut artifacts: HashSet<String> = HashSet::new();
    for key in seen.keys() {
        artifacts.insert(key.clone());
        artifacts.insert(format!("{key}.d"));
        artifacts.insert(format!("{key}.tmp"));
        artifacts.insert(format!("{key}.d.tmp"));
    }
    for key in seen.keys() {
        for ancestor in Path::new(key).ancestors().skip(1) {
            if artifacts.contains(&ancestor.to_string_lossy().to_lowercase()) {
                return Err(format!(
                    "{owner} object/dep file path conflicts with a directory: {key}"
                ));
            }
        }
    }
    Ok(())
}

/// Path of a source file relative to the project root (used to mirror object paths).
pub(super) fn project_relative(dir: &Path, src: &Path) -> Result<PathBuf, String> {
    let rel = src
        .strip_prefix(dir)
        .map_err(|_| format!("Source file is not inside the project: {}", src.display()))?;
    let mut out = PathBuf::new();
    for component in rel.components() {
        match component {
            std::path::Component::Normal(part) => out.push(part),
            std::path::Component::CurDir => {}
            _ => {
                return Err(format!(
                    "Source file path contains an illegal component (.. / root / prefix): {}",
                    src.display()
                ));
            }
        }
    }
    if out.as_os_str().is_empty() {
        return Err(format!("Source file path is empty: {}", src.display()));
    }
    Ok(out)
}

pub(super) fn is_source(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| SOURCE_EXTS.iter().any(|ext| ext.eq_ignore_ascii_case(e)))
}

/// Recursively collect source files under a directory. Do not follow symlink/junction
/// directories (cycle guard); linked source files are still collected.
pub(super) fn collect_recursive(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("Cannot read {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("Cannot read directory entries of {}: {e}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| format!("Cannot read type of {}: {e}", path.display()))?;
        if file_type.is_dir() {
            collect_recursive(&path, out)?;
        } else if is_source(&path) && path.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

pub(super) fn check_source_ext(path: &Path) -> Result<(), String> {
    if is_source(path) {
        Ok(())
    } else {
        Err(format!(
            "sources list only accepts {:?}, rejected: {}",
            SOURCE_EXTS,
            path.display()
        ))
    }
}

// ── Build execution ──────────────────────────────────────────────

pub(super) fn obj_for(dir: &Path, src: &Path, obj_dir: &Path) -> Result<PathBuf, String> {
    let mut rel = project_relative(dir, src)?;
    rel.set_extension("o");
    Ok(obj_dir.join(rel))
}

pub(super) fn lib_obj_dir(scope_dir: &Path, name: &str) -> PathBuf {
    scope_dir.join("lib").join(name).join("obj")
}

pub(super) fn lib_obj_for(
    dir: &Path,
    scope_dir: &Path,
    name: &str,
    src: &Path,
) -> Result<PathBuf, String> {
    let mut rel = project_relative(dir, src)?;
    rel.set_extension("o");
    Ok(lib_obj_dir(scope_dir, name).join(rel))
}

pub(super) fn shared_obj_dir(scope_dir: &Path, name: &str) -> PathBuf {
    scope_dir.join(".shared").join(name).join("obj")
}

pub(super) fn bin_obj_dir(scope_dir: &Path, name: &str, multi: bool) -> PathBuf {
    if multi {
        scope_dir.join("obj").join(name)
    } else {
        scope_dir.join("obj")
    }
}

/// Test objects live in their own tree so `build`/`run` (obj/) and `cizon test` (test-obj/)
/// never invalidate each other's artifacts, even in a single-bin project where obj/ mirrors
/// src/ paths directly.
pub(super) fn test_obj_dir(scope_dir: &Path, name: &str) -> PathBuf {
    scope_dir.join("test-obj").join(name)
}

pub(super) fn exec_obj_dir(scope_dir: &Path, name: &str, multi: bool, set: super::ExecSet) -> PathBuf {
    match set {
        super::ExecSet::Bins => bin_obj_dir(scope_dir, name, multi),
        super::ExecSet::Tests => test_obj_dir(scope_dir, name),
    }
}

pub(super) fn shared_obj_for(
    dir: &Path,
    scope_dir: &Path,
    name: &str,
    src: &Path,
) -> Result<PathBuf, String> {
    let mut rel = project_relative(dir, src)?;
    rel.set_extension("o");
    Ok(shared_obj_dir(scope_dir, name).join(rel))
}

#[cfg(test)]
mod tests {
    use super::without_ndebug;

    #[test]
    fn test_flags_strip_every_ndebug_spelling() {
        let flags = vec![
            "-std=c++23".to_string(),
            "-DNDEBUG".to_string(),
            "-D".to_string(),
            "NDEBUG".to_string(),
            "-D".to_string(),
            "NDEBUG=1".to_string(),
            "-DNDEBUG=1".to_string(),
            "-Wall".to_string(),
        ];
        assert_eq!(without_ndebug(&flags), ["-std=c++23", "-Wall"]);
        // NDEBUG is case-sensitive; unrelated defines survive.
        let flags = vec!["-D".to_string(), "DEBUG".to_string()];
        assert_eq!(without_ndebug(&flags), flags);
    }

    #[test]
    fn overlapping_exec_sources_flags_shared_files() {
        use super::{overlapping_exec_sources, BinTarget};
        use std::path::PathBuf;
        let mk = |name: &str, src: &str| BinTarget {
            name: name.to_string(),
            sources: vec![PathBuf::from(src)],
            links: vec![],
            out_name: format!("{name}.exe"),
            extern_links: vec![],
            include: None,
            prefix: None,
            compile_flags: vec![],
        };
        let bins = vec![mk("app", "src/main.cpp")];
        let tests = vec![mk("t1", "tests/a.cpp"), mk("t2", "tests/a.cpp")];
        let overlaps = overlapping_exec_sources(&bins, &tests);
        assert_eq!(overlaps.len(), 1);
        assert_eq!(overlaps[0].0, PathBuf::from("tests/a.cpp"));
        assert_eq!(overlaps[0].1, vec!["t1".to_string(), "t2".to_string()]);
        // Disjoint sources produce no warning.
        let tests2 = vec![mk("t1", "tests/a.cpp"), mk("t2", "tests/b.cpp")];
        assert!(overlapping_exec_sources(&bins, &tests2).is_empty());
    }
}
