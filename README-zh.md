# Cizon2

[English](README.md) | 简体中文

声明式、渐进式的现代 C++ 构建工具。

用 `cizon.toml` 声明构建配置，在终端用简洁命令完成 C++ 编译与运行，以接近 cargo 的体验服务**中小型项目**。

当前版本：**v0.6.1**。目前主要面向 Windows x86-64（未编译 Linux/macOS 版及其他架构），支持 g++ 或 clang++，暂不支持 MSVC。

> **状态说明：** 本项目仅个人维护，仍在开发中，切勿用于真实项目。

## 特性

- 一个 `cizon.toml` 描述项目，没有构建脚本语言
- 自动探测 g++ / clang++；debug 与 release、不同编译器的产物物理隔离
- 按翻译单元增量：mtime + `-MMD`；头文件或编译参数变化会正确触发重编/重链
- 递归收集 `.cpp` / `.cc` / `.cxx`；`include/` 自动加入 `-I`
- 多个 bin、静态库、共享库 target；`links` 控制归档与 import lib 的顺序
- `shared_lib` 生成 DLL 与 import lib；import lib 未变时不重链 bin
- `cizon2 run <bin>` 选择运行目标，`build` 构建全部 target
- git 依赖 + 锁文件：`cizon2 add` / `cizon2 update`
- 可选生成 `compile_commands.json` 供 clangd / clang-tidy 使用；缺失时补 VS Code IntelliSense 配置
- 编译器警告按源文件分组高亮；构建状态区分 fresh / incremental / relinked / up to date
- 编译器 stderr 捕获后回放（非 UTF-8 内容按有损方式解码）；运行崩溃时把退出码翻译成人话

## 安装

构建与运行项目需要 Windows x86-64，以及 PATH 中的 `g++` 或 `clang++`；`new` / `help` / `version` 不需要编译器。Rust 工具链只在从源码构建时需要。

### 1. 从源码安装

```bash
cargo build --release
# 产物：target/release/cizon2.exe

# 或安装到 %USERPROFILE%\.cargo\bin
cargo install --path .
```

`cargo install --path .` 会把 `cizon2.exe` 装到 `%USERPROFILE%\.cargo\bin`；rustup 默认已把该目录加入 PATH，新开终端即可直接运行 `cizon2`。

如果只执行了 `cargo build --release`，把 `target\release\cizon2.exe` 复制到任意已在 PATH 中的目录，或按下一节把 `target\release` 加入 PATH。

### 2. 从 GitHub Release 下载

在 [Releases](https://github.com/xaoq23/Cizon2/releases) 页面下载 `cizon2-v0.6.1-windows-x86_64.exe` 与同名 `.sha256` 文件，不需要 Rust 工具链。移动到一个固定目录后，把该目录加入 PATH（见下一节）。

PowerShell 校验下载：

```powershell
Get-FileHash .\cizon2-v0.6.1-windows-x86_64.exe -Algorithm SHA256
Get-Content .\cizon2-v0.6.1-windows-x86_64.exe.sha256
```

两个 SHA-256 值应完全一致。

### 3. 把 cizon2 所在目录加入 PATH

PowerShell（把 `$dir` 换成 `target\release` 或存放下载 exe 的目录）：

```powershell
$dir = "C:\path\to\cizon2"
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
[Environment]::SetEnvironmentVariable("Path", "$userPath;$dir", "User")
```

重新打开终端后确认：

```powershell
Get-Command cizon2
cizon2 version
```

- 不要用 `setx PATH "%PATH%;..."`：它会把系统 PATH 并进用户 PATH，且超长时会被截断。
- `g++` / `clang++` 必须在 PATH 中；cizon2 在构建时探测编译器。缺少 g++ 时会回退到 clang++ 并打印提示；两者都没有则构建失败。
- MSYS2 UCRT64：把 `<MSYS2>\ucrt64\bin`（例如 `C:\Users\<user>\msys2\ucrt64\bin`）加入 PATH，并安装 `mingw-w64-ucrt-x86_64-gcc`。
- Git Bash 会继承 Windows 的 PATH，同样适用。

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

它不会执行 `git init`：版本控制交给用户自己决定。`.gitignore` 预先忽略了 `build/`、`deps/`、`compile_commands.json` 与 `.vscode/c_cpp_properties.json`。

## 命令

除 `new` 外，所有命令都在 `cizon.toml` 所在目录执行；cizon2 从当前目录读取配置。不带子命令运行 `cizon2` 会打印用法。

| 命令 | 作用 |
|---|---|
| `cizon2 new <路径>` | 创建项目骨架 |
| `cizon2 init` | 在当前目录初始化（已有文件不覆盖） |
| `cizon2 build [--release] [--compiler <spec>]` | 构建全部 target |
| `cizon2 run [--release] [--compiler <spec>] [<bin名>] [-- <程序参数...>]` | 构建全部 target 后运行指定 bin，并转发参数 |
| `cizon2 add <git-url> [--tag <tag>]` | 添加 git 依赖并写入锁文件 |
| `cizon2 update <依赖名>` | 按 `cizon.toml` 的意图对齐锁（升级/降级通用） |
| `cizon2 help` / `cizon2 version` | 帮助 / 版本 |

### `cizon2 new <路径>`

创建目录并写入 `cizon.toml`、`src/main.cpp`、`.gitignore`。项目名取路径的最后一段（`cizon2 new apps/hello` 得到项目名 `hello`）。

- 路径不能包含 `.` 或 `..` 组件，也不能以 `-` 开头
- 目录已存在时拒绝执行；不会覆盖已有文件
- 不执行 `git init`

### `cizon2 init`

在当前目录初始化单可执行项目，项目名取目录名。若当前目录已有 `cizon.toml`，或任意层级已存在 `main.cpp` / `main.cc` / `main.cxx`，则拒绝执行，避免覆盖用户代码。不接受参数。

### `cizon2 build`

构建配置中规划出的全部 bin、静态库与共享库，只编译过期对象。

| 选项 | 作用 |
|---|---|
| `--release` | 使用 `-O2`；默认 debug 使用 `-g` |
| `--compiler <spec>` | 本次构建临时指定编译器 |

- `--compiler` 可写 `g++`、`clang++` 或完整路径；裸名 `gcc` / `clang` 会归一化为 `g++` / `clang++`，`@版本` 后缀会被忽略（不做版本选择）
- 显式指定的编译器不可用时会直接报错，不会静默回退；未指定时按 g++ → clang++ 探测，g++ 缺失会打印回退提示
- 产物位于 `build/<mode>-<compiler>/`；不同模式或编译器之间不会混链
- `build` 只接受上面两个选项，不接受程序参数；要传参请用 `run`

```bash
cizon2 build
cizon2 build --release
cizon2 build --compiler clang++
```

### `cizon2 run`

先构建全部 target，再运行选中的 bin。bin 名大小写不敏感。

```bash
cizon2 run                        # 项目只有一个 bin 时可省略名字
cizon2 run worker                 # 多 bin 时选择 worker
cizon2 run worker -- --port 8080  # -- 之后的参数原样传给程序
```

- 项目有多个 bin 且未给名字时，报错并列出可选的 bin
- 只能给一个 bin 名；给两个会被拒绝
- 程序参数必须放在 `--` 之后；`--` 之前的未知 `-` 选项会被当作 cizon2 的参数并报错
- `run` 透传程序退出码；进程崩溃时额外打印退出码解释（例如 `ACCESS_VIOLATION`、`DLL_NOT_FOUND`）
- 不支持 `run --all`：多进程/IPC 编排交给外部 harness

给程序传参时请直接调用 `cizon2.exe`，或使用 PowerShell 别名（`Set-Alias cizon C:\path\to\cizon2.exe`）。PowerShell 函数包装可能在参数进入 cizon2 前吞掉裸 `--`。

### `cizon2 add <git-url> [--tag <tag>]`

浅克隆依赖到 `deps/<名字>`，向 `cizon.toml` 的 `[dependencies]` 追加条目，并写入 `cizon.lock`。依赖名取 URL 最后一段并去掉 `.git`，只允许字母、数字、`-` 和 `_`。

- `--tag` 可选；省略时克隆远端默认分支
- 依赖名已存在于 `[dependencies]`，或 `deps/<名字>` 已存在但不受管理时，拒绝执行
- 克隆或写文件失败会回滚 `cizon.toml`、`cizon.lock` 与 `deps/<名字>`
- 依赖缺少 `include/` 时打印布局警告，但不会阻止添加

```bash
cizon2 add https://github.com/fmtlib/fmt --tag 11.0.2
```

### `cizon2 update <依赖名>`

按 `cizon.toml` 中的 `git` / `tag` 重新解析依赖。升级和降级使用同一条命令：先改 `tag`，再执行 `update`。

- 依赖名必须在 `[dependencies]` 中存在
- `deps/<名字>` 有未提交改动时拒绝执行；需要保留的本地提交请先推送
- 先克隆到临时目录，成功后替换旧目录；失败时保留原有 checkout
- `build` 永远按 `cizon.lock` 的既定提交构建，不会自动追 `cizon.toml` 的新 `tag`

```bash
# 先把 cizon.toml 中的 tag 改成新版本
cizon2 update fmt
```

### `cizon2 help` / `cizon2 version`

打印用法或版本。也可使用 `--help` / `-h` 与 `--version` / `-V`。

## 构建输出

- `Compiling` / `Rebuilding g++ [n/m files]`：本次实际编译的翻译单元数 / 总数
- 编译器警告会按源文件分组：金色上下分隔线 + 黄色 `Warning <相对路径> (编译器 版本)` 头；诊断正文中的项目绝对路径前缀会替换为相对路径，行号与错误文本保持不变
- 警告不会让构建失败；需要严格模式时使用 `[compiler].flags = ["-Werror"]`
- 构建与 `Running` 状态写 stderr，程序 stdout 保持纯净，`cizon2 run > out.txt` 只包含程序输出
- 开启 `compile_commands` 且 VS Code 配置缺失时，会输出一次 `Writing .vscode/c_cpp_properties.json`（stderr）并生成该文件
- `Finished` 的状态：

| 状态 | 判定 |
|---|---|
| `fresh build` | 所有翻译单元都编译了（含首次构建、flags/编译器版本变化导致的整批重编） |
| `incremental rebuild` | 只重编了一部分翻译单元 |
| `relinked` | 没有翻译单元重编，仅重链（exe/DLL）或重建归档 |
| `up to date` | 无需编译和链接 |

## 项目结构

```
project/
├── cizon.toml          # 项目配置
├── cizon.lock          # 有依赖时生成，锁定 commit
├── include/            # 公开头，自动 -I（可以不存在）
│   └── shape/          # 可选：按库名分层，减少撞名
├── src/                # 未显式配置 sources 的 bin 默认扫描目录
├── lib/                # 静态库源码示例目录；不是自动收集目录
│   └── detail/         # 私有头示例
├── deps/               # git 依赖，自动忽略
└── build/              # 构建产物
```

目录结构是约定，不是硬编码的框架：`sources` 可以指向项目内任意相对路径（文件或目录）。`src/` 只在 target 未写 `sources` 时作为 bin 的默认扫描目录；静态库、共享库必须显式写 `sources`。

### 公开头与私有头

- `include/` 是唯一自动加入 `-I` 的目录，只放对外 API；bin 与库 target 都能包含
- `lib/` 或实现文件旁边放私有头与实现细节，默认不在 include 路径里
- 私有头用相对路径包含，如 `#include "detail/shape_impl.h"`；不要放进 `include/`
- 不要在 `[compiler].flags` 里加 `-Ilib`：flags 是全局的，会把私有目录同时暴露给 bin
- 公共头应自包含，不要依赖私有头或实现目录
- 单 exe、没有外部消费者的项目可以把头放在 `src/` 旁边；header-only 库则全部放 `include/`
- 将来 install/export 只发布 `include/` 与库产物，不发布 `lib/` 源码与私有头

## cizon.toml

cizon2 只识别四个顶层配置段：`[project]`、`[compiler]`、`[dependencies]` 和 `[[target]]`。未知配置段、以及已知段内的未知键都会直接报错，避免 `[dependancies]`、`compiler = "..."` 这类笔误被静默忽略。

最小配置只需要项目名；默认扫描 `src/`：

```toml
[project]
name = "hello"
```

完整示例：

```toml
[project]
name = "hello"
version = "0.1.0"           # 可选；仅元数据
edition = "c++23"           # 可选；缺省 -std=c++23
sources = ["src/main.cpp"]  # 可选；文件或目录；缺省扫描 src/
compile_commands = true     # 可选；生成编译数据库与 VS Code 配置

[compiler]
cpp = "clang++"             # 可选；缺省按 g++ → clang++ 探测
flags = ["-Wall"]           # 项目源码的额外编译参数
link_flags = ["-lm"]        # 追加到 bin / 共享库链接命令末尾

[dependencies]
fmt = { git = "https://github.com/fmtlib/fmt", tag = "11.0.2" }
```

### `[project]`

| 键 | 必填 | 缺省 | 说明 |
|---|---|---|---|
| `name` | 否 | 目录名 | 项目名；决定默认 bin 名与 exe 名 |
| `version` | 否 | 无 | 仅元数据，不参与编译或产物命名 |
| `edition` | 否 | `c++23` | 生成 `-std=...` |
| `sources` | 否 | 扫描 `src/` | 唯一 bin 的源码；仅在无 `[[target]]` 时可用 |
| `compile_commands` | 否 | `false` | 生成 `compile_commands.json` 与 VS Code 配置 |

- `name` 会成为文件名：不能包含路径分隔符或 Windows 保留字符，不能是 `.` / `..`、不能以 `.` 开头，也不能使用 `con`、`nul` 等设备名或 `obj` / `lib` / `deps-obj` 这些内部目录名。
- `version` 不能为空，也不能包含控制字符；当前没有生成头或编译宏，等真实消费者出现再扩展。
- `edition` 不做版本白名单：`c++XX` / `gnu++XX` 原样传给 `-std=`，裸值（如 `"17"`、`"23"`）会补成 `c++17`、`c++23`。拼错或编译器不支持时由编译器报错。
- `sources` 是项目内相对路径，可同时列文件和目录；目录会递归扫描 `.cpp` / `.cc` / `.cxx`。路径必须存在，不能是绝对路径或包含 `..`，目录也不能通过符号链接/junction 指向项目外。
- `sources` 与 `[[target]]` 互斥：一旦声明 target，源码必须写进各自的 target。省略 `sources` 时扫描 `src/`。
- `compile_commands = true` 时，每次 build 生成 `compile_commands.json`；若 `.vscode/c_cpp_properties.json` 缺失还会补一份（已有文件不覆盖）。关闭后不再更新，但不会删除已生成文件。

### `[compiler]`

| 键 | 必填 | 缺省 | 说明 |
|---|---|---|---|
| `cpp` | 否 | 自动探测 | `g++` → `clang++` |
| `flags` | 否 | 空 | 追加到项目源码的编译命令 |
| `link_flags` | 否 | 空 | 追加到 bin 与共享库的链接命令 |

- `cpp` 可写 `g++`、`clang++` 或完整路径；裸名 `gcc` / `clang` 归一化为 `g++` / `clang++`，`@版本` 后缀会被忽略。显式指定的编译器不可用时报错，不会回退；未指定时 g++ 缺失会回退到 clang++ 并打印提示。
- `flags` 作用于所有 project target 的源码，是全局的，没有 per-target flags；不要用 `-Ilib` 把私有目录暴露给所有 target。依赖源码按 `edition`、优化级与依赖 include 编译，不继承 `flags`。
- `link_flags` 只追加到 bin 和共享库的链接命令；静态库只归档、不链接。例如 GCC 下 `std::print` 需要 `-lstdc++exp`。

### `[dependencies]`

```toml
[dependencies]
fmt = { git = "https://github.com/fmtlib/fmt", tag = "11.0.2" }
```

- 每个依赖只支持 `git` 与 `tag` 两个键。键名即依赖名和 `deps/<名字>` 目录名，只允许字母、数字、`-`、`_`；`git` 必填，`tag` 可选（省略时使用远端默认分支）。
- `cizon.lock` 是构建的唯一真相：`build` 只执行锁中已确定的提交，不会自动追随尚未 `update` 的新 `tag`。
- 依赖是项目级的，不支持 per-target 依赖；添加、更新、锁恢复与目录布局见下文[依赖与锁文件](#依赖与锁文件)。

### `[[target]]`：库与 bin

一个项目可以带静态库、共享库 target，以及一个或多个可执行 target：

```toml
[[target]]
name = "shape"
kind = "static_lib"
sources = ["lib/shape.cpp"]   # 必填；文件或目录（目录递归扫描）

[[target]]
name = "shape_dll"
kind = "shared_lib"
sources = ["capi"]            # 必填；C ABI 包装层源码
links = ["shape"]             # 打进 DLL 的静态库

[[target]]
name = "app"
kind = "bin"
sources = ["src"]             # 可选；缺省扫描 src/
links = ["shape_dll", "shape"]  # 先静态归档，再共享库 import lib

[[target]]
name = "worker"
kind = "bin"
sources = ["worker"]
links = ["shape"]
```

每个 target 支持四个键：

| 键 | 必填 | 说明 |
|---|---|---|
| `name` | 视情况 | 单个 bin 可省略，默认项目名；库和多个 bin 必须显式命名 |
| `kind` | 是 | `bin` / `static_lib` / `shared_lib` |
| `sources` | 视情况 | bin 可省略并扫描 `src/`；库必填且不能为空 |
| `links` | 否 | bin 链接的库，或共享库打包的静态库 |

规则：

- 不写 `[[target]]` 时就是传统的单可执行项目，源码来自 `[project].sources` 或 `src/`
- 写了 `[[target]]` 后至少有一个 `kind = "bin"`；`[project].sources` 不能再使用
- 只有一个 bin 时 `name` 可省略并默认项目名；多个 bin 时每个都必须显式命名
- target 名大小写不敏感且不能重复，还必须满足文件名校验：不能是 `obj` / `lib` / `deps-obj`、不能以 `.` 开头、不能使用 Windows 设备名等；target 名叫 `shared` 是允许的，内部目录是 `.shared`
- `sources` 必须是项目内相对路径（不能是绝对路径或包含 `..`）；文件或目录均可，目录递归扫描；允许跨目录同名源文件
- `static_lib` 不支持 `links`；`shared_lib` 只能链接已存在的 `static_lib`；每个 `bin` 可以混合链接静态库与共享库，链接时先静态归档，再 import lib
- `links` 必须引用配置中已存在的 target，重复条目会报错；共享库不能链接其他共享库，DLL 之间的传递依赖交给 CMake
- 公共头放 `include/`（自动 `-I`）；库的私有头可放在源文件旁边
- 静态归档使用 `ar`（g++）或 `llvm-ar`（clang++）；产物为 `build/<mode>-<compiler>/lib/<name>/lib<name>.a`
- 共享库产物为 `build/<mode>-<compiler>/<name>.dll`，import lib 在 `build/<mode>-<compiler>/.shared/<name>/`（目前仅 Windows）
- Windows 下 MinGW 使用 `lib<name>.dll.a`，clang++（MSVC target）使用 `<name>.lib`；cizon2 显式传 import lib 参数
- 导出使用显式 C ABI：`extern "C" __declspec(dllexport)`。cizon2 不自动导出符号、不生成绑定，也不保证 C++ ABI
- MinGW 下 `shared_lib` 默认静态链接运行库，DLL 不依赖 `libstdc++`/`libgcc`/`libwinpthread`，Python `ctypes` 无需改 PATH。保持 C ABI 边界：DLL 与 exe 各有一套静态运行库时，不要跨边界传 C++ 对象或异常
- 每个 bin 产出 `build/<mode>-<compiler>/<name>.exe`；`cizon2 run <bin>` 会构建全部 bin 后运行指定项，只有项目恰好一个 bin 时才能省略名字
- 不支持 `run --all`：cizon2 不做多进程监督；IPC/多进程测试请用 shell/Python harness
- 对象路径镜像源相对路径：单 bin 为 `obj/<项目相对路径>.o`；多 bin 为 `obj/<bin>/<项目相对路径>.o`；静态库为 `lib/<name>/obj/...`，共享库为 `.shared/<name>/obj/...`，依赖对象在 `deps-obj/<dep>/`
- 同一目录下同名 stem（如 `util.cpp` + `util.cc`）仍会报错；改名即可
- `new` / `init` 只生成传统单可执行骨架；target 项目需手动创建源码并添加 `[[target]]`

### 从 Python 调用 DLL

```python
import ctypes

lib = ctypes.CDLL("build/debug-g++/shape_dll.dll")
print(lib.shape_area(3, 4))   # DLL 导出的 extern "C" 函数
```

Python 只需要 DLL；import lib 面向 C/C++ 消费者。Python 与 Rust `libloading` 都依赖 C ABI。Rust 若在链接期使用 import lib，需要匹配工具链：MinGW 用 `.dll.a`，MSVC 用 `.lib`。直接导出 C++ ABI 不在当前契约内；请用 C 包装或纯虚/opaque handle 接口。

## 依赖与锁文件

```bash
cizon2 add https://github.com/fmtlib/fmt --tag 11.0.2
cizon2 update fmt
```

- `cizon.lock` 是机器文件，锁定的 commit 是唯一真相；`build` 只执行锁里已经确定的事。
- `add` / `update` 做浅克隆；`build` 在 `deps/` 缺失时按锁恢复。
- 升级/降级都只改 `cizon.toml` 里的 `tag`，再运行 `cizon2 update <名字>`。
- `update` 会替换整个 `deps/<name>`；目录有未提交改动时会拒绝。需要保留的本地提交请先推送到远端并更新 `cizon.toml` 后再更新。
- 依赖约定布局：`include/` 头文件 + 平铺的 `src/*.cpp`。其他布局或需要构建脚本的库请使用 CMake。
- `deps/<name>` 缺少 `include/` 时，`add` 和 `build` 会给出布局警告；非标准布局请改用该库自己的构建系统，并通过 `link_flags` 接入。
- 依赖源码对象会链接进 bin 与每个共享库；依赖是项目级的，不支持 per-target 依赖。
- `cizon.toml`、`cizon.lock` 的 git/tag、`deps/` 的 HEAD 与工作区不一致都会大声报错；任一依赖目录缺失时按锁恢复，不静默自愈。

## 编辑器（clangd / VS Code）

在 `cizon.toml` 中开启：

```toml
[project]
compile_commands = true
```

运行 `cizon2 build` 后，项目根会生成 `compile_commands.json`，包含项目源与依赖源的完整编译参数。关闭 `compile_commands` 后 cizon2 不再更新数据库、也不再生成 VS Code 配置；两份文件若不再需要请手动删除。

微软 C/C++ 扩展不会自动读取 `compile_commands.json`。因此开启 `compile_commands` 后，若 `.vscode/c_cpp_properties.json` 不存在，cizon2 会顺手生成一份，指向编译数据库、探测到的编译器与 `include/`；已有文件一律不覆盖，删掉后下次 build 会重新生成。写入失败时构建继续，只打一条警告。clangd 不读这个文件。新项目的 `.gitignore` 已忽略它；老项目若不想提交机器相关的编译器路径，请自行补上该行。

如果 clangd 在数据库生成之前就已经在运行，需要重启一次语言服务（VS Code：`clangd: Restart language server`）；之后数据库更新会被自动感知。MinGW/g++ 环境下 clangd 若仍找不到标准库，可在 clangd 参数中加 `--query-driver=<g++.exe 路径>`。

## 调试（VSCode）

`cizon2 build` 默认是 debug 构建（`-g`），产物路径固定，但断点调试需要 VSCode 的调试扩展（cpptools + gdb）接管进程；`cizon2 run` 本身只是直接执行产物。

一次性配置 `.vscode/tasks.json`：

```json
{
  "version": "2.0.0",
  "tasks": [
    {
      "label": "cizon2: build",
      "type": "shell",
      "command": "cizon2",
      "args": ["build"],
      "options": { "cwd": "${workspaceFolder}" },
      "problemMatcher": ["$gcc"]
    }
  ]
}
```

`.vscode/launch.json`：

```json
{
  "version": "0.2.0",
  "configurations": [
    {
      "name": "cizon2: debug (g++)",
      "type": "cppdbg",
      "request": "launch",
      "program": "${workspaceFolder}/build/debug-g++/hello.exe",
      "args": [],
      "stopAtEntry": false,
      "cwd": "${workspaceFolder}",
      "environment": [],
      "externalConsole": false,
      "MIMode": "gdb",
      "miDebuggerPath": "C:/Users/<user>/msys2/ucrt64/bin/gdb.exe",
      "preLaunchTask": "cizon2: build"
    }
  ]
}
```

- `program` 的 `hello.exe` 换成 bin target 名（省略 name 时为项目名）；`debug-g++` 换成实际的 `<mode>-<compiler>` 作用域
- `miDebuggerPath` 指向本机 gdb；`cizon2` 不在 PATH 时把 task 的 `command` 改成完整路径
- g++/MinGW 用 `cppdbg + gdb`；clang++（MSVC target）需要 cpptools 的 `cppvsdbg` 或 LLDB 扩展，配置方式不同
- MSYS2 UCRT64 下用 `pacman -S mingw-w64-ucrt-x86_64-gdb` 安装 gdb
- 用 `--release` 的产物调试体验较差；断点调试请使用默认 debug

## 增量与沙盒

- 源文件或头文件（经 `.d` 解析）变新：只重编受影响的文件
- 项目源码的编译参数（edition、优化级、`[compiler].flags`、依赖 include 集合）变化：项目对象失效并重编；依赖源码只跟随 edition、优化级与依赖 include，不继承 `[compiler].flags`
- 对象路径镜像源相对路径；旧版本对象布局会在首次构建时被识别并全量重编一次
- 链接输入（对象清单、静态库归档、`link_flags`）变化：强制重链
- 静态库源变化：只重建对应归档并重链；只改 bin 不会重建归档
- 共享库源变化：重链 DLL；import lib 指纹不变时不重链 bin；导出变化才重链 bin
- 删除或重命名 shared target 会清理 DLL、import lib 与 `.shared` 对象
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

## 使用注意

- 一个项目有一个或多个可执行 target；`cizon2 run <bin>` 选择运行，只有恰好一个 bin 时才能省略名字
- 不支持 `run --all`；多进程/IPC 编排请放在外部 harness
- 静态库产物是 `.a`（GNU 归档）；`shared_lib` 产出 DLL 与 import lib
- 共享库通过 C ABI 给 Python/Rust 使用；不保证直接导出 C++ ABI
- 多个 DLL 与 exe 放在同一目录，Windows loader 不需要额外 PATH
- `include/` 是唯一自动 `-I` 的目录；私有头不要放进去，也不要用全局 `-Ilib` 暴露
- `sources` 必须是项目内相对路径；旧对象布局首次构建会全量重编一次
- `[project].version` 当前仅作元数据，不参与编译或产物命名
- 同一项目不支持并发构建；请勿在多个终端同时运行 `cizon2 build`
- 关闭 `compile_commands` 不会删除已有的 `compile_commands.json` 或 `.vscode/c_cpp_properties.json`，不再需要时手动删除
- 配置段或已知段内的未知键会直接报错，不会静默忽略
- 编译器警告（例如忽略 `[[nodiscard]]` 返回值）不会让构建失败；需要严格模式时使用 `-Werror`

## 后续改进（未排期）

以下问题已识别，但尚未承诺实现；按真实需求与 AGENTS.md 的闸门推进。

### 调试集成（未排期）

目标是让断点调试成为一等开发体验，但 **cizon2 不实现调试器、DAP/MI 协议或断点 UI**；断点、变量和调用栈仍由 gdb / lldb / cppvsdbg 负责。

- 产物契约：默认 debug `-g`、不 strip，产物路径固定为 `build/debug-<compiler>/<bin>.exe`
- `cizon2 run` 保持直接执行，不改成调试器启动器；调试通过编辑器的 launch 配置 + `preLaunchTask` 构建
- 触发方式优先 `[project] vscode = true`（默认关闭）：build 时按当前 plan 生成 `.vscode/launch.json` 与 `.vscode/tasks.json`；备选是一次性的 `build --vscode`，独立 `debug` 命令会触发“不加新命令”的闸门，暂不考虑
- 生成策略非破坏：已有文件不覆盖；只补缺失文件或打印可粘贴的配置片段；目标改名/切编译器后重新 build 刷新
- 调试器选择：g++/MinGW → `cppdbg` + gdb；clang++（MSVC target）→ `cppvsdbg`/PDB；clang++（GNU target）的 LLDB 配置后置
- v1 先用 float_test 验收 g++/MinGW + gdb；测试覆盖配置内容、作用域/路径、已有文件不覆盖、找不到 gdb 时明确报错，不需要真正启动调试器
- 终端内 `cizon2 run --debug` 不能提供 VSCode 断点，不作为替代方案

### 测试 target（v0.7 设计）

- `kind = "test"` 与 bin 共用编译/链接链路，但 `build` / `run` 默认忽略 test target；只有 `cizon test` 构建并运行它们
- 框架无关：唯一契约是进程退出码 0；`cizon test [name...] -- <args>` 支持按 target 筛选与参数透传，失败时转发子进程输出
- 不做自动发现、框架适配或依赖下载；gtest / Catch2 v3 通过显式 `system_links` 接入，doctest / Catch2 v2 走 header-only
- CTest 的 labels / fixtures / parallel / timeout 等先不做，等真实需求
- 设计先于实现：用真实项目 dogfood 并通过三道闸后再开工

### 可靠性

- 链接器直接写 exe、锁文件/`cizon.toml` 目前也不是断电级原子写；可增加临时产物替换
- 没有项目级构建锁；两个终端同时 build 仍不受支持
- 关闭 `compile_commands` 不清理已生成的 `compile_commands.json` 与 `.vscode/c_cpp_properties.json`
- 删除依赖后 `cizon.lock`/`deps/` 不自动清理；可能需要独立的 remove 命令（会触发二级闸）

### 诊断与工程

- 配置结构错误来自 serde，`unknown field` 等提示为英文（低优先级风格项）
- CI 在 tag 上构建 release；测试在本地跑，缺少编译器/git 时会跳过
- `rustfmt`/`clippy` 尚未接入

### 等真实需求再开

- target 间传递依赖、per-target flags/include
- install/export、一次性 CMake 导出命令
- MSVC、交叉编译、外部包管理
- `[project].version` 的生成头/编译宏消费者
- 编译器常见安装目录发现与版本选择

## 什么时候该用 CMake

Cizon2 面向中小型项目（一个或多个 bin，可带静态库/共享库 target）。出现以下需求时，请直接使用 CMake：

- 嵌套 target 图、库之间的传递依赖、per-target flags/include
- install/export/find_package/打包发布、DLL 版本号/SONAME
- 共享库依赖共享库、直接分发 C++ ABI
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

推送 `v*` 标签会触发 `.github/workflows/release.yml`：构建 Windows 二进制、上传 `cizon2-v<version>-windows-x86_64.exe` 与 SHA-256 文件，并创建或更新 GitHub Release。手动触发可指定已存在的 tag（例如 `v0.6.0`）。测试仍在本地跑，CI 不执行测试。

更详细的边界、闸门与路线见[AGENTS.md](AGENTS.md)。
