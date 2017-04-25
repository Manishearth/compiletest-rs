// Copyright 2015 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

#[derive(Copy, Clone)]
struct S;

impl S {
    fn mutate(&mut self) {
    }
}

fn func(arg: S) {
    //~^ NOTE consider changing this to `mut arg`
    arg.mutate(); //~ ERROR cannot borrow immutable argument
                  //~| NOTE cannot borrow mutably
}

fn main() {
    let local = S;
    //~^ NOTE consider changing this to `mut local`
    local.mutate(); //~ ERROR cannot borrow immutable local variable
                    //~| NOTE cannot borrow mutably
}
