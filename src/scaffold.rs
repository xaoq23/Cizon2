//! `cizon2 new <目录名>` 与 `cizon2 init`：生成项目骨架。
//!
//! 模板极简（AGENTS.md v0.2）：cizon.toml + src/main.cpp + .gitignore，不做 git init。
//! 模板里不放任何 Cizon2 不解析的段：注释的 `[dependencies]` 一旦被用户启用会
//! 当场报错（config 边界检查），shipped 模板不允许自己埋雷。

use std::path::{Path, PathBuf};

use crate::config;

const TOML_TEMPLATE: &str = r#"[project]
name = "{name}"
edition = "c++23"
# compile_commands = true     # 生成编译数据库；缺失时补 VS Code IntelliSense 配置（不覆盖）

# [compiler]
# flags = ["-Wall"]        # 附加编译参数
# link_flags = []          # 附加链接参数
"#;

const MAIN_TEMPLATE: &str = r#"#include <iostream>

int main() {
    std::cout << "Hello, World!" << '\n';
}
"#;

const GITIGNORE_TEMPLATE: &str =
    "build/\ndeps/\ncompile_commands.json\n.vscode/c_cpp_properties.json\n";

/// `cizon2 new <目录名>`：在当前目录下创建新项目。
pub fn new_cmd(args: &[String]) -> Result<i32, String> {
    let name = match args {
        [name] if !name.is_empty() => name,
        _ => return Err("用法: cizon2 new <目录名>".into()),
    };
    if name.starts_with('-') {
        return Err("用法: cizon2 new <目录名>（目录名不能以 - 开头）".into());
    }
    let dir = Path::new(name);
    validate_new_path(dir)?;
    let project = project_name(dir, name);
    config::validate_project_name(&project)?;
    match std::fs::create_dir(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(format!(
                "目录 {name} 已存在。换个名字，或进到目录里用 `cizon2 init`。"
            ));
        }
        Err(e) => return Err(format!("无法创建目录 {name}: {e}")),
    }
    if let Err(e) = scaffold(dir, &project) {
        let _ = std::fs::remove_dir_all(dir);
        return Err(e);
    }
    print_summary(&project);
    println!("\x1b[32m      Next\x1b[0m cd \"{name}\" && cizon2 build");
    Ok(0)
}

/// `cizon2 init`：在当前目录就地初始化；已有 cizon.toml 则拒绝。
pub fn init_cmd(args: &[String]) -> Result<i32, String> {
    if !args.is_empty() {
        return Err("用法: cizon2 init（不接受参数）".into());
    }
    let dir = std::env::current_dir().map_err(|e| format!("无法读取当前目录: {e}"))?;
    init_in(&dir)
}

pub fn init_in(dir: &Path) -> Result<i32, String> {
    if dir.join("cizon.toml").exists() {
        return Err("当前目录已有 cizon.toml，不重复初始化。".into());
    }
    // 比 Cizon1 多的保险：绝不覆盖用户已写的文件
    if let Some(existing) = existing_main_source(dir)? {
        return Err(format!(
            "已存在 {}，拒绝覆盖。删除或移走后重试。",
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

/// new 允许相对或绝对路径，但不能包含 . / .. 组件，
/// 保证失败清理只会删除本次创建的叶子目录。
fn validate_new_path(path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty() {
        return Err("用法: cizon2 new <目录名>".into());
    }
    for component in path.components() {
        if matches!(
            component,
            std::path::Component::CurDir | std::path::Component::ParentDir
        ) {
            return Err(format!(
                "目录名 {} 不能包含 . 或 .. 组件。",
                path.display()
            ));
        }
    }
    Ok(())
}

/// src/ 下是否已有名为 main 的 C++ 源（任意大小写/扩展名/子目录）。
fn existing_main_source(dir: &Path) -> Result<Option<PathBuf>, String> {
    fn walk(dir: &Path) -> Result<Option<PathBuf>, String> {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("无法读取 {}: {e}", dir.display())),
        };
        for entry in entries {
            let entry = entry.map_err(|e| format!("无法读取 {} 的目录项: {e}", dir.display()))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|e| format!("无法读取 {} 的类型: {e}", path.display()))?;
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

/// 项目名：路径最后一段；new 时兜底为整个 name，init 兜底 "project"。
fn project_name(dir: &Path, fallback: &str) -> String {
    dir.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| fallback.into())
}

/// 写入模板文件；create_new 防止 check-then-write 之间被其他进程抢占。
fn scaffold(dir: &Path, name: &str) -> Result<(), String> {
    let src = dir.join("src");
    std::fs::create_dir_all(&src).map_err(|e| format!("无法创建 {}: {e}", src.display()))?;
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
                format!("已存在 {}，拒绝覆盖。删除或移走后重试。", path.display())
            } else {
                format!("写入 {} 失败: {e}", path.display())
            }
        })?;
    if let Err(e) = file.write_all(content.as_bytes()) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(format!("写入 {} 失败: {e}", path.display()));
    }
    Ok(())
}

/// init 可能遇到用户已有的 .gitignore：只补缺失条目，绝不覆盖。
fn ensure_gitignore(dir: &Path) -> Result<(), String> {
    let path = dir.join(".gitignore");
    let mut text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("读 {} 失败: {e}", path.display())),
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
    std::fs::write(&path, text).map_err(|e| format!("写入 {} 失败: {e}", path.display()))
}

fn print_summary(project: &str) {
    println!("\x1b[32m      Ready\x1b[0m 项目 {project} 已就绪");
    println!("\x1b[2m   cizon.toml   → 项目配置");
    println!("   src/main.cpp → 程序入口（从这里开始写）\x1b[0m");
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
                ".gitignore 缺少 {entry}: {ignore}"
            );
        }
        // 模板必须能被解析器直接接受 —— shipped 模板与解析器互锁
        crate::config::load(&dir).unwrap();
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn new_rejects_option_like_name() {
        let name = "-cizon2-option-like-name".to_string();
        assert!(!Path::new(&name).exists());
        assert!(new_cmd(&[name.clone()]).is_err());
        assert!(!Path::new(&name).exists(), "拒绝后不应留下目录");
    }

    #[test]
    fn new_rejects_path_traversal_and_invalid_project_name() {
        for name in ["a/../b", "..", "foo."] {
            let err = new_cmd(&[name.to_string()]).unwrap_err();
            assert!(
                err.contains("不能") || err.contains("必须是相对路径"),
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
        assert!(err.contains("已存在"), "got: {err}");
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
        assert!(init_in(&p).is_err(), "已有 main.cpp 应拒绝");
        assert_eq!(fs::read_to_string(p.join("src/main.cpp")).unwrap(), "// mine");
        fs::remove_dir_all(p).ok();
    }

    #[test]
    fn init_rejects_nested_main() {
        let dir = temp("clobber-nested");
        let p = PathBuf::from(&dir);
        fs::create_dir_all(p.join("src/app")).unwrap();
        fs::write(p.join("src/app/main.cpp"), "// mine").unwrap();
        assert!(init_in(&p).is_err(), "递归扫描下嵌套 main 也应拒绝");
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
                "{entry} 应只出现一次: {text}"
            );
        }
        fs::remove_dir_all(p).ok();
    }
}
