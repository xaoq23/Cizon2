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
    let mut owned: Vec<(PathBuf, Vec<String>)> = Vec::new();
    // Mirror the real compile flags: objects that may land in a Linux .so are compiled -fPIC.
    let with_pic = |flags: Vec<String>| -> Vec<String> {
        let mut flags = flags;
        if super::state::pic_for_shared(plan) {
            flags.push("-fPIC".to_string());
        }
        flags
    };
    for bin in &plan.bins {
        owned.extend(
            bin.sources
                .iter()
                .map(|s| (s.clone(), bin.compile_flags.clone())),
        );
    }
    // Test sources use their own flags (NDEBUG stripped) so IntelliSense matches what
    // `cizon test` compiles even though `build` never compiles them.
    for test in &plan.tests {
        owned.extend(
            test.sources
                .iter()
                .map(|s| (s.clone(), test.compile_flags.clone())),
        );
    }
    for lib in &plan.libs {
        let flags = with_pic(lib.compile_flags.clone());
        owned.extend(lib.sources.iter().map(|s| (s.clone(), flags.clone())));
    }
    for shared in &plan.shared {
        let flags = with_pic(shared.compile_flags.clone());
        owned.extend(shared.sources.iter().map(|s| (s.clone(), flags.clone())));
    }
    owned.extend(
        dep_jobs
            .iter()
            .map(|(name, src, _)| (src.clone(), with_pic(plan.dep_flags_for(name)))),
    );
    let entries: Vec<(&Path, &[String])> = owned
        .iter()
        .map(|(path, flags)| (path.as_path(), flags.as_slice()))
        .collect();
    let json = render_compile_db(dir, &plan.cxx.name, &entries);
    let path = dir.join("compile_commands.json");
    let unchanged = std::fs::read_to_string(&path).map_or(false, |old| old == json);
    if !unchanged {
        crate::atomic::write(&path, json.as_bytes())
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

/// Fixed build task used as `preLaunchTask` by generated debug configurations.
pub(super) fn render_tasks() -> String {
    "{\n  \"version\": \"2.0.0\",\n  \"tasks\": [\n    {\n      \"label\": \"cizon: build\",\n      \"type\": \"shell\",\n      \"command\": \"cizon\",\n      \"args\": [\"build\"],\n      \"options\": { \"cwd\": \"${workspaceFolder}\" },\n      \"problemMatcher\": [\"$gcc\"]\n    }\n  ]\n}\n".to_string()
}

/// Launch configurations for the project's bin targets. Test targets are intentionally
/// not included in this first version; MSVC / lldb are outside the current boundary.
pub(super) fn render_launch(entries: &[(String, String)], scope: &str, gdb: Option<&Path>) -> String {
    let mut configs = Vec::new();
    for (name, out_name) in entries {
        let program = format!("${{workspaceFolder}}/build/{scope}/{out_name}");
        let mut lines = vec![
            "      \"name\": \"cizon: debug (".to_string() + &json_escape(name) + ")\",",
            "      \"type\": \"cppdbg\",".to_string(),
            "      \"request\": \"launch\",".to_string(),
            "      \"program\": \"".to_string() + &json_escape(&program) + "\",",
            "      \"args\": [],".to_string(),
            "      \"stopAtEntry\": false,".to_string(),
            "      \"cwd\": \"${workspaceFolder}\",".to_string(),
            "      \"environment\": [],".to_string(),
            "      \"externalConsole\": false,".to_string(),
            "      \"MIMode\": \"gdb\",".to_string(),
        ];
        if let Some(gdb) = gdb {
            lines.push(format!(
                "      \"miDebuggerPath\": \"{}\",",
                json_escape(&gdb.display().to_string().replace('\\', "/"))
            ));
        }
        lines.push("      \"preLaunchTask\": \"cizon: build\"".to_string());
        configs.push(format!("    {{\n{}\n    }}", lines.join("\n")));
    }
    format!(
        "{{\n  \"version\": \"0.2.0\",\n  \"configurations\": [\n{}\n  ]\n}}\n",
        configs.join(",\n")
    )
}

/// Locate gdb: prefer the compiler's own bin directory (MSYS2 installs it there), then PATH.
pub(super) fn find_debugger(cxx_name: &str) -> Option<PathBuf> {
    if let Some(exe) = crate::compiler::executable_path(cxx_name) {
        if let Some(bin) = exe.parent() {
            let names = if cfg!(windows) { &["gdb.exe", "gdb"][..] } else { &["gdb"][..] };
            for name in names {
                let candidate = bin.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    crate::compiler::executable_path("gdb")
}

/// Create .vscode/launch.json and tasks.json only when missing. Existing user files are
/// never touched; the returned paths are the ones actually written this run.
pub(super) fn ensure_vscode_configs(
    dir: &Path,
    entries: &[(String, String)],
    scope: &str,
    gdb: Option<&Path>,
) -> Result<Vec<PathBuf>, String> {
    let vscode = dir.join(".vscode");
    std::fs::create_dir_all(&vscode)
        .map_err(|e| format!("Failed to create {}: {e}", rel_display(dir, &vscode)))?;
    let files = [("tasks.json", render_tasks()), ("launch.json", render_launch(entries, scope, gdb))];
    let mut written = Vec::new();
    for (name, content) in files {
        let path = vscode.join(name);
        match crate::atomic::write_new(&path, content.as_bytes()) {
            Ok(true) => written.push(path),
            Ok(false) => {}
            Err(e) => {
                // Roll back everything created in this run: a half-generated .vscode
                // directory would never be completed by later builds (create_new skips it).
                for created in &written {
                    let _ = std::fs::remove_file(created);
                }
                return Err(e);
            }
        }
    }
    Ok(written)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tasks_config_uses_cizon_build() {
        let text = render_tasks();
        assert!(text.contains("\"label\": \"cizon: build\""));
        assert!(text.contains("\"command\": \"cizon\""));
        assert!(text.contains("\"args\": [\"build\"]"));
        assert!(text.contains("\"problemMatcher\": [\"$gcc\"]"));
    }

    #[test]
    fn launch_config_has_one_configuration_per_bin() {
        let entries = [("app".to_string(), "app.exe".to_string()), ("tool".to_string(), "tool".to_string())];
        let gdb = Path::new("/opt/toolchain/bin/gdb");
        let text = render_launch(&entries, "debug-g++", Some(gdb));
        assert_eq!(text.matches("\"type\": \"cppdbg\"").count(), 2);
        assert!(text.contains("\"name\": \"cizon: debug (app)\""));
        assert!(text.contains("${workspaceFolder}/build/debug-g++/app.exe"));
        assert!(text.contains("${workspaceFolder}/build/debug-g++/tool"));
        assert!(text.contains("\"miDebuggerPath\": \"/opt/toolchain/bin/gdb\""));
        assert!(text.contains("\"preLaunchTask\": \"cizon: build\""));
        let without = render_launch(&entries, "debug-g++", None);
        assert!(!without.contains("miDebuggerPath"));
    }

    #[test]
    fn ensure_vscode_configs_never_overwrites_existing_files() {
        let dir = std::env::temp_dir().join(format!(
            "cizon-vscode-config-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        let vscode = dir.join(".vscode");
        std::fs::create_dir_all(&vscode).unwrap();
        let sentinel = "// user owned\n";
        std::fs::write(vscode.join("launch.json"), sentinel).unwrap();
        let entries = [("app".to_string(), "app".to_string())];
        let written = ensure_vscode_configs(&dir, &entries, "debug-g++", None).unwrap();
        assert_eq!(written.len(), 1, "only tasks.json should be created");
        assert_eq!(std::fs::read_to_string(vscode.join("launch.json")).unwrap(), sentinel);
        assert!(vscode.join("tasks.json").is_file());
        std::fs::remove_dir_all(dir).ok();
    }
}
