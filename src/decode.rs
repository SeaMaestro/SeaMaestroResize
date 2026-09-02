use anyhow::{Context, Result};
use std::io::Read;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use flate2::read::GzDecoder;
use image::ImageDecoder;

use crate::msg;
use crate::usable_ram;

use resvg::{tiny_skia, usvg};

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
        let usable = ((usable_ram() as f64 * RAM_FRACTION) as u64).max(256 * 1024 * 1024);
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
        let mut used = self.used.lock().unwrap_or_else(|e| e.into_inner());
        while *used + need > self.total {
            used = self.cv.wait(used).unwrap_or_else(|e| e.into_inner());
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

pub(crate) fn probe_dims(raw: &[u8]) -> Option<(u32, u32)> {
    if looks_like_svg(raw) {
        return probe_svg_dims(raw);
    }
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
    if (raw.len() >= 8 && &raw[4..8] == b"JXL ") || raw.starts_with(&[0xFF, 0x0A]) {
        if let Ok(image) = jxl_oxide::JxlImage::builder().read(std::io::Cursor::new(raw)) {
            return Some((image.width(), image.height()));
        }
    }
    if raw.len() >= 12 && &raw[4..12] == b"ftypcrx " {
        return bmff_tkhd_dims(raw);
    }
    if raw.len() >= 12 && &raw[4..8] == b"ftyp" {
        if let Ok(ctx) = libheif_rs::HeifContext::read_from_bytes(raw) {
            if let Ok(handle) = ctx.primary_image_handle() {
                return Some((handle.width(), handle.height()));
            }
        }
    }
    if raw.len() >= 8 && (raw.starts_with(b"II*\0") || raw.starts_with(b"MM\0*")) {
        return tiff_dims(raw);
    }
    if raw.len() >= 6 && &raw[0..4] == [0, 0, 1, 0] {
        return ico_dims(raw);
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

fn rd16(b: &[u8], off: usize, little: bool) -> Option<u16> {
    let s = b.get(off..off + 2)?;
    Some(if little { u16::from_le_bytes([s[0], s[1]]) } else { u16::from_be_bytes([s[0], s[1]]) })
}

fn rd32(b: &[u8], off: usize, little: bool) -> Option<u32> {
    let s = b.get(off..off + 4)?;
    Some(if little { u32::from_le_bytes([s[0], s[1], s[2], s[3]]) } else { u32::from_be_bytes([s[0], s[1], s[2], s[3]]) })
}

fn tiff_dims(raw: &[u8]) -> Option<(u32, u32)> {
    let little = raw.starts_with(b"II*\0");
    let ifd0 = rd32(raw, 4, little)? as usize;
    let n = rd16(raw, ifd0, little)? as usize;
    let mut w = None;
    let mut h = None;
    for i in 0..n {
        let e = ifd0 + 2 + i * 12;
        if e + 12 > raw.len() { break; }
        let tag = rd16(raw, e, little)?;
        let typ = rd16(raw, e + 2, little)?;
        let val = match typ {
            3 => rd16(raw, e + 8, little)? as u32,
            4 => rd32(raw, e + 8, little)?,
            _ => continue,
        };
        if tag == 0x0100 { w = Some(val); }
        else if tag == 0x0101 { h = Some(val); }
    }
    Some((w?, h?))
}

fn ico_dims(raw: &[u8]) -> Option<(u32, u32)> {
    let count = u16::from_le_bytes([raw[4], raw[5]]) as usize;
    let mut mw = 0u32;
    let mut mh = 0u32;
    for i in 0..count {
        let off = 6 + i * 16;
        if off + 2 > raw.len() { break; }
        let w = if raw[off] == 0 { 256 } else { raw[off] as u32 };
        let h = if raw[off + 1] == 0 { 256 } else { raw[off + 1] as u32 };
        mw = mw.max(w);
        mh = mh.max(h);
    }
    if mw == 0 || mh == 0 { None } else { Some((mw, mh)) }
}

fn bmff_tkhd_dims(raw: &[u8]) -> Option<(u32, u32)> {
    let mut best: Option<(u32, u32)> = None;
    bmff_tkhd_scan(raw, 0, raw.len() as u64, 0, &mut best);
    best
}

fn bmff_tkhd_scan(
    raw: &[u8],
    start: u64,
    end: u64,
    depth: u32,
    best: &mut Option<(u32, u32)>,
) {
    if depth > 8 {
        return;
    }
    let mut off = start;
    while off + 8 <= end {
        let size32 = match raw.get(off as usize..off as usize + 4) {
            Some(s) => u32::from_be_bytes([s[0], s[1], s[2], s[3]]),
            None => return,
        };
        let ftype = match raw.get(off as usize + 4..off as usize + 8) {
            Some(s) => s,
            None => return,
        };
        let (hdr, size) = if size32 == 1 {
            let ls = match raw.get(off as usize + 8..off as usize + 16) {
                Some(s) => u64::from_be_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]),
                None => return,
            };
            (16u64, ls)
        } else if size32 == 0 {
            (8u64, end - off)
        } else {
            (8u64, size32 as u64)
        };
        if size < hdr || off + size > end {
            return;
        }
        let box_end = off + size;
        let payload = off + hdr;

        if ftype == b"tkhd" && payload < box_end {
            let ver = raw[payload as usize];
            let woff = if ver == 1 { payload + 88 } else { payload + 76 };
            if woff + 8 <= box_end {
                let wf = u32::from_be_bytes([
                    raw[woff as usize],
                    raw[woff as usize + 1],
                    raw[woff as usize + 2],
                    raw[woff as usize + 3],
                ]);
                let hf = u32::from_be_bytes([
                    raw[woff as usize + 4],
                    raw[woff as usize + 5],
                    raw[woff as usize + 6],
                    raw[woff as usize + 7],
                ]);
                let (w, h) = (wf >> 16, hf >> 16);
                if w > 0 && h > 0 {
                    let area = (w as u64) * (h as u64);
                    let better = match *best {
                        Some((bw, bh)) => (bw as u64) * (bh as u64) < area,
                        None => true,
                    };
                    if better {
                        *best = Some((w, h));
                    }
                }
            }
        }

        if is_bmff_container(ftype) {
            let child = if ftype == b"meta" { payload + 4 } else { payload };
            if child <= box_end {
                bmff_tkhd_scan(raw, child, box_end, depth + 1, best);
            }
        }
        off = box_end;
    }
}

fn is_bmff_container(ftype: &[u8]) -> bool {
    const CONTAINERS: [&[u8]; 18] = [
        b"moov", b"trak", b"mdia", b"minf", b"stbl", b"edts", b"udta",
        b"moof", b"traf", b"mfra", b"meta", b"iprp", b"ipco", b"dinf",
        b"wave", b"ilst", b"ipro", b"keys",
    ];
    CONTAINERS.contains(&ftype)
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
        let data_end = i.checked_add(8)?.checked_add(len)?;
        let chunk_end = data_end.checked_add(4)?;
        if chunk_end > raw.len() {
            return None;
        }
        if &raw[i + 4..i + 8] == b"eXIf" {
            return Some(raw[i + 8..data_end].to_vec());
        }
        i = chunk_end;
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
        let start = i.checked_add(8)?;
        let end = start.checked_add(size)?;
        if end > raw.len() {
            return None;
        }
        if fourcc == b"EXIF" {
            return Some(raw[start..end].to_vec());
        }
        i = end + (size & 1);
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
    if (raw.len() >= 8 && &raw[4..8] == b"JXL ") || raw.starts_with(&[0xFF, 0x0A]) {
        return jxl_exif(raw);
    }
    None
}

// ── decode_image ──────────────────────────────────────────────

pub(crate) fn is_jxl(raw: &[u8]) -> bool {
    (raw.len() >= 8 && &raw[4..8] == b"JXL ") || raw.starts_with(&[0xFF, 0x0A])
}

pub(crate) struct JxlPrepared {
    image: jxl_oxide::JxlImage,
}

impl JxlPrepared {
    pub(crate) fn prepare(raw: &[u8]) -> Option<Self> {
        if !is_jxl(raw) {
            return None;
        }

        use jxl_oxide::{AllocTracker, EnumColourEncoding, JxlImage, RenderingIntent};

        let mut image = JxlImage::builder()
            .alloc_tracker(AllocTracker::with_limit(runtime_limits().max_alloc as usize))
            .read(std::io::Cursor::new(raw))
            .ok()?;

        if image.width() > 32768 || image.height() > 32768 {
            return None;
        }

        if image.pixel_format().has_black() {
            image.request_color_encoding(EnumColourEncoding::srgb(RenderingIntent::Relative));
        }

        Some(Self { image })
    }

    pub(crate) fn dims(&self) -> (u32, u32) {
        (self.image.width(), self.image.height())
    }

    pub(crate) fn decode(self) -> Result<(image::DynamicImage, Option<Vec<u8>>, Option<Vec<u8>>)> {
        use jxl_oxide::AuxBoxData;

        let exif = match self.image.aux_boxes().first_exif() {
            Ok(AuxBoxData::Data(r)) => {
                let off = r.tiff_header_offset() as usize;
                r.payload().get(off..).map(|b| b.to_vec())
            }
            _ => None,
        };

        let icc = Some(self.image.rendered_icc());

        let render = self
            .image
            .render_frame(0)
            .map_err(|e| anyhow::anyhow!("JXL render failed: {}", e))?;

        let mut stream = render.stream();
        let w = stream.width();
        let h = stream.height();
        let c = stream.channels() as usize;
        let count = (w as usize) * (h as usize) * c;

        let grayscale = self.image.pixel_format().is_grayscale();
        let alpha = self.image.pixel_format().has_alpha();
        let bit_depth = self.image.image_header().metadata.bit_depth;
        let is_float = matches!(
            bit_depth,
            jxl_oxide::image::BitDepth::FloatSample { .. }
                | jxl_oxide::image::BitDepth::IntegerSample { bits_per_sample: 17.. }
        );
        let need_16bit = bit_depth.bits_per_sample() > 8;

        let img = if is_float && !grayscale {
            let mut buf = vec![0f32; count];
            stream.write_to_buffer(&mut buf);
            if alpha {
                image::DynamicImage::ImageRgba32F(
                    image::Rgba32FImage::from_raw(w, h, buf)
                        .ok_or_else(|| anyhow::anyhow!("JXL buffer size mismatch"))?,
                )
            } else {
                image::DynamicImage::ImageRgb32F(
                    image::Rgb32FImage::from_raw(w, h, buf)
                        .ok_or_else(|| anyhow::anyhow!("JXL buffer size mismatch"))?,
                )
            }
        } else if need_16bit {
            let mut buf = vec![0u16; count];
            stream.write_to_buffer(&mut buf);
            match (grayscale, alpha) {
                (false, false) => image::DynamicImage::ImageRgb16(
                    image::ImageBuffer::<image::Rgb<u16>, Vec<u16>>::from_raw(w, h, buf)
                        .ok_or_else(|| anyhow::anyhow!("JXL buffer size mismatch"))?,
                ),
                (false, true) => image::DynamicImage::ImageRgba16(
                    image::ImageBuffer::<image::Rgba<u16>, Vec<u16>>::from_raw(w, h, buf)
                        .ok_or_else(|| anyhow::anyhow!("JXL buffer size mismatch"))?,
                ),
                (true, false) => image::DynamicImage::ImageLuma16(
                    image::ImageBuffer::<image::Luma<u16>, Vec<u16>>::from_raw(w, h, buf)
                        .ok_or_else(|| anyhow::anyhow!("JXL buffer size mismatch"))?,
                ),
                (true, true) => image::DynamicImage::ImageLumaA16(
                    image::ImageBuffer::<image::LumaA<u16>, Vec<u16>>::from_raw(w, h, buf)
                        .ok_or_else(|| anyhow::anyhow!("JXL buffer size mismatch"))?,
                ),
            }
        } else {
            let mut buf = vec![0u8; count];
            stream.write_to_buffer(&mut buf);
            match (grayscale, alpha) {
                (false, false) => image::DynamicImage::ImageRgb8(
                    image::RgbImage::from_raw(w, h, buf)
                        .ok_or_else(|| anyhow::anyhow!("JXL buffer size mismatch"))?,
                ),
                (false, true) => image::DynamicImage::ImageRgba8(
                    image::RgbaImage::from_raw(w, h, buf)
                        .ok_or_else(|| anyhow::anyhow!("JXL buffer size mismatch"))?,
                ),
                (true, false) => image::DynamicImage::ImageLuma8(
                    image::GrayImage::from_raw(w, h, buf)
                        .ok_or_else(|| anyhow::anyhow!("JXL buffer size mismatch"))?,
                ),
                (true, true) => image::DynamicImage::ImageLumaA8(
                    image::GrayAlphaImage::from_raw(w, h, buf)
                        .ok_or_else(|| anyhow::anyhow!("JXL buffer size mismatch"))?,
                ),
            }
        };

        Ok((img, icc, exif))
    }
}

pub(crate) fn decode_image(
    raw: &[u8],
    path: Option<&Path>,
    target: Option<(u32, u32)>,
    svg: Option<&usvg::Tree>,
) -> Result<(image::DynamicImage, Option<Vec<u8>>, Option<Vec<u8>>)> {
    if let Some(tree) = svg {
        let (tw, th) = match target {
            Some(d) => d,
            None => {
                let size = tree.size();
                (size.width().ceil() as u32, size.height().ceil() as u32)
            }
        };
        let img = decode_svg(tree, tw, th).map_err(anyhow::Error::msg)?;
        return Ok((img, None, None));
    }

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

fn svg_font_db() -> Arc<fontdb::Database> {
    static DB: OnceLock<Arc<fontdb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        Arc::new(db)
    })
    .clone()
}

fn svg_options(path: Option<&Path>) -> usvg::Options<'static> {
    let mut opts = usvg::Options::default();
    opts.fontdb = svg_font_db();
    opts.resources_dir = path.and_then(|p| p.parent().map(Path::to_path_buf));
    opts
}

pub(crate) fn looks_like_svg(raw: &[u8]) -> bool {
    if raw.starts_with(&[0x1f, 0x8b]) {
        return true;
    }
    let raw = raw.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(raw);
    let first = raw.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(raw.len());
    let head = &raw[first..];
    head.starts_with(b"<svg") || head.starts_with(b"<?xml")
}

pub(crate) fn raster_need(w: u32, h: u32) -> u64 {
    (w as u64)
        .saturating_mul(h as u64)
        .saturating_mul(4)
        .clamp(1, runtime_limits().max_alloc)
}

fn estimate_image_peak(w: u32, h: u32, raw_len: usize) -> Option<u64> {
    let px = (w as u64).checked_mul(h as u64)?;
    px.checked_mul(12)?.checked_add(raw_len as u64)
}

pub(crate) const MAX_SVG_DEPTH: usize = 32;

fn collect_raster_images(group: &usvg::Group, depth: usize, out: &mut Vec<(u32, u32, usize, bool)>) -> Option<()> {
    if depth > MAX_SVG_DEPTH {
        return None;
    }
    for node in group.children() {
        collect_node_raster_images(node, depth + 1, out)?;
    }
    Some(())
}

fn collect_node_raster_images(node: &usvg::Node, depth: usize, out: &mut Vec<(u32, u32, usize, bool)>) -> Option<()> {
    if depth > MAX_SVG_DEPTH {
        return None;
    }
    if let usvg::Node::Image(img) = node {
        let w = img.size().width().ceil().max(1.0) as u32;
        let h = img.size().height().ceil().max(1.0) as u32;
        match img.kind() {
            usvg::ImageKind::JPEG(raw) => {
                out.push((w, h, raw.len(), true));
            }
            usvg::ImageKind::PNG(raw)
            | usvg::ImageKind::GIF(raw)
            | usvg::ImageKind::WEBP(raw) => {
                out.push((w, h, raw.len(), false));
            }
            usvg::ImageKind::SVG(tree) => {
                collect_raster_images(tree.root(), depth + 1, out)?;
            }
        }
    }
    if let usvg::Node::Group(g) = node {
        if let Some(clip) = g.clip_path() {
            collect_raster_images(clip.root(), depth + 1, out)?;
            if let Some(sub) = clip.clip_path() {
                collect_raster_images(sub.root(), depth + 1, out)?;
            }
        }
        if let Some(mask) = g.mask() {
            collect_raster_images(mask.root(), depth + 1, out)?;
        }
        collect_raster_images(g, depth + 1, out)?;
    }
    let mut ok = true;
    node.subroots(|sub| {
        if ok {
            if collect_raster_images(sub, depth + 1, out).is_none() {
                ok = false;
            }
        }
    });
    if ok { Some(()) } else { None }
}

pub(crate) fn vector_peak_cap() -> u64 {
    runtime_limits().max_alloc.saturating_mul(3) / 4
}

pub(crate) fn vector_peak_estimate(tree: &usvg::Tree, grayscale: bool) -> Option<u64> {
    let mut images = Vec::new();
    collect_raster_images(tree.root(), 0, &mut images)?;
    let mut sum = 0u64;
    for (w, h, raw_len, is_jpeg) in images {
        let est = if is_jpeg && !grayscale {
            u64::try_from(raw_len).ok()?
        } else {
            estimate_image_peak(w, h, raw_len)?
        };
        sum = sum.checked_add(est)?;
    }
    Some(sum)
}

pub(crate) fn max_raster_image_peak(tree: &usvg::Tree) -> Option<u64> {
    let mut images = Vec::new();
    collect_raster_images(tree.root(), 0, &mut images)?;
    let mut max = 0u64;
    for (w, h, raw_len, _) in images {
        let est = estimate_image_peak(w, h, raw_len)?;
        max = max.max(est);
    }
    Some(max)
}

pub(crate) struct ParsedSvg {
    pub tree: usvg::Tree,
    pub width: u32,
    pub height: u32,
}

fn find_subslice(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    hay.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

fn skip_tag(raw: &[u8], mut i: usize) -> Option<usize> {
    let n = raw.len();
    let mut quote: Option<u8> = None;
    while i < n {
        let b = raw[i];
        if let Some(q) = quote {
            if b == q {
                quote = None;
            }
        } else if b == b'"' || b == b'\'' {
            quote = Some(b);
        } else if b == b'>' {
            return Some(i + 1);
        }
        i += 1;
    }
    None
}

fn skip_open_tag(raw: &[u8], from: usize) -> Option<(usize, bool)> {
    let close = skip_tag(raw, from)?;
    let self_closing = close > from + 1 && raw[close - 2] == b'/';
    Some((close, self_closing))
}

fn svg_xml_max_depth(raw: &[u8]) -> Option<usize> {
    let mut depth = 0usize;
    let mut max = 0usize;
    let mut i = 0usize;
    let n = raw.len();
    while i < n {
        if raw[i] != b'<' {
            i += 1;
            continue;
        }
        if i + 1 >= n {
            return None;
        }
        match raw[i + 1] {
            b'?' => {
                let close = find_subslice(raw, i + 2, b"?>")?;
                i = close + 2;
            }
            b'!' => {
                if raw[i..].starts_with(b"<!--") {
                    let close = find_subslice(raw, i + 4, b"-->")?;
                    i = close + 3;
                } else if raw[i..].starts_with(b"<![CDATA[") {
                    let close = find_subslice(raw, i + 9, b"]]>")?;
                    i = close + 3;
                } else {
                    i = skip_tag(raw, i + 2)?;
                }
            }
            b'/' => {
                i = skip_tag(raw, i + 2)?;
                depth = depth.checked_sub(1)?;
            }
            _ => {
                let (close, self_closing) = skip_open_tag(raw, i + 1)?;
                if self_closing {
                    max = max.max(depth.saturating_add(1));
                } else {
                    depth = depth.checked_add(1)?;
                    max = max.max(depth);
                }
                i = close;
            }
        }
    }
    if depth != 0 {
        return None;
    }
    Some(max)
}

pub(crate) fn parse_svg(raw: &[u8], path: Option<&Path>) -> anyhow::Result<ParsedSvg> {
    let mut gz_buf = Vec::new();
    let svg_bytes: &[u8] = if raw.starts_with(&[0x1f, 0x8b]) {
        GzDecoder::new(raw)
            .read_to_end(&mut gz_buf)
            .map_err(|_| anyhow::anyhow!("SVG decompression failed"))?;
        &gz_buf
    } else {
        raw
    };
    if let Some(depth) = svg_xml_max_depth(svg_bytes) {
        if depth > MAX_SVG_DEPTH {
            anyhow::bail!("SVG nesting too deep (limit {})", MAX_SVG_DEPTH);
        }
    }
    let tree = usvg::Tree::from_data(svg_bytes, &svg_options(path))
        .map_err(|_| anyhow::anyhow!("SVG parse failed"))?;
    let size = tree.size();
    if size.width() <= 0.0 || size.height() <= 0.0 {
        anyhow::bail!("SVG has invalid size");
    }
    Ok(ParsedSvg {
        tree,
        width: size.width().ceil() as u32,
        height: size.height().ceil() as u32,
    })
}

pub(crate) fn probe_svg_dims(raw: &[u8]) -> Option<(u32, u32)> {
    parse_svg(raw, None).ok().map(|s| (s.width, s.height))
}

pub fn decode_svg(
    tree: &usvg::Tree,
    target_w: u32,
    target_h: u32,
) -> Result<image::DynamicImage, String> {
    let size = tree.size();
    let src_w = size.width();
    let src_h = size.height();
    if src_w <= 0.0 || src_h <= 0.0 {
        return Err("SVG has invalid size".to_string());
    }

    let tw = target_w.max(1);
    let th = target_h.max(1);
    let worst = (tw as u64).saturating_mul(th as u64).saturating_mul(4);
    if worst > runtime_limits().max_alloc {
        return Err("SVG target size too large".to_string());
    }
    if let Some(peak) = max_raster_image_peak(tree) {
        if peak > vector_peak_cap() {
            return Err(format!("embedded SVG image too large: {} bytes peak", peak));
        }
    } else {
        return Err("embedded SVG image size overflow".to_string());
    }
    let mut pixmap = tiny_skia::Pixmap::new(tw, th)
        .ok_or_else(|| "SVG target size too large".to_string())?;
    let transform = tiny_skia::Transform::from_scale(tw as f32 / src_w, th as f32 / src_h);
    resvg::render(tree, transform, &mut pixmap.as_mut());

    unpremultiply_rgba(pixmap.data_mut());
    let rgba = pixmap.data().to_vec();
    let img = image::RgbaImage::from_raw(tw, th, rgba)
        .ok_or_else(|| "failed to build SVG raster buffer".to_string())?;

    Ok(image::DynamicImage::ImageRgba8(img))
}

fn unpremultiply_rgba(buf: &mut [u8]) {
    for px in buf.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a == 0 {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
        } else {
            for c in 0..3 {
                let v = px[c] as u32;
                px[c] = ((v * 255 + a / 2) / a) as u8;
            }
        }
    }
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

    let img = match (has_alpha, bpp) {
        (false, 3) => image::RgbImage::from_raw(width, height, packed).map(image::DynamicImage::ImageRgb8),
        (true, 4) => image::RgbaImage::from_raw(width, height, packed).map(image::DynamicImage::ImageRgba8),
        (false, 6) | (true, 8) => {
            let channels = if has_alpha { 4 } else { 3 };
            let mut down = Vec::with_capacity(width as usize * height as usize * channels);
            for px in packed.chunks_exact(bpp) {
                for c in 0..channels {
                    down.push(px[c * 2 + 1]);
                }
            }
            if has_alpha {
                image::RgbaImage::from_raw(width, height, down).map(image::DynamicImage::ImageRgba8)
            } else {
                image::RgbImage::from_raw(width, height, down).map(image::DynamicImage::ImageRgb8)
            }
        }
        _ => anyhow::bail!("{}", msg().err_heif_decode),
    };

    Ok(img.context(msg().err_heif_decode)?)
}

// ── RAW helpers ───────────────────────────────────────────────

pub(crate) fn is_raw_bytes(raw: &[u8]) -> bool {
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
