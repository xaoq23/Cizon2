# Cizon2

<p align="center">
<img width="250" height="250" alt="cizon loge" src="https://github.com/user-attachments/assets/9ea31b2b-7ae1-4137-aedd-919ae7f01917" />
</p>

[简体中文](README-zh.md) | English

A declarative, progressive modern C++ build tool.

Describe the build in `cizon.toml` and compile/run C++ from concise terminal commands, bringing a cargo-like experience to small and medium projects.

Current version: **v0.6.1**. Primarily targets Windows x86-64 (no Linux/macOS builds or other architectures); supports g++ or clang++, not MSVC yet.

> **Status:** personally maintained and still under active development. Do not use it for real projects yet.

## Features

- One `cizon.toml` describes the project; there is no build-script language
- Auto-detects g++ / clang++; debug vs release and different compilers are isolated on disk
- Translation-unit-level incrementality: mtime + `-MMD`; header or compile-flag changes trigger the right rebuilds and relinks
- Collects `.cpp` / `.cc` / `.cxx` recursively; `include/` is added to `-I` automatically
- Multiple bins, static libraries, and shared libraries through `[[target]]`; `links` controls archive/import-library order
- `shared_lib` builds DLLs and import libraries; unchanged import libraries do not force a relink
- `cizon2 run <bin>` selects a target to run; `build` builds every target
- Git dependencies with a lock file: `cizon2 add` / `cizon2 update`
- Optional `compile_commands.json` for clangd / clang-tidy, plus a VS Code IntelliSense config when missing
- Compiler warnings grouped by source file and highlighted; build status distinguishes fresh / incremental / relinked / up to date
- Compiler stderr is captured and replayed (lossy decoding for non-UTF-8); crash exit codes are translated into plain language

## Installation

Building and running projects requires Windows x86-64 and `g++` or `clang++` on PATH; `new` / `help` / `version` need no compiler. A Rust toolchain is only needed when building from source.

### 1. Build from source

```bash
cargo build --release
# artifact: target/release/cizon2.exe

# or install into %USERPROFILE%\.cargo\bin
cargo install --path .
```

`cargo install --path .` puts `cizon2.exe` in `%USERPROFILE%\.cargo\bin`; rustup keeps that directory on PATH, so a new terminal can run `cizon2` directly.

If you only ran `cargo build --release`, copy `target\release\cizon2.exe` into a directory already on PATH, or add `target\release` to PATH as described below.

### 2. Download from GitHub Releases

Download `cizon2-v0.6.1-windows-x86_64.exe` and its `.sha256` file from the [Releases](https://github.com/xaoq23/Cizon2/releases) page; no Rust toolchain is needed. Move the exe to a fixed directory and add that directory to PATH (next section).

Verify the download in PowerShell:

```powershell
Get-FileHash .\cizon2-v0.6.1-windows-x86_64.exe -Algorithm SHA256
Get-Content .\cizon2-v0.6.1-windows-x86_64.exe.sha256
```

Both SHA-256 values must match exactly.

### 3. Add the cizon2 directory to PATH

PowerShell (point `$dir` at `target\release` or the directory holding the downloaded exe):

```powershell
$dir = "C:\path\to\cizon2"
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
[Environment]::SetEnvironmentVariable("Path", "$userPath;$dir", "User")
```

Reopen the terminal, then verify:

```powershell
Get-Command cizon2
cizon2 version
```

- Do not use `setx PATH "%PATH%;..."`: it folds the system PATH into the user PATH and truncates long values.
- `g++` / `clang++` must be on PATH; cizon2 probes the compiler at build time. When g++ is missing it falls back to clang++ with a notice; when neither exists the build fails.
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

Except for `new`, every command runs in the directory containing `cizon.toml`; cizon2 reads its configuration from the current directory. Running `cizon2` without a subcommand prints usage.

| Command | Purpose |
|---|---|
| `cizon2 new <path>` | Create a project skeleton |
| `cizon2 init` | Initialize in the current directory (existing files are not overwritten) |
| `cizon2 build [--release] [--compiler <spec>]` | Build every target |
| `cizon2 run [--release] [--compiler <spec>] [<bin-name>] [-- <program args...>]` | Build every target, then run the selected bin and forward arguments |
| `cizon2 add <git-url> [--tag <tag>]` | Add a git dependency and write the lock file |
| `cizon2 update <dependency>` | Align the lock with the intent in `cizon.toml` (upgrade or downgrade) |
| `cizon2 help` / `cizon2 version` | Help / version |

### `cizon2 new <path>`

Creates a directory containing `cizon.toml`, `src/main.cpp`, and `.gitignore`. The project name is the last path segment (`cizon2 new apps/hello` produces the project name `hello`).

- The path cannot contain `.` or `..` components and cannot start with `-`
- Refuses to run when the directory already exists; existing files are never overwritten
- Does not run `git init`

### `cizon2 init`

Initializes a single-executable project in the current directory; the project name is the directory name. It refuses to run when `cizon.toml` already exists or any `main.cpp` / `main.cc` / `main.cxx` exists anywhere below the directory, so user code is never overwritten. It accepts no arguments.

### `cizon2 build`

Builds every bin, static library, and shared library in the plan, compiling only stale objects.

| Option | Purpose |
|---|---|
| `--release` | Uses `-O2`; the default debug build uses `-g` |
| `--compiler <spec>` | Temporarily selects the compiler for this build |

- `--compiler` accepts `g++`, `clang++`, or a full path; bare `gcc` / `clang` are normalized to `g++` / `clang++`, and an `@version` suffix is ignored (no version selection)
- An explicitly selected compiler that cannot be probed is a hard error with no silent fallback; without `--compiler`, cizon2 probes g++ → clang++ and prints a notice when g++ is missing
- Artifacts live under `build/<mode>-<compiler>/`; linking across modes or compilers never happens
- `build` accepts only the two options above and no program arguments; use `run` to pass arguments

```bash
cizon2 build
cizon2 build --release
cizon2 build --compiler clang++
```

### `cizon2 run`

Builds every target first, then runs the selected bin. Bin names are case-insensitive.

```bash
cizon2 run                        # valid when the project has exactly one bin
cizon2 run worker                 # select worker when multiple bins exist
cizon2 run worker -- --port 8080  # everything after -- goes to the program
```

- With multiple bins and no name, cizon2 reports an error and lists the available bins
- At most one bin name is accepted; two names are rejected
- Program arguments must follow `--`; an unknown `-` option before `--` is treated as a cizon2 option and rejected
- `run` forwards the program exit code and adds a plain-language explanation for crashes such as `ACCESS_VIOLATION` or `DLL_NOT_FOUND`
- `run --all` is not supported: multi-process/IPC orchestration belongs in an external harness

When passing arguments to the program, call `cizon2.exe` directly or use a PowerShell alias (`Set-Alias cizon C:\path\to\cizon2.exe`). A PowerShell function wrapper can consume the bare `--` before cizon2 receives it.

### `cizon2 add <git-url> [--tag <tag>]`

Shallow-clones the dependency into `deps/<name>`, appends an entry to `[dependencies]` in `cizon.toml`, and writes `cizon.lock`. The dependency name is the last URL segment with `.git` removed and may contain only letters, digits, `-`, and `_`.

- `--tag` is optional; without it the remote default branch is cloned
- Refuses when the name already exists in `[dependencies]`, or when `deps/<name>` exists but is not managed by cizon2
- A clone or write failure rolls back `cizon.toml`, `cizon.lock`, and `deps/<name>`
- A dependency without `include/` prints a layout warning but is still added

```bash
cizon2 add https://github.com/fmtlib/fmt --tag 11.0.2
```

### `cizon2 update <dependency>`

Re-resolves a dependency from its `git` / `tag` entry in `cizon.toml`. Upgrade and downgrade use the same command: change the `tag`, then run `update`.

- The dependency must exist in `[dependencies]`
- Refuses when `deps/<name>` has uncommitted changes; push local commits first if they must be kept
- Clones into a temporary directory first and replaces the old checkout only on success; failures keep the existing checkout
- `build` always builds the commit pinned in `cizon.lock` and never chases a new `tag` in `cizon.toml` on its own

```bash
# change the tag in cizon.toml first
cizon2 update fmt
```

### `cizon2 help` / `cizon2 version`

Print usage or the version. `--help` / `-h` and `--version` / `-V` are equivalent.

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
├── src/                # default source directory for bins without explicit sources
├── lib/                # example static-library source directory; not collected automatically
│   └── detail/         # example private header
├── deps/               # git dependencies, ignored automatically
└── build/              # build artifacts
```

The layout is a convention, not a hard-coded framework: `sources` may point at any project-relative file or directory. `src/` is only the default scan directory for bins that omit `sources`; static and shared libraries must list their sources explicitly.

### Public and private headers

- `include/` is the only directory added to `-I` automatically; it holds the public API and is visible to both the bin and library targets
- Private headers and implementation details belong in `lib/` or next to the implementation files and stay off the include path by default
- Include private headers with relative paths, e.g. `#include "detail/shape_impl.h"`; never put them in `include/`
- Do not add `-Ilib` to `[compiler].flags`: flags are global and would expose private directories to the bin as well
- Public headers should be self-contained and must not depend on private headers or implementation directories
- Single-executable projects without external consumers may keep headers next to `src/`; header-only libraries keep everything in `include/`
- Future install/export will publish only `include/` and library artifacts, never `lib/` sources or private headers

## cizon.toml

cizon2 recognizes exactly four top-level sections: `[project]`, `[compiler]`, `[dependencies]`, and `[[target]]`. Unknown sections, and unknown keys inside known sections, are hard errors, so typos such as `[dependancies]` or `compiler = "..."` are never silently ignored.

The minimal configuration only needs a project name and scans `src/`:

```toml
[project]
name = "hello"
```

Full example:

```toml
[project]
name = "hello"
version = "0.1.0"           # optional; metadata only
edition = "c++23"           # optional; defaults to -std=c++23
sources = ["src/main.cpp"]  # optional; file or directory; defaults to scanning src/
compile_commands = true     # optional; generates the compilation database and VS Code config

[compiler]
cpp = "clang++"             # optional; defaults to probing g++ → clang++
flags = ["-Wall"]           # extra compile flags for project sources
link_flags = ["-lm"]        # appended to bin / shared-library link commands

[dependencies]
fmt = { git = "https://github.com/fmtlib/fmt", tag = "11.0.2" }
```

### `[project]`

| Key | Required | Default | Meaning |
|---|---|---|---|
| `name` | No | Directory name | Project name; determines the default bin name and exe name |
| `version` | No | None | Metadata only; not used for compilation or artifact names |
| `edition` | No | `c++23` | Produces `-std=...` |
| `sources` | No | Scan `src/` | Sources of the only bin; only valid without `[[target]]` |
| `compile_commands` | No | `false` | Generates `compile_commands.json` and the VS Code config |

- `name` becomes a file name: no path separators or Windows-reserved characters, not `.` / `..`, no leading dot, and no device names such as `con` / `nul` or internal directories such as `obj` / `lib` / `deps-obj`.
- `version` must be non-empty and free of control characters; no generated header or compile macro exists yet, and that waits for a real consumer.
- `edition` has no version whitelist: `c++XX` / `gnu++XX` are passed through to `-std=`, while a bare value such as `"17"` or `"23"` becomes `c++17` / `c++23`. A typo or unsupported version fails at the compiler.
- `sources` holds project-relative paths and may mix files and directories; directories are scanned recursively for `.cpp` / `.cc` / `.cxx`. Every path must exist, cannot be absolute or contain `..`, and a directory reached through a symlink/junction outside the project is rejected.
- `sources` and `[[target]]` are mutually exclusive: with targets, sources belong to each target. Omitting `sources` scans `src/`.
- With `compile_commands = true`, every build writes `compile_commands.json`; a missing `.vscode/c_cpp_properties.json` is also created (existing files are never overwritten). Turning it off stops updates but does not delete generated files.

### `[compiler]`

| Key | Required | Default | Meaning |
|---|---|---|---|
| `cpp` | No | Auto-detect | `g++` → `clang++` |
| `flags` | No | Empty | Appended to project source compile commands |
| `link_flags` | No | Empty | Appended to bin and shared-library link commands |

- `cpp` accepts `g++`, `clang++`, or a full path; bare `gcc` / `clang` are normalized to `g++` / `clang++`, and an `@version` suffix is ignored. An explicitly selected compiler that cannot be probed is a hard error; without one, a missing g++ falls back to clang++ with a notice.
- `flags` applies to every project target and is global; per-target flags do not exist. Do not expose private directories to every target with `-Ilib`. Dependency sources compile with `edition`, the optimization level, and dependency includes; they do not inherit `flags`.
- `link_flags` is appended only to bin and shared-library links; static libraries are archived, not linked. For example, GCC needs `-lstdc++exp` for `std::print`.

### `[dependencies]`

```toml
[dependencies]
fmt = { git = "https://github.com/fmtlib/fmt", tag = "11.0.2" }
```

- Each dependency accepts only the `git` and `tag` keys. The key is the dependency name and the `deps/<name>` directory; it may contain only letters, digits, `-`, and `_`. `git` is required, `tag` is optional (the remote default branch when omitted).
- `cizon.lock` is the single source of truth for builds: `build` uses the pinned commit and never chases a new `tag` that has not been applied with `update`.
- Dependencies are project-wide, not per-target; adding, updating, lock restoration, and layout are covered in [Dependencies and the lock file](#dependencies-and-the-lock-file).

### `[[target]]`: libraries and bins

A project may carry static and shared library targets plus one or more executable targets:

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

[[target]]
name = "worker"
kind = "bin"
sources = ["worker"]
links = ["shape"]
```

Each target accepts four keys:

| Key | Required | Meaning |
|---|---|---|
| `name` | Depends | A single bin may omit it and use the project name; libraries and multiple bins must name it |
| `kind` | Yes | `bin` / `static_lib` / `shared_lib` |
| `sources` | Depends | A bin may omit it and scan `src/`; libraries must provide it and it cannot be empty |
| `links` | No | Libraries linked by a bin, or static libraries packed into a shared library |

Rules:

- Without `[[target]]`, the project is the traditional single-executable project, with sources from `[project].sources` or `src/`
- With `[[target]]`, at least one `kind = "bin"` is required, and `[project].sources` can no longer be used
- A single bin may omit `name` and use `[project].name`; with multiple bins every bin must be named explicitly
- Target names are case-insensitive and unique, and must pass file-name validation: not `obj` / `lib` / `deps-obj`, no leading dot, and no Windows device names. A target named `shared` is allowed because the internal directory is `.shared`
- `sources` must be project-relative paths (no absolute paths or `..`); files and directories are accepted and directories are scanned recursively; same-named sources in different directories are allowed
- `static_lib` does not support `links`; `shared_lib` may link existing `static_lib` targets only; each `bin` may link both, with static archives first, then import libraries
- Every `links` entry must reference an existing target and duplicates are rejected; shared libraries cannot link other shared libraries, and transitive DLL graphs are a CMake job
- Public headers go in `include/` (automatic `-I`); a library's private headers may sit next to its sources
- Static archives use `ar` (g++) or `llvm-ar` (clang++); the artifact is `build/<mode>-<compiler>/lib/<name>/lib<name>.a`
- A shared library produces `build/<mode>-<compiler>/<name>.dll` plus an import library under `build/<mode>-<compiler>/.shared/<name>/` (Windows only today)
- On Windows, MinGW uses `lib<name>.dll.a` and clang++ (MSVC target) uses `<name>.lib`; cizon2 passes the import-library flag explicitly
- Exports are explicit C ABI: write `extern "C" __declspec(dllexport)`. cizon2 does not auto-export symbols, generate bindings, or guarantee the C++ ABI
- On MinGW, `shared_lib` links its runtime statically by default, so the DLL has no `libstdc++`/`libgcc`/`libwinpthread` dependency; Python `ctypes` can load it without touching PATH. Keep the boundary C ABI: with separate static runtimes, do not pass C++ objects or exceptions across the DLL boundary
- Every bin produces `build/<mode>-<compiler>/<name>.exe`; `cizon2 run <bin>` builds all bins and runs the selected one; an omitted name is only valid when the project has exactly one bin
- `run --all` is not supported: cizon2 does not supervise multiple processes. Use a shell/Python harness for IPC or multi-process tests
- Object paths mirror source paths: a single bin uses `obj/<project-relative-path>.o`; multiple bins use `obj/<bin>/<project-relative-path>.o`; static libraries use `lib/<name>/obj/...`, shared libraries `.shared/<name>/obj/...`, dependencies `deps-obj/<dep>/`
- Same stem in the same directory (e.g. `util.cpp` + `util.cc`) is still an error; rename one
- `new` / `init` only generate the traditional single-executable skeleton; target projects need manually created sources and `[[target]]` entries

### Using a DLL from Python

```python
import ctypes

lib = ctypes.CDLL("build/debug-g++/shape_dll.dll")
print(lib.shape_area(3, 4))   # an extern "C" function exported by the DLL
```

Python only needs the DLL; the import library exists for C/C++ consumers. Python and Rust `libloading` callers both rely on the C ABI. Rust link-time consumers must match the import-library flavor: MinGW uses `.dll.a`, MSVC uses `.lib`. Direct C++ ABI export is outside the current contract; use a C wrapper or a pure-virtual/opaque-handle interface.

## Dependencies and the lock file

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
- Dependency source objects are linked into the bin and every shared library; dependencies are project-wide, not per-target.
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
- Project source compile inputs (edition, optimization level, `[compiler].flags`, dependency include set) change: project objects are invalidated and rebuilt; dependency sources follow only edition, optimization, and dependency includes, never `[compiler].flags`
- Object paths mirror source paths; a build with an older object layout is detected and does one full rebuild
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

- One or more executable targets per project; `cizon2 run <bin>` selects one, and an omitted name is only valid with exactly one bin
- `run --all` is not supported; multi-process/IPC orchestration belongs in an external harness
- Static library artifacts are `.a` (GNU archives); `shared_lib` produces a DLL plus an import library
- Shared libraries use the C ABI for Python/Rust; direct C++ ABI export is not guaranteed
- Multiple DLLs sit next to the exe; the Windows loader finds them without PATH changes
- `include/` is the only automatic `-I` directory; keep private headers out of it and do not expose them with a global `-Ilib`
- `sources` must be project-relative paths; a build with an older object layout does one full rebuild
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

### Test targets (v0.7 design)

- `kind = "test"` shares the bin compile/link path, but `build` / `run` ignore test targets; only `cizon test` builds and runs them
- Framework-agnostic: the only contract is exit code 0; `cizon test [name...] -- <args>` filters targets and forwards arguments, replaying child output on failure
- No auto-discovery, framework adapters, or dependency downloads; gtest / Catch2 v3 use explicit `system_links`, while doctest / Catch2 v2 are header-only
- CTest labels / fixtures / parallel / timeout are deferred until real demand
- Design first: dogfood on a real project and pass the three gates before implementation

### Reliability

- The linker writes the exe directly, and lock/`cizon.toml` writes are not power-loss atomic; temporary-file replacement could be added
- No project-level build lock; two terminals building at once remain unsupported
- Disabling `compile_commands` leaves the generated `compile_commands.json` and `.vscode/c_cpp_properties.json` behind
- Removing a dependency leaves `cizon.lock`/`deps/` untouched; a separate remove command may be needed (would trip the second gate)

### Diagnostics and engineering

- Config structure errors come from serde; messages such as `unknown field` are English (low-priority style issue)
- CI builds tagged releases; tests are local and skip when the compiler/git is missing
- `rustfmt`/`clippy` are not wired in yet

### Wait for real demand

- Transitive target dependencies, per-target flags/include
- install/export, a one-shot CMake export command
- MSVC, cross-compilation, external package managers
- Consumers of a generated header/compile macro for `[project].version`
- Compiler discovery in common install locations and version selection

## When to use CMake

Cizon2 targets small-to-medium projects with one or more bins (optionally with static/shared library targets). Reach for CMake when you need:

- Nested target graphs, transitive target dependencies, per-target flags/include
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

Pushing a `v*` tag runs `.github/workflows/release.yml`: it builds the Windows binary, uploads `cizon2-v<version>-windows-x86_64.exe` plus a SHA-256 file, and creates or updates the GitHub Release. Manual dispatch accepts an existing tag (for example `v0.6.0`). Tests stay local; CI does not run them.

See [AGENTS.md](AGENTS.md) for detailed boundaries, gates, and roadmap (currently in Chinese).
