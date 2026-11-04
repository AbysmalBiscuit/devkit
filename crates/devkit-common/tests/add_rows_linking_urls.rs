use devkit_common::ui;
use devkit_git::child_env::child_env;

// The real 8-column `devrun status` shape: PORT APP ROLE HOLDER PID LISTENING
// AGE URL, URL last at index 7.
const HEADERS: [&str; 8] = [
    "PORT",
    "APP",
    "ROLE",
    "HOLDER",
    "PID",
    "LISTENING",
    "AGE",
    "URL",
];
const LONG_URL: &str = "https://app.someview.example.com/some/very/long/path/that/is/quite/long";
const SHORT_URL: &str = "http://localhost:39240";
const ELLIPSIS: char = '\u{2026}';

fn row(url: &str) -> Vec<String> {
    vec![
        "39240".to_string(),
        "frontend-dashboard".to_string(),
        "issue".to_string(),
        "osc8repro".to_string(),
        "-".to_string(),
        "no".to_string(),
        "44s".to_string(),
        url.to_string(),
    ]
}

fn introducer_lines(rendered: &str) -> usize {
    rendered.lines().filter(|l| l.contains("\x1b]8;;")).count()
}

fn render(url: &str) -> String {
    let mut table = ui::table(&HEADERS);
    ui::add_rows_linking_urls(&mut table, vec![row(url)], 7);
    table.to_string()
}

/// Wide enough that the other 7 columns leave plenty of room: the URL is linked
/// and the escape survives intact on one rendered line.
#[test]
fn a_wide_terminal_links_the_url() {
    child_env()
        .set("DEVKIT_HYPERLINKS", "always")
        .set("COLUMNS", "200")
        .run(|| {
            assert!(LONG_URL.chars().count() > ui::URL_LABEL_MAX);
            let rendered = render(LONG_URL);
            assert_eq!(
                introducer_lines(&rendered),
                1,
                "expected exactly one OSC 8 introducer at 200 columns: {rendered:?}"
            );

            // A URL shorter than the label budget renders untruncated (no
            // ellipsis).
            let rendered = render(SHORT_URL);
            assert!(
                rendered.contains(SHORT_URL),
                "a url under the budget must render whole: {rendered:?}"
            );
            assert!(
                !rendered.contains(ELLIPSIS),
                "a url under the budget must not be truncated: {rendered:?}"
            );
        });
}

/// At 60 columns the natural width of the other 7 columns already exceeds what
/// a pinned URL column could leave them, so the URL falls back to plain text
/// instead of crushing every other column to a single character.
#[test]
fn a_narrow_terminal_falls_back_to_a_plain_url() {
    child_env()
        .set("DEVKIT_HYPERLINKS", "always")
        .set("COLUMNS", "60")
        .run(|| {
            let rendered = render(LONG_URL);
            assert_eq!(
                introducer_lines(&rendered),
                0,
                "expected no OSC 8 escape once the url column can't fit at 60: {rendered:?}"
            );
            // Even a healthy plain-text rendering compresses "LISTENING", the
            // widest.
            for header in HEADERS
                .iter()
                .filter(|h| !matches!(**h, "URL" | "LISTENING"))
            {
                assert!(
                    rendered.contains(header),
                    "{header} must not be reduced below its own width at 60: {rendered:?}"
                );
            }
        });
}

/// Without hyperlink support every cell, URL included, passes through unchanged
/// and no column is constrained, at whatever width.
#[test]
fn without_hyperlink_support_no_url_is_linked() {
    child_env()
        .set("DEVKIT_HYPERLINKS", "never")
        .set("COLUMNS", "60")
        .run(|| {
            let rendered = render(LONG_URL);
            assert_eq!(
                introducer_lines(&rendered),
                0,
                "no OSC 8 escape without hyperlink support: {rendered:?}"
            );
        });
}
