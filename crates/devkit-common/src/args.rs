//! `--arg key=value` and `--arg-file key=path`, the two ways every command
//! that renders templates takes a variable. One parser, so no command accepts
//! a spelling another refuses.

use std::{collections::BTreeMap, io::Read};

use anyhow::{Context, Result, bail};

/// Parse `--arg` pairs and `--arg-file` pairs into one map. A file's contents
/// become the value unchanged, trailing newline included; a path of `-` reads
/// stdin. Naming one key through both flags, or `-` twice, is refused.
pub fn parse(pairs: &[String], files: &[String]) -> Result<BTreeMap<String, String>> {
    parse_with_stdin(pairs, files, &mut std::io::stdin())
}

fn parse_with_stdin(
    pairs: &[String],
    files: &[String],
    stdin: &mut dyn Read,
) -> Result<BTreeMap<String, String>> {
    let mut out: BTreeMap<String, String> = pairs
        .iter()
        .map(|pair| split(pair, "--arg", "key=value"))
        .collect::<Result<_>>()?;
    let mut stdin_read = false;
    for pair in files {
        let (key, path) = split(pair, "--arg-file", "key=path")?;
        if out.contains_key(&key) {
            bail!("`{key}` is given more than once across --arg and --arg-file");
        }
        let value = if path == "-" {
            if stdin_read {
                bail!("--arg-file reads stdin (`-`) at most once");
            }
            stdin_read = true;
            let mut s = String::new();
            stdin
                .read_to_string(&mut s)
                .with_context(|| format!("reading --arg-file {key} from stdin"))?;
            s
        } else {
            std::fs::read_to_string(&path)
                .with_context(|| format!("reading --arg-file {key}={path}"))?
        };
        out.insert(key, value);
    }
    Ok(out)
}

fn split(pair: &str, flag: &str, shape: &str) -> Result<(String, String)> {
    pair.split_once('=')
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .with_context(|| format!("{flag} must be {shape}, got `{pair}`"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_file_value_is_its_contents_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("body.md");
        std::fs::write(&path, "one\ntwo\n").unwrap();
        let out = parse(&strings(&["a=1"]), &[format!("body={}", path.display())]).unwrap();
        assert_eq!(out["a"], "1");
        assert_eq!(out["body"], "one\ntwo\n");
    }

    #[test]
    fn dash_reads_stdin_once() {
        let out = parse_with_stdin(&[], &strings(&["body=-"]), &mut "in\n".as_bytes()).unwrap();
        assert_eq!(out["body"], "in\n");

        let err = parse_with_stdin(&[], &strings(&["a=-", "b=-"]), &mut "x".as_bytes())
            .unwrap_err()
            .to_string();
        assert!(err.contains("at most once"), "{err}");
    }

    #[test]
    fn one_key_through_both_flags_is_refused() {
        let err = parse_with_stdin(
            &strings(&["msg=x"]),
            &strings(&["msg=-"]),
            &mut "".as_bytes(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("`msg`"), "{err}");
    }

    #[test]
    fn a_pair_without_equals_is_refused() {
        assert!(parse(&strings(&["noeq"]), &[]).is_err());
        assert!(parse(&[], &strings(&["noeq"])).is_err());
    }

    #[test]
    fn a_missing_file_names_the_arg() {
        let err = parse(&[], &strings(&["body=/no/such/file"])).unwrap_err();
        assert!(format!("{err:#}").contains("body=/no/such/file"), "{err:#}");
    }
}
