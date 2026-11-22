//! `cizon new <directory>` / `cizon init [--kind ...]`: generate the project skeleton.
//!
//! The default template stays the zero-config implicit single-bin project. Library templates are
//! target-based because a library must declare a `[[target]]`; their public include root nests a
//! project-named directory so multiple libraries do not pollute each other's header names.

use std::path::{Path, PathBuf};

use crate::config;
use crate::pipeline::output::{style_out, DIM, GREEN};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProjectKind {
    Bin,
    StaticLib,
    SharedLib,
}

impl ProjectKind {
    const OPTIONS: &'static str = "bin|static_lib|shared_lib";

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "bin" => Ok(Self::Bin),
            "static_lib" => Ok(Self::StaticLib),
            "shared_lib" => Ok(Self::SharedLib),
            other => Err(format!(
                "unknown --kind {other}; expected one of {}. `test` is not a project template.",
                Self::OPTIONS
            )),
        }
    }

    fn target_kind(self) -> &'static str {
        match self {
            Self::Bin => "bin",
            Self::StaticLib => "static_lib",
            Self::SharedLib => "shared_lib",
        }
    }

    fn is_library(self) -> bool {
        !matches!(self, Self::Bin)
    }
}

const TOML_TEMPLATE: &str = r#"[project]
name = "{name}"
edition = "c++23"
# compile_commands = true     # generate the compilation database; add the VS Code IntelliSense configuration when missing (never overwrite an existing one)

# [compiler]
# flags = ["-Wall"]        # extra compile flags
# link_flags = []          # extra link flags
"#;

const LIB_TOML_TEMPLATE: &str = r#"[project]
name = "{name}"
edition = "c++23"

[[target]]
name = "{name}"
kind = "{kind}"
sources = ["src"]
include = "include"

[[target]]
name = "{name}_test"
kind = "test"
sources = ["tests"]
links = ["{name}"]
"#;

const MAIN_TEMPLATE: &str = r#"#include <iostream>

int main() {
    std::cout << "Hello, World!" << '\n';
}
"#;

const STATIC_HEADER_TEMPLATE: &str = r#"#pragma once
int add(int a, int b);
"#;

const STATIC_SOURCE_TEMPLATE: &str = r#"#include "{name}/{name}.hpp"

int add(int a, int b) { return a + b; }
"#;

const SHARED_HEADER_TEMPLATE: &str = r#"#pragma once

extern "C" int add(int a, int b);
"#;

const SHARED_SOURCE_TEMPLATE: &str = r#"#include "{name}/{name}.hpp"

#if defined(_WIN32)
#define CIZON_EXPORT __declspec(dllexport)
#else
#define CIZON_EXPORT __attribute__((visibility("default")))
#endif

extern "C" CIZON_EXPORT int add(int a, int b) { return a + b; }
"#;

const TEST_TEMPLATE: &str = r#"#include <cassert>
#include "{name}/{name}.hpp"

int main() {
    assert(add(2, 3) == 5);
    return 0;
}
"#;

const GITIGNORE_TEMPLATE: &str =
    "build/\ndeps/\ncompile_commands.json\n.vscode/c_cpp_properties.json\n";

fn new_usage() -> String {
    format!(
        "Usage: cizon new <directory name> [--kind {}]",
        ProjectKind::OPTIONS
    )
}

fn init_usage() -> String {
    format!(
        "Usage: cizon init [--kind {}]",
        ProjectKind::OPTIONS
    )
}

/// `cizon new <directory name> [--kind ...]`: create a new project in the current directory.
pub fn new_cmd(args: &[String]) -> Result<i32, String> {
    let mut kind = ProjectKind::Bin;
    let mut name: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--kind" {
            let value = args
                .get(i + 1)
                .ok_or_else(|| format!("--kind requires a value. {}", new_usage()))?;
            kind = ProjectKind::parse(value)?;
            i += 2;
        } else if let Some(value) = arg.strip_prefix("--kind=") {
            kind = ProjectKind::parse(value)?;
            i += 1;
        } else if arg.starts_with('-') {
            return Err(new_usage());
        } else {
            if name.is_some() {
                return Err(new_usage());
            }
            name = Some(arg.clone());
            i += 1;
        }
    }
    let name = name.ok_or_else(new_usage)?;
    if name.is_empty() {
        return Err(new_usage());
    }
    let dir = Path::new(&name);
    validate_new_path(dir)?;
    let project = project_name(dir, &name);
    config::validate_project_name(&project)?;
    match std::fs::create_dir(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(format!(
                "the directory {name} already exists. Pick another name, or cd into it and use `cizon init`."
            ));
        }
        Err(e) => return Err(format!("cannot create the directory {name}: {e}")),
    }
    if let Err(e) = scaffold(dir, &project, kind) {
        let _ = std::fs::remove_dir_all(dir);
        return Err(e);
    }
    print_summary(&project, kind);
    print_next(Some(&name), kind);
    Ok(0)
}

/// `cizon init [--kind ...]`: initialize the current directory in place.
pub fn init_cmd(args: &[String]) -> Result<i32, String> {
    let mut kind = ProjectKind::Bin;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--kind" {
            let value = args
                .get(i + 1)
                .ok_or_else(|| format!("--kind requires a value. {}", init_usage()))?;
            kind = ProjectKind::parse(value)?;
            i += 2;
        } else if let Some(value) = arg.strip_prefix("--kind=") {
            kind = ProjectKind::parse(value)?;
            i += 1;
        } else {
            return Err(init_usage());
        }
    }
    let dir = std::env::current_dir().map_err(|e| format!("cannot read the current directory: {e}"))?;
    init_with_kind(&dir, kind)
}

fn init_with_kind(dir: &Path, kind: ProjectKind) -> Result<i32, String> {
    if dir.join("cizon.toml").exists() {
        return Err("the current directory already has a cizon.toml; not initializing twice.".into());
    }
    // Extra safety net compared to Cizon1: never overwrite a file the user already wrote.
    if let Some(existing) = existing_main_source(dir)? {
        return Err(format!(
            "{} already exists, refusing to overwrite it. Delete or move it and try again.",
            existing.display()
        ));
    }
    let project = project_name(dir, "project");
    config::validate_project_name(&project)?;
    scaffold(dir, &project, kind)?;
    print_summary(&project, kind);
    print_next(None, kind);
    Ok(0)
}

/// new accepts a relative or an absolute path, but no . or .. components, so that failure
/// cleanup only ever removes the leaf directory this run created.
fn validate_new_path(path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty() {
        return Err("Usage: cizon new <directory name>".into());
    }
    for component in path.components() {
        if matches!(
            component,
            std::path::Component::CurDir | std::path::Component::ParentDir
        ) {
            return Err(format!(
                "the directory name {} must not contain . or .. components.",
                path.display()
            ));
        }
    }
    Ok(())
}

/// Whether src/ already holds a C++ source named main (any case, any extension, any subdirectory).
fn existing_main_source(dir: &Path) -> Result<Option<PathBuf>, String> {
    fn walk(dir: &Path) -> Result<Option<PathBuf>, String> {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("cannot read {}: {e}", dir.display())),
        };
        for entry in entries {
            let entry = entry.map_err(|e| format!("cannot read the entries of {}: {e}", dir.display()))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|e| format!("cannot read the type of {}: {e}", path.display()))?;
            if file_type.is_dir() {
                if let Some(found) = walk(&path)? {
                    return Ok(Some(found));
                }
                continue;
            }
            let source = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| ["cpp", "cc", "cxx"].iter().any(|x| x.eq_ignore_ascii_case(e)));
            let main = path
                .file_stem()
                .is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case("main"));
            if source && main {
                return Ok(Some(path));
            }
        }
        Ok(None)
    }
    walk(&dir.join("src"))
}

/// Project name: the last component of the path; the fallback is the whole name for new and
/// "project" for init.
fn project_name(dir: &Path, fallback: &str) -> String {
    dir.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| fallback.into())
}

/// Write the template files; create_new prevents another process from winning the race between
/// the check and the write. On failure, remove the files this run created.
fn scaffold(dir: &Path, name: &str, kind: ProjectKind) -> Result<(), String> {
    let mut created: Vec<PathBuf> = Vec::new();
    let result = (|| -> Result<(), String> {
        match kind {
            ProjectKind::Bin => {
                let src = dir.join("src");
                std::fs::create_dir_all(&src)
                    .map_err(|e| format!("cannot create {}: {e}", src.display()))?;
                let main_cpp = src.join("main.cpp");
                write_new(&main_cpp, MAIN_TEMPLATE)?;
                created.push(main_cpp);
                let toml = dir.join("cizon.toml");
                write_new(&toml, &TOML_TEMPLATE.replace("{name}", name))?;
                created.push(toml);
            }
            ProjectKind::StaticLib | ProjectKind::SharedLib => {
                let include = dir.join("include").join(name);
                let src = dir.join("src");
                let tests = dir.join("tests");
                for path in [&include, &src, &tests] {
                    std::fs::create_dir_all(path)
                        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
                }
                let toml = dir.join("cizon.toml");
                let toml_text = LIB_TOML_TEMPLATE
                    .replace("{name}", name)
                    .replace("{kind}", kind.target_kind());
                write_new(&toml, &toml_text)?;
                created.push(toml);

                let header = include.join(format!("{name}.hpp"));
                let header_text = match kind {
                    ProjectKind::StaticLib => STATIC_HEADER_TEMPLATE,
                    ProjectKind::SharedLib => SHARED_HEADER_TEMPLATE,
                    ProjectKind::Bin => unreachable!("handled above"),
                };
                write_new(&header, header_text)?;
                created.push(header);

                let source = src.join(format!("{name}.cpp"));
                let source_text = match kind {
                    ProjectKind::StaticLib => STATIC_SOURCE_TEMPLATE,
                    ProjectKind::SharedLib => SHARED_SOURCE_TEMPLATE,
                    ProjectKind::Bin => unreachable!("handled above"),
                }
                .replace("{name}", name);
                write_new(&source, &source_text)?;
                created.push(source);

                let test = tests.join(format!("{name}_test.cpp"));
                write_new(&test, &TEST_TEMPLATE.replace("{name}", name))?;
                created.push(test);
            }
        }
        ensure_gitignore(dir)
    })();
    if result.is_err() {
        for path in created.iter().rev() {
            let _ = std::fs::remove_file(path);
        }
    }
    result
}

fn write_new(path: &Path, content: &str) -> Result<(), String> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                format!("{} already exists, refusing to overwrite it. Delete or move it and try again.", path.display())
            } else {
                format!("failed to write {}: {e}", path.display())
            }
        })?;
    if let Err(e) = file.write_all(content.as_bytes()) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(format!("failed to write {}: {e}", path.display()));
    }
    Ok(())
}

/// init may meet a .gitignore the user already has: only append the missing entries, never
/// overwrite.
fn ensure_gitignore(dir: &Path) -> Result<(), String> {
    let path = dir.join(".gitignore");
    let mut text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("failed to read {}: {e}", path.display())),
    };
    let mut changed = false;
    for entry in GITIGNORE_TEMPLATE.lines() {
        if text.lines().any(|l| l.trim() == entry) {
            continue;
        }
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(entry);
        text.push('\n');
        changed = true;
    }
    if !changed {
        return Ok(());
    }
    std::fs::write(&path, text).map_err(|e| format!("failed to write {}: {e}", path.display()))
}

fn print_summary(project: &str, kind: ProjectKind) {
    println!(
        "{} the project {project} is ready",
        style_out("      Ready", GREEN)
    );
    println!(
        "{}",
        style_out("   cizon.toml   → project configuration", DIM)
    );
    match kind {
        ProjectKind::Bin => println!(
            "{}",
            style_out("   src/main.cpp → program entry point (start writing here)", DIM)
        ),
        ProjectKind::StaticLib | ProjectKind::SharedLib => {
            let interface = if kind == ProjectKind::SharedLib {
                "public header (C ABI)"
            } else {
                "public header"
            };
            println!(
                "{}",
                style_out(
                    &format!("   include/{project}/{project}.hpp → {interface}"),
                    DIM
                )
            );
            println!(
                "{}",
                style_out(
                    &format!("   src/{project}.cpp → library implementation"),
                    DIM
                )
            );
            println!(
                "{}",
                style_out(
                    &format!("   tests/{project}_test.cpp → native-assert test"),
                    DIM
                )
            );
        }
    }
}

fn print_next(created: Option<&str>, kind: ProjectKind) {
    let prefix = created
        .map(|name| format!("cd \"{name}\" && "))
        .unwrap_or_default();
    let command = if kind.is_library() {
        "cizon build && cizon test"
    } else {
        "cizon build"
    };
    println!("{} {prefix}{command}", style_out("      Next", GREEN));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler;
    use crate::config::TargetKind;
    use crate::pipeline;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    fn temp(tag: &str) -> String {
        std::env::temp_dir()
            .join(format!("cizon-scaffold-{tag}-{}", std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos()))
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn new_scaffolds_and_template_parses() {
        let name = temp("new");
        let args = [name.clone()];
        assert_eq!(new_cmd(&args).unwrap(), 0);
        let dir = PathBuf::from(&name);
        let toml_text = fs::read_to_string(dir.join("cizon.toml")).unwrap();
        assert!(
            toml_text.contains(&format!(
                "name = \"{}\"",
                dir.file_name().unwrap().to_string_lossy()
            )),
            "got: {toml_text}"
        );
        let main = fs::read_to_string(dir.join("src/main.cpp")).unwrap();
        assert!(main.contains("#include <iostream>"));
        assert!(main.contains("int main()"));
        assert!(main.contains("Hello, World!"));
        let ignore = fs::read_to_string(dir.join(".gitignore")).unwrap();
        for entry in GITIGNORE_TEMPLATE.lines() {
            assert!(
                ignore.lines().any(|l| l == entry),
                ".gitignore is missing {entry}: {ignore}"
            );
        }
        // The template must be accepted by the parser as-is: shipped template and parser are
        // locked together.
        crate::config::load(&dir).unwrap();
        fs::remove_dir_all(dir).ok();
    }

    fn assert_library_template(dir: &Path, kind: TargetKind) {
        let cfg = config::load(dir).unwrap();
        let targets = cfg.targets().expect("library template must use targets");
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].kind, kind);
        assert_eq!(targets[0].include.as_deref(), Some("include"));
        assert_eq!(targets[1].kind, TargetKind::Test);

        let name = dir.file_name().unwrap().to_string_lossy();
        for rel in [
            format!("include/{name}/{name}.hpp"),
            format!("src/{name}.cpp"),
            format!("tests/{name}_test.cpp"),
        ] {
            assert!(dir.join(&rel).is_file(), "missing {rel}");
        }

        if compiler::pick(None).is_err() {
            return;
        }
        let cxx = compiler::pick(None).unwrap();
        let p = pipeline::plan(dir, &cfg, cxx, false).unwrap();
        pipeline::build_bins(dir, &cfg, &p).unwrap();
        let result = pipeline::build_tests(dir, &cfg, &p, &[]).unwrap();
        let status = Command::new(&result.execs[0].1).status().unwrap();
        assert!(status.success(), "generated library test must pass");
    }

    #[test]
    fn new_static_lib_template_parses_and_builds() {
        let name = temp("static");
        assert_eq!(
            new_cmd(&[name.clone(), "--kind".into(), "static_lib".into()]).unwrap(),
            0
        );
        let dir = PathBuf::from(&name);
        assert_library_template(&dir, TargetKind::StaticLib);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn new_shared_lib_template_parses_and_builds() {
        let name = temp("shared");
        assert_eq!(
            new_cmd(&[format!("--kind=shared_lib"), name.clone()]).unwrap(),
            0
        );
        let dir = PathBuf::from(&name);
        assert_library_template(&dir, TargetKind::SharedLib);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn init_supports_library_kinds() {
        let dir = temp("init-static");
        let p = PathBuf::from(&dir);
        init_with_kind(&p, ProjectKind::StaticLib).unwrap();
        assert_library_template(&p, TargetKind::StaticLib);
        fs::remove_dir_all(p).ok();
    }

    #[test]
    fn new_and_init_validate_kind_arguments() {
        assert!(new_cmd(&["x".into(), "--kind".into(), "test".into()]).is_err());
        assert!(new_cmd(&["x".into(), "--kind".into()]).is_err());
        assert!(new_cmd(&["--kind=nope".into(), "x".into()]).is_err());
        assert!(init_cmd(&["--kind".into(), "test".into()]).is_err());
        assert!(init_cmd(&["--kind".into()]).is_err());
    }

    #[test]
    fn new_rejects_option_like_name() {
        let name = "-cizon-option-like-name".to_string();
        assert!(!Path::new(&name).exists());
        assert!(new_cmd(&[name.clone()]).is_err());
        assert!(!Path::new(&name).exists(), "a rejected name must not leave a directory behind");
    }

    #[test]
    fn new_rejects_path_traversal_and_invalid_project_name() {
        for name in ["a/../b", "..", "foo."] {
            let err = new_cmd(&[name.to_string()]).unwrap_err();
            assert!(
                err.contains("must not contain") || err.contains("file name"),
                "name={name} got: {err}"
            );
        }
    }

    #[test]
    fn init_rejects_existing_main_cc() {
        let dir = temp("clobber-cc");
        let p = PathBuf::from(&dir);
        fs::create_dir_all(p.join("src")).unwrap();
        fs::write(p.join("src/main.cc"), "// mine").unwrap();
        let err = init_with_kind(&p, ProjectKind::Bin).unwrap_err();
        assert!(err.contains("already exists"), "got: {err}");
        assert_eq!(fs::read_to_string(p.join("src/main.cc")).unwrap(), "// mine");
        fs::remove_dir_all(p).ok();
    }

    #[test]
    fn new_rejects_existing_dir() {
        let name = temp("dup");
        let args = [name.clone()];
        new_cmd(&args).unwrap();
        assert!(new_cmd(&args).is_err());
        fs::remove_dir_all(Path::new(&name)).ok();
    }

    #[test]
    fn init_in_fresh_dir_then_refuses_repeat() {
        let dir = temp("init");
        let p = PathBuf::from(&dir);
        init_with_kind(&p, ProjectKind::Bin).unwrap();
        assert!(p.join("cizon.toml").exists());
        assert!(p.join("src/main.cpp").exists());
        assert!(init_with_kind(&p, ProjectKind::Bin).is_err());
        fs::remove_dir_all(p).ok();
    }

    #[test]
    fn init_never_clobbers_existing_main() {
        let dir = temp("clobber");
        let p = PathBuf::from(&dir);
        fs::create_dir_all(p.join("src")).unwrap();
        fs::write(p.join("src/main.cpp"), "// mine").unwrap();
        assert!(
            init_with_kind(&p, ProjectKind::Bin).is_err(),
            "an existing main.cpp should be refused"
        );
        assert_eq!(fs::read_to_string(p.join("src/main.cpp")).unwrap(), "// mine");
        fs::remove_dir_all(p).ok();
    }

    #[test]
    fn init_rejects_nested_main() {
        let dir = temp("clobber-nested");
        let p = PathBuf::from(&dir);
        fs::create_dir_all(p.join("src/app")).unwrap();
        fs::write(p.join("src/app/main.cpp"), "// mine").unwrap();
        assert!(
            init_with_kind(&p, ProjectKind::Bin).is_err(),
            "the recursive scan should refuse a nested main too"
        );
        assert_eq!(fs::read_to_string(p.join("src/app/main.cpp")).unwrap(), "// mine");
        fs::remove_dir_all(p).ok();
    }

    #[test]
    fn init_preserves_existing_gitignore() {
        let dir = temp("gitignore");
        let p = PathBuf::from(&dir);
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join(".gitignore"), "/target\ncustom\n").unwrap();
        init_with_kind(&p, ProjectKind::Bin).unwrap();
        let text = fs::read_to_string(p.join(".gitignore")).unwrap();
        assert!(text.contains("/target"));
        assert!(text.contains("custom"));
        for entry in [
            "build/",
            "deps/",
            "compile_commands.json",
            ".vscode/c_cpp_properties.json",
        ] {
            assert_eq!(
                text.lines().filter(|l| *l == entry).count(),
                1,
                "{entry} should appear exactly once: {text}"
            );
        }
        fs::remove_dir_all(p).ok();
    }
}
