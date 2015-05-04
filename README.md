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
[dev-dependencies.compiletest]
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

Each mode corresponds to a folder with the same name in the `tests` folder. That
is for the `compile-fail` mode the test runner looks for the
`tests/compile-fail` folder.

Adding flags to the Rust compiler is a matter of assigning the correct field in
the config.

```rust
config.target_rustcflags = Some("-L target/debug".to_string());
```

This is useful (and necessary) for library development. Note that other
secondary library dependencies may have their build artifacts placed in
different (non-obvious) locations and these locations must also be added.

Example
-------
See the `test-project` folder for a complete working example using the
`compiletest-rs` utility. Simply `cd test-project` and `cargo test` to see the
tests run.

TODO
----
 - The `run-pass` mode is strictly not necessary since it's baked right into
   Cargo, but I haven't bothered to take it out
 - Find out if it is possible to capture the build flags during
   compilation. Then it should be possible to for `compiletest-rs` to capture
   (among other things) build dependencies (like `-L`). In the case a library
   would depend on a second library, the generated `.rlib` for the second
   library may end up in non-obvious places (and missing from the build
   path). Currently the work-around is to explicitly list the search paths as
   extra rustc flags.
