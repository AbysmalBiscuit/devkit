//! Gives the binaries on Windows the 8 MiB main-thread stack Linux and macOS
//! default to, in place of Windows' 1 MiB. Building the clap command tree
//! takes close to 1 MiB in an unoptimized build, so on Windows any new flag
//! could overflow it before a command runs.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    const STACK: u32 = 8 * 1024 * 1024;
    match std::env::var("CARGO_CFG_TARGET_ENV").as_deref() {
        Ok("msvc") => println!("cargo:rustc-link-arg-bins=/STACK:{STACK}"),
        Ok("gnu") => println!("cargo:rustc-link-arg-bins=-Wl,--stack,{STACK}"),
        _ => {}
    }
}
