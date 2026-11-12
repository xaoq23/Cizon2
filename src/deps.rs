//! `cizon2 add <git-url> [--tag <tag>]`: the minimal correct implementation of git
//! dependencies (v0.2 route).
//!
//! Design boundaries:
//! - Commit to the "regular layout": `<dep>/include/` headers + flat `<dep>/src/*.cpp`;
//!   libraries beyond this shape are not accommodated -- that is CMake's territory
//! - `cizon.lock` is a machine file (rewritten as a whole table each time, comments not
//!   preserved), the locked commit is the single source of truth; any mismatch between
//!   cizon.toml <-> cizon.lock <-> deps/ -> report loudly, never silently self-heal
//! - Header paths are written into flags unconditionally (`-I<dep>/include` and `-I<dep>`);
//!   when the directory does not exist the compiler safely ignores it, so planning does not
//!   need to do any network operation first

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config;

const CIZON_LOCK: &str = "cizon.lock";
const DEP_EXTS: &[&str] = &["cpp", "cc", "cxx"];

/// `cizon2 add <git-url> [--tag <tag>]`
pub fn add_cmd(args: &[String]) -> Result<i32, String> {
    let dir = std::env::current_dir().map_err(|e| format!("failed to read current directory: {e}"))?;
    add_in(&dir, args)
}

pub fn add_in(dir: &Path, args: &[String]) -> Result<i32, String> {
    let (url, tag) = parse_args(args)?;
    let cfg = config::load(dir)?;
    let name = dep_name(&url)?;

    if cfg.dependencies().map_or(false, |m| m.contains_key(&name)) {
        return Err(format!("[dependencies] already contains {name}. Delete that entry and retry."));
    }
    let dep_dir = dir.join("deps").join(&name);
    if dep_dir.exists() {
        return Err(format!(
            "deps/{name} exists but is not managed by cizon2. Confirm it is unneeded, delete the directory, then rerun add."
        ));
    }

    let toml_path = dir.join("cizon.toml");
    let original_toml =
        std::fs::read_to_string(&toml_path).map_err(|e| format!("failed to read cizon.toml: {e}"))?;
    let lock_path = dir.join(CIZON_LOCK);
    let original_lock = match std::fs::read_to_string(&lock_path) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("failed to read {CIZON_LOCK}: {e}")),
    };

    let rel = format!("deps/{name}");
    let commit = match shallow_clone(&dir, &url, tag.as_deref(), &rel) {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dep_dir);
            return Err(e);
        }
    };
    if let Err(e) = append_entry(&dir, &name, &url, tag.as_deref()) {
        let toml_ok = std::fs::write(&toml_path, &original_toml).is_ok();
        let dep_ok = remove_dep_dir(&dep_dir);
        return Err(rollback_error(e, toml_ok && dep_ok, &name));
    }
    if let Err(e) = write_lock_entry(&dir, &name, &url, tag.as_deref(), &commit) {
        let toml_ok = std::fs::write(&toml_path, &original_toml).is_ok();
        let lock_ok = match &original_lock {
            Some(text) => std::fs::write(&lock_path, text).is_ok(),
            None => remove_file_if_exists(&lock_path),
        };
        let dep_ok = remove_dep_dir(&dep_dir);
        return Err(rollback_error(e, toml_ok && lock_ok && dep_ok, &name));
    }
    ignore_deps_dir(&dir);
    if let Some(warning) = layout_warning(&dep_dir, &name) {
        eprintln!("{warning}");
    }

    println!("\x1b[32m      Added\x1b[0m {name} → deps/{name} (locked commit {commit})");
    Ok(0)
}

fn remove_dep_dir(path: &Path) -> bool {
    match std::fs::remove_dir_all(path) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => false,
    }
}

/// Dependency layout hint, shown at acquisition time (add/update). A non-standard layout does not
/// block the build; this tells the user whether the dep is usable as-is (header-only) or must be
/// built externally and linked through extern_links.
fn layout_warning(dep_dir: &Path, name: &str) -> Option<String> {
    if dep_dir.join("include").is_dir() {
        return None;
    }
    let has_sources = flat_sources(&dep_dir.join("src"))
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    if !has_sources {
        return Some(format!(
            "Note: deps/{name} has no include/ and no flat src/*.cpp, so cizon2 compiles nothing from it. If it is header-only (e.g. doctest), its headers may already be reachable through the automatic -Ideps/{name} include path — just write a test target. If it must be compiled (e.g. googletest, Catch2), build/install it with its own build system and link it through a test target's extern_links (e.g. extern_links = [\"gtest_main\", \"gtest\"])."
        ));
    }
    Some(format!(
        "Warning: deps/{name} is missing include/, which does not match the cizon2 dependency layout (include/ + flat src/*.cpp). Build it with its own build system; compiled test libraries belong in a test target's extern_links, header-only frameworks can be vendored under include/."
    ))
}

fn remove_file_if_exists(path: &Path) -> bool {
    match std::fs::remove_file(path) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => false,
    }
}

fn rollback_error(err: String, ok: bool, name: &str) -> String {
    if ok {
        format!("{err}\nRolled back cizon.toml, {CIZON_LOCK} and deps/{name}.")
    } else {
        format!(
            "{err}\nAutomatic rollback failed, please check cizon.toml, {CIZON_LOCK} and deps/{name} manually."
        )
    }
}

fn parse_args(args: &[String]) -> Result<(String, Option<String>), String> {
    let usage = "Usage: cizon2 add <git-url> [--tag <tag>]";
    let mut iter = args.iter();
    let url = match iter.next() {
        Some(u) if !u.is_empty() => u.clone(),
        _ => return Err(usage.into()),
    };
    let mut tag = None;
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--tag" => match iter.next() {
                Some(t) if !t.is_empty() => tag = Some(t.clone()),
                _ => return Err("--tag requires an argument.".into()),
            },
            other => return Err(format!("Unknown argument {other}. {usage}")),
        }
    }
    Ok((url, tag))
}

/// Dependency name = the last URL segment after stripping trailing / and .git.
/// Only letters, digits, - and _ are allowed (the intersection of a valid TOML bare key
/// and a valid Windows directory name).
fn dep_name(url: &str) -> Result<String, String> {
    let mut name = url
        .replace('\\', "/")
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string();
    if name.to_lowercase().ends_with(".git") {
        let new_len = name.len() - 4;
        name.truncate(new_len);
    }
    if name.is_empty() {
        return Err("failed to parse a dependency name from the URL.".into());
    }
    if !config::is_valid_dep_name(&name) {
        return Err(format!("dependency name {name} contains illegal characters (only letters, digits, - and _ are allowed)."));
    }
    Ok(name)
}

/// Shallow clone (locate with --branch when a tag is given) into `rel`, return the resulting commit.
fn shallow_clone(dir: &Path, url: &str, tag: Option<&str>, rel: &str) -> Result<String, String> {
    let mut args: Vec<String> = ["clone", "--depth", "1"]
        .iter()
        .map(|s| (*s).into())
        .collect();
    if let Some(t) = tag {
        args.push("--branch".into());
        args.push(t.into());
    }
    args.push(url.into());
    args.push(rel.into());
    git_cmd(dir, &args)?;
    git_cmd(dir, &["-C".into(), rel.into(), "rev-parse".into(), "HEAD".into()])
}

/// Centralize git execution details in one place: cwd = project root, args forwarded
/// verbatim, stdout returned trimmed.
fn git_cmd(dir: &Path, args: &[String]) -> Result<String, String> {
    let out = Command::new("git")
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .args(args)
        .output()
        .map_err(|e| format!("failed to start git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed: {}",
            git_verb(args),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Show the real git subcommand in error messages (skipping -C/-c and their values).
fn git_verb(args: &[String]) -> &str {
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-C" | "-c" => i += 2,
            s if s.starts_with('-') => i += 1,
            s => return s,
        }
    }
    "git"
}

/// Escape a TOML basic string (both URL and tag go through here).
fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn entry_line(name: &str, url: &str, tag: Option<&str>) -> String {
    match tag {
        Some(t) => format!("{name} = {{ git = \"{}\", tag = \"{}\" }}", esc(url), esc(t)),
        None => format!("{name} = {{ git = \"{}\" }}", esc(url)),
    }
}

/// `[dependencies]` table header; allows whitespace inside and around the brackets and a
/// trailing comment, but never mistakes `[dependencies.foo]` for the header.
fn is_dependencies_header(line: &str) -> bool {
    let t = line.trim().trim_start_matches('\u{feff}').trim();
    let Some(rest) = t.strip_prefix('[') else {
        return false;
    };
    let Some((inner, tail)) = rest.split_once(']') else {
        return false;
    };
    inner.trim() == "dependencies"
        && (tail.trim().is_empty() || tail.trim_start().starts_with('#'))
}

/// Register in cizon.toml: if a [dependencies] header exists, insert on the line after it;
/// otherwise append the whole section to the end of the file. Re-parse before writing,
/// refusing to turn a valid file into invalid TOML.
fn append_entry(dir: &Path, name: &str, url: &str, tag: Option<&str>) -> Result<(), String> {
    let path = dir.join("cizon.toml");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("failed to read cizon.toml: {e}"))?;
    let has_header = text.lines().any(is_dependencies_header);
    if !has_header && text.parse::<toml::Table>().is_ok_and(|t| t.contains_key("dependencies")) {
        return Err(
            "cizon.toml declares dependencies with an inline table; change it to a [dependencies] header and retry (file not modified)."
                .into(),
        );
    }
    let entry = entry_line(name, url, tag);
    let mut out = String::new();
    let mut inserted = false;
    for line in text.lines() {
        out.push_str(line);
        out.push('\n');
        if !inserted && is_dependencies_header(line) {
            out.push_str(&entry);
            out.push('\n');
            inserted = true;
        }
    }
    if !inserted {
        out.push_str("\n[dependencies]\n");
        out.push_str(&entry);
        out.push('\n');
    }
    let table: toml::Table = out
        .parse()
        .map_err(|e| format!("updating cizon.toml would produce invalid TOML (file not modified): {e}"))?;
    let registered = table
        .get("dependencies")
        .and_then(|v| v.as_table())
        .is_some_and(|t| t.contains_key(name));
    if !registered {
        return Err(format!(
            "failed to register {name} in cizon.toml's [dependencies] (file not modified). Check that section's format."
        ));
    }
    std::fs::write(&path, out).map_err(|e| format!("failed to write cizon.toml: {e}"))
}

/// The lock file is a machine file: read the original table → insert/update the entry →
/// rewrite the whole table.
fn write_lock_entry(
    dir: &Path,
    name: &str,
    url: &str,
    tag: Option<&str>,
    commit: &str,
) -> Result<(), String> {
    let lock_path = dir.join(CIZON_LOCK);
    let mut table: toml::Table = match std::fs::read_to_string(&lock_path) {
        Ok(text) => text
            .parse()
            .map_err(|e| format!("{CIZON_LOCK} failed to parse (file not modified): {e}"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
        Err(e) => return Err(format!("failed to read {CIZON_LOCK}: {e}")),
    };
    let mut entry = toml::Table::new();
    entry.insert("git".into(), url.into());
    if let Some(t) = tag {
        entry.insert("tag".into(), t.into());
    }
    entry.insert("commit".into(), commit.into());
    table.insert(name.into(), entry.into());
    let text = toml::to_string(&table).map_err(|e| format!("failed to serialize the lock: {e}"))?;
    std::fs::write(&lock_path, text).map_err(|e| format!("failed to write {CIZON_LOCK}: {e}"))
}

/// Best-effort add deps/ to .gitignore (create it if missing; leave it alone if a deps/
/// line already exists). On a read failure only warn, never overwrite the user's file with
/// empty content.
fn ignore_deps_dir(dir: &Path) {
    let path = dir.join(".gitignore");
    let mut text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            eprintln!("Warning: failed to read {}, .gitignore not updated: {e}", path.display());
            return;
        }
    };
    if text.lines().any(|l| l.trim() == "deps/") {
        return;
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str("deps/\n");
    if let Err(e) = std::fs::write(&path, text) {
        eprintln!("Warning: failed to update {}: {e}", path.display());
    }
}

// ─────────────────────────────────────────────────────────────
// Third stage of the lock lifecycle: update -- align the lock and deps with the intent in
// cizon.toml. Upgrade/downgrade/cross-grade share one command: the tag is only changed in
// cizon.toml, update lands it; build always executes what the lock has already decided,
// never chasing new wishes in the toml.
// ─────────────────────────────────────────────────────────────

/// `cizon2 update <dependency-name>`: align the lock and deps with cizon.toml's tag intent
/// (upgrade and downgrade are symmetric).
pub fn update_cmd(args: &[String]) -> Result<i32, String> {
    let dir = std::env::current_dir().map_err(|e| format!("failed to read current directory: {e}"))?;
    update_in(&dir, args)
}

pub fn update_in(dir: &Path, args: &[String]) -> Result<i32, String> {
    if args.len() != 1 || args[0].is_empty() {
        return Err("Usage: cizon2 update <dependency-name>".into());
    }
    let name = &args[0];
    let cfg = config::load(dir)?;
    let info = cfg
        .dependencies()
        .and_then(|m| m.get(name))
        .ok_or_else(|| format!("{name} is not in [dependencies]. Run cizon2 add first, or check the spelling."))?;

    let deps_dir = dir.join("deps");
    std::fs::create_dir_all(&deps_dir)
        .map_err(|e| format!("failed to create {}: {e}", deps_dir.display()))?;
    let dep_dir = deps_dir.join(name);
    // update replaces the whole checkout; uncommitted changes must be explicitly discarded
    if dep_dir.exists() {
        let status = git_cmd(
            dir,
            &[
                "-C".into(),
                format!("deps/{name}"),
                "status".into(),
                "--porcelain".into(),
            ],
        )?;
        if !status.is_empty() {
            return Err(format!(
                "deps/{name} has uncommitted changes, update would discard them. Commit and push to the remote, update cizon.toml, then retry; or delete the directory to discard the changes."
            ));
        }
    }
    // Clone to a temp directory first: on a network failure keep the old checkout instead of
    // deleting then pulling
    let tmp_rel = format!(
        "deps/.{name}.tmp-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    );
    let tmp_dir = dir.join(&tmp_rel);
    let commit = match shallow_clone(dir, &info.git, info.tag.as_deref(), &tmp_rel) {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return Err(e);
        }
    };
    if dep_dir.exists() {
        if let Err(e) = std::fs::remove_dir_all(&dep_dir) {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return Err(format!("failed to delete deps/{name}: {e}"));
        }
    }
    if let Err(e) = std::fs::rename(&tmp_dir, &dep_dir) {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(format!("failed to move {tmp_rel} to deps/{name}: {e}"));
    }
    if let Err(e) = write_lock_entry(dir, name, &info.git, info.tag.as_deref(), &commit) {
        return Err(format!(
            "{e}\nThe dependency directory was updated, but writing the lock failed; fix it and rerun cizon2 update {name}."
        ));
    }
    println!("\x1b[32m   Updated\x1b[0m {name} @ {commit}");
    Ok(0)
}

// ─────────────────────────────────────────────────────────────
// Dependency contract during build: restore / verify / produce the compile triple
// ─────────────────────────────────────────────────────────────

/// Called by build: skip when the dependency directory already matches the lock, restore
/// from the lock when it is missing (including tag verification), and on success return the
/// (dep-name, src, obj) compile triple; obj goes to obj_dir/<dep>/<stem>.o, physically
/// isolated from the project objs.
pub fn prepare(
    dir: &Path,
    cfg: &config::Config,
    obj_dir: &Path,
) -> Result<Vec<(String, PathBuf, PathBuf)>, String> {
    let Some(map) = cfg.dependencies() else { return Ok(vec![]) };
    if map.is_empty() {
        return Ok(vec![]);
    }
    let lock = read_lock(dir)?;

    let mut jobs = Vec::new();
    for (name, info) in map {
        restore_dep(dir, name, info, &lock)?;
        let dep_dir = dir.join("deps").join(name);
        let src_dir = dep_dir.join("src");
        let dep_obj_dir = obj_dir.join(name);
        std::fs::create_dir_all(&dep_obj_dir)
            .map_err(|e| format!("failed to create {}: {e}", dep_obj_dir.display()))?;
        let mut stems: HashMap<String, PathBuf> = HashMap::new();
        for src in flat_sources(&src_dir)? {
            // A C++20 module unit (e.g. fmt's src/fmt.cc) cannot be compiled the normal way,
            // and a normal .cpp can never start with `module;` -- skip it silently.
            if is_module_unit(&src)? {
                continue;
            }
            let stem = src.file_stem().unwrap().to_string_lossy().into_owned();
            if let Some(prev) = stems.insert(stem.to_ascii_lowercase(), src.clone()) {
                return Err(format!(
                    "object name conflict (same stem) among {name}'s dependency source files:\n  {}\n  {}\nKeep the dependency's src/ flat with unique file names.",
                    prev.display(),
                    src.display()
                ));
            }
            let obj = dep_obj_dir.join(format!("{stem}.o"));
            jobs.push((name.clone(), src, obj));
        }
    }
    Ok(jobs)
}

fn is_commit_hash(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// `cizon2 sync`: restore/verify every dependency checkout from cizon.lock without compiling or
/// changing cizon.toml intent. A fresh clone of the project materializes its deps from the lock.
pub fn sync(dir: &Path, cfg: &config::Config) -> Result<usize, String> {
    let Some(map) = cfg.dependencies() else {
        return Ok(0);
    };
    if map.is_empty() {
        return Ok(0);
    }
    let lock = read_lock(dir)?;
    for (name, info) in map {
        restore_dep(dir, name, info, &lock)?;
    }
    Ok(map.len())
}

fn read_lock(dir: &Path) -> Result<toml::Table, String> {
    let lock_path = dir.join(CIZON_LOCK);
    let lock_text = match std::fs::read_to_string(&lock_path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "{CIZON_LOCK} is missing, but [dependencies] declares dependencies. Run cizon2 update <dependency-name> to rebuild the lock from cizon.toml."
            ));
        }
        Err(e) => return Err(format!("failed to read {CIZON_LOCK}: {e}")),
    };
    lock_text
        .parse()
        .map_err(|e| format!("{CIZON_LOCK} failed to parse: {e}"))
}

/// Restore/verify one dependency checkout against the lock (no compilation): validate the lock
/// entry, ensure the checkout sits at the pinned commit, and refuse a dirty or mismatched tree.
fn restore_dep(
    dir: &Path,
    name: &str,
    info: &config::DepInfo,
    lock: &toml::Table,
) -> Result<(), String> {
    let entry = lock
        .get(name)
        .and_then(|v| v.as_table())
        .ok_or_else(|| {
            format!("no lock for {name} in {CIZON_LOCK}. Run cizon2 update {name} to create the lock.")
        })?;
    if entry.get("git").and_then(|v| v.as_str()) != Some(info.git.as_str()) {
        return Err(format!(
            "[dependencies] and {CIZON_LOCK} have a git URL mismatch for {name}. Follow [dependencies]: run cizon2 update {name} to align."
        ));
    }
    if entry.get("tag").and_then(|v| v.as_str()) != info.tag.as_deref() {
        return Err(format!(
            "[dependencies] and {CIZON_LOCK} have a tag mismatch for {name}. Run cizon2 update {name} to align."
        ));
    }
    let Some(commit) = entry.get("commit").and_then(|v| v.as_str()) else {
        return Err(format!(
            "{name} in {CIZON_LOCK} is missing a commit. Run cizon2 update {name} to rebuild the lock."
        ));
    };
    if !is_commit_hash(commit) {
        return Err(format!(
            "{name}'s commit in {CIZON_LOCK} is not a 40/64-character hex string. Run cizon2 update {name} to rebuild the lock."
        ));
    }
    let commit = commit.to_ascii_lowercase();

    let dep_dir = dir.join("deps").join(name);
    if dep_dir.exists() {
        let head = git_cmd(
            dir,
            &[
                "-C".into(),
                format!("deps/{name}"),
                "rev-parse".into(),
                "HEAD".into(),
            ],
        )?;
        if head != commit {
            return Err(format!(
                "deps/{name}'s HEAD ({head}) mismatches the lock ({commit}). If a local commit must be kept, push to the remote and update cizon.toml's tag first; otherwise run cizon2 update {name} to align, or delete the directory and let sync restore from the lock."
            ));
        }
        let status = git_cmd(
            dir,
            &[
                "-C".into(),
                format!("deps/{name}"),
                "status".into(),
                "--porcelain".into(),
            ],
        )?;
        if !status.is_empty() {
            return Err(format!(
                "deps/{name} has uncommitted changes, the lock no longer represents the actual code. Commit and push to the remote, update cizon.toml's tag, then run update; or delete the directory and let sync restore from the lock."
            ));
        }
    } else {
        clone_pinned(dir, &info.git, info.tag.as_deref(), &commit, name)?;
        eprintln!("\x1b[32m   Restoring\x1b[0m {name} @ {commit}");
    }
    Ok(())
}

/// Restore by tag (shallow clone + commit verification) or by bare commit (full clone +
/// checkout). On failure, clean up the half-built directory so the next build never hits an
/// untrusted checkout.
fn clone_pinned(
    dir: &Path,
    url: &str,
    tag: Option<&str>,
    commit: &str,
    name: &str,
) -> Result<(), String> {
    let rel = format!("deps/{name}");
    let target = dir.join(&rel);
    let result = (|| -> Result<(), String> {
        match tag {
            Some(t) => {
                git_cmd(dir, &[
                    "clone".into(), "--depth".into(), "1".into(),
                    "--branch".into(), t.into(), url.into(), rel.clone(),
                ])?;
                let head =
                    git_cmd(dir, &["-C".into(), rel.clone(), "rev-parse".into(), "HEAD".into()])?;
                if head != commit {
                    return Err(format!(
                        "tag {t} now points to {head}, which mismatches the lock {commit} (the tag was moved). Run cizon2 update {name} to refresh the lock."
                    ));
                }
            }
            None => {
                git_cmd(dir, &["clone".into(), url.into(), rel.clone()])?;
                git_cmd(dir, &["-C".into(), rel.clone(), "checkout".into(), commit.into()])?;
                let head =
                    git_cmd(dir, &["-C".into(), rel, "rev-parse".into(), "HEAD".into()])?;
                if head != commit {
                    return Err(format!(
                        "after checkout {commit}, HEAD is {head}, which mismatches the lock. Run cizon2 update {name} to rebuild the lock."
                    ));
                }
            }
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&target);
    }
    result
}

/// Whether this is a C++20 module unit: after skipping BOM, whitespace and line/block
/// comments, the first token is `module;`.
fn is_module_unit(path: &Path) -> Result<bool, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let text = String::from_utf8_lossy(&bytes);
    let mut rest = text.trim_start_matches('\u{feff}');
    loop {
        rest = rest.trim_start();
        if let Some(after) = rest.strip_prefix("//") {
            rest = after.split_once('\n').map_or("", |(_, r)| r);
        } else if let Some(after) = rest.strip_prefix("/*") {
            let Some(end) = after.find("*/") else {
                return Ok(false);
            };
            rest = &after[end + 2..];
        } else {
            break;
        }
    }
    Ok(rest.starts_with("module;"))
}

/// Flat scan of the dep's src/ (the dependency contract is still a flat layout, no recursion).
fn flat_sources(src: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = match std::fs::read_dir(src) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(format!("failed to read {}: {e}", src.display())),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("failed to read a directory entry of {}: {e}", src.display()))?;
        let path = entry.path();
        let is_source = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| DEP_EXTS.iter().any(|ext| ext.eq_ignore_ascii_case(e)));
        if !is_source {
            continue;
        }
        // Only fetch metadata for source files; follow symlinks, treat dangling links as nonexistent
        match std::fs::metadata(&path) {
            Ok(meta) if meta.is_file() => out.push(path),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(format!("failed to read metadata of {}: {e}", path.display()));
            }
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "cizon2-deps-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ))
    }

    fn has_git() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// A local "remote" repo (no network): include/dep.h + flat src/dep.cpp + init commit.
    fn make_origin(dir: &Path, with_tag: bool) {
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("include")).unwrap();
        fs::write(dir.join("include/dep.h"), "int depv();\n").unwrap();
        fs::write(dir.join("src/dep.cpp"), "int depv() { return 42; }\n").unwrap();
        git_cmd(dir, &["init".into()]).expect("this test needs git on PATH");
        git_cmd(dir, &["add".into(), "-A".into()]).unwrap();
        git_cmd(
            dir,
            &[
                "-c".into(),
                "user.name=t".into(),
                "-c".into(),
                "user.email=t@t".into(),
                "commit".into(),
                "-m".into(),
                "init".into(),
                "--no-gpg-sign".into(),
            ],
        )
        .unwrap();
        if with_tag {
            git_cmd(dir, &["tag".into(), "v1".into()]).unwrap();
        }
    }

    fn add_args(dir: &Path) -> [String; 1] {
        [dir.to_string_lossy().into_owned()]
    }

    fn find_named(dir: &Path, name: &str) -> Option<PathBuf> {
        for entry in fs::read_dir(dir).ok()?.flatten() {
            let path = entry.path();
            if path.is_file() && path.file_name().is_some_and(|n| n == name) {
                return Some(path);
            }
            if path.is_dir() {
                if let Some(found) = find_named(&path, name) {
                    return Some(found);
                }
            }
        }
        None
    }

    fn find_obj(proj: &Path, dep: Option<&str>, stem: &str) -> PathBuf {
        let target = format!("{stem}.o");
        for scope in fs::read_dir(proj.join("build")).unwrap().flatten() {
            let root = match dep {
                Some(name) => scope.path().join("deps-obj").join(name),
                None => scope.path().join("obj"),
            };
            if let Some(path) = find_named(&root, &target) {
                return path;
            }
        }
        panic!("object not found: {stem}");
    }

    fn mtime(path: &Path) -> std::time::SystemTime {
        fs::metadata(path).unwrap().modified().unwrap()
    }

    fn make_header_origin(dir: &Path, value: u32, func: &str) {
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("include")).unwrap();
        fs::write(
            dir.join("include/config.h"),
            format!("#pragma once\n#define VALUE {value}\n"),
        )
        .unwrap();
        fs::write(
            dir.join("src/dep.cpp"),
            format!("#include \"config.h\"\nint {func}() {{ return VALUE; }}\n"),
        )
        .unwrap();
        git_cmd(dir, &["init".into()]).unwrap();
        git_cmd(dir, &["add".into(), "-A".into()]).unwrap();
        git_cmd(
            dir,
            &[
                "-c".into(),
                "user.name=t".into(),
                "-c".into(),
                "user.email=t@t".into(),
                "commit".into(),
                "-m".into(),
                "init".into(),
                "--no-gpg-sign".into(),
            ],
        )
        .unwrap();
    }

    #[test]
    fn project_flag_change_does_not_rebuild_dependencies() {
        if !has_git() || crate::compiler::pick(None).is_err() {
            return;
        }
        let origin = temp("flag-origin");
        let proj = temp("flag-proj");
        make_origin(&origin, true);
        fs::create_dir_all(proj.join("src")).unwrap();
        fs::write(proj.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();
        fs::write(
            proj.join("src/main.cpp"),
            "#include \"dep.h\"\nint main() { return depv() - 42; }\n",
        )
        .unwrap();
        add_in(&proj, &add_args(&origin)).unwrap();
        let name = origin.file_name().unwrap().to_string_lossy().into_owned();
        let cfg = config::load(&proj).unwrap();
        let cxx = crate::compiler::pick(None).unwrap();
        let plan = crate::pipeline::plan(&proj, &cfg, cxx.clone(), false).unwrap();
        crate::pipeline::build(&proj, &cfg, &plan).unwrap();

        let dep_obj = find_obj(&proj, Some(&name), "dep");
        let main_obj = find_obj(&proj, None, "main");
        let (dep_t, main_t) = (mtime(&dep_obj), mtime(&main_obj));
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(
            proj.join("cizon.toml"),
            format!(
                "[project]\nname = \"t\"\n\n[compiler]\nflags = [\"-DEXTRA=1\"]\n\n[dependencies]\n{name} = {{ git = \"{}\" }}\n",
                esc(&origin.to_string_lossy())
            ),
        )
        .unwrap();
        let cfg = config::load(&proj).unwrap();
        let plan = crate::pipeline::plan(&proj, &cfg, cxx, false).unwrap();
        crate::pipeline::build(&proj, &cfg, &plan).unwrap();
        assert_eq!(mtime(&dep_obj), dep_t, "a user flags change should not recompile dependency sources");
        assert_ne!(mtime(&main_obj), main_t, "project sources should recompile with the new flags");
        fs::remove_dir_all(origin).ok();
        fs::remove_dir_all(proj).ok();
    }

    #[test]
    fn sync_restores_missing_dep_from_lock_without_building() {
        if !has_git() {
            return;
        }
        let origin = temp("sync-origin");
        let proj = temp("sync-proj");
        make_origin(&origin, false);
        fs::create_dir_all(proj.join("src")).unwrap();
        fs::write(proj.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();
        fs::write(proj.join("src/main.cpp"), "int main(){return 0;}").unwrap();
        add_in(&proj, &add_args(&origin)).unwrap();
        let name = origin.file_name().unwrap().to_string_lossy().into_owned();
        let dep_dir = proj.join("deps").join(&name);
        assert!(dep_dir.exists());

        // Remove the checkout; sync must restore it from the lock without compiling anything.
        fs::remove_dir_all(&dep_dir).unwrap();
        let cfg = config::load(&proj).unwrap();
        assert_eq!(sync(&proj, &cfg).unwrap(), 1);
        assert!(dep_dir.join("include/dep.h").exists());
        assert!(!proj.join("build").exists(), "sync must not build");
        // A second sync just verifies the restored checkout.
        assert_eq!(sync(&proj, &cfg).unwrap(), 1);

        fs::remove_dir_all(origin).ok();
        fs::remove_dir_all(proj).ok();
    }

    #[test]
    fn git_verb_skips_options() {
        assert_eq!(
            git_verb(&["-C".into(), "deps/x".into(), "rev-parse".into(), "HEAD".into()]),
            "rev-parse"
        );
        assert_eq!(git_verb(&["clone".into(), "url".into()]), "clone");
        assert_eq!(
            git_verb(&[
                "-c".into(),
                "user.name=t".into(),
                "commit".into(),
                "-m".into(),
                "x".into()
            ]),
            "commit"
        );
    }

    #[test]
    fn dep_sources_use_their_own_headers() {
        if !has_git() || crate::compiler::pick(None).is_err() {
            return;
        }
        let origin_a = temp("inc-a");
        let origin_b = temp("inc-b");
        let proj = temp("inc-proj");
        make_header_origin(&origin_a, 1, "depa");
        make_header_origin(&origin_b, 2, "depb");
        fs::create_dir_all(proj.join("src")).unwrap();
        fs::write(proj.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();
        fs::write(
            proj.join("src/main.cpp"),
            "int depa(); int depb();\nint main(){ return (depa() == 1 && depb() == 2) ? 0 : 1; }\n",
        )
        .unwrap();
        add_in(&proj, &add_args(&origin_a)).unwrap();
        add_in(&proj, &add_args(&origin_b)).unwrap();

        let cfg = config::load(&proj).unwrap();
        let cxx = crate::compiler::pick(None).unwrap();
        let plan = crate::pipeline::plan(&proj, &cfg, cxx, false).unwrap();
        let exe = crate::pipeline::build(&proj, &cfg, &plan).unwrap();
        assert!(
            Command::new(&exe).status().unwrap().success(),
            "each dependency must see its own config.h first"
        );
        fs::remove_dir_all(origin_a).ok();
        fs::remove_dir_all(origin_b).ok();
        fs::remove_dir_all(proj).ok();
    }

    #[test]
    fn corrupt_lock_is_not_silently_reset() {
        let dir = temp("corrupt-lock");
        fs::create_dir_all(&dir).unwrap();
        let original = "not = [valid toml";
        fs::write(dir.join(CIZON_LOCK), original).unwrap();
        let err = write_lock_entry(&dir, "x", "https://example.com/x", None, "abc")
            .unwrap_err();
        assert!(err.contains("failed to parse"), "got: {err}");
        assert_eq!(fs::read_to_string(dir.join(CIZON_LOCK)).unwrap(), original);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn module_unit_detection_ignores_comment_header() {
        let dir = temp("module-unit");
        fs::create_dir_all(&dir).unwrap();
        let module = dir.join("fmt.cc");
        fs::write(&module, "// license\n// more\n\nmodule;\n").unwrap();
        assert!(is_module_unit(&module).unwrap());
        let block = dir.join("block.cc");
        fs::write(&block, "\u{feff}/* license */\nmodule;\n").unwrap();
        assert!(is_module_unit(&block).unwrap(), "BOM + block comment header should also be recognized");
        let regular = dir.join("main.cpp");
        fs::write(&regular, "#include <iostream>\nint main() {}\n").unwrap();
        assert!(!is_module_unit(&regular).unwrap());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn layout_warning_distinguishes_header_only_and_compiled() {
        let dir = temp("layout-warning");
        fs::create_dir_all(&dir).unwrap();
        // No include/ and no flat sources: header-only-or-external note, not a hard layout error.
        let note = layout_warning(&dir, "dep").expect("should hint");
        assert!(note.contains("header-only"), "got: {note}");
        assert!(note.contains("extern_links"), "got: {note}");
        // Flat sources present but no include/: genuine layout warning.
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/a.cpp"), "int a;").unwrap();
        let warning = layout_warning(&dir, "dep").expect("should warn");
        assert!(warning.contains("missing include/"), "got: {warning}");
        // include/ present: no hint.
        fs::create_dir_all(dir.join("include")).unwrap();
        assert!(layout_warning(&dir, "dep").is_none());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn dep_name_normalizes_and_validates() {
        assert_eq!(dep_name("https://example.com/x/fmt.git").unwrap(), "fmt");
        assert_eq!(dep_name("https://example.com/x/fmt/").unwrap(), "fmt");
        assert_eq!(dep_name(r"C:\repos\fmt").unwrap(), "fmt");
        assert!(dep_name("///").is_err());
        assert!(dep_name("https://example.com/a b").is_err());
        assert!(dep_name("https://example.com/..").is_err());
    }

    #[test]
    fn parse_args_validation() {
        assert!(parse_args(&[]).is_err());
        assert!(parse_args(&["u".into(), "--tag".into()]).is_err());
        assert!(parse_args(&["u".into(), "--nope".into()]).is_err());
        assert_eq!(
            parse_args(&["u".into(), "--tag".into(), "v1".into()]).unwrap(),
            ("u".into(), Some("v1".into()))
        );
    }

    #[test]
    fn commit_hash_validation() {
        assert!(is_commit_hash(&"a".repeat(40)));
        assert!(is_commit_hash(&"0".repeat(64)));
        assert!(!is_commit_hash("main"));
        assert!(!is_commit_hash(&"g".repeat(40)));
        assert!(!is_commit_hash(&"a".repeat(39)));
    }

    #[test]
    fn flat_sources_filters_case_insensitively() {
        let dir = temp("flat-sources");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("A.CPP"), "").unwrap();
        fs::write(dir.join("b.cc"), "").unwrap();
        fs::write(dir.join("notes.txt"), "").unwrap();
        let found = flat_sources(&dir).unwrap();
        let names: Vec<String> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["A.CPP", "b.cc"], "should collect only source files: {names:?}");
        assert!(flat_sources(&dir.join("missing")).unwrap().is_empty());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn append_entry_handles_commented_header() {
        let dir = temp("commented-header");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n[dependencies] # pinned\n",
        )
        .unwrap();
        append_entry(&dir, "fmt", "https://example.com/fmt", None).unwrap();
        let text = fs::read_to_string(dir.join("cizon.toml")).unwrap();
        assert_eq!(text.matches("[dependencies]").count(), 1);
        let cfg = config::load(&dir).unwrap();
        assert!(cfg.dependencies().is_some_and(|m| m.contains_key("fmt")));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn append_entry_handles_spaced_header() {
        for (i, header) in ["[ dependencies ]", "[ dependencies ] # c"].iter().enumerate() {
            let dir = temp(&format!("spaced-header-{i}"));
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("cizon.toml"),
                format!("[project]\nname = \"t\"\n\n{header}\n"),
            )
            .unwrap();
            append_entry(&dir, "fmt", "https://example.com/fmt", None).unwrap();
            let cfg = config::load(&dir).unwrap();
            assert!(
                cfg.dependencies().is_some_and(|m| m.contains_key("fmt")),
                "header={header:?}"
            );
            fs::remove_dir_all(dir).ok();
        }
    }

    #[test]
    fn append_entry_rejects_inline_dependencies_without_writing() {
        let dir = temp("inline-deps");
        fs::create_dir_all(&dir).unwrap();
        let original =
            "dependencies = { fmt = { git = \"x\" } }\n\n[project]\nname = \"t\"\n";
        fs::write(dir.join("cizon.toml"), original).unwrap();
        let err = append_entry(&dir, "fmt", "https://example.com/fmt", None).unwrap_err();
        assert!(err.contains("inline"), "got: {err}");
        assert_eq!(fs::read_to_string(dir.join("cizon.toml")).unwrap(), original);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn ignore_deps_dir_preserves_non_utf8_gitignore() {
        let dir = temp("gitignore-nonutf8");
        fs::create_dir_all(&dir).unwrap();
        let original = [0xFFu8, 0xFE, b'\n'];
        fs::write(dir.join(".gitignore"), original).unwrap();
        ignore_deps_dir(&dir);
        assert_eq!(fs::read(dir.join(".gitignore")).unwrap(), original);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn add_records_config_and_lock() {
        if !has_git() {
            return;
        }
        let origin = temp("origin");
        let proj = temp("proj");
        make_origin(&origin, false);
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();

        let args = add_args(&origin);
        add_in(&proj, &args).unwrap();

        let name = origin.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            config::load(&proj)
                .unwrap()
                .dependencies()
                .is_some_and(|m| m.contains_key(&name))
        );
        let lock: toml::Table = fs::read_to_string(proj.join(CIZON_LOCK))
            .unwrap()
            .parse()
            .unwrap();
        let expect = git_cmd(&origin, &["rev-parse".into(), "HEAD".into()]).unwrap();
        let entry = lock.get(&name).unwrap();
        assert_eq!(entry.get("commit").unwrap().as_str(), Some(expect.as_str()));
        assert_eq!(
            entry.get("git").unwrap().as_str(),
            Some(origin.to_string_lossy().as_ref())
        );
        assert_eq!(
            git_cmd(
                &proj.join("deps").join(&name),
                &["rev-parse".into(), "HEAD".into()]
            )
            .unwrap(),
            expect
        );
        let toml_text = fs::read_to_string(proj.join("cizon.toml")).unwrap();
        assert!(toml_text.contains("[dependencies]"));
        assert!(fs::read_to_string(proj.join(".gitignore"))
            .unwrap()
            .lines()
            .any(|l| l.trim() == "deps/"));
        fs::remove_dir_all(origin).ok();
        fs::remove_dir_all(proj).ok();
    }

    #[test]
    fn add_with_tag_records_tag() {
        if !has_git() {
            return;
        }
        let origin = temp("tag-origin");
        let proj = temp("tag-proj");
        make_origin(&origin, true);
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();

        let url = origin.to_string_lossy().into_owned();
        add_in(&proj, &[url.clone(), "--tag".into(), "v1".into()]).unwrap();

        let name = origin.file_name().unwrap().to_string_lossy().into_owned();
        let lock: toml::Table = fs::read_to_string(proj.join(CIZON_LOCK))
            .unwrap()
            .parse()
            .unwrap();
        let entry = lock.get(&name).unwrap();
        assert_eq!(entry.get("tag").unwrap().as_str(), Some("v1"));
        let expect = git_cmd(&origin, &["rev-parse".into(), "v1".into()]).unwrap();
        assert_eq!(entry.get("commit").unwrap().as_str(), Some(expect.as_str()));

        // tag restore path: after deleting deps, prepare should clone by tag and verify the commit
        fs::remove_dir_all(proj.join("deps")).unwrap();
        let cfg = config::load(&proj).unwrap();
        prepare(&proj, &cfg, &proj.join("obj")).unwrap();
        assert_eq!(
            git_cmd(
                &proj.join("deps").join(&name),
                &["rev-parse".into(), "HEAD".into()]
            )
            .unwrap(),
            expect
        );

        // tag moved: restore from the lock must reject it
        fs::write(origin.join("second.txt"), "later").unwrap();
        git_cmd(&origin, &["add".into(), "-A".into()]).unwrap();
        git_cmd(
            &origin,
            &[
                "-c".into(),
                "user.name=t".into(),
                "-c".into(),
                "user.email=t@t".into(),
                "commit".into(),
                "-m".into(),
                "second".into(),
                "--no-gpg-sign".into(),
            ],
        )
        .unwrap();
        git_cmd(&origin, &["tag".into(), "-f".into(), "v1".into()]).unwrap();
        fs::remove_dir_all(proj.join("deps")).unwrap();
        let err = prepare(&proj, &cfg, &proj.join("obj")).unwrap_err();
        assert!(err.contains("tag was moved"), "got: {err}");

        fs::remove_dir_all(origin).ok();
        fs::remove_dir_all(proj).ok();
    }

    #[test]
    fn update_clone_failure_preserves_old_checkout() {
        if !has_git() {
            return;
        }
        let origin = temp("upd-fail-origin");
        let proj = temp("upd-fail-proj");
        make_origin(&origin, true);
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();
        add_in(&proj, &add_args(&origin)).unwrap();
        let name = origin.file_name().unwrap().to_string_lossy().into_owned();
        let c0 = git_cmd(&origin, &["rev-parse".into(), "v1".into()]).unwrap();
        let dep_dir = proj.join("deps").join(&name);

        fs::write(
            proj.join("cizon.toml"),
            format!("[project]\nname = \"t\"\n\n[dependencies]\n{name} = {{ git = \"{}\", tag = \"missing\" }}\n", esc(&origin.to_string_lossy())),
        )
        .unwrap();
        assert!(update_in(&proj, &[name.clone()]).is_err());
        assert_eq!(
            git_cmd(&dep_dir, &["rev-parse".into(), "HEAD".into()]).unwrap(),
            c0,
            "the old checkout must be kept when the clone fails"
        );
        let lock: toml::Table = fs::read_to_string(proj.join(CIZON_LOCK))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(
            lock.get(&name).unwrap().get("commit").unwrap().as_str(),
            Some(c0.as_str())
        );
        fs::remove_dir_all(origin).ok();
        fs::remove_dir_all(proj).ok();
    }

    #[test]
    fn add_rejects_duplicate() {
        if !has_git() {
            return;
        }
        let origin = temp("dup-origin");
        let proj = temp("dup-proj");
        make_origin(&origin, true);
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();
        let args = add_args(&origin);
        add_in(&proj, &args).unwrap();
        let err = add_in(&proj, &args).unwrap_err();
        assert!(err.contains("already contains"), "got: {err}");
        fs::remove_dir_all(origin).ok();
        fs::remove_dir_all(proj).ok();
    }

    #[test]
    fn update_rejects_unknown_name() {
        let proj = temp("upd-unknown-proj");
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();
        assert!(
            update_in(&proj, &[String::from("nope")])
                .unwrap_err()
                .contains("is not in [dependencies]")
        );
        fs::remove_dir_all(proj).ok();
    }

    /// Acceptance test for symmetric upgrade/downgrade: the tag is only changed in
    /// cizon.toml, update pulls the lock+deps along.
    #[test]
    fn update_aligns_lock_with_toml_intent() {
        if !has_git() {
            return;
        }
        let origin = temp("upd-origin");
        let proj = temp("upd-proj");
        make_origin(&origin, true); // v1 → c0
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();

        let args = add_args(&origin);
        add_in(&proj, &args).unwrap();
        let name = origin.file_name().unwrap().to_string_lossy().into_owned();
        let c0 = git_cmd(&origin, &["rev-parse".into(), "v1".into()]).unwrap();

        // advance origin to c1 and tag v2 (the remote "new version")
        fs::write(origin.join("second.txt"), "later").unwrap();
        git_cmd(&origin, &["add".into(), "-A".into()]).unwrap();
        git_cmd(
            &origin,
            &[
                "-c".into(),
                "user.name=t".into(),
                "-c".into(),
                "user.email=t@t".into(),
                "commit".into(),
                "-m".into(),
                "second".into(),
                "--no-gpg-sign".into(),
            ],
        )
        .unwrap();
        git_cmd(&origin, &["tag".into(), "v2".into()]).unwrap();
        let c1 = git_cmd(&origin, &["rev-parse".into(), "HEAD".into()]).unwrap();

        // the user's only action: express the new intent in cizon.toml
        fs::write(
            proj.join("cizon.toml"),
            format!("[project]\nname = \"t\"\n\n[dependencies]\n{name} = {{ git = \"{}\", tag = \"v2\" }}\n", esc(&origin.to_string_lossy())),
        )
        .unwrap();

        update_in(&proj, &[name.clone()]).unwrap();

        let lock: toml::Table = fs::read_to_string(proj.join(CIZON_LOCK))
            .unwrap()
            .parse()
            .unwrap();
        let entry = lock.get(&name).unwrap();
        assert_eq!(entry.get("tag").unwrap().as_str(), Some("v2"));
        assert_eq!(entry.get("commit").unwrap().as_str(), Some(c1.as_str()));
        assert_eq!(
            git_cmd(
                &proj.join("deps").join(&name),
                &["rev-parse".into(), "HEAD".into()]
            )
            .unwrap(),
            c1
        );

        // symmetric downgrade: switch back to v1 → update pulls the lock and deps back to c0
        fs::write(
            proj.join("cizon.toml"),
            format!("[project]\nname = \"t\"\n\n[dependencies]\n{name} = {{ git = \"{}\", tag = \"v1\" }}\n", esc(&origin.to_string_lossy())),
        )
        .unwrap();
        update_in(&proj, &[name.clone()]).unwrap();
        let lock: toml::Table = fs::read_to_string(proj.join(CIZON_LOCK))
            .unwrap()
            .parse()
            .unwrap();
        let entry = lock.get(&name).unwrap();
        assert_eq!(entry.get("tag").unwrap().as_str(), Some("v1"));
        assert_eq!(entry.get("commit").unwrap().as_str(), Some(c0.as_str()));
        assert_eq!(
            git_cmd(
                &proj.join("deps").join(&name),
                &["rev-parse".into(), "HEAD".into()]
            )
            .unwrap(),
            c0
        );
        fs::remove_dir_all(origin).ok();
        fs::remove_dir_all(proj).ok();
    }

    #[test]
    fn build_compiles_dep_and_restores_from_lock() {
        if !has_git() {
            return;
        }
        if crate::compiler::pick(None).is_err() {
            return; // skip when no compiler is available (same skip strategy as pick_finds...)
        }
        let origin = temp("e2e-origin");
        let proj = temp("e2e-proj");
        make_origin(&origin, true);
        fs::create_dir_all(proj.join("src")).unwrap();
        fs::write(proj.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();
        fs::write(
            proj.join("src/main.cpp"),
            "#include \"dep.h\"\nint main() { return depv() - 42; }\n",
        )
        .unwrap();

        let args = add_args(&origin);
        add_in(&proj, &args).unwrap();
        let name = origin.file_name().unwrap().to_string_lossy().into_owned();
        let c0 = git_cmd(&origin, &["rev-parse".into(), "HEAD".into()]).unwrap();
        let cxx = crate::compiler::pick(None).unwrap();
        let cfg = config::load(&proj).unwrap();
        let plan = crate::pipeline::plan(&proj, &cfg, cxx.clone(), false).unwrap();
        let exe = crate::pipeline::build(&proj, &cfg, &plan).unwrap();
        assert!(exe.is_file(), "the first build should produce an exe");
        assert!(
            Command::new(&exe).status().unwrap().success(),
            "the program built from the dependency should run correctly"
        );

        // the dependency working tree is modified → the lock no longer represents the actual code, it must error
        fs::write(
            proj.join("deps").join(&name).join("src/dep.cpp"),
            "int depv() { return 0; }\n",
        )
        .unwrap();
        let cfg = config::load(&proj).unwrap();
        let plan = crate::pipeline::plan(&proj, &cfg, cxx.clone(), false).unwrap();
        let err = crate::pipeline::build(&proj, &cfg, &plan).unwrap_err();
        assert!(err.contains("uncommitted changes"), "got: {err}");

        // key contract: deps/ deleted → build automatically restores from the lock and recompiles
        fs::remove_dir_all(proj.join("deps")).unwrap();
        let cfg = config::load(&proj).unwrap();
        let plan = crate::pipeline::plan(&proj, &cfg, cxx, false).unwrap();
        crate::pipeline::build(&proj, &cfg, &plan).unwrap();
        assert_eq!(
            git_cmd(
                &proj.join("deps").join(&name),
                &["rev-parse".into(), "HEAD".into()]
            )
            .unwrap(),
            c0,
            "the restored HEAD must return to the commit in the lock"
        );
        fs::remove_dir_all(origin).ok();
        fs::remove_dir_all(proj).ok();
    }

    #[test]
    fn dependency_named_src_does_not_collide_with_project_src() {
        if !has_git() || crate::compiler::pick(None).is_err() {
            return;
        }
        let origin_parent = temp("dep-src-parent");
        let origin = origin_parent.join("src");
        let proj = temp("dep-src-proj");
        make_origin(&origin, false);
        fs::create_dir_all(proj.join("src")).unwrap();
        fs::write(proj.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();
        fs::write(
            proj.join("src/main.cpp"),
            "#include \"dep.h\"\nint main(){ return depv() - 42; }\n",
        )
        .unwrap();
        add_in(&proj, &add_args(&origin)).unwrap();

        let cfg = config::load(&proj).unwrap();
        let cxx = crate::compiler::pick(None).unwrap();
        let plan = crate::pipeline::plan(&proj, &cfg, cxx, false).unwrap();
        let exe = crate::pipeline::build(&proj, &cfg, &plan).unwrap();
        assert!(Command::new(&exe).status().unwrap().success());
        assert!(find_obj(&proj, Some("src"), "dep").is_file());
        assert!(find_obj(&proj, None, "main").is_file());
        fs::remove_dir_all(origin_parent).ok();
        fs::remove_dir_all(proj).ok();
    }

    #[test]
    fn prepare_missing_lock_suggests_update() {
        let proj = temp("no-lock-proj");
        fs::create_dir_all(&proj).unwrap();
        fs::write(
            proj.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n[dependencies]\nfoo = { git = \"x\" }\n",
        )
        .unwrap();
        let cfg = config::load(&proj).unwrap();
        let err = prepare(&proj, &cfg, &proj.join("obj")).unwrap_err();
        assert!(err.contains("update"), "got: {err}");
        fs::remove_dir_all(proj).ok();
    }

    #[test]
    fn prepare_rejects_non_hash_commit() {
        let proj = temp("bad-commit-proj");
        fs::create_dir_all(&proj).unwrap();
        fs::write(
            proj.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n[dependencies]\nfoo = { git = \"x\" }\n",
        )
        .unwrap();
        fs::write(
            proj.join(CIZON_LOCK),
            "foo = { git = \"x\", commit = \"main\" }\n",
        )
        .unwrap();
        let cfg = config::load(&proj).unwrap();
        let err = prepare(&proj, &cfg, &proj.join("obj")).unwrap_err();
        assert!(err.contains("40/64"), "got: {err}");
        fs::remove_dir_all(proj).ok();
    }

    #[test]
    fn prepare_rejects_dep_stem_collision() {
        if !has_git() {
            return;
        }
        let origin = temp("stem-origin");
        let proj = temp("stem-proj");
        fs::create_dir_all(origin.join("src")).unwrap();
        fs::write(origin.join("src/foo.cpp"), "int a();\n").unwrap();
        fs::write(origin.join("src/foo.cc"), "int b();\n").unwrap();
        git_cmd(&origin, &["init".into()]).unwrap();
        git_cmd(&origin, &["add".into(), "-A".into()]).unwrap();
        git_cmd(
            &origin,
            &[
                "-c".into(),
                "user.name=t".into(),
                "-c".into(),
                "user.email=t@t".into(),
                "commit".into(),
                "-m".into(),
                "init".into(),
                "--no-gpg-sign".into(),
            ],
        )
        .unwrap();
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();
        add_in(&proj, &add_args(&origin)).unwrap();

        let cfg = config::load(&proj).unwrap();
        let err = prepare(&proj, &cfg, &proj.join("obj")).unwrap_err();
        assert!(err.contains("conflict"), "got: {err}");
        fs::remove_dir_all(origin).ok();
        fs::remove_dir_all(proj).ok();
    }
}
