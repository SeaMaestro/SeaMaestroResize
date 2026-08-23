use unicode_width::UnicodeWidthStr;

use crate::lang::HelpRow;
use crate::msg;

pub(crate) fn pad_right(s: &str, width: usize) -> String {
    let w = UnicodeWidthStr::width(s);
    if w >= width {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(width - w))
    }
}

fn fit_cell(s: &str, width: usize) -> String {
    let w = UnicodeWidthStr::width(s);
    if w <= width {
        return format!("{}{}", s, " ".repeat(width - w));
    }
    let mut out = String::new();
    let mut cur = 0;
    for ch in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if cur + cw <= width {
            out.push(ch);
            cur += cw;
        } else {
            break;
        }
    }
    out
}

pub(crate) fn boxed(lines: &str) -> String {
    let rows: Vec<&str> = lines.split('\n').collect();
    let width = rows
        .iter()
        .map(|l| UnicodeWidthStr::width(*l))
        .max()
        .unwrap_or(0);
    let top = format!("  ╔{}╗", "═".repeat(width + 4));
    let mut out = vec![top];
    for line in rows {
        out.push(format!("  ║  {}  ║", fit_cell(line, width)));
    }
    out.push(format!("  ╚{}╝", "═".repeat(width + 4)));
    out.join("\n")
}

fn two_col(label: &str, value: &str, label_w: usize) -> String {
    let w = UnicodeWidthStr::width(label);
    let pad = label_w.saturating_sub(w);
    format!("{}{}│ {}", label, " ".repeat(pad), value)
}

pub(crate) fn print_help_table() {
    let m = msg();
    let top = "═".repeat(84);
    let mid = "─".repeat(84);
    const W: usize = 80;
    eprintln!("  ╔{}╗", top);
    let mut first = true;
    let mut i = 0usize;
    while i < m.help_table.len() {
        match &m.help_table[i] {
            HelpRow::Title(t) => {
                if !first {
                    eprintln!("  ╠{}╣", mid);
                }
                eprintln!("  ║  {}  ║", fit_cell(t, W));
                if first {
                    eprintln!("  ╠{}╣", mid);
                    first = false;
                }
                i += 1;
            }
            HelpRow::Text(t) => {
                eprintln!("  ║  {}  ║", fit_cell(t, W));
                i += 1;
            }
            HelpRow::Pair(..) => {
                let start = i;
                let mut label_w = 0usize;
                while i < m.help_table.len() {
                    if let HelpRow::Pair(l, _) = &m.help_table[i] {
                        label_w = label_w.max(l.width());
                        i += 1;
                    } else {
                        break;
                    }
                }
                for j in start..i {
                    if let HelpRow::Pair(l, v) = &m.help_table[j] {
                        eprintln!("  ║  {}  ║", fit_cell(&two_col(l, v, label_w), W));
                    }
                }
            }
        }
    }
    eprintln!("  ╚{}╝", top);
}