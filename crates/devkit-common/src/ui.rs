use comfy_table::{Table, presets::NOTHING};

/// A borderless table with the given header row.
pub fn table(headers: &[&str]) -> Table {
    let mut t = Table::new();
    t.load_preset(NOTHING);
    t.set_header(headers.iter().copied());
    t
}

/// OSC8 hyperlink when the terminal supports it; otherwise just the label.
pub fn link(label: &str, url: &str) -> String {
    if supports_hyperlinks::on(supports_hyperlinks::Stream::Stdout) {
        format!("\x1b]8;;{url}\x1b\\{label}\x1b]8;;\x1b\\")
    } else {
        label.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn link_plain_when_unsupported() {
        // In test env stdout is not a tty; link == label.
        assert_eq!(link("PR #1", "https://x"), "PR #1");
    }
}
