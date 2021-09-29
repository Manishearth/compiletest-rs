#![feature(prelude_import)]
#![no_std]
#[prelude_import]
use ::std::prelude::rust_2015::*;
#[macro_use]
extern crate std;
// pretty-compare-only
// pretty-mode:expanded
// pp-exact:macro.pp

macro_rules! square { ($x : expr) => { $x * $x } ; }

fn f() -> i8 { 5 * 5 }
