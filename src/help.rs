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

fn wrap_value(value: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    let mut line_w = 0usize;
    let mut word = String::new();
    let mut word_w = 0usize;
    let mut sep = String::new();
    let mut sep_w = 0usize;

    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == ' ' {
            sep.push(' ');
            sep_w += 1;
            continue;
        }
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        word.push(ch);
        word_w += cw;
        if chars.peek().map_or(true, |c| *c == ' ') {
            if line_w > 0 && line_w + sep_w + word_w > width {
                out.push(std::mem::take(&mut line));
                line_w = 0;
            }
            if line_w > 0 {
                line.push_str(&sep);
                line_w += sep_w;
            }
            line.push_str(&word);
            line_w += word_w;
            word.clear();
            word_w = 0;
            sep.clear();
            sep_w = 0;
        }
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

fn wrap_line(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let lead = text.len() - text.trim_start_matches(' ').len();
    let lead_w = UnicodeWidthStr::width(&text[..lead]);
    let rest = &text[lead..];
    let avail = width.saturating_sub(lead_w);
    let mut chunks = wrap_value(rest, avail);
    if chunks.is_empty() {
        chunks.push(String::new());
    }
    let mut out = Vec::with_capacity(chunks.len());
    for (idx, chunk) in chunks.into_iter().enumerate() {
        if idx == 0 {
            out.push(format!("{}{}", &text[..lead], chunk));
        } else {
            out.push(format!("{}{}", " ".repeat(lead_w), chunk));
        }
    }
    out
}

fn print_pair_wrapped(label: &str, value: &str, label_w: usize, width: usize) {
    let value_w = width.saturating_sub(label_w + 2);
    let chunks = wrap_value(value, value_w);
    if chunks.is_empty() {
        eprintln!("  ║  {}  ║", pad_right(&two_col(label, "", label_w), width));
        return;
    }
    let first_prefix = format!("{}{}│ ", label, " ".repeat(label_w.saturating_sub(label.width())));
    let cont_prefix = format!("{}│ ", " ".repeat(label_w));
    eprintln!("  ║  {}{}  ║", first_prefix, pad_right(&chunks[0], value_w));
    for chunk in chunks.iter().skip(1) {
        eprintln!("  ║  {}{}  ║", cont_prefix, pad_right(chunk, value_w));
    }
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
                for line in wrap_line(t, W) {
                    eprintln!("  ║  {}  ║", pad_right(&line, W));
                }
                if first {
                    eprintln!("  ╠{}╣", mid);
                    first = false;
                }
                i += 1;
            }
            HelpRow::Text(t) => {
                for line in wrap_line(t, W) {
                    eprintln!("  ║  {}  ║", pad_right(&line, W));
                }
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
                        print_pair_wrapped(l, v, label_w, W);
                    }
                }
            }
        }
    }
    eprintln!("  ╚{}╝", top);
}