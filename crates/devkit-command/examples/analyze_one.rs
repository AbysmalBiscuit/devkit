//! Analyze one command read from stdin and print the time taken with the
//! analysis. `DIALECT` selects `bash` (default), `powershell`, or `fish`.
//!
//! ```sh
//! printf '%s' 'printf x > a.txt' | cargo run -p devkit-command --release --features corpus --example analyze_one
//! ```

use std::{io::Read, time::Instant};

use devkit_command::{Context, Dialect, Limits, PathStyle};

fn main() {
    let mut command = String::new();
    std::io::stdin()
        .read_to_string(&mut command)
        .expect("read command from stdin");
    let dialect = match std::env::var("DIALECT").as_deref() {
        Ok("powershell") => Dialect::PowerShell,
        Ok("fish") => Dialect::Fish,
        Ok("bash") | Err(_) => Dialect::Bash,
        Ok(other) => panic!("unknown DIALECT {other:?}: use bash, powershell, or fish"),
    };
    let ctx = Context {
        dialect,
        cwd: std::env::var("CWD").ok(),
        path_style: if dialect == Dialect::PowerShell {
            PathStyle::Windows
        } else {
            PathStyle::Unix
        },
        limits: Limits::default(),
    };
    let start = Instant::now();
    let analysis = devkit_command::analyze(&command, &ctx);
    let elapsed = start.elapsed();
    println!("{analysis:#?}\nanalyzed in {elapsed:.1?}");
}
