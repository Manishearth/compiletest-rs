// Copyright 2013 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.


trait Foo {
    fn dummy(&self) { }
}

fn a(_x: Box<Foo+Send>) {
}

fn c(x: Box<Foo+Sync+Send>) {
    a(x);
}

fn d(x: Box<Foo>) {
    a(x); //~  ERROR mismatched types
          //~| expected trait `Foo + std::marker::Send`, found trait `Foo`
          //~| expected type `std::boxed::Box<(dyn Foo + std::marker::Send + 'static)>`
          //~| found type `std::boxed::Box<(dyn Foo + 'static)>`
}

fn main() { }
