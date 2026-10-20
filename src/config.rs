//! cizon.toml 解析。
//!
//! v0.1 边界：只认 `[project]` 与 `[compiler]` 两段。其他段一律明确报错，
//! 绝不静默忽略 —— 拼写错误和"等未来版本来支持"必须当场暴露，而不是构建出
//! 一个看起来成功实际没加依赖的产物。

use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Deserialize, Default)]
pub struct Config {
    pub project: Option<Project>,
    pub compiler: Option<CompilerSection>,
}

#[derive(Debug, Deserialize, Default)]
pub struct Project {
    pub name: Option<String>,
    pub edition: Option<String>,
    /// 显式源文件列表（相对项目根）。缺省时扫描 src/*.{cpp,cc,cxx}。
    #[serde(default)]
    pub sources: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Default)]
pub struct CompilerSection {
    pub cpp: Option<String>,
    #[serde(default)]
    pub flags: Vec<String>,
    #[serde(default)]
    pub link_flags: Vec<String>,
}

/// v0.2+ 才会落地的配置段 —— 现在出现即报错。
const FUTURE_SECTIONS: &[&str] = &[
    "dependencies",
    "dev-dependencies",
    "workspace",
    "override",
    "patch",
    "features",
    "target",
    "pch",
    "build",
];

/// v0.1 从未有的配置段 —— 出现视为拼写错误。
const UNKNOWN_HINT: &str = "cizon2 v0.1 只认 [project] 与 [compiler]";

pub fn load(dir: &Path) -> Result<Config, String> {
    let path = dir.join("cizon.toml");
    let text = std::fs::read_to_string(&path).map_err(|_| {
        format!(
            "未找到 {}: 此目录不是 cizon2 项目。请先运行 `cizon2 new <名字>`（v0.2）或手写 cizon.toml。",
            path.display()
        )
    })?;

    let table: toml::Table = text
        .parse()
        .map_err(|e| format!("cizon.toml 解析失败:\n{e}"))?;

    for key in table.keys() {
        if key == "project" || key == "compiler" {
            continue;
        }
        if FUTURE_SECTIONS.contains(&key.as_str()) {
            return Err(format!(
                "cizon.toml 含有 v0.1 边界之外的配置段 [{key}]。\n\
                 对应能力（依赖管理/PCH/features 等）尚未迁移，先删除或注释该段。",
                key = key
            ));
        }
        return Err(format!(
            "cizon.toml 含有未知配置段 [{key}]。{UNKNOWN_HINT}，请检查拼写。",
            key = key
        ));
    }

    toml::Value::Table(table)
        .try_into::<Config>()
        .map_err(|e| format!("cizon.toml 结构错误:\n{e}"))
}

impl Config {
    /// 项目名：[project].name 优先，缺省用目录名。
    pub fn name(&self, dir: &Path) -> String {
        if let Some(name) = self.project.as_ref().and_then(|p| p.name.clone()) {
            return name;
        }
        dir.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "app".into())
    }

    pub fn edition(&self) -> Option<&str> {
        self.project.as_ref().and_then(|p| p.edition.as_deref())
    }

    pub fn explicit_sources(&self) -> Option<&Vec<String>> {
        self.project.as_ref().and_then(|p| p.sources.as_ref())
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn write_config(text: &str) -> (PathBuf, Config) {
        let dir = std::env::temp_dir().join(format!(
            "cizon2-cfg-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("cizon.toml"), text).unwrap();
        let cfg = load(&dir).unwrap();
        (dir, cfg)
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
        let (_, cfg) = write_config(
            "[project]\nname = \"a\"\nedition = \"c++17\"\n\n\
             [compiler]\ncpp = \"g++\"\nflags = [\"-O2\"]\nlink_flags = [\"-lm\"]\n",
        );
        assert_eq!(cfg.edition(), Some("c++17"));
        assert_eq!(cfg.explicit_compiler(), Some("g++"));
        assert_eq!(cfg.flags(), &["-O2"]);
        assert_eq!(cfg.link_flags(), &["-lm"]);
    }

    #[test]
    fn future_section_rejected_loudly() {
        let dir = std::env::temp_dir().join("cizon2-cfg-future-rej");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("cizon.toml"), "[project]\nname=\"a\"\n\n[workspace]\nmembers=[]\n")
            .unwrap();
        let err = load(&dir).unwrap_err();
        assert!(err.contains("[workspace]"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn typo_section_rejected_loudly() {
        let dir = std::env::temp_dir().join("cizon2-cfg-typo-rej");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("cizon.toml"), "[dependancies]\n").unwrap();
        let err = load(&dir).unwrap_err();
        assert!(err.contains("[dependancies]"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn missing_file_gives_actionable_message() {
        let dir = std::env::temp_dir().join("cizon2-cfg-missing-proj");
        std::fs::create_dir_all(&dir).unwrap();
        let err = load(&dir).unwrap_err();
        assert!(err.contains("cizon.toml"), "got: {err}");
        std::fs::remove_dir_all(dir).ok();
    }
}
