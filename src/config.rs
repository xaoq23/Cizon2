//! cizon.toml 解析。
//!
//! 当前边界：只认 `[project]`、`[dependencies]` 与 `[compiler]` 三段。其他段一律
//! 明确报错，绝不静默忽略 —— 拼写错误和"等未来版本来支持"必须当场暴露，而不是
//! 构建出一个看起来成功实际没加依赖的产物。

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Deserialize, Default)]
pub struct Config {
    pub project: Option<Project>,
    pub dependencies: Option<BTreeMap<String, DepInfo>>,
    pub compiler: Option<CompilerSection>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub name: Option<String>,
    pub edition: Option<String>,
    /// 显式源文件列表（相对项目根）。缺省时递归扫描 src/ 下的 .cpp/.cc/.cxx。
    #[serde(default)]
    pub sources: Option<Vec<String>>,
    /// 是否在项目根生成 compile_commands.json（clangd / clang-tidy 用）。
    #[serde(default)]
    pub compile_commands: Option<bool>,
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
    "target",
    "pch",
    "build",
];

/// 从未支持的配置段 —— 出现视为拼写错误。
const UNKNOWN_HINT: &str = "cizon2 只认 [project]、[dependencies] 与 [compiler]";

/// 依赖名同时是 `deps/` 下的目录名，只允许与 TOML bare key 交集的安全字符。
pub(crate) fn is_valid_dep_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// 项目名会被用作可执行文件名：必须是单个文件名，不能逃出构建作用域。
pub(crate) fn validate_project_name(name: &str) -> Result<(), String> {
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
            "项目名 {name:?} 不能作为文件名：不允许空名、路径分隔符、Windows 保留字符/设备名，且不能以点或空格结尾。"
        ));
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

    for key in table.keys() {
        if key == "project" || key == "dependencies" || key == "compiler" {
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

    let cfg: Config = toml::Value::Table(table)
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
    if let Some(deps) = &cfg.dependencies {
        for name in deps.keys() {
            if !is_valid_dep_name(name) {
                return Err(format!(
                    "依赖名 {name:?} 非法：只允许字母、数字、- 和 _（它同时是 deps/ 下的目录名）。"
                ));
            }
        }
    }
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
