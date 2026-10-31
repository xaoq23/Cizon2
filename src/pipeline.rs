//! 构建管线：源收集 → 并行编译（%.d 增量） → 链接。
//!
//! 从 Cizon1 迁移的最小正确核心：mtime + `-MMD` 依赖文件级增量，std::thread 并行，
//! stderr 直传 —— 不迁移 PCH / 指纹缓存 / 错误格式化器。
//! v0.2.1 起按现代 C++ 结构收集：递归 src/，include/ 自动上 -I。

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Instant;

use crate::compiler::{self, Cxx};
use crate::config::{Config, TargetKind};
use crate::deps;

const SOURCE_EXTS: &[&str] = &["cpp", "cc", "cxx"];

#[derive(Debug)]
struct LibTarget {
    name: String,
    sources: Vec<PathBuf>,
}

#[derive(Debug)]
pub struct Plan {
    cxx: Cxx,
    /// bin 的源文件（相对项目根）
    sources: Vec<PathBuf>,
    /// 编译期 flags（edition + 优化级 + 用户 flags + 依赖 include 路径）
    flags: Vec<String>,
    /// 链接期附加 flags（用户 link_flags；用户 flags 已在 flags 中，不重复传）
    link_flags: Vec<String>,
    /// 依赖源的基础编译 flags（edition + 优化级；不套用户 flags，免得 -Werror 误伤三方代码）
    dep_base: Vec<String>,
    /// 每个依赖自己的 -I，保证依赖源优先找到自己的头
    dep_includes: BTreeMap<String, Vec<String>>,
    /// 静态库 target；按配置顺序构建
    libs: Vec<LibTarget>,
    /// bin 要链接的静态库名，按声明顺序
    links: Vec<String>,
    out_name: String,
    /// "debug" / "release" —— 与编译器一起决定产物作用域
    mode: &'static str,
}

impl Plan {
    /// 依赖源的编译 flags：基础参数 + 自己的 -I（优先）+ 其他依赖的 -I。
    fn dep_flags_for(&self, name: &str) -> Vec<String> {
        let mut flags = self.dep_base.clone();
        if let Some(own) = self.dep_includes.get(name) {
            flags.extend(own.iter().cloned());
        }
        for (other, includes) in &self.dep_includes {
            if other != name {
                flags.extend(includes.iter().cloned());
            }
        }
        flags
    }
}

/// 为一次构建装配计划：失败即中止，不进入编译阶段。
pub fn plan(dir: &Path, cfg: &Config, cxx: Cxx, release: bool) -> Result<Plan, String> {
    let (sources, libs, links, out_name) = if let Some(targets) = cfg.targets() {
        let bin = targets
            .iter()
            .find(|t| t.kind == TargetKind::Bin)
            .expect("config validation guarantees exactly one bin");
        let sources = collect_target_sources(
            dir,
            bin.sources.as_deref(),
            &format!("bin target {}", bin.name),
        )?;
        if sources.is_empty() {
            return Err(format!("bin target {} 没有可编译的源文件。", bin.name));
        }
        let mut libs = Vec::new();
        for target in targets.iter().filter(|t| t.kind == TargetKind::StaticLib) {
            let lib_sources = collect_target_sources(
                dir,
                target.sources.as_deref(),
                &format!("静态库 target {}", target.name),
            )?;
            if lib_sources.is_empty() {
                return Err(format!(
                    "静态库 target {} 没有可编译的源文件。",
                    target.name
                ));
            }
            libs.push(LibTarget {
                name: target.name.clone(),
                sources: lib_sources,
            });
        }
        let links = bin
            .links
            .iter()
            .map(|link| {
                libs.iter()
                    .find(|lib| lib.name.eq_ignore_ascii_case(link))
                    .map(|lib| lib.name.clone())
                    .unwrap_or_else(|| link.clone())
            })
            .collect();
        (sources, libs, links, exe_name(&bin.name))
    } else {
        let sources = collect_sources(dir, cfg.explicit_sources())?;
        if sources.is_empty() {
            return Err(format!(
                "{} 下没有 .cpp 文件。在 src/ 放入源代码，或在 [project] sources = [...] 显式列出。",
                dir.join("src").display()
            ));
        }
        (sources, Vec::new(), Vec::new(), exe_name(&cfg.name(dir)))
    };

    let edition = compiler::edition_flag(cfg.edition());
    let optimization = if release { "-O2" } else { "-g" };
    let mut flags = vec![edition.clone(), optimization.into()];
    flags.extend(cfg.flags().iter().cloned());
    // 公有头目录：现代结构的 include/ 自动上 -I；目录不存在时编译器安全忽略
    flags.push(format!("-I{}", dir.join("include").display()));
    // 依赖头文件路径无条件进 flags：目录不存在时编译器安全忽略，
    // 因此 planning 无需网络操作
    let mut dep_includes: BTreeMap<String, Vec<String>> = BTreeMap::new();
    if let Some(map) = cfg.dependencies() {
        for name in map.keys() {
            let inc = dir.join("deps").join(name);
            let include = format!("-I{}", inc.join("include").display());
            let root = format!("-I{}", inc.display());
            flags.push(include.clone());
            flags.push(root.clone());
            dep_includes.insert(name.clone(), vec![include, root]);
        }
    }
    let dep_base = vec![edition, optimization.into()];

    let link_flags: Vec<String> = cfg.link_flags().to_vec();

    Ok(Plan {
        cxx,
        sources,
        flags,
        link_flags,
        dep_base,
        dep_includes,
        libs,
        links,
        out_name,
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

/// 输出用路径：以项目根为基准；不在其下时退回原路径。
pub(crate) fn rel_display(base: &Path, path: &Path) -> String {
    path.strip_prefix(base).unwrap_or(path).display().to_string()
}

/// 删除目录（不存在视为成功）；失败必须上报，否则旧对象会静默存活。
fn reset_dir(path: &Path) -> Result<(), String> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("无法清空 {}: {e}", path.display())),
    }
}

fn state_current(path: &Path, wanted: &str) -> Result<bool, String> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(t == wanted),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("读取 {} 失败: {e}", path.display())),
    }
}

/// 项目源对象在 obj/ 根层；依赖对象在 obj/<dep>/ 子目录。
fn clear_project_objects(obj_dir: &Path) -> Result<(), String> {
    let entries = match std::fs::read_dir(obj_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("无法读取 {}: {e}", obj_dir.display())),
    };
    for entry in entries {
        let entry = entry.map_err(|e| format!("无法读取 {} 的目录项: {e}", obj_dir.display()))?;
        let path = entry.path();
        let is_file = entry
            .file_type()
            .map_err(|e| format!("无法读取 {} 的类型: {e}", path.display()))?
            .is_file();
        if is_file {
            std::fs::remove_file(&path)
                .map_err(|e| format!("删除 {} 失败: {e}", path.display()))?;
        }
    }
    Ok(())
}

fn clear_dep_objects(obj_dir: &Path) -> Result<(), String> {
    let entries = match std::fs::read_dir(obj_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("无法读取 {}: {e}", obj_dir.display())),
    };
    for entry in entries {
        let entry = entry.map_err(|e| format!("无法读取 {} 的目录项: {e}", obj_dir.display()))?;
        let path = entry.path();
        let is_dir = entry
            .file_type()
            .map_err(|e| format!("无法读取 {} 的类型: {e}", path.display()))?
            .is_dir();
        if is_dir {
            reset_dir(&path)?;
        }
    }
    Ok(())
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

/// 源清单只接受项目内相对路径，避免编译项目外文件或把 build/deps 扫进来。
fn resolve_source_path(dir: &Path, raw: &str, owner: &str) -> Result<PathBuf, String> {
    if raw.trim().is_empty() {
        return Err(format!("{owner} sources 不能包含空路径。"));
    }
    let rel = Path::new(raw);
    let bad_component = rel.components().any(|c| {
        !matches!(
            c,
            std::path::Component::Normal(_) | std::path::Component::CurDir
        )
    });
    if rel.is_absolute() || bad_component {
        return Err(format!(
            "{owner} sources 必须是项目内相对路径（不能是绝对路径或包含 ..）: {raw}"
        ));
    }
    Ok(dir.join(rel))
}

/// 显式 sources 或递归扫描 src/。同名 stem 冲突立刻报错（对象文件按 stem 命名）。
fn collect_sources(dir: &Path, explicit: Option<&[String]>) -> Result<Vec<PathBuf>, String> {
    let mut out: Vec<PathBuf> = Vec::new();
    match explicit {
        Some(list) => {
            for raw in list {
                let path = resolve_source_path(dir, raw, "[project].sources")?;
                if path.is_dir() {
                    if std::fs::canonicalize(&path).ok() == std::fs::canonicalize(dir).ok() {
                        return Err("[project].sources 不能指向项目根目录。".into());
                    }
                    collect_recursive(&path, &mut out)?;
                } else if path.is_file() {
                    check_source_ext(&path)?;
                    out.push(path);
                } else {
                    return Err(format!(
                        "[project].sources 列出的路径不存在: {}",
                        path.display()
                    ));
                }
            }
            out.sort();
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
    let owner = if explicit.is_some() {
        "[project].sources"
    } else {
        "src/"
    };
    check_stems(&out, owner)?;
    Ok(out)
}

/// target 的 sources：文件或目录（目录递归扫描）；bin 缺省扫描 src/。
fn collect_target_sources(
    dir: &Path,
    list: Option<&[String]>,
    owner: &str,
) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    match list {
        None => {
            let src = dir.join("src");
            if !src.is_dir() {
                return Err(format!("未找到 src/ 目录（或无权读取）: {}", src.display()));
            }
            collect_recursive(&src, &mut out)?;
        }
        Some(entries) => {
            for raw in entries {
                let path = resolve_source_path(dir, raw, owner)?;
                if path.is_dir() {
                    if std::fs::canonicalize(&path).ok() == std::fs::canonicalize(dir).ok() {
                        return Err(format!("{owner} sources 不能指向项目根目录。"));
                    }
                    collect_recursive(&path, &mut out)?;
                } else if path.is_file() {
                    check_source_ext(&path)?;
                    out.push(path);
                } else {
                    return Err(format!("{owner} sources 列出的路径不存在: {}", path.display()));
                }
            }
        }
    }
    out.sort();
    check_stems(&out, owner)?;
    Ok(out)
}

/// 同一 target 内 stem 必须唯一（obj 按 stem 命名）；Windows 大小写不敏感。
fn check_stems(sources: &[PathBuf], owner: &str) -> Result<(), String> {
    let mut stems: HashMap<String, PathBuf> = HashMap::new();
    for src in sources {
        let stem = src.file_stem().unwrap().to_string_lossy().to_string();
        if let Some(prev) = stems.insert(stem.to_ascii_lowercase(), src.clone()) {
            if prev == *src {
                return Err(format!("{owner} 重复列出: {}", src.display()));
            }
            return Err(format!(
                "{owner} 的源文件对象名冲突（同 stem）:\n  {}\n  {}\n请改名；同一 target 内源文件 stem 必须唯一。",
                prev.display(),
                src.display()
            ));
        }
    }
    Ok(())
}

fn is_source(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| SOURCE_EXTS.iter().any(|ext| ext.eq_ignore_ascii_case(e)))
}

/// 递归收集目录下的源文件。不跟进符号链接目录（防环），链接的源文件照收。
fn collect_recursive(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("无法读取 {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("无法读取 {} 的目录项: {e}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| format!("无法读取 {} 的类型: {e}", path.display()))?;
        if file_type.is_dir() {
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

/// 一个待编译单元：项目源用 plan.flags，依赖源用各自的 dep flags（不套用户 flags）。
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
    let marker_state = match std::fs::read_to_string(&marker) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("读取 {} 失败: {e}", marker.display())),
    };
    if marker_state.as_deref() != Some(plan.cxx.version.as_str()) {
        reset_dir(&scope_dir)?;
    }
    let obj_dir = scope_dir.join("obj");
    // 编译参数变化不触碰任何 mtime，不主动作废对象就会静默复用旧行为的 .o。
    // 项目、依赖、库分开记录：改用户 flags 重编项目与库；依赖 flags 只影响依赖。
    let project_state = scope_dir.join(".project-state");
    let deps_state = scope_dir.join(".deps-state");
    let libs_state = scope_dir.join(".libs-state");
    let wanted_project = project_state_text(plan);
    let wanted_deps = deps_state_text(plan);
    let wanted_libs = libs_state_text(plan);
    if !state_current(&project_state, &wanted_project)? {
        clear_project_objects(&obj_dir)?;
        // 项目 flags 同样作用于库源
        clear_lib_artifacts(&scope_dir)?;
    }
    if !state_current(&deps_state, &wanted_deps)? {
        clear_dep_objects(&obj_dir)?;
    }
    if !state_current(&libs_state, &wanted_libs)? {
        clear_lib_artifacts(&scope_dir)?;
    }
    std::fs::create_dir_all(&obj_dir)
        .map_err(|e| format!("无法创建 {} : {e}", obj_dir.display()))?;
    for lib in &plan.libs {
        let lib_obj = lib_obj_dir(&scope_dir, &lib.name);
        std::fs::create_dir_all(&lib_obj)
            .map_err(|e| format!("无法创建 {}: {e}", lib_obj.display()))?;
    }
    std::fs::write(&project_state, &wanted_project)
        .map_err(|e| format!("写 {} 失败: {e}", project_state.display()))?;
    std::fs::write(&deps_state, &wanted_deps)
        .map_err(|e| format!("写 {} 失败: {e}", deps_state.display()))?;
    std::fs::write(&libs_state, &wanted_libs)
        .map_err(|e| format!("写 {} 失败: {e}", libs_state.display()))?;
    std::fs::write(&marker, &plan.cxx.version)
        .map_err(|e| format!("写 {} 失败: {e}", marker.display()))?;
    cleanup_stale_exe(&scope_dir, plan);

    // 0) 依赖契约：restore / 校验。目录缺失按锁恢复，与锁不一致当 loud 报错。
    let dep_jobs = deps::prepare(dir, cfg, &obj_dir)?;

    // 0.5) 编辑器支持：显式开启才写项目根的编译数据库
    if cfg.compile_commands() {
        write_compile_db(dir, plan, &dep_jobs)?;
    }

    // 1) 判定需要重编译的源
    let mut jobs: Vec<Job> = Vec::new(); // 项目源
    for src in &plan.sources {
        let obj = obj_for(src, &obj_dir);
        if needs_recompile(src, &obj) {
            jobs.push(Job { src: src.clone(), obj, flags: plan.flags.clone() });
        }
    }
    for (name, dsrc, dobj) in &dep_jobs {
        if needs_recompile(dsrc, dobj) {
            jobs.push(Job {
                src: dsrc.clone(),
                obj: dobj.clone(),
                flags: plan.dep_flags_for(name),
            });
        }
    }
    for lib in &plan.libs {
        for src in &lib.sources {
            let obj = lib_obj_for(&scope_dir, &lib.name, src);
            if needs_recompile(src, &obj) {
                jobs.push(Job {
                    src: src.clone(),
                    obj,
                    flags: plan.flags.clone(),
                });
            }
        }
    }
    let total = plan.sources.len()
        + dep_jobs.len()
        + plan.libs.iter().map(|l| l.sources.len()).sum::<usize>();
    let recompiled = jobs.len();
    if recompiled > 0 {
        let label = if recompiled == total { "Compiling" } else { "Rebuilding" };
        step_line(
            label,
            &format!(
                "{} {} [{recompiled}/{total} files]",
                plan.cxx.name,
                plan.cxx.short_version()
            ),
        );
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
    // 先输出成功翻译单元的警告：失败后下次只会重编失败文件，警告不能丢
    let mut all = std::io::stderr();
    for w in &warnings {
        let _ = all.write_all(w.as_bytes());
    }
    if !failures.is_empty() {
        let mut msg = String::new();
        for (src, stderr) in &failures {
            msg.push_str(&format!("编译失败: {}\n", rel_display(dir, src)));
            msg.push_str(stderr);
        }
        return Err(msg);
    }
    // 3) 静态库归档（对象有更新或归档缺失时重建；全部新鲜时不要求归档工具存在）
    let pending_archives: Vec<(PathBuf, Vec<PathBuf>)> = plan
        .libs
        .iter()
        .filter_map(|lib| {
            let lib_objs: Vec<PathBuf> = lib
                .sources
                .iter()
                .map(|src| lib_obj_for(&scope_dir, &lib.name, src))
                .collect();
            let archive = archive_path(&scope_dir, &lib.name);
            archive_stale(&archive, &lib_objs).then_some((archive, lib_objs))
        })
        .collect();
    let libs_rebuilt = !pending_archives.is_empty();
    if libs_rebuilt {
        let ar = compiler::archiver(&plan.cxx)?;
        for (archive, lib_objs) in &pending_archives {
            step_line("Archiving", &rel_display(dir, archive));
            archive_lib(&ar, archive, lib_objs)?;
        }
    }
    let archives: Vec<PathBuf> = plan
        .links
        .iter()
        .map(|name| archive_path(&scope_dir, name))
        .collect();

    // 4) 链接（无重编译、链接输入与参数未变且产物新鲜时跳过）
    let exe = scope_dir.join(&plan.out_name);
    let mut objs: Vec<PathBuf> = plan
        .sources
        .iter()
        .map(|src| obj_for(src, &obj_dir))
        .collect();
    objs.extend(dep_jobs.iter().map(|(_, _, obj)| obj.clone()));
    // 对象集收缩（删源）不产生任何 mtime 变化，靠这项状态强制重链。
    let link_state = scope_dir.join(".link-state");
    let wanted_link = link_state_text(plan, &objs, &archives);
    let link_current = std::fs::read_to_string(&link_state)
        .map_or(false, |t| t == wanted_link);
    if recompiled == 0
        && !libs_rebuilt
        && link_current
        && exe_is_fresh(&exe, &objs, &archives)
    {
        step_line(
            "Finished",
            &format!("up to date ({:.2}s)", started.elapsed().as_secs_f32()),
        );
        return Ok(exe);
    }
    step_line("Linking", &rel_display(dir, &exe));
    link(
        &plan.cxx.name,
        &objs,
        &archives,
        &exe,
        &plan.flags,
        &plan.link_flags,
    )
    .map_err(|e| format!("链接失败: {}\n{e}", rel_display(dir, &exe)))?;
    std::fs::write(&link_state, &wanted_link)
        .map_err(|e| format!("写 {} 失败: {e}", link_state.display()))?;

    step_fin(started, recompiled, total);
    Ok(exe)
}

/// 项目源的编译参数。逐行文本便于人眼 diff。
fn project_state_text(plan: &Plan) -> String {
    let mut s = String::from("project:\n");
    for f in &plan.flags {
        s.push_str(f);
        s.push('\n');
    }
    s
}

/// 依赖源的编译参数（基础 flags + 每个依赖自己的 include 集）。
fn deps_state_text(plan: &Plan) -> String {
    let mut s = String::from("dep-base:\n");
    for f in &plan.dep_base {
        s.push_str(f);
        s.push('\n');
    }
    for (name, includes) in &plan.dep_includes {
        s.push_str(&format!("dep {name}:\n"));
        for f in includes {
            s.push_str(f);
            s.push('\n');
        }
    }
    s
}

/// 静态库 target 的源清单；变化时清空库对象与归档。
fn libs_state_text(plan: &Plan) -> String {
    let mut s = String::from("libs:\n");
    for lib in &plan.libs {
        s.push_str(&format!("lib {}:\n", lib.name));
        for src in &lib.sources {
            s.push_str(&src.display().to_string());
            s.push('\n');
        }
    }
    s
}

/// 链接状态：exe 名、对象、参与链接的归档、链接参数。任一项变化即需重链。
fn link_state_text(plan: &Plan, objs: &[PathBuf], archives: &[PathBuf]) -> String {
    let mut s = format!("exe: {}\n", plan.out_name);
    s.push_str("objects:\n");
    for obj in objs {
        s.push_str(&obj.display().to_string());
        s.push('\n');
    }
    s.push_str("archives:\n");
    for archive in archives {
        s.push_str(&archive.display().to_string());
        s.push('\n');
    }
    s.push_str("link-flags:\n");
    for f in &plan.link_flags {
        s.push_str(f);
        s.push('\n');
    }
    s
}

/// 写 compile_commands.json：bin/库源用 plan.flags，依赖源用各自的 dep_flags_for。
/// 手写 JSON，不引入 serde_json（依赖白名单）。
fn write_compile_db(
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
        std::fs::write(&path, json).map_err(|e| format!("写 {} 失败: {e}", path.display()))?;
    }
    Ok(())
}

fn render_compile_db(dir: &Path, cxx: &str, entries: &[(&Path, &[String])]) -> String {
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

fn json_escape(s: &str) -> String {
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

/// bin 改名后清理旧 exe 与 Windows 链接侧产物。
/// 失败只警告：旧 exe 可能正在运行，不应阻断新产物构建。
fn cleanup_stale_exe(scope_dir: &Path, plan: &Plan) {
    let old = std::fs::read_to_string(scope_dir.join(".link-state"))
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                line.strip_prefix("exe: ")
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_string)
            })
        });
    if let Some(old) = old {
        if old != plan.out_name {
            let path = scope_dir.join(&old);
            remove_artifact(&path);
            remove_artifact(&path.with_extension("pdb"));
            remove_artifact(&path.with_extension("ilk"));
        }
    }

    // 旧版本的 .link-state 没有 exe 行：Windows 上按扩展名清理顶层其他产物。
    #[cfg(windows)]
    {
        let current_stem = Path::new(&plan.out_name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if let Ok(entries) = std::fs::read_dir(scope_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
                    continue;
                };
                if !["exe", "pdb", "ilk"]
                    .iter()
                    .any(|x| x.eq_ignore_ascii_case(ext))
                {
                    continue;
                }
                let stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if stem != current_stem {
                    remove_artifact(&path);
                }
            }
        }
    }
}

fn remove_artifact(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!("警告: 清理旧产物 {} 失败: {e}", path.display()),
    }
}

/// exe 是否不旧于所有对象与静态库归档。
fn exe_is_fresh(exe: &Path, objs: &[PathBuf], archives: &[PathBuf]) -> bool {
    let Ok(exe_time) = std::fs::metadata(exe).and_then(|m| m.modified()) else {
        return false;
    };
    objs.iter()
        .chain(archives.iter())
        .all(|input| {
            std::fs::metadata(input)
                .and_then(|m| m.modified())
                .map_or(false, |t| t <= exe_time)
        })
}

fn obj_for(src: &Path, obj_dir: &Path) -> PathBuf {
    let stem = src.file_stem().unwrap().to_string_lossy();
    obj_dir.join(format!("{stem}.o"))
}

fn lib_obj_dir(scope_dir: &Path, name: &str) -> PathBuf {
    scope_dir.join("lib").join(name).join("obj")
}

fn lib_obj_for(scope_dir: &Path, name: &str, src: &Path) -> PathBuf {
    let stem = src.file_stem().unwrap().to_string_lossy();
    lib_obj_dir(scope_dir, name).join(format!("{stem}.o"))
}

fn archive_path(scope_dir: &Path, name: &str) -> PathBuf {
    scope_dir
        .join("lib")
        .join(name)
        .join(format!("lib{name}.a"))
}

/// 项目 flags 或库源清单变化时，库对象与归档一起作废。
fn clear_lib_artifacts(scope_dir: &Path) -> Result<(), String> {
    reset_dir(&scope_dir.join("lib"))
}

fn archive_stale(archive: &Path, objs: &[PathBuf]) -> bool {
    let Ok(archive_time) = std::fs::metadata(archive).and_then(|m| m.modified()) else {
        return true;
    };
    objs.iter().any(|obj| {
        std::fs::metadata(obj)
            .and_then(|m| m.modified())
            .map_or(true, |t| t > archive_time)
    })
}

fn archive_lib(ar: &str, archive: &Path, objs: &[PathBuf]) -> Result<(), String> {
    if objs.is_empty() {
        return Err(format!("静态库 {} 没有对象文件。", archive.display()));
    }
    let tmp = temp_sibling(archive);
    let _ = std::fs::remove_file(&tmp);
    let out = Command::new(ar)
        .arg("rcs")
        .arg(&tmp)
        .args(objs)
        .output()
        .map_err(|e| format!("启动归档工具 {ar} 失败: {e}"))?;
    if !out.status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!(
            "归档失败: {} {}\n{}",
            ar,
            archive.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    replace_file(&tmp, archive)
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
        Ok(inputs) if !inputs.is_empty() => {
            // 源清单切换后 stem 可能不变：旧 .d 不包含当前 src 时必须重编。
            if !inputs.iter().any(|p| same_path(p, src)) {
                return true;
            }
            inputs.iter().any(|p| stale(p))
        }
        _ => true,
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn dep_file_for(obj: &Path) -> PathBuf {
    PathBuf::from(format!("{}.d", obj.display()))
}

fn temp_sibling(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.tmp", path.display()))
}

fn replace_file(tmp: &Path, dest: &Path) -> Result<(), String> {
    if dest.exists() {
        std::fs::remove_file(dest)
            .map_err(|e| format!("替换 {} 前删除旧文件失败: {e}", dest.display()))?;
    }
    std::fs::rename(tmp, dest).map_err(|e| {
        let _ = std::fs::remove_file(tmp);
        format!("替换 {} 失败: {e}", dest.display())
    })
}

/// 解析 gcc/clang 的 depfile（`target: dep dep dep ...`），
/// 首行目标后的所有输入；处理 `\` 续行、`\ ` 转义空格与 Windows 路径分隔符。
/// `-MP` 生成的孤行（如 `foo.h:`）因 token 以 ':' 结尾而被安全跳过。
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
    Ok(split_dep_tokens(deps)
        .into_iter()
        .filter(|s| !s.ends_with(':'))
        .map(|s| {
            if cfg!(target_os = "windows") {
                PathBuf::from(s.replace('/', "\\"))
            } else {
                PathBuf::from(s)
            }
        })
        .collect())
}

/// make 风格 token 切分：空白分隔，`\ ` 表示字面空格，其余反斜杠保留。
fn split_dep_tokens(s: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&' ') => {
                current.push(' ');
                chars.next();
            }
            c if c.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// 单文件编译。Ok 表示成功（Some = 警告输出），Err = 失败（完整 stderr）。
/// 对象与 .d 先写到临时文件，成功后再替换，避免半成品被下一次构建当成新鲜产物。
fn compile_one(
    cxx: &str,
    src: &Path,
    obj: &Path,
    flags: &[String],
) -> Result<Option<String>, String> {
    let dep_file = dep_file_for(obj);
    let tmp_obj = temp_sibling(obj);
    let tmp_dep = temp_sibling(&dep_file);
    let _ = std::fs::remove_file(&tmp_obj);
    let _ = std::fs::remove_file(&tmp_dep);
    let mut cmd = Command::new(cxx);
    cmd.arg("-c").arg(src).arg("-o").arg(&tmp_obj);
    cmd.arg("-MMD").arg("-MF").arg(&tmp_dep).arg("-MP");
    for f in flags {
        cmd.arg(f);
    }
    let out = match cmd.output() {
        Ok(out) => out,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_obj);
            let _ = std::fs::remove_file(&tmp_dep);
            return Err(format!("启动编译器 {cxx} 失败: {e}"));
        }
    };
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    if !out.status.success() {
        let _ = std::fs::remove_file(&tmp_obj);
        let _ = std::fs::remove_file(&tmp_dep);
        return Err(stderr);
    }
    replace_file(&tmp_obj, obj)?;
    replace_file(&tmp_dep, &dep_file)?;
    if stderr.trim().is_empty() {
        Ok(None)
    } else {
        Ok(Some(stderr))
    }
}

fn link(
    cxx: &str,
    objs: &[PathBuf],
    archives: &[PathBuf],
    exe: &Path,
    flags: &[String],
    link_flags: &[String],
) -> Result<(), String> {
    let mut cmd = Command::new(cxx);
    for obj in objs {
        cmd.arg(obj);
    }
    for archive in archives {
        cmd.arg(archive);
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
        assert_eq!(p.flags.iter().filter(|f| *f == "-Wall").count(), 1);
        assert!(!p.flags.iter().any(|f| f == "-lm"));
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
    fn rel_display_strips_project_root() {
        let base = Path::new("proj");
        let inside = base.join("build").join("app.exe");
        assert_eq!(
            rel_display(base, &inside),
            Path::new("build").join("app.exe").display().to_string()
        );
        let outside = Path::new("elsewhere").join("app.exe");
        assert_eq!(rel_display(base, &outside), outside.display().to_string());
    }

    #[test]
    fn reset_dir_reports_failure() {
        let dir = temp_dir("reset-file");
        let file = dir.join("not-a-dir");
        fs::write(&file, "").unwrap();
        let err = reset_dir(&file).unwrap_err();
        assert!(err.contains("无法清空"), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn split_state_clears_only_its_objects() {
        let dir = temp_dir("split-clear");
        let obj = dir.join("obj");
        fs::create_dir_all(obj.join("dep")).unwrap();
        fs::write(obj.join("main.o"), "").unwrap();
        fs::write(obj.join("dep/dep.o"), "").unwrap();
        clear_project_objects(&obj).unwrap();
        assert!(!obj.join("main.o").exists());
        assert!(obj.join("dep/dep.o").exists(), "项目 flags 变化不应删依赖对象");
        clear_dep_objects(&obj).unwrap();
        assert!(!obj.join("dep").exists());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn dep_include_order_prefers_own_dependency() {
        let mut includes = BTreeMap::new();
        includes.insert("a".to_string(), vec!["-I/a".to_string()]);
        includes.insert(
            "b".to_string(),
            vec!["-I/b/include".to_string(), "-I/b".to_string()],
        );
        let p = Plan {
            cxx: Cxx { name: "g++".into(), version: "test".into() },
            sources: vec![],
            flags: vec![],
            link_flags: vec![],
            dep_base: vec!["-std=c++23".to_string()],
            dep_includes: includes,
            libs: vec![],
            links: vec![],
            out_name: "t".into(),
            mode: "debug",
        };
        let flags = p.dep_flags_for("b");
        let pos = |needle: &str| flags.iter().position(|f| f == needle).unwrap();
        assert!(pos("-I/b/include") < pos("-I/a"), "自己的头必须排在其他依赖之前");
        assert!(pos("-I/b") < pos("-I/a"));
    }

    #[test]
    fn collect_sources_from_src_dir() {
        let dir = temp_dir("collect");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}").unwrap();
        fs::write(dir.join("src/util.cc"), "//").unwrap();
        fs::write(dir.join("src/notes.txt"), "ignore").unwrap();
        let s = collect_sources(&dir, None).unwrap();
        let names: Vec<String> = s
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["main.cpp", "util.cc"]);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn collects_uppercase_source_extension() {
        let dir = temp_dir("upper-ext");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/Util.CPP"), "//").unwrap();
        let s = collect_sources(&dir, None).unwrap();
        assert_eq!(s.len(), 1, "Windows 上大写扩展名不能漏收");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn explicit_sources_resolve_and_validate() {
        let dir = temp_dir("explicit");
        fs::create_dir_all(dir.join("third_party")).unwrap();
        fs::write(dir.join("third_party/x.cpp"), "//").unwrap();
        let list = vec!["third_party/x.cpp".to_string()];
        let s = collect_sources(&dir, Some(&list)).unwrap();
        assert_eq!(s, vec![dir.join("third_party/x.cpp")]);

        let missing = vec!["missing.cpp".to_string()];
        let err = collect_sources(&dir, Some(&missing)).unwrap_err();
        assert!(err.contains("不存在"), "got: {err}");

        fs::write(dir.join("notes.txt"), "").unwrap();
        let bad_ext = vec!["notes.txt".to_string()];
        let err = collect_sources(&dir, Some(&bad_ext)).unwrap_err();
        assert!(err.contains("只接受"), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn explicit_sources_accept_directory() {
        let dir = temp_dir("explicit-dir");
        fs::create_dir_all(dir.join("app/sub")).unwrap();
        fs::write(dir.join("app/a.cpp"), "//").unwrap();
        fs::write(dir.join("app/sub/b.cc"), "//").unwrap();
        fs::write(dir.join("app/notes.txt"), "ignore").unwrap();
        let list = vec!["app".to_string()];
        let s = collect_sources(&dir, Some(&list)).unwrap();
        let names: Vec<String> = s
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["a.cpp", "b.cc"]);
        let root = vec![".".to_string()];
        let err = collect_sources(&dir, Some(&root)).unwrap_err();
        assert!(err.contains("项目根"), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn case_insensitive_stem_conflict_detected() {
        let dir = temp_dir("case-conflict");
        fs::create_dir_all(dir.join("src/a")).unwrap();
        fs::create_dir_all(dir.join("src/b")).unwrap();
        fs::write(dir.join("src/a/Main.cpp"), "//").unwrap();
        fs::write(dir.join("src/b/main.cpp"), "//").unwrap();
        let err = collect_sources(&dir, None).unwrap_err();
        assert!(err.contains("冲突"), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn duplicate_explicit_source_reported_as_duplicate() {
        let dir = temp_dir("dup-source");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "//").unwrap();
        let list = vec!["src/main.cpp".to_string(), "src/main.cpp".to_string()];
        let err = collect_sources(&dir, Some(&list)).unwrap_err();
        assert!(err.contains("重复列出"), "got: {err}");
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
    fn depfile_parsing_handles_escaped_spaces() {
        let dir = temp_dir("depfile-space");
        let f = dir.join("main.d");
        fs::write(
            &f,
            "main.o: C:\\path\\ with\\ space\\src\\main.cpp \\\n  C:\\path\\ with\\ space\\src\\h.h\n\n\
             C:\\path\\ with\\ space\\src\\h.h:\n",
        )
        .unwrap();
        let inputs = dep_inputs(&f).unwrap();
        let names: Vec<String> = inputs
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        assert_eq!(names.len(), 2, "空格必须留在 token 内: {names:?}");
        assert!(
            names.iter().any(|p| p.contains("with space")),
            "转义空格应还原: {names:?}"
        );
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
        // -g 在 GNU 与 MSVC 目标上都可接受；-lm 只在 MinGW/GNU 工具链存在
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n[compiler]\nlink_flags = [\"-g\"]\n",
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
        assert!(out.contains("main"), "重建后的程序应正常运行: {out:?}");
        assert!(
            !out.contains("helper"),
            "删源后必须重链，旧 exe 仍含已删除的翻译单元: {out:?}"
        );
        assert_eq!(run_code(&exe), 0);
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

    // ── v0.3：compile_commands.json（默认关闭，toml 显式开启） ──

    #[test]
    fn json_escape_handles_windows_paths_and_quotes() {
        assert_eq!(json_escape(r"C:\a\b"), r"C:\\a\\b");
        assert_eq!(json_escape("a\"b"), r#"a\"b"#);
        assert_eq!(json_escape("a\nb"), r"a\nb");
    }

    #[test]
    fn render_compile_db_emits_entries() {
        let flags_a = vec!["-std=c++23".to_string(), "-IX".to_string()];
        let flags_b = vec!["-O2".to_string()];
        let entries = [
            (Path::new("src/a.cpp"), flags_a.as_slice()),
            (Path::new("src/b.cpp"), flags_b.as_slice()),
        ];
        let json = render_compile_db(Path::new("proj"), "g++", &entries);
        assert!(json.starts_with("[\n  {"), "got: {json}");
        assert_eq!(json.matches("\"file\"").count(), 2);
        assert!(json.contains(
            "\"arguments\": [\"g++\", \"-c\", \"src/a.cpp\", \"-std=c++23\", \"-IX\"]"
        ));
        assert!(json.contains("},\n  {"), "条目之间必须有逗号: {json}");
        assert!(json.trim_end().ends_with(']'));
    }

    #[test]
    fn compile_db_absent_by_default() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("db-off");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}\n").unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();

        build_project(&dir).unwrap();
        assert!(
            !dir.join("compile_commands.json").exists(),
            "未开启时不应生成 compile_commands.json"
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn compile_db_written_when_enabled() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("db-on");
        fs::create_dir_all(dir.join("src/cli")).unwrap();
        fs::create_dir_all(dir.join("include")).unwrap();
        fs::write(dir.join("include/greet.h"), "#pragma once\n").unwrap();
        fs::write(
            dir.join("src/cli/main.cpp"),
            "#include \"greet.h\"\nint main(){return 0;}\n",
        )
        .unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\nedition = \"c++17\"\ncompile_commands = true\n\n\
             [compiler]\nflags = [\"-Wall\"]\n",
        )
        .unwrap();

        build_project(&dir).unwrap();
        let text = fs::read_to_string(dir.join("compile_commands.json")).unwrap();
        assert!(text.trim_start().starts_with('['), "应为 JSON 数组: {text}");
        assert_eq!(text.matches("\"file\"").count(), 1, "got: {text}");
        assert_eq!(text.matches("\"arguments\"").count(), 1, "got: {text}");
        let file = json_escape(
            &dir.join("src")
                .join("cli")
                .join("main.cpp")
                .display()
                .to_string(),
        );
        assert!(text.contains(&format!("\"file\": \"{file}\"")), "got: {text}");
        assert!(text.contains("-std=c++17"), "got: {text}");
        assert!(text.contains("-Wall"), "got: {text}");
        let include = json_escape(&format!("-I{}", dir.join("include").display()));
        assert!(text.contains(&format!("\"{include}\"")), "got: {text}");
        if cfg!(windows) {
            assert!(text.contains(r"\\"), "Windows 路径反斜杠必须转义: {text}");
        }
        let db = dir.join("compile_commands.json");
        let db_t = mtime(&db);
        std::thread::sleep(std::time::Duration::from_millis(50));
        build_project(&dir).unwrap();
        assert_eq!(mtime(&db), db_t, "内容未变不应重写 compile_commands.json");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn compile_failure_then_fix_recovers() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("recover");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(
            dir.join("src/main.cpp"),
            "int main(){ return does_not_exist; }\n",
        )
        .unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();

        let err = build_project(&dir).unwrap_err();
        assert!(err.contains("编译失败"), "got: {err}");

        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}\n").unwrap();
        let exe = build_project(&dir).unwrap();
        assert_eq!(run_code(&exe), 0);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn header_change_recompiles_only_dependent_tu() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("header-inc");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("include")).unwrap();
        fs::write(dir.join("include/util.h"), "int value();\n").unwrap();
        fs::write(
            dir.join("src/main.cpp"),
            "#include \"util.h\"\nint main(){ return value(); }\n",
        )
        .unwrap();
        fs::write(dir.join("src/util.cpp"), "int value(){ return 0; }\n").unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();

        build_project(&dir).unwrap();
        let main_obj = obj_path(&dir, "main");
        let util_obj = obj_path(&dir, "util");
        let (main_t, util_t) = (mtime(&main_obj), mtime(&util_obj));
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(
            dir.join("include/util.h"),
            "// changed\nint value();\n",
        )
        .unwrap();
        build_project(&dir).unwrap();
        assert_ne!(mtime(&main_obj), main_t, "包含头文件的 TU 必须重编");
        assert_eq!(mtime(&util_obj), util_t, "不包含该头的 TU 不应重编");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn missing_version_marker_invalidates_scope() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("marker-missing");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}\n").unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();
        build_project(&dir).unwrap();
        let obj = obj_path(&dir, "main");
        let obj_t = mtime(&obj);
        let scope = obj.parent().unwrap().parent().unwrap();
        fs::remove_file(scope.join(".compiler-version")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        build_project(&dir).unwrap();
        assert_ne!(mtime(&obj), obj_t, "版本指纹缺失时必须重建");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn build_aborts_when_object_dir_cannot_be_reset() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("obj-blocked");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}\n").unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n[compiler]\nflags = [\"-DVALUE=1\"]\n",
        )
        .unwrap();
        build_project(&dir).unwrap();
        let obj = obj_path(&dir, "main");
        let obj_dir = obj.parent().unwrap().to_path_buf();
        fs::remove_dir_all(&obj_dir).unwrap();
        fs::write(&obj_dir, "").unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n[compiler]\nflags = [\"-DVALUE=2\"]\n",
        )
        .unwrap();
        let err = build_project(&dir).unwrap_err();
        assert!(err.contains("无法读取"), "got: {err}");
        fs::remove_file(&obj_dir).ok();
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn compiler_version_change_invalidates_scope() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("compiler-version");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}\n").unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();
        let cfg = crate::config::load(&dir).unwrap();

        let mut cxx = crate::compiler::pick(None).unwrap();
        cxx.version = "test-v1".into();
        let p1 = plan(&dir, &cfg, cxx.clone(), false).unwrap();
        build(&dir, &cfg, &p1).unwrap();
        let obj = obj_path(&dir, "main");
        let obj_t = mtime(&obj);

        std::thread::sleep(std::time::Duration::from_millis(50));
        cxx.version = "test-v2".into();
        let p2 = plan(&dir, &cfg, cxx, false).unwrap();
        build(&dir, &cfg, &p2).unwrap();
        assert_ne!(mtime(&obj), obj_t, "编译器版本变化必须作废整个作用域");
        fs::remove_dir_all(dir).ok();
    }

    // ── v0.4：单 bin + 多静态库 ──────────────────────────

    fn target_toml(flags: Option<&str>) -> String {
        let compiler = flags
            .map(|f| format!("[compiler]\nflags = {f}\n\n"))
            .unwrap_or_default();
        format!(
            "[project]\nname = \"t\"\nedition = \"c++23\"\ncompile_commands = true\n\n\
             {compiler}[[target]]\nname = \"foo\"\nkind = \"static_lib\"\nsources = [\"lib\"]\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"foo\"]\n"
        )
    }

    fn target_project(tag: &str) -> PathBuf {
        let dir = temp_dir(tag);
        for sub in ["src", "lib", "include"] {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }
        fs::write(dir.join("include/foo.h"), "#pragma once\nint foo();\n").unwrap();
        fs::write(
            dir.join("lib/foo.cpp"),
            "#include \"foo.h\"\nint foo(){ return 1; }\n",
        )
        .unwrap();
        fs::write(
            dir.join("src/main.cpp"),
            "#include \"foo.h\"\nint main(){ return foo() == 1 ? 0 : 1; }\n",
        )
        .unwrap();
        fs::write(dir.join("cizon.toml"), target_toml(None)).unwrap();
        dir
    }

    fn lib_archive_path(dir: &Path, lib: &str) -> PathBuf {
        for scope in fs::read_dir(dir.join("build")).unwrap().flatten() {
            let path = scope
                .path()
                .join("lib")
                .join(lib)
                .join(format!("lib{lib}.a"));
            if path.exists() {
                return path;
            }
        }
        panic!("未找到 lib{lib}.a");
    }

    fn libs_supported() -> bool {
        have_compiler()
            && crate::compiler::pick(None)
                .ok()
                .is_some_and(|cxx| crate::compiler::archiver(&cxx).is_ok())
    }

    #[test]
    fn targets_build_static_lib_and_link() {
        if !libs_supported() {
            return;
        }
        let dir = target_project("lib-e2e");
        let exe = build_project(&dir).unwrap();
        assert_eq!(run_code(&exe), 0);
        assert!(lib_archive_path(&dir, "foo").is_file());
        let db = fs::read_to_string(dir.join("compile_commands.json")).unwrap();
        assert_eq!(db.matches("\"file\"").count(), 2, "bin+lib 两个源: {db}");
        assert!(db.contains("foo.cpp"), "got: {db}");
        assert!(db.contains("main.cpp"), "got: {db}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn lib_source_change_rebuilds_archive_without_recompiling_bin() {
        if !libs_supported() {
            return;
        }
        let dir = target_project("lib-inc");
        let exe = build_project(&dir).unwrap();
        let archive = lib_archive_path(&dir, "foo");
        let main_obj = obj_path(&dir, "main");
        let (archive_t, main_t, exe_t) = (mtime(&archive), mtime(&main_obj), mtime(&exe));
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(
            dir.join("lib/foo.cpp"),
            "#include \"foo.h\"\nint foo(){ return 2; }\n",
        )
        .unwrap();
        let exe = build_project(&dir).unwrap();
        assert_ne!(mtime(&archive), archive_t, "库源变化必须重建归档");
        assert_eq!(mtime(&main_obj), main_t, "库源变化不应重编 bin 源");
        assert_ne!(mtime(&exe), exe_t, "归档变化必须重链");
        // main 仍然检查 foo()==1，新库返回 2 → 退出码 1，证明链接的是新代码
        assert_eq!(run_code(&exe), 1, "exe 必须包含新库代码");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn bin_source_change_keeps_archive() {
        if !libs_supported() {
            return;
        }
        let dir = target_project("bin-inc");
        let exe = build_project(&dir).unwrap();
        let archive = lib_archive_path(&dir, "foo");
        let (archive_t, exe_t) = (mtime(&archive), mtime(&exe));
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(
            dir.join("src/main.cpp"),
            "#include \"foo.h\"\n// changed\nint main(){ return foo() == 1 ? 0 : 1; }\n",
        )
        .unwrap();
        let exe = build_project(&dir).unwrap();
        assert_eq!(mtime(&archive), archive_t, "只改 bin 不应重建归档");
        assert_ne!(mtime(&exe), exe_t);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn renaming_bin_target_removes_old_exe() {
        if !libs_supported() {
            return;
        }
        let dir = target_project("rename-bin");
        let old_exe = build_project(&dir).unwrap();
        assert!(old_exe.is_file());
        let old_pdb = old_exe.with_extension("pdb");
        let had_pdb = old_pdb.exists();

        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\nedition = \"c++23\"\n\n\
             [[target]]\nname = \"foo\"\nkind = \"static_lib\"\nsources = [\"lib\"]\n\n\
             [[target]]\nname = \"app2\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"foo\"]\n",
        )
        .unwrap();
        let new_exe = build_project(&dir).unwrap();
        assert!(!old_exe.exists(), "bin 改名后旧 exe 必须清理");
        if had_pdb {
            assert!(!old_pdb.exists(), "旧 exe 的 .pdb 也应清理");
        }
        assert!(new_exe.is_file());
        assert_eq!(run_code(&new_exe), 0);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn project_flag_change_rebuilds_library() {
        if !libs_supported() {
            return;
        }
        let dir = target_project("lib-flags");
        build_project(&dir).unwrap();
        let archive = lib_archive_path(&dir, "foo");
        let lib_obj = archive.parent().unwrap().join("obj/foo.o");
        let main_obj = obj_path(&dir, "main");
        let (archive_t, obj_t, main_t) = (mtime(&archive), mtime(&lib_obj), mtime(&main_obj));
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(dir.join("cizon.toml"), target_toml(Some("[\"-Wall\"]"))).unwrap();
        build_project(&dir).unwrap();
        assert_ne!(mtime(&lib_obj), obj_t, "项目 flags 变化应重编库源");
        assert_ne!(mtime(&main_obj), main_t, "项目 flags 变化应重编 bin 源");
        assert_ne!(mtime(&archive), archive_t, "库对象变化应重建归档");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn target_sources_reject_empty_entry() {
        let dir = target_project("lib-empty-src");
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"\"]\n",
        )
        .unwrap();
        let cfg = crate::config::load(&dir).unwrap();
        let cxx = Cxx { name: "g++".into(), version: "test".into() };
        let err = plan(&dir, &cfg, cxx, false).unwrap_err();
        assert!(err.contains("空路径"), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn target_sources_must_stay_inside_project() {
        let dir = target_project("lib-escape");
        let absolute = dir.join("src").to_string_lossy().into_owned();
        for bad in ["../outside", ".", absolute.as_str()] {
            fs::write(
                dir.join("cizon.toml"),
                format!(
                    "[project]\nname = \"t\"\n\n[[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [{bad:?}]\n"
                ),
            )
            .unwrap();
            let cfg = crate::config::load(&dir).unwrap();
            let cxx = Cxx { name: "g++".into(), version: "test".into() };
            let err = plan(&dir, &cfg, cxx, false).unwrap_err();
            assert!(
                err.contains("相对路径") || err.contains("项目根"),
                "source={bad:?} got: {err}"
            );
        }
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn bin_source_switch_with_same_stem_recompiles() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("bin-switch");
        fs::create_dir_all(dir.join("src/a")).unwrap();
        fs::create_dir_all(dir.join("src/b")).unwrap();
        fs::write(dir.join("src/a/main.cpp"), "int main(){ return 0; }\n").unwrap();
        fs::write(dir.join("src/b/main.cpp"), "int main(){ return 1; }\n").unwrap();
        let config = |sources: &str| {
            format!(
                "[project]\nname = \"t\"\n\n[[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [{sources:?}]\n"
            )
        };
        fs::write(dir.join("cizon.toml"), config("src/a")).unwrap();
        let exe = build_project(&dir).unwrap();
        assert_eq!(run_code(&exe), 0);

        // b 的 mtime 早于旧 main.o；旧 .d 不包含它，必须重编
        fs::write(dir.join("cizon.toml"), config("src/b")).unwrap();
        let exe = build_project(&dir).unwrap();
        assert_eq!(run_code(&exe), 1, "切换同 stem 的 bin 源后必须使用新源");
        fs::remove_dir_all(dir).ok();
    }
}
