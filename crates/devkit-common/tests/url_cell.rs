use devkit_common::ui;

/// `DEVKIT_HYPERLINKS` and `COLUMNS` are both process environment, racy under
/// a parallel test runner, so every scenario lives in one sequential test —
/// the same isolation `devkit_hyperlinks_env_overrides_detection` uses.
#[test]
fn url_cell_and_pin_url_column() {
    let url = "https://app.example.com/dashboard/some/really/long/path/segment/x";
    assert!(url.chars().count() > ui::URL_LABEL_MAX);

    let headers = ["ROLE", "APP", "PORT", "URL", "PID", "READY", "LOG"];
    let row = |url_cell: String| {
        vec![
            "issue".to_string(),
            "frontend-dashboard".to_string(),
            "39240".to_string(),
            url_cell,
            "-".to_string(),
            "no".to_string(),
            "0s".to_string(),
        ]
    };
    let introducer_lines =
        |rendered: &str| rendered.lines().filter(|l| l.contains("\x1b]8;;")).count();

    unsafe { std::env::set_var("DEVKIT_HYPERLINKS", "always") };

    for width in [200, 120, 100, 80, 60, 50, 45, 40, 30, 20] {
        unsafe { std::env::set_var("COLUMNS", width.to_string()) };
        let mut t = ui::table(&headers);
        t.add_row(row(ui::url_cell(url)));
        ui::pin_url_column(&mut t, 3, ui::url_column_budget([url]));
        let rendered = t.to_string();
        assert_eq!(
            introducer_lines(&rendered),
            1,
            "OSC 8 escape split across lines at width {width}: {rendered:?}"
        );
    }

    // Regression guard: the same over-budget URL, linked without truncation
    // or a pinned column, still splits at 60 the way it did before the fix —
    // this fails if `url_cell`/`pin_url_column` stop truncating or pinning.
    unsafe { std::env::set_var("COLUMNS", "60") };
    let mut naive = ui::table(&headers);
    naive.add_row(row(ui::link(url, url)));
    let rendered = naive.to_string();
    assert!(
        introducer_lines(&rendered) > 1,
        "expected the unfixed cell to split at COLUMNS=60: {rendered:?}"
    );

    // Hyperlinks off: the cell is the full URL as plain text, and pinning
    // is skipped entirely.
    unsafe { std::env::set_var("DEVKIT_HYPERLINKS", "never") };
    assert_eq!(ui::url_cell(url), url);
    let mut plain = ui::table(&headers);
    plain.add_row(row(ui::url_cell(url)));
    ui::pin_url_column(&mut plain, 3, ui::url_column_budget([url]));
    assert!(
        plain.column_mut(3).unwrap().constraint().is_none(),
        "pin_url_column must no-op without hyperlink support"
    );

    unsafe { std::env::remove_var("DEVKIT_HYPERLINKS") };
    unsafe { std::env::remove_var("COLUMNS") };
}
