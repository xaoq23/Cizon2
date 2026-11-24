# Cizon

<p align="center">
<img width="250" height="250" alt="cizon loge" src="https://github.com/user-attachments/assets/9ea31b2b-7ae1-4137-aedd-919ae7f01917" />
</p>

[简体中文](README-zh.md) | English

A declarative, progressive modern C++ build tool.

Describe the build in `cizon.toml` and compile/run C++ from concise terminal commands, bringing a cargo-like experience to small and medium projects.

Current version: **v0.10.0**. Adds local `[subproject]` composition: library/test directories contribute targets to one root build without duplicate declarations, with plan-time duplicate-header checks, case-insensitive name handling, and validation of subproject git sources and include roots. This sits on top of v0.9.6's reliability release (atomic writes, project-level build lock, regression coverage), v0.9.4's `cizon help toml`, v0.9.3's rename, edition validation and output cleanup, and v0.9.1's library project templates. Targets Windows and Linux x86-64; supports g++ or clang++, not MSVC yet. macOS is not supported.

> **Status:** personally maintained, pre-1.0 and still evolving. It is dogfooded on small real projects; expect breaking changes between minor versions.

## Features

- One `cizon.toml` describes the project; there is no build-script language
- Auto-detects g++ / clang++; debug vs release and different compilers are isolated on disk
- Translation-unit-level incrementality: mtime + `-MMD`; header or compile-flag changes trigger the right rebuilds and relinks
- Collects `.cpp` / `.cc` / `.cxx` recursively; `include/` is added to `-I` automatically, and a target's `include` root propagates to its consumers
- Multiple bins, static libraries, and shared libraries through `[[target]]`; static libraries may publicly depend on other static libraries, with includes and archive order resolved from the graph
- v0.10: `[subproject]` combines local libraries/tests under one root build; it is not a workspace and has no independent subproject build, lock, or release lifecycle
- `shared_lib` builds DLLs + import libraries on Windows and `.so` files on Linux (objects compiled `-fPIC`, the `.so` linked by name with an `$ORIGIN` rpath); unchanged import libraries do not force a relink
- `cizon run <bin>` selects a target to run; `build` builds every bin/library target
- v0.7: `kind = "test"` + `cizon test`, framework-agnostic with the exit code as the contract; `extern_links` maps external library names to `-l`
- Git dependencies with a lock file: `cizon add` / `cizon update` / `cizon sync`; `sync --check` verifies a locked checkout in CI without writing
- `--quiet` silences build/run/test progress lines (errors, warnings and program output still print)
- Optional `compile_commands.json` for clangd / clang-tidy, plus a VS Code IntelliSense config when missing
- Compiler warnings grouped by source file and highlighted; build status distinguishes fresh / incremental / relinked / up to date
- Compiler stderr is captured and replayed (lossy decoding for non-UTF-8); crash exit codes are translated into plain language

## Installation

Building and running projects requires Windows x86-64 or Linux x86-64 and `g++` or `clang++` on PATH; `new` / `help` / `version` need no compiler. A Rust toolchain is only needed when building from source.

### 1. Build from source

```bash
cargo build --release
# artifact: target/release/cizon (cizon.exe on Windows)

# or install into ~/.cargo/bin
cargo install --path .
```

`cargo install --path .` puts the binary in `~/.cargo/bin` (`%USERPROFILE%\.cargo\bin` on Windows); rustup keeps that directory on PATH, so a new terminal can run `cizon` directly.

### 2. Download from GitHub Releases

Download the asset for your platform and its `.sha256` file from the [Releases](https://github.com/xaoq23/Cizon2/releases) page; no Rust toolchain is needed.

- Windows: `cizon-v0.10.0-windows-x86_64.exe`
- Linux: `cizon-v0.10.0-linux-x86_64`

Windows (PowerShell):

```powershell
Get-FileHash .\cizon-v0.10.0-windows-x86_64.exe -Algorithm SHA256
Get-Content .\cizon-v0.10.0-windows-x86_64.exe.sha256
```

Linux:

```bash
sha256sum -c cizon-v0.10.0-linux-x86_64.sha256
chmod +x cizon-v0.10.0-linux-x86_64
```

Both SHA-256 values must match exactly.

### 3. Put cizon on PATH

Windows (PowerShell; point `$dir` at the download directory or `target\release`):

```powershell
$dir = "C:\path\to\cizon"
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
[Environment]::SetEnvironmentVariable("Path", "$userPath;$dir", "User")
```

Linux:

```bash
install -Dm755 cizon-v0.10.0-linux-x86_64 ~/.local/bin/cizon
# If ~/.local/bin is not on PATH, add it:
export PATH="$HOME/.local/bin:$PATH"
```

Reopen the terminal (or export PATH), then verify with `cizon version`.

- Do not use `setx PATH "%PATH%;..."` on Windows: it folds the system PATH into the user PATH and truncates long values.
- `g++` / `clang++` must be on PATH; cizon probes the compiler at build time. When g++ is missing it falls back to clang++ with a notice; when neither exists the build fails.
- MSYS2 UCRT64: add `<MSYS2>\ucrt64\bin` (for example `C:\Users\<user>\msys2\ucrt64\bin`) to PATH and install `mingw-w64-ucrt-x86_64-gcc`.
- Git Bash inherits the Windows PATH, so the same setting applies there.
- Shared libraries: Windows produces a DLL + import library next to the executable; Linux produces a `.so` found via the executable's `$ORIGIN` rpath.

## Quick start

```bash
cizon new hello
cd hello
cizon run
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

Commands that operate on a project (`build` / `run` / `test` / `add` / `update` / `sync`) run in the directory containing `cizon.toml`; `new` creates a directory, `init` initializes the current directory, and `help` / `version` need no project. Running `cizon` without a subcommand prints usage.

| Command | Purpose |
|---|---|
| `cizon new <path> [--kind <kind>]` | Create a project skeleton (`bin`, `static_lib`, or `shared_lib`) |
| `cizon init [--kind <kind>]` | Initialize in the current directory (existing files are not overwritten) |
| `cizon build [--release] [--compiler <spec>] [--quiet]` | Build every bin and library target (not tests) |
| `cizon run [--release] [--compiler <spec>] [--quiet] [<bin-name>] [-- <program args...>]` | Build bins and their dependencies, then run and forward arguments |
| `cizon test [--release] [--compiler <spec>] [--quiet] [<test-name>...] [-- <test args...>]` | Build and run test targets one by one |
| `cizon add <git-url> [--tag <tag>]` | Add a git dependency and write the lock file |
| `cizon update <dependency>` | Align the lock with the intent in `cizon.toml` (upgrade or downgrade) |
| `cizon sync [--check]` | Restore deps from `cizon.lock`, or verify them without writing |
| `cizon help [<command>\|toml]` / `cizon version` | General, per-command, or cizon.toml reference / version |

`--quiet` on `build` / `run` / `test` suppresses cizon's own progress lines; errors, compiler warnings and the program/test output still print.

### `cizon new <path> [--kind bin|static_lib|shared_lib]`

Creates a directory containing `cizon.toml` and `.gitignore`. The project name is the last path segment (`cizon new apps/hello` produces the project name `hello`).

- Default `--kind bin` keeps the zero-config implicit single-executable template: `src/main.cpp`
- `--kind static_lib|shared_lib` generates a target-based library project:
  - `include/<name>/<name>.hpp` (public header; the shared template uses C ABI)
  - `src/<name>.cpp`, `tests/<name>_test.cpp` (native assert)
  - `cizon.toml` declares the library target and a test target that links it
- The path cannot contain `.` or `..` components and cannot start with `-`
- Refuses to run when the directory already exists; existing files are never overwritten
- Does not run `git init`

### `cizon init [--kind bin|static_lib|shared_lib]`

Initializes a project in the current directory; the project name is the directory name. The default `bin` template writes `src/main.cpp`; library kinds write the `include/<name>/`, `src/`, and `tests/` layout described above. It refuses to run when `cizon.toml` already exists or any `main.cpp` / `main.cc` / `main.cxx` exists under `src/`, so user code is never overwritten.

### `cizon build`

Builds every bin, static library, and shared library in the plan, compiling only stale objects; test targets are excluded. A project without bins (test-only or library-only) still builds successfully: its libraries are built, no exe is produced, and no error is raised.

| Option | Purpose |
|---|---|
| `--release` | Uses `-O2 -DNDEBUG` (asserts off for project/dependency sources); the default debug build uses `-g`. Test sources always strip NDEBUG |
| `--compiler <spec>` | Temporarily selects the compiler for this build |
| `--quiet` / `-q` | Suppress progress lines; errors, warnings and program output still print |

- `--compiler` accepts `g++`, `clang++`, or a full path; bare `gcc` / `clang` are normalized to `g++` / `clang++`, and an `@version` suffix is ignored (no version selection)
- An explicitly selected compiler that cannot be probed is a hard error with no silent fallback; without `--compiler`, cizon probes g++ → clang++ and prints a notice when g++ is missing
- Artifacts live under `build/<mode>-<compiler>/`; linking across modes or compilers never happens
- `build` accepts only the three options above and no program arguments; use `run` to pass arguments

```bash
cizon build
cizon build --release
cizon build --compiler clang++
```

### `cizon run`

Builds every bin and its dependencies, then runs the selected bin; test targets are not involved. Bin names are case-insensitive.

```bash
cizon run                        # valid when the project has exactly one bin
cizon run worker                 # select worker when multiple bins exist
cizon run worker -- --port 8080  # everything after -- goes to the program
```

- With multiple bins and no name, cizon reports an error and lists the available bins
- At most one bin name is accepted; two names are rejected
- Program arguments must follow `--`; an unknown `-` option before `--` is treated as a cizon option and rejected
- `run` forwards the program exit code and adds a plain-language explanation for crashes such as `ACCESS_VIOLATION` or `DLL_NOT_FOUND`
- `run --all` is not supported: multi-process/IPC orchestration belongs in an external harness

When passing arguments to the program, call `cizon.exe` directly or use a PowerShell alias (`Set-Alias cizon C:\path\to\cizon.exe`). A PowerShell function wrapper can consume the bare `--` before cizon receives it.

### `cizon test`

Builds the selected test targets (all when no filter is given) and runs them in declaration order. The only pass contract is **process exit code 0 = pass**; it does not parse gtest/Catch2/doctest output or discover test cases.

```bash
cizon test                              # run every test target
cizon test calc_test codec_test         # select targets, case-insensitive
cizon test calc_test -- --gtest_filter=Add*  # everything after -- goes to each test process
```

- Accepts `--release` and `--compiler <spec>` like `build` / `run`; the build phase builds the selected test targets and all planned library targets (bins are not built), and `build` / `run` never build or execute test targets
- Any non-zero exit or crash fails the whole run; cizon keeps running the remaining tests and prints a `passed/failed/total` summary
- Child stdout/stderr are replayed untouched on stderr (cizon's stdout stays empty); `Running`, `Passed` / `Failed`, and the summary also go to stderr
- With no test targets, `cizon test` prints `no test targets` and exits 0; explicitly naming a missing test is an error
- Test sources never see `-DNDEBUG`, so assert-based tests cannot silently become no-ops
- Test processes run with the project root as their working directory; crash diagnostics reuse `run`'s exit-code table (`ACCESS_VIOLATION`, `DLL_NOT_FOUND`, ...)
- v1 does not run tests in parallel, apply timeouts, or write report files; CTest labels/fixtures wait for real demand
- Acquiring frameworks is not cizon's job: compiled frameworks use a test target's `extern_links`, header-only frameworks are vendored

### `cizon add <git-url> [--tag <tag>]`

Shallow-clones the dependency into `deps/<name>`, appends an entry to `[dependencies]` in `cizon.toml`, and writes `cizon.lock`. The dependency name is the last URL segment with `.git` removed and may contain only letters, digits, `-`, and `_`.

- `--tag` is optional; without it the remote default branch is cloned
- Refuses when the name already exists in `[dependencies]`, or when `deps/<name>` exists but is not managed by cizon
- A clone or write failure rolls back `cizon.toml`, `cizon.lock`, and `deps/<name>`
- A dependency without `include/` prints a one-time layout note (it is still added): header-only libraries (e.g. doctest) may already be usable via the automatic `-Ideps/<name>` include path; compiled libraries (e.g. googletest/Catch2) point at the system package manager plus a test target's `extern_links`

```bash
cizon add https://github.com/fmtlib/fmt --tag 11.0.2
```

### `cizon update <dependency>`

Re-resolves a dependency from its `git` / `tag` entry in the root or an included subproject's `cizon.toml`. Upgrade and downgrade use the same command: change the `tag`, then run `update`.

- The dependency name is case-insensitive and must exist in the merged `[dependencies]` set
- With subprojects, `update` writes the root `cizon.lock`; `sync` restores the merged dependency set from that lock
- Refuses when `deps/<name>` has uncommitted changes; push local commits first if they must be kept
- Clones into a temporary directory first and replaces the old checkout only on success; failures keep the existing checkout
- `build` always builds the commit pinned in `cizon.lock` and never chases a new `tag` in `cizon.toml` on its own

```bash
# change the tag in cizon.toml first
cizon update fmt
```

### `cizon sync`

Restores every dependency checkout from `cizon.lock` without building or changing `cizon.toml`. Use it after cloning a project to materialize its dependencies (the lock is the source of truth). It needs git but no compiler.

`cizon sync --check` is the CI gate: it verifies that the lock agrees with `cizon.toml` and every checkout is present, at the locked commit and clean — with no network and no writes. Exit non-zero on any drift.

```bash
git clone <project> && cd <project>
cizon sync
cizon build
# in CI:
cizon sync --check
```

### `cizon help` / `cizon version`

`cizon help` prints general usage; `cizon help <command>` (or `cizon <command> --help` / `-h`) prints detailed per-command help. `cizon help toml` prints an offline reference for every `cizon.toml` key, including defaults and constraints. `cizon version` prints the version; aliases `--version`, `-V`, `-v`.

## Build output

- `Compiling` / `Rebuilding g++ [n/m files]`: translation units actually compiled this run / total
- Compiler warnings are grouped per source file: a yellow `Warning <relative path> (compiler version)` header; a blank line separates the warning section from build status, with no decorative rule; project absolute path prefixes in diagnostic bodies are replaced with relative paths, while line numbers and message text stay untouched
- Warnings do not fail the build; set `[compiler] werror = true` for strict mode
- Build and `Running` status go to stderr, keeping program stdout clean: `cizon run > out.txt` contains only program output
- ANSI color is decided per stream and turns off automatically when stdout or stderr is redirected (pipes, files, CI); timings keep the unit attached to the number (`0.12s`), so escape-stripped logs cannot turn them into `ms`
- `cizon test` prints `Running` / `Passed` / `Failed` per target and a final `test result: ok` or `FAILED`; any failure exits 1, all-pass exits 0
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
├── tests/              # example test-target source directory (declare it in the target)
├── deps/               # git dependencies, ignored automatically
└── build/              # build artifacts
```

The layout is a convention, not a hard-coded framework: `sources` may point at any project-relative file or directory. `src/` is only the default scan directory for bins that omit `sources`; static and shared libraries must list their sources explicitly.

### Public and private headers

- The project-root `include/` is the only automatic `-I` for project public headers; it is optional and visible to every target. Dependency directories are also added automatically (`deps/<name>` and `deps/<name>/include`)
- Multi-library projects should keep each library's public headers inside that library (for example `libs/net/include`) and point the target's `include` at it. Do not copy or move them into the root `include/`: target `include` propagation preserves ownership and gives consumers the same view
- Private headers and implementation details belong in `lib/` or next to the implementation files and stay off the include path by default
- Include private headers with relative paths, e.g. `#include "detail/shape_impl.h"`; never put them in `include/`
- Do not add `-Ilib` to `[compiler].flags`: flags are global and would expose private directories to the bin as well
- Public headers should be self-contained and must not depend on private headers or implementation directories
- Single-executable projects without external consumers may keep headers next to `src/`; header-only libraries keep everything in `include/`
- Future install/export will publish only `include/` and library artifacts, never `lib/` sources or private headers

## cizon.toml

Run `cizon help toml` for a terminal-local reference of every configuration key.

cizon recognizes exactly five top-level sections: `[project]`, `[compiler]`, `[dependencies]`, `[subproject]`, and `[[target]]`. Unknown sections, and unknown keys inside known sections, are hard errors, so typos such as `[dependancies]` or `compiler = "..."` are never silently ignored.

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
- `edition` is validated while loading `cizon.toml`: accepted versions are 98 / 03 / 11 / 14 / 17 / 20 / 23 / 26, optionally prefixed with `c++` or `gnu++`, plus the aliases 0x / 1y / 1z / 2a / 2b / 2c. A bare value such as `"17"` becomes `c++17`. Invalid values fail before the compiler runs
- `sources` holds project-relative paths and may mix files and directories; directories are scanned recursively for `.cpp` / `.cc` / `.cxx`. Every path must exist, cannot be absolute or contain `..`, and a directory reached through a symlink/junction outside the project is rejected.
- `sources` and `[[target]]` are mutually exclusive: with targets, sources belong to each target. Omitting `sources` scans `src/`.
- With `compile_commands = true`, every build writes `compile_commands.json`; a missing `.vscode/c_cpp_properties.json` is also created (existing files are never overwritten). Turning it off stops updates but does not delete generated files.

### `[compiler]`

| Key | Required | Default | Meaning |
|---|---|---|---|
| `cpp` | No | Auto-detect | `g++` → `clang++` |
| `strict` | No | `false` | Expand to `-Wall -Wextra -Wpedantic` for project sources |
| `werror` | No | `false` | Append `-Werror` (warnings become errors) for project sources |
| `flags` | No | Empty | Appended to project source compile commands |
| `link_flags` | No | Empty | Appended to bin and shared-library link commands |

- `cpp` accepts `g++`, `clang++`, or a full path; bare `gcc` / `clang` are normalized to `g++` / `clang++`, and an `@version` suffix is ignored. An explicitly selected compiler that cannot be probed is a hard error; without one, a missing g++ falls back to clang++ with a notice.
- `strict` and `werror` are convenience switches over warning policy: `strict = true` is exactly `-Wall -Wextra -Wpedantic`, `werror = true` adds `-Werror`. They apply to project sources only (never dependencies), and user `flags` come after so they can still override (e.g. `-Wno-...`). Prefer these over hand-writing the same `-W` flags.
- `flags` applies to every project target and is global; per-target flags do not exist. Do not expose private directories to every target with `-Ilib` — use a target's `prefix` for installed packages instead. Dependency sources compile with `edition`, the optimization level, and dependency includes; they do not inherit `flags`.
- `link_flags` is appended only to bin and shared-library links; static libraries are archived, not linked. For example, GCC needs `-lstdc++exp` for `std::print`.

### `[dependencies]`

```toml
[dependencies]
fmt = { git = "https://github.com/fmtlib/fmt", tag = "11.0.2" }
```

- Each dependency accepts only the `git` and `tag` keys. The key is the dependency name and the `deps/<name>` directory; it may contain only letters, digits, `-`, and `_`. Names are case-insensitive, but every file must use the same spelling. `git` is required, `tag` is optional (the remote default branch when omitted).
- `cizon.lock` is the single source of truth for builds: `build` uses the pinned commit and never chases a new `tag` that has not been applied with `update`.
- Dependencies are project-wide, not per-target; adding, updating, lock restoration, and layout are covered in [Dependencies and the lock file](#dependencies-and-the-lock-file).

### `[subproject]` (v0.10)

`[subproject]` is available in v0.10.0. It lets local library/test directories contribute targets to one root build without duplicate declarations or moving public headers. It is **not** a workspace: there is one root build, lock file, compiler, and edition; no subproject has an independent build or release lifecycle.

Root `cizon.toml`:

```toml
[project]
name = "mesh"
version = "0.1.0"
edition = "c++20"

[subproject]
net = "libs/net"
storage = "libs/storage"

[[target]]
name = "mesh"
kind = "bin"
sources = ["src/main.cpp"]
links = ["net", "storage"]
```

`libs/net/cizon.toml` (and similarly `libs/storage/cizon.toml`):

```toml
[[target]]
name = "net"
kind = "static_lib"
sources = ["src"]
include = "include"
```

- A subproject provides `static_lib`, `shared_lib`, and `test` targets only; a `bin` there is a plan error. Both the declaring file and each subproject must declare at least one `[[target]]`. Target names are globally unique and case-insensitive.
- Paths must stay inside the root project, including after resolving symlinks/junctions. Each subproject's `sources`, `include`, and `prefix` resolve against its own directory. Public headers stay with their library and propagate through target `include` as usual.
- The root `[compiler]` and `[project].edition` win. A subproject's `[compiler]` is ignored with a warning, as is a differing edition; neither creates a separate toolchain configuration.
- Subproject `[dependencies]` merge into the root dependency set. Names are case-insensitive, but every file must use the same key spelling: case-only duplicates are errors, the same exact key with a different git URL or tag is an error, and exact duplicate declarations deduplicate. Because checkouts are cloned from the root, subproject git sources must be absolute paths or URL/SSH forms, not relative paths.
- `cizon update <name>` can update a dependency declared in any included file, but writes only the root `cizon.lock`. `cizon sync` restores all merged dependencies from that lock into the root `deps/`; `sync --check` verifies them without network access or writes.
- Subprojects may nest. Inclusion cycles are plan errors; a diamond (two routes to the same canonical directory) includes it once.
- Use CMake instead for workspace/package lifecycles, independent compilers or locks, conditional or large target graphs, and install/export.

### `[[target]]`: libraries and bins

A project may carry static and shared library targets plus one or more bin and test targets:

```toml
[[target]]
name = "shape"
kind = "static_lib"
sources = ["lib/shape.cpp"]   # required; file or directory (recursive)
include = "lib/shape/include" # optional public header root; inherited by consumers

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

[[target]]
name = "shape_test"
kind = "test"
sources = ["tests"]
links = ["shape"]                       # project libraries only
prefix = "deps/.install/googletest"     # installed package root; cizon derives -I/-L from it
extern_links = ["gtest_main", "gtest"]  # installed external libraries, mapped to -l in declaration order
```

Declaring the first `[[target]]` switches the project from implicit to explicit mode: the automatic bin from `src/` is no longer created. The minimal migration is one block (a lone bin takes the project name, and omitting `sources` still scans `src/`):

```toml
[[target]]
kind = "bin"
```

Each target accepts seven keys:

| Key | Required | Meaning |
|---|---|---|
| `name` | Depends | A single bin may omit it and use the project name; libraries, tests, and multiple bins must name it |
| `kind` | Yes | `bin` / `static_lib` / `shared_lib` / `test` |
| `sources` | Depends | A bin may omit it and scan `src/`; libraries and tests must provide it and it cannot be empty |
| `include` | No | Project-relative public header root. Libraries propagate it through `links`; bins/tests use it only for themselves |
| `links` | No | Project libraries linked by a bin/test, static libraries packed into a shared library, or public static dependencies of a static library |
| `prefix` | No | Installed-package root; cizon expands it to `-I<prefix>/include` (compile) and `-L<prefix>/lib` (link) for this target only |
| `extern_links` | No | Test targets only: external library names mapped to `-l<name>` at link time |

Rules:

- Without `[[target]]`, the project is the traditional single-executable project, with sources from `[project].sources` or `src/`
- With `[[target]]`, the implicit bin is gone: every target declares its own sources, and `[project].sources` is rejected. At least one target is required; a library-only project is valid
- Library-only projects are supported: `build` produces the declared libraries, `run` reports that there is no bin, and `test` reports `no test targets`. Give the library a test target when you want it verified; a library that must be installed or exported for other projects belongs in CMake
- Source files left under `src/` that no target compiles trigger a warning, since the implicit build no longer picks them up
- A single bin may omit `name` and use `[project].name`; with multiple bins every bin must be named explicitly; tests must always be named
- A test target shares the bin compile/link pipeline but is built and executed only by `cizon test`; `build` / `run` ignore it. Projects without bins (test-only or library-only) build their libraries successfully and produce no exe
- Test sources never define `-DNDEBUG`, so assert-based tests cannot be compiled away and silently pass; release builds define NDEBUG for project and dependency sources, while `cizon test --release` still strips it from test sources. Because of this, inline assert-bearing headers can differ between test and library TUs; keeping tests live is the accepted trade-off
- `extern_links` is accepted only on test targets; cizon does not probe, download, or version-lock them. An unresolved `-l<name>` prints an installation hint
- `extern_links` is appended in declaration order; static-library dependents come first (`gtest_main` depends on `gtest`, so write `["gtest_main", "gtest"]`)
- `prefix` declares an installed package root (project-relative, must exist); cizon derives `-I<prefix>/include` and `-L<prefix>/lib` for that target only, so consuming an installed library needs no hand-written search-path flags. It assumes the standard `include/` + `lib/` layout. It is input-only and is not propagated: a library's public headers must be self-contained or get external includes from project-level dependencies. Keep external build/install artifacts under `deps/.build/<name>` / `deps/.install/<name>`, not at the project root
- Test artifacts live in `build/<mode>-<compiler>/` (`<name>.exe` on Windows, `<name>` on Linux) and their objects in `build/<mode>-<compiler>/test-obj/<name>/`, fully isolated from the bin `obj/` tree
- Target names are case-insensitive and unique, and must pass file-name validation: not `obj` / `lib` / `deps-obj`, no leading dot, and no Windows device names. A target named `shared` is allowed because the internal directory is `.shared`
- `sources` must be project-relative paths (no absolute paths or `..`); files and directories are accepted and directories are scanned recursively; same-named sources in different directories are allowed
- `include` adds a target-owned public header root on top of the automatic project-root `include/`; it must exist, be a directory, stay inside the project, and must not be the project root. Missing paths fail during planning, before compilation
- Cizon checks root `include/`, automatic dependency include roots, and target/library `include` roots for duplicate public-header relative paths. Root `include/` shadowing a linked library/dependency header, or duplicates between linked library include roots, is a plan error; comparisons are case-insensitive on Windows. Collisions between two dependency include roots remain allowed and follow dependency search order
- Include-directory symlinks/junctions are scanned for duplicates and must stay inside the project; cycle guards prevent repeated scanning
- For `static_lib` / `shared_lib`, `include` is PUBLIC: the library's own sources use it, and every direct or transitive consumer through `links` inherits it. For a bin/test it applies only to that target
- `static_lib` may link existing `static_lib` targets as public dependencies; `shared_lib` may link existing `static_lib` targets only; each `bin` / `test` may link both, with static archives first, then import libraries
- Link order inside the static dependency graph is derived dependent-first (consumers before their dependencies), so consumers do not hand-order archives; cycles are an error (cizon does not add `--start-group`)
- Every `links` entry must reference an existing target and duplicates are rejected; shared libraries cannot link other shared libraries, and transitive DLL graphs are a CMake job
- `include` and `prefix` point in opposite directions: `prefix` is an input for consuming an installed package (target-local `-I`/`-L`, never propagated); `include` is the target's public output, propagated through `links`
- Public headers go in the project-root `include/` or a target's `include` root; a library's private headers may sit next to its sources
- Static archives use `ar` (g++) or `llvm-ar` (clang++); the artifact is `build/<mode>-<compiler>/lib/<name>/lib<name>.a`
- A shared library produces `build/<mode>-<compiler>/<name>.dll` plus an import library under `build/<mode>-<compiler>/.shared/<name>/` on Windows, or `build/<mode>-<compiler>/lib<name>.so` on Linux
- On Windows, MinGW uses `lib<name>.dll.a` and clang++ (MSVC target) uses `<name>.lib`; cizon passes the import-library flag explicitly
- On Linux, objects that may land in a `.so` (dependency objects, static libraries, the shared target itself) compile with `-fPIC`; consumers link the `.so` by name with `-Wl,-rpath,$ORIGIN`, so the executable finds it next to itself with no `LD_LIBRARY_PATH`
- Linux `.so` files carry an unversioned SONAME (`lib<name>.so`), so consumers do not depend on how they spelled the path; versioned SONAMEs / SOVERSION remain an install/export concern for CMake
- Exports are explicit C ABI: use `extern "C"` with `__declspec(dllexport)` on Windows or `__attribute__((visibility("default")))` on Linux (the generated shared template shows both branches). cizon does not auto-export symbols, generate bindings, or guarantee the C++ ABI
- On MinGW, `shared_lib` links its runtime statically by default, so the DLL has no `libstdc++`/`libgcc`/`libwinpthread` dependency; Python `ctypes` can load it without touching PATH. Keep the boundary C ABI: with separate static runtimes, do not pass C++ objects or exceptions across the DLL boundary
- Every bin produces `build/<mode>-<compiler>/<name>` (`<name>.exe` on Windows); `cizon run <bin>` builds all bins and runs the selected one; an omitted name is only valid when the project has exactly one bin
- `run --all` is not supported: cizon does not supervise multiple processes. Use a shell/Python harness for IPC or multi-process tests
- Object paths mirror source paths: a single bin uses `obj/<project-relative-path>.o`; multiple bins use `obj/<bin>/<project-relative-path>.o`; static libraries use `lib/<name>/obj/...`, shared libraries `.shared/<name>/obj/...`, dependencies `deps-obj/<dep>/`
- Same stem in the same directory (e.g. `util.cpp` + `util.cc`) is still an error; rename one
- `new` / `init` default to the implicit single-executable skeleton; `--kind static_lib|shared_lib` generate target-based library projects with `include/<name>/`, `src/`, and `tests/`, so public headers keep their library ownership

### Test framework recipes

`cizon test` does not know any framework; it only reads the process exit code. Common setups:

- **gtest (system package)**: `pacman -S mingw-w64-ucrt-x86_64-gtest`, then use `extern_links = ["gtest_main", "gtest"]` on the test target; the UCRT64 sysroot already provides the headers.
- **doctest / Catch2 v2 (header-only)**: vendor the single header under `include/` (for example `include/doctest/doctest.h`) and define `DOCTEST_CONFIG_IMPLEMENT_WITH_MAIN` or `CATCH_CONFIG_MAIN` in one test source; no `extern_links` needed.
- **Catch2 v3 (compiled library)**: after a system install use `extern_links = ["Catch2Main", "Catch2"]`; without a system package, build it with CMake — cizon does not download or build frameworks.

Declaration order is link order. Static libraries are order-sensitive, so gtest must be written as `gtest_main` before `gtest`.

### Using a DLL from Python

```python
import ctypes

lib = ctypes.CDLL("build/debug-g++/shape_dll.dll")
print(lib.shape_area(3, 4))   # an extern "C" function exported by the DLL
```

Python only needs the DLL; the import library exists for C/C++ consumers. Python and Rust `libloading` callers both rely on the C ABI. Rust link-time consumers must match the import-library flavor: MinGW uses `.dll.a`, MSVC uses `.lib`. Direct C++ ABI export is outside the current contract; use a C wrapper or a pure-virtual/opaque-handle interface.

## Dependencies and the lock file

```bash
cizon add https://github.com/fmtlib/fmt --tag 11.0.2
cizon update fmt
```

- `cizon.lock` is a machine file; the pinned commit is the single source of truth, and `build` only does what the lock already determines.
- `add` / `update` shallow-clone; `build` restores from the lock when `deps/` is missing.
- To upgrade or downgrade, change only the `tag` in `cizon.toml`, then run `cizon update <name>`.
- `update` replaces the entire `deps/<name>` tree and refuses when the directory has uncommitted changes. To keep local commits, push them and update the `tag` first.
- Dependencies follow a fixed layout: headers in `include/` plus flat `src/*.cpp`. Cizon checks out the git source and compiles those sources directly; it does not run dependency build systems or install steps. For other layouts, use CMake.
- When `deps/<name>` has no `include/`, `add` prints a one-time layout note (later `build`/`test` do not repeat it): header-only libraries may already be usable, while compiled libraries should go through the system package manager plus `extern_links`, or be vendored header-only.
- Dependency source objects are linked into the bin and every shared library; dependencies are project-wide, not per-target.
- If an external dependency needs its own cmake build/install, keep the build tree and install prefix under `deps/.build/<name>` and `deps/.install/<name>`, not at the project root; the root keeps only source directories plus `deps/` and `build/`.
- Any mismatch among the git/tag in `cizon.toml`/`cizon.lock`, the `deps/` HEAD, and the working tree is a loud error; a missing dependency directory is restored from the lock, never silently self-healed.

## Editors (clangd / VS Code)

Enable in `cizon.toml`:

```toml
[project]
compile_commands = true
```

After `cizon build`, the project root gets `compile_commands.json` containing full compile flags for project and dependency sources. With `compile_commands` off, cizon no longer updates the database or creates the VS Code config; delete them manually if you no longer need them.

The Microsoft C/C++ extension does not read `compile_commands.json` on its own. When `compile_commands` is enabled, cizon therefore also creates `.vscode/c_cpp_properties.json` if that file is missing, pointing it at the generated database, the detected compiler, and `include/`. Existing files are never overwritten; delete the file and rebuild to regenerate it. If the file cannot be written, the build continues with a warning. clangd ignores this file. New projects' `.gitignore` excludes it; add the same line to older projects if you do not want to commit machine-specific compiler paths.

If clangd was already running before the database was generated, restart the language server once (VS Code: `clangd: Restart language server`); later database updates are picked up automatically. If clangd still cannot find the standard library in a MinGW/g++ setup, add `--query-driver=<path to g++.exe>` to clangd's arguments.

## Debugging (VS Code)

`cizon build` is a debug build (`-g`) by default with a fixed artifact path, but breakpoint debugging requires VS Code's debug extension (cpptools + gdb) to own the process; `cizon run` itself only executes the artifact.

One-time `.vscode/tasks.json`:

```json
{
  "version": "2.0.0",
  "tasks": [
    {
      "label": "cizon: build",
      "type": "shell",
      "command": "cizon",
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
      "name": "cizon: debug (g++)",
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
      "preLaunchTask": "cizon: build"
    }
  ]
}
```

- Replace `hello.exe` in `program` with the bin target name (the project name when `name` is omitted); replace `debug-g++` with the actual `<mode>-<compiler>` scope
- Point `miDebuggerPath` at your local gdb; if `cizon` is not on PATH, set the task `command` to its full path
- g++/MinGW uses `cppdbg + gdb`; clang++ (MSVC target) needs cpptools' `cppvsdbg` or the LLDB extension, configured differently
- On MSYS2 UCRT64, install gdb with `pacman -S mingw-w64-ucrt-x86_64-gdb`
- `--release` artifacts are poor debugging targets; use the default debug build for breakpoints

## Incremental builds and sandbox

- A newer source file or header (tracked via `.d` files): only affected files are recompiled
- Project source compile inputs (edition, optimization level, `[compiler].flags`, dependency include set) change: project objects are invalidated and rebuilt; dependency sources follow only edition, optimization, and dependency includes, never `[compiler].flags`
- Object paths mirror source paths; a build with an older object layout is detected and does one full rebuild
- Test objects live in `build/<mode>-<compiler>/test-obj/<name>/` with independent link state; `cizon test <name>` builds only the selected test and never removes other test artifacts
- Test sources compile without `-DNDEBUG`; changing `extern_links` changes the link fingerprint and forces a relink
- Target include paths change: affected targets and their consumers are invalidated and recompiled
- Link inputs (object list, static archives, `extern_links`, `link_flags`) change: forced relink
- Static library sources change: only that archive is rebuilt, then relinked; changing only the bin does not rebuild archives
- Shared library sources change: the DLL relinks; if the import library fingerprint is unchanged, the bin is not relinked. Export changes relink the bin
- Removing or renaming a target cleans its exe, archive/DLL, import library and state files
- Compiler version changes: the whole `build/<mode>-<compiler>/` scope is invalidated
- A project-level lock (`build/.cizon-build.lock`) rejects a second concurrent build. If no build is running, delete a stale lock file and retry

## Troubleshooting

### `<print>` / `std::println` fails to link with GCC

Typical errors:

```
undefined reference to `std::__open_terminal(_iobuf*)'
undefined reference to `std::__write_to_terminal(void*, std::span<char, ...>)'
```

Cause: libstdc++ ships the C++23 `<print>` functions in an experimental library, `libstdc++exp`, which must be linked explicitly; g++ does not add it automatically. This is libstdc++ packaging, not specific to MinGW, and not a cizon problem.

Add it to the link flags in `[compiler]`:

```toml
[compiler]
link_flags = ["-lstdc++exp"]
```

cizon appends `link_flags` to the end of the link command; with that, `std::print` / `std::println` link normally.

Alternatives:

- Switch to clang++ (MSVC target with MSVC STL): no extra library needed; if clang++ uses libstdc++, `-lstdc++exp` is still required
- Avoid `<print>` and use `std::format` + `std::cout`

### Runtime DLL version conflicts (Windows)

Symptom: the exe exits immediately with `0xC0000135` (DLL not found) or `0xC0000139` (entry point not found). The latter usually means an older `libstdc++-6.dll` / `libgcc_s_seh-1.dll` earlier in PATH (for example Git for Windows' bundled MinGW) shadows the compiler's runtime; libraries that reference newer symbols (such as gtest) then fail at startup.

cizon puts the compiler's bin directory first in the child PATH for `run` / `test`, avoiding this shadowing proactively. If it still happens (e.g. launching the exe directly), put the compiler's bin directory earlier in PATH, or link with `-static`.

## Usage notes

- When `[[target]]` is used, at least one target is required; library-only and test-only projects are valid. `cizon run <bin>` runs bins only, and an omitted name is only valid with exactly one bin
- `cizon test` builds and runs test targets only; exit 0 when every test exits 0, otherwise 1; no test targets prints `no test targets` and exits 0
- Test targets never define `-DNDEBUG`; frameworks are supplied by the user, and external libraries are wired in explicitly through `extern_links`
- `run --all` is not supported; multi-process/IPC orchestration belongs in an external harness
- Static library artifacts are `.a` (GNU archives); on Windows `shared_lib` produces a DLL plus an import library, and on Linux a `.so` without an import library
- Shared libraries use the C ABI for Python/Rust; direct C++ ABI export is not guaranteed
- Multiple DLLs sit next to the exe; the Windows loader finds them without PATH changes
- The project-root `include/` is the only automatic project public-header `-I`; a target's `include` is explicit and propagates through `links`, and dependency directories are also added automatically. Keep private headers out of all of them and do not expose them with a global `-Ilib`
- `sources` must be project-relative paths; a build with an older object layout does one full rebuild
- `[project].version` is metadata only; it does not affect compilation or artifact names
- A project-level lock (`build/.cizon-build.lock`) rejects a second concurrent build; delete a stale lock only when no build is running
- Turning `compile_commands` off does not delete an existing `compile_commands.json` or `.vscode/c_cpp_properties.json`; delete them manually when no longer needed
- Unknown sections or unknown keys in known sections are hard errors, never silently ignored
- Compiler warnings (e.g. ignoring a `[[nodiscard]]` return value) do not fail the build; set `[compiler] werror = true` for strict mode
- Overlapping sources across bin/test targets (common with recursive directories) print a warning; multiple `main()` definitions fail to link — use explicit files or separate directories
- `run`/`test` put the compiler's bin directory first in the child PATH to avoid older runtime DLLs shadowing it (see Troubleshooting)

## Roadmap and known issues

Items that are not committed work yet advance on real demand and the gates in AGENTS.md.

### Debugging integration (unscheduled)

The goal is first-class breakpoint debugging, but **cizon does not implement a debugger, the DAP/MI protocol, or breakpoint UI**; breakpoints, variables, and call stacks remain gdb / lldb / cppvsdbg's job.

- Artifact contract: debug builds use `-g`, are not stripped, and land at the fixed path `build/debug-<compiler>/<bin>.exe`
- `cizon run` stays a direct executor and will not become a debugger launcher; debugging is driven by an editor launch configuration plus a `preLaunchTask` build
- Preferred trigger: `[project] vscode = true` (off by default), generating `.vscode/launch.json` and `.vscode/tasks.json` at build time from the current plan; a one-shot `build --vscode` is the fallback, while a standalone `debug` command would trip the "no new commands" gate and is not planned
- Generation is non-destructive: existing files are never overwritten; only missing files are added or paste-ready snippets printed; a target rename or compiler switch refreshes them on the next build
- Debugger choice: g++/MinGW → `cppdbg` + gdb; clang++ (MSVC target) → `cppvsdbg`/PDB; LLDB for clang++ (GNU target) comes later
- v1 is validated on float_test with g++/MinGW + gdb; tests cover configuration content, scope/paths, non-overwrite behavior, and a clear error when gdb is missing—without actually starting a debugger
- Terminal-only `cizon run --debug` cannot provide VS Code breakpoints and is not a substitute

### Reliability

- exe, build state, `cizon.lock` and `compile_commands.json` are written through a sibling temporary file and then replaced; Unix uses an atomic rename, Windows removes the old file before renaming
- Disabling `compile_commands` leaves the generated `compile_commands.json` and `.vscode/c_cpp_properties.json` behind; delete them manually
- Removing a dependency leaves `cizon.lock`/`deps/` untouched; a separate remove command may be needed (would trip the second gate)

### Diagnostics and engineering

- Config structure errors come from serde; messages such as `unknown field` are English (low-priority style issue)
- Windows tests run locally; tagged Linux CI jobs run the full suite
- `rustfmt`/`clippy` are not wired in yet
- CTest labels / fixtures / parallel / timeout / report files wait for real demand

### Wait for real demand

- Per-target compile flags/definitions and conditional target selection
- install/export, a one-shot CMake export command
- MSVC, cross-compilation, external package managers
- Consumers of a generated header/compile macro for `[project].version`
- Compiler discovery in common install locations and version selection

## When to use CMake

Cizon targets small-to-medium projects with one or more bins (optionally with static/shared library targets, including acyclic static-library dependencies and their public includes). The v0.10 `[subproject]` boundary allows local library/test composition under one root build, not independent package lifecycles. Reach for CMake when you need:

- Workspaces or monorepo packages with independent builds, compilers, locks, or releases
- Conditional or very large target graphs, per-target compile flags/definitions, generated sources
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

Pushing a `v*` tag runs `.github/workflows/release.yml`: it builds the Windows and Linux binaries, runs the test suite on Linux (the only gate for `.so` support), uploads `cizon-v<version>-windows-x86_64.exe` and `cizon-v<version>-linux-x86_64` plus SHA-256 files, and creates or updates the GitHub Release. Manual dispatch accepts an existing tag (for example `v0.10.0`). Windows tests stay local.

See [AGENTS.md](AGENTS.md) for detailed boundaries, gates, and roadmap (currently in Chinese).
