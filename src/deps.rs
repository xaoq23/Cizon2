//! `cizon2 add <git-url> [--tag <tag>]`：git 依赖的最小正确实现（v0.2 路线）。
//!
//! 设计边界：
//! - 承诺"常规布局"：`<dep>/include/` 头文件 + 平铺 `<dep>/src/*.cpp`；
//!   超出此形状的库不迁 —— 那是 CMake 的领地
//! - `cizon.lock` 是机器文件（每次整表重写，不保注释），锁定的 commit 是唯一真相；
//!   cizon.toml ↔ cizon.lock ↔ deps/ 任一不一致 → 大声报错，绝不静默自愈
//! - 头文件路径无条件写进 flags（`-I<dep>/include` 与 `-I<dep>`），
//!   目录不存在时编译器安全忽略，因此 planning 不需要先做网络操作

use std::collections::HashMap;
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

    let toml_path = dir.join("cizon.toml");
    let original_toml =
        std::fs::read_to_string(&toml_path).map_err(|e| format!("读 cizon.toml 失败: {e}"))?;
    let lock_path = dir.join(CIZON_LOCK);
    let original_lock = match std::fs::read_to_string(&lock_path) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("读 {CIZON_LOCK} 失败: {e}")),
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

    println!("\x1b[32m      Added\x1b[0m {name} → deps/{name} (锁 commit {commit})");
    println!("\x1b[32m      Next\x1b[0m cizon2 build");
    Ok(0)
}

fn remove_dep_dir(path: &Path) -> bool {
    match std::fs::remove_dir_all(path) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => false,
    }
}

/// 依赖布局提示：非标准布局不阻止构建，但明确告诉用户为什么可能找不到头。
fn layout_warning(dep_dir: &Path, name: &str) -> Option<String> {
    if dep_dir.join("include").is_dir() {
        return None;
    }
    Some(format!(
        "警告: deps/{name} 缺少 include/，不符合 cizon2 依赖布局（include/ + 平铺 src/*.cpp）。若该库使用其它构建系统，请改用它构建并通过 link_flags 接入。"
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
        format!("{err}\n已回滚 cizon.toml、{CIZON_LOCK} 与 deps/{name}。")
    } else {
        format!(
            "{err}\n自动回滚未完成，请手动检查 cizon.toml、{CIZON_LOCK} 与 deps/{name}。"
        )
    }
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
    if !config::is_valid_dep_name(&name) {
        return Err(format!("依赖名 {name} 含非法字符（只允许字母数字、- 和 _）。"));
    }
    Ok(name)
}

/// 浅克隆（有 tag 则一并 --branch 定位）到 `rel`，返回所在 commit。
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

/// 把 git 执行细节收敛在一处：cwd = 项目根，args 原样转发，stdout trim 返回。
fn git_cmd(dir: &Path, args: &[String]) -> Result<String, String> {
    let out = Command::new("git")
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .args(args)
        .output()
        .map_err(|e| format!("启动 git 失败: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {} 失败: {}",
            git_verb(args),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// 错误信息里显示真正的 git 子命令（跳过 -C/-c 及其取值）。
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

/// `[dependencies]` 表头；允许括号内外空白与行尾注释，
/// 但不会把 `[dependencies.foo]` 误认成表头。
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

/// 在 cizon.toml 里登记：有 [dependencies] 头就在其下一行插入；
/// 没有则整段追加到文件尾。写前重新解析，拒绝把合法文件改成非法 TOML。
fn append_entry(dir: &Path, name: &str, url: &str, tag: Option<&str>) -> Result<(), String> {
    let path = dir.join("cizon.toml");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("读 cizon.toml 失败: {e}"))?;
    let has_header = text.lines().any(is_dependencies_header);
    if !has_header && text.parse::<toml::Table>().is_ok_and(|t| t.contains_key("dependencies")) {
        return Err(
            "cizon.toml 用内联表声明了 dependencies；请改成 [dependencies] 表头后再重试（未改动文件）。"
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
        .map_err(|e| format!("更新 cizon.toml 会得到非法 TOML（未改动文件）: {e}"))?;
    let registered = table
        .get("dependencies")
        .and_then(|v| v.as_table())
        .is_some_and(|t| t.contains_key(name));
    if !registered {
        return Err(format!(
            "无法在 cizon.toml 的 [dependencies] 中登记 {name}（未改动文件）。请检查该段格式。"
        ));
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
/// 读失败时只警告，绝不用空内容覆盖用户文件。
fn ignore_deps_dir(dir: &Path) {
    let path = dir.join(".gitignore");
    let mut text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            eprintln!("警告: 读 {} 失败，未更新 .gitignore: {e}", path.display());
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
        eprintln!("警告: 更新 {} 失败: {e}", path.display());
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

    let deps_dir = dir.join("deps");
    std::fs::create_dir_all(&deps_dir)
        .map_err(|e| format!("无法创建 {}: {e}", deps_dir.display()))?;
    let dep_dir = deps_dir.join(name);
    // update 会替换整个 checkout；未提交改动必须显式放弃
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
                "deps/{name} 有未提交改动，update 会丢弃它们。提交并推送到远端、更新 cizon.toml 后重试；或删除该目录放弃改动。"
            ));
        }
    }
    // 先克隆到临时目录：网络失败时保留旧的 checkout，而不是先删后拉
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
            return Err(format!("删 deps/{name} 失败: {e}"));
        }
    }
    if let Err(e) = std::fs::rename(&tmp_dir, &dep_dir) {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(format!("将 {tmp_rel} 移到 deps/{name} 失败: {e}"));
    }
    if let Err(e) = write_lock_entry(dir, name, &info.git, info.tag.as_deref(), &commit) {
        return Err(format!(
            "{e}\n依赖目录已更新，但锁写入失败；修复后重跑 cizon2 update {name}。"
        ));
    }
    println!("\x1b[32m   Updated\x1b[0m {name} @ {commit}");
    Ok(0)
}

// ─────────────────────────────────────────────────────────────
// build 期的依赖契约：restore / 校验 / 产出编译三元组
// ─────────────────────────────────────────────────────────────

/// build 调用：依赖目录满足锁则跳过，缺失则按锁恢复（含 tag 校验），
/// 成功后返回 (dep名, src, obj) 编译三元组；obj 放 obj_dir/<dep>/<stem>.o，
/// 与项目 obj 物理隔离。
pub fn prepare(
    dir: &Path,
    cfg: &config::Config,
    obj_dir: &Path,
) -> Result<Vec<(String, PathBuf, PathBuf)>, String> {
    let Some(map) = cfg.dependencies() else { return Ok(vec![]) };
    if map.is_empty() {
        return Ok(vec![]);
    }
    let lock_path = dir.join(CIZON_LOCK);
    let lock_text = match std::fs::read_to_string(&lock_path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "{CIZON_LOCK} 缺失，但 [dependencies] 声明了依赖。运行 cizon2 update <依赖名> 按 cizon.toml 重建锁。"
            ));
        }
        Err(e) => return Err(format!("读 {CIZON_LOCK} 失败: {e}")),
    };
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
                "{CIZON_LOCK} 中 {name} 缺少 commit。运行 cizon2 update {name} 重建锁。"
            ));
        };
        if !is_commit_hash(commit) {
            return Err(format!(
                "{CIZON_LOCK} 中 {name} 的 commit 不是 40/64 位十六进制。运行 cizon2 update {name} 重建锁。"
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
                    "deps/{name} 的 HEAD ({head}) 与锁 ({commit}) 不一致。若本地有需要保留的提交，先推送到远端并更新 cizon.toml 的 tag；否则运行 cizon2 update {name} 对齐，或删除该目录后 build 按锁恢复。"
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
                    "deps/{name} 有未提交改动，锁不再代表实际代码。提交并推送到远端、更新 cizon.toml 的 tag 后再 update；或删除该目录后 build 按锁恢复。"
                ));
            }
        } else {
            clone_pinned(dir, &info.git, info.tag.as_deref(), &commit, name)?;
            eprintln!("\x1b[32m   Restoring\x1b[0m {name} @ {commit}");
        }

        if let Some(warning) = layout_warning(&dep_dir, name) {
            eprintln!("{warning}");
        }

        let src_dir = dep_dir.join("src");
        let dep_obj_dir = obj_dir.join(name);
        std::fs::create_dir_all(&dep_obj_dir)
            .map_err(|e| format!("无法创建 {}: {e}", dep_obj_dir.display()))?;
        let mut stems: HashMap<String, PathBuf> = HashMap::new();
        for src in flat_sources(&src_dir)? {
            // C++20 模块单元（如 fmt 的 src/fmt.cc）无法按常规方式编译，
            // 且常规 .cpp 不可能以 `module;` 开头 —— 静默跳过。
            if is_module_unit(&src)? {
                continue;
            }
            let stem = src.file_stem().unwrap().to_string_lossy().into_owned();
            if let Some(prev) = stems.insert(stem.to_ascii_lowercase(), src.clone()) {
                return Err(format!(
                    "依赖 {name} 的源文件对象名冲突（同 stem）:\n  {}\n  {}\n请让依赖的 src/ 保持平铺且文件名唯一。",
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

/// 按 tag（浅克隆 + commit 校验）或裸 commit（完整克隆 + checkout）还原。
/// 失败时清理半成品目录，避免下次 build 撞上不可信的 checkout。
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
                        "tag {t} 当前指向 {head}，与锁 {commit} 不一致（tag 被移动过）。运行 cizon2 update {name} 刷新锁。"
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
                        "checkout {commit} 后 HEAD 为 {head}，与锁不一致。运行 cizon2 update {name} 重建锁。"
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

/// 是否为 C++20 模块单元：跳过 BOM、空白与行/块注释后，首个 token 是 `module;`。
fn is_module_unit(path: &Path) -> Result<bool, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("读取 {} 失败: {e}", path.display()))?;
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

/// 平铺扫描 dep 的 src/（依赖契约仍是平铺布局，不递归）。
fn flat_sources(src: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = match std::fs::read_dir(src) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(format!("无法读取 {}: {e}", src.display())),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("无法读取 {} 的目录项: {e}", src.display()))?;
        let path = entry.path();
        let is_source = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| DEP_EXTS.iter().any(|ext| ext.eq_ignore_ascii_case(e)));
        if !is_source {
            continue;
        }
        // 只对源文件取元数据；跟随符号链接，悬空链接视为不存在
        match std::fs::metadata(&path) {
            Ok(meta) if meta.is_file() => out.push(path),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(format!("无法读取 {} 的元数据: {e}", path.display()));
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
        panic!("未找到对象 {stem}");
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
        assert_eq!(mtime(&dep_obj), dep_t, "用户 flags 变化不应重编依赖源");
        assert_ne!(mtime(&main_obj), main_t, "项目源应按新 flags 重编");
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
            "每个依赖必须优先看到自己的 config.h"
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
        assert!(is_module_unit(&module).unwrap());
        let block = dir.join("block.cc");
        fs::write(&block, "\u{feff}/* license */\nmodule;\n").unwrap();
        assert!(is_module_unit(&block).unwrap(), "BOM + 块注释头也应识别");
        let regular = dir.join("main.cpp");
        fs::write(&regular, "#include <iostream>\nint main() {}\n").unwrap();
        assert!(!is_module_unit(&regular).unwrap());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn layout_warning_checks_include_dir() {
        let dir = temp("layout-warning");
        fs::create_dir_all(&dir).unwrap();
        let warning = layout_warning(&dir, "dep").expect("missing include should warn");
        assert!(warning.contains("include/"), "got: {warning}");
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
        assert_eq!(names, ["A.CPP", "b.cc"], "应只收源文件: {names:?}");
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
        assert!(err.contains("内联"), "got: {err}");
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

        // tag 恢复路径：删掉 deps 后 prepare 应按 tag 克隆并校验 commit
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

        // tag 被移动：按锁恢复必须拒绝
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
        assert!(err.contains("tag 被移动"), "got: {err}");

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
            "克隆失败时旧 checkout 必须保留"
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
        assert!(err.contains("已有"), "got: {err}");
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
        let c0 = git_cmd(&origin, &["rev-parse".into(), "v1".into()]).unwrap();

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
                "--no-gpg-sign".into(),
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

        // 降级同构：改回 v1 → update 把锁与 deps 拉回 c0
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
        let name = origin.file_name().unwrap().to_string_lossy().into_owned();
        let c0 = git_cmd(&origin, &["rev-parse".into(), "HEAD".into()]).unwrap();
        let cxx = crate::compiler::pick(None).unwrap();
        let cfg = config::load(&proj).unwrap();
        let plan = crate::pipeline::plan(&proj, &cfg, cxx.clone(), false).unwrap();
        let exe = crate::pipeline::build(&proj, &cfg, &plan).unwrap();
        assert!(exe.is_file(), "首次构建应产出 exe");
        assert!(
            Command::new(&exe).status().unwrap().success(),
            "依赖构建出的程序应正常运行"
        );

        // 依赖工作区被改动 → 锁不再代表实际代码，必须报错
        fs::write(
            proj.join("deps").join(&name).join("src/dep.cpp"),
            "int depv() { return 0; }\n",
        )
        .unwrap();
        let cfg = config::load(&proj).unwrap();
        let plan = crate::pipeline::plan(&proj, &cfg, cxx.clone(), false).unwrap();
        let err = crate::pipeline::build(&proj, &cfg, &plan).unwrap_err();
        assert!(err.contains("未提交改动"), "got: {err}");

        // 关键契约：deps/ 被删除 → build 自动按锁恢复并重编
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
            "恢复后的 HEAD 必须回到锁中的 commit"
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
        assert!(err.contains("冲突"), "got: {err}");
        fs::remove_dir_all(origin).ok();
        fs::remove_dir_all(proj).ok();
    }
}
