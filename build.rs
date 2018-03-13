use std::env;

pub fn main() {
    if env::var("CARGO_FEATURE_NORUSTC").is_ok() {
        println!("cargo:rustc-env=TARGET={}", env::var("TARGET").unwrap());
        println!("cargo:rustc-env=HOST={}", env::var("HOST").unwrap());
    }
}
