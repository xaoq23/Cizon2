use std::io::Write;
use std::path::{Path, PathBuf};

use super::output::rel_display;
use super::Plan;

/// Write compile_commands.json: bin/library sources use plan.flags, dependency sources use their
/// own dep_flags_for. Hand-written JSON; serde_json is not introduced (dependency whitelist).
pub(super) fn write_compile_db(
    dir: &Path,
    plan: &Plan,
    dep_jobs: &[(String, PathBuf, PathBuf)],
) -> Result<(), String> {
    let mut owned: Vec<(PathBuf, Vec<String>)> = plan
        .sources
        .iter()
        .map(|s| (s.clone(), plan.flags.clone()))
        .collect();
    for lib in &plan.libs {
        owned.extend(
            lib.sources
                .iter()
                .map(|s| (s.clone(), plan.flags.clone())),
        );
    }
    owned.extend(
        dep_jobs
            .iter()
            .map(|(name, src, _)| (src.clone(), plan.dep_flags_for(name))),
    );
    let entries: Vec<(&Path, &[String])> = owned
        .iter()
        .map(|(path, flags)| (path.as_path(), flags.as_slice()))
        .collect();
    let json = render_compile_db(dir, &plan.cxx.name, &entries);
    let path = dir.join("compile_commands.json");
    let unchanged = std::fs::read_to_string(&path).map_or(false, |old| old == json);
    if !unchanged {
        std::fs::write(&path, json)
            .map_err(|e| format!("Failed to write {}: {e}", rel_display(dir, &path)))?;
    }
    Ok(())
}

/// Get the C++ standard that actually takes effect: with repeated `-std=` the last one wins
/// (consistent with the compiler).
pub(super) fn cpp_standard(flags: &[String]) -> &str {
    flags
        .iter()
        .rev()
        .find_map(|f| f.strip_prefix("-std="))
        .unwrap_or("c++23")
}

/// The Microsoft C/C++ extension does not read compile_commands.json automatically; when
/// c_cpp_properties.json is missing, add one. Existing files are never overwritten, to avoid
/// breaking the user's own IntelliSense configuration.
pub(super) fn ensure_cpptools_config(dir: &Path, compiler: &str, standard: &str) -> Result<bool, String> {
    let vscode = dir.join(".vscode");
    let path = vscode.join("c_cpp_properties.json");
    if path.exists() {
        return Ok(false);
    }
    std::fs::create_dir_all(&vscode)
        .map_err(|e| format!("Failed to create {}: {e}", rel_display(dir, &vscode)))?;
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(e) => return Err(format!("Failed to write {}: {e}", rel_display(dir, &path))),
    };
    if let Err(e) = file.write_all(render_cpptools_config(compiler, standard).as_bytes()) {
        drop(file);
        let _ = std::fs::remove_file(&path);
        return Err(format!("Failed to write {}: {e}", rel_display(dir, &path)));
    }
    Ok(true)
}

pub(super) fn render_cpptools_config(compiler: &str, standard: &str) -> String {
    let compiler = compiler.replace('\\', "/");
    format!(
        "{{\n  \"version\": 4,\n  \"configurations\": [\n    {{\n      \"name\": \"{}\",\n      \"compilerPath\": \"{}\",\n      \"intelliSenseMode\": \"{}\",\n      \"includePath\": [\"${{workspaceFolder}}/include\"],\n      \"cppStandard\": \"{}\",\n      \"compileCommands\": \"${{workspaceFolder}}/compile_commands.json\"\n    }}\n  ]\n}}\n",
        cpptools_config_name(),
        json_escape(&compiler),
        intelli_sense_mode(&compiler),
        json_escape(standard)
    )
}

pub(super) fn cpptools_config_name() -> &'static str {
    if cfg!(windows) {
        "Win32"
    } else if cfg!(target_os = "macos") {
        "Mac"
    } else {
        "Linux"
    }
}

pub(super) fn intelli_sense_mode(compiler: &str) -> &'static str {
    let clang = compiler.to_ascii_lowercase().contains("clang");
    if cfg!(windows) {
        if clang { "windows-clang-x64" } else { "windows-gcc-x64" }
    } else if cfg!(target_os = "macos") {
        if clang { "macos-clang-x64" } else { "macos-gcc-x64" }
    } else if clang {
        "linux-clang-x64"
    } else {
        "linux-gcc-x64"
    }
}

pub(super) fn render_compile_db(dir: &Path, cxx: &str, entries: &[(&Path, &[String])]) -> String {
    let directory = json_escape(&dir.display().to_string());
    let mut out = String::from("[\n");
    for (i, (src, flags)) in entries.iter().enumerate() {
        if i > 0 {
            out.push_str(",\n");
        }
        let file = json_escape(&src.display().to_string());
        out.push_str("  {\n");
        out.push_str(&format!("    \"directory\": \"{directory}\",\n"));
        out.push_str(&format!("    \"file\": \"{file}\",\n"));
        out.push_str(&format!(
            "    \"arguments\": [\"{}\", \"-c\", \"{file}\"",
            json_escape(cxx)
        ));
        for f in *flags {
            out.push_str(&format!(", \"{}\"", json_escape(f)));
        }
        out.push_str("]\n  }");
    }
    out.push_str("\n]\n");
    out
}

pub(super) fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}
