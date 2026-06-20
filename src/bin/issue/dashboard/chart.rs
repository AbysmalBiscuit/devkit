use chrono::{DateTime, Datelike, Utc};
use textplots::{Chart, ColorPlot, Shape};

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
/// tallest possible column (`max_total`) fills `rows`. Largest-remainder rounding
/// keeps the visible cell total faithful. Returns segment indices bottom→top.
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

/// Terminal width: $COLUMNS, else TIOCGWINSZ, else 100.
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
        let mut ws: libc_winsize =
            libc_winsize { ws_row: 0, ws_col: 0, ws_xpixel: 0, ws_ypixel: 0 };
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
struct libc_winsize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

#[cfg(unix)]
unsafe fn ioctl_winsize(fd: i32, ws: *mut libc_winsize) -> i32 {
    // TIOCGWINSZ is 0x5413 on Linux.
    unsafe extern "C" {
        fn ioctl(fd: i32, request: u64, ...) -> i32;
    }
    unsafe { ioctl(fd, 0x5413, ws) }
}

fn ansi(rgb: (u8, u8, u8), s: &str) -> String {
    format!("\x1b[38;2;{};{};{}m{s}\x1b[0m", rgb.0, rgb.1, rgb.2)
}

/// Render stacked vertical bars. `series[k][b]` = value of status k in bucket b.
pub fn render_stacked_bars(
    title: &str,
    labels: &[String],
    series: &[Vec<u32>],
    names: &[String],
    colors: &[(u8, u8, u8)],
    starts: &[DateTime<Utc>],
    daily_gridlines: bool,
) {
    println!("\n{title}");
    let n = labels.len();
    let max_total: u32 =
        (0..n).map(|b| series.iter().map(|s| s[b]).sum::<u32>()).max().unwrap_or(0);
    // Build each bucket's bottom→top cell stack.
    let columns: Vec<Vec<usize>> = (0..n)
        .map(|b| stack_column(&series.iter().map(|s| s[b]).collect::<Vec<_>>(), max_total, BLOCK_HEIGHT))
        .collect();
    for row in (0..BLOCK_HEIGHT).rev() {
        let mut line = String::new();
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
        println!("{line}");
    }
    // Sparse x labels (~every tenth) and a legend.
    let step = std::cmp::max(1, n / 10);
    let mut axis = String::new();
    for (b, lab) in labels.iter().enumerate() {
        if b % step == 0 {
            axis.push_str(lab);
            axis.push(' ');
        }
    }
    println!("{axis}");
    let legend: Vec<String> =
        names.iter().zip(colors).map(|(nm, c)| ansi(*c, &format!("■ {nm}"))).collect();
    println!("{}", legend.join("  "));
}

/// Render one non-stacked line per series via textplots (braille canvas).
pub fn render_lines(
    title: &str,
    series: &[Vec<u32>],
    names: &[String],
    colors: &[(u8, u8, u8)],
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
        .map(|s| s.iter().enumerate().map(|(i, &v)| (i as f32, v as f32)).collect())
        .collect();
    let mut chart = Chart::new(width * 2, 60, 0.0, (n.saturating_sub(1)) as f32);
    // textplots' builder borrows each Shape for the chart's lifetime.
    let shapes: Vec<Shape> = points.iter().map(|p| Shape::Lines(p)).collect();
    let mut plot = &mut chart;
    for (sh, col) in shapes.iter().zip(colors) {
        plot = plot.linecolorplot(sh, rgb::RGB8::new(col.0, col.1, col.2));
    }
    plot.display();
    let legend: Vec<String> =
        names.iter().zip(colors).map(|(nm, c)| ansi(*c, &format!("─ {nm}"))).collect();
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
        // Two segments split proportionally, indices bottom→top.
        assert_eq!(stack_column(&[2, 2], 4, 4), vec![0, 0, 1, 1]);
    }
    #[test]
    fn stack_column_empty_when_zero() {
        assert!(stack_column(&[0, 0], 4, 4).is_empty());
        assert!(stack_column(&[1], 4, 0).is_empty());
    }
}
