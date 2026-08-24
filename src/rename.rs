use std::env;
use std::sync::OnceLock;

use anyhow::Result;
use regex::Regex;

use crate::{Config, ImageFormat, Size};

pub(crate) fn try_apply_single(config: &mut Config, token: &str) -> bool {
    let t = token.to_lowercase();

    if let Some(cap) = get_q_re().captures(&t) {
        let m = cap.get(0).unwrap();
        if m.len() != t.len() { return false; }
        let q: u8 = cap[1].parse().unwrap_or(0);
        if q > 0 && q <= 100 { config.quality = q; return true; }
        return false;
    }

    if let Some(cap) = get_pct_re().captures(&t) {
        let m = cap.get(0).unwrap();
        if m.len() != t.len() { return false; }
        let p: f64 = cap[1].parse().unwrap_or(0.0);
        if p > 0.0 && p <= 200.0 {
            config.target_size = Some(Size::Percent(p / 100.0));
            return true;
        }
        return false;
    }

    if let Some(cap) = get_dims_re().captures(&t) {
        let m = cap.get(0).unwrap();
        if m.len() != t.len() { return false; }
        let w: u32 = cap[1].parse().unwrap_or(0);
        let h: u32 = cap[2].parse().unwrap_or(0);
        if w > 0 && h > 0 {
            config.target_size = Some(Size::Dimensions(w, h));
            return true;
        }
        return false;
    }

    if let Some(rest) = t.strip_prefix('w') {
        if let Ok(w) = rest.parse::<u32>() {
            if w > 0 {
                config.target_size = Some(Size::Width(w));
                return true;
            }
        }
    }

    if let Some(rest) = t.strip_prefix('h') {
        if let Ok(h) = rest.parse::<u32>() {
            if h > 0 {
                config.target_size = Some(Size::Height(h));
                return true;
            }
        }
    }

    if let Ok(n) = t.parse::<u32>() {
        if n > 0 {
            config.target_size = Some(Size::LongEdge(n));
            return true;
        }
    }

    if let Some(fmt) = ImageFormat::from_str(&t) {
        config.format = fmt;
        return true;
    }

    match t.as_str() {
        "bw" | "gray" | "grey" | "mono" | "grayscale" => {
            config.grayscale = true;
            return true;
        }
        "lossless" => {
            config.lossless = true;
            return true;
        }
        "progressive" | "prog" => {
            config.progressive = true;
            return true;
        }
        "shanty" => {
            config.shanty = true;
            return true;
        }
        "exif" => {
            config.keep_exif = true;
            return true;
        }
        "sharp" => {
            config.sharpen = true;
            return true;
        }
        "merge" => {
            config.merge = true;
            return true;
        }
        _ => {}
    }

    false
}

impl Config {
    pub(crate) fn from_exe_name() -> Result<Self> {
        let exe_path = env::current_exe()?;
        let stem = exe_path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy();

        let mut config = Config {
            target_size: None,
            quality: 85,
            format: ImageFormat::WebP,
            grayscale: false,
            lossless: false,
            progressive: false,
            sharpen: false,
            no_pause: false,
            output: None,
            shanty: false,
            keep_exif: false,
            merge: false,
        };

        let cleaned: String = stem
            .chars()
            .map(|c| if "_-.,; ".contains(c) { ' ' } else { c })
            .collect();

        let tokens: Vec<String> = cleaned
            .split_whitespace()
            .map(|s| s.to_lowercase())
            .filter_map(|s| {
                let s = s
                    .strip_prefix("seamonkeyresize")
                    .or_else(|| s.strip_prefix("seamonkey"))
                    .or_else(|| s.strip_prefix("resize"))
                    .unwrap_or(&s);
                if s.is_empty() { None } else { Some(s.to_string()) }
            })
            .collect();

        for token in &tokens {
            if try_apply_single(&mut config, token) {
                continue;
            }
            if decompose_and_apply(&mut config, token) {
                continue;
            }
        }

        Ok(config)
    }
}

fn decompose_and_apply(config: &mut Config, token: &str) -> bool {
    let mut remaining = token;
    let mut applied = false;

    while !remaining.is_empty() {
        match try_match_at_start(remaining) {
            Some((matched, rest)) => {
                if !try_apply_single(config, matched) {
                    return false;
                }
                remaining = rest;
                applied = true;
            }
            None => return false,
        }
    }

    applied
}

static Q_RE: OnceLock<Regex> = OnceLock::new();
static PCT_RE: OnceLock<Regex> = OnceLock::new();
static DIMS_RE: OnceLock<Regex> = OnceLock::new();
static BARE_RE: OnceLock<Regex> = OnceLock::new();

fn get_q_re() -> &'static Regex {
    Q_RE.get_or_init(|| Regex::new(r"^q(\d{1,3})").unwrap())
}
fn get_pct_re() -> &'static Regex {
    PCT_RE.get_or_init(|| Regex::new(r"^(\d{1,3})(?:p|pct|%)").unwrap())
}
fn get_dims_re() -> &'static Regex {
    DIMS_RE.get_or_init(|| Regex::new(r"^(\d{1,5})x(\d{1,5})").unwrap())
}
fn get_bare_re() -> &'static Regex {
    BARE_RE.get_or_init(|| Regex::new(r"^(\d{2,4})").unwrap())
}

const KEYWORDS_ORDERED: &[&str] = &[
    "jpeg", "jxl", "webp", "avif", "png", "ico", "tiff", "qoi", "bmp", "gif", "jpg", "tif", "pdf", "grayscale", "gray", "grey", "mono", "bw", "lossless", "progressive", "prog", "shanty", "sharp", "exif", "merge",
];

fn try_match_at_start(s: &str) -> Option<(&str, &str)> {
    if s.is_empty() {
        return None;
    }

    if let Some(cap) = get_pct_re().captures(s) {
        let m = cap.get(0).unwrap();
        return Some((&s[..m.end()], &s[m.end()..]));
    }

    if let Some(cap) = get_dims_re().captures(s) {
        let m = cap.get(0).unwrap();
        return Some((&s[..m.end()], &s[m.end()..]));
    }

    if let Some(cap) = get_q_re().captures(s) {
        let m = cap.get(0).unwrap();
        return Some((&s[..m.end()], &s[m.end()..]));
    }

    let lower = s.to_lowercase();

    if let Some(rest) = s.strip_prefix('w') {
        if let Some(cap) = get_bare_re().captures(rest) {
            let m = cap.get(0).unwrap();
            return Some((&s[..1 + m.end()], &s[1 + m.end()..]));
        }
    }

    if let Some(rest) = s.strip_prefix('h') {
        if let Some(cap) = get_bare_re().captures(rest) {
            let m = cap.get(0).unwrap();
            return Some((&s[..1 + m.end()], &s[1 + m.end()..]));
        }
    }

    if let Some(cap) = get_bare_re().captures(s) {
        let m = cap.get(0).unwrap();
        return Some((&s[..m.end()], &s[m.end()..]));
    }

    for kw in KEYWORDS_ORDERED {
        if lower.starts_with(kw) {
            return Some((&s[..kw.len()], &s[kw.len()..]));
        }
    }

    None
}