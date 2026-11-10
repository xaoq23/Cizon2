# Cizon2

[English](README.md) | 简体中文

零配置的 C++ 构建与运行工具：`cizon2 build`、`cizon2 run`，像 cargo/uv 一样开箱即用，不用先学一套构建脚本语言。

当前版本：**v0.6.1**。主要面向 Windows x86-64（未编译 Linux/macOS 版及其他架构），支持 g++ 或 clang++，暂不支持 MSVC。

> **状态说明：** 本项目仅个人维护，仍在开发中，切勿用于真实项目。

## 特性

- 一个 `cizon.toml` 描述项目，没有构建脚本
- 自动探测 g++ / clang++；debug 与 release、不同编译器的产物物理隔离
- 按翻译单元增量：mtime + `-MMD`；头文件或编译参数变化会正确触发重编/重链
- 现代 C++ 目录结构：递归收集源文件，`include/` 自动加入 `-I`
- v0.4：一个可执行 target + 多个静态库 target，静态库自动归档，`links` 控制归档/import lib 顺序
- v0.5：`shared_lib` 生成 DLL 与 import lib；bin 可链接多个 DLL，import lib 未变时不重链 bin
- v0.6：多个 bin target；`cizon2 run <bin>` 选择运行，`build` 构建全部
- git 依赖 + 锁文件：`cizon2 add` / `cizon2 update`
- 可选生成 `compile_commands.json` 供 clangd / clang-tidy 使用；缺失时补 VS Code IntelliSense 配置
- 编译器警告按源文件分组高亮；构建状态区分 fresh / incremental / relinked / up to date
- 编译器 stderr 捕获后回放（非 UTF-8 内容按有损方式解码）；运行崩溃时把退出码翻译成人话

## 安装

需要 Rust 工具链，以及 PATH 中的 `g++` 或 `clang++`。

```bash
cargo build --release
# 产物：target/release/cizon2（Windows 为 cizon2.exe）

# 或安装到 ~/.cargo/bin
cargo install --path .
```

### 让 `cizon2` 在命令行中直接可用

`cargo install --path .` 是最省事的方式：它把 `cizon2.exe` 装到 `%USERPROFILE%\.cargo\bin`，rustup 默认已把该目录加入 PATH，新开终端即可直接运行 `cizon2`。

如果只执行了 `cargo build --release`，可以二选一：

- 把 `target\release\cizon2.exe` 复制到任意已在 PATH 中的目录；
- 或把 `target\release` 加入当前用户的 PATH。PowerShell：

```powershell
$dir = "C:\path\to\Cizon2\target\release"
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
[Environment]::SetEnvironmentVariable("Path", "$userPath;$dir", "User")
```

重新打开终端后确认：

```powershell
Get-Command cizon2
cizon2 version
```

- 不要用 `setx PATH "%PATH%;..."`：它会把系统 PATH 并进用户 PATH，且超长时会被截断。
- `g++` / `clang++` 也必须在 PATH 中；cizon2 在构建时探测编译器。
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

| 命令 | 作用 |
|---|---|
| `cizon2 new <目录名>` | 创建项目骨架 |
| `cizon2 init` | 在当前目录初始化（已有文件不覆盖） |
| `cizon2 build [--release] [--compiler <spec>]` | 构建 |
| `cizon2 run [--release] [--compiler <spec>] [<bin名>] [-- <程序参数...>]` | 构建后运行指定的 bin 并转发参数 |
| `cizon2 add <git-url> [--tag <tag>]` | 添加 git 依赖并写入锁文件 |
| `cizon2 update <依赖名>` | 按 `cizon.toml` 的意图对齐锁（升级/降级通用） |
| `cizon2 help` / `cizon2 version` | 帮助 / 版本 |

默认是 debug 构建（`-g`）；`--release` 使用 `-O2`。产物位于 `build/<mode>-<compiler>/`，不同模式与编译器之间禁止混链。

`--compiler` 可以写 `g++`、`clang++` 或完整路径；裸名 `gcc` / `clang` 会归一化为 `g++` / `clang++`，`@版本` 后缀会被忽略（不做版本选择）。

给程序传参时请直接调用 `cizon2`，或使用 PowerShell 别名（`Set-Alias cizon C:\path\to\cizon2.exe`）。函数包装可能在参数进入 cizon2 前吞掉裸 `--`。

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
- 将来 install/export 只发布 `include/` 与库产物，不发布 `lib/` 源码与私有头

## cizon.toml

```toml
[project]
name = "hello"
version = "0.1.0"           # 可选；当前仅元数据，不参与编译/产物命名
edition = "c++23"           # 也接受 c++17 / gnu++20 / 裸版本号 "17"
sources = ["src/main.cpp"]  # 可选；文件或目录（目录递归）；缺省扫描 src/
compile_commands = true     # 可选；生成编译数据库与 VS Code 配置

[compiler]
cpp = "clang++"             # 可选；缺省按 g++ > clang++ 探测
flags = ["-Wall"]
link_flags = ["-lm"]

[dependencies]
fmt = { git = "https://github.com/fmtlib/fmt", tag = "11.0.2" }
```

未知的配置段、以及已知段内的未知键都会直接报错，避免 `[dependancies]`、`compiler = "..."` 这类笔误被静默忽略。

### 库与 bin target（v0.4 / v0.5 / v0.6）

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

规则：

- 不写 `[[target]]` 时保持传统单可执行项目，配置语义不变；0.4.1 首次构建会迁移对象布局（全量重编一次）
- 写了 `[[target]]` 后至少有一个 `kind = "bin"`；`[project].sources` 不能再使用
- 只有一个 bin 时 name 可省略并默认项目名；多个 bin 时每个都必须显式命名
- `static_lib` 不支持 `links`；`shared_lib` 只能链接 `static_lib`；每个 `bin` 可以混合链接，先静态归档，再 import lib
- 共享库不能链接其他共享库；DLL 之间的传递依赖交给 CMake
- 公共头放 `include/`（自动 `-I`）；库的私有头可放在源文件旁边
- 静态归档使用 `ar`（g++）或 `llvm-ar`（clang++）；产物为 `build/<mode>-<compiler>/lib/<name>/lib<name>.a`
- 共享库产物为 `build/<mode>-<compiler>/<name>.dll`，import lib 在 `build/<mode>-<compiler>/.shared/<name>/`（v0.5 仅 Windows）
- Windows 下 MinGW 使用 `lib<name>.dll.a`，clang++（MSVC target）使用 `<name>.lib`；cizon2 显式传 import lib 参数
- 导出使用显式 C ABI：`extern "C" __declspec(dllexport)`。cizon2 不自动导出符号、不生成绑定，也不保证 C++ ABI
- MinGW 下 `shared_lib` 默认静态链接运行库，DLL 不依赖 `libstdc++`/`libgcc`/`libwinpthread`，Python `ctypes` 无需改 PATH。保持 C ABI 边界：DLL 与 exe 各有一套静态运行库时，不要跨边界传 C++ 对象或异常
- 每个 bin 产出 `build/<mode>-<compiler>/<name>.exe`；`cizon2 run <bin>` 会构建全部 bin 后运行指定项，只有项目恰好一个 bin 时才能省略名字
- 不支持 `run --all`：cizon2 不做多进程监督；IPC/多进程测试请用 shell/Python harness
- target/项目名大小写不敏感、不能重复，也不能是 `obj` / `lib` / `deps-obj` 或以 `.` 开头；target 名叫 `shared` 是允许的，内部目录是 `.shared`
- sources 必须是项目内相对路径（不能是绝对路径或包含 `..`）；允许跨目录同名源文件
- 对象路径镜像源相对路径：单 bin 为 `obj/<项目相对路径>.o`；多 bin 为 `obj/<bin>/<项目相对路径>.o`；静态库为 `lib/<name>/obj/...`，共享库为 `.shared/<name>/obj/...`，依赖对象在 `deps-obj/<dep>/`
- 同一目录下同名 stem（如 `util.cpp` + `util.cc`）仍会报错；改名即可
- `new` / `init` 只生成传统单可执行骨架；target 项目需手动创建源码并添加 `[[target]]`

### 从 Python 调用 DLL

```python
import ctypes

lib = ctypes.CDLL("build/debug-g++/shape_dll.dll")
print(lib.shape_area(3, 4))   # DLL 导出的 extern "C" 函数
```

Python 只需要 DLL；import lib 面向 C/C++ 消费者。Python 与 Rust `libloading` 都依赖 C ABI。Rust 若在链接期使用 import lib，需要匹配工具链：MinGW 用 `.dll.a`，MSVC 用 `.lib`。直接导出 C++ ABI 不在 v0.5 契约内；请用 C 包装或纯虚/opaque handle 接口。

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
- 项目 flags（edition、优化级、用户 flags、依赖 include）变化：bin 与库对象作废；依赖 flags 变化只影响依赖对象
- 对象路径镜像源相对路径；布局升级后首次构建会自动清空旧对象并全量重编
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
- sources 必须是项目内相对路径；0.4.1 首次构建会迁移对象布局并全量重编一次
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

- 多可执行 target、target 间传递依赖、per-target flags/include
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
