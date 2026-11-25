    use super::*;
    use super::output::*;
    use super::targets::*;
    use std::collections::HashSet;
    use std::fs;
    use std::process::Command;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cizon-bl-{tag}-{}",
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
        let cfg = crate::config::load_resolved(&dir).unwrap();
        let cxx = Cxx { name: "g++".into(), version: "test".into() };
        let p = plan(&dir, &cfg, cxx, false).unwrap();
        assert_eq!(p.flags.iter().filter(|f| *f == "-Wall").count(), 1);
        assert!(!p.flags.iter().any(|f| f == "-lm"));
        assert_eq!(p.link_flags, vec!["-lm".to_string()]);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unused_src_sources_ignore_files_compiled_by_targets() {
        let dir = temp_dir("unused-src");
        fs::create_dir_all(dir.join("src/sub")).unwrap();
        fs::write(dir.join("src/used.cpp"), "").unwrap();
        fs::write(dir.join("src/main.cpp"), "").unwrap();
        fs::write(dir.join("src/sub/helper.cc"), "").unwrap();
        fs::write(dir.join("src/readme.txt"), "").unwrap();
        let used: HashSet<PathBuf> = [PathBuf::from("src/used.cpp")].into_iter().collect();
        let unused = unused_src_sources(&dir.join("src"), &dir, &used);
        assert_eq!(
            unused,
            vec![dir.join("src/main.cpp"), dir.join("src/sub/helper.cc")]
        );

        let empty = unused_src_sources(&dir.join("missing").join("src"), &dir, &HashSet::new());
        assert!(empty.is_empty());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn compiler_path_is_sanitized_for_scope_dir() {
        assert_eq!(scope_label("g++"), "g++");
        assert_eq!(scope_label(r"C:\mingw64\bin\g++.exe"), "C__mingw64_bin_g++.exe");
        assert_eq!(scope_label("/usr/bin/clang++"), "_usr_bin_clang++");
    }

    #[test]
    fn library_only_project_builds_its_archive() {
        if crate::compiler::pick(None).is_err() {
            return;
        }
        let dir = temp_dir("lib-only-build");
        fs::create_dir_all(dir.join("lib")).unwrap();
        fs::write(dir.join("lib/core.cpp"), "int core(){return 0;}").unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n\
             [[target]]\nname = \"core\"\nkind = \"static_lib\"\nsources = [\"lib/core.cpp\"]\n",
        )
        .unwrap();
        let cfg = crate::config::load_resolved(&dir).unwrap();
        let cxx = crate::compiler::pick(None).unwrap();
        let p = plan(&dir, &cfg, cxx, false).unwrap();
        let built = build_bins(&dir, &cfg, &p).unwrap();
        assert!(built.execs.is_empty(), "a library-only build produces no bin");
        let scope = dir
            .join("build")
            .join(format!("{}-{}", p.mode, scope_label(&p.cxx.name)));
        let archive = scope.join("lib").join("core").join("libcore.a");
        assert!(archive.is_file(), "missing {}", archive.display());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn release_defines_ndebug_for_project_and_deps_but_not_tests() {
        let dir = temp_dir("release-ndebug");
        fs::create_dir_all(dir.join("tests")).unwrap();
        fs::write(dir.join("tests/main.cpp"), "int main(){return 0;}").unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n\
             [[target]]\nname = \"t_test\"\nkind = \"test\"\nsources = [\"tests\"]\n",
        )
        .unwrap();
        let cfg = crate::config::load_resolved(&dir).unwrap();
        let mk = || Cxx {
            name: "g++".into(),
            version: "test".into(),
        };
        let debug = plan(&dir, &cfg, mk(), false).unwrap();
        assert!(!debug.flags.iter().any(|f| f == "-DNDEBUG"));
        assert!(!debug.dep_base.iter().any(|f| f == "-DNDEBUG"));

        let release = plan(&dir, &cfg, mk(), true).unwrap();
        assert!(release.flags.iter().any(|f| f == "-DNDEBUG"));
        assert!(release.dep_base.iter().any(|f| f == "-DNDEBUG"));
        assert!(
            !release.tests[0]
                .compile_flags
                .iter()
                .any(|f| f.contains("NDEBUG")),
            "test sources must never see -DNDEBUG: {:?}",
            release.tests[0].compile_flags
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn finished_elapsed_keeps_unit_next_to_number() {
        assert_eq!(elapsed_text(0.12, false), "0.12s");
        let colored = elapsed_text(0.12, true);
        assert!(colored.starts_with("\x1b[2m0.12s"), "got: {colored:?}");
        assert!(!colored.contains("\x1b[0ms"), "reset must follow the unit: {colored:?}");
    }

    #[test]
    fn warning_section_has_header_and_blank_separation() {
        let dir = Path::new("proj");
        let cxx = Cxx { name: "g++".into(), version: "g++ 14.2.0".into() };
        let warnings = vec![(
            dir.join("src").join("main.cpp"),
            "main.cpp:54: warning: ignored\n".to_string(),
        )];
        let text = warning_section_text(dir, &cxx, &warnings);
        assert!(text.starts_with('\n'), "got: {text:?}");
        assert!(!text.contains('─'), "decorative divider must be gone: {text:?}");
        assert!(!text.contains("\x1b[38;2;255;241;168m"));
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
            tests: vec![],
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
        let cfg = crate::config::load_resolved(&dir).unwrap();
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
        let cfg = crate::config::load_resolved(&dir).unwrap();
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
        have_compiler() && (cfg!(windows) || cfg!(target_os = "linux"))
    }

    fn build_project(dir: &Path) -> Result<PathBuf, String> {
        let cfg = crate::config::load_resolved(dir)?;
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
        for attempt in 0..5 {
            match Command::new(exe).status() {
                Ok(status) => return status.code().unwrap_or(-1),
                // WSL can briefly report ETXTBSY after copying a fresh executable.
                Err(e)
                    if e.kind() == std::io::ErrorKind::ExecutableFileBusy && attempt < 4 =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(e) => panic!("cannot run {}: {e}", exe.display()),
            }
        }
        unreachable!("the final attempt either runs or returns an error")
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
        let cfg = crate::config::load_resolved(&dir).unwrap();
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
        let cfg = crate::config::load_resolved(&dir).unwrap();
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
        let cfg = crate::config::load_resolved(&dir).unwrap();
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
        let cfg = crate::config::load_resolved(&dir).unwrap();

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
        let cfg = crate::config::load_resolved(&dir).unwrap();
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
            let cfg = crate::config::load_resolved(&dir).unwrap();
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
            // __declspec(dllexport) is Windows-only (g++/clang++ on Linux reject it); ELF
            // exports every symbol by default, so the marker is empty there.
            "#if defined(_WIN32)\n#define CZ_EXPORT __declspec(dllexport)\n#else\n#define CZ_EXPORT\n#endif\n#include \"core.h\"\nextern \"C\" CZ_EXPORT int add(int a, int b) { return core_add(a, b); }\n",
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
            .flat_map(|ext| {
                [
                    scope.join(format!("{name}.{ext}")),
                    scope.join(format!("lib{name}.{ext}")),
                ]
            })
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
        let so = shared_dll(&scope, "core_dll");
        #[cfg(not(target_os = "linux"))]
        let _ = so;
        // Linux has no import library: the .so itself is the link artifact.
        if cfg!(windows) {
            shared_import_lib(&scope, "core_dll");
        }
        #[cfg(target_os = "linux")]
        {
            if let Ok(out) = Command::new("readelf")
                .env("LC_ALL", "C")
                .arg("-d")
                .arg(&so)
                .output()
            {
                assert!(out.status.success(), "readelf failed on {}", so.display());
                let text = String::from_utf8_lossy(&out.stdout);
                assert!(
                    text.contains("SONAME") && text.contains("libcore_dll.so"),
                    "missing unversioned SONAME in {}:\n{text}",
                    so.display()
                );
            }
        }
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
            "#if defined(_WIN32)\n#define CZ_EXPORT __declspec(dllexport)\n#else\n#define CZ_EXPORT\n#endif\n#include \"dep.h\"\nextern \"C\" CZ_EXPORT int shared_value() { return dep_value(); }\n",
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
        // Windows-only: the import-library fingerprint covers the export surface only. On Linux
        // the .so fingerprint is content-based, so an impl-only change correctly relinks the bin.
        if !cfg!(windows) || !shared_supported() {
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
            format!("{text}\nextern \"C\" CZ_EXPORT int sub(int a, int b) {{ return a - b; }}\n"),
        )
        .unwrap();
        let exe = build_project(&dir).unwrap();
        assert_ne!(mtime(&exe), app_t, "export change must relink the bin");
        assert_eq!(run_code(&exe), 0);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn missing_import_lib_is_regenerated() {
        // Windows-only: Linux has no import library to delete and regenerate.
        if !cfg!(windows) || !shared_supported() {
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
            "#if defined(_WIN32)\n#define CZ_EXPORT __declspec(dllexport)\n#else\n#define CZ_EXPORT\n#endif\nextern \"C\" CZ_EXPORT int one() { return 1; }\n",
        )
        .unwrap();
        fs::write(
            dir.join("two/two.cpp"),
            "#if defined(_WIN32)\n#define CZ_EXPORT __declspec(dllexport)\n#else\n#define CZ_EXPORT\n#endif\nextern \"C\" CZ_EXPORT int two() { return 2; }\n",
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
        let cfg = crate::config::load_resolved(dir).unwrap();
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
        assert_eq!(result.execs.len(), 2);
        let one = result.execs.iter().find(|(n, _)| n == "one").unwrap();
        let two = result.execs.iter().find(|(n, _)| n == "two").unwrap();
        assert_eq!(run_code(&one.1), 1);
        assert_eq!(run_code(&two.1), 2);

        let cfg = crate::config::load_resolved(&dir).unwrap();
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
        let one = result.execs.iter().find(|(n, _)| n == "one").unwrap().1.clone();
        let two = result.execs.iter().find(|(n, _)| n == "two").unwrap().1.clone();
        let one_t = mtime(&one);
        let two_t = mtime(&two);
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(dir.join("one/main.cpp"), "int main() { return 3; }\n").unwrap();
        let result = build_bins_project(&dir);
        let one_new = result.execs.iter().find(|(n, _)| n == "one").unwrap().1.clone();
        let two_new = result.execs.iter().find(|(n, _)| n == "two").unwrap().1.clone();
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
        // Use the real artifact paths from the build result: the executable suffix is
        // host-dependent (.exe on Windows, extensionless on Linux).
        let one = result.execs.iter().find(|(n, _)| n == "one").unwrap().1.clone();
        let two = result.execs.iter().find(|(n, _)| n == "two").unwrap().1.clone();
        assert!(two.exists());
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\nedition = \"c++23\"\n\n\
             [[target]]\nname = \"one\"\nkind = \"bin\"\nsources = [\"one\"]\n",
        )
        .unwrap();
        let _ = build_bins_project(&dir);
        assert!(one.exists());
        assert!(!two.exists(), "removed bin exe must be cleaned");
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
            "#if defined(_WIN32)\n#define CZ_EXPORT __declspec(dllexport)\n#else\n#define CZ_EXPORT\n#endif\nextern \"C\" CZ_EXPORT int shared_value() { return 42; }\n",
        )
        .unwrap();
        let main = "extern \"C\" int shared_value();\nint main() { return shared_value() == 42 ? 0 : 1; }\n";
        fs::write(dir.join("a/main.cpp"), main).unwrap();
        fs::write(dir.join("b/main.cpp"), main).unwrap();
        let result = build_bins_project(&dir);
        let scope = scope_of(&dir, &result.execs[0].1);
        shared_dll(&scope, "depdll");
        for (_, exe) in &result.execs {
            assert_eq!(run_code(exe), 0);
        }
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn missing_extern_lib_hint_names_the_library_and_install_path() {
        let stderr = "C:/msys64/ucrt64/bin/../lib/gcc/x86_64-w64-mingw32/14.2.0/../../../../x86_64-w64-mingw32/bin/ld.exe: cannot find -lgtest: No such file or directory";
        let links = vec!["gtest".to_string(), "gtest_main".to_string()];
        let hint = super::missing_extern_lib_hint(stderr, &links).unwrap();
        assert!(hint.contains("-lgtest"), "got: {hint}");
        assert!(hint.contains("system package manager"), "got: {hint}");
        assert!(super::missing_extern_lib_hint("undefined reference to `foo'", &links).is_none());
        assert!(super::missing_extern_lib_hint(stderr, &["other".to_string()]).is_none());
        // Some MinGW linkers print only the collect2 summary; the guidance must survive.
        let hint =
            super::missing_extern_lib_hint("collect2.exe: error: ld returned 5 exit status", &links)
                .unwrap();
        assert!(hint.contains("gtest, gtest_main"), "got: {hint}");
    }

    #[test]
    fn extern_links_test_target_builds_and_runs() {
        if !have_compiler() {
            return;
        }
        // -lm ships with every g++/clang++ toolchain, so this exercises the extern_links path
        // without requiring a pre-installed test framework.
        let dir = temp_dir("system-link-e2e");
        fs::create_dir_all(dir.join("tests")).unwrap();
        fs::write(
            dir.join("tests/math_test.cpp"),
            "#include <cmath>\nint main(){ return std::pow(2.0, 10.0) == 1024.0 ? 0 : 1; }\n",
        )
        .unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n\
             [[target]]\nname = \"t_test\"\nkind = \"test\"\nsources = [\"tests\"]\nextern_links = [\"m\"]\n",
        )
        .unwrap();
        let cfg = crate::config::load_resolved(&dir).unwrap();
        let cxx = crate::compiler::pick(None).unwrap();
        let plan = plan(&dir, &cfg, cxx, false).unwrap();
        let result = super::build_tests(&dir, &cfg, &plan, &[]).unwrap();
        assert_eq!(result.execs.len(), 1);
        assert_eq!(run_code(&result.execs[0].1), 0, "extern_links -lm must link and run");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn extern_links_change_forces_relink() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("system-relink");
        fs::create_dir_all(dir.join("tests")).unwrap();
        fs::write(dir.join("tests/main.cpp"), "int main(){return 0;}").unwrap();
        let toml_path = dir.join("cizon.toml");
        fs::write(
            &toml_path,
            "[project]\nname = \"t\"\n\n\
             [[target]]\nname = \"t_test\"\nkind = \"test\"\nsources = [\"tests\"]\nextern_links = [\"m\"]\n",
        )
        .unwrap();
        let cfg = crate::config::load_resolved(&dir).unwrap();
        let cxx = crate::compiler::pick(None).unwrap();
        let first_plan = plan(&dir, &cfg, cxx.clone(), false).unwrap();
        let result = super::build_tests(&dir, &cfg, &first_plan, &[]).unwrap();
        let exe = result.execs[0].1.clone();
        let exe_t = mtime(&exe);
        std::thread::sleep(std::time::Duration::from_millis(50));
        // A changed external library set must change the link fingerprint and force a relink.
        fs::write(
            &toml_path,
            "[project]\nname = \"t\"\n\n\
             [[target]]\nname = \"t_test\"\nkind = \"test\"\nsources = [\"tests\"]\nextern_links = [\"m\", \"stdc++\"]\n",
        )
        .unwrap();
        let cfg = crate::config::load_resolved(&dir).unwrap();
        let second_plan = plan(&dir, &cfg, cxx, false).unwrap();
        super::build_tests(&dir, &cfg, &second_plan, &[]).unwrap();
        assert_ne!(mtime(&exe), exe_t, "a extern_links change must relink");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn test_targets_never_compile_with_ndebug() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("test-ndebug");
        fs::create_dir_all(dir.join("tests")).unwrap();
        fs::write(
            dir.join("tests/assert_test.cpp"),
            "#include <cassert>\nint main(){ assert(false); return 0; }\n",
        )
        .unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n\
             [compiler]\nflags = [\"-DNDEBUG\"]\n\n\
             [[target]]\nname = \"t_test\"\nkind = \"test\"\nsources = [\"tests\"]\n",
        )
        .unwrap();
        let cfg = crate::config::load_resolved(&dir).unwrap();
        let cxx = crate::compiler::pick(None).unwrap();
        let p = plan(&dir, &cfg, cxx, false).unwrap();
        assert!(
            p.flags.iter().any(|f| f == "-DNDEBUG"),
            "user flags must still reach normal targets"
        );
        assert!(
            !p.tests[0]
                .compile_flags
                .iter()
                .any(|f| f.contains("NDEBUG")),
            "test sources must never see -DNDEBUG: {:?}",
            p.tests[0].compile_flags
        );
        let result = super::build_tests(&dir, &cfg, &p, &[]).unwrap();
        assert_ne!(
            run_code(&result.execs[0].1),
            0,
            "assert(false) must stay live in a test target"
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn strict_switch_expands_warning_flags_for_project_only() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("strict");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}").unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n[compiler]\nstrict = true\nwerror = true\n",
        )
        .unwrap();
        let cfg = crate::config::load_resolved(&dir).unwrap();
        let cxx = crate::compiler::pick(None).unwrap();
        let p = plan(&dir, &cfg, cxx, false).unwrap();
        for f in ["-Wall", "-Wextra", "-Wpedantic", "-Werror"] {
            assert!(p.flags.iter().any(|x| x == f), "missing {f}: {:?}", p.flags);
        }
        assert!(
            !p.dep_base.iter().any(|x| x.starts_with("-W")),
            "dependency sources must not inherit strict/werror: {:?}",
            p.dep_base
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn target_defines_propagate_through_static_and_shared_closures() {
        let dir = temp_dir("target-defines");
        for path in [
            "libs/core/src",
            "libs/wrapper/src",
            "libs/api/src",
            "app/src",
            "other/src",
            "tests",
        ] {
            write_sub(&dir, &format!("{path}/unit.cpp"), "// unit\n");
        }
        write_sub(
            &dir,
            "cizon.toml",
            "[project]\nname = \"t\"\n\n[compiler]\nflags = [\"-DCORE=99\"]\n\n\
             [[target]]\nname = \"core\"\nkind = \"static_lib\"\nsources = [\"libs/core/src\"]\n\
             defines = [\"CORE=1\", \"CORE\", \"NDEBUG\"]\n\n\
             [[target]]\nname = \"wrapper\"\nkind = \"static_lib\"\nsources = [\"libs/wrapper/src\"]\n\
             links = [\"core\"]\ndefines = [\"WRAPPER=1\", \"CORE=1\"]\n\n\
             [[target]]\nname = \"api\"\nkind = \"shared_lib\"\nsources = [\"libs/api/src\"]\n\
             links = [\"wrapper\"]\ndefines = [\"API=1\"]\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"app/src\"]\n\
             links = [\"api\"]\ndefines = [\"APP=1\"]\n\n\
             [[target]]\nname = \"other\"\nkind = \"bin\"\nsources = [\"other/src\"]\n\
             defines = [\"OTHER=1\"]\n\n\
             [[target]]\nname = \"t_test\"\nkind = \"test\"\nsources = [\"tests\"]\n\
             links = [\"api\"]\ndefines = [\"TEST=1\", \"NDEBUG\"]\n",
        );
        let cfg = crate::config::load_resolved(&dir).unwrap();
        let cxx = Cxx { name: "g++".into(), version: "test".into() };
        let p = plan(&dir, &cfg, cxx, false).unwrap();
        let has = |flags: &[String], define: &str| flags.iter().any(|f| *f == format!("-D{define}"));
        let core = p.libs.iter().find(|l| l.name == "core").unwrap();
        let wrapper = p.libs.iter().find(|l| l.name == "wrapper").unwrap();
        let api = p.shared.iter().find(|s| s.name == "api").unwrap();
        let app = p.bins.iter().find(|b| b.name == "app").unwrap();
        let other = p.bins.iter().find(|b| b.name == "other").unwrap();
        assert!(has(&core.compile_flags, "CORE=1"));
        assert!(has(&wrapper.compile_flags, "CORE=1"));
        assert!(has(&wrapper.compile_flags, "WRAPPER=1"));
        for define in ["CORE=1", "WRAPPER=1", "API=1"] {
            assert!(has(&api.compile_flags, define), "missing {define}: {:?}", api.compile_flags);
        }
        for define in ["CORE=1", "WRAPPER=1", "API=1", "APP=1"] {
            assert!(has(&app.compile_flags, define), "missing {define}: {:?}", app.compile_flags);
        }
        let global = app.compile_flags.iter().position(|f| f == "-DCORE=99").unwrap();
        let local = app.compile_flags.iter().position(|f| f == "-DCORE=1").unwrap();
        assert!(global < local, "target definitions must follow global flags: {:?}", app.compile_flags);
        assert_eq!(
            app.compile_flags.iter().filter(|f| *f == "-DCORE=1").count(),
            1,
            "identical public definitions must deduplicate: {:?}",
            app.compile_flags
        );
        assert!(has(&other.compile_flags, "OTHER=1"));
        assert!(!has(&other.compile_flags, "CORE=1"));
        for define in ["CORE=1", "WRAPPER=1", "API=1", "TEST=1"] {
            assert!(has(&p.tests[0].compile_flags, define), "missing {define}: {:?}", p.tests[0].compile_flags);
        }
        assert!(
            !p.tests[0].compile_flags.iter().any(|f| f.contains("NDEBUG")),
            "test sources must strip propagated NDEBUG: {:?}",
            p.tests[0].compile_flags
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn link_state_tracks_effective_compile_definitions() {
        let dir = temp_dir("define-link-state");
        write_sub(&dir, "src/main.cpp", "int main(){return 0;}\n");
        write_sub(&dir, "api/unit.cpp", "// unit\n");
        let config = |value: u32| {
            format!(
                "[project]\nname = \"t\"\n\n\
                 [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\n\
                 defines = [\"VALUE={value}\"]\n\n\
                 [[target]]\nname = \"api\"\nkind = \"shared_lib\"\nsources = [\"api\"]\n\
                 defines = [\"VALUE={value}\"]\n"
            )
        };
        let cxx = Cxx { name: "g++".into(), version: "test".into() };
        write_sub(&dir, "cizon.toml", &config(1));
        let cfg = crate::config::load_resolved(&dir).unwrap();
        let first = plan(&dir, &cfg, cxx.clone(), false).unwrap();
        let bin_state = super::state::link_state_text(&first, &first.bins[0], &[], &[], &[]);
        let shared_state =
            super::state::shared_link_state_text(&first, &first.shared[0], &[], &[]);
        assert!(bin_state.contains("-DVALUE=1"), "got: {bin_state}");
        assert!(shared_state.contains("-DVALUE=1"), "got: {shared_state}");

        write_sub(&dir, "cizon.toml", &config(2));
        let cfg = crate::config::load_resolved(&dir).unwrap();
        let second = plan(&dir, &cfg, cxx, false).unwrap();
        let next_bin = super::state::link_state_text(&second, &second.bins[0], &[], &[], &[]);
        let next_shared =
            super::state::shared_link_state_text(&second, &second.shared[0], &[], &[]);
        assert_ne!(bin_state, next_bin, "definition changes must force an executable relink");
        assert_ne!(shared_state, next_shared, "definition changes must force a shared relink");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn target_defines_conflict_in_one_link_closure_is_rejected() {
        let dir = temp_dir("target-defines-conflict");
        for path in ["a/src", "b/src", "app/src"] {
            write_sub(&dir, &format!("{path}/unit.cpp"), "// unit\n");
        }
        write_sub(
            &dir,
            "cizon.toml",
            "[project]\nname = \"t\"\n\n\
             [[target]]\nname = \"a\"\nkind = \"static_lib\"\nsources = [\"a/src\"]\n\
             defines = [\"FEATURE=1\"]\n\n\
             [[target]]\nname = \"b\"\nkind = \"static_lib\"\nsources = [\"b/src\"]\n\
             defines = [\"FEATURE=2\"]\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"app/src\"]\nlinks = [\"a\", \"b\"]\n",
        );
        let cfg = crate::config::load_resolved(&dir).unwrap();
        let cxx = Cxx { name: "g++".into(), version: "test".into() };
        let err = plan(&dir, &cfg, cxx, false).unwrap_err();
        assert!(err.contains("conflicting definition"), "got: {err}");
        assert!(err.contains("FEATURE"), "got: {err}");
        assert!(err.contains("\"a\"") && err.contains("\"b\""), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn implicit_project_defines_apply_and_cli_wins() {
        let dir = temp_dir("project-defines");
        write_sub(&dir, "src/main.cpp", "int main(){return VALUE;}\n");
        write_sub(
            &dir,
            "cizon.toml",
            "[project]\nname = \"t\"\ndefines = [\"VALUE=1\"]\n\n[compiler]\nflags = [\"-DVALUE=0\"]\n",
        );
        let cfg = crate::config::load_resolved(&dir).unwrap();
        let cxx = Cxx { name: "g++".into(), version: "test".into() };
        let p = plan_with_defines(&dir, &cfg, cxx, false, &["VALUE=2".to_string()]).unwrap();
        let flags = &p.bins[0].compile_flags;
        let global = flags.iter().position(|f| f == "-DVALUE=0").unwrap();
        let project = flags.iter().position(|f| f == "-DVALUE=1").unwrap();
        let cli = flags.iter().position(|f| f == "-DVALUE=2").unwrap();
        assert!(global < project && project < cli, "got: {flags:?}");
        assert!(!p.flags.iter().any(|f| f == "-DVALUE=1" || f == "-DVALUE=2"));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn cli_defines_override_targets_without_touching_dependencies() {
        let dir = temp_dir("cli-defines");
        write_sub(&dir, "libs/core/src/unit.cpp", "// unit\n");
        write_sub(&dir, "app/src/main.cpp", "int main(){return 0;}\n");
        write_sub(&dir, "tests/main.cpp", "int main(){return 0;}\n");
        write_sub(
            &dir,
            "cizon.toml",
            "[project]\nname = \"t\"\n\n\
             [dependencies]\nfmt = { git = \"https://example.com/fmt\" }\n\n\
             [[target]]\nname = \"core\"\nkind = \"static_lib\"\nsources = [\"libs/core/src\"]\n\
             defines = [\"VALUE=1\", \"CORE=1\"]\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"app/src\"]\nlinks = [\"core\"]\n\n\
             [[target]]\nname = \"t_test\"\nkind = \"test\"\nsources = [\"tests\"]\nlinks = [\"core\"]\n",
        );
        let cfg = crate::config::load_resolved(&dir).unwrap();
        let cxx = Cxx { name: "g++".into(), version: "test".into() };
        let cli = ["VALUE=2".to_string(), "CLI=1".to_string(), "NDEBUG".to_string()];
        let p = plan_with_defines(&dir, &cfg, cxx, false, &cli).unwrap();
        let has = |flags: &[String], define: &str| flags.iter().any(|f| *f == format!("-D{define}"));
        for target in p
            .libs
            .iter()
            .map(|l| &l.compile_flags)
            .chain(std::iter::once(&p.bins[0].compile_flags))
            .chain(std::iter::once(&p.tests[0].compile_flags))
        {
            assert!(has(target, "CLI=1"), "got: {target:?}");
            assert!(has(target, "VALUE=2"), "got: {target:?}");
            let target_value = target.iter().position(|f| f == "-DVALUE=1").unwrap();
            let cli_value = target.iter().position(|f| f == "-DVALUE=2").unwrap();
            assert!(target_value < cli_value, "CLI must override target defines: {target:?}");
        }
        assert!(!p.tests[0].compile_flags.iter().any(|f| f.contains("NDEBUG")));
        assert!(!p.dep_base.iter().any(|f| f.contains("CLI") || f.contains("VALUE")));
        assert!(!p.flags.iter().any(|f| f.contains("CLI") || f.contains("VALUE")));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn public_defines_keep_abi_and_rebuild_when_changed() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("target-defines-abi");
        write_sub(
            &dir,
            "libs/core/include/core.hpp",
            "#pragma once\n#include <cstddef>\n#ifndef CORE_LAYOUT\n#define CORE_LAYOUT 1\n#endif\n\
             struct CoreLayout { unsigned char bytes[CORE_LAYOUT]; };\nstd::size_t core_layout_size();\n",
        );
        write_sub(
            &dir,
            "libs/core/src/core.cpp",
            "#include \"core.hpp\"\nstd::size_t core_layout_size(){return sizeof(CoreLayout);}\n",
        );
        write_sub(
            &dir,
            "app/src/main.cpp",
            "#include \"core.hpp\"\n#include <cstdio>\n\
             int main(){std::printf(\"%zu %zu\\n\", sizeof(CoreLayout), core_layout_size());\
             return sizeof(CoreLayout)==core_layout_size()?0:1;}\n",
        );
        let config = |layout: u32| {
            format!(
                "[project]\nname = \"t\"\n\n[compiler]\nflags = [\"-DCORE_LAYOUT=99\"]\nwerror = true\n\n\
                 [[target]]\nname = \"core\"\nkind = \"static_lib\"\nsources = [\"libs/core/src\"]\n\
                 include = \"libs/core/include\"\ndefines = [\"CORE_LAYOUT={layout}\"]\n\n\
                 [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"app/src\"]\nlinks = [\"core\"]\n"
            )
        };
        write_sub(&dir, "cizon.toml", &config(4));
        let exe = build_project(&dir).unwrap();
        assert_eq!(run_stdout(&exe).trim(), "4 4");
        assert_eq!(run_code(&exe), 0);
        let scope = scope_of(&dir, &exe);
        let core_obj = find_named(&scope, "core.o").expect("core object");
        let main_obj = obj_path(&dir, "main");
        let (core_t, main_t, exe_t) = (mtime(&core_obj), mtime(&main_obj), mtime(&exe));

        std::thread::sleep(std::time::Duration::from_millis(50));
        write_sub(&dir, "cizon.toml", &config(8));
        let exe = build_project(&dir).unwrap();
        assert_eq!(run_stdout(&exe).trim(), "8 8");
        assert_eq!(run_code(&exe), 0);
        assert_ne!(mtime(&core_obj), core_t, "the library definition change must rebuild its object");
        assert_ne!(mtime(&main_obj), main_t, "the public definition change must rebuild consumers");
        assert_ne!(mtime(&exe), exe_t, "the ABI change must relink the executable");

        let (core_t, main_t, exe_t) = (mtime(&core_obj), mtime(&main_obj), mtime(&exe));
        std::thread::sleep(std::time::Duration::from_millis(50));
        build_project(&dir).unwrap();
        assert_eq!(mtime(&core_obj), core_t, "a no-op build must keep library objects");
        assert_eq!(mtime(&main_obj), main_t, "a no-op build must keep consumer objects");
        assert_eq!(mtime(&exe), exe_t, "a no-op build must keep the executable");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn cli_defines_are_temporary_and_trigger_rebuild() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("cli-defines-temp");
        write_sub(&dir, "src/main.cpp", "int main(){return VALUE;}\n");
        write_sub(
            &dir,
            "cizon.toml",
            "[project]\nname = \"t\"\n\n[compiler]\nflags = [\"-DVALUE=1\"]\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\ndefines = [\"VALUE=2\"]\n",
        );
        let cfg = crate::config::load_resolved(&dir).unwrap();
        let cxx = crate::compiler::pick(None).unwrap();
        let defined = plan_with_defines(&dir, &cfg, cxx, false, &["VALUE=3".to_string()]).unwrap();
        let exe = build(&dir, &cfg, &defined).unwrap();
        assert_eq!(run_code(&exe), 3, "the CLI definition must win for this invocation");

        let exe = build_project(&dir).unwrap();
        assert_eq!(
            run_code(&exe),
            2,
            "without the CLI override, the TOML target definition must apply again"
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn implicit_project_defines_build_and_cli_override() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("project-defines-build");
        write_sub(&dir, "src/main.cpp", "int main(){return VALUE;}\n");
        write_sub(
            &dir,
            "cizon.toml",
            "[project]\nname = \"t\"\ndefines = [\"VALUE=1\"]\n",
        );
        let exe = build_project(&dir).unwrap();
        assert_eq!(run_code(&exe), 1, "project defines must reach the implicit bin");

        let cfg = crate::config::load_resolved(&dir).unwrap();
        let cxx = crate::compiler::pick(None).unwrap();
        let p = plan_with_defines(&dir, &cfg, cxx, false, &["VALUE=3".to_string()]).unwrap();
        let exe = build(&dir, &cfg, &p).unwrap();
        assert_eq!(run_code(&exe), 3, "the CLI must override project defines");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn target_include_propagates_transitively_and_links_static_deps() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("target-include");
        for path in [
            "libs/net/include/downflux/net",
            "libs/net/src",
            "libs/storage/include/downflux/storage",
            "libs/storage/src",
            "app/src",
            "tests",
        ] {
            fs::create_dir_all(dir.join(path)).unwrap();
        }
        fs::write(
            dir.join("libs/net/include/downflux/net/http_client.hpp"),
            "#pragma once\nint net_ping();\n",
        )
        .unwrap();
        fs::write(
            dir.join("libs/net/src/http_client.cpp"),
            "#include \"downflux/net/http_client.hpp\"\nint net_ping(){return 7;}\n",
        )
        .unwrap();
        fs::write(
            dir.join("libs/storage/include/downflux/storage/file_writer.hpp"),
            "#pragma once\nint storage_write();\n",
        )
        .unwrap();
        fs::write(
            dir.join("libs/storage/src/file_writer.cpp"),
            "#include \"downflux/net/http_client.hpp\"\n\
             #include \"downflux/storage/file_writer.hpp\"\n\
             int storage_write(){return net_ping()+5;}\n",
        )
        .unwrap();
        let main_source = "#include \"downflux/net/http_client.hpp\"\n\
                           #include \"downflux/storage/file_writer.hpp\"\n\
                           int main(){return net_ping()+storage_write()==19?0:1;}\n";
        fs::write(dir.join("app/src/main.cpp"), main_source).unwrap();
        fs::write(dir.join("tests/main.cpp"), main_source).unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"downflux\"\ncompile_commands = true\n\n\
             [[target]]\nname = \"net\"\nkind = \"static_lib\"\nsources = [\"libs/net/src\"]\ninclude = \"libs/net/include\"\n\n\
             [[target]]\nname = \"storage\"\nkind = \"static_lib\"\nsources = [\"libs/storage/src\"]\ninclude = \"libs/storage/include\"\nlinks = [\"net\"]\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"app/src\"]\nlinks = [\"storage\"]\n\n\
             [[target]]\nname = \"storage_test\"\nkind = \"test\"\nsources = [\"tests\"]\nlinks = [\"storage\"]\n",
        )
        .unwrap();
        let cfg = crate::config::load_resolved(&dir).unwrap();
        let cxx = crate::compiler::pick(None).unwrap();
        let p = plan(&dir, &cfg, cxx, false).unwrap();
        let result = super::build_bins(&dir, &cfg, &p).unwrap();
        assert_eq!(result.execs.len(), 1);
        assert_eq!(
            run_code(&result.execs[0].1),
            0,
            "includes and static link closure must propagate transitively"
        );
        let tests = super::build_tests(&dir, &cfg, &p, &[]).unwrap();
        assert_eq!(
            run_code(&tests.execs[0].1),
            0,
            "test targets must inherit the same include interface"
        );
        let db = fs::read_to_string(dir.join("compile_commands.json"))
            .unwrap()
            .replace("\\\\", "/");
        assert!(db.contains("libs/net/include"), "compile db must carry propagated includes");
        assert!(db.contains("libs/storage/include"));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn build_lock_rejects_a_second_build() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("build-lock");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}\n").unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();
        let lock = dir.join("build").join(".cizon-build.lock");
        fs::create_dir_all(lock.parent().unwrap()).unwrap();
        fs::write(&lock, "pid 0\n").unwrap();

        let err = build_project(&dir).unwrap_err();
        assert!(err.contains("another cizon build"), "got: {err}");
        assert!(lock.exists(), "a second build must not remove the active lock");

        fs::remove_file(&lock).unwrap();
        let exe = build_project(&dir).unwrap();
        assert!(!lock.exists(), "the lock must disappear after a successful build");
        assert_eq!(run_code(&exe), 0);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn clean_removes_only_build_directory() {
        let dir = temp_dir("clean");
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();
        fs::write(dir.join("cizon.lock"), "locked\n").unwrap();
        fs::write(dir.join("compile_commands.json"), "[]\n").unwrap();
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("deps/pkg")).unwrap();
        fs::create_dir_all(dir.join(".vscode")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}\n").unwrap();
        fs::write(dir.join("deps/pkg/keep.txt"), "keep\n").unwrap();
        fs::write(dir.join(".vscode/settings.json"), "{}\n").unwrap();
        fs::create_dir_all(dir.join("build/debug-g++/obj")).unwrap();
        fs::write(dir.join("build/debug-g++/obj/main.o"), "old").unwrap();

        assert!(clean(&dir).unwrap(), "an existing build directory must be cleaned");
        assert!(!dir.join("build").exists());
        assert!(dir.join("cizon.toml").exists(), "clean must preserve config");
        assert!(dir.join("cizon.lock").exists(), "clean must preserve the lock");
        assert!(dir.join("compile_commands.json").exists(), "clean must preserve the compilation database");
        assert!(dir.join("deps/pkg/keep.txt").exists(), "clean must preserve dependencies");
        assert!(dir.join(".vscode/settings.json").exists(), "clean must preserve editor configuration");
        assert!(dir.join("src/main.cpp").exists(), "clean must preserve sources");
        assert!(!clean(&dir).unwrap(), "a missing build directory is a no-op");
        assert!(!dir.join("build").exists(), "a no-op clean must not create build/");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn clean_removes_a_stray_build_file() {
        let dir = temp_dir("clean-file");
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();
        fs::write(dir.join("build"), "not a directory\n").unwrap();
        assert!(clean(&dir).unwrap(), "a stray build file must be removed");
        assert!(!dir.join("build").exists());
        assert!(!clean(&dir).unwrap(), "cleaning twice is a no-op");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn clean_does_not_follow_build_directory_link() {
        let parent = temp_dir("clean-link");
        let dir = parent.join("project");
        let outside = parent.join("outside");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("artifact"), "keep me\n").unwrap();
        if !make_dir_link(&dir.join("build"), &outside) {
            fs::remove_dir_all(parent).ok();
            return;
        }

        assert!(clean(&dir).unwrap(), "the build link itself should be removed");
        assert!(!dir.join("build").exists());
        assert!(outside.join("artifact").exists(), "clean must not delete the link target");
        fs::remove_dir_all(parent).ok();
    }

    #[test]
    fn clean_reports_an_active_build_lock() {
        let dir = temp_dir("clean-lock");
        fs::write(dir.join("cizon.toml"), "[project]\nname = \"t\"\n").unwrap();
        fs::create_dir_all(dir.join("build/debug-g++")).unwrap();
        let _lock = BuildLock::acquire(&dir).unwrap();
        let err = clean(&dir).unwrap_err();
        assert!(err.contains("another cizon build"), "got: {err}");
        assert!(dir.join("build").exists(), "a failed clean must preserve artifacts");
        drop(_lock);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn static_link_change_relinks_with_new_archive() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("link-change");
        for sub in ["one", "two", "src"] {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }
        fs::write(dir.join("one/value.cpp"), "int value(){return 1;}\n").unwrap();
        fs::write(dir.join("two/value.cpp"), "int value(){return 2;}\n").unwrap();
        fs::write(
            dir.join("src/main.cpp"),
            "#include <cstdio>\nint value();\nint main(){std::printf(\"%d\\n\", value()); return 0;}\n",
        )
        .unwrap();
        let config = |link: &str| {
            format!(
                "[project]\nname = \"t\"\nedition = \"c++23\"\n\n\
                 [[target]]\nname = \"one\"\nkind = \"static_lib\"\nsources = [\"one\"]\n\n\
                 [[target]]\nname = \"two\"\nkind = \"static_lib\"\nsources = [\"two\"]\n\n\
                 [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"{link}\"]\n"
            )
        };

        fs::write(dir.join("cizon.toml"), config("one")).unwrap();
        let exe = build_project(&dir).unwrap();
        assert_eq!(run_stdout(&exe).trim(), "1");

        fs::write(dir.join("cizon.toml"), config("two")).unwrap();
        let exe = build_project(&dir).unwrap();
        assert_eq!(
            run_stdout(&exe).trim(),
            "2",
            "changing links must relink against the new archive"
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn removing_static_target_cleans_archive_and_state() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("remove-lib");
        fs::create_dir_all(dir.join("lib")).unwrap();
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("lib/core.cpp"), "int core(){return 0;}\n").unwrap();
        fs::write(dir.join("src/main.cpp"), "int main(){return 0;}\n").unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\nedition = \"c++23\"\n\n\
             [[target]]\nname = \"core\"\nkind = \"static_lib\"\nsources = [\"lib\"]\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"core\"]\n",
        )
        .unwrap();
        build_project(&dir).unwrap();
        let archive = find_named(&dir.join("build"), "libcore.a").expect("archive");
        assert!(archive.is_file());

        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\nedition = \"c++23\"\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\n",
        )
        .unwrap();
        assert_eq!(run_code(&build_project(&dir).unwrap()), 0);
        assert!(
            find_named(&dir.join("build"), "libcore.a").is_none(),
            "removing a static target must delete its archive"
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn shared_include_change_recompiles_shared_and_consumer() {
        if !shared_supported() {
            return;
        }
        let dir = temp_dir("shared-include-change");
        for sub in ["inc_a", "inc_b", "api/src", "app/src"] {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }
        fs::write(
            dir.join("inc_a/api.hpp"),
            "#pragma once\n#define API_VALUE 1\nextern \"C\" int api_value();\n",
        )
        .unwrap();
        fs::write(
            dir.join("inc_b/api.hpp"),
            "#pragma once\n#define API_VALUE 2\nextern \"C\" int api_value();\n",
        )
        .unwrap();
        fs::write(
            dir.join("api/src/api.cpp"),
            "#if defined(_WIN32)\n#define CZ_EXPORT __declspec(dllexport)\n#else\n#define CZ_EXPORT\n#endif\n\
             #include \"api.hpp\"\nextern \"C\" CZ_EXPORT int api_value(){return API_VALUE;}\n",
        )
        .unwrap();
        fs::write(
            dir.join("app/src/main.cpp"),
            "#include <cstdio>\n#include \"api.hpp\"\n\
             int main(){std::printf(\"%d\\n\", API_VALUE); return api_value()==API_VALUE?0:1;}\n",
        )
        .unwrap();
        let config = |include: &str| {
            format!(
                "[project]\nname = \"t\"\nedition = \"c++23\"\n\n\
                 [[target]]\nname = \"api\"\nkind = \"shared_lib\"\nsources = [\"api/src\"]\ninclude = \"{include}\"\n\n\
                 [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"app/src\"]\nlinks = [\"api\"]\n"
            )
        };

        fs::write(dir.join("cizon.toml"), config("inc_a")).unwrap();
        let exe = build_project(&dir).unwrap();
        assert_eq!(run_stdout(&exe).trim(), "1");
        assert_eq!(run_code(&exe), 0);

        fs::write(dir.join("cizon.toml"), config("inc_b")).unwrap();
        let exe = build_project(&dir).unwrap();
        assert_eq!(
            run_stdout(&exe).trim(),
            "2",
            "changing a shared target include must recompile its consumer"
        );
        assert_eq!(
            run_code(&exe),
            0,
            "changing a shared target include must rebuild the shared library and consumer"
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn target_include_change_recompiles_library_and_consumer() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("include-change");
        for sub in ["inc_a", "inc_b", "lib", "src"] {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }
        fs::write(
            dir.join("inc_a/core.hpp"),
            "#pragma once\n#define CORE_VALUE 1\nint core();\n",
        )
        .unwrap();
        fs::write(
            dir.join("inc_b/core.hpp"),
            "#pragma once\n#define CORE_VALUE 2\nint core();\n",
        )
        .unwrap();
        fs::write(
            dir.join("lib/core.cpp"),
            "#include \"core.hpp\"\nint core(){return CORE_VALUE;}\n",
        )
        .unwrap();
        fs::write(
            dir.join("src/main.cpp"),
            "#include <cstdio>\n#include \"core.hpp\"\n\
             int main(){std::printf(\"%d\\n\", CORE_VALUE); return core()==CORE_VALUE?0:1;}\n",
        )
        .unwrap();
        let config = |include: &str| {
            format!(
                "[project]\nname = \"t\"\nedition = \"c++23\"\n\n\
                 [[target]]\nname = \"core\"\nkind = \"static_lib\"\nsources = [\"lib\"]\ninclude = \"{include}\"\n\n\
                 [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"core\"]\n"
            )
        };

        fs::write(dir.join("cizon.toml"), config("inc_a")).unwrap();
        let exe = build_project(&dir).unwrap();
        assert_eq!(run_code(&exe), 0);
        assert_eq!(run_stdout(&exe).trim(), "1");

        fs::write(dir.join("cizon.toml"), config("inc_b")).unwrap();
        let exe = build_project(&dir).unwrap();
        assert_eq!(
            run_stdout(&exe).trim(),
            "2",
            "the consumer must be recompiled against the new include path"
        );
        assert_eq!(
            run_code(&exe),
            0,
            "changing a target include must recompile both the library and its consumer"
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn shared_lib_propagates_public_includes_through_static_deps() {
        if !shared_supported() {
            return;
        }
        let dir = temp_dir("shared-include");
        for sub in [
            "core/include/core",
            "core/src",
            "api/include/api",
            "api/src",
            "app/src",
        ] {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }
        fs::write(
            dir.join("core/include/core/core.hpp"),
            "#pragma once\nint core_value();\n",
        )
        .unwrap();
        fs::write(
            dir.join("core/src/core.cpp"),
            "#include \"core/core.hpp\"\nint core_value(){return 5;}\n",
        )
        .unwrap();
        fs::write(
            dir.join("api/include/api/api.hpp"),
            "#pragma once\n#include \"core/core.hpp\"\nextern \"C\" int api_value();\n",
        )
        .unwrap();
        fs::write(
            dir.join("api/src/api.cpp"),
            "#if defined(_WIN32)\n#define CZ_EXPORT __declspec(dllexport)\n#else\n#define CZ_EXPORT\n#endif\n\
             #include \"api/api.hpp\"\nextern \"C\" CZ_EXPORT int api_value(){return core_value()+1;}\n",
        )
        .unwrap();
        fs::write(
            dir.join("app/src/main.cpp"),
            "#include \"api/api.hpp\"\nint main(){return api_value()==6?0:1;}\n",
        )
        .unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\nedition = \"c++23\"\n\n\
             [[target]]\nname = \"core\"\nkind = \"static_lib\"\nsources = [\"core/src\"]\ninclude = \"core/include\"\n\n\
             [[target]]\nname = \"api\"\nkind = \"shared_lib\"\nsources = [\"api/src\"]\ninclude = \"api/include\"\nlinks = [\"core\"]\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"app/src\"]\nlinks = [\"api\"]\n",
        )
        .unwrap();
        let exe = build_project(&dir).unwrap();
        let scope = scope_of(&dir, &exe);
        shared_dll(&scope, "api");
        assert_eq!(
            run_code(&exe),
            0,
            "shared_lib must propagate its own and static dependencies' public includes"
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn target_include_paths_are_validated_at_plan_time() {
        let dir = temp_dir("target-include-invalid");
        fs::create_dir_all(dir.join("lib")).unwrap();
        fs::write(dir.join("lib/x.cpp"), "int x(){return 0;}").unwrap();
        fs::write(dir.join("inc.txt"), "").unwrap();
        let mk = || Cxx {
            name: "g++".into(),
            version: "test".into(),
        };
        let check = |include: &str, needle: &str| {
            fs::write(
                dir.join("cizon.toml"),
                format!(
                    "[project]\nname = \"t\"\n\n\
                     [[target]]\nname = \"x\"\nkind = \"static_lib\"\nsources = [\"lib/x.cpp\"]\ninclude = \"{include}\"\n"
                ),
            )
            .unwrap();
            let cfg = crate::config::load_resolved(&dir).unwrap();
            let err = plan(&dir, &cfg, mk(), false).unwrap_err();
            assert!(err.contains(needle), "include {include}: got {err}");
        };
        check("missing", "not a directory");
        check("inc.txt", "not a directory");
        check(".", "project root");
        check("../outside", "project-relative");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn static_library_cycle_is_rejected_at_plan_time() {
        let dir = temp_dir("static-cycle");
        fs::create_dir_all(dir.join("lib")).unwrap();
        fs::write(dir.join("lib/a.cpp"), "int a(){return 0;}").unwrap();
        fs::write(dir.join("lib/b.cpp"), "int b(){return 0;}").unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n\
             [[target]]\nname = \"a\"\nkind = \"static_lib\"\nsources = [\"lib/a.cpp\"]\nlinks = [\"b\"]\n\n\
             [[target]]\nname = \"b\"\nkind = \"static_lib\"\nsources = [\"lib/b.cpp\"]\nlinks = [\"a\"]\n",
        )
        .unwrap();
        let cfg = crate::config::load_resolved(&dir).unwrap();
        let cxx = Cxx {
            name: "g++".into(),
            version: "test".into(),
        };
        let err = plan(&dir, &cfg, cxx, false).unwrap_err();
        assert!(err.contains("cycle"), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn prefix_provides_include_path_transparently() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("prefix");
        fs::create_dir_all(dir.join("tests")).unwrap();
        fs::create_dir_all(dir.join("pkg/include")).unwrap();
        fs::create_dir_all(dir.join("own/include")).unwrap();
        fs::write(
            dir.join("pkg/include/mylib.h"),
            "#pragma once\ninline int answer(){return 42;}\n",
        )
        .unwrap();
        fs::write(
            dir.join("own/include/own.h"),
            "#pragma once\ninline int own(){return 1;}\n",
        )
        .unwrap();
        fs::write(
            dir.join("tests/t.cpp"),
            "#include <mylib.h>\n#include <own.h>\nint main(){return answer()+own()==43?0:1;}\n",
        )
        .unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n\
             [[target]]\nname = \"t_test\"\nkind = \"test\"\nsources = [\"tests\"]\ninclude = \"own/include\"\nprefix = \"pkg\"\n",
        )
        .unwrap();
        let cfg = crate::config::load_resolved(&dir).unwrap();
        let cxx = crate::compiler::pick(None).unwrap();
        let p = plan(&dir, &cfg, cxx, false).unwrap();
        let result = super::build_tests(&dir, &cfg, &p, &[]).unwrap();
        assert_eq!(
            run_code(&result.execs[0].1),
            0,
            "target include and prefix include must both resolve"
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn prefix_must_be_an_existing_directory() {
        if !have_compiler() {
            return;
        }
        let dir = temp_dir("prefix-missing");
        fs::create_dir_all(dir.join("tests")).unwrap();
        fs::write(dir.join("tests/t.cpp"), "int main(){return 0;}").unwrap();
        fs::write(
            dir.join("cizon.toml"),
            "[project]\nname = \"t\"\n\n\
             [[target]]\nname = \"t_test\"\nkind = \"test\"\nsources = [\"tests\"]\nprefix = \"nope\"\n",
        )
        .unwrap();
        let cfg = crate::config::load_resolved(&dir).unwrap();
        let cxx = crate::compiler::pick(None).unwrap();
        let err = plan(&dir, &cfg, cxx, false).unwrap_err();
        assert!(err.contains("prefix"), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    // ── [subproject]: nested cizon projects ──────────────────────────────────

    /// Build through the resolved config (root + [subproject] graph merged).
    fn build_project_resolved(dir: &Path) -> Result<PathBuf, String> {
        let cfg = crate::config::load_resolved(dir)?;
        let cxx = crate::compiler::pick(None)?;
        let plan = plan(dir, &cfg, cxx, false)?;
        build(dir, &cfg, &plan)
    }

    fn write_sub(dir: &Path, rel: &str, text: &str) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, text).unwrap();
    }

    /// downflux shape: root bin + net/storage library subprojects. storage links net,
    /// the root bin links both; each library keeps its public headers in its own
    /// include/ and declares them with the v0.9.0 `include` field.
    fn downflux_project(tag: &str) -> PathBuf {
        let dir = temp_dir(tag);
        write_sub(
            &dir,
            "libs/net/cizon.toml",
            "[project]\nname = \"net\"\n\n\
             [[target]]\nname = \"net\"\nkind = \"static_lib\"\nsources = [\"src\"]\ninclude = \"include\"\n",
        );
        write_sub(
            &dir,
            "libs/net/include/downflux/net/http_client.hpp",
            "#pragma once\nnamespace downflux::net { int fetch(); }\n",
        );
        write_sub(
            &dir,
            "libs/net/src/http_client.cpp",
            "#include \"downflux/net/http_client.hpp\"\nnamespace downflux::net { int fetch() { return 40; } }\n",
        );
        write_sub(
            &dir,
            "libs/storage/cizon.toml",
            "[project]\nname = \"storage\"\n\n\
             [[target]]\nname = \"storage\"\nkind = \"static_lib\"\nsources = [\"src\"]\ninclude = \"include\"\nlinks = [\"net\"]\n",
        );
        write_sub(
            &dir,
            "libs/storage/include/downflux/storage/file_writer.hpp",
            "#pragma once\nnamespace downflux::storage { int write_all(); }\n",
        );
        write_sub(
            &dir,
            "libs/storage/src/file_writer.cpp",
            "#include \"downflux/storage/file_writer.hpp\"\n#include \"downflux/net/http_client.hpp\"\nnamespace downflux::storage { int write_all() { return downflux::net::fetch() + 2; } }\n",
        );
        write_sub(
            &dir,
            "cizon.toml",
            "[project]\nname = \"downflux\"\n\n\
             [subproject]\nnet = \"libs/net\"\nstorage = \"libs/storage\"\n\n\
             [[target]]\nname = \"downflux\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"net\", \"storage\"]\n",
        );
        write_sub(
            &dir,
            "src/main.cpp",
            "#include \"downflux/net/http_client.hpp\"\n#include \"downflux/storage/file_writer.hpp\"\n#include <cstdio>\nint main() {\n    int n = downflux::net::fetch() + downflux::storage::write_all();\n    std::printf(\"downflux: %d\\n\", n);\n    return n == 82 ? 0 : 1;\n}\n",
        );
        dir
    }

    #[test]
    fn subproject_downflux_builds_and_runs() {
        if !libs_supported() {
            return;
        }
        let dir = downflux_project("sub-downflux");
        let exe = build_project_resolved(&dir).unwrap();
        assert_eq!(run_stdout(&exe).trim(), "downflux: 82");
        assert_eq!(run_code(&exe), 0);
        // Both archives exist: each subproject's sources compiled against its own
        // directory, and storage's compile found net's headers through links.
        assert!(lib_archive_path(&dir, "net").is_file());
        assert!(lib_archive_path(&dir, "storage").is_file());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn subproject_compiler_is_ignored() {
        if !libs_supported() {
            return;
        }
        // The subproject names a compiler that does not exist; the build must still
        // succeed with the root project's compiler.
        let dir = downflux_project("sub-compiler-ignored");
        fs::write(
            dir.join("libs/net/cizon.toml"),
            "[project]\nname = \"net\"\n\n[compiler]\ncpp = \"definitely-not-a-compiler\"\nflags = [\"-DNOPE=1\"]\n\n\
             [[target]]\nname = \"net\"\nkind = \"static_lib\"\nsources = [\"src\"]\ninclude = \"include\"\n",
        )
        .unwrap();
        let exe = build_project_resolved(&dir).unwrap();
        assert_eq!(run_stdout(&exe).trim(), "downflux: 82");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn subproject_include_resolves_against_subproject_dir() {
        if !have_compiler() {
            return;
        }
        // The subproject's include = "include" must resolve to libs/net/include, not to
        // the root's include/ (which exists here but holds no such header).
        let dir = temp_dir("sub-include-dir");
        fs::create_dir_all(dir.join("include")).unwrap();
        fs::write(dir.join("include/root.h"), "#pragma once\n").unwrap();
        write_sub(
            &dir,
            "libs/net/cizon.toml",
            "[project]\nname = \"net\"\n\n\
             [[target]]\nname = \"net\"\nkind = \"static_lib\"\nsources = [\"src\"]\ninclude = \"missing\"\n",
        );
        fs::create_dir_all(dir.join("libs/net/src")).unwrap();
        fs::write(dir.join("libs/net/src/net.cpp"), "int net(){ return 0; }\n").unwrap();
        write_sub(
            &dir,
            "cizon.toml",
            "[project]\nname = \"app\"\n\n[subproject]\nnet = \"libs/net\"\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"net\"]\n",
        );
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.cpp"), "int net();\nint main(){ return net(); }\n").unwrap();
        let cfg = crate::config::load_resolved(&dir).unwrap();
        let cxx = crate::compiler::pick(None).unwrap();
        let err = plan(&dir, &cfg, cxx, false).unwrap_err();
        assert!(err.contains("include path"), "got: {err}");
        assert!(err.contains("not a directory"), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn subproject_diamond_builds_shared_lib_once() {
        if !libs_supported() {
            return;
        }
        // A includes B and C; B and C both include D. The build must succeed and D's
        // archive must exist exactly once.
        let dir = temp_dir("sub-diamond-build");
        write_sub(
            &dir,
            "cizon.toml",
            "[project]\nname = \"app\"\n\n[subproject]\nb = \"libs/b\"\nc = \"libs/c\"\nd = \"libs/d\"\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"b_lib\", \"c_lib\", \"d_lib\"]\n",
        );
        write_sub(
            &dir,
            "libs/b/cizon.toml",
            "[project]\nname = \"b\"\n\n[subproject]\nd = \"../d\"\n\n\
             [[target]]\nname = \"b_lib\"\nkind = \"static_lib\"\nsources = [\"src\"]\nlinks = [\"d_lib\"]\n",
        );
        write_sub(
            &dir,
            "libs/b/src/b.cpp",
            "int d_lib();\nint b_lib() { return d_lib(); }\n",
        );
        write_sub(
            &dir,
            "libs/c/cizon.toml",
            "[project]\nname = \"c\"\n\n[subproject]\nd = \"../d\"\n\n\
             [[target]]\nname = \"c_lib\"\nkind = \"static_lib\"\nsources = [\"src\"]\nlinks = [\"d_lib\"]\n",
        );
        write_sub(
            &dir,
            "libs/c/src/c.cpp",
            "int d_lib();\nint c_lib() { return d_lib(); }\n",
        );
        write_sub(
            &dir,
            "libs/d/cizon.toml",
            "[project]\nname = \"d\"\n\n[[target]]\nname = \"d_lib\"\nkind = \"static_lib\"\nsources = [\"src\"]\n",
        );
        write_sub(&dir, "libs/d/src/d.cpp", "int d_lib() { return 3; }\n");
        write_sub(
            &dir,
            "src/main.cpp",
            "int b_lib();\nint c_lib();\nint main() { return b_lib() + c_lib() == 6 ? 0 : 1; }\n",
        );
        let exe = build_project_resolved(&dir).unwrap();
        assert_eq!(run_code(&exe), 0);
        assert!(lib_archive_path(&dir, "d_lib").is_file());
        assert!(lib_archive_path(&dir, "b_lib").is_file());
        assert!(lib_archive_path(&dir, "c_lib").is_file());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn subproject_nested_builds_and_runs() {
        if !libs_supported() {
            return;
        }
        // Root -> mid -> leaf; the root bin links the leaf's target directly.
        let dir = temp_dir("sub-nested-build");
        write_sub(
            &dir,
            "cizon.toml",
            "[project]\nname = \"app\"\n\n[subproject]\nmid = \"libs/mid\"\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"mid\", \"leaf\"]\n",
        );
        write_sub(
            &dir,
            "libs/mid/cizon.toml",
            "[project]\nname = \"mid\"\n\n[subproject]\nleaf = \"leaf\"\n\n\
             [[target]]\nname = \"mid\"\nkind = \"static_lib\"\nsources = [\"src\"]\n",
        );
        write_sub(&dir, "libs/mid/src/mid.cpp", "int mid() { return 1; }\n");
        write_sub(
            &dir,
            "libs/mid/leaf/cizon.toml",
            "[project]\nname = \"leaf\"\n\n[[target]]\nname = \"leaf\"\nkind = \"static_lib\"\nsources = [\"src\"]\n",
        );
        write_sub(&dir, "libs/mid/leaf/src/leaf.cpp", "int leaf() { return 2; }\n");
        write_sub(
            &dir,
            "src/main.cpp",
            "int mid();\nint leaf();\nint main() { return mid() + leaf() == 3 ? 0 : 1; }\n",
        );
        let exe = build_project_resolved(&dir).unwrap();
        assert_eq!(run_code(&exe), 0);
        assert!(lib_archive_path(&dir, "mid").is_file());
        assert!(lib_archive_path(&dir, "leaf").is_file());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn subproject_edition_is_ignored() {
        if !libs_supported() {
            return;
        }
        // The subproject declares c++17 but uses std::expected (C++23-only): the build
        // must still succeed, because the root's edition (c++23 here, the default)
        // applies to every target. The differing edition is warned about at load time.
        let dir = temp_dir("sub-edition-build");
        write_sub(
            &dir,
            "libs/net/cizon.toml",
            "[project]\nname = \"net\"\nedition = \"c++17\"\n\n\
             [[target]]\nname = \"net\"\nkind = \"static_lib\"\nsources = [\"src\"]\n",
        );
        write_sub(
            &dir,
            "libs/net/src/net.cpp",
            "#include <expected>\nstd::expected<int, int> net() { return 7; }\nint net_value() { return net().value(); }\n",
        );
        write_sub(
            &dir,
            "cizon.toml",
            "[project]\nname = \"app\"\n\n[subproject]\nnet = \"libs/net\"\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"net\"]\n",
        );
        write_sub(
            &dir,
            "src/main.cpp",
            "int net_value();\nint main() { return net_value() == 7 ? 0 : 1; }\n",
        );
        let exe = build_project_resolved(&dir).unwrap();
        assert_eq!(run_code(&exe), 0);
        assert!(lib_archive_path(&dir, "net").is_file());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn subproject_shared_lib_builds_and_runs() {
        if !shared_supported() {
            return;
        }
        // A shared_lib subproject: the .so is produced, the root bin links it and
        // runs against it (the downflux shape, with a shared library instead of a
        // static one).
        let dir = temp_dir("sub-shared-lib");
        write_sub(
            &dir,
            "libs/api/cizon.toml",
            "[project]\nname = \"api\"\n\n\
             [[target]]\nname = \"api\"\nkind = \"shared_lib\"\nsources = [\"src\"]\n",
        );
        write_sub(
            &dir,
            "libs/api/src/api.cpp",
            "#if defined(_WIN32)\n#define CZ_EXPORT __declspec(dllexport)\n#else\n#define CZ_EXPORT\n#endif\nextern \"C\" CZ_EXPORT int api_value() { return 5; }\n",
        );
        write_sub(
            &dir,
            "cizon.toml",
            "[project]\nname = \"app\"\n\n[subproject]\napi = \"libs/api\"\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"api\"]\n",
        );
        write_sub(
            &dir,
            "src/main.cpp",
            "extern \"C\" int api_value();\nint main() { return api_value() == 5 ? 0 : 1; }\n",
        );
        let exe = build_project_resolved(&dir).unwrap();
        let scope = scope_of(&dir, &exe);
        let _so = shared_dll(&scope, "api");
        assert_eq!(run_code(&exe), 0);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn subproject_test_links_other_subproject_lib() {
        if !libs_supported() {
            return;
        }
        // A's test target links B's library directly: test targets resolve links
        // across subprojects, not just against the root's own libraries.
        let dir = temp_dir("sub-cross-test");
        write_sub(
            &dir,
            "libs/b/cizon.toml",
            "[project]\nname = \"b\"\n\n\
             [[target]]\nname = \"b_lib\"\nkind = \"static_lib\"\nsources = [\"src\"]\n",
        );
        write_sub(&dir, "libs/b/src/b.cpp", "int b_lib() { return 3; }\n");
        write_sub(
            &dir,
            "libs/a/cizon.toml",
            "[project]\nname = \"a\"\n\n\
             [[target]]\nname = \"a_test\"\nkind = \"test\"\nsources = [\"tests\"]\nlinks = [\"b_lib\"]\n",
        );
        write_sub(
            &dir,
            "libs/a/tests/a_test.cpp",
            "int b_lib();\nint main() { return b_lib() == 3 ? 0 : 1; }\n",
        );
        write_sub(
            &dir,
            "cizon.toml",
            "[project]\nname = \"app\"\n\n[subproject]\na = \"libs/a\"\nb = \"libs/b\"\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"b_lib\"]\n",
        );
        write_sub(
            &dir,
            "src/main.cpp",
            "int b_lib();\nint main() { return b_lib() == 3 ? 0 : 1; }\n",
        );
        let cfg = crate::config::load_resolved(&dir).unwrap();
        let cxx = crate::compiler::pick(None).unwrap();
        let p = plan(&dir, &cfg, cxx, false).unwrap();
        let result = super::build_tests(&dir, &cfg, &p, &[]).unwrap();
        assert_eq!(result.execs.len(), 1);
        assert_eq!(result.execs[0].0, "a_test");
        assert_eq!(run_code(&result.execs[0].1), 0);
        assert!(lib_archive_path(&dir, "b_lib").is_file());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unused_sources_covers_subproject_src() {
        // The net subproject compiles only src/a.cpp; src/unused.cpp sits in the
        // subproject's src/ uncompiled. The unused-source scan must report it,
        // just as it reports uncompiled files under the root's src/.
        let dir = temp_dir("unused-sub-src");
        write_sub(
            &dir,
            "libs/net/cizon.toml",
            "[project]\nname = \"net\"\n\n\
             [[target]]\nname = \"net\"\nkind = \"static_lib\"\nsources = [\"src/a.cpp\"]\n",
        );
        write_sub(&dir, "libs/net/src/a.cpp", "int net() { return 1; }\n");
        write_sub(&dir, "libs/net/src/unused.cpp", "int unused() { return 2; }\n");
        write_sub(
            &dir,
            "cizon.toml",
            "[project]\nname = \"app\"\n\n[subproject]\nnet = \"libs/net\"\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"net\"]\n",
        );
        write_sub(
            &dir,
            "src/main.cpp",
            "int net();\nint main() { return net(); }\n",
        );
        let cfg = crate::config::load_resolved(&dir).unwrap();
        let cxx = Cxx { name: "g++".into(), version: "test".into() };
        let p = plan(&dir, &cfg, cxx, false).unwrap();
        let base_dirs = target_base_dirs(&dir, cfg.targets().unwrap());
        let unused = unused_sources(&dir, &base_dirs, &p.bins, &p.tests, &p.libs, &p.shared);
        assert_eq!(unused, vec![dir.join("libs/net/src/unused.cpp")]);
        fs::remove_dir_all(dir).ok();
    }

    // ── Duplicate public headers ──────────────────────────────────────────

    /// Two library subprojects that both provide `include/util.hpp` at their include
    /// root, with a root bin linking both: the shape that used to build silently and
    /// let link order pick the winner.
    fn duplicate_header_project(tag: &str) -> PathBuf {
        let dir = temp_dir(tag);
        write_sub(
            &dir,
            "libs/net/cizon.toml",
            "[project]\nname = \"net\"\n\n\
             [[target]]\nname = \"net\"\nkind = \"static_lib\"\nsources = [\"src\"]\ninclude = \"include\"\n",
        );
        write_sub(
            &dir,
            "libs/net/include/util.hpp",
            "#pragma once\ninline const char* util_origin() { return \"net\"; }\n",
        );
        write_sub(&dir, "libs/net/src/net.cpp", "int net() { return 1; }\n");
        write_sub(
            &dir,
            "libs/storage/cizon.toml",
            "[project]\nname = \"storage\"\n\n\
             [[target]]\nname = \"storage\"\nkind = \"static_lib\"\nsources = [\"src\"]\ninclude = \"include\"\n",
        );
        write_sub(
            &dir,
            "libs/storage/include/util.hpp",
            "#pragma once\ninline const char* util_origin() { return \"storage\"; }\n",
        );
        write_sub(&dir, "libs/storage/src/storage.cpp", "int storage() { return 2; }\n");
        write_sub(
            &dir,
            "cizon.toml",
            "[project]\nname = \"app\"\n\n[subproject]\nnet = \"libs/net\"\nstorage = \"libs/storage\"\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"net\", \"storage\"]\n",
        );
        write_sub(&dir, "src/main.cpp", "int main() { return 0; }\n");
        dir
    }

    #[test]
    fn duplicate_header_in_same_link_closure_is_rejected_at_plan_time() {
        let dir = duplicate_header_project("dup-header-closure");
        let cfg = crate::config::load_resolved(&dir).unwrap();
        let cxx = Cxx { name: "g++".into(), version: "test".into() };
        let err = plan(&dir, &cfg, cxx, false).unwrap_err();
        assert!(err.contains("duplicate header \"util.hpp\""), "got: {err}");
        assert!(err.contains("libs/net/include/util.hpp"), "got: {err}");
        assert!(err.contains("libs/storage/include/util.hpp"), "got: {err}");
        assert!(err.contains("\"net\""), "got: {err}");
        assert!(err.contains("\"storage\""), "got: {err}");
        // Labels are project-relative, never absolute or canonical paths.
        assert!(!err.contains(dir.to_string_lossy().as_ref()), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn namespaced_headers_do_not_collide() {
        // Same two-library shape, but each header lives under include/<project>/, so
        // the relative paths differ and the plan must succeed.
        let dir = temp_dir("dup-header-namespaced");
        write_sub(
            &dir,
            "libs/net/cizon.toml",
            "[project]\nname = \"net\"\n\n\
             [[target]]\nname = \"net\"\nkind = \"static_lib\"\nsources = [\"src\"]\ninclude = \"include\"\n",
        );
        write_sub(
            &dir,
            "libs/net/include/downflux/net/util.hpp",
            "#pragma once\ninline const char* util_origin() { return \"net\"; }\n",
        );
        write_sub(&dir, "libs/net/src/net.cpp", "int net() { return 1; }\n");
        write_sub(
            &dir,
            "libs/storage/cizon.toml",
            "[project]\nname = \"storage\"\n\n\
             [[target]]\nname = \"storage\"\nkind = \"static_lib\"\nsources = [\"src\"]\ninclude = \"include\"\n",
        );
        write_sub(
            &dir,
            "libs/storage/include/downflux/storage/util.hpp",
            "#pragma once\ninline const char* util_origin() { return \"storage\"; }\n",
        );
        write_sub(&dir, "libs/storage/src/storage.cpp", "int storage() { return 2; }\n");
        write_sub(
            &dir,
            "cizon.toml",
            "[project]\nname = \"app\"\n\n[subproject]\nnet = \"libs/net\"\nstorage = \"libs/storage\"\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"net\", \"storage\"]\n",
        );
        write_sub(&dir, "src/main.cpp", "int main() { return 0; }\n");
        let cfg = crate::config::load_resolved(&dir).unwrap();
        let cxx = Cxx { name: "g++".into(), version: "test".into() };
        let _p = plan(&dir, &cfg, cxx, false).unwrap();
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn transitive_duplicate_header_is_rejected_at_plan_time() {
        // storage links net; both provide include/util.hpp. The root bin links
        // storage only, but the collision sits in its transitive closure.
        let dir = temp_dir("dup-header-transitive");
        write_sub(
            &dir,
            "libs/net/cizon.toml",
            "[project]\nname = \"net\"\n\n\
             [[target]]\nname = \"net\"\nkind = \"static_lib\"\nsources = [\"src\"]\ninclude = \"include\"\n",
        );
        write_sub(
            &dir,
            "libs/net/include/util.hpp",
            "#pragma once\ninline const char* util_origin() { return \"net\"; }\n",
        );
        write_sub(&dir, "libs/net/src/net.cpp", "int net() { return 1; }\n");
        write_sub(
            &dir,
            "libs/storage/cizon.toml",
            "[project]\nname = \"storage\"\n\n\
             [[target]]\nname = \"storage\"\nkind = \"static_lib\"\nsources = [\"src\"]\ninclude = \"include\"\nlinks = [\"net\"]\n",
        );
        write_sub(
            &dir,
            "libs/storage/include/util.hpp",
            "#pragma once\ninline const char* util_origin() { return \"storage\"; }\n",
        );
        write_sub(
            &dir,
            "libs/storage/src/storage.cpp",
            "int net();\nint storage() { return net(); }\n",
        );
        write_sub(
            &dir,
            "cizon.toml",
            "[project]\nname = \"app\"\n\n[subproject]\nnet = \"libs/net\"\nstorage = \"libs/storage\"\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"storage\"]\n",
        );
        write_sub(&dir, "src/main.cpp", "int main() { return 0; }\n");
        let cfg = crate::config::load_resolved(&dir).unwrap();
        let cxx = Cxx { name: "g++".into(), version: "test".into() };
        let err = plan(&dir, &cfg, cxx, false).unwrap_err();
        assert!(err.contains("duplicate header \"util.hpp\""), "got: {err}");
        assert!(err.contains("libs/net/include/util.hpp"), "got: {err}");
        assert!(err.contains("libs/storage/include/util.hpp"), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn duplicate_header_in_separate_closures_is_allowed() {
        // Both libraries provide include/util.hpp, but no target links both: bin A
        // links net only, bin B links storage only. The check runs per link closure,
        // so the plan must succeed.
        let dir = temp_dir("dup-header-separate");
        write_sub(
            &dir,
            "libs/net/cizon.toml",
            "[project]\nname = \"net\"\n\n\
             [[target]]\nname = \"net\"\nkind = \"static_lib\"\nsources = [\"src\"]\ninclude = \"include\"\n",
        );
        write_sub(
            &dir,
            "libs/net/include/util.hpp",
            "#pragma once\ninline const char* util_origin() { return \"net\"; }\n",
        );
        write_sub(&dir, "libs/net/src/net.cpp", "int net() { return 1; }\n");
        write_sub(
            &dir,
            "libs/storage/cizon.toml",
            "[project]\nname = \"storage\"\n\n\
             [[target]]\nname = \"storage\"\nkind = \"static_lib\"\nsources = [\"src\"]\ninclude = \"include\"\n",
        );
        write_sub(
            &dir,
            "libs/storage/include/util.hpp",
            "#pragma once\ninline const char* util_origin() { return \"storage\"; }\n",
        );
        write_sub(&dir, "libs/storage/src/storage.cpp", "int storage() { return 2; }\n");
        write_sub(
            &dir,
            "cizon.toml",
            "[project]\nname = \"app\"\n\n[subproject]\nnet = \"libs/net\"\nstorage = \"libs/storage\"\n\n\
             [[target]]\nname = \"app_a\"\nkind = \"bin\"\nsources = [\"src/a.cpp\"]\nlinks = [\"net\"]\n\n\
             [[target]]\nname = \"app_b\"\nkind = \"bin\"\nsources = [\"src/b.cpp\"]\nlinks = [\"storage\"]\n",
        );
        write_sub(&dir, "src/a.cpp", "int net();\nint main() { return net(); }\n");
        write_sub(&dir, "src/b.cpp", "int storage();\nint main() { return storage(); }\n");
        let cfg = crate::config::load_resolved(&dir).unwrap();
        let cxx = Cxx { name: "g++".into(), version: "test".into() };
        let _p = plan(&dir, &cfg, cxx, false).unwrap();
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn duplicate_header_check_includes_project_root_include() {
        let dir = temp_dir("dup-header-root");
        write_sub(&dir, "include/api.hpp", "#pragma once\n");
        write_sub(
            &dir,
            "libs/net/cizon.toml",
            "[project]\nname = \"net\"\n\n\
             [[target]]\nname = \"net\"\nkind = \"static_lib\"\nsources = [\"src\"]\ninclude = \"include\"\n",
        );
        write_sub(&dir, "libs/net/include/api.hpp", "#pragma once\n");
        write_sub(&dir, "libs/net/src/net.cpp", "int net() { return 1; }\n");
        write_sub(
            &dir,
            "cizon.toml",
            "[project]\nname = \"app\"\n\n[subproject]\nnet = \"libs/net\"\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"net\"]\n",
        );
        write_sub(&dir, "src/main.cpp", "int net();\nint main() { return net(); }\n");
        let cfg = crate::config::load_resolved(&dir).unwrap();
        let cxx = Cxx { name: "g++".into(), version: "test".into() };
        let err = plan(&dir, &cfg, cxx, false).unwrap_err();
        assert!(err.contains("duplicate header \"api.hpp\""), "got: {err}");
        assert!(err.contains("include/api.hpp"), "got: {err}");
        assert!(err.contains("libs/net/include/api.hpp"), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn duplicate_header_check_follows_include_directory_links() {
        let parent = temp_dir("dup-header-links");
        let dir = parent.join("project");
        fs::create_dir_all(dir.join("a/include")).unwrap();
        fs::create_dir_all(dir.join("b/include")).unwrap();
        fs::create_dir_all(dir.join("a-vendor")).unwrap();
        fs::create_dir_all(dir.join("b-vendor")).unwrap();
        fs::write(dir.join("a-vendor/api.hpp"), "#pragma once\n").unwrap();
        fs::write(dir.join("b-vendor/api.hpp"), "#pragma once\n").unwrap();
        let a_link = dir.join("a/include/vendor");
        let b_link = dir.join("b/include/vendor");
        if !make_dir_link(&a_link, &dir.join("a-vendor"))
            || !make_dir_link(&b_link, &dir.join("b-vendor"))
        {
            remove_dir_link(&a_link);
            remove_dir_link(&b_link);
            fs::remove_dir_all(parent).ok();
            return;
        }
        let pairs = vec![
            ("a".to_string(), dir.join("a/include")),
            ("b".to_string(), dir.join("b/include")),
        ];
        let err = check_closure_includes(&dir, &pairs, cfg!(windows)).unwrap_err();
        assert!(err.contains("duplicate header \"vendor/api.hpp\""), "got: {err}");
        remove_dir_link(&a_link);
        remove_dir_link(&b_link);
        fs::remove_dir_all(parent).ok();
    }

    #[test]
    fn include_directory_link_outside_project_is_rejected() {
        let parent = temp_dir("include-link-outside");
        let dir = parent.join("project");
        let outside = parent.join("outside");
        fs::create_dir_all(dir.join("include")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("api.hpp"), "#pragma once\n").unwrap();
        let link = dir.join("include/vendor");
        if !make_dir_link(&link, &outside) {
            fs::remove_dir_all(parent).ok();
            return;
        }
        let pairs = vec![("lib".to_string(), dir.join("include"))];
        let err = check_closure_includes(&dir, &pairs, cfg!(windows)).unwrap_err();
        assert!(err.contains("outside the project"), "got: {err}");
        assert!(err.contains("include/vendor"), "got: {err}");
        assert!(!err.contains(&dir.to_string_lossy().to_string()), "got: {err}");
        remove_dir_link(&link);
        fs::remove_dir_all(parent).ok();
    }

    #[test]
    fn duplicate_header_rejects_real_build() {
        if !libs_supported() {
            return;
        }
        // The same colliding shape, built through the resolved config: the build
        // must fail at plan time, before any compilation.
        let dir = duplicate_header_project("dup-header-build");
        let err = build_project_resolved(&dir).unwrap_err();
        assert!(err.contains("duplicate header \"util.hpp\""), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    fn case_only_header_project(tag: &str) -> PathBuf {
        // Full project: net provides include/Util.hpp, storage provides
        // include/util.hpp, root bin links both. The two headers differ only by
        // case, so they shadow each other on case-insensitive filesystems
        // (Windows) but are different files on case-sensitive ones (Linux).
        let dir = temp_dir(tag);
        write_sub(
            &dir,
            "libs/net/cizon.toml",
            "[project]\nname = \"net\"\n\n\
             [[target]]\nname = \"net\"\nkind = \"static_lib\"\nsources = [\"src\"]\ninclude = \"include\"\n",
        );
        write_sub(
            &dir,
            "libs/net/include/Util.hpp",
            "#pragma once\ninline const char* util_origin() { return \"net\"; }\n",
        );
        write_sub(&dir, "libs/net/src/net.cpp", "int net() { return 1; }\n");
        write_sub(
            &dir,
            "libs/storage/cizon.toml",
            "[project]\nname = \"storage\"\n\n\
             [[target]]\nname = \"storage\"\nkind = \"static_lib\"\nsources = [\"src\"]\ninclude = \"include\"\n",
        );
        write_sub(
            &dir,
            "libs/storage/include/util.hpp",
            "#pragma once\ninline const char* util_origin() { return \"storage\"; }\n",
        );
        write_sub(&dir, "libs/storage/src/storage.cpp", "int storage() { return 2; }\n");
        write_sub(
            &dir,
            "cizon.toml",
            "[project]\nname = \"app\"\n\n[subproject]\nnet = \"libs/net\"\nstorage = \"libs/storage\"\n\n\
             [[target]]\nname = \"app\"\nkind = \"bin\"\nsources = [\"src\"]\nlinks = [\"net\", \"storage\"]\n",
        );
        write_sub(&dir, "src/main.cpp", "int main() { return 0; }\n");
        dir
    }

    #[test]
    fn case_only_duplicate_header_is_rejected_when_case_insensitive() {
        // Simulates the Windows branch: the duplicate test must treat Util.hpp
        // and util.hpp as the same header.
        let dir = case_only_header_project("dup-header-case-insensitive");
        let pairs = vec![
            ("net".to_string(), dir.join("libs/net/include")),
            ("storage".to_string(), dir.join("libs/storage/include")),
        ];
        let err = check_closure_includes(&dir, &pairs, true).unwrap_err();
        assert!(err.contains("duplicate header"), "got: {err}");
        // The message names both files with their original case.
        assert!(err.contains("libs/net/include/Util.hpp"), "got: {err}");
        assert!(err.contains("libs/storage/include/util.hpp"), "got: {err}");
        assert!(!err.contains(dir.to_string_lossy().as_ref()), "got: {err}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn case_only_duplicate_header_is_allowed_when_case_sensitive() {
        // Same two headers, exact matching: they are different files, so the
        // check must pass.
        let dir = case_only_header_project("dup-header-case-sensitive");
        let pairs = vec![
            ("net".to_string(), dir.join("libs/net/include")),
            ("storage".to_string(), dir.join("libs/storage/include")),
        ];
        check_closure_includes(&dir, &pairs, false).unwrap();
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn case_only_duplicate_header_at_plan_time_follows_platform() {
        // The same project through plan(): on case-insensitive filesystems
        // (Windows) the two headers shadow each other and the plan must fail;
        // on case-sensitive ones (Linux) they are different files and the plan
        // must succeed.
        let dir = case_only_header_project("dup-header-case-plan");
        let cfg = crate::config::load_resolved(&dir).unwrap();
        let cxx = Cxx { name: "g++".into(), version: "test".into() };
        let result = plan(&dir, &cfg, cxx, false);
        if cfg!(windows) {
            let err = result.unwrap_err();
            assert!(err.contains("duplicate header"), "got: {err}");
            assert!(err.contains("libs/net/include/Util.hpp"), "got: {err}");
            assert!(err.contains("libs/storage/include/util.hpp"), "got: {err}");
        } else {
            result.unwrap();
        }
        fs::remove_dir_all(dir).ok();
    }
