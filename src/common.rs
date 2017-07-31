// Copyright 2012-2014 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.
pub use self::Mode::*;

use std::env;
use std::fmt;
use std::mem;
use std::str::FromStr;
use std::path::PathBuf;
use rustc;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    CompileFail,
    ParseFail,
    RunFail,
    RunPass,
    RunPassValgrind,
    Pretty,
    DebugInfoGdb,
    DebugInfoLldb,
    Codegen,
    Rustdoc,
    CodegenUnits,
    Incremental,
    RunMake,
    Ui,
    MirOpt,
}

impl FromStr for Mode {
    type Err = ();
    fn from_str(s: &str) -> Result<Mode, ()> {
        match s {
            "compile-fail" => Ok(CompileFail),
            "parse-fail" => Ok(ParseFail),
            "run-fail" => Ok(RunFail),
            "run-pass" => Ok(RunPass),
            "run-pass-valgrind" => Ok(RunPassValgrind),
            "pretty" => Ok(Pretty),
            "debuginfo-lldb" => Ok(DebugInfoLldb),
            "debuginfo-gdb" => Ok(DebugInfoGdb),
            "codegen" => Ok(Codegen),
            "rustdoc" => Ok(Rustdoc),
            "codegen-units" => Ok(CodegenUnits),
            "incremental" => Ok(Incremental),
            "run-make" => Ok(RunMake),
            "ui" => Ok(Ui),
            "mir-opt" => Ok(MirOpt),
            _ => Err(()),
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(match *self {
                              CompileFail => "compile-fail",
                              ParseFail => "parse-fail",
                              RunFail => "run-fail",
                              RunPass => "run-pass",
                              RunPassValgrind => "run-pass-valgrind",
                              Pretty => "pretty",
                              DebugInfoGdb => "debuginfo-gdb",
                              DebugInfoLldb => "debuginfo-lldb",
                              Codegen => "codegen",
                              Rustdoc => "rustdoc",
                              CodegenUnits => "codegen-units",
                              Incremental => "incremental",
                              RunMake => "run-make",
                              Ui => "ui",
                              MirOpt => "mir-opt",
                          },
                          f)
    }
}

#[derive(Clone)]
pub struct Config {
    // The library paths required for running the compiler
    pub compile_lib_path: PathBuf,

    // The library paths required for running compiled programs
    pub run_lib_path: PathBuf,

    // The rustc executable
    pub rustc_path: PathBuf,

    // The rustdoc executable
    pub rustdoc_path: PathBuf,

    // The python executable to use for LLDB
    pub lldb_python: String,

    // The python executable to use for htmldocck
    pub docck_python: String,

    // The llvm FileCheck binary path
    pub llvm_filecheck: Option<PathBuf>,

    // The valgrind path
    pub valgrind_path: Option<String>,

    // Whether to fail if we can't run run-pass-valgrind tests under valgrind
    // (or, alternatively, to silently run them like regular run-pass tests).
    pub force_valgrind: bool,

    // The directory containing the tests to run
    pub src_base: PathBuf,

    // The directory where programs should be built
    pub build_base: PathBuf,

    // The name of the stage being built (stage1, etc)
    pub stage_id: String,

    // The test mode, compile-fail, run-fail, run-pass
    pub mode: Mode,

    // Run ignored tests
    pub run_ignored: bool,

    // Only run tests that match this filter
    pub filter: Option<String>,

    // Exactly match the filter, rather than a substring
    pub filter_exact: bool,

    // Write out a parseable log of tests that were run
    pub logfile: Option<PathBuf>,

    // A command line to prefix program execution with,
    // for running under valgrind
    pub runtool: Option<String>,

    // Flags to pass to the compiler when building for the host
    pub host_rustcflags: Option<String>,

    // Flags to pass to the compiler when building for the target
    pub target_rustcflags: Option<String>,

    // Target system to be tested
    pub target: String,

    // Host triple for the compiler being invoked
    pub host: String,

    // Version of GDB
    pub gdb_version: Option<String>,

    // Version of LLDB
    pub lldb_version: Option<String>,

    // Version of LLVM
    pub llvm_version: Option<String>,

    // Path to the android tools
    pub android_cross_path: PathBuf,

    // Extra parameter to run adb on arm-linux-androideabi
    pub adb_path: String,

    // Extra parameter to run test suite on arm-linux-androideabi
    pub adb_test_dir: String,

    // status whether android device available or not
    pub adb_device_status: bool,

    // the path containing LLDB's Python module
    pub lldb_python_dir: Option<String>,

    // Explain what's going on
    pub verbose: bool,

    // Print one character per test instead of one line
    pub quiet: bool,

    // Configuration for various run-make tests frobbing things like C compilers
    // or querying about various LLVM component information.
    pub cc: String,
    pub cxx: String,
    pub cflags: String,
    pub llvm_components: String,
    pub llvm_cxxflags: String,
    pub nodejs: Option<String>,
}

impl Config {

    /// Add rustc flags to link with the crate's dependencies in addition to the crate itself
    pub fn link_deps(&mut self) {
        // The linked library path env var name depends on the target OS. I'm not certain if this
        // covers all the cases adequately.
        let (varname, path_delim) = if cfg!(any(target_os = "macos", target_os = "ios")) {
            ("DYLD_LIBRARY_PATH", ":")
        } else if cfg!(target_os = "windows") {
            ("PATH", ";")
        } else {
            ("LD_LIBRARY_PATH", ":")
        };

        // Dependencies can be found in the environment variable. Throw everything there into the
        // link flags
        let lib_paths = env::var(varname).unwrap_or_else(|e| {
            panic!("Cannot link to dependencies. Problem with env var '{}': {:?}", varname, e)
        });

        // Move the flags out so we can mutate them and put them back in
        let flags_opt = mem::replace(&mut self.target_rustcflags, None);

        // Append to current flags if any are set, otherwise make new String
        let mut flags = flags_opt.unwrap_or(String::new());
        for p in lib_paths.split(path_delim) {
            flags += &*format!(" -L {} ", p);
        }

        self.target_rustcflags = Some(flags);
    }
}

impl Default for Config {
    fn default() -> Config {
        let platform = rustc::session::config::host_triple().to_string();

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
            target: platform.clone(),
            host: platform.clone(),
            gdb_version: None,
            lldb_version: None,
            llvm_version: None,
            android_cross_path: PathBuf::from("android-cross-path"),
            adb_path: "adb-path".to_owned(),
            adb_test_dir: "adb-test-dir/target".to_owned(),
            adb_device_status: false,
            lldb_python_dir: None,
            verbose: false,
            quiet: false,
            cc: "cc".to_string(),
            cxx: "cxx".to_string(),
            cflags: "cflags".to_string(),
            llvm_components: "llvm-components".to_string(),
            llvm_cxxflags: "llvm-cxxflags".to_string(),
            nodejs: None,
        }
    }
}
