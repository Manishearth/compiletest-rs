// Copyright 2012-2014 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

#![crate_type = "lib"]

#![feature(box_syntax)]
#![feature(rustc_private)]
//#![feature(static_in_const)]
#![feature(test)]
#![feature(libc)]

//#![deny(warnings)]

extern crate libc;
extern crate test;
extern crate rustc;
// extern crate getopts;
extern crate rustc_serialize;

#[macro_use]
extern crate log;
extern crate env_logger;
extern crate filetime;

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use filetime::FileTime;
use common::Config;
use common::{Pretty, DebugInfoGdb, DebugInfoLldb, Mode};
use test::TestPaths;
use rustc::session::config::host_triple;

use self::header::EarlyProps;

pub mod procsrv;
pub mod util;
mod json;
pub mod header;
pub mod runtest;
pub mod common;
pub mod errors;
mod raise_fd_limit;
mod uidiff;

pub fn default_config() -> Config {
    Config {
        compile_lib_path: PathBuf::from(""),
        run_lib_path: PathBuf::from(""),
        rustc_path: PathBuf::from("rustc"),
        rustdoc_path: PathBuf::from("rustdoc-path"),
        lldb_python: "python".to_owned(),
        docck_python: "docck-python".to_owned(),
        valgrind_path: None,
        force_valgrind: false,
        llvm_filecheck: None,
        src_base: PathBuf::from("tests/run-pass"),
        build_base: env::temp_dir(),
        stage_id: "stage-id".to_owned(),
        mode: Mode::RunPass,
        run_ignored: false,
        filter: None,
        filter_exact: false,
        logfile: None,
        runtool: None,
        host_rustcflags: None,
        target_rustcflags: None,
        target: host_triple().to_owned(),
        host: "(none)".to_owned(),
        gdb: None,
        gdb_version: None,
        gdb_native_rust: false,
        lldb_version: None,
        llvm_version: None,
        android_cross_path: PathBuf::from("android-cross-path"),
        adb_path: "adb-path".to_owned(),
        adb_test_dir: "adb-test-dir/target".to_owned(),
        adb_device_status: false,
        lldb_python_dir: None,
        verbose: false,
        quiet: false,
        qemu_test_client: None,
        cc: "cc".to_string(),
        cxx: "cxx".to_string(),
        cflags: "cflags".to_string(),
        llvm_components: "llvm-components".to_string(),
        llvm_cxxflags: "llvm-cxxflags".to_string(),
        nodejs: None,
    }
}

pub fn run_tests(config: &Config) {
    if config.target.contains("android") {
        if let DebugInfoGdb = config.mode {
            println!("{} debug-info test uses tcp 5039 port.\
                     please reserve it", config.target);
        }

        // android debug-info test uses remote debugger
        // so, we test 1 thread at once.
        // also trying to isolate problems with adb_run_wrapper.sh ilooping
        match config.mode {
            // These tests don't actually run code or don't run for android, so
            // we don't need to limit ourselves there
            Mode::Ui |
            Mode::CompileFail |
            Mode::ParseFail |
            Mode::RunMake |
            Mode::Codegen |
            Mode::CodegenUnits |
            Mode::Pretty |
            Mode::Rustdoc => {}

            _ => {
                env::set_var("RUST_TEST_THREADS", "1");
            }

        }
    }

    match config.mode {
        DebugInfoLldb => {
            if let Some(lldb_version) = config.lldb_version.as_ref() {
                if is_blacklisted_lldb_version(&lldb_version[..]) {
                    println!("WARNING: The used version of LLDB ({}) has a \
                              known issue that breaks debuginfo tests. See \
                              issue #32520 for more information. Skipping all \
                              LLDB-based tests!",
                             lldb_version);
                    return
                }
            }

            // Some older versions of LLDB seem to have problems with multiple
            // instances running in parallel, so only run one test thread at a
            // time.
            env::set_var("RUST_TEST_THREADS", "1");
        }

        DebugInfoGdb => {
            if config.qemu_test_client.is_some() {
                println!("WARNING: debuginfo tests are not available when \
                          testing with QEMU");
                return
            }
        }
        _ => { /* proceed */ }
    }

    // FIXME(#33435) Avoid spurious failures in codegen-units/partitioning tests.
    if let Mode::CodegenUnits = config.mode {
        let _ = fs::remove_dir_all("tmp/partitioning-tests");
    }

    let opts = test_opts(config);
    let tests = make_tests(config);
    // sadly osx needs some file descriptor limits raised for running tests in
    // parallel (especially when we have lots and lots of child processes).
    // For context, see #8904
    unsafe { raise_fd_limit::raise_fd_limit(); }
    // Prevent issue #21352 UAC blocking .exe containing 'patch' etc. on Windows
    // If #11207 is resolved (adding manifest to .exe) this becomes unnecessary
    env::set_var("__COMPAT_LAYER", "RunAsInvoker");

    // Let tests know which target they're running as
    env::set_var("TARGET", &config.target);

    let res = test::run_tests_console(&opts, tests.into_iter().collect());
    match res {
        Ok(true) => {}
        Ok(false) => panic!("Some tests failed"),
        Err(e) => {
            println!("I/O failure during tests: {:?}", e);
        }
    }
}

pub fn test_opts(config: &Config) -> test::TestOpts {
    test::TestOpts {
        filter: config.filter.clone(),
        filter_exact: config.filter_exact,
        run_ignored: config.run_ignored,
        quiet: config.quiet,
        logfile: config.logfile.clone(),
        run_tests: true,
        bench_benchmarks: true,
        nocapture: match env::var("RUST_TEST_NOCAPTURE") {
            Ok(val) => &val != "0",
            Err(_) => false
        },
        color: test::AutoColor,
        test_threads: None,
        skip: vec![],
        list: false,
    }
}

pub fn make_tests(config: &Config) -> Vec<test::TestDescAndFn> {
    debug!("making tests from {:?}",
           config.src_base.display());
    let mut tests = Vec::new();
    collect_tests_from_dir(config,
                           &config.src_base,
                           &config.src_base,
                           &PathBuf::new(),
                           &mut tests)
        .unwrap();
    tests
}

fn collect_tests_from_dir(config: &Config,
                          base: &Path,
                          dir: &Path,
                          relative_dir_path: &Path,
                          tests: &mut Vec<test::TestDescAndFn>)
                          -> io::Result<()> {
    // Ignore directories that contain a file
    // `compiletest-ignore-dir`.
    for file in fs::read_dir(dir)? {
        let file = file?;
        let name = file.file_name();
        if name == *"compiletest-ignore-dir" {
            return Ok(());
        }
        if name == *"Makefile" && config.mode == Mode::RunMake {
            let paths = TestPaths {
                file: dir.to_path_buf(),
                base: base.to_path_buf(),
                relative_dir: relative_dir_path.parent().unwrap().to_path_buf(),
            };
            tests.push(make_test(config, &paths));
            return Ok(())
        }
    }

    // If we find a test foo/bar.rs, we have to build the
    // output directory `$build/foo` so we can write
    // `$build/foo/bar` into it. We do this *now* in this
    // sequential loop because otherwise, if we do it in the
    // tests themselves, they race for the privilege of
    // creating the directories and sometimes fail randomly.
    let build_dir = config.build_base.join(&relative_dir_path);
    fs::create_dir_all(&build_dir).unwrap();

    // Add each `.rs` file as a test, and recurse further on any
    // subdirectories we find, except for `aux` directories.
    let dirs = fs::read_dir(dir)?;
    for file in dirs {
        let file = file?;
        let file_path = file.path();
        let file_name = file.file_name();
        if is_test(&file_name) {
            debug!("found test file: {:?}", file_path.display());
            let paths = TestPaths {
                file: file_path,
                base: base.to_path_buf(),
                relative_dir: relative_dir_path.to_path_buf(),
            };
            tests.push(make_test(config, &paths))
        } else if file_path.is_dir() {
            let relative_file_path = relative_dir_path.join(file.file_name());
            if &file_name == "auxiliary" {
                // `aux` directories contain other crates used for
                // cross-crate tests. Don't search them for tests, but
                // do create a directory in the build dir for them,
                // since we will dump intermediate output in there
                // sometimes.
                let build_dir = config.build_base.join(&relative_file_path);
                fs::create_dir_all(&build_dir).unwrap();
            } else {
                debug!("found directory: {:?}", file_path.display());
                collect_tests_from_dir(config,
                                       base,
                                       &file_path,
                                       &relative_file_path,
                                       tests)?;
            }
        } else {
            debug!("found other file/directory: {:?}", file_path.display());
        }
    }
    Ok(())
}

pub fn is_test(file_name: &OsString) -> bool {
    let file_name = file_name.to_str().unwrap();

    if !file_name.ends_with(".rs") {
        return false;
    }

    // `.`, `#`, and `~` are common temp-file prefixes.
    let invalid_prefixes = &[".", "#", "~"];
    !invalid_prefixes.iter().any(|p| file_name.starts_with(p))
}

pub fn make_test(config: &Config, testpaths: &TestPaths) -> test::TestDescAndFn {
    let early_props = EarlyProps::from_file(config, &testpaths.file);

    // The `should-fail` annotation doesn't apply to pretty tests,
    // since we run the pretty printer across all tests by default.
    // If desired, we could add a `should-fail-pretty` annotation.
    let should_panic = match config.mode {
        Pretty => test::ShouldPanic::No,
        _ => if early_props.should_fail {
            test::ShouldPanic::Yes
        } else {
            test::ShouldPanic::No
        }
    };

    // Debugging emscripten code doesn't make sense today
    let mut ignore = early_props.ignore || !up_to_date(config, testpaths, &early_props);
    if (config.mode == DebugInfoGdb || config.mode == DebugInfoLldb) &&
        config.target.contains("emscripten") {
        ignore = true;
    }

    test::TestDescAndFn {
        desc: test::TestDesc {
            name: make_test_name(config, testpaths),
            ignore: ignore,
            should_panic: should_panic,
        },
        testfn: make_test_closure(config, testpaths),
    }
}

fn stamp(config: &Config, testpaths: &TestPaths) -> PathBuf {
    let stamp_name = format!("{}-H-{}-T-{}-S-{}.stamp",
                             testpaths.file.file_name().unwrap()
                                           .to_str().unwrap(),
                             config.host,
                             config.target,
                             config.stage_id);
    config.build_base.canonicalize()
          .unwrap_or(config.build_base.clone())
          .join(stamp_name)
}

fn up_to_date(config: &Config, testpaths: &TestPaths, props: &EarlyProps) -> bool {
    let stamp = mtime(&stamp(config, testpaths));
    let mut inputs = vec![
        mtime(&testpaths.file),
        mtime(&config.rustc_path),
    ];
    for aux in props.aux.iter() {
        inputs.push(mtime(&testpaths.file.parent().unwrap()
                                         .join("auxiliary")
                                         .join(aux)));
    }
    if let Ok(dir) = config.run_lib_path.read_dir() {
        for lib in dir {
            let lib = lib.unwrap();
            inputs.push(mtime(&lib.path()));
        }
    }
    inputs.iter().any(|input| *input > stamp)
}

fn mtime(path: &Path) -> FileTime {
    fs::metadata(path).map(|f| {
        FileTime::from_last_modification_time(&f)
    }).unwrap_or(FileTime::zero())
}

pub fn make_test_name(config: &Config, testpaths: &TestPaths) -> test::TestName {
    // Convert a complete path to something like
    //
    //    run-pass/foo/bar/baz.rs
    let path =
        PathBuf::from(config.mode.to_string())
        .join(&testpaths.relative_dir)
        .join(&testpaths.file.file_name().unwrap());
    test::DynTestName(format!("[{}] {}", config.mode, path.display()))
}

pub fn make_test_closure(config: &Config, testpaths: &TestPaths) -> test::TestFn {
    let config = config.clone();
    let testpaths = testpaths.clone();
    test::DynTestFn(Box::new(move |()| {
        runtest::run(config, &testpaths)
    }))
}

/// Returns (Path to GDB, GDB Version, GDB has Rust Support)
fn analyze_gdb(gdb: Option<String>) -> (Option<String>, Option<u32>, bool) {
    #[cfg(not(windows))]
    const GDB_FALLBACK: &str = "gdb";
    #[cfg(windows)]
    const GDB_FALLBACK: &str = "gdb.exe";

    const MIN_GDB_WITH_RUST: u32 = 7011010;

    let gdb = match gdb {
        None => GDB_FALLBACK,
        Some(ref s) if s.is_empty() => GDB_FALLBACK, // may be empty if configure found no gdb
        Some(ref s) => s,
    };

    let version_line = Command::new(gdb).arg("--version").output().map(|output| {
        String::from_utf8_lossy(&output.stdout).lines().next().unwrap().to_string()
    }).ok();

    let version = match version_line {
        Some(line) => extract_gdb_version(&line),
        None => return (None, None, false),
    };

    let gdb_native_rust = version.map_or(false, |v| v >= MIN_GDB_WITH_RUST);

    return (Some(gdb.to_owned()), version, gdb_native_rust);
}

fn extract_gdb_version(full_version_line: &str) -> Option<u32> {
    let full_version_line = full_version_line.trim();

    // GDB versions look like this: "major.minor.patch?.yyyymmdd?", with both
    // of the ? sections being optional

    // We will parse up to 3 digits for minor and patch, ignoring the date
    // We limit major to 1 digit, otherwise, on openSUSE, we parse the openSUSE version

    // don't start parsing in the middle of a number
    let mut prev_was_digit = false;
    for (pos, c) in full_version_line.char_indices() {
        if prev_was_digit || !c.is_digit(10) {
            prev_was_digit = c.is_digit(10);
            continue
        }

        prev_was_digit = true;

        let line = &full_version_line[pos..];

        let next_split = match line.find(|c: char| !c.is_digit(10)) {
            Some(idx) => idx,
            None => continue, // no minor version
        };

        if line.as_bytes()[next_split] != b'.' {
            continue; // no minor version
        }

        let major = &line[..next_split];
        let line = &line[next_split + 1..];

        let (minor, patch) = match line.find(|c: char| !c.is_digit(10)) {
            Some(idx) => if line.as_bytes()[idx] == b'.' {
                let patch = &line[idx + 1..];

                let patch_len = patch.find(|c: char| !c.is_digit(10)).unwrap_or(patch.len());
                let patch = &patch[..patch_len];
                let patch = if patch_len > 3 || patch_len == 0 { None } else { Some(patch) };

                (&line[..idx], patch)
            } else {
                (&line[..idx], None)
            },
            None => (line, None),
        };

        if major.len() != 1 || minor.is_empty() {
            continue;
        }

        let major: u32 = major.parse().unwrap();
        let minor: u32 = minor.parse().unwrap();
        let patch: u32 = patch.unwrap_or("0").parse().unwrap();

        return Some(((major * 1000) + minor) * 1000 + patch);
    }

    None
}

fn extract_lldb_version(full_version_line: Option<String>) -> Option<String> {
    // Extract the major LLDB version from the given version string.
    // LLDB version strings are different for Apple and non-Apple platforms.
    // At the moment, this function only supports the Apple variant, which looks
    // like this:
    //
    // LLDB-179.5 (older versions)
    // lldb-300.2.51 (new versions)
    //
    // We are only interested in the major version number, so this function
    // will return `Some("179")` and `Some("300")` respectively.

    if let Some(ref full_version_line) = full_version_line {
        if !full_version_line.trim().is_empty() {
            let full_version_line = full_version_line.trim();

            for (pos, l) in full_version_line.char_indices() {
                if l != 'l' && l != 'L' { continue }
                if pos + 5 >= full_version_line.len() { continue }
                let l = full_version_line[pos + 1..].chars().next().unwrap();
                if l != 'l' && l != 'L' { continue }
                let d = full_version_line[pos + 2..].chars().next().unwrap();
                if d != 'd' && d != 'D' { continue }
                let b = full_version_line[pos + 3..].chars().next().unwrap();
                if b != 'b' && b != 'B' { continue }
                let dash = full_version_line[pos + 4..].chars().next().unwrap();
                if dash != '-' { continue }

                let vers = full_version_line[pos + 5..].chars().take_while(|c| {
                    c.is_digit(10)
                }).collect::<String>();
                if !vers.is_empty() { return Some(vers) }
            }
        }
    }
    None
}

fn is_blacklisted_lldb_version(version: &str) -> bool {
    version == "350"
}

#[test]
fn test_extract_gdb_version() {
    macro_rules! test { ($($expectation:tt: $input:tt,)*) => {{$(
        assert_eq!(extract_gdb_version($input), Some($expectation));
    )*}}}

    test! {
        7000001: "GNU gdb (GDB) CentOS (7.0.1-45.el5.centos)",

        7002000: "GNU gdb (GDB) Red Hat Enterprise Linux (7.2-90.el6)",

        7004000: "GNU gdb (Ubuntu/Linaro 7.4-2012.04-0ubuntu2.1) 7.4-2012.04",
        7004001: "GNU gdb (GDB) 7.4.1-debian",

        7006001: "GNU gdb (GDB) Red Hat Enterprise Linux 7.6.1-80.el7",

        7007001: "GNU gdb (Ubuntu 7.7.1-0ubuntu5~14.04.2) 7.7.1",
        7007001: "GNU gdb (Debian 7.7.1+dfsg-5) 7.7.1",
        7007001: "GNU gdb (GDB) Fedora 7.7.1-21.fc20",

        7008000: "GNU gdb (GDB; openSUSE 13.2) 7.8",
        7009001: "GNU gdb (GDB) Fedora 7.9.1-20.fc22",
        7010001: "GNU gdb (GDB) Fedora 7.10.1-31.fc23",

        7011000: "GNU gdb (Ubuntu 7.11-0ubuntu1) 7.11",
        7011001: "GNU gdb (Ubuntu 7.11.1-0ubuntu1~16.04) 7.11.1",
        7011001: "GNU gdb (Debian 7.11.1-2) 7.11.1",
        7011001: "GNU gdb (GDB) Fedora 7.11.1-86.fc24",
        7011001: "GNU gdb (GDB; openSUSE Leap 42.1) 7.11.1",
        7011001: "GNU gdb (GDB; openSUSE Tumbleweed) 7.11.1",

        7011090: "7.11.90",
        7011090: "GNU gdb (Ubuntu 7.11.90.20161005-0ubuntu1) 7.11.90.20161005-git",

        7012000: "7.12",
        7012000: "GNU gdb (GDB) 7.12",
        7012000: "GNU gdb (GDB) 7.12.20161027-git",
        7012050: "GNU gdb (GDB) 7.12.50.20161027-git",
    }
}
