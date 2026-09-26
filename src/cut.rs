use std::path::{Path, PathBuf};
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
const DE_FRINGE_BLEND: f32 = 0.75;

#[allow(dead_code)]
pub(crate) const EP_AUTO: u8 = 0;
pub(crate) const EP_CPU: u8 = 1;
pub(crate) const EP_DML: u8 = 2;

static SESSION: Mutex<Option<(Session, bool)>> = Mutex::new(None);

fn ort_err<R>(err: ort::Error<R>) -> anyhow::Error {
    anyhow::anyhow!("{err}")
}

#[derive(Debug)]
struct DmlOom {
    detail: String,
}

impl std::fmt::Display for DmlOom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.detail)
    }
}

impl std::error::Error for DmlOom {}

fn is_oom(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("8007000e")
        || lower.contains("e_outofmemory")
        || lower.contains("out of memory")
        || lower.contains("not enough memory")
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
            if is_oom(&detail) {
                anyhow::Error::new(DmlOom { detail })
            } else {
                ort_err(err).context(crate::msg().err_infer_failed)
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
    if low < 0.0 || high > 1.0 {
        for value in alpha.iter_mut() {
            *value = 1.0 / (1.0 + (-(*value)).exp());
        }
        low = f32::INFINITY;
        high = f32::NEG_INFINITY;
        for value in alpha.iter() {
            low = low.min(*value);
            high = high.max(*value);
        }
    }
    let span = (high - low).max(1e-8);
    for value in alpha.iter_mut() {
        *value = (*value - low) / span;
    }
    alpha
}

fn tile_origins(width: u32, height: u32) -> Vec<(u32, u32)> {
    let tile = INPUT_SIZE as u32;
    let step = tile - TILE_OVERLAP;
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

fn frame_logits(session: &mut Session, img: &DynamicImage, tiled: bool) -> Result<(Vec<f32>, usize)> {
    let width = img.width();
    let height = img.height();
    let tile = INPUT_SIZE as u32;
    if !tiled || (width <= tile && height <= tile) {
        let logits = infer(session, preprocess(img)?)?;
        return Ok((resize_logits(&logits, width, height), 1));
    }
    let (w, h) = (width as usize, height as usize);
    let overlap = TILE_OVERLAP as usize;
    let mut combined = vec![0f32; w * h];
    let mut total = vec![0f32; w * h];
    let mut count = 0usize;
    for (x, y) in tile_origins(width, height) {
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
    Ok((combined, count))
}

fn box_blur(src: &[f32], width: usize, height: usize, radius: usize) -> Vec<f32> {
    if radius <= 1 {
        return src.to_vec();
    }
    let r = radius / 2;
    let stride = width + 1;
    let mut sat = vec![0f64; stride * (height + 1)];
    for y in 0..height {
        let mut row_sum = 0f64;
        for x in 0..width {
            row_sum += src[y * width + x] as f64;
            sat[(y + 1) * stride + x + 1] = sat[y * stride + x + 1] + row_sum;
        }
    }
    let mut out = vec![0f32; width * height];
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
    out
}

fn fb_estimate(
    image: &[f32],
    foreground: &[f32],
    background: &[f32],
    alpha: &[f32],
    width: usize,
    height: usize,
    radius: usize,
) -> (Vec<f32>, Vec<f32>) {
    let pixels = width * height;
    let blurred_alpha = box_blur(alpha, width, height, radius);
    let mut foreground_scaled = vec![0f32; pixels];
    let mut background_scaled = vec![0f32; pixels];
    for index in 0..pixels {
        let a = alpha[index];
        foreground_scaled[index] = foreground[index] * a;
        background_scaled[index] = background[index] * (1.0 - a);
    }
    let blurred_foreground = box_blur(&foreground_scaled, width, height, radius);
    let blurred_background = box_blur(&background_scaled, width, height, radius);
    let mut refined = vec![0f32; pixels];
    let mut background_out = vec![0f32; pixels];
    for index in 0..pixels {
        let a = alpha[index];
        let ba = blurred_alpha[index];
        let f_star = blurred_foreground[index] / (ba + 1e-5);
        let b_star = blurred_background[index] / ((1.0 - ba) + 1e-5);
        background_out[index] = b_star;
        refined[index] = f_star + a * (image[index] - a * f_star - (1.0 - a) * b_star);
    }
    (refined, background_out)
}

fn de_fringe_radius(max_side: u32) -> usize {
    let scaled = DE_FRINGE_BASE_RADIUS * max_side as f64 / DE_FRINGE_BASE_SIDE;
    (scaled.round() as usize).max(DE_FRINGE_MIN_RADIUS)
}

fn refine_channel(
    image: &[f32],
    alpha: &[f32],
    width: usize,
    height: usize,
    radius: usize,
) -> Vec<f32> {
    let (first, background) = fb_estimate(image, image, image, alpha, width, height, radius);
    let (second, _) = fb_estimate(image, &first, &background, alpha, width, height, 6);
    second
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
    for channel in channels.iter_mut() {
        *channel = refine_channel(channel, &alpha, width, height, radius);
    }
    for (index, pixel) in rgba.pixels_mut().enumerate() {
        for (channel, value) in pixel.0.iter_mut().take(3).enumerate() {
            let original = *value as f32 / 255.0;
            let refined = channels[channel][index];
            let blended = original * (1.0 - DE_FRINGE_BLEND) + refined * DE_FRINGE_BLEND;
            *value = (blended.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
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
        if max_side <= crate::CUT_MAX_FRINGE_SIDE {
            apply_de_fringe(&mut rgba, de_fringe_radius(max_side));
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
            if config.ep == EP_CPU {
                eprintln!("  {}", crate::msg().cold_start_cpu);
            } else {
                eprintln!("  {}", crate::msg().cold_start_gpu);
            }
            let started = Instant::now();
            let (session, backend) = build_session(config.ep, config.threads)?;
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
        let first = {
            let (session, _) = guard.as_mut().expect("session is initialized above");
            frame_logits(session, img, config.tile)
        };
        let (logits, tiles) = match first {
            Ok(result) => result,
            Err(err) if err.is::<DmlOom>() && matches!(guard.as_ref(), Some((_, true))) => {
                if config.ep == EP_DML {
                    eprintln!("  {}", crate::msg().warn_dml_oom_explicit);
                } else {
                    eprintln!("  {}", crate::msg().note_dml_oom_fallback);
                }
                *guard = None;
                let started = Instant::now();
                let (session, _) = build_session(EP_CPU, config.threads)?;
                eprintln!(
                    "{}",
                    crate::msg()
                        .session_ready
                        .replacen("{}", "CPU (DirectML out of memory)", 1)
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
                frame_logits(session, img, config.tile)?
            }
            Err(err) if err.is::<DmlOom>() => {
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
                    .replacen("{}", &INPUT_SIZE.to_string(), 1)
                    .replacen("{}", &TILE_OVERLAP.to_string(), 1)
            );
        }
        logits
    };
    let rgba = finish_frame(logits, img, config.de_fringe, config.raw_alpha);
    Ok(DynamicImage::ImageRgba8(rgba))
}



#[cfg(test)]
mod tests {
    use super::is_oom;

    #[test]
    fn oom_signatures_are_detected() {
        assert!(is_oom(
            "[E:onnxruntime:, sequential_executor.cc:572] Non-zero status code returned while running Add node. Status Message: DmlCommon::TranslateHresult] 0x8007000E"
        ));
        assert!(is_oom("Not enough memory resources are available to complete this operation."));
        assert!(is_oom("E_OUTOFMEMORY"));
        assert!(is_oom("dml out of memory"));
    }

    #[test]
    fn non_oom_errors_are_not_treated_as_oom() {
        assert!(!is_oom("cannot build input tensor"));
        assert!(!is_oom("Non-zero status code returned while running Resize: Invalid argument"));
        assert!(!is_oom("unexpected model output (expected float32 or float16)"));
    }
}
