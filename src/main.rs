#![cfg_attr(windows, windows_subsystem = "console")]

#[cfg(target_os = "windows")]
#[repr(C)]
#[allow(dead_code)]
struct WinSystemTime {
    w_year: u16,
    w_month: u16,
    w_day_of_week: u16,
    w_day: u16,
    w_hour: u16,
    w_minute: u16,
    w_second: u16,
    w_milliseconds: u16,
}

#[cfg(target_os = "windows")]
extern "C" {
    fn GetDriveTypeW(lpRootPathName: *const u16) -> u32;
    fn GetLocalTime(lpSystemTime: *mut WinSystemTime);
}

#[cfg(target_os = "windows")]
fn is_on_removable_drive(path: &Path) -> bool {
    let path_str = path.to_string_lossy();
    if path_str.len() < 3
        || path_str.as_bytes().get(1) != Some(&b':')
        || path_str.as_bytes().get(2) != Some(&b'\\')
    {
        return false;
    }
    let drive_letter = path_str.chars().next().unwrap();
    let root = format!("{}:\\", drive_letter);
    let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { GetDriveTypeW(wide.as_ptr()) == 2 }
}

#[cfg(target_os = "windows")]
#[repr(C)]
#[allow(dead_code)]
struct MemoryStatusEx {
    dw_length: u32,
    dw_memory_load: u32,
    ull_total_phys: u64,
    ull_avail_phys: u64,
    ull_total_page_file: u64,
    ull_avail_page_file: u64,
    ull_total_virtual: u64,
    ull_avail_virtual: u64,
    ull_avail_extended_virtual: u64,
}

#[cfg(target_os = "windows")]
extern "C" {
    fn GlobalMemoryStatusEx(lp_buffer: *mut MemoryStatusEx) -> i32;
}

#[cfg(target_os = "windows")]
pub(crate) fn usable_ram() -> u64 {
    let mut st: MemoryStatusEx = unsafe { std::mem::zeroed() };
    st.dw_length = std::mem::size_of::<MemoryStatusEx>() as u32;
    unsafe { GlobalMemoryStatusEx(&mut st) };
    st.ull_avail_phys
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn usable_ram() -> u64 {
    16 * 1024 * 1024 * 1024
}

#[cfg(not(target_os = "windows"))]
fn is_on_removable_drive(_path: &Path) -> bool {
    false
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn path_key(p: &Path) -> String {
    p.to_string_lossy().to_lowercase()
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn path_key(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

#[cfg(windows)]
fn fs_path(p: &Path) -> PathBuf {
    let raw = p.as_os_str().to_string_lossy().into_owned();
    if raw.starts_with(r"\\?\") {
        return p.to_path_buf();
    }
    let abs = match std::path::absolute(p) {
        Ok(a) => a,
        Err(_) => return p.to_path_buf(),
    };
    let norm = abs.to_string_lossy().into_owned().replace('/', "\\");
    if norm.len() < 248 {
        return p.to_path_buf();
    }
    let out = if norm.starts_with("\\\\") {
        format!(r"\\?\UNC\{}", &norm[2..])
    } else {
        format!(r"\\?\{}", norm)
    };
    PathBuf::from(out)
}

#[cfg(not(windows))]
fn fs_path(p: &Path) -> PathBuf {
    p.to_path_buf()
}

fn exe_dir() -> PathBuf {
    env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

mod lang;
mod metadata;
mod encode;
mod decode;
mod jpeg_dct;
mod rename;
mod help;
mod pdf;
mod svg_pdf;

use clap::{CommandFactory, FromArgMatches, Parser};
use std::env;
use std::fs;
use std::io::IsTerminal;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::process;
use std::io::Write;
use std::collections::{HashMap, HashSet};
use anyhow::{Context, Result};
use image::imageops::FilterType;
use fast_image_resize::{IntoImageView, ResizeOptions, Resizer};
use rayon::prelude::*;
use unicode_width::UnicodeWidthStr;
use indicatif::{ProgressBar, ProgressStyle};
use encode::{encode_bmp, encode_to_vec, set_avif_threads};
use decode::{
    decode_image, is_heif, is_raw_bytes, looks_like_svg, mem_budget, parse_svg,
    probe_dims, raster_need, JxlPrepared, MemPermit,
};
use rename::try_apply_single;
use help::{boxed, pad_right, print_help_table};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

static LANG: RwLock<lang::Lang> = RwLock::new(lang::Lang::En);

fn set_lang(l: lang::Lang) {
    *LANG.write().unwrap() = l;
}

pub(crate) fn msg() -> &'static lang::Messages {
    lang::messages(*LANG.read().unwrap())
}

fn detect_cli_lang(args: &[String]) -> Option<lang::Lang> {
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--lang" {
            if let Some(v) = args.get(i + 1) {
                return lang::Lang::from_code(v);
            }
        } else if let Some(v) = args[i].strip_prefix("--lang=") {
            return lang::Lang::from_code(v);
        }
        i += 1;
    }
    None
}

fn cli_command() -> clap::Command {
    let m = msg();
    let mut cmd = Cli::command();
    cmd = cmd
        .about(m.about)
        .after_help(m.after_help);
    cmd = cmd
        .mut_arg("size", |a| a.help(m.size_help).help_heading(m.h_resize))
        .mut_arg("quality", |a| a.help(m.quality_help).help_heading(m.h_resize))
        .mut_arg("format", |a| a.help(m.format_help).help_heading(m.h_resize))
        .mut_arg("bw", |a| a.help(m.bw_help).help_heading(m.h_resize))
        .mut_arg("lossless", |a| a.help(m.lossless_help).help_heading(m.h_resize))
        .mut_arg("progressive", |a| a.help(m.progressive_help).help_heading(m.h_resize))
        .mut_arg("sharpen", |a| a.help(m.sharpen_help).help_heading(m.h_resize))
        .mut_arg("no_pause", |a| a.help(m.no_pause_help).help_heading(m.h_misc))
        .mut_arg("output", |a| a.help(m.output_help).help_heading(m.h_misc))
        .mut_arg("shanty", |a| a.help(m.shanty_help).help_heading(m.h_misc))
        .mut_arg("keep_exif", |a| a.help(m.keep_exif_help).help_heading(m.h_misc))
        .mut_arg("lang", |a| a.help_heading(m.h_misc))
        .mut_arg("files", |a| a.help_heading(m.h_input));
    cmd
}

static SHANTY_IDX: AtomicUsize = AtomicUsize::new(0);
fn next_shanty() -> &'static str {
    let i = SHANTY_IDX.fetch_add(1, Ordering::Relaxed);
    let s = msg().shanties;
    &s[i % s.len()]
}

static HAD_ERRORS: AtomicBool = AtomicBool::new(false);

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

fn run_safely<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(p) => Err(anyhow::anyhow!("{}", panic_message(p))),
    }
}

// ── CLI ───────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "SeaMaestroResize",
    version,
    about = "⚓ Maritime image resizer — resize, convert, and optimize images from the command line.",
    after_help = "INPUT: JPEG PNG WebP AVIF JXL ICO TIFF QOI BMP GIF SVG SVGZ TGA PNM PBM PGM PPM PAM DDS HDR EXR FF HEIC/HEIF  RAW(CR2 NEF ARW DNG...)\n\
    OUTPUT: webp jpeg avif jxl png ico tiff qoi bmp gif pdf\n\n  \
    CLI EXAMPLES:\n    \
    seamaestro --size 800 --format webp --quality 80 photo.jpg\n    \
    seamaestro --size 1024x768 --format jpeg --progressive *.jpg\n    \
    seamaestro --size 50pct --format avif photo.heic\n    \
    seamaestro --size 300 --format png --bw --output result.png photo.jpg\n    \
    seamaestro --merge vacation_folder\n    \
    cat photo.jpg | seamaestro --format webp > out.webp\n\n  \
    EXE RENAME EXAMPLES (Windows):\n    \
    SeaMaestroResize_q80_w800_webp.exe      → quality 80, 800px wide, WebP\n    \
    SeaMaestroResize1920jpgq85.exe          → 1920px wide, JPEG, quality 85\n    \
    SeaMaestroResize_w300_h300_png_bw.exe   → 300×300 cover crop, PNG, grayscale"
)]
struct Cli {
    #[arg(long, help_heading = "RESIZE", verbatim_doc_comment)]
    size: Option<String>,
    #[arg(long, default_value_t = 85, help_heading = "RESIZE")]
    quality: u8,
    #[arg(long, value_enum, default_value_t = ImageFormat::Jpeg, help_heading = "RESIZE")]
    format: ImageFormat,
    #[arg(long, help_heading = "RESIZE")]
    bw: bool,
    #[arg(long, help_heading = "RESIZE")]
    lossless: bool,
    #[arg(long, help_heading = "RESIZE")]
    progressive: bool,
    #[arg(long, help = "Sharpen after resize", help_heading = "RESIZE")]
    sharpen: bool,
    #[arg(long, help_heading = "MISC")]
    no_pause: bool,
    #[arg(long, help_heading = "MISC")]
    output: Option<String>,
    #[arg(long, help_heading = "MISC")]
    shanty: bool,
    #[arg(long, help_heading = "MISC")]
    keep_exif: bool,
    #[arg(long, help = "Combine all images into a single PDF (implies --format pdf)", help_heading = "MISC")]
    merge: bool,
    #[arg(long, help = "Language code: en, ru, uk, de, es, fr, el, fil", help_heading = "MISC")]
    lang: Option<String>,
    #[arg(help_heading = "INPUT")]
    files: Vec<String>,
}

// ── Config ────────────────────────────────────────────────────

#[allow(dead_code)]
pub(crate) struct Config {
    pub(crate) lang: lang::Lang,
    target_size: Option<Size>,
    pub(crate) quality: u8,
    pub(crate) format: ImageFormat,
    grayscale: bool,
    pub(crate) lossless: bool,
    pub(crate) progressive: bool,
    sharpen: bool,
    no_pause: bool,
    output: Option<String>,
    shanty: bool,
    keep_exif: bool,
    merge: bool,
}

enum Size {
    Width(u32),
    Height(u32),
    LongEdge(u32),
    Dimensions(u32, u32),
    Percent(f64),
}

#[derive(Debug, Clone, Copy, PartialEq, clap::ValueEnum)]
pub(crate) enum ImageFormat {
    #[clap(name = "webp")] WebP,
    #[clap(name = "jpeg", alias = "jpg")] Jpeg,
    #[clap(name = "avif")] Avif,
    #[clap(name = "png")] Png,
    #[clap(name = "ico")] Ico,
    #[clap(name = "tiff", alias = "tif")] Tiff,
    #[clap(name = "qoi")] Qoi,
    #[clap(name = "bmp")] Bmp,
    #[clap(name = "gif")] Gif,
    #[clap(name = "jxl")] Jxl,
    #[clap(name = "pdf")] Pdf,
}

impl ImageFormat {
    fn extension(&self) -> &str {
        match self {
            ImageFormat::WebP => "webp",
            ImageFormat::Jpeg => "jpg",
            ImageFormat::Avif => "avif",
            ImageFormat::Png => "png",
            ImageFormat::Ico => "ico",
            ImageFormat::Tiff => "tiff",
            ImageFormat::Qoi => "qoi",
            ImageFormat::Bmp => "bmp",
            ImageFormat::Gif => "gif",
            ImageFormat::Jxl => "jxl",
            ImageFormat::Pdf => "pdf",
        }
    }
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "webp" => Some(ImageFormat::WebP),
            "jpeg" | "jpg" => Some(ImageFormat::Jpeg),
            "avif" => Some(ImageFormat::Avif),
            "png" => Some(ImageFormat::Png),
            "ico" => Some(ImageFormat::Ico),
            "tiff" | "tif" => Some(ImageFormat::Tiff),
            "qoi" => Some(ImageFormat::Qoi),
            "bmp" => Some(ImageFormat::Bmp),
            "gif" => Some(ImageFormat::Gif),
            "jxl" => Some(ImageFormat::Jxl),
            "pdf" => Some(ImageFormat::Pdf),
            _ => None,
        }
    }
    fn is_lossless(&self) -> bool {
        matches!(self, ImageFormat::Png | ImageFormat::Ico | ImageFormat::Qoi | ImageFormat::Bmp | ImageFormat::Gif | ImageFormat::Tiff)
    }
}

// ── InputEntry ────────────────────────────────────────────────

struct InputEntry {
    file: PathBuf,
    root: PathBuf,
    direct_file: bool,
}

// ── main / run ────────────────────────────────────────────────

fn main() -> std::process::ExitCode {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(s) = info.payload().downcast_ref::<&str>() {
            if *s == "jpeg_fatal_error" {
                return;
            }
        }
        default_hook(info);
    }));

    jxl_oxide::integration::register_image_decoding_hook();
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let _ = rayon::ThreadPoolBuilder::new().num_threads(cpus).build_global();
    match run() {
        Ok(config) => {
            if !config.no_pause {
                pause();
            }
            if HAD_ERRORS.load(Ordering::Relaxed) {
                std::process::ExitCode::FAILURE
            } else {
                std::process::ExitCode::SUCCESS
            }
        }
        Err(e) => {
            eprintln!("  {} {:#}", msg().mayday_captain, e);
            pause();
            std::process::ExitCode::FAILURE
        }
    }
}

fn pause() {
    eprintln!("\n  {}", msg().press_enter);
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
}

fn run() -> Result<Config> {
    let args: Vec<String> = env::args().collect();

    let detected = detect_cli_lang(&args).unwrap_or_else(|| {
        env::current_exe()
            .ok()
            .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
            .map(|stem| lang::Lang::detect(&stem))
            .unwrap_or(lang::Lang::En)
    });
    set_lang(detected);

    let use_cli = args.iter().any(|a| a.starts_with("--"));

    if use_cli {
        let matches = cli_command().get_matches_from(&args);
        let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
        if let Some(ref l) = cli.lang {
            if let Some(l) = lang::Lang::from_code(l) {
                set_lang(l);
            }
        }
        let mut config = Config {
            lang: detected,
            target_size: None,
            quality: cli.quality,
            format: cli.format,
            grayscale: cli.bw,
            lossless: cli.lossless,
            progressive: cli.progressive,
            sharpen: cli.sharpen,
            no_pause: cli.no_pause,
            output: cli.output.clone(),
            shanty: cli.shanty,
            keep_exif: cli.keep_exif,
            merge: cli.merge,
        };
        if let Some(ref s) = cli.size {
            if s == "42" {
                eprintln!("  {}", msg().the_answer);
                config.target_size = Some(Size::Width(42));
            } else if s == "666" {
                eprintln!("  {}", msg().kraken);
                config.target_size = Some(Size::Width(666));
            } else if s == "0" {
                config.target_size = None;
            } else if let Ok(w) = s.parse::<u32>() {
                config.target_size = Some(Size::LongEdge(w));
            } else if !try_apply_single(&mut config, s) {
                eprintln!("  {}", msg().invalid_size.replacen("{}", s, 1));
                process::exit(1);
            }
        }

        let entries = collect_input_files(&cli.files);
        let mut stdin_buf: Option<Vec<u8>> = None;
        if entries.is_empty() {
            if !std::io::stdin().is_terminal() {
                let mut buf = Vec::new();
                std::io::stdin().read_to_end(&mut buf)?;
                if !buf.is_empty() {
                    stdin_buf = Some(buf);
                }
            }
        }
        if entries.is_empty() && stdin_buf.is_none() {
            eprintln!("  {}", msg().no_input_files);
            eprintln!("  {}\n", msg().drop_hint);
            HAD_ERRORS.store(true, Ordering::Relaxed);
            return Ok(config);
        }
        if config.merge {
            config.format = ImageFormat::Pdf;
        }
        if entries.len() > 1 && config.output.is_some() && !config.merge {
            eprintln!("  {}", msg().output_ignored);
            config.output = None;
        }
        banner(&config);
        if let Some(buf) = stdin_buf {
            if config.output.is_none() {
                run_safely(|| process_and_write_stdout(&buf, &config))?;
            } else {
                let out = PathBuf::from(config.output.as_deref().unwrap());
                if let Some(parent) = out.parent() {
                    if !parent.as_os_str().is_empty() {
                        fs::create_dir_all(&fs_path(parent))
                            .with_context(|| msg().err_mkdir.replacen("{}", &parent.display().to_string(), 1))?;
                    }
                }
                let bytes = run_safely(|| process_bytes(&buf, &config))?;
                atomic_write(&out, &bytes)?;
            }
        } else {
            process_all(entries, &config);
        }
        return Ok(config);
    }

    // ── Exe rename / symlink mode ─────────────────────────────

    let exe_path = env::current_exe()?;
    let stem = exe_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();

    if !stem.contains("SeaMaestroResize") {
        #[cfg(target_os = "windows")] {
            eprintln!("{}", boxed(msg().rename_windows));
        }
        #[cfg(not(target_os = "windows"))] {
            eprintln!("{}", boxed(msg().rename_linux));
        }
        return Ok(Config {
            lang: lang::Lang::En,
            target_size: None, quality: 85, format: ImageFormat::Jpeg,
            grayscale: false, lossless: false, progressive: false,
            sharpen: false, no_pause: false, output: None, shanty: false, keep_exif: false, merge: false,
        });
    }

    let mut config = Config::from_exe_name()?;
    set_lang(config.lang);
    if config.merge {
        config.format = ImageFormat::Pdf;
    }
    banner(&config);

    if args.len() < 2 {
        let exe_name = exe_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        eprintln!("  {}", msg().drop_onto.replacen("{}", &exe_name, 1));
        print_help_table();
        eprintln!();
        return Ok(config);
    }

    let entries = collect_input_files(&args[1..]);
    if entries.is_empty() {
        eprintln!("  {}", msg().no_valid_files);
        HAD_ERRORS.store(true, Ordering::Relaxed);
        return Ok(config);
    }

    process_all(entries, &config);
    Ok(config)
}

// ── collect_input_files ───────────────────────────────────────

fn collect_input_files(paths: &[String]) -> Vec<InputEntry> {
    let exts = [
        "jpg","jpeg","png","webp","avif","jxl","ico","tiff","tif","qoi","bmp","gif",
        "heic","heif","hif",
        "cr2","cr3","crw","nef","nrw",
        "arw","srf","sr2","dng",
        "raf","orf","pef","rw2",
        "mrw","mef","erf","kdc",
        "dcs","dcr","srw","iiq",
        "3fr","mos","x3f","ari",
        "svg","svgz",
        "tga","pnm","pbm","pgm","ppm","pam",
        "dds","hdr","exr","ff",
    ];
    let mut result = Vec::new();
    for raw in paths {
        let path = Path::new(raw);
        if path.is_file() {
            let root = path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
            result.push(InputEntry { file: path.to_path_buf(), root, direct_file: true });
        } else if path.is_dir() {
            let root = path.to_path_buf();
            for entry in jwalk::WalkDir::new(path)
                .skip_hidden(false)
                .into_iter()
                .filter_map(Result::ok)
            {
                if entry.file_type().is_file() {
                    let sub = entry.path();
                    if let Some(ext) = sub.extension().and_then(|e| e.to_str()) {
                        if exts.contains(&ext.to_lowercase().as_str()) {
                            result.push(InputEntry { file: sub, root: root.clone(), direct_file: false });
                        }
                    }
                }
            }
        }
    }
    result
}

// ── group_by_root ─────────────────────────────────────────────

fn group_by_root(entries: &[InputEntry]) -> HashMap<PathBuf, Vec<(PathBuf, PathBuf)>> {
    let mut map: HashMap<PathBuf, Vec<(PathBuf, PathBuf)>> = HashMap::new();
    for entry in entries {
        let rel = entry.file.strip_prefix(&entry.root)
            .unwrap_or_else(|_| Path::new(entry.file.file_name().unwrap_or_default()))
            .to_path_buf();
        map.entry(entry.root.clone()).or_default().push((entry.file.clone(), rel));
    }
    map
}

fn find_common_parent<I, P>(paths: I) -> PathBuf
where I: IntoIterator<Item = P>, P: AsRef<Path>
{
    let mut iter = paths.into_iter();
    let first = match iter.next() {
        Some(p) => p.as_ref().to_path_buf(),
        None => return PathBuf::new(),
    };
    let mut common = first.clone();
    for p in iter {
        common = common
            .components()
            .zip(p.as_ref().components())
            .take_while(|(a, b)| a == b)
            .map(|(a, _)| a)
            .collect();
        if common.as_os_str().is_empty() {
            break;
        }
    }
    if common.as_os_str().is_empty() {
        first
    } else {
        common
    }
}

fn has_word(haystack: &str, needle: &str) -> bool {
    haystack
        .split(|c: char| !c.is_alphabetic())
        .any(|w| w == needle)
}

fn easter_index(fl: &str) -> Option<usize> {
    let idx = if has_word(fl, "titanic") { 0 }
    else if has_word(fl, "treasure") || has_word(fl, "gold") { 1 }
    else if has_word(fl, "kraken") { 2 }
    else if has_word(fl, "parrot") || has_word(fl, "polly") { 3 }
    else if has_word(fl, "davy") || has_word(fl, "jones") { 4 }
    else if has_word(fl, "storm") || has_word(fl, "tempest") { 5 }
    else if has_word(fl, "whale") || has_word(fl, "moby") { 6 }
    else if has_word(fl, "pirate") { 7 }
    else if has_word(fl, "corsair") { 8 }
    else if has_word(fl, "mutiny") { 9 }
    else if has_word(fl, "plank") { 10 }
    else if has_word(fl, "booty") { 11 }
    else if has_word(fl, "loot") { 12 }
    else if has_word(fl, "chest") { 13 }
    else if has_word(fl, "rum") { 14 }
    else if has_word(fl, "grog") { 15 }
    else if has_word(fl, "ship") { 16 }
    else if has_word(fl, "vessel") { 17 }
    else if has_word(fl, "galleon") { 18 }
    else if has_word(fl, "galley") { 19 }
    else if has_word(fl, "fleet") { 20 }
    else if has_word(fl, "sea") { 21 }
    else if has_word(fl, "ocean") { 22 }
    else if has_word(fl, "wave") { 23 }
    else if has_word(fl, "tide") { 24 }
    else if has_word(fl, "reef") { 25 }
    else if has_word(fl, "lagoon") { 26 }
    else if has_word(fl, "atoll") { 27 }
    else if has_word(fl, "island") { 28 }
    else if has_word(fl, "compass") { 29 }
    else if has_word(fl, "map") { 30 }
    else if has_word(fl, "horizon") { 31 }
    else if has_word(fl, "mermaid") { 32 }
    else if has_word(fl, "siren") { 33 }
    else if has_word(fl, "captain") { 34 }
    else if has_word(fl, "crew") { 35 }
    else if has_word(fl, "sailor") { 36 }
    else if has_word(fl, "lookout") { 37 }
    else if has_word(fl, "scurvy") { 38 }
    else if has_word(fl, "bilge") { 39 }
    else if has_word(fl, "barnacle") { 40 }
    else if has_word(fl, "blimey") || has_word(fl, "blime") { 41 }
    else if has_word(fl, "arrr") || has_word(fl, "arr") { 42 }
    else if has_word(fl, "ahoy") { 43 }
    else if has_word(fl, "matey") { 44 }
    else if has_word(fl, "landlubber") { 45 }
    else if has_word(fl, "shiver") { 46 }
    else if has_word(fl, "jolly") || has_word(fl, "roger") { 47 }
    else if has_word(fl, "cutlass") { 48 }
    else if has_word(fl, "cannon") { 49 }
    else if has_word(fl, "anchor") { 50 }
    else if has_word(fl, "harbor") || has_word(fl, "port") { 51 }
    else if has_word(fl, "maroon") { 52 }
    else if has_word(fl, "keel") { 53 }
    else if has_word(fl, "poop") { 54 }
    else if has_word(fl, "arse") || has_word(fl, "ass") { 55 }
    else { return None; };
    Some(idx)
}

fn is_directory_empty(path: &Path) -> bool {
    fs::read_dir(fs_path(path)).map(|mut d| d.next().is_none()).unwrap_or(true)
}

#[cfg(target_os = "windows")]
fn local_now() -> (i64, u32, u32, u32, u32, u32) {
    let mut st: WinSystemTime = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut st) };
    (
        st.w_year as i64,
        st.w_month as u32,
        st.w_day as u32,
        st.w_hour as u32,
        st.w_minute as u32,
        st.w_second as u32,
    )
}

#[cfg(not(target_os = "windows"))]
fn local_now() -> (i64, u32, u32, u32, u32, u32) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as libc::time_t)
        .unwrap_or(0);
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&secs, &mut tm) };
    (
        tm.tm_year as i64 + 1900,
        (tm.tm_mon + 1) as u32,
        tm.tm_mday as u32,
        tm.tm_hour as u32,
        tm.tm_min as u32,
        tm.tm_sec as u32,
    )
}

fn unique_output_dir(base: &Path) -> PathBuf {
    if fs_path(base).exists() && !is_directory_empty(base) {
        let (y, mo, d, h, mi, s) = local_now();
        let stamp = format!("{:04}-{:02}-{:02}_{:02}{:02}{:02}", y, mo, d, h, mi, s);
        let mut candidate = PathBuf::from(format!("{}_{}", base.display(), stamp));
        if fs_path(&candidate).exists() && !is_directory_empty(&candidate) {
            candidate = PathBuf::from(format!(
                "{}_{}_{}",
                base.display(),
                stamp,
                fastrand::u32(100..999)
            ));
        }
        candidate
    } else {
        base.to_path_buf()
    }
}

fn unique_output_dir_reserved(base: &Path, used: &mut HashSet<String>) -> PathBuf {
    let collides = |p: &Path| (fs_path(p).exists() && !is_directory_empty(p)) || used.contains(&path_key(p));
    if collides(base) {
        let (y, mo, d, h, mi, s) = local_now();
        let stamp = format!("{:04}-{:02}-{:02}_{:02}{:02}{:02}", y, mo, d, h, mi, s);
        let mut candidate = PathBuf::from(format!("{}_{}", base.display(), stamp));
        if collides(&candidate) {
            candidate = PathBuf::from(format!(
                "{}_{}_{}",
                base.display(),
                stamp,
                fastrand::u32(100..999)
            ));
        }
        used.insert(path_key(&candidate));
        candidate
    } else {
        used.insert(path_key(base));
        base.to_path_buf()
    }
}

// ── process_files ─────────────────────────────────────────────

struct Pool {
    key: String,
    entries: Vec<InputEntry>,
}

fn pool_key(path: &Path) -> String {
    match path.components().next() {
        Some(std::path::Component::Prefix(p)) => path_key(Path::new(p.as_os_str())),
        _ => String::new(),
    }
}

fn partition_into_pools(entries: Vec<InputEntry>) -> Vec<Pool> {
    let mut pools: Vec<Pool> = Vec::new();
    for entry in entries {
        let key = pool_key(&entry.root);
        if let Some(pool) = pools.iter_mut().find(|p| p.key == key) {
            pool.entries.push(entry);
        } else {
            pools.push(Pool { key, entries: vec![entry] });
        }
    }
    pools
}

fn process_all(entries: Vec<InputEntry>, config: &Config) {
    let pools = partition_into_pools(entries);
    HAD_ERRORS.store(false, Ordering::Relaxed);
    for pool in &pools {
        process_files(&pool.entries, config);
    }
}

fn process_files(entries: &[InputEntry], config: &Config) {
    let total = entries.len();
    if total == 0 { captain_log(0); return; }
    set_avif_threads(total);

    if config.merge && total > 1 {
        process_merge(entries, config);
        return;
    }

    let grouped = group_by_root(entries);
    let single_file_mode = total == 1 && entries[0].direct_file;
    let multi_root = grouped.len() > 1;

    let any_removable = grouped.keys().any(|r| is_on_removable_drive(r));

    let unified_base: Option<PathBuf> = if multi_root && !any_removable {
        let roots: Vec<&PathBuf> = grouped.keys().collect();
        let common = find_common_parent(roots);
        Some(common.join("SeaMaestroResized"))
    } else {
        None
    };

    // 1. Флаг: перетащили несколько файлов россыпью (Сценарий 2)
    let loose_files = !single_file_mode 
        && !any_removable 
        && grouped.len() == 1 
        && entries.iter().all(|e| e.direct_file);

    // 2. Флаг: перетащили одну плоскую папку (Сценарий 3)
    let flat_single_folder = !single_file_mode 
        && !any_removable 
        && !loose_files 
        && grouped.len() == 1 
        && {
            let (_, files) = grouped.iter().next().unwrap();
            !files.iter().any(|(_, rel)| rel.components().count() > 1)
        };

    let mut tasks: Vec<(PathBuf, PathBuf, PathBuf)> = Vec::new();
    let mut used_out_dirs: HashSet<String> = HashSet::new();
    for (root, files) in &grouped {
        let out_dir = if any_removable {
            if single_file_mode {
                unique_output_dir(&exe_dir().join("SeaMaestroResized"))
            } else {
                let is_loose = entries.iter().filter(|e| &e.root == root).all(|e| e.direct_file);
                if is_loose {
                    unique_output_dir(&exe_dir().join("SeaMaestroResized"))
                } else {
                    let root_name = root.file_name()
                        .unwrap_or_else(|| std::ffi::OsStr::new("unknown"))
                        .to_string_lossy();
                    unique_output_dir_reserved(
                        &exe_dir().join("SeaMaestroResized").join(format!("{}_Resized", root_name)),
                        &mut used_out_dirs,
                    )
                }
            }
        } else if single_file_mode {
            root.clone()
        } else if loose_files {
            unique_output_dir(&root.join("SeaMaestroResized"))
        } else if flat_single_folder {
            let parent = root.parent().unwrap_or(root);
            unique_output_dir(&parent.join("SeaMaestroResized"))
        } else if let Some(ref base) = unified_base {
            let root_name = root.file_name()
                .unwrap_or_else(|| std::ffi::OsStr::new("unknown"))
                .to_string_lossy();
            unique_output_dir_reserved(
                &base.join(format!("{}_Resized", root_name)),
                &mut used_out_dirs,
            )
        } else {
            let parent = root.parent().unwrap_or(root);
            let root_name = root.file_name()
                .unwrap_or_else(|| std::ffi::OsStr::new("unknown"))
                .to_string_lossy();
            unique_output_dir(&parent.join("SeaMaestroResized").join(format!("{}_Resized", root_name)))
        };

        for (file, rel) in files {
            tasks.push((file.clone(), out_dir.clone(), rel.clone()));
        }
    }

    let output_base: PathBuf = if let Some(ref out) = config.output {
        Path::new(out).parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
    } else if any_removable || !multi_root {
        tasks.first().map(|(_, d, _)| d.clone()).unwrap_or_default()
    } else {
        unified_base.unwrap_or_default()
    };

    captain_log(total);
    let m = msg();
    let stderr_is_tty = std::io::stderr().is_terminal();
    let pb: Option<ProgressBar> = if stderr_is_tty {
        let pb = ProgressBar::new(total as u64);
        pb.set_style(
            ProgressStyle::with_template(
                "{spinner:.green} [{elapsed}] {msg} {wide_bar:.cyan/blue} {pos}/{len} ({percent}%) {eta}",
            )
            .unwrap()
            .progress_chars("█▓▒░ "),
        );
        pb.enable_steady_tick(std::time::Duration::from_millis(100));
        pb.set_message("resizing images…");
        Some(pb)
    } else {
        None
    };
    let progress = Arc::new(AtomicUsize::new(0));
    let stat_in = Arc::new(AtomicU64::new(0));
    let stat_out = Arc::new(AtomicU64::new(0));

    let mut disk_check_cache: HashMap<PathBuf, bool> = HashMap::new();
    let mut used_paths: HashSet<String> = HashSet::new();
    let mut final_tasks: Vec<(PathBuf, PathBuf)> = Vec::with_capacity(tasks.len());
    for (input, output_dir, rel) in &tasks {
        let check_disk = *disk_check_cache.entry(output_dir.clone()).or_insert_with(|| {
            output_dir.exists() && !is_directory_empty(output_dir)
        });
        let base = compute_output_path(input, config, output_dir, Some(rel));
        let path_collision = used_paths.contains(&path_key(&base)) || (check_disk && base.exists());

        let final_path = if config.output.is_none() && path_collision {
            let stem = input.file_stem().unwrap_or_default().to_string_lossy();
            let ext = config.format.extension();
            let suffix = build_suffix(config);
            let parent = base.parent().unwrap_or(output_dir);
            let mut counter = 1u32;
            loop {
                let candidate = parent.join(format!("{}_{}{}.{}", stem, counter, suffix, ext));
                if !used_paths.contains(&path_key(&candidate)) && (!check_disk || !candidate.exists()) {
                    break candidate;
                }
                counter += 1;
            }
        } else {
            base
        };
        used_paths.insert(path_key(&final_path));
        final_tasks.push((input.clone(), final_path));
    }

    let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let max_workers = cpus;
    let task_idx = AtomicUsize::new(0);
    let (tx, rx) = std::sync::mpsc::channel();
    let task_idx = &task_idx;
    let final_tasks = &final_tasks;
    let progress = &progress;
    let stat_in = &stat_in;
    let stat_out = &stat_out;
    let pb = &pb;

    std::thread::scope(|s| {
        for _ in 0..max_workers {
            let tx = tx.clone();
            s.spawn(move || {
                loop {
                    let idx = task_idx.fetch_add(1, Ordering::Relaxed);
                    if idx >= final_tasks.len() { break; }
                    let (input, final_path) = &final_tasks[idx];

                    let result = run_safely(|| process_image(input, config, final_path));

                    let current = progress.fetch_add(1, Ordering::Relaxed) + 1;
                    if let Some(ref pb) = pb {
                        pb.inc(1);
                    }

                    let (in_meta, out_meta) = match &result {
                        Ok(out_path) => (fs::metadata(input).ok(), fs::metadata(out_path).ok()),
                        Err(_) => (None, None),
                    };
                    let in_size = in_meta.as_ref().map(|m| m.len()).unwrap_or(1);
                    let out_size = out_meta.as_ref().map(|m| m.len()).unwrap_or(1);

                    let formatted = match &result {
                        Ok(out_path) => {
                            let pct = (out_size as f64 / in_size as f64) * 100.0;
                            let remark = if pct < 20.0 { m.remark_great }
                            else if pct < 50.0 { m.remark_good }
                            else if pct < 80.0 { m.remark_ok }
                            else if pct < 100.0 { m.remark_bail }
                            else { m.remark_gain };
                            let change_pct = ((in_size as f64 - out_size as f64) / in_size as f64 * 100.0).abs();
                            let diff_str = if out_size < in_size {
                                m.diff_down
                                    .replacen("{}", &human_size(in_size.saturating_sub(out_size)), 1)
                                    .replacen("{:.0}", &format!("{:.0}", change_pct), 1)
                            } else {
                                m.diff_up
                                    .replacen("{}", &human_size(out_size.saturating_sub(in_size)), 1)
                                    .replacen("{:.0}", &format!("{:.0}", change_pct), 1)
                            };
                            m.file_line
                                .replacen("{}", &current.to_string(), 1)
                                .replacen("{}", &total.to_string(), 1)
                                .replacen("{}", &short_name(&input.file_name().unwrap_or_default().to_string_lossy(), 28), 1)
                                .replacen("{}", &short_name(&out_path.file_name().unwrap_or_default().to_string_lossy(), 28), 1)
                                .replacen("{}", &human_size(in_size), 1)
                                .replacen("{}", &human_size(out_size), 1)
                                .replacen("{}", &diff_str, 1)
                                .replacen("{}", remark, 1)
                        }
                        Err(_) => m.file_error
                            .replacen("{}", &current.to_string(), 1)
                            .replacen("{}", &total.to_string(), 1)
                            .replacen("{}", &short_name(&input.file_name().unwrap_or_default().to_string_lossy(), 28), 1),
                    };

                    match pb {
                        Some(pb) => pb.println(formatted),
                        None => eprintln!("{}", formatted),
                    }

                    if config.shanty {
                        let shanty = format!("  {}", next_shanty());
                        match pb {
                            Some(pb) => pb.println(shanty),
                            None => eprintln!("{}", shanty),
                        }
                    }

                    if let (Some(in_meta), Some(out_meta)) = (&in_meta, &out_meta) {
                        stat_in.fetch_add(in_meta.len(), Ordering::Relaxed);
                        stat_out.fetch_add(out_meta.len(), Ordering::Relaxed);
                    }

                    let out_tuple = (input.clone(), result.map(|o| o.clone()).map_err(|e| format!("{}", e)), current);
                    let _ = tx.send((idx, out_tuple));
                }
            });
        }
        drop(tx);
    });

    let mut results: Vec<_> = rx.into_iter().collect();
    results.sort_by_key(|(i, _)| *i); 

    if let Some(pb) = pb {
        pb.finish_and_clear();
    }

    let in_total = stat_in.load(Ordering::Relaxed);
    let out_total = stat_out.load(Ordering::Relaxed);
    let mut errors = 0usize;

    for (_, (input, result, _)) in &results {
        let fl = input.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
        if let Some(idx) = easter_index(&fl) {
            eprintln!("  {}", m.easter[idx]);
        }
        if let Err(e) = result {
            errors += 1;
            eprintln!("  MAYDAY! {} — {}", input.file_name().unwrap_or_default().to_string_lossy(), e);
        }
    }

    eprintln!("\n  ═══════════════════════════════════════════════");
    eprintln!("  {}", m.voyage_complete.replacen("{}", &total.to_string(), 1));
    if errors > 0 {
        eprintln!("  {}", m.voyage_errors.replacen("{}", &errors.to_string(), 1));
    }

    let (diff_str, pct_total, arrow) = if in_total > out_total {
        let pct = (in_total as f64 - out_total as f64) / in_total as f64 * 100.0;
        (m.cargo_discharged.replacen("{}", &human_size(in_total - out_total), 1), pct, "↑")
    } else if out_total > in_total {
        let pct = (out_total as f64 - in_total as f64) / in_total as f64 * 100.0;
        (m.cargo_took_on.replacen("{}", &human_size(out_total - in_total), 1), pct, "↓")
    } else {
        (String::new(), 0.0, "=")
    };

    if in_total == out_total {
        eprintln!("  {}", m.cargo_line_nochange
            .replacen("{}", &human_size(in_total), 1)
            .replacen("{}", &human_size(out_total), 1));
    } else {
        eprintln!("  {}", m.cargo_line
            .replacen("{}", &human_size(in_total), 1)
            .replacen("{}", &human_size(out_total), 1)
            .replacen("{}", &diff_str, 1)
            .replacen("{}", arrow, 1)
            .replacen("{:.0}", &format!("{:.0}", pct_total), 1));
    }
    eprintln!("  {}", m.output_label.replacen("{}", &output_base.display().to_string(), 1));
    eprintln!("  {}", random_funny_message());
    HAD_ERRORS.store(errors > 0, Ordering::Relaxed);
}

// ── build_suffix ──────────────────────────────────────────────

fn is_lossless_mode(config: &Config) -> bool {
    config.lossless
        && matches!(config.format, ImageFormat::WebP | ImageFormat::Jxl | ImageFormat::Pdf)
}

fn build_suffix(config: &Config) -> String {
    let mut parts = Vec::new();
    if let Some(ref size) = config.target_size {
        match size {
            Size::Width(w) => parts.push(format!("w{}", w)),
            Size::LongEdge(n) => parts.push(format!("l{}", n)),
            Size::Height(h) => parts.push(format!("h{}", h)),
            Size::Dimensions(w, h) => parts.push(format!("{}x{}", w, h)),
            Size::Percent(p) => parts.push(format!("p{:.0}", p * 100.0)),
        }
    }
    if !config.format.is_lossless() && !is_lossless_mode(config) {
        parts.push(format!("q{}", config.quality));
    }
    if config.grayscale { parts.push("bw".to_string()); }
    if parts.is_empty() { String::new() } else { format!("_{}", parts.join("_")) }
}

// ── compute_output_path ───────────────────────────────────────

fn compute_output_path(
    input: &Path,
    config: &Config,
    output_dir: &Path,
    rel_path: Option<&Path>,
) -> PathBuf {
    if let Some(ref out) = config.output {
        return PathBuf::from(out);
    }

    let stem = input.file_stem().unwrap_or_default().to_string_lossy();
    let ext = config.format.extension();
    let suffix = build_suffix(config);
    let out_filename = format!("{}{}.{}", stem, suffix, ext);

    if let Some(rel) = rel_path {
        let rel = rel.to_path_buf();
        let components: Vec<_> = rel.components().collect();
        let mut result_components = Vec::new();
        for (i, comp) in components.iter().enumerate() {
            if i == components.len() - 1 { break; }
            if let std::path::Component::Normal(name) = comp {
                let name_str = name.to_string_lossy();
                if !name_str.is_empty() {
                    result_components.push(format!("{}_Resized", name_str));
                }
            }
        }
        let modified_base: PathBuf = result_components.iter().collect();
        output_dir.join(modified_base).join(out_filename)
    } else {
        output_dir.join(out_filename)
    }
}

// ── process_image ─────────────────────────────────────────────

fn compute_need(raw: &[u8], svg: Option<&crate::decode::ParsedSvg>, config: &Config) -> u64 {
    let is_svg = svg.is_some();
    let (orig_w, orig_h) = if let Some(s) = svg {
        (s.width, s.height)
    } else {
        probe_dims(raw).unwrap_or((32768, 32768))
    };
    compute_need_inner(raw, orig_w, orig_h, is_svg, config)
}

fn compute_need_inner(raw: &[u8], orig_w: u32, orig_h: u32, is_svg: bool, config: &Config) -> u64 {
    let ((tw, th), _) = svg_target_dims((orig_w, orig_h), config.target_size.as_ref());
    let orig_raster = raster_need(orig_w, orig_h);
    let target_raster = raster_need(tw, th);
    let decode_raster = if is_svg { target_raster } else { orig_raster };

    let (in_mult, in_oh): (u64, u64) = if is_svg {
        (1, 16)
    } else if (raw.len() >= 8 && &raw[4..8] == b"JXL ") || raw.starts_with(&[0xFF, 0x0A]) {
        (8, 256)
    } else if is_heif(raw) || (raw.len() >= 12 && (&raw[4..12] == b"ftypavif" || &raw[4..12] == b"ftypavis")) {
        (4, 128)
    } else if is_raw_bytes(raw) {
        (6, 128)
    } else if raw.len() >= 30 && raw.starts_with(b"RIFF") && &raw[8..12] == b"WEBP" {
        (3, 64)
    } else if raw.len() >= 24 && raw.starts_with(b"\x89PNG\r\n\x1a\n") {
        (3, 64)
    } else {
        (1, 16)
    };

    let (out_mult, out_oh): (u64, u64) = match config.format {
        ImageFormat::Avif => (4, 128),
        ImageFormat::Jxl => (16, 512),
        ImageFormat::WebP | ImageFormat::Png | ImageFormat::Pdf => (3, 64),
        _ => (1, 16),
    };

    let decode_need = decode_raster.saturating_mul(in_mult).saturating_add(in_oh * 1024 * 1024);
    let encode_need = target_raster.saturating_mul(out_mult).saturating_add(out_oh * 1024 * 1024);
    decode_need.max(encode_need).saturating_add(raw.len() as u64)
}

fn exif_orientation(raw: &[u8]) -> Option<u32> {
    let reader = exif::Reader::new();
    let exif = reader.read_from_container(&mut std::io::Cursor::new(raw)).ok()?;
    exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)?
        .value
        .get_uint(0)
}

fn try_dct_downscale(raw: &[u8], config: &Config) -> Option<Vec<u8>> {
    if config.format != ImageFormat::Jpeg || config.grayscale || config.sharpen || config.progressive {
        return None;
    }
    if raw.len() < 3 || raw[0] != 0xFF || raw[1] != 0xD8 || raw[2] != 0xFF {
        return None;
    }
    let (ow, oh) = probe_dims(raw)?;
    let is_rotated = matches!(exif_orientation(raw), Some(5..=8));
    let (logical_w, logical_h) = if is_rotated { (oh, ow) } else { (ow, oh) };
    let ((tw, th), exact) = svg_target_dims((logical_w, logical_h), config.target_size.as_ref());
    if !exact {
        return None;
    }
    if tw >= logical_w || th >= logical_h {
        return None;
    }
    let s = (tw as f64 / logical_w as f64).max(th as f64 / logical_h as f64);
    let n = (s * 8.0).ceil() as u32;
    if n == 0 || n >= 8 {
        return None;
    }

    let need = compute_need_inner(raw, ow, oh, false, config);
    let budget = mem_budget();
    budget.acquire(need);
    let _permit = MemPermit { budget, need };

    let (img, icc) = crate::jpeg_dct::decode_scaled_jpeg(raw, n, 8)?;
    let img = auto_orient(img, raw);
    let img = if img.width() == tw && img.height() == th {
        img
    } else {
        resize_dynamic(img, tw, th, ResizeOptions::new())
    };
    let exif = if config.keep_exif { crate::decode::extract_exif(raw) } else { None };
    encode_to_vec(&img, config, icc.as_deref(), exif.as_deref()).ok()
}

fn process_image(input: &Path, config: &Config, final_path: &Path) -> Result<PathBuf> {
    let raw = fs::read(input)
        .with_context(|| msg().err_read.replacen("{}", &input.display().to_string(), 1))?;

    let svg = if looks_like_svg(&raw) {
        Some(parse_svg(&raw, Some(input))?)
    } else { None };
    let svg_render = svg
        .as_ref()
        .map(|s| svg_target_dims((s.width, s.height), config.target_size.as_ref()));

    let out_path = if let Some(ref out) = config.output {
        PathBuf::from(out)
    } else {
        final_path.to_path_buf()
    };

    if path_key(&out_path) == path_key(input) {
        anyhow::bail!("{}", msg().err_overwrite.replacen("{}", &input.display().to_string(), 1));
    }

    if let Some(bytes) = try_dct_downscale(&raw, config) {
        if let Some(parent) = out_path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(&fs_path(parent))
                    .with_context(|| msg().err_mkdir.replacen("{}", &parent.display().to_string(), 1))?;
            }
        }
        atomic_write(&out_path, &bytes)?;
        return Ok(out_path);
    }

    if !config.sharpen && matches!(config.format, ImageFormat::Pdf) {
        if let (Some(s), Some(((tw, th), true))) = (&svg, svg_render) {
            if let Some(vp) = crate::svg_pdf::build_vector_page(&s.tree, tw, th, config.grayscale) {
                if let Some(parent) = out_path.parent() {
                    if !parent.as_os_str().is_empty() {
                        fs::create_dir_all(&fs_path(parent))
                            .with_context(|| msg().err_mkdir.replacen("{}", &parent.display().to_string(), 1))?;
                    }
                }
                let bytes = crate::pdf::page_pdf(crate::pdf::PdfPage::Vector(vp))?;
                atomic_write(&out_path, &bytes)?;
                return Ok(out_path);
            }
        }
    }

    let jxl = JxlPrepared::prepare(&raw);
    let need = if let Some(j) = &jxl {
        compute_need_inner(&raw, j.dims().0, j.dims().1, false, config)
    } else {
        compute_need(&raw, svg.as_ref(), config)
    };
    let budget = mem_budget();
    budget.acquire(need);
    let _permit = MemPermit { budget, need };

    let (mut img, icc, exif) = match jxl {
        Some(j) => j
            .decode()
            .with_context(|| msg().err_decode.replacen("{}", &input.display().to_string(), 1))?,
        None => decode_image(
            &raw,
            Some(input),
            svg_render.map(|((w, h), _)| (w, h)),
            svg.as_ref().map(|s| &s.tree),
        )
        .with_context(|| msg().err_decode.replacen("{}", &input.display().to_string(), 1))?,
    };
    if !looks_like_svg(&raw) && !is_heif(&raw) {
        img = auto_orient(img, &raw);
    }

    if config.grayscale { img = img.grayscale(); }
    if let Some(ref size) = config.target_size {
        let skip_resize = matches!(svg_render, Some((_, true)));
        if !skip_resize {
            img = apply_resize(img, size);
        }
    }
    if config.sharpen {
        img = sharpen_par(img, 3);
    }

    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(&fs_path(parent))
                .with_context(|| msg().err_mkdir.replacen("{}", &parent.display().to_string(), 1))?;
        }
    }

    save_image(&img, &out_path, config, icc.as_deref(), exif.as_deref())?;
    Ok(out_path)
}

// ── apply_resize ──────────────────────────────────────────────

fn apply_resize(img: image::DynamicImage, size: &Size) -> image::DynamicImage {
    match size {
        Size::Width(tw) => {
            let h = ((img.height() as f64 * *tw as f64 / img.width() as f64) as u32).max(1);
            resize_dynamic(img, *tw, h, ResizeOptions::new())
        }
        Size::Height(th) => {
            let w = ((img.width() as f64 * *th as f64 / img.height() as f64) as u32).max(1);
            resize_dynamic(img, w, *th, ResizeOptions::new())
        }
        Size::Dimensions(tw, th) => {
            if img.pixel_type().is_some() {
                let (w0, h0) = (img.width() as f64, img.height() as f64);
                let target_ratio = *tw as f64 / *th as f64;
                let (left, top, cw, ch) = if w0 / h0 > target_ratio {
                    let cw = h0 * target_ratio;
                    ((w0 - cw) / 2.0, 0.0, cw, h0)
                } else {
                    let ch = w0 / target_ratio;
                    (0.0, (h0 - ch) / 2.0, w0, ch)
                };
                let opts = ResizeOptions::new().crop(left, top, cw, ch);
                let mut dst = image::DynamicImage::new(*tw, *th, img.color());
                let mut resizer = Resizer::new();
                if resizer.resize(&img, &mut dst, &opts).is_ok() {
                    return dst;
                }
            }
            let ratio = (*tw as f64 / img.width() as f64).max(*th as f64 / img.height() as f64);
            let iw = (img.width() as f64 * ratio).ceil() as u32;
            let ih = (img.height() as f64 * ratio).ceil() as u32;
            let tmp = img.resize_exact(iw, ih, FilterType::Lanczos3);
            let x = (iw.saturating_sub(*tw)) / 2;
            let y = (ih.saturating_sub(*th)) / 2;
            tmp.crop_imm(x, y, *tw, *th)
        }
        Size::LongEdge(n) => {
            if *n >= img.width().max(img.height()) {
                img
            } else if img.width() >= img.height() {
                let h = ((img.height() as f64 * *n as f64 / img.width() as f64) as u32).max(1);
                resize_dynamic(img, *n, h, ResizeOptions::new())
            } else {
                let w = ((img.width() as f64 * *n as f64 / img.height() as f64) as u32).max(1);
                resize_dynamic(img, w, *n, ResizeOptions::new())
            }
        }
        Size::Percent(p) => {
            let w = ((img.width() as f64 * p) as u32).max(1);
            let h = ((img.height() as f64 * p) as u32).max(1);
            resize_dynamic(img, w, h, ResizeOptions::new())
        }
    }
}

fn svg_target_dims(native: (u32, u32), size: Option<&Size>) -> ((u32, u32), bool) {
    let (nw, nh) = native;
    match size {
        None => ((nw, nh), true),
        Some(Size::Width(tw)) => {
            let h = ((nh as f64 * *tw as f64 / nw as f64) as u32).max(1);
            ((*tw, h), true)
        }
        Some(Size::Height(th)) => {
            let w = ((nw as f64 * *th as f64 / nh as f64) as u32).max(1);
            ((w, *th), true)
        }
        Some(Size::LongEdge(n)) => {
            if *n >= nw.max(nh) {
                ((nw, nh), true)
            } else if nw >= nh {
                let h = ((nh as f64 * *n as f64 / nw as f64) as u32).max(1);
                ((*n, h), true)
            } else {
                let w = ((nw as f64 * *n as f64 / nh as f64) as u32).max(1);
                ((w, *n), true)
            }
        }
        Some(Size::Percent(p)) => {
            let w = ((nw as f64 * *p) as u32).max(1);
            let h = ((nh as f64 * *p) as u32).max(1);
            ((w, h), true)
        }
        Some(Size::Dimensions(tw, th)) => {
            let ratio = (*tw as f64 / nw as f64).max(*th as f64 / nh as f64);
            let iw = (nw as f64 * ratio).ceil() as u32;
            let ih = (nh as f64 * ratio).ceil() as u32;
            ((iw, ih), false)
        }
    }
}

fn resize_dynamic(
    img: image::DynamicImage,
    w: u32,
    h: u32,
    opts: ResizeOptions,
) -> image::DynamicImage {
    if img.pixel_type().is_none() {
        return img.resize_exact(w, h, FilterType::Lanczos3);
    }
    let mut dst = image::DynamicImage::new(w, h, img.color());
    let mut resizer = Resizer::new();
    if resizer.resize(&img, &mut dst, &opts).is_ok() {
        dst
    } else {
        img.resize_exact(w, h, FilterType::Lanczos3)
    }
}

fn sharpen_par(mut img: image::DynamicImage, threshold: i32) -> image::DynamicImage {
    let done = if let Some(g) = img.as_mut_luma8() {
        let (w, h) = (g.width() as usize, g.height() as usize);
        unsharp_plane(g.as_mut(), w, h, 1, threshold);
        true
    } else if let Some(la) = img.as_mut_luma_alpha8() {
        let (w, h) = (la.width() as usize, la.height() as usize);
        unsharp_plane(la.as_mut(), w, h, 2, threshold);
        true
    } else if let Some(rgb) = img.as_mut_rgb8() {
        let (w, h) = (rgb.width() as usize, rgb.height() as usize);
        unsharp_plane(rgb.as_mut(), w, h, 3, threshold);
        true
    } else if let Some(rgba) = img.as_mut_rgba8() {
        let (w, h) = (rgba.width() as usize, rgba.height() as usize);
        unsharp_plane(rgba.as_mut(), w, h, 4, threshold);
        true
    } else {
        false
    };
    if done {
        img
    } else {
        img.unsharpen(1.0, threshold)
    }
}

fn unsharp_plane(buf: &mut [u8], w: usize, h: usize, ch: usize, threshold: i32) {
    let taps = [0.054488f32, 0.244201, 0.402620, 0.244201, 0.054488];
    let r = 2isize;
    let stride = w * ch;
    let mut tmp = vec![0f32; w * h];
    for c in 0..ch {
        let src_plane: &[u8] = buf;
        tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            let src = &src_plane[y * stride..(y + 1) * stride];
            for x in 0..w {
                let mut acc = 0f32;
                for (k, t) in taps.iter().enumerate() {
                    let xx = (x as isize + k as isize - r).clamp(0, (w - 1) as isize) as usize;
                    acc += src[xx * ch + c] as f32 * t;
                }
                row[x] = acc;
            }
        });
        buf.par_chunks_mut(stride).enumerate().for_each(|(y, row)| {
            for x in 0..w {
                let mut acc = 0f32;
                for (k, t) in taps.iter().enumerate() {
                    let yy = (y as isize + k as isize - r).clamp(0, (h - 1) as isize) as usize;
                    acc += tmp[yy * w + x] * t;
                }
                let o = row[x * ch + c] as f32;
                let diff = o - acc;
                let amt = if diff.abs() >= threshold as f32 { diff } else { 0.0 };
                row[x * ch + c] = (o + amt).clamp(0.0, 255.0).round() as u8;
            }
        });
    }
}

// ── save_image ────────────────────────────────────────────────

fn save_image(img: &image::DynamicImage, out: &Path, config: &Config, icc: Option<&[u8]>, exif: Option<&[u8]>) -> Result<()> {
    match config.format {
        ImageFormat::Bmp => atomic_write_with(out, |tmp| encode_bmp(img, tmp)),
        _ => {
            let exif = if config.keep_exif { exif } else { None };
            let bytes = encode_to_vec(img, config, icc, exif)?;
            atomic_write(out, &bytes)
        }
    }
}

fn temp_path(out: &Path) -> PathBuf {
    let name = out.file_name().unwrap_or_default().to_string_lossy();
    out.with_file_name(format!(".{}.{}.tmp", name, fastrand::u32(..)))
}

fn atomic_write(out: &Path, bytes: &[u8]) -> Result<()> {
    let out_buf = fs_path(out);
    let out = out_buf.as_path();
    let tmp = temp_path(out);
    if let Err(e) = fs::write(&tmp, bytes) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    if let Err(e) = fs::rename(&tmp, out) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

fn atomic_write_with<F>(out: &Path, write: F) -> Result<()>
where F: FnOnce(&Path) -> Result<()>
{
    let out_buf = fs_path(out);
    let out = out_buf.as_path();
    let tmp = temp_path(out);
    if let Err(e) = write(&tmp) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = fs::rename(&tmp, out) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

// ── process_and_write_stdout / encode_to_vec ──────────────────

fn process_bytes(raw: &[u8], config: &Config) -> Result<Vec<u8>> {
    let svg = if looks_like_svg(raw) {
        Some(parse_svg(raw, None)?)
    } else { None };
    let svg_render = svg
        .as_ref()
        .map(|s| svg_target_dims((s.width, s.height), config.target_size.as_ref()));

    let jxl = JxlPrepared::prepare(raw);
    let need = if let Some(j) = &jxl {
        compute_need_inner(raw, j.dims().0, j.dims().1, false, config)
    } else {
        compute_need(raw, svg.as_ref(), config)
    };
    let budget = mem_budget();
    budget.acquire(need);
    let _permit = MemPermit { budget, need };

    let (img, icc, exif) = match jxl {
        Some(j) => j
            .decode()
            .with_context(|| msg().err_decode.replacen("{}", "<stdin>", 1))?,
        None => decode_image(
            raw,
            None,
            svg_render.map(|((w, h), _)| (w, h)),
            svg.as_ref().map(|s| &s.tree),
        )
        .with_context(|| msg().err_decode.replacen("{}", "<stdin>", 1))?,
    };
    let img = if !looks_like_svg(raw) && !is_heif(raw) { auto_orient(img, raw) } else { img };
    let img = if config.grayscale { img.grayscale() } else { img };
    let img = if let Some(ref size) = config.target_size {
        let skip_resize = matches!(svg_render, Some((_, true)));
        if skip_resize {
            img
        } else {
            apply_resize(img, size)
        }
    } else { img };
    let img = if config.sharpen { sharpen_par(img, 3) } else { img };
    let exif = if config.keep_exif { exif.as_deref() } else { None };
    encode_to_vec(&img, config, icc.as_deref(), exif)
}

fn process_and_write_stdout(raw: &[u8], config: &Config) -> Result<()> {
    let bytes = process_bytes(raw, config)?;
    std::io::stdout().write_all(&bytes)?;
    Ok(())
}

// ── auto_orient ───────────────────────────────────────────────

fn auto_orient(img: image::DynamicImage, raw_bytes: &[u8]) -> image::DynamicImage {
    let reader = exif::Reader::new();
    let exif = match reader.read_from_container(&mut std::io::Cursor::new(raw_bytes)) {
        Ok(e) => e,
        Err(_) => return img,
    };
    let orientation = match exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY) {
        Some(f) => match f.value.get_uint(0) {
            Some(v) => v,
            None => return img,
        },
        None => return img,
    };
    match orientation {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.fliph().rotate270(),
        6 => img.rotate90(),
        7 => img.fliph().rotate90(),
        8 => img.rotate270(),
        _ => img,
    }
}

// ── banner ────────────────────────────────────────────────────

fn banner(config: &Config) {
    let m = msg();
    let size_str = match &config.target_size {
        Some(Size::Width(w)) => m.size_wide.replacen("{}", &w.to_string(), 1),
        Some(Size::Height(h)) => m.size_tall.replacen("{}", &h.to_string(), 1),
        Some(Size::Dimensions(w, h)) => m.size_crop
            .replacen("{}", &w.to_string(), 1)
            .replacen("{}", &h.to_string(), 1),
        Some(Size::LongEdge(n)) => m.size_long_edge.replacen("{}", &n.to_string(), 1),
        Some(Size::Percent(p)) => m.size_percent.replacen("{:.0}", &format!("{:.0}", p * 100.0), 1),
        None => m.size_original.to_string(),
    };
    let fmt_str = match config.format {
        ImageFormat::WebP if config.lossless => m.fmt_webp_lossless.to_string(),
        ImageFormat::Jpeg if config.progressive => m.fmt_jpeg_progressive.to_string(),
        f => f.extension().to_uppercase(),
    };
    let gray_str = if config.grayscale { m.on } else { m.off };
    let top = "═".repeat(62);
    eprintln!("  ╔{}╗", top);
    eprintln!("  ║  {}{}  ║", pad_right(m.banner_title, 58), "");
    eprintln!("  ║  {}{}  ║", pad_right(m.banner_tagline, 58), "");
    eprintln!("  ║  {}  ║", pad_right(&format!("{} Version: {}", m.banner_by, env!("CARGO_PKG_VERSION")), 58));
    eprintln!("  ║  {}  ║", pad_right("🖂  seamaestro@proton.me", 58));
    eprintln!("  ║  {}  ║", pad_right("⎇  https://github.com/SeaMaestro/SeaMaestroResize", 58));
    eprintln!("  ╠{}╣", "─".repeat(62));
    eprintln!("  ║  {:<13}{}  ║", m.label_size, pad_right(&size_str, 45));
    eprintln!("  ║  {:<13}{}  ║", m.label_format, pad_right(&fmt_str, 45));
    if !config.format.is_lossless() && !is_lossless_mode(config) {
        eprintln!("  ║  {:<13}{}  ║", m.label_quality, pad_right(&format!("{}", config.quality), 45));
    }
    if is_lossless_mode(config) {
        eprintln!("  ║  {:<13}{}  ║", m.label_lossless, pad_right(m.on, 45));
    }
    if config.progressive && config.format == ImageFormat::Jpeg {
        eprintln!("  ║  {:<13}{}  ║", m.label_progressive, pad_right(m.on, 45));
    }
    eprintln!("  ║  {:<13}{}  ║", m.label_grayscale, pad_right(gray_str, 45));
    if config.merge {
        eprintln!("  ║  {:<13}{}  ║", m.label_merge, pad_right(m.on, 45));
    }
    if config.keep_exif
        && matches!(config.format, ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::WebP | ImageFormat::Jxl)
    {
        eprintln!("  ║  {:<13}{}  ║", m.label_exif, pad_right(m.on, 45));
    }
    if config.sharpen {
        eprintln!("  ║  {:<13}{}  ║", m.label_sharpen, pad_right(m.on, 45));
    }
    if config.shanty {
        eprintln!("  ║  {:<13}{}  ║", m.label_shanty, pad_right(m.on, 45));
    }
    eprintln!("  ╚{}╝", top);
    if config.shanty { eprintln!("  {}", next_shanty()); }
    eprintln!();
}

// ── captain_log ────────────────────────────────────────────────

fn captain_log(total: usize) {
    let m = msg();
    let idx = fastrand::usize(..m.weather.len());
    let icon = lang::WEATHER_ICONS[idx];
    let weather = m.weather[idx];
    let body = match total {
        0 => m.log_body[0].to_string(),
        1 => m.log_body[1].to_string(),
        n @ 2..=10 => m.log_body[2].replacen("{}", &n.to_string(), 1),
        n @ 11..=20 => m.log_body[3].replacen("{}", &n.to_string(), 1),
        n @ 21..=30 => m.log_body[4].replacen("{}", &n.to_string(), 1),
        n @ 31..=40 => m.log_body[5].replacen("{}", &n.to_string(), 1),
        n @ 41..=50 => m.log_body[6].replacen("{}", &n.to_string(), 1),
        n @ 51..=100 => m.log_body[7].replacen("{}", &n.to_string(), 1),
        n @ 101..=200 => m.log_body[8].replacen("{}", &n.to_string(), 1),
        n @ 201..=500 => m.log_body[9].replacen("{}", &n.to_string(), 1),
        n => m.log_body[10].replacen("{}", &n.to_string(), 1),
    };
    let line = m.log_header
        .replacen("{}", &body, 1)
        .replacen("{}", icon, 1)
        .replacen("{}", weather, 1);
    eprintln!("  {}", line);
    eprintln!("  ═══════════════════════════════════════════════");
    eprintln!();
}

// ── human_size ─────────────────────────────────────────────────

fn human_size(bytes: u64) -> String {
    if bytes < 1024 { format!("{} B", bytes) }
    else if bytes < 1024*1024 { format!("{:.1} KB", bytes as f64 / 1024.0) }
    else if bytes < 1024*1024*1024 { format!("{:.1} MB", bytes as f64 / (1024.0*1024.0)) }
    else { format!("{:.2} GB", bytes as f64 / (1024.0*1024.0*1024.0)) }
}

// ── short_name ─────────────────────────────────────────────────

fn short_name(name: &str, max: usize) -> String {
    let w = UnicodeWidthStr::width(name);
    if w <= max { return name.to_string(); }

    let (stem, ext) = match name.rfind('.') {
        Some(dot) => {
            let (s, e) = name.split_at(dot);
            (s, e) // e starts with '.'
        }
        None => (name, ""),
    };

    let ext_w = UnicodeWidthStr::width(ext);
    let ellipsis_w = 3;
    let tail_chars = 5;

    // Fallback if max is too small for anything but begin...
    if max <= ellipsis_w + ext_w + 1 {
        let mut result = String::new();
        let mut cur = 0;
        for ch in name.chars() {
            let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
            if cur + cw + ellipsis_w <= max {
                result.push(ch);
                cur += cw;
            } else { break; }
        }
        return format!("{}...", result);
    }

    // Tail: last 'tail_chars' of stem (if stem long enough)
    let tail_str: String = if stem.chars().count() > tail_chars {
        let skip = stem.chars().count() - tail_chars;
        stem.chars().skip(skip).collect()
    } else {
        stem.to_string()
    };
    let tail_w = UnicodeWidthStr::width(tail_str.as_str());
    let available_for_begin = max.saturating_sub(ellipsis_w + tail_w + ext_w);

    let mut begin = String::new();
    let mut cur_w = 0;
    for ch in stem.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
        if cur_w + cw <= available_for_begin {
            begin.push(ch);
            cur_w += cw;
        } else { break; }
    }

    if stem.chars().count() <= tail_chars {
        return format!("{}...{}", begin, ext);
    }

    format!("{}...{}{}", begin, tail_str, ext)
}

// ── random_funny_message ───────────────────────────────────────

fn random_funny_message() -> String {
    let m = msg();
    m.funny[fastrand::usize(..m.funny.len())].to_string()
}

struct MergeGroup<'a> {
    dir: PathBuf,
    files: Vec<&'a InputEntry>,
}

fn group_by_parent<'a>(entries: &'a [InputEntry]) -> Vec<MergeGroup<'a>> {
    let mut map: HashMap<PathBuf, Vec<&'a InputEntry>> = HashMap::new();
    for e in entries {
        let parent = e.file.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
        map.entry(parent).or_default().push(e);
    }
    let mut groups: Vec<MergeGroup<'a>> = map
        .into_iter()
        .map(|(dir, files)| MergeGroup { dir, files })
        .collect();
    groups.sort_by(|a, b| path_key(&a.dir).cmp(&path_key(&b.dir)));
    groups
}

fn merge_chunk_len(total: usize, config: &Config) -> usize {
    let budget = (usable_ram() / 8).clamp(64 * 1024 * 1024, 512 * 1024 * 1024);
    let per_page = if config.lossless {
        64 * 1024 * 1024
    } else {
        8 * 1024 * 1024
    };
    let n = (budget / per_page) as usize;
    n.clamp(1, total)
}

fn truncate_chars(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn cap_name_parts(parts: &[String]) -> String {
    const LIMIT: usize = 120;
    let full = parts.join("_");
    if full.len() <= LIMIT {
        return full;
    }
    let first = truncate_chars(parts.first().map(|s| s.as_str()).unwrap_or(""), 48);
    let last = truncate_chars(parts.last().map(|s| s.as_str()).unwrap_or(""), 48);
    let hash = crc32fast::hash(full.as_bytes());
    format!("{}_..._{}_{:08x}", first, last, hash)
}

fn merge_path_plans(
    groups: &[MergeGroup<'_>],
    common_base: &Path,
    out_dir: &Path,
) -> Vec<(usize, PathBuf, String)> {
    let rels: Vec<(usize, Vec<String>)> = groups
        .iter()
        .enumerate()
        .map(|(idx, g)| {
            let rel = g.dir.strip_prefix(common_base).unwrap_or(&g.dir);
            let mut comps: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .collect();
            if comps.is_empty() {
                comps.push(common_base.file_name().unwrap_or_default().to_string_lossy().to_string());
            }
            (idx, comps)
        })
        .collect();

    let common_len = {
        let mut bound = usize::MAX;
        for (_, comps) in &rels {
            bound = bound.min(comps.len().saturating_sub(1));
        }
        let mut cl = 0usize;
        'outer: while cl < bound {
            let c = &rels[0].1[cl];
            for (_, comps) in &rels[1..] {
                if &comps[cl] != c {
                    break 'outer;
                }
            }
            cl += 1;
        }
        cl
    };

    let mut children: HashMap<Vec<String>, HashSet<String>> = HashMap::new();
    for (_, comps) in &rels {
        let path_len = comps.len().saturating_sub(1);
        let path = &comps[common_len..path_len];
        let leaf = comps.last().cloned().unwrap_or_default();
        for i in 0..path.len() {
            let node: Vec<String> = path[..=i].to_vec();
            let next = if i + 1 < path.len() { path[i + 1].clone() } else { leaf.clone() };
            children.entry(node).or_default().insert(next);
        }
    }

    let mut plans = Vec::with_capacity(rels.len());
    for (idx, comps) in &rels {
        let path_len = comps.len().saturating_sub(1);
        let path = &comps[common_len..path_len];
        let leaf = comps.last().cloned().unwrap_or_default();
        let mut dir_parts: Vec<String> = Vec::new();
        let mut name_parts: Vec<String> = Vec::new();
        for i in 0..path.len() {
            let node: Vec<String> = path[..=i].to_vec();
            let count = children.get(&node).map(|s| s.len()).unwrap_or(0);
            if count > 1 {
                name_parts.push(path[i].clone());
                dir_parts.push(cap_name_parts(&name_parts));
                name_parts.clear();
            } else {
                name_parts.push(path[i].clone());
            }
        }
        let mut full = path.to_vec();
        full.push(leaf.clone());
        let leaf_branches = children.get(&full).map(|s| s.len()).unwrap_or(0) > 1;
        if leaf_branches {
            name_parts.push(leaf.clone());
            dir_parts.push(cap_name_parts(&name_parts));
            name_parts.clear();
            name_parts.push(leaf.clone());
        } else {
            name_parts.push(leaf.clone());
        }
        let name = cap_name_parts(&name_parts);
        let dir = dir_parts.iter().fold(out_dir.to_path_buf(), |acc, d| acc.join(d));
        plans.push((*idx, dir, name));
    }
    plans
}

fn process_merge(entries: &[InputEntry], config: &Config) {
    let total = entries.len();
    if total == 0 { captain_log(0); return; }

    let groups = group_by_parent(entries);
    let (default_dir, common_base) = merge_output_dir(entries);

    let out_dir: PathBuf = if let Some(o) = &config.output {
        let p = PathBuf::from(o);
        if let Err(e) = fs::create_dir_all(&fs_path(&p)) {
            eprintln!("  {}", msg().err_mkdir.replacen("{}", &p.display().to_string(), 1));
            eprintln!("  {:#}", e);
            HAD_ERRORS.store(true, Ordering::Relaxed);
            return;
        }
        p
    } else {
        if let Err(e) = fs::create_dir_all(&fs_path(&default_dir)) {
            eprintln!("  {}", msg().err_mkdir.replacen("{}", &default_dir.display().to_string(), 1));
            eprintln!("  {:#}", e);
            HAD_ERRORS.store(true, Ordering::Relaxed);
            return;
        }
        default_dir
    };

    captain_log(total);

    let stderr_is_tty = std::io::stderr().is_terminal();
    let pb: Option<ProgressBar> = if stderr_is_tty {
        let pb = ProgressBar::new(total as u64);
        pb.set_style(
            ProgressStyle::with_template(
                "{spinner:.green} [{elapsed}] {msg} {wide_bar:.cyan/blue} {pos}/{len} ({percent}%) {eta}",
            )
            .unwrap()
            .progress_chars("█▓▒░ "),
        );
        pb.enable_steady_tick(std::time::Duration::from_millis(100));
        pb.set_message("building PDF…");
        Some(pb)
    } else {
        None
    };

    let progress = AtomicUsize::new(0);
    let stat_in = AtomicU64::new(0);
    let stat_out = AtomicU64::new(0);
    let mut error_list: Vec<(String, String)> = Vec::new();

    let plans = merge_path_plans(&groups, &common_base, &out_dir);

    for (idx, dir, name) in &plans {
        if let Err(e) = fs::create_dir_all(&fs_path(dir)) {
            eprintln!("  {}", msg().err_mkdir.replacen("{}", &dir.display().to_string(), 1));
            eprintln!("  {:#}", e);
            HAD_ERRORS.store(true, Ordering::Relaxed);
            continue;
        }
        if let Some((_path, size)) = merge_group_to_pdf(
            &groups[*idx], dir, name, config, &pb, &progress, &stat_in, total, &mut error_list,
        ) {
            stat_out.fetch_add(size, Ordering::Relaxed);
        }
    }

    if let Some(pb) = pb {
        pb.finish_and_clear();
    }

    for (name, e) in &error_list {
        eprintln!("  MAYDAY! {} — {}", name, e);
    }

    let m = msg();
    let in_total = stat_in.load(Ordering::Relaxed);
    let out_total = stat_out.load(Ordering::Relaxed);

    eprintln!("\n  ═══════════════════════════════════════════════");
    eprintln!("  {}", m.voyage_complete.replacen("{}", &total.to_string(), 1));
    if !error_list.is_empty() {
        eprintln!("  {}", m.voyage_errors.replacen("{}", &error_list.len().to_string(), 1));
    }

    let (diff_str, pct_total, arrow) = if in_total > out_total {
        let pct = (in_total as f64 - out_total as f64) / in_total as f64 * 100.0;
        (m.cargo_discharged.replacen("{}", &human_size(in_total - out_total), 1), pct, "↑")
    } else if out_total > in_total {
        let pct = (out_total as f64 - in_total as f64) / in_total as f64 * 100.0;
        (m.cargo_took_on.replacen("{}", &human_size(out_total - in_total), 1), pct, "↓")
    } else {
        (String::new(), 0.0, "=")
    };

    if in_total == out_total {
        eprintln!("  {}", m.cargo_line_nochange
            .replacen("{}", &human_size(in_total), 1)
            .replacen("{}", &human_size(out_total), 1));
    } else {
        eprintln!("  {}", m.cargo_line
            .replacen("{}", &human_size(in_total), 1)
            .replacen("{}", &human_size(out_total), 1)
            .replacen("{}", &diff_str, 1)
            .replacen("{}", arrow, 1)
            .replacen("{:.0}", &format!("{:.0}", pct_total), 1));
    }
    eprintln!("  {}", m.output_label.replacen("{}", &out_dir.display().to_string(), 1));
    eprintln!("  {}", random_funny_message());
    HAD_ERRORS.store(!error_list.is_empty(), Ordering::Relaxed);
}

fn process_one_to_pdf(entry: &InputEntry, config: &Config) -> Result<crate::pdf::PdfPage> {
    let raw = fs::read(&entry.file)
        .with_context(|| msg().err_read.replacen("{}", &entry.file.display().to_string(), 1))?;

    let svg = if looks_like_svg(&raw) {
        Some(parse_svg(&raw, Some(&entry.file))?)
    } else { None };
    let svg_render = svg
        .as_ref()
        .map(|s| svg_target_dims((s.width, s.height), config.target_size.as_ref()));

    if !config.sharpen {
        if let (Some(s), Some(((tw, th), true))) = (&svg, svg_render) {
            if let Some(vp) = crate::svg_pdf::build_vector_page(&s.tree, tw, th, config.grayscale) {
                return Ok(crate::pdf::PdfPage::Vector(vp));
            }
        }
    }

    let jxl = JxlPrepared::prepare(&raw);
    let need = if let Some(j) = &jxl {
        compute_need_inner(&raw, j.dims().0, j.dims().1, false, config)
    } else {
        compute_need(&raw, svg.as_ref(), config)
    };
    let budget = mem_budget();
    budget.acquire(need);
    let _permit = MemPermit { budget, need };

    let (img, _icc, _exif) = match jxl {
        Some(j) => j
            .decode()
            .with_context(|| msg().err_decode.replacen("{}", &entry.file.display().to_string(), 1))?,
        None => decode_image(
            &raw,
            None,
            svg_render.map(|((w, h), _)| (w, h)),
            svg.as_ref().map(|s| &s.tree),
        )
        .with_context(|| msg().err_decode.replacen("{}", &entry.file.display().to_string(), 1))?,
    };
    let img = if !looks_like_svg(&raw) && !is_heif(&raw) { auto_orient(img, &raw) } else { img };
    let img = if config.grayscale { img.grayscale() } else { img };
    let img = if let Some(ref size) = config.target_size {
        let skip_resize = matches!(svg_render, Some((_, true)));
        if skip_resize {
            img
        } else {
            apply_resize(img, size)
        }
    } else { img };
    let img = if config.sharpen { sharpen_par(img, 3) } else { img };
    crate::pdf::make_page(&img, config)
}

fn merge_group_to_pdf(
    group: &MergeGroup,
    out_dir: &Path,
    rel_name: &str,
    config: &Config,
    pb: &Option<ProgressBar>,
    progress: &AtomicUsize,
    stat_in: &AtomicU64,
    total: usize,
    error_list: &mut Vec<(String, String)>,
) -> Option<(PathBuf, u64)> {
    let m = msg();

    let out_file = unique_merge_pdf(out_dir, rel_name, config);
    let out_name = out_file.file_name().unwrap_or_default().to_string_lossy().to_string();
    let out_file = fs_path(&out_file);

    let tmp = temp_path(&out_file);
    let mut sink = match crate::pdf::PdfSink::create(&tmp) {
        Ok(s) => s,
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            eprintln!("  {:#}", e);
            HAD_ERRORS.store(true, Ordering::Relaxed);
            return None;
        }
    };
    if let Err(e) = sink.write_catalog() {
        drop(sink);
        let _ = fs::remove_file(&tmp);
        eprintln!("  {:#}", e);
        HAD_ERRORS.store(true, Ordering::Relaxed);
        return None;
    }

    let mut ordered: Vec<&InputEntry> = group.files.iter().copied().collect();
    ordered.sort_by(|a, b| path_key(&a.file).cmp(&path_key(&b.file)));

    let group_total = ordered.len();
    let chunk = merge_chunk_len(group_total, config);
    let mut written = 0usize;

    let mut offset = 0usize;
    while offset < group_total {
        let end = (offset + chunk).min(group_total);
        let slice = &ordered[offset..end];
        let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        let chunk_workers = cpus.min(slice.len());
        let task_idx = AtomicUsize::new(0);
        let (tx, rx) = std::sync::mpsc::channel();
        let task_idx = &task_idx;
        let progress = progress;
        let stat_in = stat_in;
        let pb = pb;
        let out_name = out_name.as_str();

        std::thread::scope(|s| {
            for _ in 0..chunk_workers {
                let tx = tx.clone();
                s.spawn(move || {
                    loop {
                        let local = task_idx.fetch_add(1, Ordering::Relaxed);
                        if local >= slice.len() { break; }
                        let entry = slice[local];
                        let idx = offset + local;
                        let name = entry.file.file_name().unwrap_or_default().to_string_lossy();
                        let in_size = fs::metadata(&entry.file).map(|mt| mt.len()).unwrap_or(1);
                        let res = run_safely(|| process_one_to_pdf(entry, config)).map_err(|e| format!("{:#}", e));

                        let current = progress.fetch_add(1, Ordering::Relaxed) + 1;
                        if let Some(ref pb) = pb {
                            pb.inc(1);
                        }

                        match &res {
                            Ok(page) => {
                                let out_size = page.size_hint() as u64;
                                let pct = (out_size as f64 / in_size as f64) * 100.0;
                                let remark = if pct < 20.0 { m.remark_great }
                                    else if pct < 50.0 { m.remark_good }
                                    else if pct < 80.0 { m.remark_ok }
                                    else if pct < 100.0 { m.remark_bail }
                                    else { m.remark_gain };
                                let change_pct = ((in_size as f64 - out_size as f64) / in_size as f64 * 100.0).abs();
                                let diff_str = if out_size < in_size {
                                    m.diff_down
                                        .replacen("{}", &human_size(in_size.saturating_sub(out_size)), 1)
                                        .replacen("{:.0}", &format!("{:.0}", change_pct), 1)
                                } else {
                                    m.diff_up
                                        .replacen("{}", &human_size(out_size.saturating_sub(in_size)), 1)
                                        .replacen("{:.0}", &format!("{:.0}", change_pct), 1)
                                };
                                let line = m.file_line
                                    .replacen("{}", &current.to_string(), 1)
                                    .replacen("{}", &total.to_string(), 1)
                                    .replacen("{}", &short_name(&name, 28), 1)
                                    .replacen("{}", &short_name(out_name, 28), 1)
                                    .replacen("{}", &human_size(in_size), 1)
                                    .replacen("{}", &human_size(out_size), 1)
                                    .replacen("{}", &diff_str, 1)
                                    .replacen("{}", remark, 1);
                                match pb {
                                    Some(pb) => pb.println(line),
                                    None => eprintln!("{}", line),
                                }
                                stat_in.fetch_add(in_size, Ordering::Relaxed);
                            }
                            Err(_) => {
                                let line = m.file_error
                                    .replacen("{}", &current.to_string(), 1)
                                    .replacen("{}", &total.to_string(), 1)
                                    .replacen("{}", &short_name(&name, 28), 1);
                                match pb {
                                    Some(pb) => pb.println(line),
                                    None => eprintln!("{}", line),
                                }
                            }
                        }

                        let _ = tx.send((idx, res));
                    }
                });
            }
            drop(tx);
        });

        let mut results: Vec<(usize, Result<crate::pdf::PdfPage, String>)> = rx.into_iter().collect();
        results.sort_by_key(|(i, _)| *i);

        for (idx, res) in results {
            match res {
                Ok(page) => {
                    if let Err(e) = sink.write_page(&page, written) {
                        drop(sink);
                        let _ = fs::remove_file(&tmp);
                        eprintln!("  {:#}", e);
                        HAD_ERRORS.store(true, Ordering::Relaxed);
                        return None;
                    }
                    written += 1;
                }
                Err(e) => {
                    HAD_ERRORS.store(true, Ordering::Relaxed);
                    let name = ordered[idx].file.file_name().unwrap_or_default().to_string_lossy();
                    error_list.push((name.to_string(), e));
                }
            }
        }
        offset = end;
    }

    let out_total = if written > 0 {
        match sink.finish(written) {
            Ok(size) => size,
            Err(e) => {
                drop(sink);
                let _ = fs::remove_file(&tmp);
                eprintln!("  {:#}", e);
                HAD_ERRORS.store(true, Ordering::Relaxed);
                return None;
            }
        }
    } else {
        0
    };
    drop(sink);

    if written > 0 {
        if let Err(e) = fs::rename(&tmp, &out_file) {
            let _ = fs::remove_file(&tmp);
            eprintln!("  {:#}", e);
            HAD_ERRORS.store(true, Ordering::Relaxed);
            return None;
        }
    } else {
        let _ = fs::remove_file(&tmp);
        return None;
    }

    Some((out_file, out_total))
}

fn merge_output_dir(entries: &[InputEntry]) -> (PathBuf, PathBuf) {
    let mut roots: Vec<&PathBuf> = Vec::new();
    for e in entries {
        if !roots.iter().any(|r| path_key(r) == path_key(&e.root)) {
            roots.push(&e.root);
        }
    }
    let any_removable = roots.iter().any(|r| is_on_removable_drive(*r));
    let common = find_common_parent(roots);
    let base_name = common
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("SeaMaestro"))
        .to_string_lossy()
        .to_string();
    let parent = common.parent().unwrap_or(&common).to_path_buf();

    let out_dir = if any_removable {
        unique_output_dir(&exe_dir().join(format!("{}_Merged", base_name)))
    } else {
        unique_output_dir(&parent.join(format!("{}_Merged", base_name)))
    };
    (out_dir, common)
}

fn unique_merge_pdf(out_dir: &Path, rel: &str, config: &Config) -> PathBuf {
    let suffix = build_suffix(config);
    let base = out_dir.join(format!("{}_Merged{}.pdf", rel, suffix));
    if !fs_path(&base).exists() { return base; }
    let (y, mo, d, h, mi, s) = local_now();
    let stamp = format!("{:04}-{:02}-{:02}_{:02}{:02}{:02}", y, mo, d, h, mi, s);
    let candidate = out_dir.join(format!("{}_Merged{}_{}.pdf", rel, suffix, stamp));
    if fs_path(&candidate).exists() {
        return out_dir.join(format!("{}_Merged{}_{}_{}.pdf", rel, suffix, stamp, fastrand::u32(100..999)));
    }
    candidate
}
