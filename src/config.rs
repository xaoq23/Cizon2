//! cizon.toml 解析。
//!
//! 当前边界：只认 `[project]`、`[dependencies]`、`[compiler]` 与 `[[target]]` 四段。
//! 其他段一律明确报错，绝不静默忽略 —— 拼写错误和"等未来版本来支持"必须当场暴露，
//! 而不是构建出一个看起来成功实际没加依赖的产物。

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Deserialize, Default)]
pub struct Config {
    pub project: Option<Project>,
    pub dependencies: Option<BTreeMap<String, DepInfo>>,
    pub compiler: Option<CompilerSection>,
    /// `[[target]]`：单 bin + 多静态库；缺省时保持隐式单可执行项目。
    #[serde(default, rename = "target")]
    pub targets: Option<Vec<Target>>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub name: Option<String>,
    /// 项目版本（元数据）；当前不参与编译或产物命名，预留给 install/export。
    #[serde(default)]
    pub version: Option<String>,
    pub edition: Option<String>,
    /// 显式源文件列表（相对项目根）。缺省时递归扫描 src/ 下的 .cpp/.cc/.cxx。
    #[serde(default)]
    pub sources: Option<Vec<String>>,
    /// 是否在项目根生成 compile_commands.json（clangd / clang-tidy 用）。
    #[serde(default)]
    pub compile_commands: Option<bool>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    Bin,
    StaticLib,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    /// static_lib 必填；bin 可省略，默认使用 [project].name。
    #[serde(default)]
    pub name: Option<String>,
    pub kind: TargetKind,
    /// 文件或目录；目录递归扫描。bin 缺省为 src/，static_lib 必填。
    #[serde(default)]
    pub sources: Option<Vec<String>>,
    /// 仅 bin：按顺序链接的静态库 target 名。
    #[serde(default)]
    pub links: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct CompilerSection {
    pub cpp: Option<String>,
    #[serde(default)]
    pub flags: Vec<String>,
    #[serde(default)]
    pub link_flags: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct DepInfo {
    pub git: String,
    pub tag: Option<String>,
}

/// [dependencies] 自 v0.2 起实装；此表之外的段一律明确报错。
const FUTURE_SECTIONS: &[&str] = &[
    "dev-dependencies",
    "workspace",
    "override",
    "patch",
    "features",
    "pch",
    "build",
];

/// 从未支持的配置段 —— 出现视为拼写错误。
const UNKNOWN_HINT: &str =
    "cizon2 只认 [project]、[dependencies]、[compiler] 与 [[target]]";

/// 依赖名同时是 `deps/` 下的目录名，只允许与 TOML bare key 交集的安全字符。
pub(crate) fn is_valid_dep_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// 项目名/target 名都会进入产物文件名：必须是单个文件名，不能逃出构建作用域。
fn validate_file_component(name: &str, what: &str) -> Result<(), String> {
    const RESERVED: &[&str] = &[
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7",
        "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    let bad_char = name
        .chars()
        .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'));
    let stem = name.split('.').next().unwrap_or("").to_ascii_lowercase();
    if name.is_empty()
        || name == "."
        || name == ".."
        || bad_char
        || name.ends_with(' ')
        || name.ends_with('.')
        || RESERVED.contains(&stem.as_str())
    {
        return Err(format!(
            "{what} {name:?} 不能作为文件名：不允许空名、路径分隔符、Windows 保留字符/设备名，且不能以点或空格结尾。"
        ));
    }
    Ok(())
}

pub(crate) fn validate_project_name(name: &str) -> Result<(), String> {
    validate_file_component(name, "项目名")
}

fn validate_target_name(name: &str) -> Result<(), String> {
    validate_file_component(name, "target 名")?;
    if name.starts_with('.')
        || name.eq_ignore_ascii_case("obj")
        || name.eq_ignore_ascii_case("lib")
    {
        return Err(format!(
            "target 名 {name:?} 与构建内部目录/状态文件冲突（不能以 . 开头，也不能是 obj/lib）。"
        ));
    }
    Ok(())
}

/// 校验并解析 `[[target]]`：bin 省略 name 时使用项目名；库必须显式命名。
fn validate_targets(cfg: &mut Config, project_name: &str) -> Result<(), String> {
    if cfg
        .project
        .as_ref()
        .is_some_and(|p| p.sources.is_some())
    {
        return Err("[project].sources 与 [[target]] 不能同时使用：请把源文件写到对应 target。".into());
    }
    let Some(targets) = &mut cfg.targets else {
        return Ok(());
    };
    if targets.is_empty() {
        return Err("[[target]] 不能为空。".into());
    }

    for target in targets.iter_mut() {
        match target.kind {
            TargetKind::Bin => {
                if target.name.is_none() {
                    target.name = Some(project_name.to_string());
                }
            }
            TargetKind::StaticLib => {
                if target.name.is_none() {
                    return Err("静态库 target 必须提供 name。".into());
                }
            }
        }
        validate_target_name(target.name.as_deref().expect("name resolved"))?;
    }

    let mut names = HashSet::new();
    let mut libs = HashSet::new();
    let mut bins = 0usize;
    for target in targets.iter() {
        let name = target.name.as_deref().expect("name resolved");
        if !names.insert(name.to_ascii_lowercase()) {
            return Err(format!("target 名 {name} 重复（大小写不敏感）。"));
        }
        match target.kind {
            TargetKind::Bin => bins += 1,
            TargetKind::StaticLib => {
                libs.insert(name.to_ascii_lowercase());
                if !target.links.is_empty() {
                    return Err(format!(
                        "静态库 target {name} 不支持 links（v0.4 不支持库依赖库）。"
                    ));
                }
                if target.sources.as_ref().is_none_or(|s| s.is_empty()) {
                    return Err(format!("静态库 target {name} 必须提供 sources。"));
                }
            }
        }
        if let Some(sources) = &target.sources {
            if target.kind == TargetKind::Bin && sources.is_empty() {
                return Err(format!("bin target {name} 的 sources 不能为空。"));
            }
        }
    }
    if bins != 1 {
        return Err(format!(
            "[[target]] 必须恰好有一个 kind = \"bin\"（当前 {bins} 个）。"
        ));
    }
    let bin = targets
        .iter()
        .find(|t| t.kind == TargetKind::Bin)
        .expect("bins == 1");
    let bin_name = bin.name.as_deref().expect("name resolved");
    let mut seen = HashSet::new();
    for link in &bin.links {
        let lower = link.to_ascii_lowercase();
        if !libs.contains(&lower) {
            return Err(format!(
                "bin target {bin_name} 链接了不存在的静态库 {link}。"
            ));
        }
        if !seen.insert(lower) {
            return Err(format!("bin target {bin_name} 的 links 重复列出 {link}。"));
        }
    }
    Ok(())
}

pub fn load(dir: &Path) -> Result<Config, String> {
    let path = dir.join("cizon.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "未找到 {}: 此目录不是 cizon2 项目。请先运行 `cizon2 new <名字>` 或手写 cizon.toml。",
                path.display()
            ));
        }
        Err(e) => return Err(format!("读取 {} 失败: {e}", path.display())),
    };

    let table: toml::Table = text
        .parse()
        .map_err(|e| format!("cizon.toml 解析失败:\n{e}"))?;

    if table.get("target").is_some_and(|v| v.is_table()) {
        return Err(
            "cizon.toml 的 target 必须是数组：请使用 [[target]]（双中括号），不能写 [target]。"
                .into(),
        );
    }

    for key in table.keys() {
        if key == "project" || key == "dependencies" || key == "compiler" || key == "target" {
            continue;
        }
        if FUTURE_SECTIONS.contains(&key.as_str()) {
            return Err(format!(
                "cizon.toml 含有当前边界之外的配置段 [{key}]。\n\
                 对应能力（PCH / features / workspace 等）尚未实装，先删除或注释该段。",
                key = key
            ));
        }
        return Err(format!(
            "cizon.toml 含有未知配置段 [{key}]。{UNKNOWN_HINT}，请检查拼写。",
            key = key
        ));
    }

    let mut cfg: Config = toml::Value::Table(table)
        .try_into::<Config>()
        .map_err(|e| format!("cizon.toml 结构错误:\n{e}"))?;

    let effective_name = match cfg.project.as_ref().and_then(|p| p.name.as_deref()) {
        Some(name) => name.to_string(),
        None => dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "app".into()),
    };
    validate_project_name(&effective_name)?;
    if let Some(version) = cfg.project.as_ref().and_then(|p| p.version.as_deref()) {
        if version.trim().is_empty() {
            return Err("[project].version 不能为空。".into());
        }
        if version.chars().any(|c| c.is_control()) {
            return Err("[project].version 不能包含控制字符。".into());
        }
    }
    if let Some(deps) = &cfg.dependencies {
        for name in deps.keys() {
            if !is_valid_dep_name(name) {
                return Err(format!(
                    "依赖名 {name:?} 非法：只允许字母、数字、- 和 _（它同时是 deps/ 下的目录名）。"
                ));
            }
        }
    }
    validate_targets(&mut cfg, &effective_name)?;
    Ok(cfg)
}

impl Config {
    /// 项目名：[project].name 优先，缺省用目录名。
    pub fn name(&self, dir: &Path) -> String {
        if let Some(name) = self.project.as_ref().and_then(|p| p.name.as_deref()) {
            return name.to_string();
        }
        dir.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "app".into())
    }

    pub fn edition(&self) -> Option<&str> {
        self.project.as_ref().and_then(|p| p.edition.as_deref())
    }

    pub fn explicit_sources(&self) -> Option<&[String]> {
        self.project.as_ref().and_then(|p| p.sources.as_deref())
    }

    /// [[target]] 列表；None 表示传统隐式单可执行项目。
    pub fn targets(&self) -> Option<&[Target]> {
        self.targets.as_deref()
    }

    /// 默认关闭：不是所有开发者都需要编译数据库。
    pub fn compile_commands(&self) -> bool {
        self.project
            .as_ref()
            .and_then(|p| p.compile_commands)
            .unwrap_or(false)
    }

    pub fn flags(&self) -> &[String] {
        self.compiler.as_ref().map(|c| c.flags.as_slice()).unwrap_or(&[])
    }

    pub fn link_flags(&self) -> &[String] {
        self.compiler.as_ref().map(|c| c.link_flags.as_slice()).unwrap_or(&[])
    }

    pub fn explicit_compiler(&self) -> Option<&str> {
        self.compiler.as_ref().and_then(|c| c.cpp.as_deref())
    }

    /// 已声明的 git 依赖（[dependencies]），BTreeMap 保证遍历顺序稳定。
    pub fn dependencies(&self) -> Option<&BTreeMap<String, DepInfo>> {
        self.dependencies.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn write_config(text: &str) -> (PathBuf, Config) {
        let dir = temp_dir("parse");
        std::fs::write(dir.join("cizon.toml"), text).unwrap();
        let cfg = load(&dir).unwrap();
        (dir, cfg)
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cizon2-cfg-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parse_minimal() {
        let (dir, cfg) = write_config("[project]\nname = \"hello\"\n");
        assert_eq!(cfg.name(&dir), "hello");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn project_version_is_metadata_only() {
        let (dir, cfg) = write_config("[project]\nname = \"a\"\nversion = \"1.2.3\"\n");
        assert_eq!(
            cfg.project.as_ref().and_then(|p| p.version.as_deref()),
            Some("1.2.3")
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn empty_project_version_rejected() {
        let dir = temp_dir("empty-version");
        std::fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"a\"\nversion = \"  \"\n",
        )
        .unwrap();
        assert!(load(&dir).unwrap_err().contains("不能为空"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn name_defaults_to_dir_name() {
        let (dir, cfg) = write_config("[project]\n");
        let fallback = Path::new(&dir);
        assert_eq!(cfg.name(fallback), dir.file_name().unwrap().to_string_lossy());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn parse_compiler_section() {
        let (dir, cfg) = write_config(
            "[project]\nname = \"a\"\nedition = \"c++17\"\n\n\
             [compiler]\ncpp = \"g++\"\nflags = [\"-O2\"]\nlink_flags = [\"-lm\"]\n",
        );
        assert_eq!(cfg.edition(), Some("c++17"));
        assert_eq!(cfg.explicit_compiler(), Some("g++"));
        assert_eq!(cfg.flags(), &["-O2"]);
        assert_eq!(cfg.link_flags(), &["-lm"]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn parse_dependencies_section() {
        let (dir, cfg) = write_config(
            "[project]\nname = \"a\"\n\n\
             [dependencies]\nfmt = { git = \"https://example.com/fmtlib/fmt\", tag = \"11.0.2\" }\n",
        );
        let deps = cfg.dependencies().unwrap();
        assert_eq!(
            deps.get("fmt").map(|d| (d.git.as_str(), d.tag.as_deref())),
            Some(("https://example.com/fmtlib/fmt", Some("11.0.2")))
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn future_section_rejected_loudly() {
        let dir = temp_dir("future-rej");
        std::fs::write(dir.join("cizon.toml"), "[project]\nname=\"a\"\n\n[workspace]\nmembers=[]\n")
            .unwrap();
        let err = load(&dir).unwrap_err();
        assert!(err.contains("[workspace]"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn typo_section_rejected_loudly() {
        let dir = temp_dir("typo-rej");
        std::fs::write(dir.join("cizon.toml"), "[dependancies]\n").unwrap();
        let err = load(&dir).unwrap_err();
        assert!(err.contains("[dependancies]"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();
    }

    /// MossKV 实例：真键名是 cpp，写成 compiler 曾被 serde 静默忽略。
    #[test]
    fn unknown_key_in_compiler_section_rejected() {
        let dir = temp_dir("unknown-compiler");
        std::fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"a\"\n\n[compiler]\ncompiler = \"clang++\"\n",
        )
        .unwrap();
        let err = load(&dir).unwrap_err();
        assert!(err.contains("unknown field"), "got: {err}");
        assert!(err.contains("cpp"), "错误应提示正确键名 cpp: {err}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unknown_key_in_project_section_rejected() {
        let dir = temp_dir("unknown-project");
        std::fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"a\"\nlicence = \"MIT\"\n",
        )
        .unwrap();
        let err = load(&dir).unwrap_err();
        assert!(err.contains("licence"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unknown_key_in_dependency_entry_rejected() {
        let dir = temp_dir("unknown-dep-key");
        std::fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"a\"\n\n[dependencies]\n\
             fmt = { git = \"https://example.com/fmt\", branch = \"main\" }\n",
        )
        .unwrap();
        let err = load(&dir).unwrap_err();
        assert!(err.contains("branch"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn compile_commands_defaults_off_and_can_be_enabled() {
        let (dir, cfg) = write_config("[project]\nname = \"a\"\n");
        assert!(!cfg.compile_commands());
        std::fs::remove_dir_all(dir).ok();

        let (dir, cfg) = write_config("[project]\nname = \"a\"\ncompile_commands = true\n");
        assert!(cfg.compile_commands());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn parse_targets_single_bin_and_libs() {
        let (dir, cfg) = write_config(
            "[project]\nname = \"t\"\n\n\
             [[target]]\nname = \"foo\"\nkind = \"static_lib\"\nsources = [\"lib/foo.cpp\"]\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"foo\"]\n",
        );
        let targets = cfg.targets().expect("targets");
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].name.as_deref(), Some("foo"));
        assert_eq!(targets[0].kind, TargetKind::StaticLib);
        assert_eq!(targets[0].sources.as_deref(), Some(&["lib/foo.cpp".to_string()][..]));
        assert_eq!(targets[1].kind, TargetKind::Bin);
        assert_eq!(targets[1].links, ["foo"]);
        std::fs::remove_dir_all(dir).ok();
    }

    fn target_config(targets: &str) -> String {
        format!("[project]\nname = \"t\"\n\n{targets}")
    }

    #[test]
    fn bin_target_name_defaults_to_project_name() {
        let (dir, cfg) = write_config(
            "[project]\nname = \"myapp\"\n\n\
             [[target]]\nkind = \"bin\"\nsources = [\"src\"]\n",
        );
        let bin = cfg
            .targets()
            .unwrap()
            .iter()
            .find(|t| t.kind == TargetKind::Bin)
            .unwrap();
        assert_eq!(bin.name.as_deref(), Some("myapp"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn static_lib_target_requires_name() {
        let dir = temp_dir("lib-no-name");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config(
                "[[target]]\nkind = \"static_lib\"\nsources = [\"lib/x.cpp\"]\n\n\
                 [[target]]\nname = \"app\"\nkind = \"bin\"\n",
            ),
        )
        .unwrap();
        assert!(load(&dir).unwrap_err().contains("必须提供 name"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn targets_require_exactly_one_bin() {
        let dir = temp_dir("no-bin");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config("[[target]]\nname = \"foo\"\nkind = \"static_lib\"\nsources = [\"lib/foo.cpp\"]\n"),
        )
        .unwrap();
        let err = load(&dir).unwrap_err();
        assert!(err.contains("恰好有一个"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();

        let dir = temp_dir("two-bin");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config(
                "[[target]]\nname = \"a\"\nkind = \"bin\"\n\n\
                 [[target]]\nname = \"b\"\nkind = \"bin\"\n",
            ),
        )
        .unwrap();
        assert!(load(&dir).unwrap_err().contains("恰好有一个"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn target_links_and_sources_are_validated() {
        let dir = temp_dir("bad-links");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config(
                "[[target]]\nname = \"foo\"\nkind = \"static_lib\"\nsources = [\"lib/foo.cpp\"]\n\n\
                 [[target]]\nname = \"app\"\nkind = \"bin\"\nlinks = [\"missing\"]\n",
            ),
        )
        .unwrap();
        assert!(load(&dir).unwrap_err().contains("不存在"));
        std::fs::remove_dir_all(dir).ok();

        let dir = temp_dir("lib-links");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config(
                "[[target]]\nname = \"foo\"\nkind = \"static_lib\"\nsources = [\"lib/foo.cpp\"]\nlinks = [\"x\"]\n\n\
                 [[target]]\nname = \"app\"\nkind = \"bin\"\n",
            ),
        )
        .unwrap();
        assert!(load(&dir).unwrap_err().contains("不支持 links"));
        std::fs::remove_dir_all(dir).ok();

        let dir = temp_dir("lib-no-sources");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config(
                "[[target]]\nname = \"foo\"\nkind = \"static_lib\"\n\n\
                 [[target]]\nname = \"app\"\nkind = \"bin\"\n",
            ),
        )
        .unwrap();
        assert!(load(&dir).unwrap_err().contains("必须提供 sources"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn targets_reject_duplicate_names_and_project_sources() {
        let dir = temp_dir("dup-targets");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config(
                "[[target]]\nname = \"x\"\nkind = \"static_lib\"\nsources = [\"lib/x.cpp\"]\n\n\
                 [[target]]\nname = \"x\"\nkind = \"bin\"\n",
            ),
        )
        .unwrap();
        assert!(load(&dir).unwrap_err().contains("重复"));
        std::fs::remove_dir_all(dir).ok();

        let dir = temp_dir("targets-sources");
        std::fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\nsources = [\"src/main.cpp\"]\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\n",
        )
        .unwrap();
        assert!(load(&dir).unwrap_err().contains("不能同时使用"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unknown_key_in_target_rejected() {
        let dir = temp_dir("unknown-target-key");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config("[[target]]\nname = \"app\"\nkind = \"bin\"\nextra = true\n"),
        )
        .unwrap();
        let err = load(&dir).unwrap_err();
        assert!(err.contains("extra"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn single_table_target_rejected_with_hint() {
        let dir = temp_dir("single-target-table");
        std::fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n[target]\nname = \"app\"\nkind = \"bin\"\n",
        )
        .unwrap();
        let err = load(&dir).unwrap_err();
        assert!(err.contains("[[target]]"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn target_names_are_case_insensitive_and_reserved_checked() {
        let dir = temp_dir("case-targets");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config(
                "[[target]]\nname = \"Foo\"\nkind = \"static_lib\"\nsources = [\"lib/foo.cpp\"]\n\n\
                 [[target]]\nname = \"foo\"\nkind = \"bin\"\n",
            ),
        )
        .unwrap();
        assert!(load(&dir).unwrap_err().contains("重复"));
        std::fs::remove_dir_all(dir).ok();

        let dir = temp_dir("reserved-target");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config("[[target]]\nname = \"obj\"\nkind = \"bin\"\n"),
        )
        .unwrap();
        assert!(load(&dir).unwrap_err().contains("内部"));
        std::fs::remove_dir_all(dir).ok();

        // links 大小写不敏感
        let dir = temp_dir("case-links");
        std::fs::write(
            dir.join("cizon.toml"),
            target_config(
                "[[target]]\nname = \"Foo\"\nkind = \"static_lib\"\nsources = [\"lib/foo.cpp\"]\n\n\
                 [[target]]\nname = \"app\"\nkind = \"bin\"\nlinks = [\"FOO\"]\n",
            ),
        )
        .unwrap();
        assert!(load(&dir).is_ok());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn missing_file_gives_actionable_message() {
        let dir = temp_dir("missing-proj");
        let err = load(&dir).unwrap_err();
        assert!(err.contains("未找到"), "got: {err}");
        assert!(err.contains("不是 cizon2 项目"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn read_error_is_not_reported_as_missing() {
        let dir = temp_dir("bad-utf8");
        // 非 UTF-8 内容：读文件失败，不能被伪装成"未找到 cizon.toml"
        std::fs::write(dir.join("cizon.toml"), [0xFFu8, 0xFE]).unwrap();
        let err = load(&dir).unwrap_err();
        assert!(err.contains("读取"), "got: {err}");
        assert!(!err.contains("未找到"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn project_name_rejects_path_escape() {
        let dir = temp_dir("name-escape");
        for bad in ["../evil", "a/b", "a\\b", "C:\\x", "aux", ""] {
            std::fs::write(
                dir.join("cizon.toml"),
                format!("[project]\nname = {bad:?}\n"),
            )
            .unwrap();
            let err = load(&dir).unwrap_err();
            assert!(err.contains("文件名"), "name={bad:?} got: {err}");
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn dependency_name_rejects_path_traversal() {
        let dir = temp_dir("dep-name-escape");
        for bad in ["..", "a/b", "a\\b", ""] {
            std::fs::write(
                dir.join("cizon.toml"),
                format!("[project]\nname = \"a\"\n\n[dependencies]\n{bad:?} = {{ git = \"x\" }}\n"),
            )
            .unwrap();
            let err = load(&dir).unwrap_err();
            assert!(err.contains("依赖名"), "dep={bad:?} got: {err}");
        }
        std::fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"a\"\n\n[dependencies]\nmy-dep_1 = { git = \"x\" }\n",
        )
        .unwrap();
        assert!(load(&dir).is_ok(), "合法依赖名应通过");
        std::fs::remove_dir_all(dir).ok();
    }
}
