use anstyle::{AnsiColor, Style};
use comfy_table::{ContentArrangement, Table, presets::NOTHING};
use std::io::IsTerminal;

/// A borderless table that wraps/truncates its content to the terminal width.
///
/// `Dynamic` arrangement measures each cell's *visible* width — comfy-table's
/// `custom_styling` strips embedded OSC 8 hyperlink and ANSI colour escapes — so
/// styled, linked cells still wrap correctly instead of overflowing the screen.
pub fn table(headers: &[&str]) -> Table {
    table_on(Stream::Stdout, headers)
}

/// [`table`] sized and coloured for `stream` — the live stderr block sizes
/// and styles to stderr's terminal even when stdout is redirected.
pub fn table_on(stream: Stream, headers: &[&str]) -> Table {
    let paint = Paint::on(stream);
    let mut t = Table::new();
    t.load_preset(NOTHING);
    t.set_content_arrangement(ContentArrangement::Dynamic);
    t.set_width(term_width_on(stream).min(u16::MAX as usize) as u16);
    t.set_header(headers.iter().map(|h| paint.dim(h)));
    t
}

/// Whether stdout is a terminal — the signal a command uses to render its
/// result for a reader instead of emitting the JSON a caller would parse.
pub fn stdout_is_tty() -> bool {
    std::io::stdout().is_terminal()
}

/// A borderless two-column `label  value` table for a command's result.
///
/// Content arrangement is disabled on purpose: a wrapped path cannot be
/// double-clicked out of the line, and a path that runs past the terminal edge
/// is the lesser problem.
pub fn kv_table(rows: &[(&str, String)]) -> Table {
    let paint = Paint::on(Stream::Stdout);
    let mut t = Table::new();
    t.load_preset(NOTHING);
    t.set_content_arrangement(ContentArrangement::Disabled);
    for (label, value) in rows {
        t.add_row(vec![paint.dim(label), value.clone()]);
    }
    t
}

/// OSC8 hyperlink when the terminal supports it; otherwise just the label.
pub fn link(label: &str, url: &str) -> String {
    link_styled(hyperlinks_enabled_on(Stream::Stdout), label, url)
}

/// Whether to emit OSC 8 hyperlinks on `stream`. `DEVKIT_HYPERLINKS` overrides
/// detection — `always`/`1`/`on`/`true`/`yes` forces links, `never`/`0`/`off`/
/// `false`/`no` suppresses them — for terminals `supports-hyperlinks` can't
/// identify. Some capable terminals (e.g. alacritty forks that report a bare
/// `TERM=xterm-256color` with no `TERM_PROGRAM`) render OSC 8 fine yet fail its
/// detection, degrading links to plain text; the override restores them. Unset
/// falls back to `supports-hyperlinks`, which gates on both TTY-ness and a
/// recognised terminal.
pub(crate) fn hyperlinks_enabled_on(stream: Stream) -> bool {
    if let Ok(v) = std::env::var("DEVKIT_HYPERLINKS")
        && let Some(force) = parse_flag(&v)
    {
        return force;
    }
    let s = match stream {
        Stream::Stdout => supports_hyperlinks::Stream::Stdout,
        Stream::Stderr => supports_hyperlinks::Stream::Stderr,
    };
    supports_hyperlinks::on(s)
}

/// Parse a tri-state boolean env value. `1`/`true`/`on`/`always`/`yes` → on,
/// `0`/`false`/`off`/`never`/`no` → off (case- and whitespace-insensitive);
/// anything else — including empty — is `None`, meaning "no opinion, detect".
fn parse_flag(v: &str) -> Option<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "on" | "always" | "yes" => Some(true),
        "0" | "false" | "off" | "never" | "no" => Some(false),
        _ => None,
    }
}

/// Render `label` as an OSC8 hyperlink to `url` when `supported`, else the bare
/// label. Split from `link` so the formatting is testable without depending on
/// ambient terminal or `FORCE_HYPERLINK` detection.
fn link_styled(supported: bool, label: &str, url: &str) -> String {
    if supported {
        format!("\x1b]8;;{url}\x1b\\{label}\x1b]8;;\x1b\\")
    } else {
        label.to_string()
    }
}

pub use devkit_config::DEFAULT_BRANCH_MAX as BRANCH_DISPLAY_MAX;

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

/// An output stream that colour and width decisions key off. Final rendered
/// output goes to stdout; live blocks (step logs, live tables) draw on
/// stderr. Each judges TTY-ness on its own stream, so `cmd > file` keeps the
/// live stderr block styled without leaking ANSI into the redirected stdout.
#[derive(Clone, Copy)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// Whether to emit ANSI colour on `stream`. `NO_COLOR` (https://no-color.org)
/// always wins; `FORCE_COLOR` opts in even when piped (e.g. into `less -R`);
/// otherwise colour is emitted only when that stream is a real terminal.
pub(crate) fn color_enabled_on(stream: Stream) -> bool {
    let is_tty = match stream {
        Stream::Stdout => std::io::stdout().is_terminal(),
        Stream::Stderr => std::io::stderr().is_terminal(),
    };
    color_choice(
        std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()),
        std::env::var_os("FORCE_COLOR").is_some_and(|v| !v.is_empty()),
        is_tty,
    )
}

/// The colour precedence: `NO_COLOR` beats `FORCE_COLOR`, which beats terminal
/// detection. Split from [`color_enabled_on`] so the rule is testable without
/// reading process-global environment.
fn color_choice(no_color: bool, force_color: bool, is_tty: bool) -> bool {
    !no_color && (force_color || is_tty)
}

/// Colour helpers keyed to one [`Stream`]. The free helpers (`green`, `dim`,
/// …) paint for stdout — the final rendered output; anything drawn live on
/// stderr styles through `Paint::on(Stream::Stderr)`.
#[derive(Clone, Copy)]
pub struct Paint {
    enabled: bool,
}

impl Paint {
    pub fn on(stream: Stream) -> Paint {
        Paint {
            enabled: color_enabled_on(stream),
        }
    }

    fn paint(&self, s: &str, style: Style) -> String {
        if self.enabled {
            format!("{}{s}{}", style.render(), style.render_reset())
        } else {
            s.to_string()
        }
    }

    fn fg(&self, s: &str, color: AnsiColor) -> String {
        self.paint(s, Style::new().fg_color(Some(color.into())))
    }

    /// `s` in green — merged PRs, completed issues, ready-to-land actions.
    pub fn green(&self, s: &str) -> String {
        self.fg(s, AnsiColor::Green)
    }

    /// `s` in yellow — in-progress states and "waiting on the other side"
    /// actions.
    pub fn yellow(&self, s: &str) -> String {
        self.fg(s, AnsiColor::Yellow)
    }

    /// `s` in red — closed/failed states and actions that need you now.
    pub fn red(&self, s: &str) -> String {
        self.fg(s, AnsiColor::Red)
    }

    /// `s` in cyan — identifiers (issue ids).
    pub fn cyan(&self, s: &str) -> String {
        self.fg(s, AnsiColor::Cyan)
    }

    /// `s` dimmed — passive/secondary values.
    pub fn dim(&self, s: &str) -> String {
        self.paint(s, Style::new().dimmed())
    }

    /// `s` dimmed even when it embeds styled spans. A painted span ends in an
    /// SGR reset, which would cancel a plain outer dim for the rest of the
    /// line; here dim is re-asserted after every embedded reset so the whole
    /// line stays dim.
    pub fn dim_all(&self, s: &str) -> String {
        if self.enabled {
            format!("\x1b[2m{}\x1b[0m", s.replace("\x1b[0m", "\x1b[0m\x1b[2m"))
        } else {
            s.to_string()
        }
    }

    /// `s` in bold green — the headline "FINISHED" verdict.
    pub fn bold_green(&self, s: &str) -> String {
        self.paint(
            s,
            Style::new().bold().fg_color(Some(AnsiColor::Green.into())),
        )
    }

    /// `s` in bold cyan — section titles above each table.
    pub fn bold_cyan(&self, s: &str) -> String {
        self.paint(
            s,
            Style::new().bold().fg_color(Some(AnsiColor::Cyan.into())),
        )
    }

    /// `s` dimmed and struck through — the superseded half of an `old → new`
    /// diff.
    pub fn dim_strike(&self, s: &str) -> String {
        self.paint(s, Style::new().dimmed().strikethrough())
    }
}

fn stdout_paint() -> Paint {
    Paint::on(Stream::Stdout)
}

/// `s` in green — merged PRs, completed issues, ready-to-land actions.
pub fn green(s: &str) -> String {
    stdout_paint().green(s)
}

/// `s` in yellow — in-progress states and "waiting on the other side" actions.
pub fn yellow(s: &str) -> String {
    stdout_paint().yellow(s)
}

/// `s` in red — closed/failed states and actions that need you now.
pub fn red(s: &str) -> String {
    stdout_paint().red(s)
}

/// `s` in cyan — identifiers (issue ids).
pub fn cyan(s: &str) -> String {
    stdout_paint().cyan(s)
}

/// `s` dimmed — passive/secondary values.
pub fn dim(s: &str) -> String {
    stdout_paint().dim(s)
}

/// `s` dimmed even when it embeds styled spans; see [`Paint::dim_all`].
pub fn dim_all(s: &str) -> String {
    stdout_paint().dim_all(s)
}

/// `s` in bold green — the headline "FINISHED" verdict.
pub fn bold_green(s: &str) -> String {
    stdout_paint().bold_green(s)
}

/// `s` in bold cyan — section titles above each table.
pub fn bold_cyan(s: &str) -> String {
    stdout_paint().bold_cyan(s)
}

/// `s` dimmed and struck through — the superseded half of an `old → new` diff.
pub fn dim_strike(s: &str) -> String {
    stdout_paint().dim_strike(s)
}

// --- terminal width ------------------------------------------------------------

/// Terminal width of stdout: `$COLUMNS`, else `TIOCGWINSZ`, else 100.
pub fn term_width() -> usize {
    term_width_on(Stream::Stdout)
}

/// Terminal width of `stream`: `$COLUMNS`, else `TIOCGWINSZ` on that stream's
/// fd, else 100.
pub fn term_width_on(stream: Stream) -> usize {
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
        let fd = match stream {
            Stream::Stdout => std::io::stdout().as_raw_fd(),
            Stream::Stderr => std::io::stderr().as_raw_fd(),
        };
        // SAFETY: ws is a plain POD struct sized for struct winsize; TIOCGWINSZ fills it.
        let rc = unsafe { ioctl_winsize(fd, &mut ws) };
        if rc == 0 && ws.ws_col > 0 {
            return ws.ws_col as usize;
        }
    }
    #[cfg(not(unix))]
    let _ = stream;
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
        // Without hyperlink support the bare label is returned, regardless of
        // ambient terminal/`FORCE_HYPERLINK` detection.
        assert_eq!(link_styled(false, "PR #1", "https://x"), "PR #1");
    }

    #[test]
    fn link_emits_osc8_when_supported() {
        assert_eq!(
            link_styled(true, "PR #1", "https://x"),
            "\x1b]8;;https://x\x1b\\PR #1\x1b]8;;\x1b\\"
        );
    }

    #[test]
    fn parse_flag_reads_boolean_words() {
        for on in ["1", "true", "on", "always", "yes", "TRUE", " Always "] {
            assert_eq!(parse_flag(on), Some(true), "{on:?} should force on");
        }
        for off in ["0", "false", "off", "never", "no", "Never"] {
            assert_eq!(parse_flag(off), Some(false), "{off:?} should force off");
        }
        // No opinion → fall back to terminal detection.
        for none in ["", "  ", "maybe", "auto"] {
            assert_eq!(parse_flag(none), None, "{none:?} should defer to detection");
        }
    }

    #[test]
    fn no_color_beats_force_color_beats_tty() {
        assert!(!color_choice(true, true, true));
        assert!(!color_choice(true, false, true));
        assert!(color_choice(false, true, false));
        assert!(color_choice(false, false, true));
        assert!(!color_choice(false, false, false));
    }

    #[test]
    fn dim_all_reasserts_dim_after_embedded_resets() {
        // An embedded span's own reset must not cancel the outer dim for the
        // rest of the line.
        let on = Paint { enabled: true };
        assert_eq!(
            on.dim_all("a \x1b[32mok\x1b[0m b"),
            "\x1b[2ma \x1b[32mok\x1b[0m\x1b[2m b\x1b[0m"
        );
        // Colour off: passthrough, like every other colour helper.
        let off = Paint { enabled: false };
        assert_eq!(
            off.dim_all("a \x1b[32mok\x1b[0m b"),
            "a \x1b[32mok\x1b[0m b"
        );
    }

    #[test]
    fn paint_styles_when_enabled() {
        // An enabled Paint (a stream that is a terminal) emits SGR codes; a
        // disabled one passes through, whatever stream it came from.
        let on = Paint { enabled: true };
        assert_eq!(on.green("ok"), "\x1b[32mok\x1b[0m");
        assert_eq!(on.dim("x"), "\x1b[2mx\x1b[0m");
        let off = Paint { enabled: false };
        assert_eq!(off.green("ok"), "ok");
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

    #[test]
    fn kv_table_leaves_values_unwrapped_and_unquoted() {
        let long = "/home/lev/Git/adaptyv/swe-11341-api-delete-the-dead-flag-entry-and-archive"
            .to_string();
        let t = kv_table(&[("worktree", long.clone()), ("branch", "lev/x".to_string())]);
        let out = t.to_string();
        assert!(
            out.lines().any(|l| visible(l).contains(&long)),
            "the path must survive on one line: {out}"
        );
        assert!(!out.contains('"'), "values are not quoted: {out}");
    }
}
