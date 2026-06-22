use anstyle::{AnsiColor, Style};
use comfy_table::{ContentArrangement, Table, presets::NOTHING};
use std::io::IsTerminal;

/// A borderless table that wraps/truncates its content to the terminal width.
///
/// `Dynamic` arrangement measures each cell's *visible* width — comfy-table's
/// `custom_styling` strips embedded OSC 8 hyperlink and ANSI colour escapes — so
/// styled, linked cells still wrap correctly instead of overflowing the screen.
pub fn table(headers: &[&str]) -> Table {
    let mut t = Table::new();
    t.load_preset(NOTHING);
    t.set_content_arrangement(ContentArrangement::Dynamic);
    t.set_width(term_width().min(u16::MAX as usize) as u16);
    t.set_header(headers.iter().map(|h| dim(h)));
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

/// Truncate to at most `max` visible characters, marking elision with `…`.
///
/// Operates on plain text (no escape awareness); apply before adding colour or
/// links so the ellipsis lands on a glyph boundary, not inside an escape.
pub fn truncate(s: &str, max: usize) -> String {
    if max == 0 || s.chars().count() <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}

// --- colour --------------------------------------------------------------------

/// Whether to emit ANSI colour. `NO_COLOR` (https://no-color.org) always wins;
/// `FORCE_COLOR` opts in even when piped (e.g. into `less -R`); otherwise colour
/// is emitted only to a real terminal. Piped output therefore stays plain by
/// default, which also keeps rendered-table tests deterministic.
fn color_enabled() -> bool {
    if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
        return false;
    }
    if std::env::var_os("FORCE_COLOR").is_some_and(|v| !v.is_empty()) {
        return true;
    }
    std::io::stdout().is_terminal()
}

fn paint(s: &str, style: Style) -> String {
    if color_enabled() {
        format!("{}{s}{}", style.render(), style.render_reset())
    } else {
        s.to_string()
    }
}

fn fg(s: &str, color: AnsiColor) -> String {
    paint(s, Style::new().fg_color(Some(color.into())))
}

/// `s` in green — merged PRs, completed issues, ready-to-land actions.
pub fn green(s: &str) -> String {
    fg(s, AnsiColor::Green)
}

/// `s` in yellow — in-progress states and "waiting on the other side" actions.
pub fn yellow(s: &str) -> String {
    fg(s, AnsiColor::Yellow)
}

/// `s` in red — closed/failed states and actions that need you now.
pub fn red(s: &str) -> String {
    fg(s, AnsiColor::Red)
}

/// `s` in cyan — identifiers (issue ids).
pub fn cyan(s: &str) -> String {
    fg(s, AnsiColor::Cyan)
}

/// `s` dimmed — passive/secondary values.
pub fn dim(s: &str) -> String {
    paint(s, Style::new().dimmed())
}

/// `s` in bold green — the headline "FINISHED" verdict.
pub fn bold_green(s: &str) -> String {
    paint(
        s,
        Style::new().bold().fg_color(Some(AnsiColor::Green.into())),
    )
}

/// `s` in bold cyan — section titles above each table.
pub fn bold_cyan(s: &str) -> String {
    paint(
        s,
        Style::new().bold().fg_color(Some(AnsiColor::Cyan.into())),
    )
}

/// `s` dimmed and struck through — the superseded half of an `old → new` diff.
pub fn dim_strike(s: &str) -> String {
    paint(s, Style::new().dimmed().strikethrough())
}

// --- terminal width ------------------------------------------------------------

/// Terminal width: `$COLUMNS`, else `TIOCGWINSZ`, else 100.
pub fn term_width() -> usize {
    if let Ok(c) = std::env::var("COLUMNS")
        && let Ok(n) = c.trim().parse::<usize>()
        && n > 0
    {
        return n;
    }
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let mut ws: Winsize = Winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let fd = std::io::stdout().as_raw_fd();
        // SAFETY: ws is a plain POD struct sized for struct winsize; TIOCGWINSZ fills it.
        let rc = unsafe { ioctl_winsize(fd, &mut ws) };
        if rc == 0 && ws.ws_col > 0 {
            return ws.ws_col as usize;
        }
    }
    100
}

#[cfg(unix)]
#[repr(C)]
struct Winsize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

#[cfg(unix)]
unsafe fn ioctl_winsize(fd: i32, ws: *mut Winsize) -> i32 {
    // TIOCGWINSZ is 0x5413 on Linux.
    unsafe extern "C" {
        fn ioctl(fd: i32, request: u64, ...) -> i32;
    }
    unsafe { ioctl(fd, 0x5413, ws) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use comfy_table::{ContentArrangement, Table, presets::NOTHING};

    #[test]
    fn link_plain_when_unsupported() {
        // In test env stdout is not a tty; link == label.
        assert_eq!(link("PR #1", "https://x"), "PR #1");
    }

    #[test]
    fn color_plain_when_not_a_tty() {
        // Tests do not run under a tty, so colour helpers pass text through
        // unchanged — keeping rendered output deterministic.
        assert_eq!(green("ok"), "ok");
        assert_eq!(dim("x"), "x");
        assert_eq!(cyan("ENG-1"), "ENG-1");
        assert_eq!(bold_green("FINISHED"), "FINISHED");
        assert_eq!(bold_cyan("TITLE"), "TITLE");
        assert_eq!(dim_strike("old"), "old");
    }

    #[test]
    fn truncate_elides_with_ellipsis() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("exactly-ten", 11), "exactly-ten");
        assert_eq!(truncate("abcdefghij", 5), "abcd…");
        assert_eq!(truncate("anything", 0), "anything");
    }

    /// Strip OSC 8 hyperlink sequences and SGR colour codes, leaving only the
    /// visible glyphs — used to measure on-screen width in tests.
    fn visible(s: &str) -> String {
        let mut out = String::new();
        let bytes = s.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == 0x1b {
                // OSC: ESC ] ... (ST = ESC \ or BEL)
                if i + 1 < bytes.len() && bytes[i + 1] == b']' {
                    i += 2;
                    while i < bytes.len() {
                        if bytes[i] == 0x07 {
                            i += 1;
                            break;
                        }
                        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                    continue;
                }
                // CSI/SGR: ESC [ ... letter
                if i + 1 < bytes.len() && bytes[i + 1] == b'[' {
                    i += 2;
                    while i < bytes.len() && !bytes[i].is_ascii_alphabetic() {
                        i += 1;
                    }
                    i += 1;
                    continue;
                }
            }
            out.push(bytes[i] as char);
            i += 1;
        }
        out
    }

    /// comfy-table's `custom_styling` must measure cell width by *visible*
    /// glyphs, ignoring embedded OSC 8 hyperlink and ANSI colour escapes. If it
    /// counted the ~40-char URL as content, the narrow-width table would either
    /// overflow or wrap the URL into view; neither happens when escapes are
    /// stripped for measurement.
    #[test]
    fn escapes_do_not_inflate_column_width() {
        let url = "https://linear.app/acme-workspace/issue/ENG-1234";
        let linked = format!("\x1b]8;;{url}\x1b\\\x1b[36mENG-1234\x1b[0m\x1b]8;;\x1b\\");

        let mut t = Table::new();
        t.load_preset(NOTHING);
        t.set_content_arrangement(ContentArrangement::Dynamic);
        t.set_width(30);
        t.set_header(["ISSUE", "NOTE"]);
        t.add_row(vec![linked, "short".to_string()]);

        let rendered = t.to_string();
        for line in rendered.lines() {
            let w = visible(line).chars().count();
            assert!(w <= 30, "line exceeds set width ({w} > 30): {line:?}");
            // The raw URL must never appear as visible text (it would mean the
            // link escape was treated as content and wrapped onto the row).
            assert!(
                !visible(line).contains("linear.app"),
                "URL leaked into visible output: {line:?}"
            );
        }
    }
}
