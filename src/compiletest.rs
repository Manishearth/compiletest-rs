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
#![feature(unboxed_closures)]
#![feature(test)]
#![feature(path_ext)]
#![feature(str_char)]
#![feature(dynamic_lib)]
#![feature(vec_push_all)]

#![deny(warnings)]
#![deny(unused_imports)]

extern crate test;
extern crate rustc;

#[macro_use]
extern crate log;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use common::{Config, Mode};
use common::{Pretty, DebugInfoGdb, DebugInfoLldb, Codegen};
use std::borrow::ToOwned;
use rustc::session::config::host_triple;

pub mod procsrv;
pub mod util;
pub mod header;
pub mod runtest;
pub mod common;
pub mod errors;

#[cfg(target_os = "macos")]
static LD_LIBRARY_PATH: &'static str = env!("DYLD_LIBRARY_PATH");

#[cfg(not(target_os = "macos"))]
static LD_LIBRARY_PATH: &'static str = env!("LD_LIBRARY_PATH");

pub fn default_config() -> Config {
    Config {
        compile_lib_path: LD_LIBRARY_PATH.to_owned(),
        run_lib_path: LD_LIBRARY_PATH.to_owned(),
        rustc_path: PathBuf::from("rustc"),
        clang_path: None,
        valgrind_path: None,
        force_valgrind: false,
        llvm_bin_path: None,
        src_base: PathBuf::from("tests/run-pass"),
        build_base: PathBuf::from("/tmp"),
        aux_base: None,
        stage_id: "stage3".to_owned(),
        mode: Mode::RunPass,
        run_ignored: false,
        filter: None,
        logfile: None,
        runtool: None,
        host_rustcflags: None,
        target_rustcflags: None,
        jit: false,
        target: host_triple().to_owned(),
        host: "(none)".to_owned(),
        gdb_version: None,
        lldb_version: None,
        android: None,
        lldb_python_dir: None,
        verbose: false
    }
}

pub fn run_tests(config: &Config) {
    if config.target.contains("android") {
        if config.mode == DebugInfoGdb {
            println!("{} debug-info test uses tcp 5039 port.\
                         please reserve it", config.target);
        }

        // android debug-info test uses remote debugger
        // so, we test 1 task at once.
        // also trying to isolate problems with adb_run_wrapper.sh ilooping
        env::set_var("RUST_TEST_TASKS","1");
    }

    match config.mode {
        DebugInfoLldb => {
            // Some older versions of LLDB seem to have problems with multiple
            // instances running in parallel, so only run one test task at a
            // time.
            env::set_var("RUST_TEST_TASKS", "1");
        }
        _ => { /* proceed */ }
    }

    let opts = test_opts(config);
    let tests = make_tests(config);
    // sadly osx needs some file descriptor limits raised for running tests in
    // parallel (especially when we have lots and lots of child processes).
    // For context, see #8904
    // #[allow(deprecated)]
    // fn raise_fd_limit() {
    //     std::old_io::test::raise_fd_limit();
    // }
    // raise_fd_limit();
    // Prevent issue #21352 UAC blocking .exe containing 'patch' etc. on Windows
    // If #11207 is resolved (adding manifest to .exe) this becomes unnecessary
    env::set_var("__COMPAT_LAYER", "RunAsInvoker");
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
        filter: match config.filter {
            None => None,
            Some(ref filter) => Some(filter.clone()),
        },
        run_ignored: config.run_ignored,
        logfile: config.logfile.clone(),
        run_tests: true,
        bench_benchmarks: true,
        nocapture: env::var("RUST_TEST_NOCAPTURE").is_ok(),
        color: test::AutoColor,
    }
}

pub fn make_tests(config: &Config) -> Vec<test::TestDescAndFn> {
    debug!("making tests from {:?}",
           config.src_base.display());
    let mut tests = Vec::new();
    let dirs = fs::read_dir(&config.src_base).unwrap();
    for file in dirs {
        let file = file.unwrap().path();
        debug!("inspecting file {:?}", file.display());
        if is_test(config, &file) {
            let t = make_test(config, &file, || {
                match config.mode {
                    Codegen => make_metrics_test_closure(config, &file),
                    _ => make_test_closure(config, &file)
                }
            });
            tests.push(t)
        }
    }
    tests
}

pub fn is_test(config: &Config, testfile: &Path) -> bool {
    // Pretty-printer does not work with .rc files yet
    let valid_extensions =
        match config.mode {
          Pretty => vec!(".rs".to_owned()),
          _ => vec!(".rc".to_owned(), ".rs".to_owned())
        };

    let invalid_prefixes = vec!(".".to_owned(), "#".to_owned(), "~".to_owned());
    let name = testfile.file_name().unwrap().to_str().unwrap();

    valid_extensions.iter().any(|ext| name.ends_with(ext)) &&
        !invalid_prefixes.iter().any(|pre| name.starts_with(pre))
}

pub fn make_test<F>(config: &Config, testfile: &Path, f: F) -> test::TestDescAndFn where
    F: FnOnce() -> test::TestFn,
{
    test::TestDescAndFn {
        desc: test::TestDesc {
            name: make_test_name(config, testfile),
            ignore: header::is_test_ignored(config, testfile),
            should_panic: test::ShouldPanic::No,
        },
        testfn: f(),
    }
}

pub fn make_test_name(config: &Config, testfile: &Path) -> test::TestName {

    // Try to elide redundant long paths
    fn shorten(path: &Path) -> String {
        let filename = path.file_name().unwrap().to_str();
        let p = path.parent().unwrap();
        let dir = p.file_name().unwrap().to_str();
        format!("{}/{}", dir.unwrap_or(""), filename.unwrap_or(""))
    }

    test::DynTestName(format!("[{}] {}", config.mode, shorten(testfile)))
}

pub fn make_test_closure(config: &Config, testfile: &Path) -> test::TestFn {
    let config = (*config).clone();
    let testfile = testfile.to_path_buf();
    test::DynTestFn(Box::new(move || {
        runtest::run(config, &testfile)
    }))
}

pub fn make_metrics_test_closure(config: &Config, testfile: &Path) -> test::TestFn {
    let config = (*config).clone();
    let testfile = testfile.to_path_buf();
    test::DynMetricFn(box move |mm: &mut test::MetricMap| {
        runtest::run_metrics(config, &testfile, mm)
    })
}

#[allow(dead_code)]
fn extract_gdb_version(full_version_line: Option<String>) -> Option<String> {
    match full_version_line {
        Some(ref full_version_line)
          if full_version_line.trim().len() > 0 => {
            let full_version_line = full_version_line.trim();

            // used to be a regex "(^|[^0-9])([0-9]\.[0-9])([^0-9]|$)"
            for (pos, c) in full_version_line.char_indices() {
                if !c.is_digit(10) { continue }
                if pos + 2 >= full_version_line.len() { continue }
                if full_version_line.char_at(pos + 1) != '.' { continue }
                if !full_version_line.char_at(pos + 2).is_digit(10) { continue }
                if pos > 0 && full_version_line.char_at_reverse(pos).is_digit(10) {
                    continue
                }
                if pos + 3 < full_version_line.len() &&
                   full_version_line.char_at(pos + 3).is_digit(10) {
                    continue
                }
                return Some(full_version_line[pos..pos+3].to_owned());
            }
            println!("Could not extract GDB version from line '{}'",
                     full_version_line);
            None
        },
        _ => None
    }
}

#[allow(dead_code)]
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

    match full_version_line {
        Some(ref full_version_line)
          if full_version_line.trim().len() > 0 => {
            let full_version_line = full_version_line.trim();

            for (pos, l) in full_version_line.char_indices() {
                if l != 'l' && l != 'L' { continue }
                if pos + 5 >= full_version_line.len() { continue }
                let l = full_version_line.char_at(pos + 1);
                if l != 'l' && l != 'L' { continue }
                let d = full_version_line.char_at(pos + 2);
                if d != 'd' && d != 'D' { continue }
                let b = full_version_line.char_at(pos + 3);
                if b != 'b' && b != 'B' { continue }
                let dash = full_version_line.char_at(pos + 4);
                if dash != '-' { continue }

                let vers = full_version_line[pos + 5..].chars().take_while(|c| {
                    c.is_digit(10)
                }).collect::<String>();
                if vers.len() > 0 { return Some(vers) }
            }
            println!("Could not extract LLDB version from line '{}'",
                     full_version_line);
            None
        },
        _ => None
    }
}
