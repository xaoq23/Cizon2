//! 构建管线：源收集 → 并行编译（%.d 增量） → 链接。
//!
//! 从 Cizon1 迁移的最小正确核心：mtime + `-MMD` 依赖文件级增量，std::thread 并行，
//! stderr 直传 —— 不迁移 PCH / 指纹缓存 / 错误格式化器。

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

/// 显式 sources 或顺序扫描 src/。同名 stem 冲突立刻报错（对象文件按 stem 命名）。
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
            let entries = std::fs::read_dir(&src)
                .map_err(|_| format!("未找到 src/ 目录（或无权读取）: {}", src.display()))?;
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() && is_source(&path) {
                    out.push(path);
                }
            }
            out.sort();
        }
    }
    // stem 冲突检查：obj = build/<scope>/obj/<stem>.o，两个同名 stem 会互相覆盖。
    let mut stems: HashMap<String, PathBuf> = HashMap::new();
    for src in &out {
        let stem = src.file_stem().unwrap().to_string_lossy().to_string();
        if let Some(prev) = stems.insert(stem.clone(), src.clone()) {
            return Err(format!(
                "两个源文件的编译产物名冲突（同 stem）:\n  {}\n  {}\n请改名，或让 src/ 只平铺源文件，或在 [project].sources 显式排重。",
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
    std::fs::create_dir_all(&obj_dir)
        .map_err(|e| format!("无法创建 {} : {e}", obj_dir.display()))?;
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
    // 3) 链接（无重编译且产物已新鲜时跳过）
    let exe = scope_dir.join(&plan.out_name);
    let mut objs: Vec<PathBuf> = plan
        .sources
        .iter()
        .map(|src| obj_for(src, &obj_dir))
        .collect();
    objs.extend(dep_jobs.iter().map(|(_, obj)| obj.clone()));
    if recompiled == 0 && exe_is_fresh(&exe, &objs) {
        step_line(
            "Finished",
            &format!("up to date ({:.2}s)", started.elapsed().as_secs_f32()),
        );
        return Ok(exe);
    }
    step_line("Linking", &format!("{} ({})", exe.display(), plan.cxx.short_version()));
    link(&plan.cxx.name, &objs, &exe, &plan.flags, &plan.link_flags)?;

    step_fin(started, recompiled, total);
    Ok(exe)
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
}
