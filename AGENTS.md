# Cizon2 — Agent Instructions

## 定位

个人开发者的零配置 C++ 构建工具。使命：`cizon2 build && cizon2 run`，达到 cargo/uv 级的开箱即用。

不做 CMake 的替代品。CMake 的生态是二十年积累的结果；需要 target 图、install/export、代码生成或 MSVC 时，由 CMake 接手，cizon2 全身而退。

上一个项目 Cizon1 因 scope creep 膨胀到一万一千行。本项目的原则是边界优先于功能。

## 当前边界（v0.4.1）

| 在边界内 | 边界外 |
|---|---|
| `build` / `run` / `new` / `init` / `add` / `update` | Workspace / monorepo / 多包 |
| 中小型单可执行项目（可带多个静态库 `[[target]]`） | 多可执行 target / 共享库（待真实需求） |
| cizon.toml 认 `[project]`、`[dependencies]`、`[compiler]`、`[[target]]` | PCH、features、install/export |
| g++ / clang++ 探测，`--compiler` 临时切换 | MSVC（等真实用户提出） |
| mtime + `-MMD` 增量 + 编译/链接/库状态指纹 | vcpkg / conda 等外部包管理 |
| compile_commands.json（默认关闭，`[project].compile_commands` 开启） | 自定义错误格式化器 |
| 编译器 stderr 捕获回放（UTF-8 有损解码）、退出码翻译 | 与 CMake 全功能对齐 |

未知配置段与已知段内的未知键一律报错，不静默忽略。

## 项目结构约定

- `include/` 只放公开头（唯一自动 `-I`）；私有头放 `lib/` 或实现文件旁边，默认不暴露
- 不要用 `[compiler].flags` 加 `-Ilib` 等方式把私有目录暴露给所有 target
- 公共头必须自包含，不能依赖私有头；单 exe 项目可把头放 `src/`，header-only 库全部放 `include/`
- `[[target]]` 的 sources 是项目内相对路径（文件或目录）；恰好一个 bin，库不能链接库
- bin 的 name 可省略并默认项目名；`[project].version` 仅元数据；对象路径镜像源相对路径，同目录同 stem 冲突

## 输出约定

- 用户面路径用项目相对路径；编译器原始诊断不改写、不缩进
- 警告按源文件分组：金色上下分隔线 + 黄色 `Warning` 头
- `Finished` 严格区分 `fresh build` / `incremental rebuild` / `relinked` / `up to date`

## 非目标（重开需明确讨论）

- monorepo / workspace / 多包、crates.io 式包注册表
- PCH、build.cpp 逃生舱、metadata / env / tree / doctor
- 企业级 CI
- `serde` / `toml` 之外的第三方 crate

## 三道闸

1. 一级闸：每周用自己的真实 C++ 项目编译；连续两周不用就停功能。
2. 二级闸：说服 1 个真人用它编译成功之前，不加新命令。
3. 三级闸：合并前必须能在不看注释的情况下讲清每个文件的逻辑。

## 协作与提交

- 新功能先征得开发者同意；小 bug 自行修，大问题先汇报。
- `main` 只收可发布版本；功能与修复提交到 `dev`，可发布后合入 `main`。
- 一个功能一个 commit；测试先行（模块单测 + 真实编译器/git 端到端）。
- 单 crate；手写参数解析，不用 clap，不引入 rayon / sha2。
- 用户不明确要求时，默认 debug 构建（`cizon2 build`）。

## 已知待办（未排期）

- VSCode 断点调试当前靠手工 `.vscode/launch.json` + `tasks.json`；未来优先按 `[project] vscode = true` 非破坏生成（不实现调试器，gdb 先行），详见 README「后续改进」
- 候选工程改进：构建状态统一到 stderr、exe/锁文件原子替换、项目级构建锁、compile_commands 关闭清理、依赖移除后的锁/目录清理、诊断相对路径、基础 CI
- 详细清单与真实需求触发条件见 README「后续改进」；开工前按三道闸确认

## 路线

1. v0.1 `build` / `run` —— 已发布 v0.1.0
2. v0.2 `new` / `init` / `add` / `update`，git 依赖与锁生命周期 —— 已发布 v0.2.0
3. v0.2.1 递归 `src/`、`include/` 自动 `-I`、配置键严格校验 —— 随 v0.3.0 发布
4. v0.3 compile_commands.json（默认关闭）—— 已发布 v0.3.0
5. v0.4 `[[target]]`：单 bin + 多静态库 —— v0.4.1 已合并 main；dev 待发布构建输出改进
6. 之后：需要嵌套 target 图、install/export、MSVC 或外部包管理时交给 CMake；是否提供一次性导出命令，等真实需求与二级闸。
