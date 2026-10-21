# Cizon2 — Agent Instructions

## 边界宣言（先读这个）

> v0.1 只服务一个人：第一周刚装好 C++ 的大学生。
> 使命：`cizon2 build && cizon2 run`，零配置，最多两个命令。
> 不做的事：与 CMake 竞争生态。CMake 的地位是 20 年生态建设的结果，不靠替代它来赢。

上一个项目（Cizon1，已归档）的教训：使命里写"大学生和企业大项目都能跑"，导致
scope creep 到一万一千行、看不懂自己的代码。本项目的第一份文档就是这份边界。

## v0.1 边界

| 在边界内 | 边界外 |
|---|---|
| `build` / `run` 两个命令 | `new` / `init` / `add`（v0.2 按 cue 逐个进） |
| 单二进制项目，src/ 平铺 .cpp | Workspace / monorepo |
| cizon.toml 只认 `[project]`、`[compiler]` | 依赖管理（git/vcpkg/conda） |
| g++ / clang++ 探测，`--compiler` 临时切换 | MSVC（等有真实用户提出） |
| mtime + `-MMD` .d 文件级增量 | PCH、features、\[target\]、compile_commands.json |
| 直传编译器 stderr | 自定义错误格式化器 |

其他配置段不静默忽略：读到即明确报错（防止 `dependancies` 拼写错误之类静默陷阱）。

## 非目标（一票否决，写入喧需要重开讨论）

- monorepo / workspace / 多包
- 包管理生态（crates.io 式注册表）
- PCH 后端、build.cpp 逃生舱、metadata / env / tree / doctor
- 企业级 CI、与 CMake 全功能对齐
- 依赖白名单（serde、toml）之外的第三方 crate

## 三道闸（每合入一个功能前过一遍）

1. **狗粮闸**：每周在自己的真实 C++ 课程/小项目上用 cizon2，连续两周不用就停功能
2. **用户闸**：说服 1 个真人用它编译成功之前，不加新命令
3. **可读闸**：合并前，我必须能在不看注释的情况下讲清每个文件的逻辑

## 硬性约束

- 单 crate。依赖白名单：`serde`、`toml`，新增任何依赖必须在 PR/commit message 里写理由
- 不用 clap / rayon / sha2 —— v0.1 的需求手写 40 行就够
- 测试先行：每个模块带单测；端到端用真实 fixture 验证
- 提交粒度小：一个功能一个 commit

## 从 Cizon1 迁移了什么

- ✅ 编译器探测优先级（g++ > clang++）、gcc→g++/clang→clang++ 别名、@version 后缀忽略
- ✅ `-MMD -MF -MP` 依赖文件生成与解析（含 Windows 路径归一化、\ 续行拼接）
- ✅ 进程退出码翻译表（Windows NTSTATUS / POSIX signal → 人话提示）
- ✅ cargo 风格的分步彩色输出
- ✅ 编译器沙盒（简化为 `build/<mode>-<compiler>/` 作用域 + `.compiler-version`
  指纹，版本变更即作废；debug/release 与不同编译器产物物理隔离，禁止混链）
- ❌ 5-crate workspace 架构（过度设计，单文件够）
- ❌ MSVC / PCH / sha256 对象命名 / rayon / 错误格式化器（复杂度不成比例）

## 开发路线

1. ✅ build + run（本里程碑，当前）
2. v0.2: `new`（模板极简：cizon.toml + src/main.cpp）、`add`（git 仓库 + 锁文件）、
   `update`（锁生命周期第三环：按 cizon.toml 意图对齐锁，升/降同构；build 永远只执行锁中已定的事）
3. v0.3: compile_commands.json（编辑器支持，便宜且高感知）

每个里程碑只在狗粮闸和用户闸都亮绿灯时开始。

## 开发要求

1. 保持main分支的绝对干净,只提交可发布版本,其他feat/fix,全部新开分支,都提交到dev分支,确保可发布后合并到main
2. 不要自己新增功能,如果需要新功能的开发,先问开发者,征求同意.
3. 遇到小bug自己修,大问题先向开发者汇报征求意见.
