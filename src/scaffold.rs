//! `cizon2 new <directory name>` and `cizon2 init`: generate the project skeleton.
//!
//! The template is minimal (AGENTS.md v0.2): cizon.toml + src/main.cpp + .gitignore, no git init.
//! The template carries no section Cizon2 does not parse: a commented-out `[dependencies]` would
//! fail loudly the moment a user enables it (the config boundary check), and the shipped template
//! must not plant that trap itself.

use std::path::{Path, PathBuf};

use crate::config;

const TOML_TEMPLATE: &str = r#"[project]
name = "{name}"
edition = "c++23"
# compile_commands = true     # generate the compilation database; add the VS Code IntelliSense configuration when missing (never overwrite an existing one)

# [compiler]
# flags = ["-Wall"]        # extra compile flags
# link_flags = []          # extra link flags
"#;

const MAIN_TEMPLATE: &str = r#"#include <iostream>

int main() {
    std::cout << "Hello, World!" << '\n';
}
"#;

const GITIGNORE_TEMPLATE: &str =
    "build/\ndeps/\ncompile_commands.json\n.vscode/c_cpp_properties.json\n";

/// `cizon2 new <directory name>`: create a new project in the current directory.
pub fn new_cmd(args: &[String]) -> Result<i32, String> {
    let name = match args {
        [name] if !name.is_empty() => name,
        _ => return Err("Usage: cizon2 new <directory name>".into()),
    };
    if name.starts_with('-') {
        return Err("Usage: cizon2 new <directory name> (the directory name must not start with -)".into());
    }
    let dir = Path::new(name);
    validate_new_path(dir)?;
    let project = project_name(dir, name);
    config::validate_project_name(&project)?;
    match std::fs::create_dir(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(format!(
                "the directory {name} already exists. Pick another name, or cd into it and use `cizon2 init`."
            ));
        }
        Err(e) => return Err(format!("cannot create the directory {name}: {e}")),
    }
    if let Err(e) = scaffold(dir, &project) {
        let _ = std::fs::remove_dir_all(dir);
        return Err(e);
    }
    print_summary(&project);
    println!("\x1b[32m      Next\x1b[0m cd \"{name}\" && cizon2 build");
    Ok(0)
}

/// `cizon2 init`: initialize the current directory in place; refused when a cizon.toml exists.
pub fn init_cmd(args: &[String]) -> Result<i32, String> {
    if !args.is_empty() {
        return Err("Usage: cizon2 init (accepts no arguments)".into());
    }
    let dir = std::env::current_dir().map_err(|e| format!("cannot read the current directory: {e}"))?;
    init_in(&dir)
}

pub fn init_in(dir: &Path) -> Result<i32, String> {
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
    scaffold(dir, &project)?;
    print_summary(&project);
    println!("\x1b[32m      Next\x1b[0m cizon2 build");
    Ok(0)
}

/// new accepts a relative or an absolute path, but no . or .. components, so that failure
/// cleanup only ever removes the leaf directory this run created.
fn validate_new_path(path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty() {
        return Err("Usage: cizon2 new <directory name>".into());
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
/// the check and the write.
fn scaffold(dir: &Path, name: &str) -> Result<(), String> {
    let src = dir.join("src");
    std::fs::create_dir_all(&src).map_err(|e| format!("cannot create {}: {e}", src.display()))?;
    let main_cpp = src.join("main.cpp");
    write_new(&main_cpp, MAIN_TEMPLATE)?;
    let toml = dir.join("cizon.toml");
    if let Err(e) = write_new(&toml, &TOML_TEMPLATE.replace("{name}", name)) {
        let _ = std::fs::remove_file(&main_cpp);
        return Err(e);
    }
    if let Err(e) = ensure_gitignore(dir) {
        let _ = std::fs::remove_file(&toml);
        let _ = std::fs::remove_file(&main_cpp);
        return Err(e);
    }
    Ok(())
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

fn print_summary(project: &str) {
    println!("\x1b[32m      Ready\x1b[0m the project {project} is ready");
    println!("\x1b[2m   cizon.toml   → project configuration");
    println!("   src/main.cpp → program entry point (start writing here)\x1b[0m");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp(tag: &str) -> String {
        std::env::temp_dir()
            .join(format!("cizon2-scaffold-{tag}-{}", std::time::SystemTime::now()
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

    #[test]
    fn new_rejects_option_like_name() {
        let name = "-cizon2-option-like-name".to_string();
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
        let err = init_in(&p).unwrap_err();
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
        init_in(&p).unwrap();
        assert!(p.join("cizon.toml").exists());
        assert!(p.join("src/main.cpp").exists());
        assert!(init_in(&p).is_err());
        fs::remove_dir_all(p).ok();
    }

    #[test]
    fn init_never_clobbers_existing_main() {
        let dir = temp("clobber");
        let p = PathBuf::from(&dir);
        fs::create_dir_all(p.join("src")).unwrap();
        fs::write(p.join("src/main.cpp"), "// mine").unwrap();
        assert!(init_in(&p).is_err(), "an existing main.cpp should be refused");
        assert_eq!(fs::read_to_string(p.join("src/main.cpp")).unwrap(), "// mine");
        fs::remove_dir_all(p).ok();
    }

    #[test]
    fn init_rejects_nested_main() {
        let dir = temp("clobber-nested");
        let p = PathBuf::from(&dir);
        fs::create_dir_all(p.join("src/app")).unwrap();
        fs::write(p.join("src/app/main.cpp"), "// mine").unwrap();
        assert!(init_in(&p).is_err(), "the recursive scan should refuse a nested main too");
        assert_eq!(fs::read_to_string(p.join("src/app/main.cpp")).unwrap(), "// mine");
        fs::remove_dir_all(p).ok();
    }

    #[test]
    fn init_preserves_existing_gitignore() {
        let dir = temp("gitignore");
        let p = PathBuf::from(&dir);
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join(".gitignore"), "/target\ncustom\n").unwrap();
        init_in(&p).unwrap();
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
