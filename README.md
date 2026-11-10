# Cizon2

[简体中文](README-zh.md) | English

Zero-config C++ build and run: `cizon2 build`, `cizon2 run` — out of the box like cargo/uv, with no build script language to learn first.

Current version: **v0.5.0**. Primarily targets Windows x86-64 (no Linux/macOS builds or other architectures); supports g++ or clang++, not MSVC yet.

> **Status:** personally maintained and still under active development. Do not use it for real projects yet.

## Features

- One `cizon.toml` describes the project; there are no build scripts
- Auto-detects g++ / clang++; debug vs release and different compilers are isolated on disk
- Translation-unit-level incrementality: mtime + `-MMD`; header or compile-flag changes trigger the right rebuilds and relinks
- Modern C++ layout: sources are collected recursively, `include/` is added to `-I` automatically
- v0.4: one executable target plus multiple static library targets; archives are built automatically and `links` controls archive/import-library order
- v0.5: `shared_lib` targets build DLLs and import libraries; the bin can link multiple DLLs, and unchanged import libraries do not force a bin relink
- Git dependencies with a lock file: `cizon2 add` / `cizon2 update`
- Optional `compile_commands.json` for clangd / clang-tidy, plus a VS Code IntelliSense config when missing
- Compiler warnings grouped by source file and highlighted; build status distinguishes fresh / incremental / relinked / up to date
- Compiler stderr is captured and replayed (lossy decoding for non-UTF-8); crash exit codes are translated into plain language

## Installation

Requires a Rust toolchain and `g++` or `clang++` on PATH.

```bash
cargo build --release
# artifact: target/release/cizon2 (cizon2.exe on Windows)

# or install into ~/.cargo/bin
cargo install --path .
```

### Calling `cizon2` from the command line

`cargo install --path .` is the shortest route: it puts `cizon2.exe` in `%USERPROFILE%\.cargo\bin`, which rustup keeps on PATH, so a new terminal can run `cizon2` directly.

If you only ran `cargo build --release`, either copy `target\release\cizon2.exe` into a directory already on PATH, or add the release directory to your user PATH:

```powershell
$dir = "C:\path\to\Cizon2\target\release"
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
[Environment]::SetEnvironmentVariable("Path", "$userPath;$dir", "User")
```

Reopen the terminal, then verify:

```powershell
Get-Command cizon2
cizon2 version
```

- Do not use `setx PATH "%PATH%;..."`: it folds the system PATH into the user PATH and truncates long values.
- `g++` / `clang++` must be on PATH as well; cizon2 probes the compiler at build time.
- MSYS2 UCRT64: add `<MSYS2>\ucrt64\bin` (for example `C:\Users\<user>\msys2\ucrt64\bin`) to PATH and install `mingw-w64-ucrt-x86_64-gcc`.
- Git Bash inherits the Windows PATH, so the same setting applies there.

## Quick start

```bash
cizon2 new hello
cd hello
cizon2 run
# Hello, World!
```

Files created by `new`:

```
hello/
├── cizon.toml
├── .gitignore
└── src/
    └── main.cpp
```

It does not run `git init`: version control is your decision. The generated `.gitignore` already ignores `build/`, `deps/`, `compile_commands.json`, and `.vscode/c_cpp_properties.json`.

## Commands

| Command | Purpose |
|---|---|
| `cizon2 new <directory>` | Create a project skeleton |
| `cizon2 init` | Initialize in the current directory (existing files are not overwritten) |
| `cizon2 build [--release] [--compiler <spec>]` | Build |
| `cizon2 run [--release] [--compiler <spec>] [-- <program args...>]` | Build, then run and forward arguments |
| `cizon2 add <git-url> [--tag <tag>]` | Add a git dependency and write the lock file |
| `cizon2 update <dependency>` | Align the lock with the intent in `cizon.toml` (upgrade or downgrade) |
| `cizon2 help` / `cizon2 version` | Help / version |

The default is a debug build (`-g`); `--release` uses `-O2`. Artifacts live under `build/<mode>-<compiler>/`; linking across modes or compilers is forbidden.

`--compiler` accepts `g++`, `clang++`, or a full path. Bare `gcc` / `clang` are normalized to `g++` / `clang++`, and an `@version` suffix is ignored (no version selection).

When passing arguments to the program, call `cizon2` directly or use a PowerShell alias (`Set-Alias cizon C:\path\to\cizon2.exe`). A PowerShell function wrapper can consume the bare `--` before cizon2 receives it.

## Build output

- `Compiling` / `Rebuilding g++ [n/m files]`: translation units actually compiled this run / total
- Compiler warnings are grouped per source file: gold top and bottom rules plus a yellow `Warning <relative path> (compiler version)` header; project absolute path prefixes in diagnostic bodies are replaced with relative paths, while line numbers and message text stay untouched
- Warnings do not fail the build; for strict mode use `[compiler].flags = ["-Werror"]`
- Build and `Running` status go to stderr, keeping program stdout clean: `cizon2 run > out.txt` contains only program output
- With `compile_commands` enabled, a missing VS Code config prints `Writing .vscode/c_cpp_properties.json` once (stderr) and is then created
- `Finished` states:

| Status | Meaning |
|---|---|
| `fresh build` | Every translation unit was compiled (first build, or a full rebuild after flag/compiler-version changes) |
| `incremental rebuild` | Only some translation units were recompiled |
| `relinked` | No translation units were recompiled; only a link (exe/DLL) or an archive was rebuilt |
| `up to date` | Nothing to compile or link |

## Project layout

```
project/
├── cizon.toml          # project configuration
├── cizon.lock          # created when dependencies exist; pins commits
├── include/            # public headers, automatic -I (optional)
│   └── shape/          # optional: one directory per library to avoid name clashes
├── src/                # bin sources (collected recursively in traditional projects)
├── lib/                # static library sources and private headers (no automatic -I)
│   └── detail/         # example private header
├── deps/               # git dependencies, ignored automatically
└── build/              # build artifacts
```

### Public and private headers

- `include/` is the only directory added to `-I` automatically; it holds the public API and is visible to both the bin and library targets
- Private headers and implementation details belong in `lib/` or next to the implementation files and stay off the include path by default
- Include private headers with relative paths, e.g. `#include "detail/shape_impl.h"`; never put them in `include/`
- Do not add `-Ilib` to `[compiler].flags`: flags are global and would expose private directories to the bin as well
- Public headers should be self-contained and must not depend on private headers or implementation directories
- Single-executable projects without external consumers may keep headers next to `src/`; header-only libraries keep everything in `include/`
- Future install/export will publish only `include/` and library artifacts, never `lib/` sources or private headers

## cizon.toml

```toml
[project]
name = "hello"
version = "0.1.0"           # optional; metadata only, not used for compilation or artifact names
edition = "c++23"           # also accepts c++17 / gnu++20 / a bare version "17"
sources = ["src/main.cpp"]  # optional; file or directory (recursive); defaults to scanning src/
compile_commands = true     # optional; generates compile_commands.json and VS Code config

[compiler]
cpp = "clang++"             # optional; defaults to probing g++ > clang++
flags = ["-Wall"]
link_flags = ["-lm"]

[dependencies]
fmt = { git = "https://github.com/fmtlib/fmt", tag = "11.0.2" }
```

Unknown sections, and unknown keys inside known sections, are hard errors, so typos such as `[dependancies]` or `compiler = "..."` are never silently ignored.

### Library targets (v0.4 / v0.5)

A project may carry static and shared library targets, but must have exactly one executable target:

```toml
[[target]]
name = "shape"
kind = "static_lib"
sources = ["lib/shape.cpp"]   # required; file or directory (recursive)

[[target]]
name = "shape_dll"
kind = "shared_lib"
sources = ["capi"]            # required; the C ABI wrapper sources
links = ["shape"]             # static libraries built into the DLL

[[target]]
name = "app"
kind = "bin"
sources = ["src"]             # optional; defaults to scanning src/
links = ["shape_dll", "shape"]  # static archives first, then shared import libraries
```

Rules:

- Without `[[target]]`, the project stays a traditional single-executable project with unchanged semantics; the first build in 0.4.1 migrates the object layout (one full rebuild)
- With `[[target]]`, exactly one `kind = "bin"` is required, and `[project].sources` can no longer be used
- `static_lib` does not support `links`; `shared_lib` may link `static_lib` only; `bin` may link both, with static archives first, then import libraries
- Shared libraries cannot link other shared libraries; transitive DLL graphs are a CMake job
- `bin` may omit `name`; it then defaults to `[project].name` (or the directory name) and must satisfy the target naming rules. `static_lib` / `shared_lib` must have a `name`
- Public headers go in `include/` (automatic `-I`); a library's private headers may sit next to its sources
- Static archives use `ar` (g++) or `llvm-ar` (clang++); the artifact is `build/<mode>-<compiler>/lib/<name>/lib<name>.a`
- A shared library produces `build/<mode>-<compiler>/<name>.dll` plus an import library under `build/<mode>-<compiler>/.shared/<name>/` (Windows only in v0.5)
- On Windows, MinGW uses `lib<name>.dll.a` and clang++ (MSVC target) uses `<name>.lib`; cizon2 passes the import-library flag explicitly
- Exports are explicit C ABI: write `extern "C" __declspec(dllexport)`. cizon2 does not auto-export symbols, generate bindings, or guarantee the C++ ABI
- On MinGW, `shared_lib` links its runtime statically by default, so the DLL has no `libstdc++`/`libgcc`/`libwinpthread` dependency; Python `ctypes` can load it without touching PATH. Keep the boundary C ABI: with separate static runtimes, do not pass C++ objects or exceptions across the DLL boundary
- `cizon2 run` always runs the single bin; an explicit `bin.name` determines the artifact name, otherwise the project name is used (plus `.exe` on Windows)
- Target/project names are case-insensitive, must be unique, and cannot be `obj` / `lib` / `deps-obj` or start with `.`; a target named `shared` is allowed because the internal directory is `.shared`
- `sources` must be project-relative paths (no absolute paths or `..`); same-named sources in different directories are allowed
- Object paths mirror source paths: `obj/<project-relative-path>.o` for the bin, `lib/<name>/obj/<project-relative-path>.o` for static libraries, `.shared/<name>/obj/<project-relative-path>.o` for shared libraries, and `deps-obj/<dep>/` for dependency objects
- Same stem in the same directory (e.g. `util.cpp` + `util.cc`) is still an error; rename one
- `new` / `init` only generate the traditional single-executable skeleton; target projects need manually created sources and `[[target]]` entries

### Using a DLL from Python

```python
import ctypes

lib = ctypes.CDLL("build/debug-g++/shape_dll.dll")
print(lib.shape_area(3, 4))   # an extern "C" function exported by the DLL
```

Python only needs the DLL; the import library exists for C/C++ consumers. Python and Rust `libloading` callers both rely on the C ABI. Rust link-time consumers must match the import-library flavor: MinGW uses `.dll.a`, MSVC uses `.lib`. Direct C++ ABI export is not part of the v0.5 contract; use a C wrapper or a pure-virtual/opaque-handle interface.

## Dependencies

```bash
cizon2 add https://github.com/fmtlib/fmt --tag 11.0.2
cizon2 update fmt
```

- `cizon.lock` is a machine file; the pinned commit is the single source of truth, and `build` only does what the lock already determines.
- `add` / `update` shallow-clone; `build` restores from the lock when `deps/` is missing.
- To upgrade or downgrade, change only the `tag` in `cizon.toml`, then run `cizon2 update <name>`.
- `update` replaces the entire `deps/<name>` tree and refuses when the directory has uncommitted changes. To keep local commits, push them and update the `tag` first.
- Dependencies follow a fixed layout: headers in `include/` plus flat `src/*.cpp`. For other layouts or libraries that need a build system, use CMake.
- When `deps/<name>` has no `include/`, `add` and `build` print a layout warning; for non-standard layouts, use the library's own build system and wire it in via `link_flags`.
- Any mismatch among the git/tag in `cizon.toml`/`cizon.lock`, the `deps/` HEAD, and the working tree is a loud error; a missing dependency directory is restored from the lock, never silently self-healed.

## Editors (clangd / VS Code)

Enable in `cizon.toml`:

```toml
[project]
compile_commands = true
```

After `cizon2 build`, the project root gets `compile_commands.json` containing full compile flags for project and dependency sources. With `compile_commands` off, cizon2 no longer updates the database or creates the VS Code config; delete them manually if you no longer need them.

The Microsoft C/C++ extension does not read `compile_commands.json` on its own. When `compile_commands` is enabled, cizon2 therefore also creates `.vscode/c_cpp_properties.json` if that file is missing, pointing it at the generated database, the detected compiler, and `include/`. Existing files are never overwritten; delete the file and rebuild to regenerate it. If the file cannot be written, the build continues with a warning. clangd ignores this file. New projects' `.gitignore` excludes it; add the same line to older projects if you do not want to commit machine-specific compiler paths.

If clangd was already running before the database was generated, restart the language server once (VS Code: `clangd: Restart language server`); later database updates are picked up automatically. If clangd still cannot find the standard library in a MinGW/g++ setup, add `--query-driver=<path to g++.exe>` to clangd's arguments.

## Debugging (VS Code)

`cizon2 build` is a debug build (`-g`) by default with a fixed artifact path, but breakpoint debugging requires VS Code's debug extension (cpptools + gdb) to own the process; `cizon2 run` itself only executes the artifact.

One-time `.vscode/tasks.json`:

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

`.vscode/launch.json`:

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

- Replace `hello.exe` in `program` with the bin target name (the project name when `name` is omitted); replace `debug-g++` with the actual `<mode>-<compiler>` scope
- Point `miDebuggerPath` at your local gdb; if `cizon2` is not on PATH, set the task `command` to its full path
- g++/MinGW uses `cppdbg + gdb`; clang++ (MSVC target) needs cpptools' `cppvsdbg` or the LLDB extension, configured differently
- On MSYS2 UCRT64, install gdb with `pacman -S mingw-w64-ucrt-x86_64-gdb`
- `--release` artifacts are poor debugging targets; use the default debug build for breakpoints

## Incremental builds and sandbox

- A newer source file or header (tracked via `.d` files): only affected files are recompiled
- Project flags (edition, optimization level, user flags, dependency includes) change: bin and library objects are invalidated; dependency flag changes only affect dependency objects
- Object paths mirror source paths; after a layout upgrade the first build clears old objects and does one full rebuild
- Link inputs (object list, static archives, `link_flags`) change: forced relink
- Static library sources change: only that archive is rebuilt, then relinked; changing only the bin does not rebuild archives
- Shared library sources change: the DLL relinks; if the import library fingerprint is unchanged, the bin is not relinked. Export changes relink the bin
- Removing or renaming a shared target cleans its DLL, import library, and `.shared` objects
- Compiler version changes: the whole `build/<mode>-<compiler>/` scope is invalidated
- Concurrent builds of the same project are unsupported: do not run `cizon2 build` in two terminals at once

## Troubleshooting

### `<print>` / `std::println` fails to link with GCC

Typical errors:

```
undefined reference to `std::__open_terminal(_iobuf*)'
undefined reference to `std::__write_to_terminal(void*, std::span<char, ...>)'
```

Cause: libstdc++ ships the C++23 `<print>` functions in an experimental library, `libstdc++exp`, which must be linked explicitly; g++ does not add it automatically. This is libstdc++ packaging, not specific to MinGW, and not a cizon2 problem.

Add it to the link flags in `[compiler]`:

```toml
[compiler]
link_flags = ["-lstdc++exp"]
```

cizon2 appends `link_flags` to the end of the link command; with that, `std::print` / `std::println` link normally.

Alternatives:

- Switch to clang++ (MSVC target with MSVC STL): no extra library needed; if clang++ uses libstdc++, `-lstdc++exp` is still required
- Avoid `<print>` and use `std::format` + `std::cout`

## Usage notes

- Exactly one executable target per project; `cizon2 run` has no target selection
- Static library artifacts are `.a` (GNU archives); `shared_lib` produces a DLL plus an import library
- Shared libraries use the C ABI for Python/Rust; direct C++ ABI export is not guaranteed
- Multiple DLLs sit next to the exe; the Windows loader finds them without PATH changes
- `include/` is the only automatic `-I` directory; keep private headers out of it and do not expose them with a global `-Ilib`
- `sources` must be project-relative paths; the first 0.4.1 build migrates the object layout and does one full rebuild
- `[project].version` is metadata only; it does not affect compilation or artifact names
- Concurrent builds of one project are unsupported; never run `cizon2 build` in multiple terminals
- Turning `compile_commands` off does not delete an existing `compile_commands.json` or `.vscode/c_cpp_properties.json`; delete them manually when no longer needed
- Unknown sections or unknown keys in known sections are hard errors, never silently ignored
- Compiler warnings (e.g. ignoring a `[[nodiscard]]` return value) do not fail the build; use `-Werror` for strict mode

## Roadmap (unscheduled)

Known issues that are not committed work yet; they advance on real demand and the gates in AGENTS.md.

### Debugging integration (unscheduled)

The goal is first-class breakpoint debugging, but **cizon2 does not implement a debugger, the DAP/MI protocol, or breakpoint UI**; breakpoints, variables, and call stacks remain gdb / lldb / cppvsdbg's job.

- Artifact contract: debug builds use `-g`, are not stripped, and land at the fixed path `build/debug-<compiler>/<bin>.exe`
- `cizon2 run` stays a direct executor and will not become a debugger launcher; debugging is driven by an editor launch configuration plus a `preLaunchTask` build
- Preferred trigger: `[project] vscode = true` (off by default), generating `.vscode/launch.json` and `.vscode/tasks.json` at build time from the current plan; a one-shot `build --vscode` is the fallback, while a standalone `debug` command would trip the "no new commands" gate and is not planned
- Generation is non-destructive: existing files are never overwritten; only missing files are added or paste-ready snippets printed; a target rename or compiler switch refreshes them on the next build
- Debugger choice: g++/MinGW → `cppdbg` + gdb; clang++ (MSVC target) → `cppvsdbg`/PDB; LLDB for clang++ (GNU target) comes later
- v1 is validated on float_test with g++/MinGW + gdb; tests cover configuration content, scope/paths, non-overwrite behavior, and a clear error when gdb is missing—without actually starting a debugger
- Terminal-only `cizon2 run --debug` cannot provide VS Code breakpoints and is not a substitute

### Reliability

- The linker writes the exe directly, and lock/`cizon.toml` writes are not power-loss atomic; temporary-file replacement could be added
- No project-level build lock; two terminals building at once remain unsupported
- Disabling `compile_commands` leaves the generated `compile_commands.json` and `.vscode/c_cpp_properties.json` behind
- Removing a dependency leaves `cizon.lock`/`deps/` untouched; a separate remove command may be needed (would trip the second gate)

### Diagnostics and engineering

- Config structure errors come from serde; messages such as `unknown field` are English (low-priority style issue)
- No baseline CI; tests silently skip when the compiler/git is missing
- `rustfmt`/`clippy` are not wired in yet

### Wait for real demand

- Multiple executable targets, transitive target dependencies, per-target flags/include
- install/export, a one-shot CMake export command
- MSVC, cross-compilation, external package managers
- Consumers of a generated header/compile macro for `[project].version`
- Compiler discovery in common install locations and version selection

## When to use CMake

Cizon2 targets small-to-medium single-executable projects (optionally with static/shared library targets). Reach for CMake when you need:

- Multiple executable targets, nested target graphs, transitive target dependencies, per-target flags/include
- install/export/find_package/packaging, DLL versioning/SONAME
- Shared libraries depending on other shared libraries, direct C++ ABI distribution
- Code generation (protobuf, Qt moc), custom build steps
- MSVC, cross-compilation, complex toolchain conditions
- External package managers such as vcpkg / Conan

None of these are supported today.

## Development

```bash
cargo test
cargo build --release
```

Single crate, depending only on `serde` and `toml`; no clap / rayon / sha2.
Features and fixes go to `dev`, then merge into `main` once releasable.

See [AGENTS.md](AGENTS.md) for detailed boundaries, gates, and roadmap (currently in Chinese).
