fn foo() {
    let x: (u64, bool, bool) = (true, 42u64, 666u64);
    //~^ ERROR mismatched types
}

fn bar() {
    let x: (u64, bool) = (true, 42u64);
    //~^ ERROR*2 mismatched types
}
