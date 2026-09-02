use devkit_common::ui;

/// `DEVKIT_HYPERLINKS` and `COLUMNS` are both process environment, racy under
/// a parallel test runner, so every scenario lives in one sequential test —
/// the same isolation `devkit_hyperlinks_env_overrides_detection` uses.
#[test]
fn add_rows_linking_urls_fits_or_falls_back() {
    // The real 8-column `devrun status` shape: PORT APP ROLE HOLDER PID
    // LISTENING AGE URL, URL last at index 7.
    let headers = [
        "PORT",
        "APP",
        "ROLE",
        "HOLDER",
        "PID",
        "LISTENING",
        "AGE",
        "URL",
    ];
    let long_url = "https://app.someview.example.com/some/very/long/path/that/is/quite/long";
    let short_url = "http://localhost:39240";
    assert!(long_url.chars().count() > ui::URL_LABEL_MAX);

    let row = |url: &str| {
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
    };
    let introducer_lines =
        |rendered: &str| rendered.lines().filter(|l| l.contains("\x1b]8;;")).count();

    unsafe { std::env::set_var("DEVKIT_HYPERLINKS", "always") };

    // Wide enough that the other 7 columns leave plenty of room: the URL is
    // linked and the escape survives intact on one rendered line.
    unsafe { std::env::set_var("COLUMNS", "200") };
    let mut wide = ui::table(&headers);
    ui::add_rows_linking_urls(&mut wide, vec![row(long_url)], 7);
    let rendered = wide.to_string();
    assert_eq!(
        introducer_lines(&rendered),
        1,
        "expected exactly one OSC 8 introducer at 200 columns: {rendered:?}"
    );

    // A URL shorter than the label budget renders untruncated (no ellipsis).
    let mut short = ui::table(&headers);
    ui::add_rows_linking_urls(&mut short, vec![row(short_url)], 7);
    let rendered = short.to_string();
    assert!(
        rendered.contains(short_url),
        "a url under the budget must render whole: {rendered:?}"
    );
    assert!(
        !rendered.contains('…'),
        "a url under the budget must not be truncated: {rendered:?}"
    );

    // At 60 columns the natural width of the other 7 columns already exceeds
    // what a pinned URL column could leave them, so the fix must fall the
    // URL back to plain text instead of crushing every other column to a
    // single character — the bug this test guards against.
    unsafe { std::env::set_var("COLUMNS", "60") };
    let mut narrow = ui::table(&headers);
    ui::add_rows_linking_urls(&mut narrow, vec![row(long_url)], 7);
    let rendered = narrow.to_string();
    assert_eq!(
        introducer_lines(&rendered),
        0,
        "expected no OSC 8 escape once the url column can't fit at 60: {rendered:?}"
    );
    // Every header except the widest ("LISTENING", which even the healthy
    // plain-text rendering compresses to fit) survives intact — unlike the
    // unconditional 40-wide pin, which crushed every column, including
    // `PORT`, down to a single character.
    for header in headers
        .iter()
        .filter(|h| !matches!(**h, "URL" | "LISTENING"))
    {
        assert!(
            rendered.contains(header),
            "{header} must not be reduced below its own width at 60: {rendered:?}"
        );
    }

    // Without hyperlink support every cell — URL included — passes through
    // unchanged and no column is constrained, at whatever width.
    unsafe { std::env::set_var("DEVKIT_HYPERLINKS", "never") };
    let mut plain = ui::table(&headers);
    ui::add_rows_linking_urls(&mut plain, vec![row(long_url)], 7);
    let rendered = plain.to_string();
    assert_eq!(
        introducer_lines(&rendered),
        0,
        "no OSC 8 escape without hyperlink support: {rendered:?}"
    );

    unsafe { std::env::remove_var("DEVKIT_HYPERLINKS") };
    unsafe { std::env::remove_var("COLUMNS") };
}
