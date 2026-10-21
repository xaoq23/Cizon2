//! `cizon2 new <目录名>` 与 `cizon2 init`：生成项目骨架。
//!
//! 模板极简（AGENTS.md v0.2）：cizon.toml + src/main.cpp + .gitignore，不做 git init。
//! 模板里不放任何 Cizon2 不解析的段：注释的 `[dependencies]` 一旦被用户启用会
//! 当场报错（config 边界检查），shipped 模板不允许自己埋雷。

use std::path::Path;

const TOML_TEMPLATE: &str = r#"[project]
name = "{name}"
edition = "c++23"

# [compiler]
# flags = ["-Wall"]        # 附加编译参数
# link_flags = []          # 附加链接参数
"#;

const MAIN_TEMPLATE: &str = r#"#include <iostream>

int main() {
    std::cout << "Hello, World!\n";
    return 0;
}
"#;

const GITIGNORE_TEMPLATE: &str = "build/\n";

/// `cizon2 new <目录名>`：在当前目录下创建新项目。
pub fn new_cmd(args: &[String]) -> Result<i32, String> {
    let name = match args {
        [name] if !name.is_empty() => name,
        _ => return Err("用法: cizon2 new <目录名>".into()),
    };
    let dir = Path::new(name);
    if dir.exists() {
        return Err(format!(
            "目录 {name} 已存在。换个名字，或进到目录里用 `cizon2 init`。"
        ));
    }
    let project = project_name(dir, name);
    scaffold(dir, &project)?;
    print_summary(&project);
    println!("\x1b[32m      Next\x1b[0m cd {name} && cizon2 build");
    Ok(0)
}

/// `cizon2 init`：在当前目录就地初始化；已有 cizon.toml 则拒绝。
pub fn init_cmd() -> Result<i32, String> {
    let dir = std::env::current_dir().map_err(|e| format!("无法读取当前目录: {e}"))?;
    init_in(&dir)
}

pub fn init_in(dir: &Path) -> Result<i32, String> {
    if dir.join("cizon.toml").exists() {
        return Err("当前目录已有 cizon.toml，不重复初始化。".into());
    }
    // 比 Cizon1 多的保险：绝不覆盖用户已写的文件
    let main_cpp = dir.join("src").join("main.cpp");
    if main_cpp.exists() {
        return Err(format!("已存在 {}，拒绝覆盖。删除或移走后重试。", main_cpp.display()));
    }
    let project = project_name(dir, "project");
    scaffold(dir, &project)?;
    print_summary(&project);
    println!("\x1b[32m      Next\x1b[0m cizon2 build");
    Ok(0)
}

/// 项目名：路径最后一段；new 时兜底为整个 name，init 兜底 "project"。
fn project_name(dir: &Path, fallback: &str) -> String {
    dir.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| fallback.into())
}

/// 写入模板文件（new 时的目录已确认不存在；init 时的冲突已在调用前检查）。
fn scaffold(dir: &Path, name: &str) -> Result<(), String> {
    let src = dir.join("src");
    std::fs::create_dir_all(&src).map_err(|e| format!("无法创建 {}: {e}", src.display()))?;
    let write = |path: &Path, content: &str| -> Result<(), String> {
        std::fs::write(path, content).map_err(|e| format!("写入 {} 失败: {e}", path.display()))
    };
    write(&dir.join("cizon.toml"), &TOML_TEMPLATE.replace("{name}", name))?;
    write(&dir.join("src").join("main.cpp"), MAIN_TEMPLATE)?;
    write(&dir.join(".gitignore"), GITIGNORE_TEMPLATE)?;
    Ok(())
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
        assert!(dir.join("src/main.cpp").exists());
        assert!(dir.join(".gitignore").exists());
        // 模板必须能被 v0.1 解析器直接接受 —— shipped 模板与解析器互锁
        crate::config::load(&dir).unwrap();
        fs::remove_dir_all(dir).ok();
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
}
