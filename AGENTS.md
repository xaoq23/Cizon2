# Cizon2 — Agent Instructions

## 定位

个人开发者的声明式、渐进式现代 C++ 构建工具。使命：`cizon2 build && cizon2 run`，达到 cargo/uv 级的开箱即用。

### 设计哲学

1. 面向中小型项目的声明式、渐进式构建工具：封装编译器差异与复杂参数，只暴露最简 CLI，降低认知负担。
2. 开发边界取决于我对 C++ 的认知，绝不做认知以外的东西，除非有真实用户提出需求。
3. 不做 CMake 的替代品。CMake 的生态是二十年积累的结果；需要 target 图、install/export、代码生成时，由 CMake 接手，cizon2 全身而退；不要试图用 TOML 模仿图灵完备的 DSL。
4. 做现代 C++ 的布道者，全面面向新的现代 C++ 项目。
5. 做 agent 友好的工具：文本声明、CLI 输出，尽量减少 AI 手写 CMakeLists 导致的构建失败。

上一个项目 Cizon1 因 scope creep 膨胀到一万一千行。本项目的原则是边界优先于功能。

## 当前边界（v0.8 开发中）

| 在边界内 | 边界外 |
|---|---|
| `build` / `run` / `test` / `sync` / `new` / `init` / `add` / `update` | Workspace / monorepo / 多包 |
| 中小型单项目（多个 bin/test + 静态库/共享库 `[[target]]`） | 嵌套 target 图 / install-export / 共享库依赖共享库 |
| `kind = "test"` + `cizon test`：构建/运行 test target，退出码 0 即通过 | 测试框架适配、自动发现、测试依赖下载/版本锁定 |
| bin/test target 的 `extern_links`（`-l<name>`，或匹配 CMake 依赖的库目标，含传递链接）+ per-target `prefix`（推导 `-I`/`-L`） | vcpkg / Conan / conda 等包管理器 |
| `[compiler] strict`/`werror` 警告开关；`help <command>` 详细帮助 | 完整 per-target flags/include 作用域 |
| cizon.toml 认 `[project]`、`[dependencies]`、`[compiler]`、`[[target]]` | PCH、features、install/export |
| g++ / clang++ / MSVC（`cl.exe`，含 vswhere 发现）探测，`--compiler` 临时切换 | MSVC 交叉编译（x86 ↔ x64） |
| mtime + `-MMD` 增量 + 编译/链接/库状态指纹 | 复杂工具链（超出单一 target triple） |
| compile_commands.json + VS Code IntelliSense 配置（默认关闭，`[project].compile_commands` 开启） | 自定义错误格式化器 |
| 交叉编译：`[compiler] target` / `--target` + 可选 `sysroot`，构建目录按目标隔离（`build/<mode>-<compiler>-<triple>/`） | 用 qemu 等方式运行交叉产物（cizon2 不搭建） |
| CMake 依赖：`add` 探测 `CMakeLists.txt` 后用 File API（codemodel v2）查询、构建、安装，`deps/.build` / `deps/.install`，指纹缓存 | CMake 依赖的公开编译定义转发、`shared_lib` 链接 CMake 依赖、非 `lib`/`lib64`/`bin` 的自定义安装目的地 |
| CI：`sync --check`（只校验）、build/run/test 的 `--quiet`、`init --ci github`（ubuntu/windows/macos 工作流） | `init --ci` 的其他 CI 提供方、超出上述三件的企业级 CI |
| 编译器 stderr 捕获回放（UTF-8 有损解码）、退出码翻译 | 与 CMake 全功能对齐 |

未知配置段与已知段内的未知键一律报错，不静默忽略。

## 项目结构约定

- `include/` 只放公开头（唯一自动 `-I`）；私有头放 `lib/` 或实现文件旁边，默认不暴露
- 不要用 `[compiler].flags` 加 `-Ilib` 等方式把私有目录暴露给所有 target
- 公共头必须自包含，不能依赖私有头；单 exe 项目可把头放 `src/`，header-only 库全部放 `include/`
- `[[target]]` 的 sources 是项目内相对路径（文件或目录）；至少一个 bin 或 test target；多个 bin 必须各自命名；test 必须显式命名且提供 sources；shared_lib 只能链接 static_lib，bin/test 可混合链接静态库与共享库
- bin 的 name 在只有一个 bin 时可省略并默认项目名；多个 bin 必须显式命名；`[project].version` 仅元数据；对象路径镜像源相对路径，同目录同 stem 冲突
- test target 不继承 `-DNDEBUG`，避免 assert 测试静默空转；test 对象独立存放在 `test-obj/<name>/`，`build` / `run` 不构建 test target
- `extern_links` 由 bin 与 test target 写（库 target 不可写），按声明顺序映射为链接期 `-l<name>`；名字匹配到 CMake 依赖的库目标时带上它的传递链接；系统库不探测、不下载、不锁版本；头文件在默认搜索路径或由 `prefix` 提供
- 外部依赖的 cmake 构建树/安装产物收进 `deps/.build/<name>`、`deps/.install/<name>`（checkout 之外的兄弟目录），不散落项目根；`prefix` 指向安装根并推导 `-I`/`-L`
- `compile_commands = true` 时，若 `.vscode/c_cpp_properties.json` 缺失则生成（不覆盖已有文件），供微软 C/C++ 扩展使用
- `shared_lib`：Windows 产 DLL + import lib，Linux 产 `.so`（对象以 `-fPIC` 编译，bin 按名字链接并以 rpath `$ORIGIN` 查找）；导出必须使用 C ABI；DLL/`.so` 与 exe 同目录，import lib 在 `.shared/<name>/`（仅 Windows），内部目录保留给 cizon2

## 输出约定

- 用户面路径用项目相对路径；编译器诊断保留行号与文本，只把项目绝对路径前缀替换为相对路径
- 构建/运行状态写 stderr，程序 stdout 保持纯净
- 警告按源文件分组：金色上下分隔线 + 黄色 `Warning` 头
- `Finished` 严格区分 `fresh build` / `incremental rebuild` / `relinked` / `up to date`
- `cizon test` 的测试进程输出统一转发到 stderr；cizon2 汇总 `passed/failed/total`，任一失败退出 1
- `run` / `test` 子进程 PATH 前置编译器 bin 目录，规避旧运行库 DLL 遮蔽；退出码表覆盖 `0xC0000139`（入口点缺失）
- 依赖布局提示仅在 `add` / `update` 打印一次（区分 header-only 与需编译库）；bin/test target 的 sources 重叠会警告

## 非目标（重开需明确讨论）

- monorepo / workspace / 多包、crates.io 式包注册表
- PCH、build.cpp 逃生舱、metadata / env / tree / doctor
- 超出 `sync --check` / `--quiet` / `init --ci github` 的企业级 CI
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
- CI 只在 `v*` tag 上构建并发布 release；测试仍在本地跑。`init --ci github` 生成的工作流在 ubuntu/windows/macos 上跑 `sync`/`build`/`test`（cizon2 自身的 release 流程仍按 tag 触发）。
- 源码注释、文档注释和用户面字符串一律使用英文；`README-zh.md` 保留中文文档。
- `README.md` 是英文主文档，`README-zh.md` 是中文版；修改文档时两份同步。
- 单 crate；手写参数解析，不用 clap，不引入 rayon / sha2。
- 用户不明确要求时，默认 debug 构建（`cizon2 build`）。

## 已知待办（未排期）

- VSCode 断点调试当前靠手工 `.vscode/launch.json` + `tasks.json`；未来优先按 `[project] vscode = true` 非破坏生成（不实现调试器，gdb 先行），详见 README「后续改进」
- 候选工程改进：exe/锁文件原子替换、项目级构建锁、compile_commands 关闭清理、依赖移除后的锁/目录清理
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
13. v0.8 CMake 依赖 + MSVC + 交叉编译 + CI 支持 —— 开发中（工作树，未提交、未发布）：`add` 集成 CMake File API（codemodel v2 解析、指纹缓存的配置/构建/安装、`extern_links` 展开含传递链接）；MSVC `cl.exe`/`link.exe`/`lib.exe`（探测、flag 映射、`.obj`/`.lib`、`/sourceDependencies` JSON 依赖文件，单元测试覆盖，**未在真实 Windows 上验证**）；交叉编译 `[compiler] target` / `--target` + 可选 `sysroot`（GCC 带前缀二进制、clang `--target`、构建目录按目标隔离、CMake `-DCMAKE_SYSTEM_NAME`/`-DCMAKE_SYSTEM_PROCESSOR`，已用 aarch64-linux-gnu 在 g++ 与 clang++ 上验证）；CI（`sync --check`、`--quiet`、`init --ci github`）。已知 v1 限制见 README「v0.8」一节。
14. 之后：需要嵌套 target 图、install/export 或外部包管理时交给 CMake；是否提供一次性导出命令，等真实需求与二级闸。
