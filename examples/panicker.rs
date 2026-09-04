//! Drives each panic hook in a real process.
//!
//! What the two hooks do differently — die versus unwind — is only observable
//! from outside, so `tests/abort_hook.rs` runs this and reads the exit status.
//! The first argument picks the hook; either way the process then panics.

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("abort") => devkit_common::report::install_abort_hook("panicker"),
        _ => devkit_common::report::install_panic_hook("panicker"),
    }
    panic!("deliberate panic");
}
