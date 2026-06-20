use anyhow::Result;

fn main() -> Result<()> {
    devkit_common::report::install_panic_hook("issue");
    println!("issue: not yet wired");
    Ok(())
}
