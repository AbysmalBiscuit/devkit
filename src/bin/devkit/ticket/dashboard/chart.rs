use chrono::{DateTime, Datelike, Utc};
use devkit_common::ui::term_width;
use textplots::{
    Chart, ColorPlot, LabelBuilder, LabelFormat, Shape, TickDisplay, TickDisplayBuilder,
};

/// (r,g,b) parsed from a Linear `#rrggbb` hex; falls back to mid-grey.
pub fn hex_rgb(hex: &str) -> (u8, u8, u8) {
    let h = hex.trim_start_matches('#');
    if h.len() >= 6 {
        let p = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(128);
        (p(0), p(2), p(4))
    } else {
        (128, 128, 128)
    }
}

/// Allocate `rows` vertical cells among stacked segment `values`, scaled so the
/// tallest possible column (`max_total`) fills `rows`. Largest-remainder
/// rounding keeps the visible cell total faithful. Returns segment indices
/// bottom->top.
pub fn stack_column(values: &[u32], max_total: u32, rows: usize) -> Vec<usize> {
    let total: u32 = values.iter().sum();
    if total == 0 || max_total == 0 || rows == 0 {
        return Vec::new();
    }
    let scale = rows as f64 / max_total as f64;
    // Ideal (fractional) cell height per segment.
    let ideal: Vec<f64> = values.iter().map(|&v| v as f64 * scale).collect();
    let target: usize = ideal.iter().sum::<f64>().round() as usize;
    let mut floors: Vec<usize> = ideal.iter().map(|x| x.floor() as usize).collect();
    let mut assigned: usize = floors.iter().sum();
    // Distribute the remaining cells to the largest fractional remainders.
    let mut order: Vec<usize> = (0..values.len()).collect();
    order.sort_by(|&a, &b| {
        (ideal[b] - ideal[b].floor())
            .partial_cmp(&(ideal[a] - ideal[a].floor()))
            .unwrap()
    });
    let mut oi = 0;
    while assigned < target && !order.is_empty() {
        floors[order[oi % order.len()]] += 1;
        assigned += 1;
        oi += 1;
    }
    let mut cells = Vec::with_capacity(target);
    for (idx, &h) in floors.iter().enumerate() {
        for _ in 0..h {
            cells.push(idx);
        }
    }
    cells
}

const BLOCK_HEIGHT: usize = 12;
const Y_TICK_EVERY: usize = 4;

/// Y-axis tick labels for the bar rows, top->bottom. Every `Y_TICK_EVERY`th
/// row from the top carries the value its top edge reaches; a tick that would
/// repeat the one above it, or the baseline's 0, stays blank.
fn y_ticks(max_total: u32, unit: &str) -> Vec<Option<String>> {
    let mut last = 0;
    (0..BLOCK_HEIGHT)
        .map(|from_top| {
            let row = BLOCK_HEIGHT - from_top;
            let value = (max_total as f64 * row as f64 / BLOCK_HEIGHT as f64).round() as u32;
            let show = from_top % Y_TICK_EVERY == 0 && value != 0 && value != last;
            show.then(|| {
                last = value;
                format!("{value}{unit}")
            })
        })
        .collect()
}

fn ansi(rgb: (u8, u8, u8), s: &str) -> String {
    format!("\x1b[38;2;{};{};{}m{s}\x1b[0m", rgb.0, rgb.1, rgb.2)
}

/// Render stacked vertical bars. `series[k][b]` = value of status k in bucket
/// b.
#[allow(clippy::too_many_arguments)]
pub fn render_stacked_bars(
    title: &str,
    labels: &[String],
    series: &[Vec<u32>],
    names: &[String],
    colors: &[(u8, u8, u8)],
    starts: &[DateTime<Utc>],
    daily_gridlines: bool,
    unit: &str,
) {
    print!(
        "{}",
        stacked_bars(
            title,
            labels,
            series,
            names,
            colors,
            starts,
            daily_gridlines,
            unit
        )
    );
}

#[allow(clippy::too_many_arguments)]
fn stacked_bars(
    title: &str,
    labels: &[String],
    series: &[Vec<u32>],
    names: &[String],
    colors: &[(u8, u8, u8)],
    starts: &[DateTime<Utc>],
    daily_gridlines: bool,
    unit: &str,
) -> String {
    let mut out = format!("\n{title}\n");
    let n = labels.len();
    let max_total: u32 = (0..n)
        .map(|b| series.iter().map(|s| s[b]).sum::<u32>())
        .max()
        .unwrap_or(0);
    // Build each bucket's bottom->top cell stack.
    let columns: Vec<Vec<usize>> = (0..n)
        .map(|b| {
            stack_column(
                &series.iter().map(|s| s[b]).collect::<Vec<_>>(),
                max_total,
                BLOCK_HEIGHT,
            )
        })
        .collect();
    let ticks = y_ticks(max_total, unit);
    let baseline = format!("0{unit}");
    let gutter = ticks
        .iter()
        .flatten()
        .map(String::len)
        .fold(baseline.len(), usize::max);
    for (row, tick) in (0..BLOCK_HEIGHT).rev().zip(&ticks) {
        let mut line = match tick {
            Some(t) => format!("{t:>gutter$} ┤"),
            None => format!("{:gutter$} │", ""),
        };
        for (b, col) in columns.iter().enumerate() {
            // A faint separator just before each Monday in daily resolution.
            if daily_gridlines && b > 0 && starts[b].weekday() == chrono::Weekday::Mon {
                line.push_str(&ansi((99, 105, 122), "│"));
            } else if b > 0 {
                line.push(' ');
            }
            match col.get(row) {
                Some(&k) => line.push_str(&ansi(colors[k], "█")),
                None => line.push(' '),
            }
        }
        out.push_str(&line);
        out.push('\n');
    }
    let width = (2 * n).saturating_sub(1);
    out.push_str(&format!("{baseline:>gutter$} └{}\n", "─".repeat(width)));
    // Each label centers on its bar, or on the Monday gridline in daily
    // charts, which label only Mondays. A label that would run into the
    // previous one is skipped.
    let mut axis = String::new();
    for (b, lab) in labels.iter().enumerate() {
        let bar = gutter + 2 + 2 * b;
        let monday = starts[b].weekday() == chrono::Weekday::Mon;
        if daily_gridlines && !monday {
            continue;
        }
        let anchor = if daily_gridlines { bar - 1 } else { bar };
        let col = anchor.saturating_sub(lab.len() / 2);
        if axis.is_empty() || col > axis.len() {
            axis.push_str(&" ".repeat(col - axis.len()));
            axis.push_str(lab);
        }
    }
    out.push_str(&axis);
    out.push('\n');
    let legend: Vec<String> = names
        .iter()
        .zip(colors)
        .map(|(nm, c)| ansi(*c, &format!("■ {nm}")))
        .collect();
    out.push_str(&" ".repeat(gutter + 2));
    out.push_str(&legend.join("  "));
    out.push('\n');
    out
}

/// Render one non-stacked line per series via textplots (braille canvas).
pub fn render_lines(
    title: &str,
    series: &[Vec<u32>],
    names: &[String],
    colors: &[(u8, u8, u8)],
    unit: &str,
) {
    println!("\n{title}");
    let n = series.first().map(|s| s.len()).unwrap_or(0);
    if n == 0 {
        println!("  (no data)");
        return;
    }
    let width = (term_width().saturating_sub(12)).clamp(40, 220) as u32;
    let points: Vec<Vec<(f32, f32)>> = series
        .iter()
        .map(|s| {
            s.iter()
                .enumerate()
                .map(|(i, &v)| (i as f32, v as f32))
                .collect()
        })
        .collect();
    let mut chart = Chart::new(width * 2, 60, 0.0, (n.saturating_sub(1)) as f32);
    let unit = unit.to_owned();
    // textplots' builder borrows each Shape for the chart's lifetime.
    let shapes: Vec<Shape> = points.iter().map(|p| Shape::Lines(p)).collect();
    let mut plot = chart
        .y_label_format(LabelFormat::Custom(Box::new(move |v| {
            format!("{}{unit}", v.round())
        })))
        .y_tick_display(TickDisplay::Sparse);
    for (sh, col) in shapes.iter().zip(colors) {
        plot = plot.linecolorplot(sh, rgb::RGB8::new(col.0, col.1, col.2));
    }
    plot.display();
    let legend: Vec<String> = names
        .iter()
        .zip(colors)
        .map(|(nm, c)| ansi(*c, &format!("─ {nm}")))
        .collect();
    println!("{}", legend.join("  "));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hex_rgb_parses() {
        assert_eq!(hex_rgb("#ff8800"), (255, 136, 0));
        assert_eq!(hex_rgb("bad"), (128, 128, 128));
    }
    #[test]
    fn stack_column_scales_to_max() {
        // Tallest column (max_total=4) fills all 4 rows.
        assert_eq!(stack_column(&[4], 4, 4).len(), 4);
        // Half-height column fills ~2 of 4 rows.
        assert_eq!(stack_column(&[2], 4, 4).len(), 2);
        // Two segments split proportionally, indices bottom->top.
        assert_eq!(stack_column(&[2, 2], 4, 4), vec![0, 0, 1, 1]);
    }

    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                chars.by_ref().find(|&c| c == 'm');
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn stacked_bars_label_the_y_axis() {
        let starts: Vec<DateTime<Utc>> = (1..=3)
            .map(|d| format!("2026-09-0{d}T00:00:00Z").parse().unwrap())
            .collect();
        let out = strip_ansi(&stacked_bars(
            "t",
            &["a".into(), "b".into(), "c".into()],
            &[vec![1, 2, 3], vec![0, 1, 2]],
            &["x".into(), "y".into()],
            &[(0, 0, 0), (0, 0, 0)],
            &starts,
            false,
            "",
        ));
        let lines: Vec<&str> = out.lines().skip(2).collect();
        let (plot, rest) = lines.split_at(BLOCK_HEIGHT + 1);
        assert!(plot[0].starts_with("5 ┤"), "{out}");
        assert!(plot[BLOCK_HEIGHT].starts_with("0 └"), "{out}");
        for row in plot {
            let axis = row.chars().nth(2);
            assert!(matches!(axis, Some('┤' | '│' | '└')), "{out}");
        }
        assert!(rest[0].starts_with("   a"), "{out}");
    }

    fn x_axis(labels: &[String], starts: &[DateTime<Utc>], daily_gridlines: bool) -> String {
        let series = vec![vec![1; labels.len()]];
        let out = strip_ansi(&stacked_bars(
            "t",
            labels,
            &series,
            &["x".into()],
            &[(0, 0, 0)],
            starts,
            daily_gridlines,
            "",
        ));
        out.lines().nth(2 + BLOCK_HEIGHT + 1).unwrap().to_string()
    }

    fn days(from: &str, n: i64) -> Vec<DateTime<Utc>> {
        let first: DateTime<Utc> = from.parse().unwrap();
        (0..n).map(|d| first + chrono::Duration::days(d)).collect()
    }

    #[test]
    fn x_labels_center_on_their_bar() {
        let starts = days("2026-09-02T00:00:00Z", 12);
        let labels: Vec<String> = (0..12).map(|b| format!("L{b:02}")).collect();
        // Gutter "1 " plus the axis glyph puts bar b at column 3 + 2b.
        assert_eq!(x_axis(&labels, &starts, false), "  L00 L02 L04 L06 L08 L10");
    }

    #[test]
    fn daily_x_labels_center_on_the_monday_gridlines() {
        let starts = days("2026-09-02T00:00:00Z", 21);
        let labels: Vec<String> = starts
            .iter()
            .map(|s| s.format("%b %d").to_string())
            .collect();
        assert_eq!(
            x_axis(&labels, &starts, true),
            // Gridlines at columns 12, 26 and 40 land on each label's space.
            format!("{}Sep 07        Sep 14        Sep 21", " ".repeat(9))
        );
    }

    #[test]
    fn stack_column_empty_when_zero() {
        assert!(stack_column(&[0, 0], 4, 4).is_empty());
        assert!(stack_column(&[1], 4, 0).is_empty());
    }
}
