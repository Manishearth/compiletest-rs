extern crate compiletest;

use std::env;
use std::process::Command;
use std::path::PathBuf;

#[test]
fn test_invoke() {
    // note that there are a number of downsides to this approach, the comments
    // below detail how to improve the portability of these commands.
    Command::new("echo").args(&["Hello, ", "World!"])
        .status().unwrap();
}


#[test]
fn have_args() {
    let args: Vec<String> = env::args().collect();

    Command::new("echo").arg(format!("{:?}", args))
        .status().unwrap();

    assert!(args.len() > 0);
}

static LD_LIBRARY_PATH: &'static str = env!("LD_LIBRARY_PATH");

#[test]
fn print_ld_library_path() {
    Command::new("echo").arg(format!("{:?}", LD_LIBRARY_PATH)).status().unwrap();
}

fn run_mode(mode: &'static str) {

    let mut config = compiletest::default_config();
    let cfg_mode = mode.parse().ok().expect("Invalid mode");

    config.mode = cfg_mode;
    config.src_base = PathBuf::from(format!("tests/{}", mode));

    compiletest::run_tests(&config);
}

#[test]
fn compile_test() {
    run_mode("compile-fail");
    run_mode("run-pass");
}
