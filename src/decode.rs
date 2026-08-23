use anyhow::{Context, Result};
use std::path::Path;
use std::sync::{Condvar, Mutex, OnceLock};

use image::ImageDecoder;

use crate::msg;
use crate::usable_ram;

const GIB: u64 = 1024 * 1024 * 1024;
const HARD_CAP: u64 = 8 * GIB;
const RAM_FRACTION: f64 = 0.6;

struct RuntimeLimits {
    max_alloc: u64,
    #[allow(dead_code)]
    budget: u64,
}

fn runtime_limits() -> &'static RuntimeLimits {
    static LIMITS: OnceLock<RuntimeLimits> = OnceLock::new();
    LIMITS.get_or_init(|| {
        let usable = (usable_ram() as f64 * RAM_FRACTION) as u64;
        RuntimeLimits {
            max_alloc: usable.min(HARD_CAP),
            budget: usable,
        }
    })
}

pub(crate) struct MemBudget {
    total: u64,
    used: Mutex<u64>,
    cv: Condvar,
}

pub(crate) struct MemPermit<'a> {
    pub(crate) budget: &'a MemBudget,
    pub(crate) need: u64,
}

impl Drop for MemPermit<'_> {
    fn drop(&mut self) {
        self.budget.release(self.need);
    }
}

impl MemBudget {
    pub(crate) fn acquire(&self, need: u64) {
        let need = need.min(self.total.max(1));
        let mut used = self.used.lock().unwrap();
        while *used + need > self.total {
            used = self.cv.wait(used).unwrap();
        }
        *used += need;
    }

    fn release(&self, need: u64) {
        let need = need.min(self.total.max(1));
        let mut used = self.used.lock().unwrap();
        *used = used.saturating_sub(need);
        self.cv.notify_all();
    }
}

pub(crate) fn mem_budget() -> &'static MemBudget {
    static BUDGET: OnceLock<MemBudget> = OnceLock::new();
    BUDGET.get_or_init(|| MemBudget {
        total: runtime_limits().budget,
        used: Mutex::new(0),
        cv: Condvar::new(),
    })
}

pub(crate) fn probe_image(raw: &[u8]) -> u64 {
    let max = runtime_limits().max_alloc;
    let (w, h) = probe_dims(raw).unwrap_or((32768, 32768));
    (w as u64)
        .saturating_mul(h as u64)
        .saturating_mul(4)
        .clamp(1, max)
}

fn probe_dims(raw: &[u8]) -> Option<(u32, u32)> {
    if raw.len() >= 24 && raw.starts_with(b"\x89PNG\r\n\x1a\n") {
        let w = u32::from_be_bytes([raw[16], raw[17], raw[18], raw[19]]);
        let h = u32::from_be_bytes([raw[20], raw[21], raw[22], raw[23]]);
        return Some((w, h));
    }
    if raw.len() >= 10 && (raw.starts_with(b"GIF87a") || raw.starts_with(b"GIF89a")) {
        let w = u16::from_le_bytes([raw[6], raw[7]]) as u32;
        let h = u16::from_le_bytes([raw[8], raw[9]]) as u32;
        return Some((w, h));
    }
    if raw.len() >= 26 && raw.starts_with(b"BM") {
        let w = i32::from_le_bytes([raw[18], raw[19], raw[20], raw[21]]).unsigned_abs();
        let h = i32::from_le_bytes([raw[22], raw[23], raw[24], raw[25]]).unsigned_abs();
        return Some((w, h));
    }
    if raw.len() >= 14 && raw.starts_with(b"qoif") {
        let w = u32::from_be_bytes([raw[4], raw[5], raw[6], raw[7]]);
        let h = u32::from_be_bytes([raw[8], raw[9], raw[10], raw[11]]);
        return Some((w, h));
    }
    if raw.len() >= 3 && raw[0] == 0xFF && raw[1] == 0xD8 && raw[2] == 0xFF {
        return jpeg_dims(raw);
    }
    if raw.len() >= 30 && raw.starts_with(b"RIFF") && &raw[8..12] == b"WEBP" {
        return webp_dims(raw);
    }
    None
}

fn jpeg_dims(raw: &[u8]) -> Option<(u32, u32)> {
    use zune_core::bytestream::ZCursor;
    use zune_core::options::DecoderOptions;
    use zune_jpeg::JpegDecoder;

    let mut decoder = JpegDecoder::new(ZCursor::new(raw));
    decoder.set_options(
        DecoderOptions::new_fast()
            .set_max_width(65535)
            .set_max_height(65535),
    );
    decoder.decode_headers().ok()?;
    let info = decoder.info()?;
    Some((u32::from(info.width), u32::from(info.height)))
}

fn webp_dims(raw: &[u8]) -> Option<(u32, u32)> {
    match &raw[12..16] {
        b"VP8X" if raw.len() >= 30 => {
            let w = 1 + u32::from_le_bytes([raw[24], raw[25], raw[26], 0]);
            let h = 1 + u32::from_le_bytes([raw[27], raw[28], raw[29], 0]);
            Some((w, h))
        }
        b"VP8L" if raw.len() >= 25 => {
            let v = u32::from_le_bytes([raw[21], raw[22], raw[23], raw[24]]);
            Some(((v & 0x3FFF) + 1, ((v >> 14) & 0x3FFF) + 1))
        }
        b"VP8 " if raw.len() >= 30 && raw[23..26] == [0x9D, 0x01, 0x2A] => {
            let w = u16::from_le_bytes([raw[26], raw[27]]) & 0x3FFF;
            let h = u16::from_le_bytes([raw[28], raw[29]]) & 0x3FFF;
            Some((w as u32, h as u32))
        }
        _ => Some((16383, 16383)),
    }
}

fn jpeg_exif(raw: &[u8]) -> Option<Vec<u8>> {
    if raw.len() < 4 {
        return None;
    }
    let mut i = 2usize;
    while i + 4 <= raw.len() {
        if raw[i] != 0xFF {
            return None;
        }
        let marker = raw[i + 1];
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        if marker == 0xD9 || marker == 0xDA {
            return None;
        }
        if marker == 0xFF {
            i += 1;
            continue;
        }
        let len = u16::from_be_bytes([raw[i + 2], raw[i + 3]]) as usize;
        if len < 2 || i + 2 + len > raw.len() {
            return None;
        }
        if marker == 0xE1 && len >= 8 && &raw[i + 4..i + 10] == b"Exif\0\0" {
            return Some(raw[i + 10..i + 2 + len].to_vec());
        }
        i += 2 + len;
    }
    None
}

fn png_exif(raw: &[u8]) -> Option<Vec<u8>> {
    if raw.len() < 8 || &raw[0..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let mut i = 8usize;
    while i + 12 <= raw.len() {
        let len = u32::from_be_bytes([raw[i], raw[i + 1], raw[i + 2], raw[i + 3]]) as usize;
        if i + 12 + len > raw.len() {
            return None;
        }
        if &raw[i + 4..i + 8] == b"eXIf" {
            return Some(raw[i + 8..i + 8 + len].to_vec());
        }
        i += 12 + len;
    }
    None
}

fn webp_exif(raw: &[u8]) -> Option<Vec<u8>> {
    if raw.len() < 20 || &raw[0..4] != b"RIFF" || &raw[8..12] != b"WEBP" {
        return None;
    }
    let mut i = 12usize;
    while i + 8 <= raw.len() {
        let fourcc = &raw[i..i + 4];
        let size = u32::from_le_bytes([raw[i + 4], raw[i + 5], raw[i + 6], raw[i + 7]]) as usize;
        let start = i + 8;
        if start + size > raw.len() {
            return None;
        }
        if fourcc == b"EXIF" {
            return Some(raw[start..start + size].to_vec());
        }
        i = start + size + (size & 1);
    }
    None
}

fn jxl_exif(raw: &[u8]) -> Option<Vec<u8>> {
    use jxl_oxide::{AuxBoxData, JxlImage};
    let image = JxlImage::builder().read(std::io::Cursor::new(raw)).ok()?;
    let raw_exif = match image.aux_boxes().first_exif().ok()? {
        AuxBoxData::Data(r) => r,
        _ => return None,
    };
    let off = raw_exif.tiff_header_offset() as usize;
    Some(raw_exif.payload().get(off..)?.to_vec())
}

fn extract_exif(raw: &[u8]) -> Option<Vec<u8>> {
    if raw.len() >= 3 && raw[0] == 0xFF && raw[1] == 0xD8 && raw[2] == 0xFF {
        return jpeg_exif(raw);
    }
    if raw.len() >= 8 && &raw[0..8] == b"\x89PNG\r\n\x1a\n" {
        return png_exif(raw);
    }
    if raw.len() >= 12 && &raw[0..4] == b"RIFF" && &raw[8..12] == b"WEBP" {
        return webp_exif(raw);
    }
    if raw.len() >= 8 && &raw[4..8] == b"JXL " {
        return jxl_exif(raw);
    }
    None
}

// ── decode_image ──────────────────────────────────────────────

pub(crate) fn decode_image(raw: &[u8], path: Option<&Path>) -> Result<(image::DynamicImage, Option<Vec<u8>>, Option<Vec<u8>>)> {
    let exif = extract_exif(raw);
    if raw.len() >= 3 && raw[0] == 0xFF && raw[1] == 0xD8 && raw[2] == 0xFF {
        if let Some((img, icc)) = decode_jpeg_fast(raw) {
            return Ok((img, icc, exif));
        }
    }
    if is_raw_bytes(raw) {
        let raw_img = match path {
            Some(p) => decode_raw(p).ok(),
            None => decode_raw_bytes(raw).ok(),
        };
        if let Some(img) = raw_img {
            return Ok((img, None, exif));
        }
    }
    if let Ok((img, icc)) = decode_with_limits(raw) {
        return Ok((img, icc, exif));
    }
    if is_heif(raw) {
        if let Ok(img) = decode_heif_manual(raw, path) {
            return Ok((img, None, exif));
        }
    }
    if let Some(p) = path {
        if let Ok(img) = decode_raw(p) {
            return Ok((img, None, exif));
        }
    }
    anyhow::bail!("{}", msg().err_unsupported)
}

fn decode_with_limits(raw: &[u8]) -> image::ImageResult<(image::DynamicImage, Option<Vec<u8>>)> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(raw));
    reader = reader.with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(32768);
    limits.max_image_height = Some(32768);
    limits.max_alloc = Some(runtime_limits().max_alloc);
    reader.limits(limits);
    let mut decoder = reader.into_decoder()?;
    let keep_icc = matches!(
        decoder.original_color_type(),
        image::ExtendedColorType::Rgb8
            | image::ExtendedColorType::Rgba8
            | image::ExtendedColorType::Rgb16
            | image::ExtendedColorType::Rgba16
            | image::ExtendedColorType::Rgb32F
            | image::ExtendedColorType::Rgba32F
            | image::ExtendedColorType::Bgr8
            | image::ExtendedColorType::Bgra8
    );
    let icc = if keep_icc { decoder.icc_profile()? } else { None };
    let img = image::DynamicImage::from_decoder(decoder)?;
    Ok((img, icc))
}

fn decode_jpeg_fast(raw: &[u8]) -> Option<(image::DynamicImage, Option<Vec<u8>>)> {
    let (dw, dh) = jpeg_dims(raw)?;
    let worst = (dw as u64).saturating_mul(dh as u64).saturating_mul(4);
    if worst > runtime_limits().max_alloc {
        return None;
    }

    let mut decoder = zune_jpeg::JpegDecoder::new(zune_core::bytestream::ZCursor::new(raw));
    let data = decoder.decode().ok()?;
    let info = decoder.info()?;
    let px = decoder.output_colorspace()?;
    let (w, h) = (info.width as u32, info.height as u32);
    let icc = match px {
        zune_core::colorspace::ColorSpace::RGB | zune_core::colorspace::ColorSpace::RGBA => {
            decoder.icc_profile()
        }
        _ => None,
    };
    let img = match px {
        zune_core::colorspace::ColorSpace::RGB => {
            image::DynamicImage::ImageRgb8(image::RgbImage::from_raw(w, h, data)?)
        }
        zune_core::colorspace::ColorSpace::Luma => {
            image::DynamicImage::ImageLuma8(image::GrayImage::from_raw(w, h, data)?)
        }
        zune_core::colorspace::ColorSpace::RGBA => {
            image::DynamicImage::ImageRgba8(image::RgbaImage::from_raw(w, h, data)?)
        }
        _ => return None,
    };
    Some((img, icc))
}

// ── HEIF helpers (libheif-rs 2.7 + image feature) ─────────────

pub(crate) fn is_heif(buf: &[u8]) -> bool {
    buf.len() > 12 && (
        &buf[4..11] == b"ftyphei"   ||
        &buf[4..12] == b"ftypmif1"  ||
        &buf[4..12] == b"ftypmsf1"
    )
}

fn decode_heif_manual(buf: &[u8], _path: Option<&Path>) -> Result<image::DynamicImage> {
    use libheif_rs::{HeifContext, LibHeif, ColorSpace, RgbChroma};

    let libheif = LibHeif::new();
    let ctx = HeifContext::read_from_bytes(buf).context(msg().err_heif_read)?;
    let handle = ctx.primary_image_handle().context(msg().err_heif_primary)?;

    let has_alpha = handle.has_alpha_channel();
    let color_space = if has_alpha {
        ColorSpace::Rgb(RgbChroma::Rgba)
    } else {
        ColorSpace::Rgb(RgbChroma::Rgb)
    };

    let heif_img = libheif
        .decode(&handle, color_space, None)
        .context(msg().err_heif_decode)?;

    let planes = heif_img.planes();
    let plane = planes.interleaved.context(msg().err_heif_plane)?;

    let width = plane.width;
    let height = plane.height;
    let stride = plane.stride;
    let bpp = (plane.storage_bits_per_pixel / 8) as usize;
    let row_size = width as usize * bpp;

    if stride == 0 || stride < row_size {
        anyhow::bail!("{}", msg().err_heif_plane);
    }

    let mut packed = Vec::with_capacity(row_size * height as usize);
    for row in plane.data.chunks_exact(stride).take(height as usize) {
        packed.extend_from_slice(&row[..row_size]);
    }

    let img = if has_alpha {
        image::RgbaImage::from_raw(width, height, packed).map(image::DynamicImage::ImageRgba8)
    } else {
        image::RgbImage::from_raw(width, height, packed).map(image::DynamicImage::ImageRgb8)
    };

    Ok(img.context(msg().err_heif_decode)?)
}

// ── RAW helpers ───────────────────────────────────────────────

fn is_raw_bytes(raw: &[u8]) -> bool {
    if raw.len() >= 4 && (&raw[0..4] == b"II*\0" || &raw[0..4] == b"MM\0*") {
        return true;
    }
    if raw.len() >= 12 && &raw[4..12] == b"ftypcrx " { return true; }
    if raw.len() >= 15 && &raw[0..15] == b"FUJIFILMCCD-RAW" { return true; }
    if raw.len() >= 4 && (&raw[0..4] == b"IIRO" || &raw[0..4] == b"MMOR") { return true; }
    if raw.len() >= 4 && &raw[0..4] == b"FOVb" { return true; }
    if raw.len() >= 4 && &raw[0..4] == b"\0MRM" { return true; }
    if raw.len() >= 4 && (&raw[0..4] == b"IIII" || &raw[0..4] == b"MMMM") { return true; }
    if raw.len() >= 4 && &raw[0..4] == b"ARRI" { return true; }
    if raw.len() >= 14 && &raw[0..14] == b"II\x1A\0\0\0HEAPCCDR" { return true; }
    if raw.len() >= 4 && &raw[0..4] == b"IIU\0" { return true; }
    false
}

fn decode_raw(path: &Path) -> Result<image::DynamicImage> {
    let rawimage = rawler::decode_file(path).context(msg().err_raw_decode)?;
    develop_raw(rawimage)
}

fn decode_raw_bytes(raw: &[u8]) -> Result<image::DynamicImage> {
    let source = rawler::rawsource::RawSource::new_from_slice(raw);
    let rawimage = rawler::decode(&source, &rawler::decoders::RawDecodeParams::default())
        .context(msg().err_raw_decode)?;
    develop_raw(rawimage)
}

fn develop_raw(rawimage: rawler::RawImage) -> Result<image::DynamicImage> {
    let intermediate = rawler::imgop::develop::RawDevelop::default()
        .develop_intermediate(&rawimage)
        .context(msg().err_raw_decode)?;
    intermediate.to_dynamic_image().context(msg().err_raw_build)
}