# Cizon 交接上下文（接手 Agent 先读这个）

> 生成时间：2026-11-25  
> 仓库：`C:\Users\zzh\NUFE\Programming\Rust\Cizon2`  
> 当前分支：`fix/audit-0.10.2`（复审通过后合回 `main`）  
> 最新代码提交：`7cfb8ad fix: keep build lock clean and normalize release concurrency`  
> 当前版本：`0.10.2`  
> 工作区：本文件待提交；其余改动已提交在 `fix/audit-0.10.2`  
> 最新成功 Release：`v0.10.1`；`v0.10.2` 待打 tag 发布  
> Release URL：<https://github.com/xaoq23/Cizon2/releases/tag/v0.10.1>

接手顺序建议：

1. 先读本文件。
2. 再读 `AGENTS.md`，它是项目边界、输出约定和提交规则的最高优先级说明。
3. 需要教学式背景时读 `README.md` / `README-zh.md`。
4. 修改前先看对应源码和测试，不要仅依据 README 推断行为。

---

## 1. 项目定位与硬边界

Cizon 是个人维护的声明式、渐进式现代 C++ 构建工具，目标体验是：

```bash
cizon build && cizon run
```

面向中小型项目，不做 CMake 替代品。需要条件化/大型 target 图、install/export、代码生成、MSVC、交叉编译、外部包管理时交给 CMake。

当前原则：

- 边界优先于功能，不做认知外或没有真实需求的功能。
- 一律文本声明、清晰 CLI，不静默忽略未知配置。
- 单 Rust crate；手写参数解析；除 `serde` / `toml` 外不引入第三方 crate。
- 新命令/功能先征得用户同意；小 bug 可自行修，大问题先汇报。
- `main` 只收可发布版本；功能/修复通常在 `dev` 或 `fix/*` / `feat/*`，复审通过后合入 `main`。
- commit message 一律英文 conventional 格式。
- 源码注释、文档注释、用户面字符串一律英文；`README-zh.md` 和 `AGENTS.md` 保留中文。
- 修改 README 时要同步中英文。
- 不移动已发布 tag；不要 force push。
- 不提取或处理凭据密码；git 网络命令使用非交互设置。

---

## 2. 当前发布状态（重要）

### 已完成

- `v0.10.1` 已打 annotated tag 并发布。
- Release 工作流成功：run `45544757118`。
- 资产已上传并通过 SHA-256 校验：
  - `cizon-v0.10.1-windows-x86_64.exe`
  - `cizon-v0.10.1-linux-x86_64`
  - 两者各自 `.sha256`
- Windows SHA-256：`aab59dc0a6d45c3ed5de60e016edde043290eb93bc972de7b84a2f4cb55406a4`
- Linux SHA-256：`88f28a7d4f42d04884cdd8678ac365fae7ccd89b7c469b86cf10d94e26903aba`
- Windows 本机安装：`C:\Users\zzh\.cargo\bin\cizon.exe`，版本 `0.10.1`
- Arch WSL 安装：`/home/zzh/.local/bin/cizon`，fish PATH 可见，版本 `0.10.1`
- `v0.10.2`：功能集不变，发布流程待完成（合 main → annotated tag → push → 验证 Release → 双平台安装）

### 特殊历史：`v0.10.0` tag 已公开但 Release 失败

- `v0.10.0` tag 指向 `7f65a5f`，保持不动，不要移动/强制重打。
- 对应工作流 `45539302545` 的 Linux 测试失败，原因是测试自身：
  - `printf("%zu", int)` 在 GCC 15 + `werror` 下触发格式错误。
- 修复提交：
  - `64e1e9b fix: make ABI define test compiler-portable`
  - 将测试探针返回值改为 `std::size_t`。
- `fce08ba chore: bump to 0.10.1` 后重新打 `v0.10.1` 并成功发布。
- 功能集与 v0.10.0 相同；v0.10.1 仅修复 Linux 发布测试可移植性。

### v0.10.2（待发布）：六路审计修复

- CI：tag / dispatch 输入改经 `env:` 传递（消除 shell 注入）、权限最小化为 release job 才有 `contents: write`、按归一化 tag 设 `concurrency`、Linux 测试补 `--locked`
- 依赖：根 `[dependencies].git` 与 `add` 的 URL 校验（拒绝相对路径/前导 `-`）、`git clone --`、SCP 无斜杠源派生名、`update` 先写锁再换 checkout 且失败回滚、`update` 校验 HEAD 与锁一致
- CLI：帮助标志在命令选项中任意位置都等价于 `help <command>`（`--` 之后不算）；`help` / `version` 拒绝多余参数；USAGE 补 `-v`
- 构建共享库：先链接到 `.tmp-shared/<原名>` 再原子替换（避免失败留损坏产物、保留 Windows import lib 内嵌 DLL 名）；成功链接的 stderr 也做相对路径化；Windows exe 占用时短时重试并给出可操作提示
- 头文件检查：`prefix/include` 纳入重复检查、非头文件扩展名不再误报、`deps::prepare` 之后重新检查恢复的依赖头
- clean：符号链接/junction 分支也检查活动构建锁；锁记录解析后的绝对路径，删链接后仍可清除目标中的锁；删除非幂等问题修复；VS Code 生成失败改用标准 Warning 样式

---

## 3. v0.10 功能总览

### `[subproject]`

- 受限本地组合，不是 workspace。
- 单一根 build/lock/compiler/edition；子项目无独立构建/发布生命周期。
- 子项目只允许 `static_lib` / `shared_lib` / `test`，不能有 `bin`。
- 声明 `[subproject]` 的文件和每个子项目自身都必须至少有一个 `[[target]]`。
- 子项目路径相对声明它的 `cizon.toml` 解析，解析链接后必须仍在根项目内。
- 嵌套允许；包含环报错；菱形按规范目录去重。
- 根 `[compiler]` / `[project].edition` 优先；子项目 `[compiler]` 和不同 edition 被警告忽略。
- 子项目 `[dependencies]` 合并进根依赖集；依赖名大小写不敏感但 key 拼写必须一致。
- 子项目 git URL 必须是绝对路径、URL 或 SCP 风格 SSH，不能相对路径。
- `update` 只写根 `cizon.lock`；`sync` 从根锁恢复合并后的依赖。

### target 与 PUBLIC 接口

- `[[target]]` kind：`bin` / `static_lib` / `shared_lib` / `test`。
- 库的 `include` / `defines` / `links` 均为 PUBLIC，沿直接/间接 `links` 传递。
- `shared_lib` 链接的静态库会打进 DLL/.so；消费者继承其 include/defines，但静态归档不再单独出现在消费者链接行。
- bin/test 的 `links` 只作用于自身；bin/test 的 defines 不反向传播。
- 静态库依赖图自动按依赖方在前排序；环报错；不加 `--start-group`。
- 目前没有 PRIVATE / INTERFACE / `$<LINK_ONLY>`；这是明确 deferred 边界。

### 宏定义：完整语义

来源与作用域：

| 来源 | 作用域 | 传播 | 持久化 |
|---|---|---|---|
| `[compiler].flags` 中的 `-D` | 所有项目源码 | 不按图传播；git 依赖不继承 | TOML |
| `[project].defines` | 仅无 `[[target]]` 的隐式单 bin | 无下游 | TOML |
| `[[target]].defines` | 当前 target；库定义 PUBLIC | 沿 static/shared links | TOML |
| CLI `-D` / `--define` | 本次命令的所有项目 target | 直接应用 | 不写 TOML/lock |

优先级从低到高：

```text
基础参数 < [compiler].flags < [project].defines / target defines < CLI -D
```

实现方式：

```text
-UNAME -DNAME=VALUE
```

- target/project define 覆盖全局 flags 且无重定义警告。
- CLI 最后覆盖 TOML；CLI 重复同名宏时后出现者生效。
- 同一 defines 列表：同宏同值去重，不同值报错。
- 同一有效 target 链接闭包：同宏同值去重，不同值 plan 期报错。
- `[project].defines` 与 `[[target]]` 同时出现是硬错误。
- git 依赖源码不继承上述任何项目宏。
- test 源码在所有传播和 CLI 覆盖后强制剥离 `NDEBUG`。
- 宏进入有效 `compile_flags` 指纹：变化会清理项目对象并强制重链；git 依赖对象不因此重编。

CLI 支持形式：

```bash
-DNAME
-DNAME=VALUE
-D NAME=VALUE
--define NAME=VALUE
```

### `clean` / `--verbose` / Cargo 风格帮助

- `cizon clean`：
  - 要求当前目录有 `cizon.toml`，但不解析配置。
  - 只删除 `build/`；不删 config/lock/deps/compile_commands/editor/source。
  - 真实 build 目录存在活动构建锁时报错；不等待。
  - `build` 是符号链接/junction 时只删链接，不跟随目标。
- `--verbose`：
  - 适用于 build/run/test。
  - stderr 打印实际 compiler/archive/link/run/test 命令。
  - 可能看到 `.tmp` 原子输出名。
  - 与 `--quiet` 互斥。
  - 没有短写 `-v`；`-v` 仍是 version 兼容别名。
- 帮助：
  - 顶层和逐命令使用 Cargo 风格 `Usage / Options / Commands`。
  - TTY 下标题/分节加粗、命令/选项着色；stdout 重定向时纯文本。
  - `cizon <command> --help` 与 `cizon help <command>` 等价，包括 `version --help` 和 `help --help`。

---

## 4. 代码结构入口

| 文件 | 职责 |
|---|---|
| `src/main.rs` | CLI dispatch、参数解析、帮助/TOML 帮助、宏 CLI、clean 命令 |
| `src/config.rs` | cizon.toml 解析、目标/依赖校验、subproject 图合并、define 校验 |
| `src/pipeline/targets.rs` | plan、源码收集、include/define 传播、重复头检查、静态链接排序 |
| `src/pipeline/mod.rs` | 构建执行、BuildLock、clean、状态失效、编译/归档/链接调度 |
| `src/pipeline/compile.rs` | 单 TU 编译、depfile、原子对象替换 |
| `src/pipeline/link.rs` | archive、shared/exe link |
| `src/pipeline/state.rs` | project/lib/shared/link/exec 状态指纹 |
| `src/pipeline/output.rs` | quiet/verbose、TTY 颜色、相对路径、状态行 |
| `src/pipeline/implib.rs` | Windows import lib / ELF SONAME |
| `src/deps.rs` | git 依赖 add/update/sync/lock、依赖源码编译准备 |
| `src/scaffold.rs` | new/init 模板 |
| `src/run.rs` / `src/test.rs` | 运行 bin/test，退出码翻译，verbose 子进程命令 |
| `.github/workflows/release.yml` | v* tag 构建 Windows/Linux、Linux 测试门禁、Release 上传 |

关键测试在：

- `src/pipeline/tests.rs`：绝大多数真实编译器端到端、增量、ABI、subproject、clean、link/define 语义。
- 各模块内 `#[cfg(test)] mod tests`：解析、状态、输出、deps、scaffold 等单元测试。

---

## 5. 验证与常用命令

当前测试总数：**291**（v0.10.2 新增 12 项审计回归测试）。

本机验证过的命令：

```bash
cargo test --quiet
cargo test --release --locked --quiet
cargo build --release --locked --quiet
```

Linux 门禁（Arch WSL）：

```bash
wsl.exe -d Archlinux bash -lc 'cd /mnt/c/Users/zzh/NUFE/Programming/Rust/Cizon2 && cargo test --release --locked --quiet'
```

最近结果：

- Windows debug：291/291
- Windows release：291/291
- Arch WSL release：291/291
- Release 工作流 Linux 测试：v0.10.1 成功；v0.10.2 待验证

注意：

- `cargo fmt --check` 会报告仓库既有的大范围格式漂移；不要为了本功能批量格式化整个仓库。
- Windows 测试是本地门禁；Linux CI 是 `.so` 唯一门禁。
- 当前 GitHub Actions 日志对未登录 API 不可读；失败时通常需要用户提供日志或从已连接浏览器查看。
- 测试临时目录曾出现并行碰撞，`config` 和 pipeline 测试已做唯一化/ETXTBSY 重试；新增测试仍应使用唯一 temp 目录。

---

## 6. 最近提交与 tags

最近提交：

```text
7cfb8ad fix: keep build lock clean and normalize release concurrency
75334e4 fix: harden audit findings for v0.10.2
fce08ba chore: bump to 0.10.1
64e1e9b fix: make ABI define test compiler-portable
7f65a5f fix: resolve v0.10.0 release audit findings   <-- v0.10.0 tag 指向这里（Release 失败）
25848a9 feat: add temporary and project compile overrides
563d0a4 feat: colorize cargo-style CLI help
7d84560 feat: add clean, verbose and cargo-style help
8847881 feat: add public per-target compile definitions
7518dd5 docs: align v0.10 subproject status wording
55358ca chore: bump to 0.10.0
```

Tags：

- `v0.10.0`：已公开，但对应 Release 工作流失败；**不要移动**。
- `v0.10.1`：当前成功发布版本。
- `v0.10.2`：待打 annotated tag 并发布；不要移动已发布 tag。
- 更早：`v0.9.6` 等，均已发布。

---

## 7. 已知非阻塞问题（代码审计 P2/P3）

v0.10.1 审计中的非发布阻塞项，**1–3 已在 v0.10.2 修复**：

1. ~~未恢复依赖时的首轮重复头检查缺口~~ — `validate_includes_after_prepare` 在 `deps::prepare` 后复查。
2. ~~`prefix` include 不参与重复头检查~~ — `extend_prefix_include` 已覆盖。
3. ~~clean 极端竞态~~ — 链接分支取构建锁，锁记录解析后路径，删除改为幂等。

仍未修复（非阻塞）：

4. **重复头扫描成本**
   - 依赖 checkout 含 `.git` 时，整树扫描会重复走查，正确但偏慢。

P1/P2 产品能力仍 deferred：

- P1：PRIVATE include。
- P2：静态库 PRIVATE link / `$<LINK_ONLY>`。

外部 cizon 项目通过 `add` 消费其 target 图、workspace/独立包生命周期、deps 搬进 build 等方案已被拒绝或 deferred。

---

## 8. 当前有效产品边界摘要

在边界内：

- `build` / `run` / `test` / `sync` / `clean` / `new` / `init` / `add` / `update`
- 多 bin/test、static/shared library target
- 无环静态库 PUBLIC 依赖与 include/define 传播
- `[subproject]` 受限本地组合
- Linux/Windows shared_lib（Linux `.so` CI 验证）
- test target + `cizon test` + extern_links + prefix
- strict/werror、compile_commands、VS Code IntelliSense
- git 依赖 lock、sync/check、原子写、项目构建锁
- PUBLIC target defines、隐式 project defines、临时 CLI defines
- clean / verbose / Cargo 风格彩色帮助

边界外：

- workspace、独立包生命周期、多包 registry
- 条件化/大型 target 图、install/export
- shared 依赖 shared、复杂 DLL 图
- MSVC、交叉编译、复杂工具链
- PCH、build.cpp 逃生舱、第三方 crate
- 自动符号导出、C++ ABI 分发
- 外部 cizon.toml target 图消费

---

## 9. 用户协作偏好（本会话确认）

- 用户使用中文讨论；代码/注释/用户字符串英文，README-zh/AGENTS 中文。
- 偏好：简单约定 + 大声报错；不为假想需求增加抽象。
- 重视真实项目 dogfood、双平台验证、独立审计 PASS。
- 发布流程：先测试 → 独立代码/文档审计 → 修复 → 合并 main → annotated tag → push → 验证 Actions/Release → 双平台安装。
- 已发布 tag 不移动；发布失败时优先下一个 patch 版本。
- 默认 debug 构建；只有明确 `--release` 才 release。
- 当前机器：
  - Windows Git Bash，g++ 16.2.x，clang++ 22.x，Rust 稳定版
  - Arch WSL，fish shell，g++ 16.2.x
  - Windows cizon：`C:\Users\zzh\.cargo\bin\cizon.exe`
  - WSL cizon：`/home/zzh/.local/bin/cizon`

---

## 10. 接手后建议

默认不要立刻扩功能。优先：

1. 复跑 `cargo test` + Linux release 门禁，确认环境。
2. 若用户没有明确新需求，保持 v0.10.2 状态（发布完成后保持 v0.10.2 已发布状态）。
3. 剩余 P2/P3（重复头扫描成本等）先确认真实痛点，再设计/测试后动。
4. 发布新版本时不要移动 `v0.10.0` / `v0.10.1` / `v0.10.2`；用新 tag。
5. 任何新功能先读 AGENTS 边界并征得用户同意。
