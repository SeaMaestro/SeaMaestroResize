use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use image::{DynamicImage, RgbaImage};
use ort::ep;
use ort::session::Session;
use ort::value::Tensor;

use crate::Config;

const MODEL: &[u8] = include_bytes!(env!("SEAMAESTRO_CUT_MODEL"));
const MODEL_PATH: &str = env!("SEAMAESTRO_CUT_MODEL");
const INPUT_SIZE: usize = 1024;
const TILE_OVERLAP: u32 = 256;
const DE_FRINGE_BASE_RADIUS: f64 = 90.0;
const DE_FRINGE_BASE_SIDE: f64 = 1024.0;
const DE_FRINGE_MIN_RADIUS: usize = 4;
const ALPHA_LEVELS: Option<(f32, f32)> = Some((0.03, 0.50));
const CUT_MIN_FREE_RAM: u64 = 3 * 1024 * 1024 * 1024;
const DE_FRINGE_BLEND: f32 = 0.75;
const DE_FRINGE_STRIP_H: usize = 512;
const DE_FRINGE_STRIP_MIN_H: usize = 128;
const DE_FRINGE_STRIP_BUDGET: usize = 512 * 1024 * 1024;

#[allow(dead_code)]
pub(crate) const EP_AUTO: u8 = 0;
pub(crate) const EP_CPU: u8 = 1;
pub(crate) const EP_DML: u8 = 2;

static SESSION: Mutex<Option<(Session, bool)>> = Mutex::new(None);
static CPU_REFINE_NOTE_SHOWN: AtomicBool = AtomicBool::new(false);

fn cpu_refine_note_once(is_dml: bool) -> bool {
    if is_dml {
        return false;
    }
    !CPU_REFINE_NOTE_SHOWN.swap(true, Ordering::Relaxed)
}

fn ort_err<R>(err: ort::Error<R>) -> anyhow::Error {
    anyhow::anyhow!("{err}")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DmlFailure {
    Oom,
    Gpu,
}

#[derive(Debug)]
struct DmlFatal {
    kind: DmlFailure,
    detail: String,
}

impl std::fmt::Display for DmlFatal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.detail)
    }
}

impl std::error::Error for DmlFatal {}

fn classify_dml_failure(text: &str) -> Option<DmlFailure> {
    let lower = text.to_ascii_lowercase();
    if lower.contains("8007000e")
        || lower.contains("e_outofmemory")
        || lower.contains("out of memory")
        || lower.contains("not enough memory")
    {
        return Some(DmlFailure::Oom);
    }
    let gpu = lower.contains("887a0020")
        || lower.contains("887a0005")
        || lower.contains("887a0006")
        || lower.contains("887a0001")
        || lower.contains("driver's state is probably suspect")
        || lower.contains("an internal issue prevented the driver")
        || lower.contains("device removed")
        || lower.contains("device hung")
        || lower.contains("dmlexecutionprovider")
        || lower.contains("dmlgraphfusionhelper")
        || lower.contains("directml");
    if gpu {
        Some(DmlFailure::Gpu)
    } else {
        None
    }
}

fn model_name() -> &'static str {
    Path::new(MODEL_PATH)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(MODEL_PATH)
}

fn create_session(use_dml: bool, threads: usize) -> Result<Session> {
    let mut builder = Session::builder().map_err(ort_err)?;
    if threads > 0 {
        builder = builder.with_intra_threads(threads).map_err(ort_err)?;
    }
    if use_dml {
        builder = builder
            .with_execution_providers([ep::DirectML::default().build()])
            .map_err(ort_err)?;
    }
    builder
        .commit_from_memory(MODEL)
        .map_err(ort_err)
        .context(crate::msg().err_session_create)
}

fn build_session(ep_choice: u8, threads: usize) -> Result<(Session, &'static str)> {
    if ep_choice == EP_CPU {
        return Ok((create_session(false, threads)?, "CPU"));
    }
    match create_session(true, threads) {
        Ok(session) => Ok((session, "DirectML")),
        Err(err) if ep_choice == EP_DML => {
            Err(err.context(crate::msg().err_dml_requested))
        }
        Err(err) => {
            eprintln!(
                "{}",
                crate::msg().note_dml_fallback.replacen("{}", &format!("{err:#}"), 1)
            );
            Ok((create_session(false, threads)?, "CPU (DirectML fallback)"))
        }
    }
}

fn preprocess(img: &DynamicImage) -> Result<Tensor<f32>> {
    let small = img
        .resize_exact(INPUT_SIZE as u32, INPUT_SIZE as u32, image::imageops::FilterType::Triangle)
        .to_rgb8();
    let mean = [0.485f32, 0.456, 0.406];
    let std = [0.229f32, 0.224, 0.225];
    let mut data = vec![0f32; 3 * INPUT_SIZE * INPUT_SIZE];
    let plane = INPUT_SIZE * INPUT_SIZE;
    for (x, y, pixel) in small.enumerate_pixels() {
        let index = y as usize * INPUT_SIZE + x as usize;
        for channel in 0..3 {
            let value = pixel.0[channel] as f32 / 255.0;
            data[channel * plane + index] = (value - mean[channel]) / std[channel];
        }
    }
    Tensor::from_array((vec![1usize, 3, INPUT_SIZE, INPUT_SIZE], data))
        .context(crate::msg().err_tensor_build)
}

fn infer(session: &mut Session, tensor: Tensor<f32>) -> Result<Vec<f32>> {
    let input_name = session
        .inputs()
        .first()
        .map(|input| input.name().to_string())
        .unwrap_or_else(|| "input".to_string());
    let outputs = session
        .run(ort::inputs![input_name.as_str() => tensor])
        .map_err(|err| {
            let detail = format!("{err:#}");
            match classify_dml_failure(&detail) {
                Some(kind) => anyhow::Error::new(DmlFatal { kind, detail }),
                None => ort_err(err).context(crate::msg().err_infer_failed),
            }
        })?;
    let output = &outputs[0];
    let data = match output.try_extract_tensor::<f32>() {
        Ok((_, values)) => values.to_vec(),
        Err(_) => {
            let (_, values) = output
                .try_extract_tensor::<half::f16>()
                .map_err(ort_err)
                .context(crate::msg().err_model_output)?;
            values.iter().map(|value| value.to_f32()).collect()
        }
    };
    let expected = INPUT_SIZE * INPUT_SIZE;
    if data.len() != expected {
        bail!(
            "{}",
            crate::msg()
                .err_model_shape
                .replacen("{}", &data.len().to_string(), 1)
                .replacen("{}", &expected.to_string(), 1)
        );
    }
    Ok(data)
}

fn resize_logits(logits: &[f32], width: u32, height: u32) -> Vec<f32> {
    if width == INPUT_SIZE as u32 && height == INPUT_SIZE as u32 {
        return logits.to_vec();
    }
    let source = image::ImageBuffer::<image::Luma<f32>, Vec<f32>>::from_raw(
        INPUT_SIZE as u32,
        INPUT_SIZE as u32,
        logits.to_vec(),
    )
    .expect("logits size mismatch");
    let resized =
        image::imageops::resize(&source, width, height, image::imageops::FilterType::Triangle);
    resized.into_raw()
}

fn post_alpha(values: Vec<f32>) -> Vec<f32> {
    let mut alpha = values;
    let mut low = f32::INFINITY;
    let mut high = f32::NEG_INFINITY;
    for value in alpha.iter() {
        low = low.min(*value);
        high = high.max(*value);
    }
    if std::env::var("SEAMAESTRO_CUT_ALPHA_PROBE").is_ok() {
        eprintln!("  alpha raw range: low={low:.6} high={high:.6}");
    }
    let span = (high - low).max(1e-8);
    for value in alpha.iter_mut() {
        *value = ((*value - low) / span).clamp(0.0, 1.0);
    }
    alpha
}

fn tile_origins(width: u32, height: u32, tile: u32, overlap: u32) -> Vec<(u32, u32)> {
    let step = tile - overlap;
    let last_x = width.saturating_sub(tile);
    let last_y = height.saturating_sub(tile);
    let mut xs = Vec::new();
    let mut x = 0u32;
    while x < last_x {
        xs.push(x);
        x += step;
    }
    xs.push(last_x);
    let mut ys = Vec::new();
    let mut y = 0u32;
    while y < last_y {
        ys.push(y);
        y += step;
    }
    ys.push(last_y);
    let mut origins = Vec::with_capacity(xs.len() * ys.len());
    for y in ys {
        for x in &xs {
            origins.push((*x, y));
        }
    }
    origins
}

fn apply_ramp(slot: &mut f32, ramp: f32) {
    if ramp < *slot {
        *slot = ramp;
    }
}

fn tile_weight(width: usize, height: usize, overlap: usize) -> Vec<f32> {
    let mut rows = vec![1f32; height];
    let mut columns = vec![1f32; width];
    if overlap > 0 && overlap * 2 < width.min(height) {
        for index in 0..overlap {
            let ramp = 0.08 + 0.92 * index as f32 / (overlap - 1).max(1) as f32;
            apply_ramp(&mut rows[index], ramp);
            apply_ramp(&mut rows[height - 1 - index], ramp);
            apply_ramp(&mut columns[index], ramp);
            apply_ramp(&mut columns[width - 1 - index], ramp);
        }
    }
    let mut weights = vec![1f32; width * height];
    for y in 0..height {
        for x in 0..width {
            weights[y * width + x] = rows[y] * columns[x];
        }
    }
    weights
}

const EDGE_REFINE_LOW: f32 = 0.03;
const EDGE_REFINE_HIGH: f32 = 0.97;
const EDGE_REFINE_MAX_TILES: usize = 48;
const EDGE_REFINE_MAX_TILES_CPU: usize = 12;
const EDGE_REFINE_SCALES: [u32; 3] = [1, 2, 4];

fn edge_tiles(
    coarse: &[f32],
    width: u32,
    height: u32,
    tile: u32,
    overlap: u32,
) -> Vec<(u32, u32, usize)> {
    let small = INPUT_SIZE;
    let mut picked: Vec<(u32, u32, usize)> = Vec::new();
    for (x, y) in tile_origins(width, height, tile, overlap) {
        let inner_x = if x == 0 { 0 } else { x + overlap / 2 };
        let inner_y = if y == 0 { 0 } else { y + overlap / 2 };
        let right = (x + tile).min(width);
        let bottom = (y + tile).min(height);
        let inner_right = if right >= width {
            width
        } else {
            right.saturating_sub(overlap / 2)
        };
        let inner_bottom = if bottom >= height {
            height
        } else {
            bottom.saturating_sub(overlap / 2)
        };
        let inner_w = inner_right.saturating_sub(inner_x).max(1);
        let inner_h = inner_bottom.saturating_sub(inner_y).max(1);
        let sx0 = (inner_x as usize * small) / width.max(1) as usize;
        let sx1 = (((inner_x + inner_w) as usize * small) / width.max(1) as usize).clamp(sx0 + 1, small);
        let sy0 = (inner_y as usize * small) / height.max(1) as usize;
        let sy1 = (((inner_y + inner_h) as usize * small) / height.max(1) as usize).clamp(sy0 + 1, small);
        let mut edge = 0usize;
        for row in sy0..sy1 {
            for column in sx0..sx1 {
                let value = coarse[row * small + column];
                if value > EDGE_REFINE_LOW && value < EDGE_REFINE_HIGH {
                    edge += 1;
                }
            }
        }
        if edge > 0 {
            picked.push((x, y, edge));
        }
    }
    picked.sort_by_key(|item| std::cmp::Reverse(item.2));
    picked
}

fn grid_extreme(src: &[f32], size: usize, radius: isize, want_min: bool) -> Vec<f32> {
    let mut out = vec![0f32; src.len()];
    let last = size as isize - 1;
    for y in 0..size {
        for x in 0..size {
            let mut best = if want_min { f32::MAX } else { f32::MIN };
            for dy in -radius..=radius {
                let yy = (y as isize + dy).clamp(0, last) as usize;
                for dx in -radius..=radius {
                    let xx = (x as isize + dx).clamp(0, last) as usize;
                    let value = src[yy * size + xx];
                    best = if want_min { best.min(value) } else { best.max(value) };
                }
            }
            out[y * size + x] = best;
        }
    }
    out
}

enum RefinePlan {
    Empty,
    Scaled(u32, Vec<(u32, u32, usize)>),
    Capped,
}

fn refine_plan(coarse: &[f32], width: u32, height: u32, cap: usize) -> RefinePlan {
    for scale in EDGE_REFINE_SCALES {
        let tile = INPUT_SIZE as u32 * scale;
        let overlap = TILE_OVERLAP * scale;
        let picked = edge_tiles(coarse, width, height, tile, overlap);
        if picked.is_empty() {
            return RefinePlan::Empty;
        }
        if picked.len() <= cap {
            return RefinePlan::Scaled(scale, picked);
        }
    }
    RefinePlan::Capped
}

#[allow(clippy::too_many_arguments)]
fn refine_edge_logits(
    session: &mut Session,
    img: &DynamicImage,
    coarse: &[f32],
    full: &mut [f32],
    width: u32,
    height: u32,
    cap: usize,
    is_dml: bool,
) -> Result<(usize, u32)> {
    if cpu_refine_note_once(is_dml) {
        eprintln!("  {}", crate::msg().note_cpu_refine_slow);
    }
    let (scale, picked) = match refine_plan(coarse, width, height, cap) {
        RefinePlan::Empty => return Ok((0, 1)),
        RefinePlan::Capped => {
            let note = if is_dml {
                crate::msg().note_refine_skip_dml
            } else {
                crate::msg().note_refine_skip_cpu
            };
            eprintln!("  {}", note);
            return Ok((0, 1));
        }
        RefinePlan::Scaled(scale, picked) => (scale, picked),
    };
    if scale > 1 {
        eprintln!(
            "  {}",
            crate::msg()
                .note_refine_scaled
                .replacen("{}", &scale.to_string(), 1)
                .replacen("{}", &picked.len().to_string(), 1)
                .replacen("{}", &cap.to_string(), 1)
        );
    }
    let w = width as usize;
    let h = height as usize;
    let overlap = (TILE_OVERLAP * scale) as usize;
    let guard_grid = coarse.len() == INPUT_SIZE * INPUT_SIZE;
    let (coarse_min, coarse_max) = if guard_grid {
        (
            grid_extreme(coarse, INPUT_SIZE, 2, true),
            grid_extreme(coarse, INPUT_SIZE, 1, false),
        )
    } else {
        (Vec::new(), Vec::new())
    };
    let mut value = vec![0f32; full.len()];
    let mut weight_total = vec![0f32; full.len()];
    let patch_side = INPUT_SIZE as u32 * scale;
    for (x, y, _) in &picked {
        let patch_width = patch_side.min(width - x);
        let patch_height = patch_side.min(height - y);
        let patch = img.crop_imm(*x, *y, patch_width, patch_height);
        let logits = infer(session, preprocess(&patch)?)?;
        let small = resize_logits(&logits, patch_width, patch_height);
        let weights = tile_weight(patch_width as usize, patch_height as usize, overlap);
        let (pw, ph) = (patch_width as usize, patch_height as usize);
        for row in 0..ph {
            let source = row * pw;
            let target = (*y as usize + row) * w + *x as usize;
            for column in 0..pw {
                let weight = weights[source + column];
                value[target + column] += small[source + column] * weight;
                weight_total[target + column] += weight;
            }
        }
    }
    for (index, slot) in full.iter_mut().enumerate() {
        let total = weight_total[index];
        if total <= 1e-6 {
            continue;
        }
        let refined = value[index] / total;
        let blend = total.min(1.0);
        let blended = *slot * (1.0 - blend) + refined * blend;
        if !guard_grid {
            *slot = blended;
            continue;
        }
        let column = index % w;
        let row = index / w;
        let cx = (column * INPUT_SIZE / w).min(INPUT_SIZE - 1);
        let cy = (row * INPUT_SIZE / h).min(INPUT_SIZE - 1);
        let cell = cy * INPUT_SIZE + cx;
        if coarse_min[cell] >= 0.85 {
            *slot = blended.max(*slot);
        } else if coarse_max[cell] <= 0.02 {
            continue;
        } else {
            *slot = blended;
        }
    }
    Ok((picked.len(), scale))
}

fn frame_logits(
    session: &mut Session,
    img: &DynamicImage,
    tiled: bool,
    cap: usize,
    is_dml: bool,
    no_refine: bool,
    profile: bool,
) -> Result<(Vec<f32>, usize, u32, u32)> {
    let width = img.width();
    let height = img.height();
    let tile = INPUT_SIZE as u32;
    if !tiled || (width <= tile && height <= tile) {
        let t_infer = std::time::Instant::now();
        let logits = infer(session, preprocess(img)?)?;
        let mut combined = resize_logits(&logits, width, height);
        let infer_secs = t_infer.elapsed();
        let t_refine = std::time::Instant::now();
        let (refined, scale) = if (width > tile || height > tile) && !no_refine {
            let coarse = post_alpha(logits);
            refine_edge_logits(session, img, &coarse, &mut combined, width, height, cap, is_dml)?
        } else {
            (0, 1)
        };
        if profile {
            eprintln!(
                "  ⏱ cut infer {:.2} s | refine {:.2} s",
                infer_secs.as_secs_f64(),
                t_refine.elapsed().as_secs_f64()
            );
        }
        return Ok((combined, 1 + refined, tile * scale, TILE_OVERLAP * scale));
    }
    let (w, h) = (width as usize, height as usize);
    let overlap = TILE_OVERLAP as usize;
    let t_tile = std::time::Instant::now();
    let mut combined = vec![0f32; w * h];
    let mut total = vec![0f32; w * h];
    let mut count = 0usize;
    for (x, y) in tile_origins(width, height, tile, TILE_OVERLAP) {
        let patch_width = tile.min(width - x);
        let patch_height = tile.min(height - y);
        let patch = img.crop_imm(x, y, patch_width, patch_height);
        let logits = infer(session, preprocess(&patch)?)?;
        let small = resize_logits(&logits, patch_width, patch_height);
        let weights = tile_weight(patch_width as usize, patch_height as usize, overlap);
        let (pw, ph) = (patch_width as usize, patch_height as usize);
        for row in 0..ph {
            let source = row * pw;
            let target = (y as usize + row) * w + x as usize;
            for column in 0..pw {
                let weight = weights[source + column];
                combined[target + column] += small[source + column] * weight;
                total[target + column] += weight;
            }
        }
        count += 1;
    }
    for index in 0..combined.len() {
        combined[index] /= total[index].max(1e-6);
    }
    if profile {
        eprintln!(
            "  ⏱ cut tiled {:.2} s ({} tiles)",
            t_tile.elapsed().as_secs_f64(),
            count
        );
    }
    Ok((combined, count, tile, TILE_OVERLAP))
}

struct BlurScratch {
    sat: Vec<f64>,
}

impl BlurScratch {
    fn new() -> Self {
        Self { sat: Vec::new() }
    }
}

fn box_blur_into(
    src: &[f32],
    width: usize,
    height: usize,
    radius: usize,
    scratch: &mut BlurScratch,
    out: &mut [f32],
) {
    let pixels = width * height;
    if radius <= 1 {
        out[..pixels].copy_from_slice(&src[..pixels]);
        return;
    }
    let r = radius / 2;
    let stride = width + 1;
    let need = stride * (height + 1);
    if scratch.sat.len() < need {
        scratch.sat.resize(need, 0f64);
    }
    let sat = &mut scratch.sat[..need];
    for value in sat.iter_mut().take(stride) {
        *value = 0f64;
    }
    for row in 1..=height {
        sat[row * stride] = 0f64;
    }
    for y in 0..height {
        let mut row_sum = 0f64;
        for x in 0..width {
            row_sum += src[y * width + x] as f64;
            sat[(y + 1) * stride + x + 1] = sat[y * stride + x + 1] + row_sum;
        }
    }
    for y in 0..height {
        let y0 = y.saturating_sub(r);
        let y1 = (y + r + 1).min(height);
        for x in 0..width {
            let x0 = x.saturating_sub(r);
            let x1 = (x + r + 1).min(width);
            let sum = sat[y1 * stride + x1] - sat[y0 * stride + x1] - sat[y1 * stride + x0]
                + sat[y0 * stride + x0];
            let area = ((y1 - y0) * (x1 - x0)) as f64;
            out[y * width + x] = (sum / area) as f32;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn fb_estimate(
    image: &[f32],
    foreground: &[f32],
    background: &[f32],
    alpha: &[f32],
    blurred_alpha: &[f32],
    width: usize,
    height: usize,
    radius: usize,
    scratch: &mut BlurScratch,
) -> (Vec<f32>, Vec<f32>) {
    let pixels = width * height;
    let mut refined = vec![0f32; pixels];
    let mut background_scaled = vec![0f32; pixels];
    for index in 0..pixels {
        let a = alpha[index];
        refined[index] = foreground[index] * a;
        background_scaled[index] = background[index] * (1.0 - a);
    }
    let mut blurred_foreground = vec![0f32; pixels];
    let mut blurred_background = vec![0f32; pixels];
    box_blur_into(&refined, width, height, radius, scratch, &mut blurred_foreground);
    box_blur_into(
        &background_scaled,
        width,
        height,
        radius,
        scratch,
        &mut blurred_background,
    );
    for index in 0..pixels {
        let a = alpha[index];
        let ba = blurred_alpha[index];
        let f_star = blurred_foreground[index] / (ba + 1e-5);
        let b_star = blurred_background[index] / ((1.0 - ba) + 1e-5);
        background_scaled[index] = b_star;
        refined[index] = f_star + a * (image[index] - a * f_star - (1.0 - a) * b_star);
    }
    (refined, background_scaled)
}

fn de_fringe_radius(max_side: u32) -> usize {
    let scaled = DE_FRINGE_BASE_RADIUS * max_side as f64 / DE_FRINGE_BASE_SIDE;
    (scaled.round() as usize).max(DE_FRINGE_MIN_RADIUS)
}

#[allow(clippy::too_many_arguments)]
fn refine_channel(
    image: &[f32],
    alpha: &[f32],
    blurred_alpha: &[f32],
    blurred_alpha_second: &[f32],
    width: usize,
    height: usize,
    radius: usize,
    scratch: &mut BlurScratch,
) -> Vec<f32> {
    let (first, background) = fb_estimate(
        image,
        image,
        image,
        alpha,
        blurred_alpha,
        width,
        height,
        radius,
        scratch,
    );
    let (second, _) = fb_estimate(
        image,
        &first,
        &background,
        alpha,
        blurred_alpha_second,
        width,
        height,
        6,
        scratch,
    );
    second
}

const DE_FRINGE_PARALLEL_MAX_PX: usize = 4 * 1024 * 1024;
const DE_FRINGE_PARALLEL_BYTES_PER_PX: u64 = 192;

fn de_fringe_parallel_ok(pixels: usize) -> bool {
    pixels <= DE_FRINGE_PARALLEL_MAX_PX
        || crate::usable_ram() >= (pixels as u64).saturating_mul(DE_FRINGE_PARALLEL_BYTES_PER_PX)
}

#[allow(clippy::too_many_arguments)]
fn refine_channels(
    channels: &[Vec<f32>; 3],
    alpha: &[f32],
    blurred_alpha: &[f32],
    blurred_alpha_second: &[f32],
    width: usize,
    height: usize,
    radius: usize,
    scratches: &mut [BlurScratch; 3],
) -> Vec<Vec<f32>> {
    if de_fringe_parallel_ok(width * height) {
        use rayon::prelude::*;
        channels
            .par_iter()
            .zip(scratches.par_iter_mut())
            .map(|(channel, scratch)| {
                refine_channel(
                    channel,
                    alpha,
                    blurred_alpha,
                    blurred_alpha_second,
                    width,
                    height,
                    radius,
                    scratch,
                )
            })
            .collect()
    } else {
        channels
            .iter()
            .map(|channel| {
                refine_channel(
                    channel,
                    alpha,
                    blurred_alpha,
                    blurred_alpha_second,
                    width,
                    height,
                    radius,
                    &mut scratches[0],
                )
            })
            .collect()
    }
}

fn apply_de_fringe(rgba: &mut RgbaImage, radius: usize) {
    let (width, height) = (rgba.width() as usize, rgba.height() as usize);
    let pixels = width * height;
    let mut alpha = vec![0f32; pixels];
    let mut channels = [vec![0f32; pixels], vec![0f32; pixels], vec![0f32; pixels]];
    for (index, pixel) in rgba.pixels().enumerate() {
        alpha[index] = pixel.0[3] as f32 / 255.0;
        for (channel, value) in pixel.0.iter().take(3).enumerate() {
            channels[channel][index] = *value as f32 / 255.0;
        }
    }
    let mut blur_scratch = BlurScratch::new();
    let mut blurred_alpha = vec![0f32; pixels];
    let mut blurred_alpha_second = vec![0f32; pixels];
    box_blur_into(&alpha, width, height, radius, &mut blur_scratch, &mut blurred_alpha);
    box_blur_into(&alpha, width, height, 6, &mut blur_scratch, &mut blurred_alpha_second);
    let mut channel_scratch = [BlurScratch::new(), BlurScratch::new(), BlurScratch::new()];
    let refined_channels = refine_channels(
        &channels,
        &alpha,
        &blurred_alpha,
        &blurred_alpha_second,
        width,
        height,
        radius,
        &mut channel_scratch,
    );
    for (index, pixel) in rgba.pixels_mut().enumerate() {
        for (channel, value) in pixel.0.iter_mut().take(3).enumerate() {
            let original = *value as f32 / 255.0;
            let refined = refined_channels[channel][index];
            let blended = original * (1.0 - DE_FRINGE_BLEND) + refined * DE_FRINGE_BLEND;
            *value = (blended.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
}

fn de_fringe_strip_height(width: usize, height: usize, radius: usize) -> usize {
    let margin = radius / 2 + 8;
    let planes = 9usize;
    let per_row = (width * 4 * planes).max(1);
    let affordable = DE_FRINGE_STRIP_BUDGET / per_row;
    let strip = affordable.saturating_sub(2 * margin);
    strip
        .clamp(DE_FRINGE_STRIP_MIN_H, DE_FRINGE_STRIP_H)
        .min(height.max(1))
}

fn apply_de_fringe_strips(rgba: &mut RgbaImage, radius: usize, strip_h: usize) {
    let (width, height) = (rgba.width() as usize, rgba.height() as usize);
    if strip_h == 0 || strip_h >= height {
        apply_de_fringe(rgba, radius);
        return;
    }
    let margin = radius / 2 + 8;
    let mut blur_scratch = BlurScratch::new();
    let mut channel_scratch = [BlurScratch::new(), BlurScratch::new(), BlurScratch::new()];
    let mut y0 = 0usize;
    while y0 < height {
        let y1 = (y0 + strip_h).min(height);
        let b0 = y0.saturating_sub(margin);
        let b1 = (y1 + margin).min(height);
        let rows = b1 - b0;
        let mut alpha = vec![0f32; width * rows];
        let mut channels = [
            vec![0f32; width * rows],
            vec![0f32; width * rows],
            vec![0f32; width * rows],
        ];
        for row in 0..rows {
            for column in 0..width {
                let pixel = rgba.get_pixel(column as u32, (b0 + row) as u32).0;
                let index = row * width + column;
                alpha[index] = pixel[3] as f32 / 255.0;
                for channel in 0..3 {
                    channels[channel][index] = pixel[channel] as f32 / 255.0;
                }
            }
        }
        let mut blurred_alpha = vec![0f32; width * rows];
        let mut blurred_alpha_second = vec![0f32; width * rows];
        box_blur_into(&alpha, width, rows, radius, &mut blur_scratch, &mut blurred_alpha);
        box_blur_into(
            &alpha,
            width,
            rows,
            6,
            &mut blur_scratch,
            &mut blurred_alpha_second,
        );
        let refined_channels = refine_channels(
            &channels,
            &alpha,
            &blurred_alpha,
            &blurred_alpha_second,
            width,
            rows,
            radius,
            &mut channel_scratch,
        );
        for row in y0..y1 {
            let source_row = row - b0;
            for column in 0..width {
                let pixel = rgba.get_pixel_mut(column as u32, row as u32);
                for (channel, plane) in refined_channels.iter().enumerate().take(3) {
                    let index = source_row * width + column;
                    let original = pixel.0[channel] as f32 / 255.0;
                    let refined = plane[index];
                    let blended = original * (1.0 - DE_FRINGE_BLEND) + refined * DE_FRINGE_BLEND;
                    pixel.0[channel] = (blended.clamp(0.0, 1.0) * 255.0).round() as u8;
                }
            }
        }
        y0 = y1;
    }
}

fn has_directml_in_system() -> bool {
    std::env::var("SystemRoot")
        .ok()
        .map(|root| Path::new(&root).join("System32").join("DirectML.dll").is_file())
        .unwrap_or(false)
}

fn env_runtime() -> Option<PathBuf> {
    std::env::var("ONNXRUNTIME_DLL")
        .ok()
        .map(PathBuf::from)
        .filter(|path| path.is_file())
}

fn exe_side_runtime() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let path = dir.join("onnxruntime.dll");
    if path.is_file() {
        Some(path)
    } else {
        None
    }
}

fn ort_logger() -> ort::logging::LoggerFunction {
    std::sync::Arc::new(
        |level: ort::logging::LogLevel, category: &str, _id: &str, location: &str, message: &str| {
            if std::env::var_os("SEAMAESTRO_ORT_LOG").is_none() {
                return;
            }
            eprintln!("  {level:?} {category} {location}: {message}");
        },
    )
}

fn init_from(path: &Path) -> Result<()> {
    crate::ort_runtime::prepare(path)?;
    ort::init_from(path)
        .map_err(|err| {
            anyhow::anyhow!(
                "{}: {err}",
                crate::msg()
                    .err_cut_load
                    .replacen("{}", &path.display().to_string(), 1)
            )
        })?
        .with_logger(ort_logger())
        .commit();
    Ok(())
}

fn init_ort() -> Result<()> {
    let mut failures: Vec<String> = Vec::new();
    if let Some(path) = env_runtime() {
        match init_from(&path) {
            Ok(()) => return Ok(()),
            Err(err) => failures.push(format!("ONNXRUNTIME_DLL: {err:#}")),
        }
    }
    match crate::ort_runtime::ensure_runtime().and_then(|path| init_from(&path)) {
        Ok(()) => return Ok(()),
        Err(err) => failures.push(format!("unpacked runtime: {err:#}")),
    }
    if let Some(path) = exe_side_runtime() {
        if let Some(dir) = path.parent() {
            let beside = |name: &str| dir.join(name).is_file();
            if !beside("onnxruntime_providers_shared.dll") {
                eprintln!("{}", crate::msg().note_providers_shared_missing);
            }
            if !beside("DirectML.dll") && !has_directml_in_system() {
                eprintln!("{}", crate::msg().note_directml_missing);
            }
        }
        match init_from(&path) {
            Ok(()) => return Ok(()),
            Err(err) => failures.push(format!("{}: {err:#}", path.display())),
        }
    }
    bail!(
        "{}",
        crate::msg()
            .err_cut_runtime_load
            .replacen("{}", &failures.join(" | "), 1)
    )
}

fn finish_frame(logits: Vec<f32>, img: &DynamicImage, de_fringe: bool, raw_alpha: bool) -> RgbaImage {
    let (width, height) = (img.width(), img.height());
    let alpha = post_alpha(logits);
    let rgb = img.to_rgb8();
    let mut rgba = RgbaImage::new(width, height);
    for (index, pixel) in rgba.pixels_mut().enumerate() {
        let x = (index as u32) % width;
        let y = (index as u32) / width;
        let source = rgb.get_pixel(x, y).0;
        let a = (alpha[index].clamp(0.0, 1.0) * 255.0).round() as u8;
        *pixel = image::Rgba([source[0], source[1], source[2], a]);
    }
    if de_fringe {
        let max_side = width.max(height);
        let radius = de_fringe_radius(max_side);
        if max_side <= crate::CUT_MAX_FRINGE_SIDE {
            apply_de_fringe(&mut rgba, radius);
        } else {
            let strip = de_fringe_strip_height(width as usize, height as usize, radius);
            if strip >= DE_FRINGE_STRIP_MIN_H && strip <= height as usize {
                apply_de_fringe_strips(&mut rgba, radius, strip);
            } else {
                eprintln!(
                    "{}",
                    crate::msg()
                        .note_de_fringe_skipped
                        .replacen("{}", &width.to_string(), 1)
                        .replacen("{}", &height.to_string(), 1)
                        .replacen("{}", &crate::CUT_MAX_FRINGE_SIDE.to_string(), 1)
                );
            }
        }
    }
    let levels = if raw_alpha { None } else { ALPHA_LEVELS };
    if let Some((black, white)) = levels {
        let span = (white - black).max(1e-8);
        for (index, pixel) in rgba.pixels_mut().enumerate() {
            let value = ((alpha[index] - black) / span).clamp(0.0, 1.0);
            pixel.0[3] = (value * 255.0).round() as u8;
        }
    }
    rgba
}

pub(crate) fn apply_cut(img: &DynamicImage, config: &Config) -> Result<DynamicImage> {
    let logits = {
        let mut guard = match SESSION.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                let mut guard = poisoned.into_inner();
                *guard = None;
                guard
            }
        };
        if guard.is_none() {
            init_ort()?;
            let mut ep = config.ep;
            if ep == EP_AUTO && crate::usable_ram() < CUT_MIN_FREE_RAM {
                eprintln!("  {}", crate::msg().note_cut_low_ram);
                ep = EP_CPU;
            }
            if ep == EP_CPU {
                eprintln!("  {}", crate::msg().cold_start_cpu);
            } else {
                eprintln!("  {}", crate::msg().cold_start_gpu);
            }
            let started = Instant::now();
            let (session, backend) = build_session(ep, config.threads)?;
            eprintln!(
                "{}",
                crate::msg()
                    .session_ready
                    .replacen("{}", backend, 1)
                    .replacen("{}", model_name(), 1)
                    .replacen("{}", &format!("{:.1}", started.elapsed().as_secs_f64()), 1)
            );
            let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
            let effective_threads = if config.threads == 0 { cores } else { config.threads };
            if backend.starts_with("CPU") && effective_threads <= 2 {
                eprintln!("  {}", crate::msg().note_cut_cpu_slow);
            }
            *guard = Some((session, !backend.starts_with("CPU")));
        }
        let is_dml = matches!(guard.as_ref(), Some((_, true)));
        let cap = if is_dml {
            EDGE_REFINE_MAX_TILES
        } else {
            EDGE_REFINE_MAX_TILES_CPU
        };
        let t_model = std::time::Instant::now();
        let first = {
            let (session, _) = guard.as_mut().expect("session is initialized above");
            frame_logits(session, img, config.tile, cap, is_dml, config.no_refine, config.profile)
        };
        let (logits, tiles, patch_px, overlap_px) = match first {
            Ok(result) => result,
            Err(err) if err.is::<DmlFatal>() && matches!(guard.as_ref(), Some((_, true))) => {
                let oom = matches!(
                    err.downcast_ref::<DmlFatal>().map(|fatal| fatal.kind),
                    Some(DmlFailure::Oom)
                );
                if oom {
                    if config.ep == EP_DML {
                        eprintln!("  {}", crate::msg().warn_dml_oom_explicit);
                    } else {
                        eprintln!("  {}", crate::msg().note_dml_oom_fallback);
                    }
                } else {
                    eprintln!("  {}", crate::msg().note_dml_gpu_fallback);
                }
                *guard = None;
                let started = Instant::now();
                let (session, _) = build_session(EP_CPU, config.threads)?;
                let backend = if oom {
                    "CPU (DirectML out of memory)"
                } else {
                    "CPU (DirectML error)"
                };
                eprintln!(
                    "{}",
                    crate::msg()
                        .session_ready
                        .replacen("{}", backend, 1)
                        .replacen("{}", model_name(), 1)
                        .replacen("{}", &format!("{:.1}", started.elapsed().as_secs_f64()), 1)
                );
                *guard = Some((session, false));
                let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
                let effective_threads = if config.threads == 0 { cores } else { config.threads };
                if effective_threads <= 2 {
                    eprintln!("  {}", crate::msg().note_cut_cpu_slow);
                }
                let (session, _) = guard.as_mut().expect("session is initialized above");
                frame_logits(session, img, config.tile, EDGE_REFINE_MAX_TILES_CPU, false, config.no_refine, config.profile)?
            }
            Err(err) if err.is::<DmlFatal>() => {
                bail!("{}", crate::msg().err_cut_oom_no_fallback);
            }
            Err(err) => return Err(err),
        };
        if tiles > 1 {
            eprintln!(
                "{}",
                crate::msg()
                    .note_tiles
                    .replacen("{}", &tiles.to_string(), 1)
                    .replacen("{}", &patch_px.to_string(), 1)
                    .replacen("{}", &overlap_px.to_string(), 1)
            );
        }
        if config.profile {
            eprintln!("  ⏱ cut model {:.2} s", t_model.elapsed().as_secs_f64());
        }
        logits
    };
    let t_de_fringe = std::time::Instant::now();
    let rgba = finish_frame(logits, img, config.de_fringe, config.raw_alpha);
    if config.profile {
        eprintln!("  ⏱ cut de-fringe {:.2} s", t_de_fringe.elapsed().as_secs_f64());
    }
    Ok(DynamicImage::ImageRgba8(rgba))
}



#[cfg(test)]
mod tests {
    use super::{classify_dml_failure, cpu_refine_note_once, DmlFailure};

    #[test]
    fn oom_signatures_are_classified_as_oom() {
        assert_eq!(
            classify_dml_failure(
                "[E:onnxruntime:, sequential_executor.cc:572] Non-zero status code returned while running Add node. Status Message: DmlCommon::TranslateHresult] 0x8007000E"
            ),
            Some(DmlFailure::Oom)
        );
        assert_eq!(
            classify_dml_failure("Not enough memory resources are available to complete this operation."),
            Some(DmlFailure::Oom)
        );
        assert_eq!(classify_dml_failure("E_OUTOFMEMORY"), Some(DmlFailure::Oom));
        assert_eq!(classify_dml_failure("dml out of memory"), Some(DmlFailure::Oom));
    }

    #[test]
    fn cpu_refine_note_fires_once_and_only_on_cpu() {
        assert!(
            !cpu_refine_note_once(true),
            "a GPU backend must never show the CPU note"
        );
        assert!(
            cpu_refine_note_once(false),
            "the first CPU refine pass must show the note"
        );
        assert!(
            !cpu_refine_note_once(false),
            "the note must not repeat for the next files"
        );
    }

    #[test]
    fn dxgi_driver_errors_are_classified_as_gpu_failure() {
        assert_eq!(
            classify_dml_failure(
                "Exception(2) tid(4e30) 887A0020 An internal issue prevented the driver from carrying out the specified operation. The driver's state is probably suspect, and the application should not continue."
            ),
            Some(DmlFailure::Gpu)
        );
        assert_eq!(
            classify_dml_failure("887A0005 The GPU device instance has been suspended"),
            Some(DmlFailure::Gpu)
        );
        assert_eq!(
            classify_dml_failure("887A0006 The GPU will not respond to more commands"),
            Some(DmlFailure::Gpu)
        );
        assert_eq!(
            classify_dml_failure("Non-zero status code returned while running Add: DmlExecutionProvider failure 0x1"),
            Some(DmlFailure::Gpu)
        );
    }

    #[test]
    fn other_errors_are_not_treated_as_dml_failures() {
        assert_eq!(classify_dml_failure("cannot build input tensor"), None);
        assert_eq!(
            classify_dml_failure("Non-zero status code returned while running Resize: Invalid argument"),
            None
        );
        assert_eq!(
            classify_dml_failure("unexpected model output (expected float32 or float16)"),
            None
        );
    }
}

#[cfg(test)]
mod strip_probe {
    use super::{apply_de_fringe, apply_de_fringe_strips};
    use image::{Rgba, RgbaImage};

    fn sample(width: u32, height: u32) -> RgbaImage {
        let mut img = RgbaImage::new(width, height);
        for (x, y, pixel) in img.enumerate_pixels_mut() {
            let hard = if (y / 512) % 2 == 0 { 250 } else { 10 };
            let soft = if x < width / 2 { 255 } else { 0 };
            *pixel = Rgba([hard, (x % 251) as u8, (y % 241) as u8, soft]);
        }
        img
    }

    #[test]
    fn strips_match_whole_frame() {
        let (width, height) = (48u32, 1600u32);
        let source = sample(width, height);
        let radius = 12usize;
        let mut whole = source.clone();
        apply_de_fringe(&mut whole, radius);
        for strip in [128usize, 512usize, height as usize] {
            let mut tiled = source.clone();
            apply_de_fringe_strips(&mut tiled, radius, strip);
            assert_eq!(
                whole.as_raw(),
                tiled.as_raw(),
                "strip={strip} de-fringe differs from the whole-frame result"
            );
        }
    }
}

#[cfg(test)]
mod tile_probe {
    use super::{
        edge_tiles, refine_plan, RefinePlan, EDGE_REFINE_MAX_TILES, EDGE_REFINE_MAX_TILES_CPU,
        INPUT_SIZE, TILE_OVERLAP,
    };
    use image::GenericImageView;

    #[test]
    #[ignore = "diagnostic: run with SM_TILE_PROBE set to a cut PNG (alpha = the edge map)"]
    fn report_tiles_needed() {
        let list = match std::env::var("SM_TILE_PROBE") {
            Ok(value) => value,
            Err(_) => return,
        };
        let mut lines: Vec<String> = Vec::new();
        for path in list.split('|') {
            let path = path.trim();
            if path.is_empty() {
                continue;
            }
            let img = match image::open(path) {
                Ok(img) => img,
                Err(err) => {
                    lines.push(format!("{path}: cannot open ({err})"));
                    continue;
                }
            };
            let (w, h) = img.dimensions();
            let small = img
                .resize_exact(INPUT_SIZE as u32, INPUT_SIZE as u32, image::imageops::FilterType::Triangle)
                .to_rgba8();
            let mut coarse = vec![0f32; INPUT_SIZE * INPUT_SIZE];
            for (index, pixel) in small.pixels().enumerate() {
                coarse[index] = pixel.0[3] as f32 / 255.0;
            }
            let picked = edge_tiles(&coarse, w, h, INPUT_SIZE as u32, TILE_OVERLAP);
            lines.push(format!(
                "{w}x{h}  tiles_needed={}  cap={EDGE_REFINE_MAX_TILES}  capped={}",
                picked.len(),
                picked.len() > EDGE_REFINE_MAX_TILES
            ));
            lines.push(format!("  file={path}"));
        }
        let _ = std::fs::write(std::env::temp_dir().join("sm_tile_probe.txt"), lines.join("\n"));
    }

    fn flat_edge(value: f32) -> Vec<f32> {
        vec![value; INPUT_SIZE * INPUT_SIZE]
    }

    #[test]
    fn scale_plan_keeps_native_when_it_fits_the_cap() {
        let picked = match refine_plan(&flat_edge(0.5), 5888, 4416, EDGE_REFINE_MAX_TILES) {
            RefinePlan::Scaled(scale, picked) => {
                assert_eq!(scale, 1, "native scale must win while it fits");
                picked
            }
            _ => panic!("expected a native-scale plan"),
        };
        assert_eq!(picked.len(), 48);
        assert!(picked.iter().any(|(x, y, _)| *x == 0 && *y == 0));
        assert!(picked.iter().any(|(x, y, _)| *x == 4864 && *y == 3392));
    }

    #[test]
    fn scale_plan_steps_down_for_tight_caps() {
        let coarse = flat_edge(0.5);
        for (cap, expected) in [(12usize, 2u32), (4usize, 4u32)] {
            match refine_plan(&coarse, 5888, 4416, cap) {
                RefinePlan::Scaled(scale, picked) => {
                    assert_eq!(scale, expected);
                    assert!(picked.len() <= cap);
                }
                _ => panic!("expected a scaled plan for cap {cap}"),
            }
        }
    }

    #[test]
    fn scale_plan_separates_empty_from_capped() {
        let blank = flat_edge(0.0);
        assert!(matches!(
            refine_plan(&blank, 5888, 4416, EDGE_REFINE_MAX_TILES),
            RefinePlan::Empty
        ));
        assert!(matches!(
            refine_plan(&flat_edge(0.5), 5888, 4416, 0),
            RefinePlan::Capped
        ));
    }

    #[test]
    fn scale_plan_keeps_single_tile_native() {
        match refine_plan(&flat_edge(0.5), 1024, 1024, EDGE_REFINE_MAX_TILES_CPU) {
            RefinePlan::Scaled(scale, picked) => {
                assert_eq!(scale, 1);
                assert_eq!(picked.len(), 1);
            }
            _ => panic!("expected a single native tile"),
        }
    }
}
