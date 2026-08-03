//! Safe cache path components for logical library names and raw git refs.

use anyhow::{Result, bail};

const CHECKOUT_RESERVED: [&str; 2] = ["repo.git", "meta.toml"];
const MAX_COMPONENT_BYTES: usize = 255;

pub fn encode(name: &str) -> String {
    name.replace('/', "~")
}

pub fn decode(dir: &str) -> String {
    dir.replace('~', "/")
}

pub fn fold_key(component: &str) -> String {
    component.to_ascii_lowercase()
}

fn reject_traversal(name: &str) -> Result<()> {
    if name
        .split(['/', '\\'])
        .any(|component| component == "." || component == "..")
    {
        bail!("`{name}` contains a path traversal component");
    }
    Ok(())
}

pub fn validate_lib(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("library name is empty");
    }
    if name.contains('~') {
        bail!(
            "library name `{name}` contains `~`, which docm uses to encode `/` in cache paths; \\
             pass --package to register it under a different name"
        );
    }
    reject_traversal(name)?;
    let folded = fold_key(name);
    if folded == "registry" || folded.starts_with("registry.") {
        bail!(
            "library name `{name}` is reserved: the docs cache keeps its reference registry \\
             at <cache>/registry.* and a library directory there would shadow it"
        );
    }
    representable(&encode(name))
}

pub fn validate_ref(git_ref: &str) -> Result<()> {
    if git_ref.is_empty() {
        bail!("ref is empty");
    }
    if git_ref.contains('~') {
        bail!("`{git_ref}` contains `~`, which is illegal in a git ref name");
    }
    reject_traversal(git_ref)?;
    let dir = encode(git_ref);
    if CHECKOUT_RESERVED
        .iter()
        .any(|reserved| fold_key(reserved) == fold_key(&dir))
    {
        bail!("ref `{git_ref}` collides with a control file inside the library directory");
    }
    representable(&dir)
}

pub fn lib_dir(name: &str) -> Result<String> {
    validate_lib(name)?;
    Ok(encode(name))
}

pub fn checkout_dir(git_ref: &str) -> Result<String> {
    validate_ref(git_ref)?;
    Ok(encode(git_ref))
}

fn representable(component: &str) -> Result<()> {
    if component.len() > MAX_COMPONENT_BYTES {
        bail!(
            "`{component}` is {} bytes; a path component cannot exceed {MAX_COMPONENT_BYTES}",
            component.len()
        );
    }
    if cfg!(windows) {
        if let Some(character) = component
            .chars()
            .find(|character| "<>:\"|?*\\\\".contains(*character))
        {
            bail!("`{component}` contains `{character}`, which Windows does not allow in a path");
        }
        let stem = component
            .split('.')
            .next()
            .unwrap_or(component)
            .to_ascii_uppercase();
        let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem[3..]
                    .parse::<u8>()
                    .is_ok_and(|number| (1..=9).contains(&number));
        if reserved {
            bail!("`{component}` is a reserved device name on Windows");
        }
    }
    Ok(())
}
