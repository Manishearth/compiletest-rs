compiletest-rs
==============

This project is an attempt at extracting the `compiletest` utility from the Rust
compiler.

The `compiletest` utility is useful for library and plugin developers, who want
to include test programs that should fail to compile, issue warnings or
otherwise produce compile-time output.


To use in your project
----------------------
To use `compiletest-rs` in your application, add the following to `Cargo.toml`

```
[dev-dependencies]
compiletest_rs = "*"
```

Create a `tests` folder in the root folder of your project. Create a test file
with something like the following:

```rust
extern crate compiletest_rs as compiletest;

use std::path::PathBuf;

fn run_mode(mode: &'static str) {
    let mut config = compiletest::Config::default();

    config.mode = mode.parse().expect("Invalid mode");
    config.src_base = PathBuf::from(format!("tests/{}", mode));
    config.link_deps(); // Populate config.target_rustcflags with dependencies on the path

    compiletest::run_tests(&config);
}

#[test]
fn compile_test() {
    run_mode("compile-fail");
    run_mode("run-pass");
}

```

Each mode corresponds to a folder with the same name in the `tests` folder. That
is for the `compile-fail` mode the test runner looks for the
`tests/compile-fail` folder.

Adding flags to the Rust compiler is a matter of assigning the correct field in
the config. The most common flag to populate is the
`target_rustcflags` to include the link dependencies on the path.

```rust
// NOTE! This is the manual way of adding flags
config.target_rustcflags = Some("-L target/debug".to_string());
```

This is useful (and necessary) for library development. Note that other
secondary library dependencies may have their build artifacts placed in
different (non-obvious) locations and these locations must also be
added.

For convenience, `Config` provides a `link_deps()` method that
populates `target_rustcflags` with all the dependencies found in the
`PATH` variable (which is OS specific). For most cases, it should be
sufficient to do:

```rust
let mut config = compiletest::Config::default();
config.link_deps();
```

Example
-------
See the `test-project` folder for a complete working example using the
`compiletest-rs` utility. Simply `cd test-project` and `cargo test` to see the
tests run.

TODO
----
 - The `run-pass` mode is strictly not necessary since it's baked right into
   Cargo, but I haven't bothered to take it out
