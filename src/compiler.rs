use std::process::Command;

/// 编译器探测优先级（沿用 Cizon1 的结论：g++ 最常见，clang++ 次之）。
pub const PREFERENCE: &[&str] = &["g++", "clang++"];

#[derive(Debug, Clone)]
pub struct Cxx {
    /// 可执行名，如 "g++"
    pub name: String,
    /// `--version` 第一行，如 "g++ (MinGW-W64 ...) 14.2.0"
    pub version: String,
}

impl Cxx {
    /// 短版本号：version 行中第一个纯「数字+点分」token（g++ → "14.2.0"，
    /// clang → "22.1.7"，clang 行尾的提交哈希不会被误取）。
    pub fn short_version(&self) -> &str {
        self.version
            .split_whitespace()
            .find(|t| !t.is_empty() && t.chars().all(|c| c.is_ascii_digit() || c == '.'))
            .unwrap_or(self.version.as_str())
    }
}

/// 规格别名归一化："gcc@14" → "g++"，"clang" → "clang++"。
/// @version 后缀直接忽略（与 Cizon1 行为一致）。
pub fn resolve_spec(spec: &str) -> &str {
    let name = match spec.find('@') {
        Some(at) => &spec[..at],
        None => spec,
    };
    match name {
        "gcc" => "g++",
        "clang" => "clang++",
        n => n,
    }
}

/// 探测一个编译器是否可用：`--version` 退出码 0 即认为可用。
fn probe(name: &str) -> Option<Cxx> {
    let out = Command::new(name)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let version = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    Some(Cxx {
        name: name.to_string(),
        version,
    })
}

/// 显式指定（`--compiler` / `[compiler].cpp`）优先，否则按优先级自动探测。
pub fn pick(explicit: Option<&str>) -> Result<Cxx, String> {
    if let Some(spec) = explicit {
        let name = resolve_spec(spec);
        return probe(name).ok_or_else(|| {
            format!("指定的编译器 {name} 在 PATH 中不可用。检查拼写，或去掉 --compiler 让 cizon2 自动探测。")
        });
    }
    for name in PREFERENCE {
        if let Some(cxx) = probe(name) {
            return Ok(cxx);
        }
    }
    Err("未找到可用的 C++ 编译器（尝试过 g++、clang++）。\n请安装 MinGW-w64 或 LLVM 并加入 PATH，\
         或在 cizon.toml 的 [compiler] cpp = \"...\" 指向编译器。"
        .into())
}

/// 生成 `-std=c++NN`；未配置时沿用 Cizon1 模板默认 c++23。
pub fn edition_flag(edition: Option<&str>) -> String {
    let ver = edition.and_then(|e| e.strip_prefix("c++")).unwrap_or("23");
    format!("-std=c++{ver}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_spec_passthrough() {
        assert_eq!(resolve_spec("g++"), "g++");
        assert_eq!(resolve_spec("g++@14"), "g++");
        assert_eq!(resolve_spec("gcc@14"), "g++");
        assert_eq!(resolve_spec("clang@18"), "clang++");
        assert_eq!(resolve_spec("clang++"), "clang++");
    }

    #[test]
    fn edition_flag_defaults_to_cxx23() {
        assert_eq!(edition_flag(None), "-std=c++23");
        assert_eq!(edition_flag(Some("c++17")), "-std=c++17");
        assert_eq!(edition_flag(Some("c++20")), "-std=c++20");
    }

    #[test]
    fn short_version_extracts_dot_number_token() {
        let gxx = Cxx {
            name: "g++".into(),
            version: "g++ (x86_64-posix-seh-rev0, Built by MinGW-Builds project) 14.2.0".into(),
        };
        assert_eq!(gxx.short_version(), "14.2.0");
        let clang = Cxx {
            name: "clang++".into(),
            version:
                "clang version 22.1.7 (https://github.com/llvm/llvm-project a255c1ed36a1d06f79bd2633ba9f8d900153007c)"
                    .into(),
        };
        assert_eq!(clang.short_version(), "22.1.7");
        let odd = Cxx { name: "c++".into(), version: "unknown compiler".into() };
        assert_eq!(odd.short_version(), "unknown compiler");
    }

    #[test]
    fn probe_rejects_garbage() {
        assert!(probe("cizon2-not-a-compiler-xyz").is_none());
    }

    #[test]
    fn pick_rejects_bad_explicit_spec() {
        assert!(pick(Some("cizon2-not-a-compiler-xyz")).is_err());
    }

    #[test]
    fn pick_finds_something_when_compiler_installed() {
        // 只要有任一真实编译器就应探测成功；CI 无编译器环境跳过。
        let has_any = PREFERENCE
            .iter()
            .any(|c| Command::new(c).arg("--version").output().map(|o| o.status.success()).unwrap_or(false));
        if !has_any {
            return;
        }
        assert!(pick(None).is_ok());
    }
}
