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

mod lang;
mod metadata;
mod encode;
mod decode;
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
use std::sync::{Arc, OnceLock};
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
    decode_image, is_heif, looks_like_svg, mem_budget, parse_svg, probe_image,
    raster_need, MemPermit,
};
use rename::try_apply_single;
use help::{boxed, pad_right, print_help_table};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

static LANG: OnceLock<lang::Lang> = OnceLock::new();

fn set_lang(l: lang::Lang) {
    let _ = LANG.set(l);
}

pub(crate) fn msg() -> &'static lang::Messages {
    lang::messages(*LANG.get().unwrap_or(&lang::Lang::En))
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
    name = "SeaMonkeyResize",
    version,
    about = "⚓ Maritime image resizer — resize, convert, and optimize images from the command line.",
    after_help = "INPUT: JPEG PNG WebP AVIF JXL ICO TIFF QOI BMP GIF SVG SVGZ TGA PNM PBM PGM PPM PAM DDS HDR EXR FF HEIC/HEIF  RAW(CR2 NEF ARW DNG...)\n\
    OUTPUT: webp jpeg avif jxl png ico tiff qoi bmp gif pdf\n\n  \
    CLI EXAMPLES:\n    \
    seamonkey --size 800 --format webp --quality 80 photo.jpg\n    \
    seamonkey --size 1024x768 --format jpeg --progressive *.jpg\n    \
    seamonkey --size 50pct --format avif photo.heic\n    \
    seamonkey --size 300 --format png --bw --output result.png photo.jpg\n    \
    seamonkey --merge vacation_folder\n    \
    cat photo.jpg | seamonkey --format webp > out.webp\n\n  \
    EXE RENAME EXAMPLES (Windows):\n    \
    SeaMonkeyResize_q80_w800_webp.exe      → quality 80, 800px wide, WebP\n    \
    SeaMonkeyResize1920jpgq85.exe          → 1920px wide, JPEG, quality 85\n    \
    SeaMonkeyResize_w300_h300_png_bw.exe   → 300×300 cover crop, PNG, grayscale"
)]
struct Cli {
    #[arg(long, help_heading = "RESIZE", verbatim_doc_comment)]
    size: Option<String>,
    #[arg(long, default_value_t = 85, help_heading = "RESIZE")]
    quality: u8,
    #[arg(long, value_enum, default_value_t = ImageFormat::WebP, help_heading = "RESIZE")]
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
                        fs::create_dir_all(parent)
                            .with_context(|| msg().err_mkdir.replacen("{}", &parent.display().to_string(), 1))?;
                    }
                }
                let bytes = run_safely(|| process_bytes(&buf, &config))?;
                atomic_write(&out, &bytes)?;
            }
        } else {
            process_files(&entries, &config);
        }
        return Ok(config);
    }

    // ── Exe rename / symlink mode ─────────────────────────────

    let exe_path = env::current_exe()?;
    let stem = exe_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();

    if !stem.to_lowercase().contains("seamonkeyresize") {
        #[cfg(target_os = "windows")] {
            eprintln!("{}", boxed(msg().rename_windows));
        }
        #[cfg(not(target_os = "windows"))] {
            eprintln!("{}", boxed(msg().rename_linux));
        }
        return Ok(Config {
            target_size: None, quality: 85, format: ImageFormat::WebP,
            grayscale: false, lossless: false, progressive: false,
            sharpen: false, no_pause: false, output: None, shanty: false, keep_exif: false, merge: false,
        });
    }

    let mut config = Config::from_exe_name()?;
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

    process_files(&entries, &config);
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
    fs::read_dir(path).map(|mut d| d.next().is_none()).unwrap_or(true)
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
    if base.exists() && !is_directory_empty(base) {
        let (y, mo, d, h, mi, s) = local_now();
        let stamp = format!("{:04}-{:02}-{:02}_{:02}{:02}{:02}", y, mo, d, h, mi, s);
        let mut candidate = PathBuf::from(format!("{}_{}", base.display(), stamp));
        if candidate.exists() && !is_directory_empty(&candidate) {
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
    let collides = |p: &Path| (p.exists() && !is_directory_empty(p)) || used.contains(&path_key(p));
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

fn process_files(entries: &[InputEntry], config: &Config) {
    HAD_ERRORS.store(false, Ordering::Relaxed);
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

    let exe_dir = env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));

    let any_removable = grouped.keys().any(|r| is_on_removable_drive(r));

    let unified_base: Option<PathBuf> = if multi_root && !any_removable {
        let roots: Vec<&PathBuf> = grouped.keys().collect();
        let common = find_common_parent(roots);
        Some(common.join("SeaMonkeyResized"))
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
                unique_output_dir(&exe_dir.join("SeaMonkeyResized"))
            } else {
                let is_loose = entries.iter().filter(|e| &e.root == root).all(|e| e.direct_file);
                if is_loose {
                    unique_output_dir(&exe_dir.join("SeaMonkeyResized"))
                } else {
                    let root_name = root.file_name()
                        .unwrap_or_else(|| std::ffi::OsStr::new("unknown"))
                        .to_string_lossy();
                    unique_output_dir_reserved(
                        &exe_dir.join("SeaMonkeyResized").join(format!("{}_Resized", root_name)),
                        &mut used_out_dirs,
                    )
                }
            }
        } else if single_file_mode {
            root.clone()
        } else if loose_files {
            unique_output_dir(&root.join("SeaMonkeyResized"))
        } else if flat_single_folder {
            let parent = root.parent().unwrap_or(root);
            unique_output_dir(&parent.join("SeaMonkeyResized"))
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
            unique_output_dir(&parent.join("SeaMonkeyResized").join(format!("{}_Resized", root_name)))
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

    let results: Vec<(PathBuf, Result<PathBuf, String>, usize)> = final_tasks
        .par_iter().enumerate()
        .map(|(_idx, (input, final_path))| {
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

            match &pb {
                Some(pb) => pb.println(formatted),
                None => eprintln!("{}", formatted),
            }

            if config.shanty {
                let shanty = format!("  {}", next_shanty());
                match &pb {
                    Some(pb) => pb.println(shanty),
                    None => eprintln!("{}", shanty),
                }
            }

            if let (Some(in_meta), Some(out_meta)) = (&in_meta, &out_meta) {
                stat_in.fetch_add(in_meta.len(), Ordering::Relaxed);
                stat_out.fetch_add(out_meta.len(), Ordering::Relaxed);
            }

            (input.clone(), result.map(|o| o.clone()).map_err(|e| format!("{}", e)), current)
        }).collect();

    if let Some(pb) = pb {
        pb.finish_and_clear();
    }

    let in_total = stat_in.load(Ordering::Relaxed);
    let out_total = stat_out.load(Ordering::Relaxed);
    let mut errors = 0usize;

    for (input, result, _seq) in &results {
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

    if !config.sharpen && matches!(config.format, ImageFormat::Pdf) {
        if let (Some(s), Some(((tw, th), true))) = (&svg, svg_render) {
            if let Some(vp) = crate::svg_pdf::build_vector_page(&s.tree, tw, th, config.grayscale) {
                if let Some(parent) = out_path.parent() {
                    if !parent.as_os_str().is_empty() {
                        fs::create_dir_all(parent)
                            .with_context(|| msg().err_mkdir.replacen("{}", &parent.display().to_string(), 1))?;
                    }
                }
                let bytes = crate::pdf::page_pdf(crate::pdf::PdfPage::Vector(vp))?;
                atomic_write(&out_path, &bytes)?;
                return Ok(out_path);
            }
        }
    }

    let need = match svg_render {
        Some(((tw, th), _)) => raster_need(tw, th),
        None => probe_image(&raw),
    };
    let budget = mem_budget();
    budget.acquire(need);
    let _permit = MemPermit { budget, need };

    let (mut img, icc, exif) = decode_image(
        &raw,
        Some(input),
        svg_render.map(|((w, h), _)| (w, h)),
        svg.as_ref().map(|s| &s.tree),
    )
    .with_context(|| msg().err_decode.replacen("{}", &input.display().to_string(), 1))?;
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
            fs::create_dir_all(parent)
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

    let (img, icc, exif) = decode_image(
        raw,
        None,
        svg_render.map(|((w, h), _)| (w, h)),
        svg.as_ref().map(|s| &s.tree),
    )
    .with_context(|| msg().err_decode.replacen("{}", "<stdin>", 1))?;
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
    eprintln!("  ║  {}{}  ║", pad_right(m.banner_by, 58), "");
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

fn process_merge(entries: &[InputEntry], config: &Config) {
    HAD_ERRORS.store(false, Ordering::Relaxed);
    let total = entries.len();
    if total == 0 { captain_log(0); return; }

    let mut ordered: Vec<&InputEntry> = entries.iter().collect();
    ordered.sort_by(|a, b| path_key(&a.file).cmp(&path_key(&b.file)));

    let out_file = if let Some(o) = &config.output {
        let p = PathBuf::from(o);
        if let Some(parent) = p.parent() {
            if !parent.as_os_str().is_empty() {
                if let Err(e) = fs::create_dir_all(parent) {
                    eprintln!("  {}", msg().err_mkdir.replacen("{}", &parent.display().to_string(), 1));
                    eprintln!("  {:#}", e);
                    HAD_ERRORS.store(true, Ordering::Relaxed);
                    return;
                }
            }
        }
        p
    } else {
        let out_dir = merge_output_dir(entries);
        if let Err(e) = fs::create_dir_all(&out_dir) {
            eprintln!("  {}", msg().err_mkdir.replacen("{}", &out_dir.display().to_string(), 1));
            eprintln!("  {:#}", e);
            HAD_ERRORS.store(true, Ordering::Relaxed);
            return;
        }
        unique_pdf_file(&out_dir, config)
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
        pb.set_message("building PDF…");
        Some(pb)
    } else {
        None
    };

    let tmp = temp_path(&out_file);
    let mut sink = match crate::pdf::PdfSink::create(&tmp) {
        Ok(s) => s,
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            eprintln!("  {:#}", e);
            HAD_ERRORS.store(true, Ordering::Relaxed);
            return;
        }
    };
    if let Err(e) = sink.write_catalog() {
        drop(sink);
        let _ = fs::remove_file(&tmp);
        eprintln!("  {:#}", e);
        HAD_ERRORS.store(true, Ordering::Relaxed);
        return;
    }

    let progress = Arc::new(AtomicUsize::new(0));
    let stat_in = AtomicU64::new(0);
    let mut error_list: Vec<(String, String)> = Vec::new();
    let out_name = out_file.file_name().unwrap_or_default().to_string_lossy().to_string();

    let chunk = merge_chunk_len(total, config);
    let mut written = 0usize;

    let mut offset = 0usize;
    while offset < total {
        let end = (offset + chunk).min(total);
        let slice = &ordered[offset..end];
        let results: Vec<(usize, Result<crate::pdf::PdfPage, String>)> = slice
            .par_iter()
            .enumerate()
            .map(|(local, entry)| {
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
                            .replacen("{}", &short_name(&out_name, 28), 1)
                            .replacen("{}", &human_size(in_size), 1)
                            .replacen("{}", &human_size(out_size), 1)
                            .replacen("{}", &diff_str, 1)
                            .replacen("{}", remark, 1);
                        match &pb {
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
                        match &pb {
                            Some(pb) => pb.println(line),
                            None => eprintln!("{}", line),
                        }
                    }
                }

                (idx, res)
            })
            .collect();

        for (idx, res) in results {
            match res {
                Ok(page) => {
                    if let Err(e) = sink.write_page(&page, written) {
                        drop(sink);
                        let _ = fs::remove_file(&tmp);
                        eprintln!("  {:#}", e);
                        HAD_ERRORS.store(true, Ordering::Relaxed);
                        return;
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

    if let Some(pb) = pb {
        pb.finish_and_clear();
    }

    for (name, e) in &error_list {
        eprintln!("  MAYDAY! {} — {}", name, e);
    }

    let in_total = stat_in.load(Ordering::Relaxed);
    let out_total = if written > 0 {
        match sink.finish(written) {
            Ok(size) => size,
            Err(e) => {
                drop(sink);
                let _ = fs::remove_file(&tmp);
                eprintln!("  {:#}", e);
                HAD_ERRORS.store(true, Ordering::Relaxed);
                return;
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
            return;
        }
    } else {
        let _ = fs::remove_file(&tmp);
    }

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
    eprintln!("  {}", m.output_label.replacen("{}", &out_file.display().to_string(), 1));
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

    let need = match svg_render {
        Some(((tw, th), _)) => raster_need(tw, th),
        None => probe_image(&raw),
    };
    let budget = mem_budget();
    budget.acquire(need);
    let _permit = MemPermit { budget, need };

    let (img, _icc, _exif) = decode_image(
        &raw,
        None,
        svg_render.map(|((w, h), _)| (w, h)),
        svg.as_ref().map(|s| &s.tree),
    )
    .with_context(|| msg().err_decode.replacen("{}", &entry.file.display().to_string(), 1))?;
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

fn merge_output_dir(entries: &[InputEntry]) -> PathBuf {
    let grouped = group_by_root(entries);
    let any_removable = grouped.keys().any(|r| is_on_removable_drive(r));
    if any_removable {
        return entries[0].root.clone();
    }
    if grouped.len() == 1 {
        let root = grouped.keys().next().unwrap().clone();
        let is_loose = entries.iter().all(|e| e.direct_file);
        if is_loose {
            return unique_output_dir(&root.join("SeaMonkeyResized"));
        }
        let parent = root.parent().unwrap_or(&root).to_path_buf();
        return unique_output_dir(&parent.join("SeaMonkeyResized"));
    }
    let roots: Vec<&PathBuf> = grouped.keys().collect();
    let common = find_common_parent(roots);
    unique_output_dir(&common.join("SeaMonkeyResized"))
}

fn unique_pdf_file(dir: &Path, config: &Config) -> PathBuf {
    let suffix = build_suffix(config);
    let base = dir.join(format!("SeaMonkeyMerged{}.pdf", suffix));
    if !base.exists() { return base; }
    let (y, mo, d, h, mi, s) = local_now();
    let stamp = format!("{:04}-{:02}-{:02}_{:02}{:02}{:02}", y, mo, d, h, mi, s);
    let candidate = dir.join(format!("SeaMonkeyMerged{}_{}.pdf", suffix, stamp));
    if candidate.exists() {
        return dir.join(format!("SeaMonkeyMerged{}_{}_{}.pdf", suffix, stamp, fastrand::u32(100..999)));
    }
    candidate
}
