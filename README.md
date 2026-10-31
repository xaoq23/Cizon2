# Cizon2

零配置的 C++ 构建与运行工具：`cizon2 build`、`cizon2 run`，像 cargo/uv 一样开箱即用，不用先学一套构建脚本语言。

当前版本：**v0.4.1**（dev 分支，待发布）。主要面向 Windows x86-64(未编译linux/macos版以及其他架构)，支持使用 g++ 或 clang++(暂不支持 MSVC)。

## 特性

- 一个 `cizon.toml` 描述项目，没有构建脚本
- 自动探测 g++ / clang++；debug 与 release、不同编译器的产物物理隔离
- 文件级增量：mtime + `-MMD`；编译参数或链接输入变化会正确触发重编/重链
- 现代 C++ 目录结构：递归收集 `src/`，`include/` 自动加入 `-I`
- v0.4：一个可执行 target + 多个静态库 target，静态库自动归档，`links` 按顺序链接
- git 依赖 + 锁文件：`cizon2 add` / `cizon2 update`
- 可选生成 `compile_commands.json`，供 clangd / clang-tidy 使用
- 编译器 stderr 捕获后回放（非 UTF-8 内容按有损方式解码）；运行崩溃时把退出码翻译成人话

## 安装

需要 Rust 工具链，以及 PATH 中的 `g++` 或 `clang++`。

```bash
cargo build --release
# 产物：target/release/cizon2（Windows 为 cizon2.exe）

# 或安装到 ~/.cargo/bin
cargo install --path .
```

## 快速开始

```bash
cizon2 new hello
cd hello
cizon2 run
# Hello, World!
```

`new` 生成的文件：

```
hello/
├── cizon.toml
├── .gitignore
└── src/
    └── main.cpp
```

它不会执行 `git init`：版本控制交给用户自己决定。`.gitignore` 预先忽略了 `build/`、`deps/` 与 `compile_commands.json`。

## 命令

| 命令 | 作用 |
|---|---|
| `cizon2 new <目录名>` | 创建项目骨架 |
| `cizon2 init` | 在当前目录初始化（已有文件不覆盖） |
| `cizon2 build [--release] [--compiler <spec>]` | 构建 |
| `cizon2 run [--release] [--compiler <spec>] [-- <程序参数...>]` | 构建后运行并转发参数 |
| `cizon2 add <git-url> [--tag <tag>]` | 添加 git 依赖并写入锁文件 |
| `cizon2 update <依赖名>` | 按 `cizon.toml` 的意图对齐锁（升级/降级通用） |
| `cizon2 help` / `cizon2 version` | 帮助 / 版本 |

默认是 debug 构建（`-g`）；`--release` 使用 `-O2`。产物位于 `build/<mode>-<compiler>/`，不同模式与编译器之间禁止混链。

`--compiler` 可以写 `g++`、`clang++` 或完整路径；裸名 `gcc` / `clang` 会归一化为 `g++` / `clang++`，`@版本` 后缀会被忽略（不做版本选择）。

## 项目结构

```
project/
├── cizon.toml          # 项目配置
├── cizon.lock          # 有依赖时生成，锁定 commit
├── include/            # 公开头，自动 -I（可以不存在）
│   └── shape/          # 可选：按库名分层，减少撞名
├── src/                # bin 的源文件（传统项目递归收集）
├── lib/                # 静态库源码与私有头（不自动 -I）
│   └── detail/         # 私有头示例
├── deps/               # git 依赖，自动忽略
└── build/              # 构建产物
```

### 公开头与私有头

- `include/` 是唯一自动加入 `-I` 的目录，只放对外 API；bin 与库 target 都能包含
- `lib/` 或实现文件旁边放私有头与实现细节，默认不在 include 路径里
- 私有头用相对路径包含，如 `#include "detail/shape_impl.h"`；不要放进 `include/`
- 不要在 `[compiler].flags` 里加 `-Ilib`：flags 是全局的，会把私有目录同时暴露给 bin
- 公共头应自包含，不要依赖私有头或实现目录
- 单 exe、没有外部消费者的项目可以把头放在 `src/` 旁边；header-only 库则全部放 `include/`
- 将来 install/export 只发布 `include/` 与静态库，不发布 `lib/` 源码与私有头

## cizon.toml

```toml
[project]
name = "hello"
version = "0.1.0"           # 可选；当前仅元数据，不参与编译/产物命名
edition = "c++23"           # 也接受 c++17 / gnu++20 / 裸版本号 "17"
sources = ["src/main.cpp"]  # 可选；文件或目录（目录递归）；缺省扫描 src/
compile_commands = true     # 可选；默认关闭

[compiler]
cpp = "clang++"             # 可选；缺省按 g++ > clang++ 探测
flags = ["-Wall"]
link_flags = ["-lm"]

[dependencies]
fmt = { git = "https://github.com/fmtlib/fmt", tag = "11.0.2" }
```

未知的配置段、以及已知段内的未知键都会直接报错，避免 `[dependancies]`、`compiler = "..."` 这类笔误被静默忽略。

### 静态库 target（v0.4）

一个项目可以带多个静态库 target，但必须恰好有一个可执行 target：

```toml
[[target]]
name = "shape"
kind = "static_lib"
sources = ["lib/shape.cpp"]   # 必填；文件或目录（目录递归扫描）

[[target]]
name = "app"
kind = "bin"
sources = ["src"]             # 可选；缺省扫描 src/
links = ["shape"]             # 按顺序链接静态库
```

规则：

- 不写 `[[target]]` 时保持传统单可执行项目，配置语义不变；0.4.1 首次构建会迁移对象布局（全量重编一次）
- 写了 `[[target]]` 后必须恰好一个 `kind = "bin"`；`[project].sources` 不能再使用
- 静态库暂不支持 `links`（v0.4 不做库依赖库）
- `bin` 的 `name` 可省略：省略时使用 `[project].name`（缺省为目录名），并需满足 target 命名规则；`static_lib` 的 `name` 必填
- 公共头放 `include/`（自动 `-I`）；库的私有头可放在源文件旁边
- 归档使用 `ar`（g++）或 `llvm-ar`（clang++）；产物为 `build/<mode>-<compiler>/lib/<name>/lib<name>.a`
- `cizon2 run` 始终运行唯一的 bin；显式 `bin.name` 决定产物名，省略时等于项目名（Windows 加 `.exe`）
- target/项目名大小写不敏感、不能重复，也不能是 `obj` / `lib` / `deps-obj` 或以 `.` 开头
- sources 必须是项目内相对路径（不能是绝对路径或包含 `..`）；允许跨目录同名源文件
- 对象路径镜像源相对路径：bin 为 `obj/<项目相对路径>.o`，库为 `lib/<name>/obj/<项目相对路径>.o`；依赖对象在 `deps-obj/<dep>/`
- 同一目录下同名 stem（如 `util.cpp` + `util.cc`）仍会报错；改名即可
- `new` / `init` 只生成传统单可执行骨架；target 项目需手动创建 `lib/` 并添加 `[[target]]`

## 依赖

```bash
cizon2 add https://github.com/fmtlib/fmt --tag 11.0.2
cizon2 update fmt
```

- `cizon.lock` 是机器文件，锁定的 commit 是唯一真相；`build` 只执行锁里已经确定的事。
- `add` / `update` 做浅克隆；`build` 在 `deps/` 缺失时按锁恢复。
- 升级/降级都只改 `cizon.toml` 里的 `tag`，再运行 `cizon2 update <名字>`。
- `update` 会替换整个 `deps/<name>`；目录有未提交改动时会拒绝。需要保留的本地提交请先推送到远端并更新 `cizon.toml` 后再更新。
- 依赖约定布局：`include/` 头文件 + 平铺的 `src/*.cpp`。其他布局或需要构建脚本的库请使用 CMake。
- `cizon.toml`、`cizon.lock` 的 git/tag、`deps/` 的 HEAD 与工作区不一致都会大声报错；任一依赖目录缺失时按锁恢复，不静默自愈。

## 编辑器（clangd）

在 `cizon.toml` 中开启：

```toml
[project]
compile_commands = true
```

运行 `cizon2 build` 后，项目根会生成 `compile_commands.json`，包含项目源与依赖源的完整编译参数。关闭 `compile_commands` 后 cizon2 不再更新该文件；若不再需要请手动删除。

如果 clangd 在数据库生成之前就已经在运行，需要重启一次语言服务（VS Code：`clangd: Restart language server`）；之后数据库更新会被自动感知。MinGW/g++ 环境下 clangd 若仍找不到标准库，可在 clangd 参数中加 `--query-driver=<g++.exe 路径>`。

## 增量与沙盒

- 源文件或头文件（经 `.d` 解析）变新：只重编受影响的文件
- 项目 flags（edition、优化级、用户 flags、依赖 include）变化：bin 与库对象作废；依赖 flags 变化只影响依赖对象
- 对象路径镜像源相对路径；布局升级后首次构建会自动清空旧对象并全量重编
- 链接输入（对象清单、静态库归档、`link_flags`）变化：强制重链
- 静态库源变化：只重建对应归档并重链；只改 bin 不会重建归档
- 编译器版本变化：`build/<mode>-<compiler>/` 整目录作废
- 同一项目不支持并发构建：请勿同时在两个终端运行 `cizon2 build`

## 故障排查

### GCC 下使用 `<print>` / `std::println` 链接失败

报错形如：

```
undefined reference to `std::__open_terminal(_iobuf*)'
undefined reference to `std::__write_to_terminal(void*, std::span<char, ...>)'
```

原因：libstdc++ 把 C++23 `<print>` 头里的函数归入实验库 `libstdc++exp`，需要显式链接；g++ 不会自动添加这个库。这是 libstdc++ 的打包方式，不是 MinGW 独有，也不是 cizon2 的问题。

在 `cizon.toml` 的 `[compiler]` 中把它写进链接参数：

```toml
[compiler]
link_flags = ["-lstdc++exp"]
```

cizon2 会把 `link_flags` 追加到链接命令末尾，加上后 `std::print` / `std::println` 即可正常链接。

替代方案：

- 改用 clang++（MSVC 目标，使用 MSVC STL）：不需要额外库；若 clang++ 仍使用 libstdc++，同样需要 `-lstdc++exp`
- 不使用 `<print>`，改用 `std::format` + `std::cout`

## 什么时候该用 CMake

Cizon2 面向中小型单可执行项目（可带静态库 target）。出现以下需求时，请直接使用 CMake：

- 多个可执行 target、嵌套 target 图、库之间的传递依赖
- 共享库（dll/so）、install / export / find_package / 打包发布
- 代码生成（protobuf、Qt moc）、自定义构建步骤
- MSVC、交叉编译、复杂工具链条件
- vcpkg / Conan 等外部包管理

这些目前都不支持

## 开发

```bash
cargo test
cargo build --release
```

单 crate，仅依赖 `serde` 与 `toml`；不引入 clap / rayon / sha2。
功能与修复提交到 `dev`，确认可发布后合入 `main`。

更详细的边界、闸门与路线见[AGENTS.md](AGENTS.md)。
