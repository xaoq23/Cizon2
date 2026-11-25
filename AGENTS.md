# Cizon — Agent Instructions

> 接手开发前先读 `HANDOFF_FOR_NEXT_AGENT（先读这个）.md`：其中包含当前发布状态、近期决策与会话上下文。

## 定位

个人开发者的声明式、渐进式现代 C++ 构建工具。使命：`cizon build && cizon run`，达到 cargo/uv 级的开箱即用。

### 设计哲学

1. 面向中小型项目的声明式、渐进式构建工具：封装编译器差异与复杂参数，只暴露最简 CLI，降低认知负担。
2. 开发边界取决于我对 C++ 的认知，绝不做认知以外的东西，除非有真实用户提出需求。
3. 不做 CMake 的替代品。CMake 的生态是二十年积累的结果；需要条件化/大型 target 图、install/export、代码生成时，由 CMake 接手，cizon 全身而退；不要试图用 TOML 模仿图灵完备的 DSL。
4. 做现代 C++ 的布道者，全面面向新的现代 C++ 项目。
5. 做 agent 友好的工具：文本声明、CLI 输出，尽量减少 AI 手写 CMakeLists 导致的构建失败。

上一个项目 Cizon1 因 scope creep 膨胀到一万一千行。本项目的原则是边界优先于功能。

## 当前边界（v0.10.2）

| 在边界内 | 边界外 |
|---|---|
| `build` / `run` / `test` / `sync` / `clean` / `new` / `init` / `add` / `update` | 独立包/workspace 生命周期、多包注册表 |
| 中小型单项目（多个 bin/test + 静态库/共享库 `[[target]]`；无环静态库公共依赖与 include 传播） | 条件化/大型 target 图 / install-export / 共享库依赖共享库 |
| `[subproject]` 受限本地组合：单一根 build/lock/compiler/edition，递归包含库/test，子项目无 bin | 子项目独立 compiler/edition/deps/lock、独立构建/发布、workspace 命令 |
| Linux `shared_lib`：`.so`、`-fPIC`、`$ORIGIN` rpath | MSVC / 交叉编译 / 复杂工具链 |
| CI 原语：`sync --check`（只校验）、`--quiet` | `init --ci`、企业级 CI |
| `kind = "test"` + `cizon test`：构建/运行 test target，退出码 0 即通过 | 测试框架适配、自动发现、测试依赖下载/版本锁定 |
| test target 的 `extern_links`（仅 `-l<name>`）+ per-target `prefix`（推导 `-I`/`-L`） | vcpkg / Conan / conda / CMake provision |
| `[compiler] strict`/`werror` 警告开关；`help <command>\|toml` 详细帮助；target `include`/`defines` PUBLIC 传播 | 任意 per-target 编译 flags、PRIVATE/INTERFACE include 与宏定义作用域 |
| cizon.toml 认 `[project]`、`[dependencies]`、`[compiler]`、`[[target]]`、`[subproject]` | PCH、features、install/export |
| g++ / clang++ 探测，`--compiler` 临时切换 | MSVC（等真实用户提出） |
| mtime + `-MMD` 增量 + 编译/链接/库状态指纹 | 交叉编译 / 复杂工具链 |
| compile_commands.json + VS Code IntelliSense 配置（默认关闭，`[project].compile_commands` 开启） | 自定义错误格式化器 |
| 编译器 stderr 捕获回放（UTF-8 有损解码）、退出码翻译 | 与 CMake 全功能对齐 |

未知配置段与已知段内的未知键一律报错，不静默忽略。

## 项目结构约定

- 根 `include/` 只放项目级公开头（唯一自动的项目公开头 `-I`；依赖目录 `deps/<name>`、`deps/<name>/include` 也会自动加入）；多库项目把公共头留在各库自己的 `include/`，用 target `include` 传播，不要复制/搬到根目录；私有头放 `lib/` 或实现文件旁边，默认不暴露
- 不要用 `[compiler].flags` 加 `-Ilib` 等方式把私有目录暴露给所有 target
- 公共头必须自包含，不能依赖私有头；单 exe 项目可把头放 `src/`，header-only 库全部放 `include/`
- `[[target]]` 的 sources 是项目内相对路径（文件或目录）；至少一个 target（声明 `[[target]]` 即退出隐式单 exe 模式，`src/` 不再自动构建，未被 target 覆盖的源码会告警）；纯库项目合法：`build` 产库，`run` 报没有 bin，`test` 报没有 test target，建议加 test target 验证，install/export 交给 CMake；多个 bin 必须各自命名；test 必须显式命名且提供 sources
- target `include` 是项目内公共头根目录：库的 include 沿 `links` PUBLIC 传播（可传递），bin/test 只作用于自身；与根 `include/` 叠加；路径必须在 plan 阶段校验存在、是目录、在项目内且不是项目根。`prefix` 是消费已安装包的输入，只作用于当前 target，不传播
- target `defines` 接受 `NAME` / `NAME=VALUE` 简单对象宏；库的 defines 沿 `links` PUBLIC 传播，bin/test 只作用于自身。target defines 位于全局 `flags` 之后并覆盖同名全局定义、不产生重定义警告，同一闭包同宏不同值报错、同值去重；git 依赖源码不继承，test 源码在传播后剥离 NDEBUG。CLI `-D` / `--define` 仅本次进程生效，最后追加覆盖 TOML 定义，不写 config/lock、不传 git 依赖，并进入编译指纹
- 隐式单 bin（无 `[[target]]`）用 `[project].defines` 传入宏，语法与优先级同 target defines；声明 `[[target]]` 后 `[project].defines` 报错，宏改由各 target `defines` 声明
- 公开头相对路径冲突在 plan 阶段检查：根 `include/`、target/库 include 与自动依赖 include 共同参与，Windows 不区分大小写；include 目录链接会被跟随，并做项目内约束与规范路径环保护。两个依赖 include 根之间的冲突仍按既有依赖搜索顺序允许
- `[subproject]` 不是 workspace：根统一 compiler/edition/deps/lock；子项目只提供 static_lib/shared_lib/test，路径必须在根内，依赖 git URL 必须绝对、URL 或 SCP 风格 SSH，target 名全局唯一且不区分大小写；声明文件与子项目自身都必须有 `[[target]]`。依赖名不区分大小写，但 key 拼写必须一致；完全重复声明去重，git/tag 冲突或仅大小写不同的重复报错。嵌套允许、环报错、菱形去重
- `new` / `init` 默认生成隐式单 bin；`--kind static_lib|shared_lib` 生成 target 库项目，公共头放 `include/<name>/`，不复制到根 `include/`
- `static_lib` 可链接 `static_lib`（公共依赖），`shared_lib` 只能链接 `static_lib`，bin/test 可混合链接；静态依赖图按依赖方在前自动排序，环直接报错（不加 `--start-group`）
- bin 的 name 在只有一个 bin 时可省略并默认项目名；多个 bin 必须显式命名；对象路径镜像源相对路径，同目录同 stem 冲突
- 内置宏：`CIZON_PROJECT_NAME`（字符串，始终定义）与 `CIZON_PROJECT_VERSION`（字符串，仅在声明 `[project].version` 时定义）注入所有项目 target，不注入 git 依赖；内置宏最先注入、无覆盖机制，`CIZON_` 前缀保留给 cizon（用户在 `defines`、CLI `-D` 或 `[compiler].flags` 中定义即报错）；改动触发全项目重编
- release 构建为项目与依赖源码定义 `-DNDEBUG`（`-O2 -DNDEBUG`）；test target 不继承 `-DNDEBUG`，避免 assert 测试静默空转；test 对象独立存放在 `build/<mode>-<compiler>/test-obj/<name>/`，`build` / `run` 不构建 test target
- `extern_links` 仅 test target 可写，按声明顺序映射为链接期 `-l<name>`；不探测、不下载、不锁版本；头文件在默认搜索路径或由 `prefix` 提供
- git 依赖约定为 `deps/<name>/include/` 头文件与平铺的 `src/*.cpp`；cizon 直接 checkout 并编译这些源码，不执行依赖自带的构建/安装步骤；其他布局交给 CMake
- 外部依赖的 cmake 构建树/安装产物收进 `deps/.build/<name>`、`deps/.install/<name>`（checkout 之外的兄弟目录），不散落项目根；`prefix` 指向安装根并推导 `-I`/`-L`
- `compile_commands = true` 时，若 `.vscode/c_cpp_properties.json` 缺失则生成（不覆盖已有文件），供微软 C/C++ 扩展使用
- 项目级构建锁为 `build/.cizon-build.lock`：并发 build 明确报错；exe、状态文件、`cizon.lock`、`compile_commands.json` 通过临时文件替换写入（Unix 原子 rename，Windows 先删后 rename）
- `shared_lib`：Windows 产 DLL + import lib，Linux 产 `.so`（对象 `-fPIC`，消费者按名链接并带 `$ORIGIN` rpath，带无版本 SONAME `lib<name>.so`；带版本的 SONAME/SOVERSION 属于 install/export，交给 CMake）；导出必须使用 C ABI；产物与 exe 同目录，import lib 在 `.shared/<name>/`（仅 Windows），内部目录保留给 cizon

## 输出约定

- `cizon help` 与逐命令帮助采用 Cargo 风格分节：Usage、Options、Commands；命令列表只列动词，具体参数放到对应命令帮助；stdout 为 TTY 时标题/分节加粗、命令名与选项着色，重定向时自动关闭颜色
- 用户面路径用项目相对路径；编译器诊断保留行号与文本，只把项目绝对路径前缀替换为相对路径
- 构建/运行状态写 stderr，程序 stdout 保持纯净；ANSI 颜色按流判断 TTY，重定向时自动关闭
- `--verbose` 在 stderr 打印实际 compiler/archive/link/run/test 进程命令，允许显示原子替换用的临时输出名；与 `--quiet` 互斥
- `cizon clean` 只删除项目 `build/`，不得顺带删除 config、lock、deps、compile_commands 或编辑器配置；`build` 为符号链接/junction 时只删链接不跟随目标；真实 build 目录存在活动构建锁时拒绝清理
- 计时单位紧贴数字（`0.12s`），RESET 不能夹在数字与单位之间，避免剥离转义码后出现 `ms` 假象
- 警告按源文件分组：黄色 `Warning` 头；警告段与构建状态之间空一行，不打印装饰分割线
- `Finished` 严格区分 `fresh build` / `incremental rebuild` / `relinked` / `up to date`
- `cizon test` 的测试进程输出统一转发到 stderr；cizon 汇总 `passed/failed/total`，任一失败退出 1
- `run` / `test` 子进程 PATH 前置编译器 bin 目录，规避旧运行库 DLL 遮蔽；退出码表覆盖 `0xC0000139`（入口点缺失）
- 依赖布局提示仅在 `add` 打印一次（区分 header-only 与需编译库）；bin/test target 的 sources 重叠会警告

## 非目标（重开需明确讨论）

- 独立包/workspace 生命周期、monorepo 多包 registry、crates.io 式包注册表；`[subproject]` 仅限单一根构建下的本地库/test 组合
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
- git commit 信息一律使用英文（保留 `feat:` / `fix:` / `docs:` 等类型前缀）。
- CI 在 `v*` tag 或手动 `workflow_dispatch` 上构建并发布 release；Windows 测试在本地跑，Linux 测试在 CI 跑（`.so` 的唯一门禁）。
- 源码注释、文档注释和用户面字符串一律使用英文；`README-zh.md` 保留中文文档。
- `README.md` 是英文主文档，`README-zh.md` 是中文版；修改文档时两份同步。
- 单 crate；手写参数解析，不用 clap，不引入 rayon / sha2。
- 用户不明确要求时，默认 debug 构建（`cizon build`）。

## 已知待办（未排期）

- VSCode 断点调试当前靠手工 `.vscode/launch.json` + `tasks.json`；未来优先按 `[project] vscode = true` 非破坏生成（不实现调试器，gdb 先行），详见 README「后续改进」
- 候选工程改进：compile_commands 关闭清理、依赖移除后的锁/目录清理
- 详细清单与真实需求触发条件见 README「后续改进」；开工前按三道闸确认

## 路线

1. v0.1 `build` / `run` —— 已发布 v0.1.0
2. v0.2 `new` / `init` / `add` / `update`，git 依赖与锁生命周期 —— 已发布 v0.2.0
3. v0.2.1 递归 `src/`、`include/` 自动 `-I`、配置键严格校验 —— 随 v0.3.0 发布
4. v0.3 compile_commands.json（默认关闭）—— 已发布 v0.3.0
5. v0.4 `[[target]]`：单 bin + 多静态库 —— 已发布 v0.4.2
6. v0.4.3 `compile_commands` 开启时补 VS Code IntelliSense 配置 —— 已发布 v0.4.3
7. v0.4.4 run 参数提示 + 源码注释/字符串全英文 —— 已发布 v0.4.4
8. v0.5 `shared_lib`：DLL + import lib，bin 链接多个 DLL，C ABI —— 已发布 v0.5.0
9. v0.5.1 `shared_lib` 依赖对象传播修复 —— 已发布 v0.5.1
10. v0.6 多 bin target + `run <bin>` —— 已发布 v0.6.0
11. v0.6.1 编译器降级提示 + 缺失运行库诊断 —— 已发布 v0.6.1
12. v0.7 `kind = "test"` + `cizon test` —— 已发布 v0.7.1：对齐 CTest 的框架无关上层，只构建/运行 test target，退出码 0 即通过；`build` / `run` 忽略 test；`cizon test [name...] -- <args>` 支持过滤与参数透传；显式 `extern_links` 接入 gtest / Catch2 v3，不内建框架适配、不下载依赖；test target 永不定义 `-DNDEBUG`。v0.7.1 追加：per-target `prefix`（透明推导 `-I`/`-L`）、`[compiler] strict`/`werror` 开关、`sync` 命令、`help <command>` 与 `--version/-v`、run/test 子进程 PATH 前置编译器 bin、`0xC0000139` 诊断、依赖布局警告分级去重、sources 重叠警告。已在真实项目完成 dogfood（gtest/doctest/Catch2/原生断言）。
13. v0.8 Linux 共享库 + CI 原语 —— 已发布 v0.8.0：Linux `shared_lib` 产 `.so`（`-fPIC`、按名链接、`$ORIGIN` rpath），补上 Linux 唯一功能缺口；`sync --check`（只校验锁与 checkout，不联网、不写）与 `--quiet`（CI 静默进度）。MSVC / 交叉编译 / CMake File API 拆为本地实验分支（`experimental/*`），不在 0.8 边界内。
14. v0.8.1–v0.9.5 维护与体验 —— 已发布；v0.9.6 为可靠性批次：原子写与项目级构建锁、include/链接/target 删除加固测试。此前包含显式 target 诊断、输出体验、纯库与 release NDEBUG、target 公共 include 与静态公共依赖、库项目模板、Linux SONAME、改名 `cizon`、edition 校验、`help toml`。
15. v0.10 target 组合与构建体验 —— 提供 `[subproject]` 受限本地组合、target `defines` PUBLIC 传播、隐式单 bin 的 `[project].defines`、临时 CLI `-D` 覆盖、`cizon clean`、`--verbose` 与 Cargo 风格彩色帮助；不是 workspace，不引入独立包生命周期。v0.10.1 为补丁版本，修复 Linux 发布测试的可移植性，功能集不变。v0.10.2 为审计修复补丁：CI tag 输入注入防护、依赖 URL 校验与 update 状态一致性、`<command> --help` 等价性、重复头检查覆盖与构建/clean 竞态加固；功能集不变。
16. 之后：需要条件化/大型 target 图、install/export、MSVC 或外部包管理时交给 CMake；是否提供一次性导出命令，等真实需求与二级闸。
