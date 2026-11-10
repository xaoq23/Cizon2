    use super::*;
    use super::output::*;
    use super::targets::*;
    use std::fs;
    use std::process::Command;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cizon2-bl-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(windows)]
    fn make_dir_link(link: &Path, target: &Path) -> bool {
        Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[cfg(unix)]
    fn make_dir_link(link: &Path, target: &Path) -> bool {
        std::os::unix::fs::symlink(target, link).is_ok()
    }

    fn remove_dir_link(path: &Path) {
        #[cfg(windows)]
        let _ = fs::remove_dir(path);
        #[cfg(unix)]
        let _ = fs::remove_file(path);
    }

    #[test]
    fn user_flags_are_linked_once() {
        let dir = temp_dir("linkflags");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}").unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname=\"t\"\n\n[compiler]\nflags=[\"-Wall\"]\nlink_flags=[\"-lm\"]\n",
        )
        .unwrap();
        let cfg = crate::config::load(&dir).unwrap();
        let cxx = Cxx { name: "g++".into(), version: "test".into() };
        let p = plan(&dir, &cfg, cxx, false).unwrap();
        assert_eq!(p.flags.iter().filter(|f| *f == "-Wall").count(), 1);
        assert!(!p.flags.iter().any(|f| f == "-lm"));
        assert_eq!(p.link_flags, vec!["-lm".to_string()]);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn compiler_path_is_sanitized_for_scope_dir() {
        assert_eq!(scope_label("g++"), "g++");
        assert_eq!(scope_label(r"C:\mingw64\bin\g++.exe"), "C__mingw64_bin_g++.exe");
        assert_eq!(scope_label("/usr/bin/clang++"), "_usr_bin_clang++");
    }

    #[test]
    fn warning_section_has_gold_divider_and_header() {
        let dir = Path::new("proj");
        let cxx = Cxx { name: "g++".into(), version: "g++ 14.2.0".into() };
        let warnings = vec![(
            dir.join("src").join("main.cpp"),
            "main.cpp:54: warning: ignored\n".to_string(),
        )];
        let text = warning_section_text(dir, &cxx, &warnings);
        assert!(text.starts_with("  \x1b[38;2;255;241;168m"));
        assert_eq!(text.matches('─').count(), 88, "two 44-wide gold dividers, top and bottom");
        assert!(text.contains("Warning"), "got: {text}");
        assert!(text.contains("main.cpp"), "got: {text}");
        assert!(text.contains("g++ 14.2.0"), "got: {text}");
        assert!(text.contains("warning: ignored"), "got: {text}");
    }

    #[test]
    fn finish_note_distinguishes_relink() {
        assert_eq!(finish_note(0, 3), "relinked");
        assert_eq!(finish_note(3, 3), "fresh build");
        assert_eq!(finish_note(1, 3), "incremental rebuild");
    }

    #[test]
    fn compiler_diagnostics_paths_are_relativized() {
        let dir = Path::new("proj");
        let sep = std::path::MAIN_SEPARATOR;
        let raw = format!("{}{sep}src{sep}main.cpp:1: warning: x", dir.display());
        assert_eq!(
            relativize_diagnostics(dir, &raw),
            format!("src{sep}main.cpp:1: warning: x")
        );
        let forward = format!(
            "{}/src/main.cpp:1: error: y",
            dir.display().to_string().replace('\\', "/")
        );
        assert_eq!(
            relativize_diagnostics(dir, &forward),
            "src/main.cpp:1: error: y"
        );
        assert_eq!(relativize_diagnostics(dir, "other.cpp:1: warning"), "other.cpp:1: warning");
    }

    #[test]
    fn rel_display_strips_project_root() {
        let base = Path::new("proj");
        let inside = base.join("build").join("app.exe");
        assert_eq!(
            rel_display(base, &inside),
            Path::new("build").join("app.exe").display().to_string()
        );
        let outside = Path::new("elsewhere").join("app.exe");
        assert_eq!(rel_display(base, &outside), outside.display().to_string());
    }

    #[test]
    fn reset_dir_reports_failure() {
        let dir = temp_dir("reset-file");
        let file = dir.join("not-a-dir");
        fs::write(&file, "").unwrap();
        let err = reset_dir(&file).unwrap_err();
        assert!(err.contains("Cannot clear"), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn split_state_clears_only_its_objects() {
        let dir = temp_dir("split-clear");
        let bin_obj = dir.join("obj");
        let deps_obj = dir.join("deps-obj");
        fs::create_dir_all(bin_obj.join("src")).unwrap();
        fs::create_dir_all(deps_obj.join("dep")).unwrap();
        fs::write(bin_obj.join("src/main.o"), "").unwrap();
        fs::write(deps_obj.join("dep/dep.o"), "").unwrap();
        clear_project_objects(&bin_obj).unwrap();
        assert!(!bin_obj.exists());
        assert!(deps_obj.join("dep/dep.o").exists(), "project flags change must not delete dependency objects");
        clear_dep_objects(&deps_obj).unwrap();
        assert!(!deps_obj.exists());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn dep_include_order_prefers_own_dependency() {
        let mut includes = BTreeMap::new();
        includes.insert("a".to_string(), vec!["-I/a".to_string()]);
        includes.insert(
            "b".to_string(),
            vec!["-I/b/include".to_string(), "-I/b".to_string()],
        );
        let p = Plan {
            cxx: Cxx { name: "g++".into(), version: "test".into() },
            bins: vec![],
            flags: vec![],
            link_flags: vec![],
            dep_base: vec!["-std=c++23".to_string()],
            dep_includes: includes,
            libs: vec![],
            shared: vec![],
            mode: "debug",
        };
        let flags = p.dep_flags_for("b");
        let pos = |needle: &str| flags.iter().position(|f| f == needle).unwrap();
        assert!(pos("-I/b/include") < pos("-I/a"), "own headers must precede other dependencies");
        assert!(pos("-I/b") < pos("-I/a"));
    }

    #[test]
    fn collect_sources_from_src_dir() {
        let dir = temp_dir("collect");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}").unwrap();
        fs::write(dir.join("src/util.cc"), "//").unwrap();
        fs::write(dir.join("src/notes.txt"), "ignore").unwrap();
        let s = collect_sources(&dir, None).unwrap();
        let names: Vec<String> = s
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["main.cpp", "util.cc"]);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn collects_uppercase_source_extension() {
        let dir = temp_dir("upper-ext");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/Util.CPP"), "//").unwrap();
        let s = collect_sources(&dir, None).unwrap();
        assert_eq!(s.len(), 1, "uppercase extension must not be missed on Windows");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn explicit_sources_resolve_and_validate() {
        let dir = temp_dir("explicit");
        fs::create_dir_all(dir.join("third_party")).unwrap();
        fs::write(dir.join("third_party/x.cpp"), "//").unwrap();
        let list = vec!["third_party/x.cpp".to_string()];
        let s = collect_sources(&dir, Some(&list)).unwrap();
        assert_eq!(s, vec![dir.join("third_party/x.cpp")]);

        let missing = vec!["missing.cpp".to_string()];
        let err = collect_sources(&dir, Some(&missing)).unwrap_err();
        assert!(err.contains("does not exist"), "got: {err}");

        fs::write(dir.join("notes.txt"), "").unwrap();
        let bad_ext = vec!["notes.txt".to_string()];
        let err = collect_sources(&dir, Some(&bad_ext)).unwrap_err();
        assert!(err.contains("only accepts"), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn explicit_sources_accept_directory() {
        let dir = temp_dir("explicit-dir");
        fs::create_dir_all(dir.join("app/sub")).unwrap();
        fs::write(dir.join("app/a.cpp"), "//").unwrap();
        fs::write(dir.join("app/sub/b.cc"), "//").unwrap();
        fs::write(dir.join("app/notes.txt"), "ignore").unwrap();
        let list = vec!["app".to_string()];
        let s = collect_sources(&dir, Some(&list)).unwrap();
        let names: Vec<String> = s
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["a.cpp", "b.cc"]);
        let root = vec![".".to_string()];
        let err = collect_sources(&dir, Some(&root)).unwrap_err();
        assert!(err.contains("project root"), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn empty_default_src_message_is_relative() {
        let dir = temp_dir("empty-src-message");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();
        let cfg = crate::config::load(&dir).unwrap();
        let cxx = Cxx { name: "g++".into(), version: "test".into() };
        let err = plan(&dir, &cfg, cxx, false).unwrap_err();
        assert!(err.contains("No .cpp file under src"), "got: {err}");
        assert!(!err.contains(&dir.display().to_string()), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn empty_explicit_sources_message_points_at_sources() {
        let dir = temp_dir("explicit-empty");
        fs::create_dir_all(dir.join("code")).unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\nsources = [\"code\"]\n",
        )
        .unwrap();
        let cfg = crate::config::load(&dir).unwrap();
        let cxx = Cxx { name: "g++".into(), version: "test".into() };
        let err = plan(&dir, &cfg, cxx, false).unwrap_err();
        assert!(err.contains("[project] sources"), "got: {err}");
        assert!(!err.contains("Put source code in src/"), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn same_directory_stem_conflict_detected() {
        let dir = temp_dir("same-dir-conflict");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/util.cpp"), "//").unwrap();
        fs::write(dir.join("src/util.cc"), "//").unwrap();
        let err = collect_sources(&dir, None).unwrap_err();
        assert!(err.contains("object path conflict"), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn object_path_cannot_be_parent_directory() {
        let dir = temp_dir("obj-parent");
        fs::create_dir_all(dir.join("src/foo.o")).unwrap();
        fs::write(dir.join("src/foo.cpp"), "//").unwrap();
        fs::write(dir.join("src/foo.o/bar.cpp"), "//").unwrap();
        let err = collect_sources(&dir, None).unwrap_err();
        assert!(err.contains("conflicts with a directory"), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn duplicate_explicit_source_reported_as_duplicate() {
        let dir = temp_dir("dup-source");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "//").unwrap();
        let list = vec!["src/main.cpp".to_string(), "src/main.cpp".to_string()];
        let err = collect_sources(&dir, Some(&list)).unwrap_err();
        assert!(err.contains("more than once"), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn depfile_parsing_with_continuation() {
        let dir = temp_dir("depfile");
        let f = dir.join("main.d");
        fs::write(
            &f,
            "build/obj/main.o: \\\n src/main.cpp src/util.hpp \\\n third_party/x.h\n\nsrc/util.hpp:\n",
        )
        .unwrap();
        let inputs = dep_inputs(&f).unwrap();
        let names: Vec<String> = inputs
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        assert!(names.iter().any(|p| p.contains("main.cpp")));
        assert_eq!(names.len(), 3, "should skip -MP phony line: {names:?}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn depfile_parsing_handles_escaped_spaces() {
        let dir = temp_dir("depfile-space");
        let f = dir.join("main.d");
        fs::write(
            &f,
            "main.o: C:\\path\\ with\\ space\\src\\main.cpp \\\n  C:\\path\\ with\\ space\\src\\h.h\n\n\
             C:\\path\\ with\\ space\\src\\h.h:\n",
        )
        .unwrap();
        let inputs = dep_inputs(&f).unwrap();
        let names: Vec<String> = inputs
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        assert_eq!(names.len(), 2, "space must stay inside the token: {names:?}");
        assert!(
            names.iter().any(|p| p.contains("with space")),
            "escaped space should be restored: {names:?}"
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn obj_missing_means_recompile() {
        let dir = temp_dir("objmissing");
        let src = dir.join("a.cpp");
        fs::write(&src, "//").unwrap();
        let obj = dir.join("nope.o");
        assert!(needs_recompile(&src, &obj));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unreadable_depfile_triggers_recompile() {
        let dir = temp_dir("baddep");
        let src = dir.join("a.cpp");
        let obj = dir.join("a.o");
        fs::write(&src, "//").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&obj, "").unwrap();
        // A directory masquerading as .d: exists but unreadable — must be treated as "needs recompile"
        fs::create_dir(dir.join("a.o.d")).unwrap();
        assert!(needs_recompile(&src, &obj));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn malformed_depfile_triggers_recompile() {
        let dir = temp_dir("garbagedep");
        let src = dir.join("a.cpp");
        let obj = dir.join("a.o");
        fs::write(&src, "//").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&obj, "").unwrap();
        fs::write(dir.join("a.o.d"), "garbage without target separator").unwrap();
        assert!(needs_recompile(&src, &obj));
        fs::remove_dir_all(dir).ok();
    }

    // ── P0: incremental build correctness (real-compiler end-to-end) ───────────────
    // Shared premise: after a config or source-set change, the artifact must change accordingly;
    // when nothing changed, objects and exe must not move a single byte.

    fn have_compiler() -> bool {
        crate::compiler::pick(None).is_ok()
    }

    fn shared_supported() -> bool {
        cfg!(windows) && have_compiler()
    }

    fn build_project(dir: &Path) -> Result<PathBuf, String> {
        let cfg = crate::config::load(dir)?;
        let cxx = crate::compiler::pick(None)?;
        let plan = plan(dir, &cfg, cxx, false)?;
        build(dir, &cfg, &plan)
    }

    /// Find stem.o under build/<scope>/obj/ (scope name varies by platform, tests do not hardcode it).
    fn find_named(dir: &Path, name: &str) -> Option<PathBuf> {
        for entry in fs::read_dir(dir).ok()?.flatten() {
            let path = entry.path();
            if path.is_file() && path.file_name().is_some_and(|n| n == name) {
                return Some(path);
            }
            if path.is_dir() {
                if let Some(found) = find_named(&path, name) {
                    return Some(found);
                }
            }
        }
        None
    }

    fn obj_path(dir: &Path, stem: &str) -> PathBuf {
        let target = format!("{stem}.o");
        for scope in fs::read_dir(dir.join("build")).unwrap().flatten() {
            if let Some(path) = find_named(&scope.path().join("obj"), &target) {
                return path;
            }
        }
        panic!("did not find {target}");
    }

    fn scope_of(dir: &Path, path: &Path) -> PathBuf {
        for scope in fs::read_dir(dir.join("build")).unwrap().flatten() {
            if path.starts_with(scope.path()) {
                return scope.path();
            }
        }
        panic!("did not find a build scope for {}", path.display());
    }

    fn mtime(p: &Path) -> std::time::SystemTime {
        fs::metadata(p).unwrap().modified().unwrap()
    }

    fn run_code(exe: &Path) -> i32 {
        Command::new(exe)
            .status()
            .unwrap()
            .code()
            .unwrap_or(-1)
    }

    fn run_stdout(exe: &Path) -> String {
        let out = Command::new(exe).output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[test]
    fn no_op_rebuild_keeps_objects_and_exe() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("noop");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}\n").unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();

        let exe = build_project(&dir).unwrap();
        let obj = obj_path(&dir, "main");
        let (obj_t, exe_t) = (mtime(&obj), mtime(&exe));
        std::thread::sleep(std::time::Duration::from_millis(50));
        build_project(&dir).unwrap();
        assert_eq!(mtime(&obj), obj_t, "a no-change build must not recompile objects");
        assert_eq!(mtime(&exe), exe_t, "a no-change build must not relink the exe");
        fs::remove_dir_all(dir).ok();
    }

    /// A compiler flags change touches no source file, but must invalidate objects:
    /// use -DVALUE=n so the old object's and old exe's return value is observable.
    #[test]
    fn compile_flag_change_rebuilds_objects() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("cflags");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(
            dir.join("src/main.cpp"),
            "#ifndef VALUE\n#define VALUE 0\n#endif\nint main(){return VALUE;}\n",
        )
        .unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n[compiler]\nflags = [\"-DVALUE=1\"]\n",
        )
        .unwrap();

        let exe = build_project(&dir).unwrap();
        // Runtime copy: on Windows a just-executed exe may be briefly held by the OS, blocking relink
        let first = dir.join("first.exe");
        fs::copy(&exe, &first).unwrap();
        assert_eq!(run_code(&first), 1, "the first build must compile with VALUE=1");
        let obj = obj_path(&dir, "main");
        let obj_t = mtime(&obj);
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n[compiler]\nflags = [\"-DVALUE=2\"]\n",
        )
        .unwrap();

        let exe = build_project(&dir).unwrap();
        assert_eq!(
            run_code(&exe),
            2,
            "a flags change must recompile objects and relink the exe (otherwise still the old VALUE=1)"
        );
        assert_ne!(mtime(&obj), obj_t, "a flags change must recompile objects");
        fs::remove_dir_all(dir).ok();
    }

    /// link_flags only affect the link: the exe must update, objects must stay untouched.
    #[test]
    fn link_flag_change_relinks_without_recompiling() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("lflags");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}\n").unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n[compiler]\nlink_flags = []\n",
        )
        .unwrap();

        let exe = build_project(&dir).unwrap();
        let obj = obj_path(&dir, "main");
        let (obj_t, exe_t) = (mtime(&obj), mtime(&exe));
        std::thread::sleep(std::time::Duration::from_millis(50));
        // -g is accepted on both GNU and MSVC targets; -lm only exists on MinGW/GNU toolchains
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n[compiler]\nlink_flags = [\"-g\"]\n",
        )
        .unwrap();

        let exe = build_project(&dir).unwrap();
        assert_ne!(mtime(&exe), exe_t, "a link_flags change must relink the exe");
        assert_eq!(mtime(&obj), obj_t, "a link_flags change must not recompile objects");
        fs::remove_dir_all(dir).ok();
    }

    /// After deleting a source file the object set shrinks: even if all remaining objects are older
    /// than the exe, a relink is still required, otherwise the old exe still hides the deleted
    /// translation unit's code.
    #[test]
    fn deleting_source_forces_relink() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("delsrc");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(
            dir.join("src/main.cpp"),
            "#include <cstdio>\nint main(){ std::printf(\"main\\n\"); return 0; }\n",
        )
        .unwrap();
        // Standalone static-initialization translation unit: main never references it, so after
        // deletion the old object is not rejected by the linker
        fs::write(
            dir.join("src/helper.cpp"),
            "#include <cstdio>\nstruct Helper { Helper(){ std::printf(\"helper\\n\"); } };\nstatic Helper h;\n",
        )
        .unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();

        let exe = build_project(&dir).unwrap();
        let first = dir.join("first.exe");
        fs::copy(&exe, &first).unwrap();
        assert!(run_stdout(&first).contains("helper"), "the first build must include helper");
        let main_obj_t = mtime(&obj_path(&dir, "main"));
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::remove_file(dir.join("src/helper.cpp")).unwrap();

        let exe = build_project(&dir).unwrap();
        let out = run_stdout(&exe);
        assert!(out.contains("main"), "the rebuilt program must run normally: {out:?}");
        assert!(
            !out.contains("helper"),
            "after deleting a source a relink is required; the old exe still contains the deleted translation unit: {out:?}"
        );
        assert_eq!(run_code(&exe), 0);
        assert_eq!(mtime(&obj_path(&dir, "main")), main_obj_t, "an unchanged source must not recompile");
        fs::remove_dir_all(dir).ok();
    }

    // ── v0.2.1: modern C++ layout (recursive src/ + include/ auto -I) ──

    #[test]
    fn collect_sources_recurses_nested_dirs() {
        let dir = temp_dir("nested");
        fs::create_dir_all(dir.join("src/cli")).unwrap();
        fs::create_dir_all(dir.join("src/core/deep")).unwrap();
        fs::write(dir.join("src/cli/main.cpp"), "//").unwrap();
        fs::write(dir.join("src/core/util.cc"), "//").unwrap();
        fs::write(dir.join("src/core/deep/x.cxx"), "//").unwrap();
        fs::write(dir.join("src/notes.txt"), "ignore").unwrap();
        let s = collect_sources(&dir, None).unwrap();
        assert_eq!(s.len(), 3, "should recursively collect three levels of source files: {s:?}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn same_stem_in_different_dirs_allowed() {
        let dir = temp_dir("nested-same-stem");
        fs::create_dir_all(dir.join("src/a")).unwrap();
        fs::create_dir_all(dir.join("src/b")).unwrap();
        fs::write(dir.join("src/a/core.cpp"), "//").unwrap();
        fs::write(dir.join("src/b/core.cpp"), "//").unwrap();
        let sources = collect_sources(&dir, None).unwrap();
        assert_eq!(sources.len(), 2, "same stem across directories must be allowed");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn same_stem_in_different_dirs_builds() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("nested-same-stem-build");
        fs::create_dir_all(dir.join("src/a")).unwrap();
        fs::create_dir_all(dir.join("src/b")).unwrap();
        fs::write(dir.join("src/a/util.cpp"), "int a(){ return 1; }\n").unwrap();
        fs::write(dir.join("src/b/util.cpp"), "int b(){ return 2; }\n").unwrap();
        fs::write(
            dir.join("src/main.cpp"),
            "int a(); int b();\nint main(){ return a() + b() == 3 ? 0 : 1; }\n",
        )
        .unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();
        let exe = build_project(&dir).unwrap();
        assert_eq!(run_code(&exe), 0);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn default_src_link_outside_project_rejected() {
        let dir = temp_dir("link-src");
        let outside = temp_dir("link-src-outside");
        fs::write(outside.join("x.cpp"), "//").unwrap();
        if !make_dir_link(&dir.join("src"), &outside) {
            fs::remove_dir_all(dir).ok();
            fs::remove_dir_all(outside).ok();
            return;
        }
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();
        let cfg = crate::config::load(&dir).unwrap();
        let cxx = Cxx { name: "g++".into(), version: "test".into() };
        let err = plan(&dir, &cfg, cxx, false).unwrap_err();
        assert!(err.contains("outside the project"), "got: {err}");
        remove_dir_link(&dir.join("src"));
        fs::remove_dir_all(dir).ok();
        fs::remove_dir_all(outside).ok();
    }

    #[test]
    fn target_source_link_outside_project_rejected() {
        let dir = temp_dir("link-target");
        let outside = temp_dir("link-target-outside");
        fs::write(outside.join("x.cpp"), "//").unwrap();
        if !make_dir_link(&dir.join("applink"), &outside) {
            fs::remove_dir_all(dir).ok();
            fs::remove_dir_all(outside).ok();
            return;
        }
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"applink\"]\n",
        )
        .unwrap();
        let cfg = crate::config::load(&dir).unwrap();
        let cxx = Cxx { name: "g++".into(), version: "test".into() };
        let err = plan(&dir, &cfg, cxx, false).unwrap_err();
        assert!(err.contains("outside the project"), "got: {err}");
        remove_dir_link(&dir.join("applink"));
        fs::remove_dir_all(dir).ok();
        fs::remove_dir_all(outside).ok();
    }

    #[test]
    fn include_dir_is_on_include_path() {
        let dir = temp_dir("incflags");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("include")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}").unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname=\"t\"\n").unwrap();
        let cfg = crate::config::load(&dir).unwrap();
        let cxx = Cxx { name: "g++".into(), version: "test".into() };
        let p = plan(&dir, &cfg, cxx, false).unwrap();
        let expected = format!("-I{}", dir.join("include").display());
        assert!(p.flags.contains(&expected), "flags missing {expected}: {:?}", p.flags);
        fs::remove_dir_all(dir).ok();
    }

    /// Modern-layout end-to-end: sources in a subdirectory, public headers in include/; must compile
    /// successfully and run correctly.
    #[test]
    fn nested_sources_use_include_dir() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("nested-e2e");
        fs::create_dir_all(dir.join("src/app")).unwrap();
        fs::create_dir_all(dir.join("include")).unwrap();
        fs::write(dir.join("include/greet.h"), "#pragma once\nint greet();\n").unwrap();
        fs::write(
            dir.join("src/app/main.cpp"),
            "#include \"greet.h\"\nint main(){ return greet() == 7 ? 0 : 1; }\n",
        )
        .unwrap();
        fs::write(
            dir.join("src/app/greet.cpp"),
            "#include \"greet.h\"\nint greet(){ return 7; }\n",
        )
        .unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();

        let exe = build_project(&dir).unwrap();
        assert_eq!(run_code(&exe), 0, "subdirectory sources + include/ must build and run normally");
        fs::remove_dir_all(dir).ok();
    }

    // ── v0.3: compile_commands.json (off by default, explicitly enabled via toml) ──

    #[test]
    fn json_escape_handles_windows_paths_and_quotes() {
        assert_eq!(json_escape(r"C:\a\b"), r"C:\\a\\b");
        assert_eq!(json_escape("a\"b"), r#"a\"b"#);
        assert_eq!(json_escape("a\nb"), r"a\nb");
    }

    #[test]
    fn render_cpptools_config_uses_compiler_and_standard() {
        let json = render_cpptools_config(r"C:\MinGW\bin\g++.exe", "c++23");
        assert!(json.contains("\"version\": 4"), "got: {json}");
        assert!(
            json.contains("\"compilerPath\": \"C:/MinGW/bin/g++.exe\""),
            "got: {json}"
        );
        assert!(json.contains("\"cppStandard\": \"c++23\""), "got: {json}");
        assert!(
            json.contains("\"compileCommands\": \"${workspaceFolder}/compile_commands.json\""),
            "got: {json}"
        );
        if cfg!(windows) {
            assert!(
                json.contains("\"intelliSenseMode\": \"windows-gcc-x64\""),
                "got: {json}"
            );
        }
    }

    #[test]
    fn cpp_standard_uses_effective_last_flag() {
        let flags = vec!["-std=c++17".to_string(), "-std=c++20".to_string()];
        assert_eq!(cpp_standard(&flags), "c++20");
        assert_eq!(cpp_standard(&[]), "c++23");
    }

    #[test]
    fn cpptools_config_is_created_once_and_never_overwritten() {
        let dir = temp_dir("vscode-cfg");
        assert!(
            ensure_cpptools_config(&dir, r"C:\MinGW\bin\g++.exe", "c++23").unwrap(),
            "should generate config when missing"
        );
        let path = dir.join(".vscode").join("c_cpp_properties.json");
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("compile_commands.json"), "got: {text}");

        fs::write(&path, "{\"sentinel\": true}\n").unwrap();
        assert!(
            !ensure_cpptools_config(&dir, r"C:\LLVM\bin\clang++.exe", "c++20").unwrap(),
            "existing file must not be rewritten"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"sentinel\": true}\n");

        fs::remove_file(&path).unwrap();
        assert!(
            ensure_cpptools_config(&dir, r"C:\MinGW\bin\g++.exe", "c++23").unwrap(),
            "should regenerate after deletion"
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn cpptools_config_reports_vscode_file() {
        let dir = temp_dir("vscode-file");
        fs::write(dir.join(".vscode"), "x").unwrap();
        let err = ensure_cpptools_config(&dir, "g++", "c++23").unwrap_err();
        assert!(err.contains(".vscode"), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn render_compile_db_emits_entries() {
        let flags_a = vec!["-std=c++23".to_string(), "-IX".to_string()];
        let flags_b = vec!["-O2".to_string()];
        let entries = [
            (Path::new("src/a.cpp"), flags_a.as_slice()),
            (Path::new("src/b.cpp"), flags_b.as_slice()),
        ];
        let json = render_compile_db(Path::new("proj"), "g++", &entries);
        assert!(json.starts_with("[\n  {"), "got: {json}");
        assert_eq!(json.matches("\"file\"").count(), 2);
        assert!(json.contains(
            "\"arguments\": [\"g++\", \"-c\", \"src/a.cpp\", \"-std=c++23\", \"-IX\"]"
        ));
        assert!(json.contains("},\n  {"), "entries must be separated by a comma: {json}");
        assert!(json.trim_end().ends_with(']'));
    }

    #[test]
    fn compile_db_absent_by_default() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("db-off");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}\n").unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();

        build_project(&dir).unwrap();
        assert!(
            !dir.join("compile_commands.json").exists(),
            "when not enabled, compile_commands.json must not be generated"
        );
        assert!(
            !dir.join(".vscode").join("c_cpp_properties.json").exists(),
            "when not enabled, the VS Code IntelliSense config must not be generated"
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn compile_db_written_when_enabled() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("db-on");
        fs::create_dir_all(dir.join("src/cli")).unwrap();
        fs::create_dir_all(dir.join("include")).unwrap();
        fs::write(dir.join("include/greet.h"), "#pragma once\n").unwrap();
        fs::write(
            dir.join("src/cli/main.cpp"),
            "#include \"greet.h\"\nint main(){return 0;}\n",
        )
        .unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\nedition = \"c++17\"\ncompile_commands = true\n\n\
             [compiler]\nflags = [\"-Wall\"]\n",
        )
        .unwrap();

        build_project(&dir).unwrap();
        let text = fs::read_to_string(dir.join("compile_commands.json")).unwrap();
        assert!(text.trim_start().starts_with('['), "should be a JSON array: {text}");
        assert_eq!(text.matches("\"file\"").count(), 1, "got: {text}");
        assert_eq!(text.matches("\"arguments\"").count(), 1, "got: {text}");
        let file = json_escape(
            &dir.join("src")
                .join("cli")
                .join("main.cpp")
                .display()
                .to_string(),
        );
        assert!(text.contains(&format!("\"file\": \"{file}\"")), "got: {text}");
        assert!(text.contains("-std=c++17"), "got: {text}");
        assert!(text.contains("-Wall"), "got: {text}");
        let include = json_escape(&format!("-I{}", dir.join("include").display()));
        assert!(text.contains(&format!("\"{include}\"")), "got: {text}");
        if cfg!(windows) {
            assert!(text.contains(r"\\"), "Windows path backslashes must be escaped: {text}");
        }
        let vscode_cfg = dir.join(".vscode").join("c_cpp_properties.json");
        let vscode_text = fs::read_to_string(&vscode_cfg).unwrap();
        assert!(
            vscode_text.contains("\"cppStandard\": \"c++17\""),
            "got: {vscode_text}"
        );
        assert!(
            vscode_text
                .contains("\"compileCommands\": \"${workspaceFolder}/compile_commands.json\""),
            "got: {vscode_text}"
        );
        assert!(
            vscode_text.contains("\"compilerPath\": \""),
            "got: {vscode_text}"
        );
        let db = dir.join("compile_commands.json");
        let db_t = mtime(&db);
        let cfg_t = mtime(&vscode_cfg);
        std::thread::sleep(std::time::Duration::from_millis(50));
        build_project(&dir).unwrap();
        assert_eq!(mtime(&db), db_t, "content unchanged, must not rewrite compile_commands.json");
        assert_eq!(mtime(&vscode_cfg), cfg_t, "existing c_cpp_properties.json must not be rewritten");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn compile_failure_then_fix_recovers() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("recover");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(
            dir.join("src/main.cpp"),
            "int main(){ return does_not_exist; }\n",
        )
        .unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();

        let err = build_project(&dir).unwrap_err();
        assert!(err.contains("Compile failed"), "got: {err}");

        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}\n").unwrap();
        let exe = build_project(&dir).unwrap();
        assert_eq!(run_code(&exe), 0);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn header_change_recompiles_only_dependent_tu() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("header-inc");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("include")).unwrap();
        fs::write(dir.join("include/util.h"), "int value();\n").unwrap();
        fs::write(
            dir.join("src/main.cpp"),
            "#include \"util.h\"\nint main(){ return value(); }\n",
        )
        .unwrap();
        fs::write(dir.join("src/util.cpp"), "int value(){ return 0; }\n").unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();

        build_project(&dir).unwrap();
        let main_obj = obj_path(&dir, "main");
        let util_obj = obj_path(&dir, "util");
        let (main_t, util_t) = (mtime(&main_obj), mtime(&util_obj));
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(
            dir.join("include/util.h"),
            "// changed\nint value();\n",
        )
        .unwrap();
        build_project(&dir).unwrap();
        assert_ne!(mtime(&main_obj), main_t, "a TU including the header must recompile");
        assert_eq!(mtime(&util_obj), util_t, "a TU not including that header must not recompile");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn missing_version_marker_invalidates_scope() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("marker-missing");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}\n").unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();
        build_project(&dir).unwrap();
        let obj = obj_path(&dir, "main");
        let obj_t = mtime(&obj);
        let scope = scope_of(&dir, &obj);
        fs::remove_file(scope.join(".compiler-version")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        build_project(&dir).unwrap();
        assert_ne!(mtime(&obj), obj_t, "when the version fingerprint is missing, rebuild");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn build_aborts_when_object_dir_cannot_be_reset() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("obj-blocked");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}\n").unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n[compiler]\nflags = [\"-DVALUE=1\"]\n",
        )
        .unwrap();
        build_project(&dir).unwrap();
        let obj = obj_path(&dir, "main");
        let obj_dir = scope_of(&dir, &obj).join("obj");
        fs::remove_dir_all(&obj_dir).unwrap();
        fs::write(&obj_dir, "").unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n[compiler]\nflags = [\"-DVALUE=2\"]\n",
        )
        .unwrap();
        let err = build_project(&dir).unwrap_err();
        assert!(
            err.contains("Cannot clear") || err.contains("Cannot read"),
            "got: {err}"
        );
        fs::remove_file(&obj_dir).ok();
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn old_object_layout_is_cleared_once() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("obj-layout");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}\n").unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();
        build_project(&dir).unwrap();
        let obj = obj_path(&dir, "main");
        let scope = scope_of(&dir, &obj);
        fs::remove_file(scope.join(".obj-layout")).unwrap();
        let stale_obj = scope.join("obj/legacy.o");
        let stale_dep = scope.join("obj/dep/dep.o");
        let stale_archive = scope.join("lib/x/libx.a");
        fs::create_dir_all(stale_dep.parent().unwrap()).unwrap();
        fs::create_dir_all(stale_archive.parent().unwrap()).unwrap();
        fs::write(&stale_obj, "").unwrap();
        fs::write(&stale_dep, "").unwrap();
        fs::write(&stale_archive, "").unwrap();
        build_project(&dir).unwrap();
        assert!(!stale_obj.exists(), "the old object layout must be cleared as a whole batch");
        assert!(!stale_dep.exists(), "stale dependency objects must be cleaned up");
        assert!(!stale_archive.exists(), "stale library archives must be cleaned up");
        assert!(obj.exists(), "objects in the new layout should be rebuilt");

        let obj_t = mtime(&obj);
        std::thread::sleep(std::time::Duration::from_millis(50));
        build_project(&dir).unwrap();
        assert_eq!(mtime(&obj), obj_t, "the layout migration should happen only once");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn compiler_version_change_invalidates_scope() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("compiler-version");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}\n").unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();
        let cfg = crate::config::load(&dir).unwrap();

        let mut cxx = crate::compiler::pick(None).unwrap();
        cxx.version = "test-v1".into();
        let p1 = plan(&dir, &cfg, cxx.clone(), false).unwrap();
        build(&dir, &cfg, &p1).unwrap();
        let obj = obj_path(&dir, "main");
        let obj_t = mtime(&obj);

        std::thread::sleep(std::time::Duration::from_millis(50));
        cxx.version = "test-v2".into();
        let p2 = plan(&dir, &cfg, cxx, false).unwrap();
        build(&dir, &cfg, &p2).unwrap();
        assert_ne!(mtime(&obj), obj_t, "a compiler version change must invalidate the whole scope");
        fs::remove_dir_all(dir).ok();
    }

    // ── v0.4: single bin + multiple static libraries ──────────────────────────

    fn target_toml(flags: Option<&str>) -> String {
        let compiler = flags
            .map(|f| format!("[compiler]\nflags = {f}\n\n"))
            .unwrap_or_default();
        format!(
            "[project]\nname = \"t\"\nedition = \"c++23\"\ncompile_commands = true\n\n\
             {compiler}[[target]]\nname = \"foo\"\nkind = \"static_lib\"\nsources = [\"lib\"]\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"foo\"]\n"
        )
    }

    fn target_project(tag: &str) -> PathBuf {
        let dir = temp_dir(tag);
        for sub in ["src", "lib", "include"] {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }
        fs::write(dir.join("include/foo.h"), "#pragma once\nint foo();\n").unwrap();
        fs::write(
            dir.join("lib/foo.cpp"),
            "#include \"foo.h\"\nint foo(){ return 1; }\n",
        )
        .unwrap();
        fs::write(
            dir.join("src/main.cpp"),
            "#include \"foo.h\"\nint main(){ return foo() == 1 ? 0 : 1; }\n",
        )
        .unwrap();
        fs::write(dir.join("cizon.toml"), target_toml(None)).unwrap();
        dir
    }

    fn lib_archive_path(dir: &Path, lib: &str) -> PathBuf {
        for scope in fs::read_dir(dir.join("build")).unwrap().flatten() {
            let path = scope
                .path()
                .join("lib")
                .join(lib)
                .join(format!("lib{lib}.a"));
            if path.exists() {
                return path;
            }
        }
        panic!("did not find lib{lib}.a");
    }

    fn libs_supported() -> bool {
        have_compiler()
            && crate::compiler::pick(None)
                .ok()
                .is_some_and(|cxx| crate::compiler::archiver(&cxx).is_ok())
    }

    #[test]
    fn targets_build_static_lib_and_link() {
        if !libs_supported() {
            return;
        }
        let dir = target_project("lib-e2e");
        let exe = build_project(&dir).unwrap();
        assert_eq!(run_code(&exe), 0);
        assert!(lib_archive_path(&dir, "foo").is_file());
        let db = fs::read_to_string(dir.join("compile_commands.json")).unwrap();
        assert_eq!(db.matches("\"file\"").count(), 2, "bin+lib two sources: {db}");
        assert!(db.contains("foo.cpp"), "got: {db}");
        assert!(db.contains("main.cpp"), "got: {db}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn lib_source_change_rebuilds_archive_without_recompiling_bin() {
        if !libs_supported() {
            return;
        }
        let dir = target_project("lib-inc");
        let exe = build_project(&dir).unwrap();
        let archive = lib_archive_path(&dir, "foo");
        let main_obj = obj_path(&dir, "main");
        let (archive_t, main_t, exe_t) = (mtime(&archive), mtime(&main_obj), mtime(&exe));
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(
            dir.join("lib/foo.cpp"),
            "#include \"foo.h\"\nint foo(){ return 2; }\n",
        )
        .unwrap();
        let exe = build_project(&dir).unwrap();
        assert_ne!(mtime(&archive), archive_t, "a library source change must rebuild the archive");
        assert_eq!(mtime(&main_obj), main_t, "a library source change must not recompile bin sources");
        assert_ne!(mtime(&exe), exe_t, "an archive change must relink");
        // main still checks foo()==1, the new library returns 2 → exit code 1, proving the link used the new code
        assert_eq!(run_code(&exe), 1, "the exe must contain the new library code");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn bin_source_change_keeps_archive() {
        if !libs_supported() {
            return;
        }
        let dir = target_project("bin-inc");
        let exe = build_project(&dir).unwrap();
        let archive = lib_archive_path(&dir, "foo");
        let (archive_t, exe_t) = (mtime(&archive), mtime(&exe));
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(
            dir.join("src/main.cpp"),
            "#include \"foo.h\"\n// changed\nint main(){ return foo() == 1 ? 0 : 1; }\n",
        )
        .unwrap();
        let exe = build_project(&dir).unwrap();
        assert_eq!(mtime(&archive), archive_t, "changing only the bin must not rebuild the archive");
        assert_ne!(mtime(&exe), exe_t);
        fs::remove_dir_all(dir).ok();
    }

    fn rename_config(name: &str) -> String {
        format!(
            "[project]\nname = \"t\"\nedition = \"c++23\"\n\n\
             [[target]]\nname = {name:?}\nkind = \"bin\"\nsources = [\"src\"]\n"
        )
    }

    #[test]
    fn library_allows_same_stem_across_dirs() {
        if !libs_supported() {
            return;
        }
        let dir = temp_dir("lib-same-stem");
        fs::create_dir_all(dir.join("lib/a")).unwrap();
        fs::create_dir_all(dir.join("lib/b")).unwrap();
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("lib/a/util.cpp"), "int a(){ return 1; }\n").unwrap();
        fs::write(dir.join("lib/b/util.cpp"), "int b(){ return 2; }\n").unwrap();
        fs::write(
            dir.join("src/main.cpp"),
            "int a(); int b();\nint main(){ return a() + b() == 3 ? 0 : 1; }\n",
        )
        .unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n\
             [[target]]\nname = \"foo\"\nkind = \"static_lib\"\nsources = [\"lib\"]\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"foo\"]\n",
        )
        .unwrap();
        let exe = build_project(&dir).unwrap();
        assert_eq!(run_code(&exe), 0);
        assert!(lib_archive_path(&dir, "foo").is_file());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn renaming_bin_target_removes_old_exe() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("rename-bin");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}\n").unwrap();
        fs::write(dir.join("cizon.toml"), rename_config("old")).unwrap();
        let old_exe = build_project(&dir).unwrap();
        assert!(old_exe.is_file());
        let old_pdb = old_exe.with_extension("pdb");
        fs::write(&old_pdb, "").unwrap();

        fs::write(dir.join("cizon.toml"), rename_config("new")).unwrap();
        let new_exe = build_project(&dir).unwrap();
        assert!(!old_exe.exists(), "after renaming the bin, the old exe must be cleaned up");
        assert!(!old_pdb.exists(), "the old exe's .pdb should be cleaned up too");
        assert!(new_exe.is_file());
        assert_eq!(run_code(&new_exe), 0);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn case_only_bin_rename_keeps_new_exe() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("rename-case");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}\n").unwrap();
        fs::write(dir.join("cizon.toml"), rename_config("case")).unwrap();
        let _old_exe = build_project(&dir).unwrap();

        fs::write(dir.join("cizon.toml"), rename_config("CASE")).unwrap();
        let new_exe = build_project(&dir).unwrap();
        assert!(new_exe.is_file(), "a case-only rename must not delete the new exe");
        assert_eq!(run_code(&new_exe), 0);
        #[cfg(unix)]
        assert!(!_old_exe.exists(), "on Unix the old exe with a different case should be deleted");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn failed_build_after_rename_keeps_old_exe() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("rename-fail");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}\n").unwrap();
        fs::write(dir.join("cizon.toml"), rename_config("old")).unwrap();
        let old_exe = build_project(&dir).unwrap();

        fs::write(
            dir.join("src/main.cpp"),
            "int main(){ return does_not_exist; }\n",
        )
        .unwrap();
        fs::write(dir.join("cizon.toml"), rename_config("new")).unwrap();
        let err = build_project(&dir).unwrap_err();
        assert!(err.contains("Compile failed"), "got: {err}");
        assert!(old_exe.exists(), "on build failure, the last runnable artifact must not be deleted");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn project_flag_change_rebuilds_library() {
        if !libs_supported() {
            return;
        }
        let dir = target_project("lib-flags");
        build_project(&dir).unwrap();
        let archive = lib_archive_path(&dir, "foo");
        let lib_obj = find_named(&archive.parent().unwrap().join("obj"), "foo.o").unwrap();
        let main_obj = obj_path(&dir, "main");
        let (archive_t, obj_t, main_t) = (mtime(&archive), mtime(&lib_obj), mtime(&main_obj));
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(dir.join("cizon.toml"), target_toml(Some("[\"-Wall\"]"))).unwrap();
        build_project(&dir).unwrap();
        assert_ne!(mtime(&lib_obj), obj_t, "a project flags change should recompile library sources");
        assert_ne!(mtime(&main_obj), main_t, "a project flags change should recompile bin sources");
        assert_ne!(mtime(&archive), archive_t, "a library object change should rebuild the archive");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn target_sources_reject_empty_entry() {
        let dir = target_project("lib-empty-src");
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"\"]\n",
        )
        .unwrap();
        let cfg = crate::config::load(&dir).unwrap();
        let cxx = Cxx { name: "g++".into(), version: "test".into() };
        let err = plan(&dir, &cfg, cxx, false).unwrap_err();
        assert!(err.contains("empty path"), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn target_sources_must_stay_inside_project() {
        let dir = target_project("lib-escape");
        let absolute = dir.join("src").to_string_lossy().into_owned();
        for bad in ["../outside", ".", absolute.as_str()] {
            fs::write(
                dir.join("cizon.toml"),
                format!(
                    "[project]\nname = \"t\"\n\n[[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [{bad:?}]\n"
                ),
            )
            .unwrap();
            let cfg = crate::config::load(&dir).unwrap();
            let cxx = Cxx { name: "g++".into(), version: "test".into() };
            let err = plan(&dir, &cfg, cxx, false).unwrap_err();
            assert!(
                err.contains("project-relative path") || err.contains("project root"),
                "source={bad:?} got: {err}"
            );
        }
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn source_switch_with_same_object_path_recompiles() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("bin-switch");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){ return 0; }\n").unwrap();
        fs::write(dir.join("src/main.cc"), "int main(){ return 1; }\n").unwrap();
        let config = |sources: &str| {
            format!(
                "[project]\nname = \"t\"\n\n[[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [{sources:?}]\n"
            )
        };
        fs::write(dir.join("cizon.toml"), config("src/main.cpp")).unwrap();
        let exe = build_project(&dir).unwrap();
        assert_eq!(run_code(&exe), 0);

        // Same object path obj/src/main.o; main.cc is older than the old object and the old .d does
        // not list it either, so it must recompile
        fs::write(dir.join("cizon.toml"), config("src/main.cc")).unwrap();
        let exe = build_project(&dir).unwrap();
        assert_eq!(run_code(&exe), 1, "switching the source at the same object path must recompile via .d source verification");
        fs::remove_dir_all(dir).ok();
    }

    // ── v0.5: shared libraries ───────────────────────────────────────

    fn shared_project(tag: &str) -> PathBuf {
        let dir = temp_dir(tag);
        for sub in ["src", "lib", "capi", "include"] {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\nedition = \"c++23\"\n\n\
             [[target]]\nname = \"core\"\nkind = \"static_lib\"\nsources = [\"lib\"]\n\n\
             [[target]]\nname = \"core_dll\"\nkind = \"shared_lib\"\nsources = [\"capi\"]\nlinks = [\"core\"]\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"core_dll\"]\n",
        )
        .unwrap();
        fs::write(dir.join("include/core.h"), "#pragma once\nint core_add(int a, int b);\n").unwrap();
        fs::write(dir.join("lib/core.cpp"), "int core_add(int a, int b) { return a + b; }\n").unwrap();
        fs::write(
            dir.join("capi/api.cpp"),
            "#include \"core.h\"\nextern \"C\" __declspec(dllexport) int add(int a, int b) { return core_add(a, b); }\n",
        )
        .unwrap();
        fs::write(
            dir.join("src/main.cpp"),
            "extern \"C\" int add(int a, int b);\nint main() { return add(1, 2) == 3 ? 0 : 1; }\n",
        )
        .unwrap();
        dir
    }

    fn shared_dll(scope: &Path, name: &str) -> PathBuf {
        ["dll", "so", "dylib"]
            .iter()
            .map(|ext| scope.join(format!("{name}.{ext}")))
            .find(|p| p.exists())
            .unwrap_or_else(|| panic!("shared library artifact not found for {name}"))
    }

    fn shared_import_lib(scope: &Path, name: &str) -> PathBuf {
        let dir = scope.join(".shared").join(name);
        fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                p.is_file()
                    && (p.extension().is_some_and(|e| e == "a" || e == "lib"))
            })
            .unwrap_or_else(|| panic!("import library not found in {}", dir.display()))
    }

    fn have_git() -> bool {
        Command::new("git").arg("--version").output().is_ok()
    }

    fn make_git_dep(origin: &Path) {
        fs::create_dir_all(origin.join("include")).unwrap();
        fs::create_dir_all(origin.join("src")).unwrap();
        fs::write(origin.join("include/dep.h"), "#pragma once\nint dep_value();\n").unwrap();
        fs::write(origin.join("src/dep.cpp"), "int dep_value() { return 42; }\n").unwrap();
        let run = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .current_dir(origin)
                    .args(args)
                    .status()
                    .unwrap()
                    .success(),
                "git {args:?} failed"
            );
        };
        run(&["init"]);
        run(&["add", "-A"]);
        run(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-m",
            "init",
            "--no-gpg-sign",
        ]);
        run(&["tag", "v1"]);
    }

    #[test]
    fn shared_lib_builds_dll_import_lib_and_bin_links_it() {
        if !shared_supported() {
            return;
        }
        let dir = shared_project("shared-build");
        let exe = build_project(&dir).unwrap();
        let scope = scope_of(&dir, &exe);
        shared_dll(&scope, "core_dll");
        shared_import_lib(&scope, "core_dll");
        assert_eq!(run_code(&exe), 0);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn shared_lib_links_git_dependency_objects() {
        if !shared_supported() || !have_git() {
            return;
        }
        let dir = temp_dir("shared-dep");
        let origin = temp_dir("shared-dep-origin");
        make_git_dep(&origin);
        let url = format!("file:///{}", origin.display().to_string().replace('\\', "/"));
        for sub in ["src", "dll"] {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\nedition = \"c++23\"\n\n\
             [[target]]\nname = \"depdll\"\nkind = \"shared_lib\"\nsources = [\"dll\"]\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"depdll\"]\n",
        )
        .unwrap();
        fs::write(
            dir.join("dll/api.cpp"),
            "#include \"dep.h\"\nextern \"C\" __declspec(dllexport) int shared_value() { return dep_value(); }\n",
        )
        .unwrap();
        fs::write(
            dir.join("src/main.cpp"),
            "extern \"C\" int shared_value();\nint main() { return shared_value() == 42 ? 0 : 1; }\n",
        )
        .unwrap();
        crate::deps::add_in(
            &dir,
            &[url, "--tag".into(), "v1".into()],
        )
        .expect("local file:// dependency should be cloned");

        let exe = build_project(&dir).unwrap();
        let scope = scope_of(&dir, &exe);
        shared_dll(&scope, "depdll");
        assert_eq!(run_code(&exe), 0, "DLL must resolve dependency objects at link time");
        fs::remove_dir_all(dir).ok();
        fs::remove_dir_all(origin).ok();
    }

    #[test]
    fn shared_impl_change_does_not_relink_bin() {
        if !shared_supported() {
            return;
        }
        let dir = shared_project("shared-norelink");
        let exe = build_project(&dir).unwrap();
        let scope = scope_of(&dir, &exe);
        let dll = shared_dll(&scope, "core_dll");
        let import_lib = shared_import_lib(&scope, "core_dll");
        let app_t = mtime(&exe);
        let dll_t = mtime(&dll);
        let import_t = mtime(&import_lib);
        std::thread::sleep(std::time::Duration::from_millis(50));

        let src = dir.join("capi/api.cpp");
        let text = fs::read_to_string(&src).unwrap();
        fs::write(&src, format!("{text}\n// implementation-only change\n")).unwrap();
        let exe = build_project(&dir).unwrap();
        assert_eq!(mtime(&exe), app_t, "import lib unchanged: bin must not relink");
        assert_eq!(mtime(&import_lib), import_t, "import lib should be preserved");
        assert_ne!(mtime(&dll), dll_t, "shared implementation change must relink the DLL");
        assert_eq!(run_code(&exe), 0);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn shared_export_change_relinks_bin() {
        if !shared_supported() {
            return;
        }
        let dir = shared_project("shared-export");
        let exe = build_project(&dir).unwrap();
        let app_t = mtime(&exe);
        std::thread::sleep(std::time::Duration::from_millis(50));

        let src = dir.join("capi/api.cpp");
        let text = fs::read_to_string(&src).unwrap();
        fs::write(
            &src,
            format!("{text}\nextern \"C\" __declspec(dllexport) int sub(int a, int b) {{ return a - b; }}\n"),
        )
        .unwrap();
        let exe = build_project(&dir).unwrap();
        assert_ne!(mtime(&exe), app_t, "export change must relink the bin");
        assert_eq!(run_code(&exe), 0);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn missing_import_lib_is_regenerated() {
        if !shared_supported() {
            return;
        }
        let dir = shared_project("shared-missing-implib");
        let exe = build_project(&dir).unwrap();
        let scope = scope_of(&dir, &exe);
        let import_lib = shared_import_lib(&scope, "core_dll");
        let app_t = mtime(&exe);
        fs::remove_file(&import_lib).unwrap();
        let exe = build_project(&dir).unwrap();
        assert!(import_lib.exists(), "missing import lib must be regenerated");
        assert_eq!(mtime(&exe), app_t, "regenerated import lib with the same exports must not relink the bin");
        assert_eq!(run_code(&exe), 0);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn shared_dll_loads_from_python() {
        if !shared_supported() {
            return;
        }
        let Ok(out) = std::process::Command::new("python")
            .arg("--version")
            .output()
        else {
            return;
        };
        if !out.status.success() {
            return;
        }
        let dir = shared_project("shared-python");
        let exe = build_project(&dir).unwrap();
        let scope = scope_of(&dir, &exe);
        let dll = shared_dll(&scope, "core_dll");
        let path = dll.to_string_lossy().replace('\\', "/");
        let script = format!(
            "import ctypes; lib = ctypes.CDLL(r'{path}'); raise SystemExit(0 if lib.add(4, 5) == 9 else 1)"
        );
        let status = std::process::Command::new("python")
            .arg("-c")
            .arg(script)
            .status()
            .unwrap();
        assert!(status.success(), "Python ctypes must load and call the DLL");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn bin_links_multiple_shared_libs() {
        if !shared_supported() {
            return;
        }
        let dir = temp_dir("shared-multi");
        for sub in ["src", "one", "two"] {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\nedition = \"c++23\"\n\n\
             [[target]]\nname = \"one\"\nkind = \"shared_lib\"\nsources = [\"one\"]\n\n\
             [[target]]\nname = \"two\"\nkind = \"shared_lib\"\nsources = [\"two\"]\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"one\", \"two\"]\n",
        )
        .unwrap();
        fs::write(
            dir.join("one/one.cpp"),
            "extern \"C\" __declspec(dllexport) int one() { return 1; }\n",
        )
        .unwrap();
        fs::write(
            dir.join("two/two.cpp"),
            "extern \"C\" __declspec(dllexport) int two() { return 2; }\n",
        )
        .unwrap();
        fs::write(
            dir.join("src/main.cpp"),
            "extern \"C\" int one();\nextern \"C\" int two();\nint main() { return one() + two() == 3 ? 0 : 1; }\n",
        )
        .unwrap();
        let exe = build_project(&dir).unwrap();
        let scope = scope_of(&dir, &exe);
        shared_dll(&scope, "one");
        shared_dll(&scope, "two");
        assert_eq!(run_code(&exe), 0);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn removing_shared_target_cleans_artifacts() {
        if !shared_supported() {
            return;
        }
        let dir = shared_project("shared-cleanup");
        let exe = build_project(&dir).unwrap();
        let scope = scope_of(&dir, &exe);
        let dll = shared_dll(&scope, "core_dll");
        assert!(scope.join(".shared").exists());

        fs::write(dir.join("src/main.cpp"), "int main() { return 0; }\n").unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\nedition = \"c++23\"\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\n",
        )
        .unwrap();
        let exe = build_project(&dir).unwrap();
        assert!(!dll.exists(), "removed shared target must delete its DLL");
        assert!(!scope.join(".shared").exists(), "removed shared target must delete .shared");
        assert_eq!(run_code(&exe), 0);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn dll_pdb_survives_no_op_build() {
        if !shared_supported() {
            return;
        }
        let dir = shared_project("shared-pdb");
        let exe = build_project(&dir).unwrap();
        let scope = scope_of(&dir, &exe);
        let pdb = scope.join("core_dll.pdb");
        if !pdb.exists() {
            fs::remove_dir_all(dir).ok();
            return;
        }
        build_project(&dir).unwrap();
        assert!(pdb.exists(), "no-op build must not delete the DLL PDB");
        fs::remove_dir_all(dir).ok();
    }

    // ── v0.6: multiple bin targets ───────────────────────────────────

    fn multi_bin_project(tag: &str) -> PathBuf {
        let dir = temp_dir(tag);
        for sub in ["one", "two"] {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\nedition = \"c++23\"\n\n\
             [[target]]\nname = \"one\"\nkind = \"bin\"\nsources = [\"one\"]\n\n\
             [[target]]\nname = \"two\"\nkind = \"bin\"\nsources = [\"two\"]\n",
        )
        .unwrap();
        fs::write(dir.join("one/main.cpp"), "int main() { return 1; }\n").unwrap();
        fs::write(dir.join("two/main.cpp"), "int main() { return 2; }\n").unwrap();
        dir
    }

    fn build_bins_project(dir: &Path) -> BuildResult {
        let cfg = crate::config::load(dir).unwrap();
        let cxx = crate::compiler::pick(None).unwrap();
        let plan = plan(dir, &cfg, cxx, false).unwrap();
        build_bins(dir, &cfg, &plan).unwrap()
    }

    #[test]
    fn multiple_bins_build_and_run_selected_bin() {
        if !have_compiler() {
            return;
        }
        let dir = multi_bin_project("multi-bin");
        let result = build_bins_project(&dir);
        assert_eq!(result.bins.len(), 2);
        let one = result.bins.iter().find(|(n, _)| n == "one").unwrap();
        let two = result.bins.iter().find(|(n, _)| n == "two").unwrap();
        assert_eq!(run_code(&one.1), 1);
        assert_eq!(run_code(&two.1), 2);

        let cfg = crate::config::load(&dir).unwrap();
        assert_eq!(
            crate::run::run(&dir, &cfg, false, None, Some("two"), &[]).unwrap(),
            2
        );
        assert!(crate::run::run(&dir, &cfg, false, None, None, &[])
            .unwrap_err()
            .contains("multiple bin targets"));
        assert!(crate::run::run(&dir, &cfg, false, None, Some("missing"), &[])
            .unwrap_err()
            .contains("unknown bin target"));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn editing_one_bin_does_not_relink_the_other() {
        if !have_compiler() {
            return;
        }
        let dir = multi_bin_project("multi-bin-incremental");
        let result = build_bins_project(&dir);
        let one = result.bins.iter().find(|(n, _)| n == "one").unwrap().1.clone();
        let two = result.bins.iter().find(|(n, _)| n == "two").unwrap().1.clone();
        let one_t = mtime(&one);
        let two_t = mtime(&two);
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(dir.join("one/main.cpp"), "int main() { return 3; }\n").unwrap();
        let result = build_bins_project(&dir);
        let one_new = result.bins.iter().find(|(n, _)| n == "one").unwrap().1.clone();
        let two_new = result.bins.iter().find(|(n, _)| n == "two").unwrap().1.clone();
        assert_ne!(mtime(&one_new), one_t, "changed bin must relink");
        assert_eq!(mtime(&two_new), two_t, "untouched bin must not relink");
        assert_eq!(run_code(&one_new), 3);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn removing_bin_cleans_old_exe() {
        if !have_compiler() {
            return;
        }
        let dir = multi_bin_project("multi-bin-cleanup");
        let result = build_bins_project(&dir);
        let scope = scope_of(&dir, &result.bins[0].1);
        assert!(scope.join("two.exe").exists());
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\nedition = \"c++23\"\n\n\
             [[target]]\nname = \"one\"\nkind = \"bin\"\nsources = [\"one\"]\n",
        )
        .unwrap();
        let _ = build_bins_project(&dir);
        assert!(scope.join("one.exe").exists());
        assert!(!scope.join("two.exe").exists(), "removed bin exe must be cleaned");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn multiple_bins_link_shared_lib() {
        if !shared_supported() {
            return;
        }
        let dir = temp_dir("multi-bin-shared");
        for sub in ["a", "b", "dll"] {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\nedition = \"c++23\"\n\n\
             [[target]]\nname = \"depdll\"\nkind = \"shared_lib\"\nsources = [\"dll\"]\n\n\
             [[target]]\nname = \"a\"\nkind = \"bin\"\nsources = [\"a\"]\nlinks = [\"depdll\"]\n\n\
             [[target]]\nname = \"b\"\nkind = \"bin\"\nsources = [\"b\"]\nlinks = [\"depdll\"]\n",
        )
        .unwrap();
        fs::write(
            dir.join("dll/api.cpp"),
            "extern \"C\" __declspec(dllexport) int shared_value() { return 42; }\n",
        )
        .unwrap();
        let main = "extern \"C\" int shared_value();\nint main() { return shared_value() == 42 ? 0 : 1; }\n";
        fs::write(dir.join("a/main.cpp"), main).unwrap();
        fs::write(dir.join("b/main.cpp"), main).unwrap();
        let result = build_bins_project(&dir);
        let scope = scope_of(&dir, &result.bins[0].1);
        shared_dll(&scope, "depdll");
        for (_, exe) in &result.bins {
            assert_eq!(run_code(exe), 0);
        }
        fs::remove_dir_all(dir).ok();
    }
