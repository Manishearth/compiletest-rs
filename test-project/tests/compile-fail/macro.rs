macro_rules! macro_with_error {
    ( ) => {
        let x: u64 = true;
    };
}

fn main() {
    macro_with_error!();  //~ ERROR mismatched types
}
