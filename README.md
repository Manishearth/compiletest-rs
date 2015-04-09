compiletest-rs
==============

This project is an attempt at extracting the `compiletest` utility from the Rust
compiler.



To use in your project
----------------------
To use `compiletest-rs` in your application, add the following to `Cargo.toml`

```
[dependencies.compiletest]
git = "https://github.com/laumann/compiletest-rs.git"
```

Create a `tests` folder in the root folder of your project. Create a test file
with something like the following:

```rust
extern crate compiletest;

use std::path::PathBuf;

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

```

Adding flags to the Rust compiler is a matter of assigning the correct field in
the config.

```rust
config.target_rustcflags = Some("-L target/debug".to_string());
```
