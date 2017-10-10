extern crate testp;
//~^ ERROR E0464
//~| ERROR E0463
// ISSUE#78: This test requires `cargo check` before `cargo test` to
// fail compilation
fn main() {}
