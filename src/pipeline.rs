//! 构建管线：源收集 → 并行编译（%.d 增量） → 链接。
//!
//! 从 Cizon1 迁移的最小正确核心：mtime + `-MMD` 依赖文件级增量，std::thread 并行，
//! stderr 直传 —— 不迁移 PCH / 指纹缓存 / 错误格式化器。
//! v0.2.1 起按现代 C++ 结构收集：递归 src/，include/ 自动上 -I。

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Instant;

use crate::compiler::{self, Cxx};
use crate::config::Config;
use crate::deps;

const SOURCE_EXTS: &[&str] = &["cpp", "cc", "cxx"];

pub struct Plan {
    cxx: Cxx,
    /// 源文件相对项目根
    sources: Vec<PathBuf>,
    /// 编译期 flags（edition + 优化级 + 用户 flags + 依赖 include 路径）
    flags: Vec<String>,
    /// 链接期附加 flags（用户 link_flags；用户 flags 已在 flags 中，不重复传）
    link_flags: Vec<String>,
    /// 依赖源码的编译 flags（edition + 优化级；不套用户 flags，免得 -Werror 误伤三方代码）
    dep_flags: Vec<String>,
    out_name: String,
    /// "debug" / "release" —— 与编译器一起决定产物作用域
    mode: &'static str,
}

/// 为一次构建装配计划：失败即中止，不进入编译阶段。
pub fn plan(dir: &Path, cfg: &Config, cxx: Cxx, release: bool) -> Result<Plan, String> {
    let sources = collect_sources(dir, cfg.explicit_sources())?;
    if sources.is_empty() {
        return Err(format!(
            "{} 下没有 .cpp 文件。在 src/ 放入源代码，或在 [project] sources = [...] 显式列出。",
            dir.join("src").display()
        ));
    }

    let edition = compiler::edition_flag(cfg.edition());
    let optimization = if release { "-O2" } else { "-g" };
    let mut flags = vec![edition.clone(), optimization.into()];
    flags.extend(cfg.flags().iter().cloned());
    // 公有头目录：现代结构的 include/ 自动上 -I；目录不存在时编译器安全忽略
    flags.push(format!("-I{}", dir.join("include").display()));
    // 依赖头文件路径无条件进 flags：目录不存在时编译器安全忽略，
    // 因此 planning 无需网络操作
    let mut dep_includes: Vec<String> = Vec::new();
    if let Some(map) = cfg.dependencies() {
        for name in map.keys() {
            let inc = dir.join("deps").join(name);
            let include = format!("-I{}", inc.join("include").display());
            let root = format!("-I{}", inc.display());
            flags.push(include.clone());
            flags.push(root.clone());
            // 依赖自己的源码也要找得到自己的头（如 fmt 的 src/*.cc 内部引用）
            dep_includes.push(include);
            dep_includes.push(root);
        }
    }
    let mut dep_flags = vec![edition, optimization.into()];
    dep_flags.extend(dep_includes);

    let link_flags: Vec<String> = cfg.link_flags().to_vec();

    Ok(Plan {
        cxx,
        sources,
        flags,
        link_flags,
        dep_flags,
        out_name: exe_name(&cfg.name(dir)),
        mode: if release { "release" } else { "debug" },
    })
}

fn exe_name(name: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// 编译器名可能是完整路径（--compiler C:\...\g++.exe），目录名只保留安全字符。
fn scope_label(cxx: &str) -> String {
    cxx.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// 显式 sources 或递归扫描 src/。同名 stem 冲突立刻报错（对象文件按 stem 命名）。
fn collect_sources(dir: &Path, explicit: Option<&[String]>) -> Result<Vec<PathBuf>, String> {
    let mut out: Vec<PathBuf> = Vec::new();
    match explicit {
        Some(list) => {
            for raw in list {
                let path = dir.join(raw);
                if !path.exists() {
                    return Err(format!("[project].sources 列出的文件不存在: {}", path.display()));
                }
                check_source_ext(&path)?;
                out.push(path);
            }
        }
        None => {
            let src = dir.join("src");
            if !src.is_dir() {
                return Err(format!("未找到 src/ 目录（或无权读取）: {}", src.display()));
            }
            collect_recursive(&src, &mut out)?;
            out.sort();
        }
    }
    // stem 冲突检查：obj = build/<scope>/obj/<stem>.o，两个同名 stem 会互相覆盖。
    let mut stems: HashMap<String, PathBuf> = HashMap::new();
    for src in &out {
        let stem = src.file_stem().unwrap().to_string_lossy().to_string();
        if let Some(prev) = stems.insert(stem.clone(), src.clone()) {
            return Err(format!(
                "两个源文件的编译产物名冲突（同 stem）:\n  {}\n  {}\n请改名，或用 [project].sources 显式排重。",
                prev.display(),
                src.display()
            ));
        }
    }
    Ok(out)
}

fn is_source(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map_or(false, |e| SOURCE_EXTS.contains(&e))
}

/// 递归收集目录下的源文件。不跟进符号链接目录（防环），链接的源文件照收。
fn collect_recursive(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("无法读取 {}: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            collect_recursive(&path, out)?;
        } else if is_source(&path) && path.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

fn check_source_ext(path: &Path) -> Result<(), String> {
    if is_source(path) {
        Ok(())
    } else {
        Err(format!(
            "sources 列表只接受 {:?}，拒绝: {}",
            SOURCE_EXTS,
            path.display()
        ))
    }
}

// ── 构建执行 ──────────────────────────────────────────────

/// 一个待编译单元：项目源用 plan.flags，依赖源用 dep_flags（不套用户 flags）。
#[derive(Clone)]
struct Job {
    src: PathBuf,
    obj: PathBuf,
    flags: Vec<String>,
}

/// 执行完整构建，返回可执行文件路径。
///
/// 产物作用域（迁移自 Cizon1 编译器沙盒教训）：`build/<mode>-<compiler>/`。
/// 不同编译器或 debug/release 互相物理隔离，禁止混链；同名编译器版本变化时
/// 依据 `.compiler-version` 指纹整目录作废。
pub fn build(dir: &Path, cfg: &Config, plan: &Plan) -> Result<PathBuf, String> {
    let started = Instant::now();
    let build_dir = dir.join("build");
    let scope = format!("{}-{}", plan.mode, scope_label(&plan.cxx.name));
    let scope_dir = build_dir.join(&scope);
    let marker = scope_dir.join(".compiler-version");
    if std::fs::read_to_string(&marker)
        .map_or(false, |t| t != plan.cxx.version)
    {
        let _ = std::fs::remove_dir_all(&scope_dir);
    }
    let obj_dir = scope_dir.join("obj");
    // 编译参数（edition/优化级/用户 flags/依赖 include 集）变化不触碰任何 mtime，
    // 不主动作废对象就会静默复用旧行为的 .o。
    let compile_state = scope_dir.join(".compile-state");
    let wanted_compile = compile_state_text(plan);
    if std::fs::read_to_string(&compile_state).map_or(true, |t| t != wanted_compile) {
        let _ = std::fs::remove_dir_all(&obj_dir);
    }
    std::fs::create_dir_all(&obj_dir)
        .map_err(|e| format!("无法创建 {} : {e}", obj_dir.display()))?;
    let _ = std::fs::write(&compile_state, &wanted_compile);
    let _ = std::fs::write(&marker, &plan.cxx.version);

    // 0) 依赖契约：restore / 校验。目录缺失按锁恢复，与锁不一致当 loud 报错。
    let dep_jobs = deps::prepare(dir, cfg, &obj_dir)?;

    // 1) 判定需要重编译的源
    let mut jobs: Vec<Job> = Vec::new(); // 项目源
    for src in &plan.sources {
        let obj = obj_for(src, &obj_dir);
        if needs_recompile(src, &obj) {
            jobs.push(Job { src: src.clone(), obj, flags: plan.flags.clone() });
        }
    }
    for (dsrc, dobj) in &dep_jobs {
        if needs_recompile(dsrc, dobj) {
            jobs.push(Job { src: dsrc.clone(), obj: dobj.clone(), flags: plan.dep_flags.clone() });
        }
    }
    let total = plan.sources.len() + dep_jobs.len();
    let recompiled = jobs.len();
    if recompiled > 0 {
        let label = if recompiled == total { "Compiling" } else { "Rebuilding" };
        step_line(label, &format!("{} [{recompiled}/{total} files]", plan.cxx.name));
    }

    // 2) 并行编译（工作队列 + n 线程），先收集输出再统一打印，避免交错乱序
    let warnings = Mutex::new(Vec::<String>::new());
    let failures = Mutex::new(Vec::<(PathBuf, String)>::new());
    if !jobs.is_empty() {
        let queue = Mutex::new(jobs);
        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2)
            .max(1);
        std::thread::scope(|scope| {
            for _ in 0..threads {
                scope.spawn(|| loop {
                    let job = queue.lock().expect("queue poisoned").pop();
                    let Some(Job { src, obj, flags }) = job else { break };
                    match compile_one(&plan.cxx.name, &src, &obj, &flags) {
                        Ok(err) => {
                            if let Some(stderr) = err {
                                warnings.lock().expect("warn poisoned").push(stderr);
                            }
                        }
                        Err(stderr) => {
                            failures.lock().expect("fail poisoned").push((src, stderr));
                        }
                    }
                });
            }
        });
    }

    let warnings = warnings.into_inner().expect("warn poisoned");
    let failures = failures.into_inner().expect("fail poisoned");
    if !failures.is_empty() {
        let mut msg = String::new();
        for (src, stderr) in &failures {
            msg.push_str(&format!("\n编译失败: {} (exit != 0)\n", src.display()));
            msg.push_str(stderr);
        }
        return Err(msg);
    }
    let mut all = std::io::stderr();
    for w in &warnings {
        let _ = all.write_all(w.as_bytes());
    }
    // 3) 链接（无重编译、链接输入与参数未变且产物新鲜时跳过）
    let exe = scope_dir.join(&plan.out_name);
    let mut objs: Vec<PathBuf> = plan
        .sources
        .iter()
        .map(|src| obj_for(src, &obj_dir))
        .collect();
    objs.extend(dep_jobs.iter().map(|(_, obj)| obj.clone()));
    // 对象集收缩（删源）不产生任何 mtime 变化，靠这项状态强制重链。
    let link_state = scope_dir.join(".link-state");
    let wanted_link = link_state_text(plan, &objs);
    let link_current = std::fs::read_to_string(&link_state)
        .map_or(false, |t| t == wanted_link);
    if recompiled == 0 && link_current && exe_is_fresh(&exe, &objs) {
        step_line(
            "Finished",
            &format!("up to date ({:.2}s)", started.elapsed().as_secs_f32()),
        );
        return Ok(exe);
    }
    step_line("Linking", &format!("{} ({})", exe.display(), plan.cxx.short_version()));
    link(&plan.cxx.name, &objs, &exe, &plan.flags, &plan.link_flags)?;
    let _ = std::fs::write(&link_state, &wanted_link);

    step_fin(started, recompiled, total);
    Ok(exe)
}

/// 编译状态：项目源与依赖源各自的全部编译参数。逐行文本便于人眼 diff。
fn compile_state_text(plan: &Plan) -> String {
    let mut s = String::from("project:\n");
    for f in &plan.flags {
        s.push_str(f);
        s.push('\n');
    }
    s.push_str("deps:\n");
    for f in &plan.dep_flags {
        s.push_str(f);
        s.push('\n');
    }
    s
}

/// 链接状态：参与链接的对象清单 + 链接参数。任一项变化即需重链。
fn link_state_text(plan: &Plan, objs: &[PathBuf]) -> String {
    let mut s = String::from("objects:\n");
    for obj in objs {
        s.push_str(&obj.display().to_string());
        s.push('\n');
    }
    s.push_str("link-flags:\n");
    for f in &plan.link_flags {
        s.push_str(f);
        s.push('\n');
    }
    s
}

/// exe 是否不旧于所有对象文件。
fn exe_is_fresh(exe: &Path, objs: &[PathBuf]) -> bool {
    let Ok(exe_time) = std::fs::metadata(exe).and_then(|m| m.modified()) else {
        return false;
    };
    objs.iter().all(|obj| {
        std::fs::metadata(obj)
            .and_then(|m| m.modified())
            .map_or(false, |t| t <= exe_time)
    })
}

fn obj_for(src: &Path, obj_dir: &Path) -> PathBuf {
    let stem = src.file_stem().unwrap().to_string_lossy();
    obj_dir.join(format!("{stem}.o"))
}

/// 增量判定：src、obj、.d 都存在，且 src 与 .d 列出的每个输入都不比 obj 新。
/// .d 读不出来时宁可重编，绝不把损坏的依赖信息当成"全部新鲜"。
fn needs_recompile(src: &Path, obj: &Path) -> bool {
    let dep_file = dep_file_for(obj);
    if !src.exists() || !obj.exists() || !dep_file.exists() {
        return true;
    }
    let Ok(obj_time) = std::fs::metadata(obj).and_then(|m| m.modified()) else {
        return true;
    };
    let stale = |p: &Path| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .map_or(true, |t| t > obj_time)
    };
    if stale(src) {
        return true;
    }
    match dep_inputs(&dep_file) {
        Ok(inputs) if !inputs.is_empty() => inputs.iter().any(|p| stale(p)),
        _ => true,
    }
}

fn dep_file_for(obj: &Path) -> PathBuf {
    PathBuf::from(format!("{}.d", obj.display()))
}

/// 解析 gcc/clang 的 depfile（`target: dep dep dep ...`），
/// 首行目标后的所有输入；处理 `\` 续行与 Windows 路径分隔符。
/// `-MP` 生成的孤行（如 `foo.h:`）因不含 ": " 而被安全跳过。
fn dep_inputs(dep_file: &Path) -> Result<Vec<PathBuf>, String> {
    let content = std::fs::read_to_string(dep_file)
        .map_err(|e| format!("读取依赖文件失败: {e}"))?;
    let mut joined = String::new();
    for line in content.lines() {
        if let Some(stripped) = line.strip_suffix('\\') {
            joined.push_str(stripped);
        } else {
            joined.push_str(line);
            joined.push(' ');
        }
    }
    let deps = joined
        .split_once(": ")
        .map(|(_, rest)| rest)
        .unwrap_or("");
    Ok(deps
        .split_whitespace()
        .filter(|s| !s.ends_with(':'))
        .map(|s| if cfg!(target_os = "windows") {
            PathBuf::from(s.replace('/', "\\"))
        } else {
            PathBuf::from(s)
        })
        .collect())
}

/// 单文件编译。Ok 表示成功（Some = 警告输出），Err = 失败（完整 stderr）。
fn compile_one(
    cxx: &str,
    src: &Path,
    obj: &Path,
    flags: &[String],
) -> Result<Option<String>, String> {
    let dep_file = dep_file_for(obj);
    let mut cmd = Command::new(cxx);
    cmd.arg("-c").arg(src).arg("-o").arg(obj);
    cmd.arg("-MMD").arg("-MF").arg(&dep_file).arg("-MP");
    for f in flags {
        cmd.arg(f);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("启动编译器 {cxx} 失败: {e}"))?;
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    if !out.status.success() {
        return Err(stderr);
    }
    if stderr.trim().is_empty() {
        Ok(None)
    } else {
        Ok(Some(stderr))
    }
}

fn link(
    cxx: &str,
    objs: &[PathBuf],
    exe: &Path,
    flags: &[String],
    link_flags: &[String],
) -> Result<(), String> {
    let mut cmd = Command::new(cxx);
    for obj in objs {
        cmd.arg(obj);
    }
    cmd.arg("-o").arg(exe);
    for f in flags {
        cmd.arg(f);
    }
    for f in link_flags {
        cmd.arg(f);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("启动链接器 {cxx} 失败: {e}"))?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        return Err(stderr.to_string());
    }
    if !stderr.trim().is_empty() {
        let mut err = std::io::stderr();
        let _ = err.write_all(stderr.as_bytes());
    }
    Ok(())
}

// ── 输出风格（cargo-like） ─────────────────────────────────

const GREEN: &str = "\x1b[32m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

fn step_line(label: &str, detail: &str) {
    println!("{GREEN}{label:>12}{RESET} {detail}", label = label, detail = detail);
}

fn step_fin(started: Instant, recompiled: usize, total: usize) {
    let secs = started.elapsed().as_secs_f32();
    let note = if recompiled == total {
        "fresh build"
    } else {
        "incremental rebuild"
    };
    step_line("Finished", &format!("{DIM}{secs:.2}{RESET}s ({note})"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cizon2-bl-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn user_flags_are_linked_once() {
        let dir = temp_dir("linkflags");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}").unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname=\"t\"\n\n[compiler]\nflags=[\"-Wall\"]\nlink_flags=[\"-lm\"]\n",
        )
        .unwrap();
        let cfg = crate::config::load(&dir).unwrap();
        let cxx = Cxx { name: "g++".into(), version: "test".into() };
        let p = plan(&dir, &cfg, cxx, false).unwrap();
        assert!(p.flags.iter().any(|f| f == "-Wall"));
        assert_eq!(p.link_flags, vec!["-lm".to_string()]);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn compiler_path_is_sanitized_for_scope_dir() {
        assert_eq!(scope_label("g++"), "g++");
        assert_eq!(scope_label(r"C:\mingw64\bin\g++.exe"), "C__mingw64_bin_g++.exe");
        assert_eq!(scope_label("/usr/bin/clang++"), "_usr_bin_clang++");
    }

    #[test]
    fn collect_sources_from_src_dir() {
        let dir = temp_dir("collect");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}").unwrap();
        fs::write(dir.join("src/util.cc"), "//").unwrap();
        fs::write(dir.join("src/notes.txt"), "ignore").unwrap();
        let s = collect_sources(&dir, None).unwrap();
        assert_eq!(s.len(), 2);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn explicit_sources_resolve_and_validate() {
        let dir = temp_dir("explicit");
        fs::create_dir_all(dir.join("third_party")).unwrap();
        fs::write(dir.join("third_party/x.cpp"), "//").unwrap();
        let list = vec!["third_party/x.cpp".to_string()];
        let s = collect_sources(&dir, Some(&list)).unwrap();
        assert_eq!(s.len(), 1);
        let missing = vec!["missing.cpp".to_string()];
        let err = collect_sources(&dir, Some(&missing)).unwrap_err();
        assert!(err.contains("不存在"));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn stem_conflict_detected() {
        let dir = temp_dir("conflict");
        fs::create_dir_all(dir.join("a")).unwrap();
        fs::create_dir_all(dir.join("b")).unwrap();
        fs::write(dir.join("a/core.cpp"), "//").unwrap();
        fs::write(dir.join("b/core.cpp"), "//").unwrap();
        let list = vec!["a/core.cpp".to_string(), "b/core.cpp".to_string()];
        let err = collect_sources(&dir, Some(&list)).unwrap_err();
        assert!(err.contains("冲突"), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn depfile_parsing_with_continuation() {
        let dir = temp_dir("depfile");
        let f = dir.join("main.d");
        fs::write(
            &f,
            "build/obj/main.o: \\\n src/main.cpp src/util.hpp \\\n third_party/x.h\n\nsrc/util.hpp:\n",
        )
        .unwrap();
        let inputs = dep_inputs(&f).unwrap();
        let names: Vec<String> = inputs
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        assert!(names.iter().any(|p| p.contains("main.cpp")));
        assert_eq!(names.len(), 3, "should skip -MP phony line: {names:?}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn obj_missing_means_recompile() {
        let dir = temp_dir("objmissing");
        let src = dir.join("a.cpp");
        fs::write(&src, "//").unwrap();
        let obj = dir.join("nope.o");
        assert!(needs_recompile(&src, &obj));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unreadable_depfile_triggers_recompile() {
        let dir = temp_dir("baddep");
        let src = dir.join("a.cpp");
        let obj = dir.join("a.o");
        fs::write(&src, "//").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&obj, "").unwrap();
        // 目录冒充 .d：存在但读不出来 —— 必须按"需要重编"处理
        fs::create_dir(dir.join("a.o.d")).unwrap();
        assert!(needs_recompile(&src, &obj));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn malformed_depfile_triggers_recompile() {
        let dir = temp_dir("garbagedep");
        let src = dir.join("a.cpp");
        let obj = dir.join("a.o");
        fs::write(&src, "//").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&obj, "").unwrap();
        fs::write(dir.join("a.o.d"), "garbage without target separator").unwrap();
        assert!(needs_recompile(&src, &obj));
        fs::remove_dir_all(dir).ok();
    }

    // ── P0：增量构建正确性（真实编译器端到端） ───────────────
    // 共同前提：配置或源码集变化后，产物必须跟着变；
    // 没有变化时，对象与 exe 一个字节都别动。

    fn have_compiler() -> bool {
        crate::compiler::pick(None).is_ok()
    }

    fn build_project(dir: &Path) -> Result<PathBuf, String> {
        let cfg = crate::config::load(dir)?;
        let cxx = crate::compiler::pick(None)?;
        let plan = plan(dir, &cfg, cxx, false)?;
        build(dir, &cfg, &plan)
    }

    /// 在 build/<scope>/obj/ 下找 stem.o（scope 名含平台差异，测试不硬编码）。
    fn obj_path(dir: &Path, stem: &str) -> PathBuf {
        for scope in fs::read_dir(dir.join("build")).unwrap().flatten() {
            let p = scope.path().join("obj").join(format!("{stem}.o"));
            if p.exists() {
                return p;
            }
        }
        panic!("未找到 {stem}.o");
    }

    fn mtime(p: &Path) -> std::time::SystemTime {
        fs::metadata(p).unwrap().modified().unwrap()
    }

    fn run_code(exe: &Path) -> i32 {
        Command::new(exe)
            .status()
            .unwrap()
            .code()
            .unwrap_or(-1)
    }

    fn run_stdout(exe: &Path) -> String {
        let out = Command::new(exe).output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[test]
    fn no_op_rebuild_keeps_objects_and_exe() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("noop");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}\n").unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();

        let exe = build_project(&dir).unwrap();
        let obj = obj_path(&dir, "main");
        let (obj_t, exe_t) = (mtime(&obj), mtime(&exe));
        std::thread::sleep(std::time::Duration::from_millis(50));
        build_project(&dir).unwrap();
        assert_eq!(mtime(&obj), obj_t, "无变更构建不应重编对象");
        assert_eq!(mtime(&exe), exe_t, "无变更构建不应重链 exe");
        fs::remove_dir_all(dir).ok();
    }

    /// 编译器 flags 变化没有触碰任何源文件，但必须让对象作废：
    /// 用 -DVALUE=n 让旧对象与旧 exe 的返回值可观测。
    #[test]
    fn compile_flag_change_rebuilds_objects() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("cflags");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(
            dir.join("src/main.cpp"),
            "#ifndef VALUE\n#define VALUE 0\n#endif\nint main(){return VALUE;}\n",
        )
        .unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n[compiler]\nflags = [\"-DVALUE=1\"]\n",
        )
        .unwrap();

        let exe = build_project(&dir).unwrap();
        // 运行副本：Windows 下刚执行过的 exe 可能被系统短暂占用，阻塞重链
        let first = dir.join("first.exe");
        fs::copy(&exe, &first).unwrap();
        assert_eq!(run_code(&first), 1, "首个构建应按 VALUE=1 编译");
        let obj = obj_path(&dir, "main");
        let obj_t = mtime(&obj);
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n[compiler]\nflags = [\"-DVALUE=2\"]\n",
        )
        .unwrap();

        let exe = build_project(&dir).unwrap();
        assert_eq!(
            run_code(&exe),
            2,
            "flags 变化必须重编对象、重链 exe（否则仍是旧的 VALUE=1）"
        );
        assert_ne!(mtime(&obj), obj_t, "flags 变化必须重编对象");
        fs::remove_dir_all(dir).ok();
    }

    /// link_flags 只影响链接：exe 必须更新，对象必须原封不动。
    #[test]
    fn link_flag_change_relinks_without_recompiling() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("lflags");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}\n").unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n[compiler]\nlink_flags = []\n",
        )
        .unwrap();

        let exe = build_project(&dir).unwrap();
        let obj = obj_path(&dir, "main");
        let (obj_t, exe_t) = (mtime(&obj), mtime(&exe));
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n[compiler]\nlink_flags = [\"-lm\"]\n",
        )
        .unwrap();

        let exe = build_project(&dir).unwrap();
        assert_ne!(mtime(&exe), exe_t, "link_flags 变化必须重链 exe");
        assert_eq!(mtime(&obj), obj_t, "link_flags 变化不应重编对象");
        fs::remove_dir_all(dir).ok();
    }

    /// 删源文件后对象集收缩：即使剩下的对象都旧于 exe，也必须重链，
    /// 否则旧 exe 里还藏着已删除翻译单元的代码。
    #[test]
    fn deleting_source_forces_relink() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("delsrc");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(
            dir.join("src/main.cpp"),
            "#include <cstdio>\nint main(){ std::printf(\"main\\n\"); return 0; }\n",
        )
        .unwrap();
        // 独立的静态初始化翻译单元：main 不引用它，删掉后旧对象不会被链接器拒绝
        fs::write(
            dir.join("src/helper.cpp"),
            "#include <cstdio>\nstruct Helper { Helper(){ std::printf(\"helper\\n\"); } };\nstatic Helper h;\n",
        )
        .unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();

        let exe = build_project(&dir).unwrap();
        let first = dir.join("first.exe");
        fs::copy(&exe, &first).unwrap();
        assert!(run_stdout(&first).contains("helper"), "首次构建应包含 helper");
        let main_obj_t = mtime(&obj_path(&dir, "main"));
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::remove_file(dir.join("src/helper.cpp")).unwrap();

        let exe = build_project(&dir).unwrap();
        let out = run_stdout(&exe);
        assert!(
            !out.contains("helper"),
            "删源后必须重链，旧 exe 仍含已删除的翻译单元: {out:?}"
        );
        assert_eq!(mtime(&obj_path(&dir, "main")), main_obj_t, "未变的源不应重编");
        fs::remove_dir_all(dir).ok();
    }

    // ── v0.2.1：现代 C++ 结构（递归 src/ + include/ 自动 -I） ──

    #[test]
    fn collect_sources_recurses_nested_dirs() {
        let dir = temp_dir("nested");
        fs::create_dir_all(dir.join("src/cli")).unwrap();
        fs::create_dir_all(dir.join("src/core/deep")).unwrap();
        fs::write(dir.join("src/cli/main.cpp"), "//").unwrap();
        fs::write(dir.join("src/core/util.cc"), "//").unwrap();
        fs::write(dir.join("src/core/deep/x.cxx"), "//").unwrap();
        fs::write(dir.join("src/notes.txt"), "ignore").unwrap();
        let s = collect_sources(&dir, None).unwrap();
        assert_eq!(s.len(), 3, "应递归收集三层源文件: {s:?}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn nested_stem_conflict_detected() {
        let dir = temp_dir("nestedconflict");
        fs::create_dir_all(dir.join("src/a")).unwrap();
        fs::create_dir_all(dir.join("src/b")).unwrap();
        fs::write(dir.join("src/a/core.cpp"), "//").unwrap();
        fs::write(dir.join("src/b/core.cpp"), "//").unwrap();
        let err = collect_sources(&dir, None).unwrap_err();
        assert!(err.contains("冲突"), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn include_dir_is_on_include_path() {
        let dir = temp_dir("incflags");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("include")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}").unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname=\"t\"\n").unwrap();
        let cfg = crate::config::load(&dir).unwrap();
        let cxx = Cxx { name: "g++".into(), version: "test".into() };
        let p = plan(&dir, &cfg, cxx, false).unwrap();
        let expected = format!("-I{}", dir.join("include").display());
        assert!(p.flags.contains(&expected), "flags 缺少 {expected}: {:?}", p.flags);
        fs::remove_dir_all(dir).ok();
    }

    /// 现代结构端到端：源在子目录、公有头在 include/，必须能编译通过并正确运行。
    #[test]
    fn nested_sources_use_include_dir() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("nested-e2e");
        fs::create_dir_all(dir.join("src/app")).unwrap();
        fs::create_dir_all(dir.join("include")).unwrap();
        fs::write(dir.join("include/greet.h"), "#pragma once\nint greet();\n").unwrap();
        fs::write(
            dir.join("src/app/main.cpp"),
            "#include \"greet.h\"\nint main(){ return greet() == 7 ? 0 : 1; }\n",
        )
        .unwrap();
        fs::write(
            dir.join("src/app/greet.cpp"),
            "#include \"greet.h\"\nint greet(){ return 7; }\n",
        )
        .unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();

        let exe = build_project(&dir).unwrap();
        assert_eq!(run_code(&exe), 0, "子目录源 + include/ 应正常构建运行");
        fs::remove_dir_all(dir).ok();
    }
}
