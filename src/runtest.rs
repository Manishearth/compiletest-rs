// Copyright 2012-2014 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use common::Config;
use common::{CompileFail, ParseFail, Pretty, RunFail, RunPass, RunPassValgrind};
use common::{Codegen, DebugInfoLldb, DebugInfoGdb, Rustdoc, CodegenUnits};
use common::{Incremental, RunMake, Ui, MirOpt};
use diff;
use errors::{self, ErrorKind, Error};
use filetime::FileTime;
use json;
use header::TestProps;
use test::TestPaths;
use util::logv;

use std::collections::HashMap;
use std::collections::HashSet;
use std::env;
use std::ffi::OsString;
use std::fs::{self, File, create_dir_all};
use std::io::prelude::*;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, ExitStatus, Stdio};
use std::str;

use extract_gdb_version;

/// The name of the environment variable that holds dynamic library locations.
pub fn dylib_env_var() -> &'static str {
    if cfg!(windows) {
        "PATH"
    } else if cfg!(target_os = "macos") {
        "DYLD_LIBRARY_PATH"
    } else if cfg!(target_os = "haiku") {
        "LIBRARY_PATH"
    } else {
        "LD_LIBRARY_PATH"
    }
}

pub fn run(config: Config, testpaths: &TestPaths) {
    match &*config.target {

        "arm-linux-androideabi" | "armv7-linux-androideabi" | "aarch64-linux-android" => {
            if !config.adb_device_status {
                panic!("android device not available");
            }
        }

        _ => {
            // android has its own gdb handling
            if config.mode == DebugInfoGdb && config.gdb.is_none() {
                panic!("gdb not available but debuginfo gdb debuginfo test requested");
            }
        }
    }

    if config.verbose {
        // We're going to be dumping a lot of info. Start on a new line.
        print!("\n\n");
    }
    debug!("running {:?}", testpaths.file.display());
    let base_props = TestProps::from_file(&testpaths.file, &config);

    let base_cx = TestCx { config: &config,
                           props: &base_props,
                           testpaths,
                           revision: None };
    base_cx.init_all();

    if base_props.revisions.is_empty() {
        base_cx.run_revision()
    } else {
        for revision in &base_props.revisions {
            let mut revision_props = base_props.clone();
            revision_props.load_from(&testpaths.file, Some(revision), &config);
            let rev_cx = TestCx {
                config: &config,
                props: &revision_props,
                testpaths,
                revision: Some(revision)
            };
            rev_cx.run_revision();
        }
    }

    base_cx.complete_all();

    File::create(::stamp(&config, testpaths)).unwrap();
}

struct TestCx<'test> {
    config: &'test Config,
    props: &'test TestProps,
    testpaths: &'test TestPaths,
    revision: Option<&'test str>
}

struct DebuggerCommands {
    commands: Vec<String>,
    check_lines: Vec<String>,
    breakpoint_lines: Vec<usize>,
}

impl<'test> TestCx<'test> {
    /// invoked once before any revisions have been processed
    fn init_all(&self) {
        assert!(self.revision.is_none(), "init_all invoked for a revision");
        if let Incremental = self.config.mode {
            self.init_incremental_test()
        }
    }

    /// Code executed for each revision in turn (or, if there are no
    /// revisions, exactly once, with revision == None).
    fn run_revision(&self) {
        match self.config.mode {
            CompileFail |
            ParseFail => self.run_cfail_test(),
            RunFail => self.run_rfail_test(),
            RunPass => self.run_rpass_test(),
            RunPassValgrind => self.run_valgrind_test(),
            Pretty => self.run_pretty_test(),
            DebugInfoGdb => self.run_debuginfo_gdb_test(),
            DebugInfoLldb => self.run_debuginfo_lldb_test(),
            Codegen => self.run_codegen_test(),
            Rustdoc => self.run_rustdoc_test(),
            CodegenUnits => self.run_codegen_units_test(),
            Incremental => self.run_incremental_test(),
            RunMake => self.run_rmake_test(),
            Ui => self.run_ui_test(),
            MirOpt => self.run_mir_opt_test(),
        }
    }

    /// Invoked after all revisions have executed.
    fn complete_all(&self) {
        assert!(self.revision.is_none(), "init_all invoked for a revision");
    }

    fn run_cfail_test(&self) {
        let proc_res = self.compile_test();

        if self.props.must_compile_successfully {
            if !proc_res.status.success() {
                self.fatal_proc_rec(
                    "test compilation failed although it shouldn't!",
                    &proc_res);
            }
        } else {
            if proc_res.status.success() {
                self.fatal_proc_rec(
                    &format!("{} test compiled successfully!", self.config.mode)[..],
                    &proc_res);
            }

            self.check_correct_failure_status(&proc_res);
        }

        let output_to_check = self.get_output(&proc_res);
        let expected_errors = errors::load_errors(&self.testpaths.file, self.revision);
        if !expected_errors.is_empty() {
            if !self.props.error_patterns.is_empty() {
                self.fatal("both error pattern and expected errors specified");
            }
            self.check_expected_errors(expected_errors, &proc_res);
        } else {
            self.check_error_patterns(&output_to_check, &proc_res);
        }

        self.check_no_compiler_crash(&proc_res);
        self.check_forbid_output(&output_to_check, &proc_res);
    }

    fn run_rfail_test(&self) {
        let proc_res = self.compile_test();

        if !proc_res.status.success() {
            self.fatal_proc_rec("compilation failed!", &proc_res);
        }

        let proc_res = self.exec_compiled_test();

        // The value our Makefile configures valgrind to return on failure
        const VALGRIND_ERR: i32 = 100;
        if proc_res.status.code() == Some(VALGRIND_ERR) {
            self.fatal_proc_rec("run-fail test isn't valgrind-clean!", &proc_res);
        }

        let output_to_check = self.get_output(&proc_res);
        self.check_correct_failure_status(&proc_res);
        self.check_error_patterns(&output_to_check, &proc_res);
    }

    fn get_output(&self, proc_res: &ProcRes) -> String {
        if self.props.check_stdout {
            format!("{}{}", proc_res.stdout, proc_res.stderr)
        } else {
            proc_res.stderr.clone()
        }
    }

    fn check_correct_failure_status(&self, proc_res: &ProcRes) {
        // The value the rust runtime returns on failure
        const RUST_ERR: i32 = 101;
        if proc_res.status.code() != Some(RUST_ERR) {
            self.fatal_proc_rec(
                &format!("failure produced the wrong error: {}",
                         proc_res.status),
                proc_res);
        }
    }

    fn run_rpass_test(&self) {
        let proc_res = self.compile_test();

        if !proc_res.status.success() {
            self.fatal_proc_rec("compilation failed!", &proc_res);
        }

        // FIXME(#41968): Move this check to tidy?
        let expected_errors = errors::load_errors(&self.testpaths.file, self.revision);
        assert!(expected_errors.is_empty(),
                "run-pass tests with expected warnings should be moved to ui/");

        let proc_res = self.exec_compiled_test();

        if !proc_res.status.success() {
            self.fatal_proc_rec("test run failed!", &proc_res);
        }
    }

    fn run_valgrind_test(&self) {
        assert!(self.revision.is_none(), "revisions not relevant here");

        if self.config.valgrind_path.is_none() {
            assert!(!self.config.force_valgrind);
            return self.run_rpass_test();
        }

        let mut proc_res = self.compile_test();

        if !proc_res.status.success() {
            self.fatal_proc_rec("compilation failed!", &proc_res);
        }

        let mut new_config = self.config.clone();
        new_config.runtool = new_config.valgrind_path.clone();
        let new_cx = TestCx { config: &new_config, ..*self };
        proc_res = new_cx.exec_compiled_test();

        if !proc_res.status.success() {
            self.fatal_proc_rec("test run failed!", &proc_res);
        }
    }

    fn run_pretty_test(&self) {
        if self.props.pp_exact.is_some() {
            logv(self.config, "testing for exact pretty-printing".to_owned());
        } else {
            logv(self.config, "testing for converging pretty-printing".to_owned());
        }

        let rounds = match self.props.pp_exact { Some(_) => 1, None => 2 };

        let mut src = String::new();
        File::open(&self.testpaths.file).unwrap().read_to_string(&mut src).unwrap();
        let mut srcs = vec![src];

        let mut round = 0;
        while round < rounds {
            logv(self.config, format!("pretty-printing round {} revision {:?}",
                                      round, self.revision));
            let proc_res = self.print_source(srcs[round].to_owned(), &self.props.pretty_mode);

            if !proc_res.status.success() {
                self.fatal_proc_rec(&format!("pretty-printing failed in round {} revision {:?}",
                                             round, self.revision),
                                    &proc_res);
            }

            let ProcRes{ stdout, .. } = proc_res;
            srcs.push(stdout);
            round += 1;
        }

        let mut expected = match self.props.pp_exact {
            Some(ref file) => {
                let filepath = self.testpaths.file.parent().unwrap().join(file);
                let mut s = String::new();
                File::open(&filepath).unwrap().read_to_string(&mut s).unwrap();
                s
            }
            None => { srcs[srcs.len() - 2].clone() }
        };
        let mut actual = srcs[srcs.len() - 1].clone();

        if self.props.pp_exact.is_some() {
            // Now we have to care about line endings
            let cr = "\r".to_owned();
            actual = actual.replace(&cr, "").to_owned();
            expected = expected.replace(&cr, "").to_owned();
        }

        self.compare_source(&expected, &actual);

        // If we're only making sure that the output matches then just stop here
        if self.props.pretty_compare_only { return; }

        // Finally, let's make sure it actually appears to remain valid code
        let proc_res = self.typecheck_source(actual);
        if !proc_res.status.success() {
            self.fatal_proc_rec("pretty-printed source does not typecheck", &proc_res);
        }

        if !self.props.pretty_expanded { return }

        // additionally, run `--pretty expanded` and try to build it.
        let proc_res = self.print_source(srcs[round].clone(), "expanded");
        if !proc_res.status.success() {
            self.fatal_proc_rec("pretty-printing (expanded) failed", &proc_res);
        }

        let ProcRes{ stdout: expanded_src, .. } = proc_res;
        let proc_res = self.typecheck_source(expanded_src);
        if !proc_res.status.success() {
            self.fatal_proc_rec(
                "pretty-printed source (expanded) does not typecheck",
                &proc_res);
        }
    }

    fn print_source(&self, src: String, pretty_type: &str) -> ProcRes {
        let aux_dir = self.aux_output_dir_name();

        let mut rustc = Command::new(&self.config.rustc_path);
        rustc.arg("-")
            .arg("-Zunstable-options")
            .args(&["--unpretty", &pretty_type])
            .args(&["--target", &self.config.target])
            .arg("-L").arg(&aux_dir)
            .args(self.split_maybe_args(&self.config.target_rustcflags))
            .args(&self.props.compile_flags)
            .envs(self.props.exec_env.clone());

        self.compose_and_run(rustc,
                             self.config.compile_lib_path.to_str().unwrap(),
                             Some(aux_dir.to_str().unwrap()),
                             Some(src))
    }

    fn compare_source(&self,
                      expected: &str,
                      actual: &str) {
        if expected != actual {
            self.error("pretty-printed source does not match expected source");
            println!("\n\
expected:\n\
------------------------------------------\n\
{}\n\
------------------------------------------\n\
actual:\n\
------------------------------------------\n\
{}\n\
------------------------------------------\n\
\n",
                     expected, actual);
            panic!();
        }
    }

    fn typecheck_source(&self, src: String) -> ProcRes {
        let mut rustc = Command::new(&self.config.rustc_path);

        let out_dir = self.output_base_name().with_extension("pretty-out");
        let _ = fs::remove_dir_all(&out_dir);
        create_dir_all(&out_dir).unwrap();

        let target = if self.props.force_host {
            &*self.config.host
        } else {
            &*self.config.target
        };

        let aux_dir = self.aux_output_dir_name();

        rustc.arg("-")
            .arg("-Zno-trans")
            .arg("--out-dir").arg(&out_dir)
            .arg(&format!("--target={}", target))
            .arg("-L").arg(&self.config.build_base)
            .arg("-L").arg(aux_dir);

        if let Some(revision) = self.revision {
            rustc.args(&["--cfg", revision]);
        }

        rustc.args(self.split_maybe_args(&self.config.target_rustcflags));
        rustc.args(&self.props.compile_flags);

        self.compose_and_run_compiler(rustc, Some(src))
    }

    fn run_debuginfo_gdb_test(&self) {
        assert!(self.revision.is_none(), "revisions not relevant here");

        let config = Config {
            target_rustcflags: self.cleanup_debug_info_options(&self.config.target_rustcflags),
            host_rustcflags: self.cleanup_debug_info_options(&self.config.host_rustcflags),
            .. self.config.clone()
        };

        let test_cx = TestCx {
            config: &config,
            ..*self
        };

        test_cx.run_debuginfo_gdb_test_no_opt();
    }

    fn run_debuginfo_gdb_test_no_opt(&self) {
        let prefixes = if self.config.gdb_native_rust {
            // GDB with Rust
            static PREFIXES: &'static [&'static str] = &["gdb", "gdbr"];
            println!("NOTE: compiletest thinks it is using GDB with native rust support");
            PREFIXES
        } else {
            // Generic GDB
            static PREFIXES: &'static [&'static str] = &["gdb", "gdbg"];
            println!("NOTE: compiletest thinks it is using GDB without native rust support");
            PREFIXES
        };

        let DebuggerCommands {
            commands,
            check_lines,
            breakpoint_lines
        } = self.parse_debugger_commands(prefixes);
        let mut cmds = commands.join("\n");

        // compile test file (it should have 'compile-flags:-g' in the header)
        let compiler_run_result = self.compile_test();
        if !compiler_run_result.status.success() {
            self.fatal_proc_rec("compilation failed!", &compiler_run_result);
        }

        let exe_file = self.make_exe_name();

        let debugger_run_result;
        match &*self.config.target {
            "arm-linux-androideabi" |
            "armv7-linux-androideabi" |
            "aarch64-linux-android" => {

                cmds = cmds.replace("run", "continue");

                let tool_path = match self.config.android_cross_path.to_str() {
                    Some(x) => x.to_owned(),
                    None => self.fatal("cannot find android cross path")
                };

                // write debugger script
                let mut script_str = String::with_capacity(2048);
                script_str.push_str(&format!("set charset {}\n", Self::charset()));
                script_str.push_str(&format!("set sysroot {}\n", tool_path));
                script_str.push_str(&format!("file {}\n", exe_file.to_str().unwrap()));
                script_str.push_str("target remote :5039\n");
                script_str.push_str(&format!("set solib-search-path \
                                              ./{}/stage2/lib/rustlib/{}/lib/\n",
                                             self.config.host, self.config.target));
                for line in &breakpoint_lines {
                    script_str.push_str(&format!("break {:?}:{}\n",
                                                 self.testpaths.file.file_name()
                                                 .unwrap()
                                                 .to_string_lossy(),
                                                 *line)[..]);
                }
                script_str.push_str(&cmds);
                script_str.push_str("\nquit\n");

                debug!("script_str = {}", script_str);
                self.dump_output_file(&script_str, "debugger.script");

                let adb_path = &self.config.adb_path;

                Command::new(adb_path)
                    .arg("push")
                    .arg(&exe_file)
                    .arg(&self.config.adb_test_dir)
                    .status()
                    .expect(&format!("failed to exec `{:?}`", adb_path));

                Command::new(adb_path)
                    .args(&["forward", "tcp:5039", "tcp:5039"])
                    .status()
                    .expect(&format!("failed to exec `{:?}`", adb_path));

                let adb_arg = format!("export LD_LIBRARY_PATH={}; \
                                       gdbserver{} :5039 {}/{}",
                                      self.config.adb_test_dir.clone(),
                                      if self.config.target.contains("aarch64")
                                      {"64"} else {""},
                                      self.config.adb_test_dir.clone(),
                                      exe_file.file_name().unwrap().to_str()
                                      .unwrap());

                debug!("adb arg: {}", adb_arg);
                let mut adb = Command::new(adb_path)
                    .args(&["shell", &adb_arg])
                    .stdout(Stdio::piped())
                    .stderr(Stdio::inherit())
                    .spawn()
                    .expect(&format!("failed to exec `{:?}`", adb_path));

                // Wait for the gdbserver to print out "Listening on port ..."
                // at which point we know that it's started and then we can
                // execute the debugger below.
                let mut stdout = BufReader::new(adb.stdout.take().unwrap());
                let mut line = String::new();
                loop {
                    line.truncate(0);
                    stdout.read_line(&mut line).unwrap();
                    if line.starts_with("Listening on port 5039") {
                        break
                    }
                }
                drop(stdout);

                let debugger_script = self.make_out_name("debugger.script");
                // FIXME (#9639): This needs to handle non-utf8 paths
                let debugger_opts =
                    vec!["-quiet".to_owned(),
                         "-batch".to_owned(),
                         "-nx".to_owned(),
                         format!("-command={}", debugger_script.to_str().unwrap())];

                let mut gdb_path = tool_path;
                gdb_path.push_str("/bin/gdb");
                let Output {
                    status,
                    stdout,
                    stderr
                } = Command::new(&gdb_path)
                    .args(&debugger_opts)
                    .output()
                    .expect(&format!("failed to exec `{:?}`", gdb_path));
                let cmdline = {
                    let mut gdb = Command::new(&format!("{}-gdb", self.config.target));
                    gdb.args(&debugger_opts);
                    let cmdline = self.make_cmdline(&gdb, "");
                    logv(self.config, format!("executing {}", cmdline));
                    cmdline
                };

                debugger_run_result = ProcRes {
                    status,
                    stdout: String::from_utf8(stdout).unwrap(),
                    stderr: String::from_utf8(stderr).unwrap(),
                    cmdline,
                };
                if adb.kill().is_err() {
                    println!("Adb process is already finished.");
                }
            }

            _=> {
                let rust_src_root = self.find_rust_src_root()
                                        .expect("Could not find Rust source root");
                let rust_pp_module_rel_path = Path::new("./src/etc");
                let rust_pp_module_abs_path = rust_src_root.join(rust_pp_module_rel_path)
                                                           .to_str()
                                                           .unwrap()
                                                           .to_owned();
                // write debugger script
                let mut script_str = String::with_capacity(2048);
                script_str.push_str(&format!("set charset {}\n", Self::charset()));
                script_str.push_str("show version\n");

                match self.config.gdb_version {
                    Some(version) => {
                        println!("NOTE: compiletest thinks it is using GDB version {}",
                                 version);

                        if version > extract_gdb_version("7.4").unwrap() {
                            // Add the directory containing the pretty printers to
                            // GDB's script auto loading safe path
                            script_str.push_str(
                                &format!("add-auto-load-safe-path {}\n",
                                         rust_pp_module_abs_path.replace(r"\", r"\\"))
                            );
                        }
                    }
                    _ => {
                        println!("NOTE: compiletest does not know which version of \
                                  GDB it is using");
                    }
                }

                // The following line actually doesn't have to do anything with
                // pretty printing, it just tells GDB to print values on one line:
                script_str.push_str("set print pretty off\n");

                // Add the pretty printer directory to GDB's source-file search path
                script_str.push_str(&format!("directory {}\n",
                                             rust_pp_module_abs_path));

                // Load the target executable
                script_str.push_str(&format!("file {}\n",
                                             exe_file.to_str().unwrap()
                                             .replace(r"\", r"\\")));

                // Force GDB to print values in the Rust format.
                if self.config.gdb_native_rust {
                    script_str.push_str("set language rust\n");
                }

                // Add line breakpoints
                for line in &breakpoint_lines {
                    script_str.push_str(&format!("break '{}':{}\n",
                                                 self.testpaths.file.file_name().unwrap()
                                                 .to_string_lossy(),
                                                 *line));
                }

                script_str.push_str(&cmds);
                script_str.push_str("\nquit\n");

                debug!("script_str = {}", script_str);
                self.dump_output_file(&script_str, "debugger.script");

                let debugger_script = self.make_out_name("debugger.script");

                // FIXME (#9639): This needs to handle non-utf8 paths
                let debugger_opts =
                    vec!["-quiet".to_owned(),
                         "-batch".to_owned(),
                         "-nx".to_owned(),
                         format!("-command={}", debugger_script.to_str().unwrap())];

                let mut gdb = Command::new(self.config.gdb.as_ref().unwrap());
                gdb.args(&debugger_opts)
                    .env("PYTHONPATH", rust_pp_module_abs_path);

                debugger_run_result =
                    self.compose_and_run(gdb,
                                         self.config.run_lib_path.to_str().unwrap(),
                                         None,
                                         None);
            }
        }

        if !debugger_run_result.status.success() {
            self.fatal_proc_rec("gdb failed to execute", &debugger_run_result);
        }

        self.check_debugger_output(&debugger_run_result, &check_lines);
    }

    fn find_rust_src_root(&self) -> Option<PathBuf> {
        let mut path = self.config.src_base.clone();
        let path_postfix = Path::new("src/etc/lldb_batchmode.py");

        while path.pop() {
            if path.join(&path_postfix).is_file() {
                return Some(path);
            }
        }

        None
    }

    fn run_debuginfo_lldb_test(&self) {
        assert!(self.revision.is_none(), "revisions not relevant here");

        if self.config.lldb_python_dir.is_none() {
            self.fatal("Can't run LLDB test because LLDB's python path is not set.");
        }

        let config = Config {
            target_rustcflags: self.cleanup_debug_info_options(&self.config.target_rustcflags),
            host_rustcflags: self.cleanup_debug_info_options(&self.config.host_rustcflags),
            .. self.config.clone()
        };


        let test_cx = TestCx {
            config: &config,
            ..*self
        };

        test_cx.run_debuginfo_lldb_test_no_opt();
    }

    fn run_debuginfo_lldb_test_no_opt(&self) {
        // compile test file (it should have 'compile-flags:-g' in the header)
        let compile_result = self.compile_test();
        if !compile_result.status.success() {
            self.fatal_proc_rec("compilation failed!", &compile_result);
        }

        let exe_file = self.make_exe_name();

        match self.config.lldb_version {
            Some(ref version) => {
                println!("NOTE: compiletest thinks it is using LLDB version {}",
                         version);
            }
            _ => {
                println!("NOTE: compiletest does not know which version of \
                          LLDB it is using");
            }
        }

        // Parse debugger commands etc from test files
        let DebuggerCommands {
            commands,
            check_lines,
            breakpoint_lines,
            ..
        } = self.parse_debugger_commands(&["lldb"]);

        // Write debugger script:
        // We don't want to hang when calling `quit` while the process is still running
        let mut script_str = String::from("settings set auto-confirm true\n");

        // Make LLDB emit its version, so we have it documented in the test output
        script_str.push_str("version\n");

        // Switch LLDB into "Rust mode"
        let rust_src_root = self.find_rust_src_root().expect("Could not find Rust source root");
        let rust_pp_module_rel_path = Path::new("./src/etc/lldb_rust_formatters.py");
        let rust_pp_module_abs_path = rust_src_root.join(rust_pp_module_rel_path)
                                                   .to_str()
                                                   .unwrap()
                                                   .to_owned();

        script_str.push_str(&format!("command script import {}\n",
                                     &rust_pp_module_abs_path[..])[..]);
        script_str.push_str("type summary add --no-value ");
        script_str.push_str("--python-function lldb_rust_formatters.print_val ");
        script_str.push_str("-x \".*\" --category Rust\n");
        script_str.push_str("type category enable Rust\n");

        // Set breakpoints on every line that contains the string "#break"
        let source_file_name = self.testpaths.file.file_name().unwrap().to_string_lossy();
        for line in &breakpoint_lines {
            script_str.push_str(&format!("breakpoint set --file '{}' --line {}\n",
                                         source_file_name,
                                         line));
        }

        // Append the other commands
        for line in &commands {
            script_str.push_str(line);
            script_str.push_str("\n");
        }

        // Finally, quit the debugger
        script_str.push_str("\nquit\n");

        // Write the script into a file
        debug!("script_str = {}", script_str);
        self.dump_output_file(&script_str, "debugger.script");
        let debugger_script = self.make_out_name("debugger.script");

        // Let LLDB execute the script via lldb_batchmode.py
        let debugger_run_result = self.run_lldb(&exe_file,
                                                &debugger_script,
                                                &rust_src_root);

        if !debugger_run_result.status.success() {
            self.fatal_proc_rec("Error while running LLDB", &debugger_run_result);
        }

        self.check_debugger_output(&debugger_run_result, &check_lines);
    }

    fn run_lldb(&self,
                test_executable: &Path,
                debugger_script: &Path,
                rust_src_root: &Path)
                -> ProcRes {
        // Prepare the lldb_batchmode which executes the debugger script
        let lldb_script_path = rust_src_root.join("src/etc/lldb_batchmode.py");
        self.cmd2procres(Command::new(&self.config.lldb_python)
                         .arg(&lldb_script_path)
                         .arg(test_executable)
                         .arg(debugger_script)
                         .env("PYTHONPATH",
                              self.config.lldb_python_dir.as_ref().unwrap()))
    }

    fn cmd2procres(&self, cmd: &mut Command) -> ProcRes {
        let (status, out, err) = match cmd.output() {
            Ok(Output { status, stdout, stderr }) => {
                (status,
                 String::from_utf8(stdout).unwrap(),
                 String::from_utf8(stderr).unwrap())
            },
            Err(e) => {
                self.fatal(&format!("Failed to setup Python process for \
                                      LLDB script: {}", e))
            }
        };

        self.dump_output(&out, &err);
        ProcRes {
            status,
            stdout: out,
            stderr: err,
            cmdline: format!("{:?}", cmd)
        }
    }

    fn parse_debugger_commands(&self, debugger_prefixes: &[&str]) -> DebuggerCommands {
        let directives = debugger_prefixes.iter().map(|prefix| (
            format!("{}-command", prefix),
            format!("{}-check", prefix),
        )).collect::<Vec<_>>();

        let mut breakpoint_lines = vec![];
        let mut commands = vec![];
        let mut check_lines = vec![];
        let mut counter = 1;
        let reader = BufReader::new(File::open(&self.testpaths.file).unwrap());
        for line in reader.lines() {
            match line {
                Ok(line) => {
                    if line.contains("#break") {
                        breakpoint_lines.push(counter);
                    }

                    for &(ref command_directive, ref check_directive) in &directives {
                        self.config.parse_name_value_directive(
                            &line,
                            command_directive).map(|cmd| {
                                commands.push(cmd)
                            });

                        self.config.parse_name_value_directive(
                            &line,
                            check_directive).map(|cmd| {
                                check_lines.push(cmd)
                            });
                    }
                }
                Err(e) => {
                    self.fatal(&format!("Error while parsing debugger commands: {}", e))
                }
            }
            counter += 1;
        }

        DebuggerCommands {
            commands,
            check_lines,
            breakpoint_lines,
        }
    }

    fn cleanup_debug_info_options(&self, options: &Option<String>) -> Option<String> {
        if options.is_none() {
            return None;
        }

        // Remove options that are either unwanted (-O) or may lead to duplicates due to RUSTFLAGS.
        let options_to_remove = [
            "-O".to_owned(),
            "-g".to_owned(),
            "--debuginfo".to_owned()
        ];
        let new_options =
            self.split_maybe_args(options).into_iter()
                                          .filter(|x| !options_to_remove.contains(x))
                                          .collect::<Vec<String>>();

        Some(new_options.join(" "))
    }

    fn check_debugger_output(&self, debugger_run_result: &ProcRes, check_lines: &[String]) {
        let num_check_lines = check_lines.len();

        let mut check_line_index = 0;
        for line in debugger_run_result.stdout.lines() {
            if check_line_index >= num_check_lines {
                break;
            }

            if check_single_line(line, &(check_lines[check_line_index])[..]) {
                check_line_index += 1;
            }
        }
        if check_line_index != num_check_lines && num_check_lines > 0 {
            self.fatal_proc_rec(&format!("line not found in debugger output: {}",
                                         check_lines[check_line_index]),
                                debugger_run_result);
        }

        fn check_single_line(line: &str, check_line: &str) -> bool {
            // Allow check lines to leave parts unspecified (e.g., uninitialized
            // bits in the  wrong case of an enum) with the notation "[...]".
            let line = line.trim();
            let check_line = check_line.trim();
            let can_start_anywhere = check_line.starts_with("[...]");
            let can_end_anywhere = check_line.ends_with("[...]");

            let check_fragments: Vec<&str> = check_line.split("[...]")
                                                       .filter(|frag| !frag.is_empty())
                                                       .collect();
            if check_fragments.is_empty() {
                return true;
            }

            let (mut rest, first_fragment) = if can_start_anywhere {
                match line.find(check_fragments[0]) {
                    Some(pos) => (&line[pos + check_fragments[0].len() ..], 1),
                    None => return false
                }
            } else {
                (line, 0)
            };

            for current_fragment in &check_fragments[first_fragment..] {
                match rest.find(current_fragment) {
                    Some(pos) => {
                        rest = &rest[pos + current_fragment.len() .. ];
                    }
                    None => return false
                }
            }

            if !can_end_anywhere && !rest.is_empty() {
                return false;
            }

            true
        }
    }

    fn check_error_patterns(&self,
                            output_to_check: &str,
                            proc_res: &ProcRes) {
        if self.props.error_patterns.is_empty() {
            if self.props.must_compile_successfully {
                return
            } else {
                self.fatal(&format!("no error pattern specified in {:?}",
                                    self.testpaths.file.display()));
            }
        }
        let mut next_err_idx = 0;
        let mut next_err_pat = self.props.error_patterns[next_err_idx].trim();
        let mut done = false;
        for line in output_to_check.lines() {
            if line.contains(next_err_pat) {
                debug!("found error pattern {}", next_err_pat);
                next_err_idx += 1;
                if next_err_idx == self.props.error_patterns.len() {
                    debug!("found all error patterns");
                    done = true;
                    break;
                }
                next_err_pat = self.props.error_patterns[next_err_idx].trim();
            }
        }
        if done { return; }

        let missing_patterns = &self.props.error_patterns[next_err_idx..];
        if missing_patterns.len() == 1 {
            self.fatal_proc_rec(
                &format!("error pattern '{}' not found!", missing_patterns[0]),
                proc_res);
        } else {
            for pattern in missing_patterns {
                self.error(&format!("error pattern '{}' not found!", *pattern));
            }
            self.fatal_proc_rec("multiple error patterns not found", proc_res);
        }
    }

    fn check_no_compiler_crash(&self, proc_res: &ProcRes) {
        for line in proc_res.stderr.lines() {
            if line.contains("error: internal compiler error") {
                self.fatal_proc_rec("compiler encountered internal error", proc_res);
            }
        }
    }

    fn check_forbid_output(&self,
                           output_to_check: &str,
                           proc_res: &ProcRes) {
        for pat in &self.props.forbid_output {
            if output_to_check.contains(pat) {
                self.fatal_proc_rec("forbidden pattern found in compiler output", proc_res);
            }
        }
    }

    fn check_expected_errors(&self,
                             expected_errors: Vec<errors::Error>,
                             proc_res: &ProcRes) {
        if proc_res.status.success() &&
            expected_errors.iter().any(|x| x.kind == Some(ErrorKind::Error)) {
            self.fatal_proc_rec("process did not return an error status", proc_res);
        }

        let file_name =
            format!("{}", self.testpaths.file.display())
            .replace(r"\", "/"); // on windows, translate all '\' path separators to '/'

        // If the testcase being checked contains at least one expected "help"
        // message, then we'll ensure that all "help" messages are expected.
        // Otherwise, all "help" messages reported by the compiler will be ignored.
        // This logic also applies to "note" messages.
        let expect_help = expected_errors.iter().any(|ee| ee.kind == Some(ErrorKind::Help));
        let expect_note = expected_errors.iter().any(|ee| ee.kind == Some(ErrorKind::Note));

        // Parse the JSON output from the compiler and extract out the messages.
        let actual_errors = json::parse_output(&file_name, &proc_res.stderr, proc_res);
        let mut unexpected = Vec::new();
        let mut found = vec![false; expected_errors.len()];
        for actual_error in &actual_errors {
            let opt_index =
                expected_errors
                .iter()
                .enumerate()
                .position(|(index, expected_error)| {
                    !found[index] &&
                        actual_error.line_num == expected_error.line_num &&
                        (expected_error.kind.is_none() ||
                         actual_error.kind == expected_error.kind) &&
                        actual_error.msg.contains(&expected_error.msg)
                });

            match opt_index {
                Some(index) => {
                    // found a match, everybody is happy
                    assert!(!found[index]);
                    found[index] = true;
                }

                None => {
                    if self.is_unexpected_compiler_message(actual_error, expect_help, expect_note) {
                        self.error(
                            &format!("{}:{}: unexpected {:?}: '{}'",
                                     file_name,
                                     actual_error.line_num,
                                     actual_error.kind.as_ref()
                                     .map_or(String::from("message"),
                                             |k| k.to_string()),
                                     actual_error.msg));
                        unexpected.push(actual_error);
                    }
                }
            }
        }

        let mut not_found = Vec::new();
        // anything not yet found is a problem
        for (index, expected_error) in expected_errors.iter().enumerate() {
            if !found[index] {
                self.error(
                    &format!("{}:{}: expected {} not found: {}",
                             file_name,
                             expected_error.line_num,
                             expected_error.kind.as_ref()
                             .map_or("message".into(),
                                     |k| k.to_string()),
                             expected_error.msg));
                not_found.push(expected_error);
            }
        }

        if !unexpected.is_empty() || !not_found.is_empty() {
            self.error(
                &format!("{} unexpected errors found, {} expected errors not found",
                         unexpected.len(), not_found.len()));
            println!("status: {}\ncommand: {}",
                   proc_res.status, proc_res.cmdline);
            if !unexpected.is_empty() {
                println!("unexpected errors (from JSON output): {:#?}\n", unexpected);
            }
            if !not_found.is_empty() {
                println!("not found errors (from test file): {:#?}\n", not_found);
            }
            panic!();
        }
    }

    /// Returns true if we should report an error about `actual_error`,
    /// which did not match any of the expected error. We always require
    /// errors/warnings to be explicitly listed, but only require
    /// helps/notes if there are explicit helps/notes given.
    fn is_unexpected_compiler_message(&self,
                                      actual_error: &Error,
                                      expect_help: bool,
                                      expect_note: bool)
                                      -> bool {
        match actual_error.kind {
            Some(ErrorKind::Help) => expect_help,
            Some(ErrorKind::Note) => expect_note,
            Some(ErrorKind::Error) |
            Some(ErrorKind::Warning) => true,
            Some(ErrorKind::Suggestion) |
            None => false
        }
    }

    fn compile_test(&self) -> ProcRes {
        let mut rustc = self.make_compile_args(
            &self.testpaths.file, TargetLocation::ThisFile(self.make_exe_name()));

        rustc.arg("-L").arg(&self.aux_output_dir_name());

        match self.config.mode {
            CompileFail | Ui => {
                // compile-fail and ui tests tend to have tons of unused code as
                // it's just testing various pieces of the compile, but we don't
                // want to actually assert warnings about all this code. Instead
                // let's just ignore unused code warnings by defaults and tests
                // can turn it back on if needed.
                rustc.args(&["-A", "unused"]);
            }
            _ => {}
        }

        self.compose_and_run_compiler(rustc, None)
    }

    fn document(&self, out_dir: &Path) -> ProcRes {
        if self.props.build_aux_docs {
            for rel_ab in &self.props.aux_builds {
                let aux_testpaths = self.compute_aux_test_paths(rel_ab);
                let aux_props = self.props.from_aux_file(&aux_testpaths.file,
                                                         self.revision,
                                                         self.config);
                let aux_cx = TestCx {
                    config: self.config,
                    props: &aux_props,
                    testpaths: &aux_testpaths,
                    revision: self.revision
                };
                let auxres = aux_cx.document(out_dir);
                if !auxres.status.success() {
                    return auxres;
                }
            }
        }

        let aux_dir = self.aux_output_dir_name();

        let rustdoc_path = self.config.rustdoc_path.as_ref().expect("--rustdoc-path passed");
        let mut rustdoc = Command::new(rustdoc_path);

        rustdoc.arg("-L").arg(aux_dir)
            .arg("-o").arg(out_dir)
            .arg(&self.testpaths.file)
            .args(&self.props.compile_flags);

        self.compose_and_run_compiler(rustdoc, None)
    }

    fn exec_compiled_test(&self) -> ProcRes {
        let env = &self.props.exec_env;

        match &*self.config.target {
            // This is pretty similar to below, we're transforming:
            //
            //      program arg1 arg2
            //
            // into
            //
            //      remote-test-client run program:support-lib.so arg1 arg2
            //
            // The test-client program will upload `program` to the emulator
            // along with all other support libraries listed (in this case
            // `support-lib.so`. It will then execute the program on the
            // emulator with the arguments specified (in the environment we give
            // the process) and then report back the same result.
            _ if self.config.remote_test_client.is_some() => {
                let aux_dir = self.aux_output_dir_name();
                let ProcArgs { mut prog, args } = self.make_run_args();
                if let Ok(entries) = aux_dir.read_dir() {
                    for entry in entries {
                        let entry = entry.unwrap();
                        if !entry.path().is_file() {
                            continue
                        }
                        prog.push_str(":");
                        prog.push_str(entry.path().to_str().unwrap());
                    }
                }
                let mut test_client = Command::new(
                    self.config.remote_test_client.as_ref().unwrap());
                test_client
                    .args(&["run", &prog])
                    .args(args)
                    .envs(env.clone());
                self.compose_and_run(test_client,
                                     self.config.run_lib_path.to_str().unwrap(),
                                     Some(aux_dir.to_str().unwrap()),
                                     None)
            }
            _ => {
                let aux_dir = self.aux_output_dir_name();
                let ProcArgs { prog, args } = self.make_run_args();
                let mut program = Command::new(&prog);
                program.args(args)
                    .current_dir(&self.output_base_name().parent().unwrap())
                    .envs(env.clone());
                self.compose_and_run(program,
                                     self.config.run_lib_path.to_str().unwrap(),
                                     Some(aux_dir.to_str().unwrap()),
                                     None)
            }
        }
    }

    /// For each `aux-build: foo/bar` annotation, we check to find the
    /// file in a `aux` directory relative to the test itself.
    fn compute_aux_test_paths(&self, rel_ab: &str) -> TestPaths {
        let test_ab = self.testpaths.file
                                    .parent()
                                    .expect("test file path has no parent")
                                    .join("auxiliary")
                                    .join(rel_ab);
        if !test_ab.exists() {
            self.fatal(&format!("aux-build `{}` source not found", test_ab.display()))
        }

        TestPaths {
            file: test_ab,
            base: self.testpaths.base.clone(),
            relative_dir: self.testpaths.relative_dir
                                        .join("auxiliary")
                                        .join(rel_ab)
                                        .parent()
                                        .expect("aux-build path has no parent")
                                        .to_path_buf()
        }
    }

    fn compose_and_run_compiler(&self, mut rustc: Command, input: Option<String>) -> ProcRes {
        if !self.props.aux_builds.is_empty() {
            create_dir_all(&self.aux_output_dir_name()).unwrap();
        }

        let aux_dir = self.aux_output_dir_name();

        for rel_ab in &self.props.aux_builds {
            let aux_testpaths = self.compute_aux_test_paths(rel_ab);
            let aux_props = self.props.from_aux_file(&aux_testpaths.file,
                                                     self.revision,
                                                     self.config);
            let aux_output = {
                let f = self.make_lib_name(&self.testpaths.file);
                let parent = f.parent().unwrap();
                TargetLocation::ThisDirectory(parent.to_path_buf())
            };
            let aux_cx = TestCx {
                config: self.config,
                props: &aux_props,
                testpaths: &aux_testpaths,
                revision: self.revision
            };
            let mut aux_rustc = aux_cx.make_compile_args(&aux_testpaths.file, aux_output);

            let crate_type = if aux_props.no_prefer_dynamic {
                None
            } else if (self.config.target.contains("musl") && !aux_props.force_host) ||
                      self.config.target.contains("emscripten") {
                // We primarily compile all auxiliary libraries as dynamic libraries
                // to avoid code size bloat and large binaries as much as possible
                // for the test suite (otherwise including libstd statically in all
                // executables takes up quite a bit of space).
                //
                // For targets like MUSL or Emscripten, however, there is no support for
                // dynamic libraries so we just go back to building a normal library. Note,
                // however, that for MUSL if the library is built with `force_host` then
                // it's ok to be a dylib as the host should always support dylibs.
                Some("lib")
            } else {
                Some("dylib")
            };

            if let Some(crate_type) = crate_type {
                aux_rustc.args(&["--crate-type", crate_type]);
            }

            aux_rustc.arg("-L").arg(&aux_dir);

            let auxres = aux_cx.compose_and_run(aux_rustc,
                                                aux_cx.config.compile_lib_path.to_str().unwrap(),
                                                Some(aux_dir.to_str().unwrap()),
                                                None);
            if !auxres.status.success() {
                self.fatal_proc_rec(
                    &format!("auxiliary build of {:?} failed to compile: ",
                             aux_testpaths.file.display()),
                    &auxres);
            }
        }

        rustc.envs(self.props.rustc_env.clone());
        self.compose_and_run(rustc,
                             self.config.compile_lib_path.to_str().unwrap(),
                             Some(aux_dir.to_str().unwrap()),
                             input)
    }

    fn compose_and_run(&self,
                       mut command: Command,
                       lib_path: &str,
                       aux_path: Option<&str>,
                       input: Option<String>) -> ProcRes {
        let cmdline =
        {
            let cmdline = self.make_cmdline(&command, lib_path);
            logv(self.config, format!("executing {}", cmdline));
            cmdline
        };

        command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::piped());

        // Need to be sure to put both the lib_path and the aux path in the dylib
        // search path for the child.
        let mut path = env::split_paths(&env::var_os(dylib_env_var()).unwrap_or(OsString::new()))
            .collect::<Vec<_>>();
        if let Some(p) = aux_path {
            path.insert(0, PathBuf::from(p))
        }
        path.insert(0, PathBuf::from(lib_path));

        // Add the new dylib search path var
        let newpath = env::join_paths(&path).unwrap();
        command.env(dylib_env_var(), newpath);

        let mut child = command.spawn().expect(&format!("failed to exec `{:?}`", &command));
        if let Some(input) = input {
            child.stdin.as_mut().unwrap().write_all(input.as_bytes()).unwrap();
        }
        let Output { status, stdout, stderr } = child.wait_with_output().unwrap();

        let result = ProcRes {
            status,
            stdout: String::from_utf8(stdout).unwrap(),
            stderr: String::from_utf8(stderr).unwrap(),
            cmdline,
        };

        self.dump_output(&result.stdout, &result.stderr);

        result
    }

    fn make_compile_args(&self, input_file: &Path, output_file: TargetLocation) -> Command {
        let mut rustc = Command::new(&self.config.rustc_path);
        rustc.arg(input_file)
            .arg("-L").arg(&self.config.build_base);

        // Optionally prevent default --target if specified in test compile-flags.
        let custom_target = self.props.compile_flags
            .iter()
            .fold(false, |acc, x| acc || x.starts_with("--target"));

        if !custom_target {
            let target = if self.props.force_host {
                &*self.config.host
            } else {
                &*self.config.target
            };

            rustc.arg(&format!("--target={}", target));
        }

        if let Some(revision) = self.revision {
            rustc.args(&["--cfg", revision]);
        }

        if let Some(ref incremental_dir) = self.props.incremental_dir {
            rustc.args(&["-Z", &format!("incremental={}", incremental_dir.display())]);
        }

        match self.config.mode {
            CompileFail |
            ParseFail |
            Incremental => {
                // If we are extracting and matching errors in the new
                // fashion, then you want JSON mode. Old-skool error
                // patterns still match the raw compiler output.
                if self.props.error_patterns.is_empty() {
                    rustc.args(&["--error-format", "json"]);
                }
            }
            MirOpt => {
                rustc.args(&[
                    "-Zdump-mir=all",
                    "-Zmir-opt-level=3",
                    "-Zdump-mir-exclude-pass-number"]);

                let mir_dump_dir = self.get_mir_dump_dir();
                create_dir_all(mir_dump_dir.as_path()).unwrap();
                let mut dir_opt = "-Zdump-mir-dir=".to_string();
                dir_opt.push_str(mir_dump_dir.to_str().unwrap());
                debug!("dir_opt: {:?}", dir_opt);

                rustc.arg(dir_opt);
            }
            RunPass |
            RunFail |
            RunPassValgrind |
            Pretty |
            DebugInfoGdb |
            DebugInfoLldb |
            Codegen |
            Rustdoc |
            RunMake |
            Ui |
            CodegenUnits => {
                // do not use JSON output
            }
        }

        if !self.props.no_prefer_dynamic {
            rustc.args(&["-C", "prefer-dynamic"]);
        }

        match output_file {
            TargetLocation::ThisFile(path) => {
                rustc.arg("-o").arg(path);
            }
            TargetLocation::ThisDirectory(path) => {
                rustc.arg("--out-dir").arg(path);
            }
        }

        if self.props.force_host {
            rustc.args(self.split_maybe_args(&self.config.host_rustcflags));
        } else {
            rustc.args(self.split_maybe_args(&self.config.target_rustcflags));
        }

        rustc.args(&self.props.compile_flags);

        rustc
    }

    fn make_lib_name(&self, auxfile: &Path) -> PathBuf {
        // what we return here is not particularly important, as it
        // happens; rustc ignores everything except for the directory.
        let auxname = self.output_testname(auxfile);
        self.aux_output_dir_name().join(&auxname)
    }

    fn make_exe_name(&self) -> PathBuf {
        let mut f = self.output_base_name();
        // FIXME: This is using the host architecture exe suffix, not target!
        if self.config.target.contains("emscripten") {
            let mut fname = f.file_name().unwrap().to_os_string();
            fname.push(".js");
            f.set_file_name(&fname);
        } else if !env::consts::EXE_SUFFIX.is_empty() {
            let mut fname = f.file_name().unwrap().to_os_string();
            fname.push(env::consts::EXE_SUFFIX);
            f.set_file_name(&fname);
        }
        f
    }

    fn make_run_args(&self) -> ProcArgs {
        // If we've got another tool to run under (valgrind),
        // then split apart its command
        let mut args = self.split_maybe_args(&self.config.runtool);

        // If this is emscripten, then run tests under nodejs
        if self.config.target.contains("emscripten") {
            if let Some(ref p) = self.config.nodejs {
                args.push(p.clone());
            } else {
                self.fatal("no NodeJS binary found (--nodejs)");
            }
        }

        let exe_file = self.make_exe_name();

        // FIXME (#9639): This needs to handle non-utf8 paths
        args.push(exe_file.to_str().unwrap().to_owned());

        // Add the arguments in the run_flags directive
        args.extend(self.split_maybe_args(&self.props.run_flags));

        let prog = args.remove(0);
         ProcArgs {
            prog,
            args,
        }
    }

    fn split_maybe_args(&self, argstr: &Option<String>) -> Vec<String> {
        match *argstr {
            Some(ref s) => {
                s
                    .split(' ')
                    .filter_map(|s| {
                        if s.chars().all(|c| c.is_whitespace()) {
                            None
                        } else {
                            Some(s.to_owned())
                        }
                    }).collect()
            }
            None => Vec::new()
        }
    }

    fn make_cmdline(&self, command: &Command, libpath: &str) -> String {
        use util;

        // Linux and mac don't require adjusting the library search path
        if cfg!(unix) {
            format!("{:?}", command)
        } else {
            // Build the LD_LIBRARY_PATH variable as it would be seen on the command line
            // for diagnostic purposes
            fn lib_path_cmd_prefix(path: &str) -> String {
                format!("{}=\"{}\"", util::lib_path_env_var(), util::make_new_path(path))
            }

            format!("{} {:?}", lib_path_cmd_prefix(libpath), command)
        }
    }

    fn dump_output(&self, out: &str, err: &str) {
        let revision = if let Some(r) = self.revision {
            format!("{}.", r)
        } else {
            String::new()
        };

        self.dump_output_file(out, &format!("{}out", revision));
        self.dump_output_file(err, &format!("{}err", revision));
        self.maybe_dump_to_stdout(out, err);
    }

    fn dump_output_file(&self,
                        out: &str,
                        extension: &str) {
        let outfile = self.make_out_name(extension);
        File::create(&outfile).unwrap().write_all(out.as_bytes()).unwrap();
    }

    fn make_out_name(&self, extension: &str) -> PathBuf {
        self.output_base_name().with_extension(extension)
    }

    fn aux_output_dir_name(&self) -> PathBuf {
        let f = self.output_base_name();
        let mut fname = f.file_name().unwrap().to_os_string();
        fname.push(&format!(".{}.libaux", self.config.mode));
        f.with_file_name(&fname)
    }

    fn output_testname(&self, filepath: &Path) -> PathBuf {
        PathBuf::from(filepath.file_stem().unwrap())
    }

    /// Given a test path like `compile-fail/foo/bar.rs` Returns a name like
    ///
    ///     <output>/foo/bar-stage1
    fn output_base_name(&self) -> PathBuf {
        let dir = self.config.build_base.join(&self.testpaths.relative_dir);

        // Note: The directory `dir` is created during `collect_tests_from_dir`
        dir
            .join(&self.output_testname(&self.testpaths.file))
            .with_extension(&self.config.stage_id)
    }

    fn maybe_dump_to_stdout(&self, out: &str, err: &str) {
        if self.config.verbose {
            println!("------{}------------------------------", "stdout");
            println!("{}", out);
            println!("------{}------------------------------", "stderr");
            println!("{}", err);
            println!("------------------------------------------");
        }
    }

    fn error(&self, err: &str) {
        match self.revision {
            Some(rev) => println!("\nerror in revision `{}`: {}", rev, err),
            None => println!("\nerror: {}", err)
        }
    }

    fn fatal(&self, err: &str) -> ! {
        self.error(err); panic!();
    }

    fn fatal_proc_rec(&self, err: &str, proc_res: &ProcRes) -> ! {
        self.try_print_open_handles();
        self.error(err);
        proc_res.fatal(None);
    }

    // This function is a poor man's attempt to debug rust-lang/rust#38620, if
    // that's closed then this should be deleted
    //
    // This is a very "opportunistic" debugging attempt, so we ignore all
    // errors here.
    fn try_print_open_handles(&self) {
        if !cfg!(windows) {
            return
        }
        if self.config.mode != Incremental {
            return
        }

        let filename = match self.testpaths.file.file_stem() {
            Some(path) => path,
            None => return,
        };

        let mut cmd = Command::new("handle.exe");
        cmd.arg("-a").arg("-u");
        cmd.arg(filename);
        cmd.arg("-nobanner");
        let output = match cmd.output() {
            Ok(output) => output,
            Err(_) => return,
        };
        println!("---------------------------------------------------");
        println!("ran extra command to debug rust-lang/rust#38620: ");
        println!("{:?}", cmd);
        println!("result: {}", output.status);
        println!("--- stdout ----------------------------------------");
        println!("{}", String::from_utf8_lossy(&output.stdout));
        println!("--- stderr ----------------------------------------");
        println!("{}", String::from_utf8_lossy(&output.stderr));
        println!("---------------------------------------------------");
    }

    // codegen tests (using FileCheck)

    fn compile_test_and_save_ir(&self) -> ProcRes {
        let aux_dir = self.aux_output_dir_name();

        let output_file = TargetLocation::ThisDirectory(
            self.output_base_name().parent().unwrap().to_path_buf());
        let mut rustc = self.make_compile_args(&self.testpaths.file, output_file);
        rustc.arg("-L").arg(aux_dir)
            .arg("--emit=llvm-ir");

        self.compose_and_run_compiler(rustc, None)
    }

    fn check_ir_with_filecheck(&self) -> ProcRes {
        let irfile = self.output_base_name().with_extension("ll");
        let mut filecheck = Command::new(self.config.llvm_filecheck.as_ref().unwrap());
        filecheck.arg("--input-file").arg(irfile)
            .arg(&self.testpaths.file);
        self.compose_and_run(filecheck, "", None, None)
    }

    fn run_codegen_test(&self) {
        assert!(self.revision.is_none(), "revisions not relevant here");

        if self.config.llvm_filecheck.is_none() {
            self.fatal("missing --llvm-filecheck");
        }

        let mut proc_res = self.compile_test_and_save_ir();
        if !proc_res.status.success() {
            self.fatal_proc_rec("compilation failed!", &proc_res);
        }

        proc_res = self.check_ir_with_filecheck();
        if !proc_res.status.success() {
            self.fatal_proc_rec("verification with 'FileCheck' failed", &proc_res);
        }
    }

    fn charset() -> &'static str {
        // FreeBSD 10.1 defaults to GDB 6.1.1 which doesn't support "auto" charset
        if cfg!(target_os = "bitrig") {
            "auto"
        } else if cfg!(target_os = "freebsd") {
            "ISO-8859-1"
        } else {
            "UTF-8"
        }
    }

    fn run_rustdoc_test(&self) {
        assert!(self.revision.is_none(), "revisions not relevant here");

        let out_dir = self.output_base_name();
        let _ = fs::remove_dir_all(&out_dir);
        create_dir_all(&out_dir).unwrap();

        let proc_res = self.document(&out_dir);
        if !proc_res.status.success() {
            self.fatal_proc_rec("rustdoc failed!", &proc_res);
        }

        if self.props.check_test_line_numbers_match {
            self.check_rustdoc_test_option(proc_res);
        } else {
            let root = self.find_rust_src_root().unwrap();
            let res = self.cmd2procres(Command::new(&self.config.docck_python)
                                       .arg(root.join("src/etc/htmldocck.py"))
                                       .arg(out_dir)
                                       .arg(&self.testpaths.file));
            if !res.status.success() {
                self.fatal_proc_rec("htmldocck failed!", &res);
            }
        }
    }

    fn get_lines<P: AsRef<Path>>(&self, path: &P,
                                 mut other_files: Option<&mut Vec<String>>) -> Vec<usize> {
        let mut file = fs::File::open(path)
                                .expect("markdown_test_output_check_entry File::open failed");
        let mut content = String::new();
        file.read_to_string(&mut content)
            .expect("markdown_test_output_check_entry read_to_string failed");
        let mut ignore = false;
        content.lines()
               .enumerate()
               .filter_map(|(line_nb, line)| {
                   if (line.trim_left().starts_with("pub mod ") ||
                       line.trim_left().starts_with("mod ")) &&
                      line.ends_with(';') {
                       if let Some(ref mut other_files) = other_files {
                           other_files.push(line.rsplit("mod ")
                                      .next()
                                      .unwrap()
                                      .replace(";", ""));
                       }
                       None
                   } else {
                       let sline = line.split("///").last().unwrap_or("");
                       let line = sline.trim_left();
                       if line.starts_with("```") {
                           if ignore {
                               ignore = false;
                               None
                           } else {
                               ignore = true;
                               Some(line_nb + 1)
                           }
                       } else {
                           None
                       }
                   }
               })
               .collect()
    }

    fn check_rustdoc_test_option(&self, res: ProcRes) {
        let mut other_files = Vec::new();
        let mut files: HashMap<String, Vec<usize>> = HashMap::new();
        let cwd = env::current_dir().unwrap();
        files.insert(self.testpaths.file.strip_prefix(&cwd)
                                        .unwrap_or(&self.testpaths.file)
                                        .to_str()
                                        .unwrap()
                                        .replace('\\', "/"),
                     self.get_lines(&self.testpaths.file, Some(&mut other_files)));
        for other_file in other_files {
            let mut path = self.testpaths.file.clone();
            path.set_file_name(&format!("{}.rs", other_file));
            files.insert(path.strip_prefix(&cwd)
                             .unwrap_or(&path)
                             .to_str()
                             .unwrap()
                             .replace('\\', "/"),
                         self.get_lines(&path, None));
        }

        let mut tested = 0;
        for _ in res.stdout.split('\n')
                           .filter(|s| s.starts_with("test "))
                           .inspect(|s| {
                               let tmp: Vec<&str> = s.split(" - ").collect();
                               if tmp.len() == 2 {
                                   let path = tmp[0].rsplit("test ").next().unwrap();
                                   if let Some(ref mut v) = files.get_mut(
                                                                &path.replace('\\', "/")) {
                                       tested += 1;
                                       let mut iter = tmp[1].split("(line ");
                                       iter.next();
                                       let line = iter.next()
                                                      .unwrap_or(")")
                                                      .split(')')
                                                      .next()
                                                      .unwrap_or("0")
                                                      .parse()
                                                      .unwrap_or(0);
                                       if let Ok(pos) = v.binary_search(&line) {
                                           v.remove(pos);
                                       } else {
                                           self.fatal_proc_rec(
                                               &format!("Not found doc test: \"{}\" in \"{}\":{:?}",
                                                        s, path, v),
                                               &res);
                                       }
                                   }
                               }
                           }) {}
        if tested == 0 {
            self.fatal_proc_rec(&format!("No test has been found... {:?}", files), &res);
        } else {
            for (entry, v) in &files {
                if !v.is_empty() {
                    self.fatal_proc_rec(&format!("Not found test at line{} \"{}\":{:?}",
                                                 if v.len() > 1 { "s" } else { "" }, entry, v),
                                        &res);
                }
            }
        }
    }

    fn run_codegen_units_test(&self) {
        assert!(self.revision.is_none(), "revisions not relevant here");

        let proc_res = self.compile_test();

        if !proc_res.status.success() {
            self.fatal_proc_rec("compilation failed!", &proc_res);
        }

        self.check_no_compiler_crash(&proc_res);

        const PREFIX: &'static str = "TRANS_ITEM ";
        const CGU_MARKER: &'static str = "@@";

        let actual: Vec<TransItem> = proc_res
            .stdout
            .lines()
            .filter(|line| line.starts_with(PREFIX))
            .map(str_to_trans_item)
            .collect();

        let expected: Vec<TransItem> = errors::load_errors(&self.testpaths.file, None)
            .iter()
            .map(|e| str_to_trans_item(&e.msg[..]))
            .collect();

        let mut missing = Vec::new();
        let mut wrong_cgus = Vec::new();

        for expected_item in &expected {
            let actual_item_with_same_name = actual.iter()
                                                   .find(|ti| ti.name == expected_item.name);

            if let Some(actual_item) = actual_item_with_same_name {
                if !expected_item.codegen_units.is_empty() &&
                   // Also check for codegen units
                   expected_item.codegen_units != actual_item.codegen_units {
                    wrong_cgus.push((expected_item.clone(), actual_item.clone()));
                }
            } else {
                missing.push(expected_item.string.clone());
            }
        }

        let unexpected: Vec<_> =
            actual.iter()
                  .filter(|acgu| !expected.iter().any(|ecgu| acgu.name == ecgu.name))
                  .map(|acgu| acgu.string.clone())
                  .collect();

        if !missing.is_empty() {
            missing.sort();

            println!("\nThese items should have been contained but were not:\n");

            for item in &missing {
                println!("{}", item);
            }

            println!("\n");
        }

        if !unexpected.is_empty() {
            let sorted = {
                let mut sorted = unexpected.clone();
                sorted.sort();
                sorted
            };

            println!("\nThese items were contained but should not have been:\n");

            for item in sorted {
                println!("{}", item);
            }

            println!("\n");
        }

        if !wrong_cgus.is_empty() {
            wrong_cgus.sort_by_key(|pair| pair.0.name.clone());
            println!("\nThe following items were assigned to wrong codegen units:\n");

            for &(ref expected_item, ref actual_item) in &wrong_cgus {
                println!("{}", expected_item.name);
                println!("  expected: {}", codegen_units_to_str(&expected_item.codegen_units));
                println!("  actual:   {}", codegen_units_to_str(&actual_item.codegen_units));
                println!("");
            }
        }

        if !(missing.is_empty() && unexpected.is_empty() && wrong_cgus.is_empty())
        {
            panic!();
        }

        #[derive(Clone, Eq, PartialEq)]
        struct TransItem {
            name: String,
            codegen_units: HashSet<String>,
            string: String,
        }

        // [TRANS_ITEM] name [@@ (cgu)+]
        fn str_to_trans_item(s: &str) -> TransItem {
            let s = if s.starts_with(PREFIX) {
                (&s[PREFIX.len()..]).trim()
            } else {
                s.trim()
            };

            let full_string = format!("{}{}", PREFIX, s.trim().to_owned());

            let parts: Vec<&str> = s.split(CGU_MARKER)
                                    .map(str::trim)
                                    .filter(|s| !s.is_empty())
                                    .collect();

            let name = parts[0].trim();

            let cgus = if parts.len() > 1 {
                let cgus_str = parts[1];

                cgus_str.split(' ')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect()
            }
            else {
                HashSet::new()
            };

            TransItem {
                name: name.to_owned(),
                codegen_units: cgus,
                string: full_string,
            }
        }

        fn codegen_units_to_str(cgus: &HashSet<String>) -> String
        {
            let mut cgus: Vec<_> = cgus.iter().collect();
            cgus.sort();

            let mut string = String::new();
            for cgu in cgus {
                string.push_str(&cgu[..]);
                string.push_str(" ");
            }

            string
        }
    }

    fn init_incremental_test(&self) {
        // (See `run_incremental_test` for an overview of how incremental tests work.)

        // Before any of the revisions have executed, create the
        // incremental workproduct directory.  Delete any old
        // incremental work products that may be there from prior
        // runs.
        let incremental_dir = self.incremental_dir();
        if incremental_dir.exists() {
            // Canonicalizing the path will convert it to the //?/ format
            // on Windows, which enables paths longer than 260 character
            let canonicalized = incremental_dir.canonicalize().unwrap();
            fs::remove_dir_all(canonicalized).unwrap();
        }
        fs::create_dir_all(&incremental_dir).unwrap();

        if self.config.verbose {
            print!("init_incremental_test: incremental_dir={}", incremental_dir.display());
        }
    }

    fn run_incremental_test(&self) {
        // Basic plan for a test incremental/foo/bar.rs:
        // - load list of revisions rpass1, cfail2, rpass3
        //   - each should begin with `rpass`, `cfail`, or `cfail`
        //   - if `rpass`, expect compile and execution to succeed
        //   - if `cfail`, expect compilation to fail
        //   - if `rfail`, expect execution to fail
        // - create a directory build/foo/bar.incremental
        // - compile foo/bar.rs with -Z incremental=.../foo/bar.incremental and -C rpass1
        //   - because name of revision starts with "rpass", expect success
        // - compile foo/bar.rs with -Z incremental=.../foo/bar.incremental and -C cfail2
        //   - because name of revision starts with "cfail", expect an error
        //   - load expected errors as usual, but filter for those that end in `[rfail2]`
        // - compile foo/bar.rs with -Z incremental=.../foo/bar.incremental and -C rpass3
        //   - because name of revision starts with "rpass", expect success
        // - execute build/foo/bar.exe and save output
        //
        // FIXME -- use non-incremental mode as an oracle? That doesn't apply
        // to #[rustc_dirty] and clean tests I guess

        let revision = self.revision.expect("incremental tests require a list of revisions");

        // Incremental workproduct directory should have already been created.
        let incremental_dir = self.incremental_dir();
        assert!(incremental_dir.exists(), "init_incremental_test failed to create incremental dir");

        // Add an extra flag pointing at the incremental directory.
        let mut revision_props = self.props.clone();
        revision_props.incremental_dir = Some(incremental_dir);
        revision_props.compile_flags.push(String::from("-Zincremental-info"));

        let revision_cx = TestCx {
            config: self.config,
            props: &revision_props,
            testpaths: self.testpaths,
            revision: self.revision,
        };

        if self.config.verbose {
            print!("revision={:?} revision_props={:#?}", revision, revision_props);
        }

        if revision.starts_with("rpass") {
            revision_cx.run_rpass_test();
        } else if revision.starts_with("rfail") {
            revision_cx.run_rfail_test();
        } else if revision.starts_with("cfail") {
            revision_cx.run_cfail_test();
        } else {
            revision_cx.fatal(
                "revision name must begin with rpass, rfail, or cfail");
        }
    }

    /// Directory where incremental work products are stored.
    fn incremental_dir(&self) -> PathBuf {
        self.output_base_name().with_extension("inc")
    }

    fn run_rmake_test(&self) {
        // FIXME(#11094): we should fix these tests
        if self.config.host != self.config.target {
            return
        }

        let cwd = env::current_dir().unwrap();
        let src_root = self.config.src_base.parent().unwrap()
                                           .parent().unwrap()
                                           .parent().unwrap();
        let src_root = cwd.join(&src_root);

        let tmpdir = cwd.join(self.output_base_name());
        if tmpdir.exists() {
            self.aggressive_rm_rf(&tmpdir).unwrap();
        }
        create_dir_all(&tmpdir).unwrap();

        let host = &self.config.host;
        let make = if host.contains("bitrig") || host.contains("dragonfly") ||
            host.contains("freebsd") || host.contains("netbsd") ||
            host.contains("openbsd") {
            "gmake"
        } else {
            "make"
        };

        let mut cmd = Command::new(make);
        cmd.current_dir(&self.testpaths.file)
           .env("TARGET", &self.config.target)
           .env("PYTHON", &self.config.docck_python)
           .env("S", src_root)
           .env("RUST_BUILD_STAGE", &self.config.stage_id)
           .env("RUSTC", cwd.join(&self.config.rustc_path))
           .env("RUSTDOC",
               cwd.join(&self.config.rustdoc_path.as_ref().expect("--rustdoc-path passed")))
           .env("TMPDIR", &tmpdir)
           .env("LD_LIB_PATH_ENVVAR", dylib_env_var())
           .env("HOST_RPATH_DIR", cwd.join(&self.config.compile_lib_path))
           .env("TARGET_RPATH_DIR", cwd.join(&self.config.run_lib_path))
           .env("LLVM_COMPONENTS", &self.config.llvm_components)
           .env("LLVM_CXXFLAGS", &self.config.llvm_cxxflags);

        // We don't want RUSTFLAGS set from the outside to interfere with
        // compiler flags set in the test cases:
        cmd.env_remove("RUSTFLAGS");

        if self.config.target.contains("msvc") {
            // We need to pass a path to `lib.exe`, so assume that `cc` is `cl.exe`
            // and that `lib.exe` lives next to it.
            let lib = Path::new(&self.config.cc).parent().unwrap().join("lib.exe");

            // MSYS doesn't like passing flags of the form `/foo` as it thinks it's
            // a path and instead passes `C:\msys64\foo`, so convert all
            // `/`-arguments to MSVC here to `-` arguments.
            let cflags = self.config.cflags.split(' ').map(|s| s.replace("/", "-"))
                                                 .collect::<Vec<_>>().join(" ");

            cmd.env("IS_MSVC", "1")
               .env("IS_WINDOWS", "1")
               .env("MSVC_LIB", format!("'{}' -nologo", lib.display()))
               .env("CC", format!("'{}' {}", self.config.cc, cflags))
               .env("CXX", &self.config.cxx);
        } else {
            cmd.env("CC", format!("{} {}", self.config.cc, self.config.cflags))
               .env("CXX", format!("{} {}", self.config.cxx, self.config.cflags));

            if self.config.target.contains("windows") {
                cmd.env("IS_WINDOWS", "1");
            }
        }

        let output = cmd.output().expect("failed to spawn `make`");
        if !output.status.success() {
            let res = ProcRes {
                status: output.status,
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                cmdline: format!("{:?}", cmd),
            };
            self.fatal_proc_rec("make failed", &res);
        }
    }

    fn aggressive_rm_rf(&self, path: &Path) -> io::Result<()> {
        for e in path.read_dir()? {
            let entry = e?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                self.aggressive_rm_rf(&path)?;
            } else {
                // Remove readonly files as well on windows (by default we can't)
                fs::remove_file(&path).or_else(|e| {
                    if cfg!(windows) && e.kind() == io::ErrorKind::PermissionDenied {
                        let mut meta = entry.metadata()?.permissions();
                        meta.set_readonly(false);
                        fs::set_permissions(&path, meta)?;
                        fs::remove_file(&path)
                    } else {
                        Err(e)
                    }
                })?;
            }
        }
        fs::remove_dir(path)
    }

    fn run_ui_test(&self) {
        println!("ui: {}", self.testpaths.file.display());

        let proc_res = self.compile_test();

        let expected_stderr_path = self.expected_output_path("stderr");
        let expected_stderr = self.load_expected_output(&expected_stderr_path);

        let expected_stdout_path = self.expected_output_path("stdout");
        let expected_stdout = self.load_expected_output(&expected_stdout_path);

        let normalized_stdout =
            self.normalize_output(&proc_res.stdout, &self.props.normalize_stdout);
        let normalized_stderr =
            self.normalize_output(&proc_res.stderr, &self.props.normalize_stderr);

        let mut errors = 0;
        errors += self.compare_output("stdout", &normalized_stdout, &expected_stdout);
        errors += self.compare_output("stderr", &normalized_stderr, &expected_stderr);

        if errors > 0 {
            println!("To update references, run this command from build directory:");
            let relative_path_to_file =
                self.testpaths.relative_dir
                              .join(self.testpaths.file.file_name().unwrap());
            println!("{}/update-references.sh '{}' '{}'",
                     self.config.src_base.display(),
                     self.config.build_base.display(),
                     relative_path_to_file.display());
            self.fatal_proc_rec(&format!("{} errors occurred comparing output.", errors),
                                &proc_res);
        }

        if self.props.run_pass {
            let proc_res = self.exec_compiled_test();

            if !proc_res.status.success() {
                self.fatal_proc_rec("test run failed!", &proc_res);
            }
        }
    }

    fn run_mir_opt_test(&self) {
        let proc_res = self.compile_test();

        if !proc_res.status.success() {
            self.fatal_proc_rec("compilation failed!", &proc_res);
        }

        let proc_res = self.exec_compiled_test();

        if !proc_res.status.success() {
            self.fatal_proc_rec("test run failed!", &proc_res);
        }
        self.check_mir_dump();
    }

    fn check_mir_dump(&self) {
        let mut test_file_contents = String::new();
        fs::File::open(self.testpaths.file.clone()).unwrap()
                                                   .read_to_string(&mut test_file_contents)
                                                   .unwrap();
        if let Some(idx) =  test_file_contents.find("// END RUST SOURCE") {
            let (_, tests_text) = test_file_contents.split_at(idx + "// END_RUST SOURCE".len());
            let tests_text_str = String::from(tests_text);
            let mut curr_test : Option<&str> = None;
            let mut curr_test_contents = Vec::new();
            for l in tests_text_str.lines() {
                debug!("line: {:?}", l);
                if l.starts_with("// START ") {
                    let (_, t) = l.split_at("// START ".len());
                    curr_test = Some(t);
                } else if l.starts_with("// END") {
                    let (_, t) = l.split_at("// END ".len());
                    if Some(t) != curr_test {
                        panic!("mismatched START END test name");
                    }
                    self.compare_mir_test_output(curr_test.unwrap(), &curr_test_contents);
                    curr_test = None;
                    curr_test_contents.clear();
                } else if l.is_empty() {
                    // ignore
                } else if l.starts_with("// ") {
                    let (_, test_content) = l.split_at("// ".len());
                    curr_test_contents.push(test_content);
                }
            }
        }
    }

    fn check_mir_test_timestamp(&self, test_name: &str, output_file: &Path) {
        let t = |file| FileTime::from_last_modification_time(&fs::metadata(file).unwrap());
        let source_file = &self.testpaths.file;
        let output_time = t(output_file);
        let source_time = t(source_file);
        if source_time > output_time {
            debug!("source file time: {:?} output file time: {:?}", source_time, output_time);
            panic!("test source file `{}` is newer than potentially stale output file `{}`.",
                   source_file.display(), test_name);
        }
    }

    fn compare_mir_test_output(&self, test_name: &str, expected_content: &[&str]) {
        let mut output_file = PathBuf::new();
        output_file.push(self.get_mir_dump_dir());
        output_file.push(test_name);
        debug!("comparing the contests of: {:?}", output_file);
        debug!("with: {:?}", expected_content);
        self.check_mir_test_timestamp(test_name, &output_file);

        let mut dumped_file = fs::File::open(output_file.clone()).unwrap();
        let mut dumped_string = String::new();
        dumped_file.read_to_string(&mut dumped_string).unwrap();
        let mut dumped_lines = dumped_string.lines().filter(|l| !l.is_empty());
        let mut expected_lines = expected_content.iter().filter(|l| !l.is_empty());

        // We expect each non-empty line from expected_content to appear
        // in the dump in order, but there may be extra lines interleaved
        while let Some(expected_line) = expected_lines.next() {
            let e_norm = normalize_mir_line(expected_line);
            if e_norm.is_empty() {
                continue;
            };
            let mut found = false;
            while let Some(dumped_line) = dumped_lines.next() {
                let d_norm = normalize_mir_line(dumped_line);
                debug!("found: {:?}", d_norm);
                debug!("expected: {:?}", e_norm);
                if e_norm == d_norm {
                    found = true;
                    break;
                };
            }
            if !found {
                let normalize_all = dumped_string.lines()
                                                 .map(nocomment_mir_line)
                                                 .filter(|l| !l.is_empty())
                                                 .collect::<Vec<_>>()
                                                 .join("\n");
                panic!("ran out of mir dump output to match against.\n\
                        Did not find expected line: {:?}\n\
                        Expected:\n{}\n\
                        Actual:\n{}",
                        expected_line,
                        expected_content.join("\n"),
                        normalize_all);
            }
        }
    }

    fn get_mir_dump_dir(&self) -> PathBuf {
        let mut mir_dump_dir = PathBuf::from(self.config.build_base
                                                    .as_path()
                                                    .to_str()
                                                    .unwrap());
        debug!("input_file: {:?}", self.testpaths.file);
        mir_dump_dir.push(self.testpaths.file.file_stem().unwrap().to_str().unwrap());
        mir_dump_dir
    }

    fn normalize_output(&self, output: &str, custom_rules: &[(String, String)]) -> String {
        let parent_dir = self.testpaths.file.parent().unwrap();
        let parent_dir_str = parent_dir.display().to_string();
        let mut normalized = output.replace(&parent_dir_str, "$DIR")
              .replace("\\", "/") // normalize for paths on windows
              .replace("\r\n", "\n") // normalize for linebreaks on windows
              .replace("\t", "\\t"); // makes tabs visible
        for rule in custom_rules {
            normalized = normalized.replace(&rule.0, &rule.1);
        }
        normalized
    }

    fn expected_output_path(&self, kind: &str) -> PathBuf {
        let extension = match self.revision {
            Some(r) => format!("{}.{}", r, kind),
            None => kind.to_string(),
        };
        self.testpaths.file.with_extension(extension)
    }

    fn load_expected_output(&self, path: &Path) -> String {
        if !path.exists() {
            return String::new();
        }

        let mut result = String::new();
        match File::open(path).and_then(|mut f| f.read_to_string(&mut result)) {
            Ok(_) => result,
            Err(e) => {
                self.fatal(&format!("failed to load expected output from `{}`: {}",
                                    path.display(), e))
            }
        }
    }

    fn compare_output(&self, kind: &str, actual: &str, expected: &str) -> usize {
        if actual == expected {
            return 0;
        }

        println!("normalized {}:\n{}\n", kind, actual);
        println!("expected {}:\n{}\n", kind, expected);
        println!("diff of {}:\n", kind);

        for diff in diff::lines(expected, actual) {
            match diff {
                diff::Result::Left(l)    => println!("-{}", l),
                diff::Result::Both(l, _) => println!(" {}", l),
                diff::Result::Right(r)   => println!("+{}", r),
            }
        }

        let output_file = self.output_base_name().with_extension(kind);
        match File::create(&output_file).and_then(|mut f| f.write_all(actual.as_bytes())) {
            Ok(()) => { }
            Err(e) => {
                self.fatal(&format!("failed to write {} to `{}`: {}",
                                    kind, output_file.display(), e))
            }
        }

        println!("\nThe actual {0} differed from the expected {0}.", kind);
        println!("Actual {} saved to {}", kind, output_file.display());
        1
    }
}

struct ProcArgs {
    prog: String,
    args: Vec<String>,
}

pub struct ProcRes {
    status: ExitStatus,
    stdout: String,
    stderr: String,
    cmdline: String,
}

impl ProcRes {
    pub fn fatal(&self, err: Option<&str>) -> ! {
        if let Some(e) = err {
            println!("\nerror: {}", e);
        }
        print!("\
            status: {}\n\
            command: {}\n\
            stdout:\n\
            ------------------------------------------\n\
            {}\n\
            ------------------------------------------\n\
            stderr:\n\
            ------------------------------------------\n\
            {}\n\
            ------------------------------------------\n\
            \n",
               self.status, self.cmdline, self.stdout,
               self.stderr);
        panic!();
    }
}

enum TargetLocation {
    ThisFile(PathBuf),
    ThisDirectory(PathBuf),
}

fn normalize_mir_line(line: &str) -> String {
    nocomment_mir_line(line).replace(char::is_whitespace, "")
}

fn nocomment_mir_line(line: &str) -> &str {
    if let Some(idx) = line.find("//") {
        let (l, _) = line.split_at(idx);
        l.trim_right()
    } else {
        line
    }
}
