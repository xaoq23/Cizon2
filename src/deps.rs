//! `cizon2 add <git-url> [--tag <tag>]`：git 依赖的最小正确实现（v0.2 路线）。
//!
//! 设计边界：
//! - 承诺"常规布局"：`<dep>/include/` 头文件 + 平铺 `<dep>/src/*.cpp`，
//!   与 cizon2 自身项目同构；超出此形状的库不迁 —— 那是 CMake 的领地
//! - `cizon.lock` 是机器文件（每次整表重写，不保注释），锁定的 commit 是唯一真相；
//!   cizon.toml ↔ cizon.lock ↔ deps/ 任一不一致 → 大声报错，绝不静默自愈
//! - 头文件路径无条件写进 flags（`-I<dep>/include` 与 `-I<dep>`），
//!   目录不存在时编译器安全忽略，因此 planning 不需要先做网络操作

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config;

const CIZON_LOCK: &str = "cizon.lock";
const DEP_EXTS: &[&str] = &["cpp", "cc", "cxx"];

/// `cizon2 add <git-url> [--tag <tag>]`
pub fn add_cmd(args: &[String]) -> Result<i32, String> {
    let dir = std::env::current_dir().map_err(|e| format!("无法读取当前目录: {e}"))?;
    add_in(&dir, args)
}

pub fn add_in(dir: &Path, args: &[String]) -> Result<i32, String> {
    let (url, tag) = parse_args(args)?;
    let cfg = config::load(dir)?;
    let name = dep_name(&url)?;

    if cfg.dependencies().map_or(false, |m| m.contains_key(&name)) {
        return Err(format!("[dependencies] 中已有 {name}。删除该条目后再重试。"));
    }
    let dep_dir = dir.join("deps").join(&name);
    if dep_dir.exists() {
        return Err(format!(
            "deps/{name} 已存在但未被 cizon2 管理。确认无用后删除该目录，再重跑 add。"
        ));
    }

    let commit = shallow_clone(&dir, &url, tag.as_deref(), &name)?;
    append_entry(&dir, &name, &url, tag.as_deref())?;
    write_lock_entry(&dir, &name, &url, tag.as_deref(), &commit)?;
    ignore_deps_dir(&dir);

    println!("\x1b[32m      Added\x1b[0m {name} → deps/{name} (锁 commit {commit})");
    println!("\x1b[32m      Next\x1b[0m cizon2 build");
    Ok(0)
}

fn parse_args(args: &[String]) -> Result<(String, Option<String>), String> {
    let usage = "用法: cizon2 add <git-url> [--tag <tag>]";
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
                _ => return Err("--tag 需要一个参数。".into()),
            },
            other => return Err(format!("未知参数 {other}。{usage}")),
        }
    }
    Ok((url, tag))
}

/// 依赖名 = URL 去掉尾部 / 与 .git 后的最后一段。只允许字母数字、- 和 _
/// （同时满足 TOML bare key 与 Windows 目录名的交集）。
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
        return Err("无法从 URL 解析依赖名。".into());
    }
    let ok = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    if !name.chars().all(ok) {
        return Err(format!("依赖名 {name} 含非法字符（只允许字母数字、- 和 _）。"));
    }
    Ok(name)
}

/// 浅克隆（有 tag 则一并 --branch 定位），返回所在 commit。
fn shallow_clone(dir: &Path, url: &str, tag: Option<&str>, name: &str) -> Result<String, String> {
    let rel = format!("deps/{name}");
    let mut args: Vec<String> = ["clone", "--depth", "1"]
        .iter()
        .map(|s| (*s).into())
        .collect();
    if let Some(t) = tag {
        args.push("--branch".into());
        args.push(t.into());
    }
    args.push(url.into());
    args.push(rel.clone());
    git_cmd(dir, &args)?;
    git_cmd(dir, &["-C".into(), rel, "rev-parse".into(), "HEAD".into()])
}

/// 把 git 执行细节收敛在一处：cwd = 项目根，args 原样转发，stdout trim 返回。
fn git_cmd(dir: &Path, args: &[String]) -> Result<String, String> {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .map_err(|e| format!("启动 git 失败: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {} 失败: {}",
            args.first().map(String::as_str).unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// TOML 基本串转义（URL/tag 都走这里）。
fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn entry_line(name: &str, url: &str, tag: Option<&str>) -> String {
    match tag {
        Some(t) => format!("{name} = {{ git = \"{}\", tag = \"{}\" }}", esc(url), esc(t)),
        None => format!("{name} = {{ git = \"{}\" }}", esc(url)),
    }
}

/// `[dependencies]` 表头；允许行尾注释，但不会把 `[dependencies.foo]` 误认成表头。
fn is_dependencies_header(line: &str) -> bool {
    let t = line.trim();
    t == "[dependencies]"
        || t.strip_prefix("[dependencies]")
            .is_some_and(|rest| rest.trim_start().starts_with('#'))
}

/// 在 cizon.toml 里登记：有 [dependencies] 头就在其下一行插入；
/// 没有则整段追加到文件尾。
fn append_entry(dir: &Path, name: &str, url: &str, tag: Option<&str>) -> Result<(), String> {
    let path = dir.join("cizon.toml");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("读 cizon.toml 失败: {e}"))?;
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
    std::fs::write(&path, out).map_err(|e| format!("写 cizon.toml 失败: {e}"))
}

/// 锁文件是机器文件：读原表 → 插入/更新条目 → 整表重写。
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
            .map_err(|e| format!("{CIZON_LOCK} 解析失败（未改动文件）: {e}"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
        Err(e) => return Err(format!("读 {CIZON_LOCK} 失败: {e}")),
    };
    let mut entry = toml::Table::new();
    entry.insert("git".into(), url.into());
    if let Some(t) = tag {
        entry.insert("tag".into(), t.into());
    }
    entry.insert("commit".into(), commit.into());
    table.insert(name.into(), entry.into());
    let text = toml::to_string(&table).map_err(|e| format!("锁序列化失败: {e}"))?;
    std::fs::write(&lock_path, text).map_err(|e| format!("写 {CIZON_LOCK} 失败: {e}"))
}

/// 尽力把 deps/ 加进 .gitignore（没有就创建；已有 deps/ 行则不动）。
fn ignore_deps_dir(dir: &Path) {
    let path = dir.join(".gitignore");
    if let Ok(text) = std::fs::read_to_string(&path) {
        if text.lines().any(|l| l.trim() == "deps/") {
            return;
        }
        let mut text = text;
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str("deps/\n");
        let _ = std::fs::write(&path, text);
    } else {
        let _ = std::fs::write(&path, "deps/\n");
    }
}

// ─────────────────────────────────────────────────────────────
// 锁生命周期第三环:update —— 把锁与 deps 对齐 cizon.toml 的意图。
// 升级/降级/横切同一命令:tag 只改在 cizon.toml,update 负责落地;
// build 永远执行锁里已定的事,不追 toml 里的新愿望。
// ─────────────────────────────────────────────────────────────

/// `cizon2 update <依赖名>`：把锁与 deps 对齐 cizon.toml 的 tag 意图（升/降同构）。
pub fn update_cmd(args: &[String]) -> Result<i32, String> {
    let dir = std::env::current_dir().map_err(|e| format!("无法读取当前目录: {e}"))?;
    update_in(&dir, args)
}

pub fn update_in(dir: &Path, args: &[String]) -> Result<i32, String> {
    if args.len() != 1 || args[0].is_empty() {
        return Err("用法: cizon2 update <依赖名>".into());
    }
    let name = &args[0];
    let cfg = config::load(dir)?;
    let info = cfg
        .dependencies()
        .and_then(|m| m.get(name))
        .ok_or_else(|| format!("{name} 不在 [dependencies] 里。先 cizon2 add,或检查拼写。"))?;

    let dep_dir = dir.join("deps").join(name);
    if dep_dir.exists() {
        std::fs::remove_dir_all(&dep_dir).map_err(|e| format!("删 deps/{name} 失败: {e}"))?;
    }
    let commit = shallow_clone(dir, &info.git, info.tag.as_deref(), name)?;
    write_lock_entry(dir, name, &info.git, info.tag.as_deref(), &commit)?;
    println!("\x1b[32m   Updated\x1b[0m {name} @ {commit}");
    Ok(0)
}

// ─────────────────────────────────────────────────────────────
// build 期的依赖契约：restore / 校验 / 产出编译三元组
// ─────────────────────────────────────────────────────────────

/// build 调用：依赖目录满足锁则跳过，缺失则按锁恢复（含 tag 校验），
/// 成功后返回 (src, obj) 编译二元组；obj 放 obj_dir/<dep>/<stem>.o，
/// 与项目 obj 物理隔离，stem 永不冲突。
pub fn prepare(
    dir: &Path,
    cfg: &config::Config,
    obj_dir: &Path,
) -> Result<Vec<(PathBuf, PathBuf)>, String> {
    let Some(map) = cfg.dependencies() else { return Ok(vec![]) };
    if map.is_empty() {
        return Ok(vec![]);
    }
    let lock_text = std::fs::read_to_string(dir.join(CIZON_LOCK)).map_err(|_| {
        format!(
            "{CIZON_LOCK} 缺失，但 [dependencies] 声明了依赖。删除整个 deps/ 后重跑 cizon2 add 补锁。"
        )
    })?;
    let lock: toml::Table = lock_text
        .parse()
        .map_err(|e| format!("{CIZON_LOCK} 解析失败: {e}"))?;

    let mut jobs = Vec::new();
    for (name, info) in map {
        let entry = lock
            .get(name)
            .and_then(|v| v.as_table())
            .ok_or_else(|| {
                format!("{CIZON_LOCK} 里没有 {name} 的锁。运行 cizon2 update {name} 建锁。")
            })?;
        if entry.get("git").and_then(|v| v.as_str()) != Some(info.git.as_str()) {
            return Err(format!(
                "[dependencies] 与 {CIZON_LOCK} 对 {name} 的 git 地址不一致。按 [dependencies] 为准:运行 cizon2 update {name} 对齐。"
            ));
        }
        if entry.get("tag").and_then(|v| v.as_str()) != info.tag.as_deref() {
            return Err(format!(
                "[dependencies] 与 {CIZON_LOCK} 对 {name} 的 tag 不一致。运行 cizon2 update {name} 对齐。"
            ));
        }
        let Some(commit) = entry.get("commit").and_then(|v| v.as_str()) else {
            return Err(format!(
                "{CIZON_LOCK} 中 {name} 缺少 commit。删除该条目后重跑 cizon2 add。"
            ));
        };

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
                    "deps/{name} 的 HEAD ({head}) 与锁 ({commit}) 不一致。删除该目录后 build 会按锁恢复。"
                ));
            }
        } else {
            clone_pinned(dir, &info.git, info.tag.as_deref(), commit, name)?;
            println!("\x1b[32m   Restoring\x1b[0m {name} @ {commit}");
        }

        let src_dir = dep_dir.join("src");
        std::fs::create_dir_all(obj_dir.join(name)).ok();
        for src in flat_sources(&src_dir) {
            // C++20 模块单元（如 fmt 的 src/fmt.cc）无法按常规方式编译，
            // 且常规 .cpp 不可能以 `module;` 开头 —— 静默跳过。
            if is_module_unit(&src) {
                continue;
            }
            let stem = src.file_stem().unwrap().to_string_lossy().into_owned();
            let obj = obj_dir.join(name).join(format!("{stem}.o"));
            jobs.push((src, obj));
        }
    }
    Ok(jobs)
}

/// 按 tag（浅克隆 + commit 校验）或裸 commit（完整克隆 + checkout）还原。
fn clone_pinned(
    dir: &Path,
    url: &str,
    tag: Option<&str>,
    commit: &str,
    name: &str,
) -> Result<(), String> {
    let rel = format!("deps/{name}");
    match tag {
        Some(t) => {
            git_cmd(dir, &[
                "clone".into(), "--depth".into(), "1".into(),
                "--branch".into(), t.into(), url.into(), rel.clone(),
            ])?;
            let head = git_cmd(dir, &["-C".into(), rel, "rev-parse".into(), "HEAD".into()])?;
            if head != commit {
                return Err(format!(
                    "tag {t} 当前指向 {head}，与锁 {commit} 不一致（tag 被移动过）。运行 cizon2 update {name} 刷新锁。"
                ));
            }
        }
        None => {
            git_cmd(dir, &["clone".into(), url.into(), rel.clone()])?;
            git_cmd(dir, &["-C".into(), rel, "checkout".into(), commit.into()])?;
        }
    }
    Ok(())
}

/// 是否为 C++20 模块单元：首个非空、非 `//` 注释行即 `module;`。
fn is_module_unit(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    text.lines()
        .map(str::trim_start)
        .find(|l| !l.is_empty() && !l.starts_with("//"))
        .is_some_and(|l| l.starts_with("module;"))
}

/// 平铺扫描 dep 的 src/（依赖契约仍是平铺布局，不递归）。
fn flat_sources(src: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(src) {
        for entry in entries.flatten() {
            let path = entry.path();
            let ok = path
                .extension()
                .and_then(|e| e.to_str())
                .map_or(false, |e| DEP_EXTS.contains(&e));
            if path.is_file() && ok {
                out.push(path);
            }
        }
    }
    out.sort();
    out
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

    /// 本地"远端"仓库（无网络）：include/dep.h + 平铺 src/dep.cpp + init commit。
    fn make_origin(dir: &Path, with_tag: bool) {
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("include")).unwrap();
        fs::write(dir.join("include/dep.h"), "int depv();\n").unwrap();
        fs::write(dir.join("src/dep.cpp"), "int depv() { return 42; }\n").unwrap();
        git_cmd(dir, &["init".into()]).expect("本测试需要 git 在 PATH");
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

    #[test]
    fn corrupt_lock_is_not_silently_reset() {
        let dir = temp("corrupt-lock");
        fs::create_dir_all(&dir).unwrap();
        let original = "not = [valid toml";
        fs::write(dir.join(CIZON_LOCK), original).unwrap();
        let err = write_lock_entry(&dir, "x", "https://example.com/x", None, "abc")
            .unwrap_err();
        assert!(err.contains("解析失败"), "got: {err}");
        assert_eq!(fs::read_to_string(dir.join(CIZON_LOCK)).unwrap(), original);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn module_unit_detection_ignores_comment_header() {
        let dir = temp("module-unit");
        fs::create_dir_all(&dir).unwrap();
        let module = dir.join("fmt.cc");
        fs::write(&module, "// license\n// more\n\nmodule;\n").unwrap();
        assert!(is_module_unit(&module));
        let regular = dir.join("main.cpp");
        fs::write(&regular, "#include <iostream>\nint main() {}\n").unwrap();
        assert!(!is_module_unit(&regular));
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
        assert_eq!(
            lock.get(&name).unwrap().get("commit").unwrap().as_str(),
            Some(expect.as_str())
        );
        assert!(
            fs::read_to_string(proj.join("cizon.toml"))
                .unwrap()
                .contains("[dependencies]")
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
        assert!(add_in(&proj, &args).is_err());
        fs::remove_dir_all(origin).ok();
        fs::remove_dir_all(proj).ok();
    }

    #[test]
    fn update_rejects_unknown_name() {
        if !has_git() {
            return;
        }
        let proj = temp("upd-unknown-proj");
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();
        assert!(
            update_in(&proj, &[String::from("nope")])
                .unwrap_err()
                .contains("不在 [dependencies] 里")
        );
        fs::remove_dir_all(proj).ok();
    }

    /// 升/降同构的验收:tag 只改在 cizon.toml,update 把锁+deps 拉过去。
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

        // origin 前进到 c1 并打 v2(远端"新版本")
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
            ],
        )
        .unwrap();
        git_cmd(&origin, &["tag".into(), "v2".into()]).unwrap();
        let c1 = git_cmd(&origin, &["rev-parse".into(), "HEAD".into()]).unwrap();

        // 用户唯一动作:在 cizon.toml 表达新意图
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
        fs::remove_dir_all(origin).ok();
        fs::remove_dir_all(proj).ok();
    }

    #[test]
    fn build_compiles_dep_and_restores_from_lock() {
        if !has_git() {
            return;
        }
        if crate::compiler::pick(None).is_err() {
            return; // 无编译器环境跳过（与 pick_finds… 同一跳过策略）
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
        let cxx = crate::compiler::pick(None).unwrap();
        let cfg = config::load(&proj).unwrap();
        let plan = crate::pipeline::plan(&proj, &cfg, cxx.clone(), false).unwrap();
        let exe = crate::pipeline::build(&proj, &cfg, &plan).unwrap();
        assert!(exe.is_file(), "首次构建应产出 exe");

        // 关键契约：deps/ 被删除 → build 自动按锁恢复并重编
        fs::remove_dir_all(proj.join("deps")).unwrap();
        let cfg = config::load(&proj).unwrap();
        let plan = crate::pipeline::plan(&proj, &cfg, cxx, false).unwrap();
        crate::pipeline::build(&proj, &cfg, &plan).unwrap();
        fs::remove_dir_all(origin).ok();
        fs::remove_dir_all(proj).ok();
    }
}
