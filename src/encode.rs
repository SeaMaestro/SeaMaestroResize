use anyhow::{Context, Result};
use std::path::Path;
use libavif_sys::*;
use libjxl_sys::*;

use crate::msg;
use crate::Config;
use crate::ImageFormat;
use crate::metadata::{encode_tiff, normalize_exif, webp_embed_metadata};
use crate::pdf::single_page_pdf;

fn write_jpeg_exif(comp: &mut mozjpeg::compress::CompressStarted<Vec<u8>>, blob: &[u8]) {
    let mut data = Vec::with_capacity(blob.len() + 6);
    data.extend_from_slice(b"Exif\0\0");
    data.extend_from_slice(blob);
    for chunk in data.chunks(65527) {
        comp.write_marker(mozjpeg::Marker::APP(1), chunk);
    }
}

fn png_embed_exif(png: Vec<u8>, exif: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(png.len() + exif.len() + 12);
    out.extend_from_slice(&png[..33]);
    out.extend_from_slice(&(exif.len() as u32).to_be_bytes());
    out.extend_from_slice(b"eXIf");
    out.extend_from_slice(exif);
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(b"eXIf");
    hasher.update(exif);
    out.extend_from_slice(&hasher.finalize().to_be_bytes());
    out.extend_from_slice(&png[33..]);
    out
}


use image::imageops::FilterType;

use std::sync::atomic::{AtomicUsize, Ordering};

static AVIF_THREADS: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn set_avif_threads(total_files: usize) {
    let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let threads = if total_files <= 1 {
        cpus.min(8)
    } else {
        1
    };
    AVIF_THREADS.store(threads, Ordering::Relaxed);
}

pub(crate) fn avif_threads() -> usize {
    let t = AVIF_THREADS.load(Ordering::Relaxed);
    if t == 0 {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(8)
    } else {
        t
    }
}

pub(crate) fn encode_to_vec(img: &image::DynamicImage, config: &Config, icc: Option<&[u8]>, exif: Option<&[u8]>) -> Result<Vec<u8>> {
    let exif = exif.and_then(|blob| normalize_exif(blob.to_vec(), img.width(), img.height()));
    let exif = exif.as_deref();
    match config.format {
        ImageFormat::Jpeg => encode_jpeg_to_vec(img, config.quality, config.progressive, icc, exif),
        ImageFormat::WebP => encode_webp_to_vec(img, config.quality, config.lossless, icc, exif),
        ImageFormat::Avif => encode_avif_to_vec(img, config.quality, icc, exif),
        ImageFormat::Png => encode_png_to_vec(img, icc, exif),
        ImageFormat::Ico => encode_ico_to_vec(img),
        ImageFormat::Tiff => encode_tiff_to_vec(img, icc),
        ImageFormat::Qoi => encode_qoi_to_vec(img),
        ImageFormat::Bmp => encode_bmp_to_vec(img),
        ImageFormat::Gif => encode_gif_to_vec(img),
        ImageFormat::Jxl => encode_jxl_to_vec(img, config.quality, config.lossless, icc, exif),
        ImageFormat::Pdf => single_page_pdf(img, config),
    }
}pub(crate) fn encode_bmp(img: &image::DynamicImage, out: &Path) -> Result<()> {
    img.save_with_format(out, image::ImageFormat::Bmp)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_mozjpeg(
    color_space: mozjpeg::ColorSpace,
    w: usize,
    h: usize,
    raw: &[u8],
    channels: usize,
    quality: u8,
    progressive: bool,
    chroma: Option<((u8, u8), (u8, u8))>,
    icc: Option<&[u8]>,
    exif: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let mut comp = mozjpeg::Compress::new(color_space);
    comp.set_size(w, h);
    if progressive {
        comp.set_progressive_mode();
    }
    comp.set_quality(quality.clamp(1, 100) as f32);
    if let Some((cb, cr)) = chroma {
        comp.set_chroma_sampling_pixel_sizes(cb, cr);
    }
    let mut comp = comp.start_compress(Vec::new())?;
    if let Some(profile) = icc {
        if !profile.is_empty() {
            comp.write_icc_profile(profile);
        }
    }
    if let Some(blob) = exif {
        write_jpeg_exif(&mut comp, blob);
    }
    for line in 0..h {
        let start = line * w * channels;
        let end = (line + 1) * w * channels;
        comp.write_scanlines(&raw[start..end])?;
    }
    Ok(comp.finish()?)
}

fn encode_jpeg_to_vec(img: &image::DynamicImage, quality: u8, progressive: bool, icc: Option<&[u8]>, exif: Option<&[u8]>) -> Result<Vec<u8>> {
    if let Some(gray) = img.as_luma8() {
        return encode_jpeg_gray(gray, quality, progressive, icc, exif);
    }
    let rgb = img.to_rgb8();
    let (w, h) = (rgb.width() as usize, rgb.height() as usize);
    let raw = rgb.into_raw();
    run_mozjpeg(mozjpeg::ColorSpace::JCS_RGB, w, h, &raw, 3, quality, progressive, None, icc, exif)
}

fn encode_jpeg_gray(gray: &image::GrayImage, quality: u8, progressive: bool, icc: Option<&[u8]>, exif: Option<&[u8]>) -> Result<Vec<u8>> {
    let (w, h) = (gray.width() as usize, gray.height() as usize);
    run_mozjpeg(mozjpeg::ColorSpace::JCS_GRAYSCALE, w, h, gray.as_raw(), 1, quality, progressive, None, icc, exif)
}

fn webp_encode(enc: webp::Encoder<'_>, quality: f32, lossless: bool) -> Result<webp::WebPMemory> {
    if lossless {
        Ok(enc.encode_lossless())
    } else {
        enc.encode_simple(false, quality)
            .map_err(|e| anyhow::anyhow!("{}", msg().err_webp.replacen("{:?}", &format!("{:?}", e), 1)))
    }
}

fn encode_webp_to_vec(img: &image::DynamicImage, quality: u8, lossless: bool, icc: Option<&[u8]>, exif: Option<&[u8]>) -> Result<Vec<u8>> {
    let (w, h) = (img.width(), img.height());
    let q = quality.clamp(1, 100) as f32;
    let data = if let Some(rgb) = img.as_rgb8() {
        webp_encode(webp::Encoder::from_rgb(rgb.as_raw(), w, h), q, lossless)?
    } else if img.color().has_alpha() {
        let owned;
        let rgba_img: &image::RgbaImage = if let Some(rgba) = img.as_rgba8() {
            rgba
        } else {
            owned = img.to_rgba8();
            &owned
        };
        webp_encode(webp::Encoder::from_rgba(rgba_img, w, h), q, lossless)?
    } else {
        let rgb = img.to_rgb8();
        webp_encode(webp::Encoder::from_rgb(&rgb, w, h), q, lossless)?
    };
    let bytes = data.to_vec();
    let has_icc = icc.is_some_and(|p| !p.is_empty());
    let has_exif = exif.is_some_and(|e| !e.is_empty());
    if has_icc || has_exif {
        webp_embed_metadata(bytes, icc, exif, w, h, img.color().has_alpha())
    } else {
        Ok(bytes)
    }
}struct AvifEncoderGuard(*mut avifEncoder);

impl Drop for AvifEncoderGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { avifEncoderDestroy(self.0) };
        }
    }
}

struct AvifImageGuard(*mut avifImage);

impl Drop for AvifImageGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { avifImageDestroy(self.0) };
        }
    }
}

fn encode_avif_to_vec(img: &image::DynamicImage, quality: u8, icc: Option<&[u8]>, exif: Option<&[u8]>) -> Result<Vec<u8>> {
    let q = quality.clamp(1, 100) as i32;
    if img.color().has_alpha() {
        let rgba = img.to_rgba8();
        let (w, h) = (rgba.width(), rgba.height());
        encode_avif_raw(rgba.as_raw(), w, h, avifRGBFormat_AVIF_RGB_FORMAT_RGBA, 4, q, icc, exif)
    } else {
        let rgb = img.to_rgb8();
        let (w, h) = (rgb.width(), rgb.height());
        encode_avif_raw(rgb.as_raw(), w, h, avifRGBFormat_AVIF_RGB_FORMAT_RGB, 3, q, icc, exif)
    }
}

extern "C" {
    fn svt_av1_set_log_callback(
        cb: Option<unsafe extern "C" fn(*mut libc::c_void, libc::c_int, *const libc::c_char, *const libc::c_char, *mut libc::c_char)>,
        ctx: *mut libc::c_void,
    );
}

unsafe extern "C" fn noop_svt_log(
    _ctx: *mut libc::c_void,
    _level: libc::c_int,
    _tag: *const libc::c_char,
    _fmt: *const libc::c_char,
    _args: *mut libc::c_char,
) {
}

#[allow(clippy::too_many_arguments)]
fn encode_avif_raw(
    pixels: &[u8],
    w: u32,
    h: u32,
    format: avifRGBFormat,
    channels: u32,
    quality: i32,
    icc: Option<&[u8]>,
    exif: Option<&[u8]>,
) -> Result<Vec<u8>> {
    unsafe {
        svt_av1_set_log_callback(Some(noop_svt_log), std::ptr::null_mut());
        let encoder = avifEncoderCreate();
        if encoder.is_null() {
            anyhow::bail!("{}", msg().err_avif.replacen("{}", "avifEncoderCreate returned NULL", 1));
        }
        let encoder = AvifEncoderGuard(encoder);
        (*encoder.0).codecChoice = avifCodecChoice_AVIF_CODEC_CHOICE_SVT;
        (*encoder.0).speed = 8;
        (*encoder.0).quality = quality;
        (*encoder.0).qualityAlpha = quality;
        (*encoder.0).maxThreads = avif_threads() as i32;

        let image = avifImageCreate(w, h, 8, avifPixelFormat_AVIF_PIXEL_FORMAT_YUV420);
        if image.is_null() {
            anyhow::bail!("{}", msg().err_avif.replacen("{}", "avifImageCreate returned NULL", 1));
        }
        let image = AvifImageGuard(image);
        (*image.0).yuvRange = avifRange_AVIF_RANGE_FULL;
        (*image.0).colorPrimaries = AVIF_COLOR_PRIMARIES_BT709 as u16;
        (*image.0).transferCharacteristics = AVIF_TRANSFER_CHARACTERISTICS_SRGB as u16;
        (*image.0).matrixCoefficients = AVIF_MATRIX_COEFFICIENTS_BT601 as u16;

        if let Some(profile) = icc.filter(|p| !p.is_empty()) {
            let res = avifImageSetProfileICC(image.0, profile.as_ptr(), profile.len());
            if res != avifResult_AVIF_RESULT_OK {
                anyhow::bail!("{}", msg().err_avif.replacen("{}", &format!("avifImageSetProfileICC: {}", res), 1));
            }
        }
        if let Some(blob) = exif.filter(|e| !e.is_empty()) {
            let res = avifImageSetMetadataExif(image.0, blob.as_ptr(), blob.len());
            if res != avifResult_AVIF_RESULT_OK {
                anyhow::bail!("{}", msg().err_avif.replacen("{}", &format!("avifImageSetMetadataExif: {}", res), 1));
            }
        }

        let mut rgb = std::mem::zeroed::<avifRGBImage>();
        avifRGBImageSetDefaults(&mut rgb, image.0);
        rgb.format = format;
        rgb.depth = 8;
        rgb.pixels = pixels.as_ptr().cast_mut();
        rgb.rowBytes = w * channels;

        let res = avifImageRGBToYUV(image.0, &rgb);
        if res != avifResult_AVIF_RESULT_OK {
            anyhow::bail!("{}", msg().err_avif.replacen("{}", &format!("avifImageRGBToYUV: {}", res), 1));
        }

        let res = avifEncoderAddImage(encoder.0, image.0, 1, avifAddImageFlag_AVIF_ADD_IMAGE_FLAG_SINGLE as u32);
        if res != avifResult_AVIF_RESULT_OK {
            anyhow::bail!("{}", msg().err_avif.replacen("{}", &format!("avifEncoderAddImage: {}", res), 1));
        }

        let mut output = std::mem::zeroed::<avifRWData>();
        let res = avifEncoderFinish(encoder.0, &mut output);
        let result = if res == avifResult_AVIF_RESULT_OK {
            Ok(std::slice::from_raw_parts(output.data, output.size).to_vec())
        } else {
            Err(anyhow::anyhow!("{}", msg().err_avif.replacen("{}", &format!("avifEncoderFinish: {}", res), 1)))
        };
        avifRWDataFree(&mut output);
        result
    }
}

fn encode_png_to_vec(img: &image::DynamicImage, icc: Option<&[u8]>, exif: Option<&[u8]>) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut encoder = image::codecs::png::PngEncoder::new(&mut buf);
    if let Some(profile) = icc {
        if !profile.is_empty() {
            image::ImageEncoder::set_icc_profile(&mut encoder, profile.to_vec())?;
        }
    }
    img.write_with_encoder(encoder).context(msg().err_png)?;
    let optimized = oxipng::optimize_from_memory(&buf, &oxipng::Options::from_preset(1))
        .map_err(|e| anyhow::anyhow!("{}", msg().err_oxipng.replacen("{}", &e.to_string(), 1)))?;
    if let Some(blob) = exif.filter(|e| !e.is_empty()) {
        return Ok(png_embed_exif(optimized, blob));
    }
    Ok(optimized)
}fn encode_ico_to_vec(img: &image::DynamicImage) -> Result<Vec<u8>> {
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    let sizes = &[16u32, 32, 48, 64, 128, 256];
    let mut ico_dir = ico::IconDir::new(ico::ResourceType::Icon);
    for &size in sizes {
        if size > w || size > h { continue; }
        let resized = image::imageops::resize(&rgba, size, size, FilterType::Lanczos3);
        let image = ico::IconImage::from_rgba_data(size, size, resized.into_raw());
        ico_dir.add_entry(ico::IconDirEntry::encode(&image).context(msg().err_ico_entry)?);
    }
    if ico_dir.entries().is_empty() {
        let image = ico::IconImage::from_rgba_data(w, h, rgba.into_raw());
        ico_dir.add_entry(ico::IconDirEntry::encode(&image).context(msg().err_ico_entry)?);
    }
    let mut buf = std::io::Cursor::new(Vec::new());
    ico_dir.write(&mut buf).context(msg().err_ico_write)?;
    Ok(buf.into_inner())
}

fn encode_tiff_to_vec(img: &image::DynamicImage, icc: Option<&[u8]>) -> Result<Vec<u8>> {
    if let Some(gray) = img.as_luma8() {
        return encode_tiff(gray.width(), gray.height(), gray.as_raw(), 1, icc);
    }
    if let Some(rgb) = img.as_rgb8() {
        return encode_tiff(rgb.width(), rgb.height(), rgb.as_raw(), 3, icc);
    }
    if let Some(rgba) = img.as_rgba8() {
        return encode_tiff(rgba.width(), rgba.height(), rgba.as_raw(), 4, icc);
    }
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Tiff)?;
    Ok(buf.into_inner())
}

fn encode_qoi_to_vec(img: &image::DynamicImage) -> Result<Vec<u8>> {
    let rgba = img.to_rgba8();
    let w = rgba.width();
    let h = rgba.height();
    qoi::encode_to_vec(rgba.into_raw(), w, h).context(msg().err_qoi)
}

fn encode_bmp_to_vec(img: &image::DynamicImage) -> Result<Vec<u8>> {
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Bmp)?;
    Ok(buf.into_inner())
}

fn encode_gif_to_vec(img: &image::DynamicImage) -> Result<Vec<u8>> {
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Gif)?;
    Ok(buf.into_inner())
}

struct JxlEncoderGuard(*mut JxlEncoder);

impl Drop for JxlEncoderGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { JxlEncoderDestroy(self.0) };
        }
    }
}

struct JxlRunnerGuard(*mut libc::c_void);

impl Drop for JxlRunnerGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { JxlThreadParallelRunnerDestroy(self.0) };
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn encode_jxl_raw(raw: &[u8], w: u32, h: u32, channels: u32, quality: u8, lossless: bool, icc: Option<&[u8]>, exif: Option<&[u8]>) -> Result<Vec<u8>> {
    unsafe {
        let enc_ptr = JxlEncoderCreate(std::ptr::null());
        if enc_ptr.is_null() {
            anyhow::bail!("JXL encode failed: JxlEncoderCreate returned NULL");
        }
        let runner_ptr = JxlThreadParallelRunnerCreate(std::ptr::null(), avif_threads());
        if runner_ptr.is_null() {
            JxlEncoderDestroy(enc_ptr);
            anyhow::bail!("JXL encode failed: JxlThreadParallelRunnerCreate returned NULL");
        }
        let runner = JxlRunnerGuard(runner_ptr);
        let enc = JxlEncoderGuard(enc_ptr);

        let res = JxlEncoderSetParallelRunner(enc.0, Some(JxlThreadParallelRunner), runner.0);
        if res != JxlEncoderStatus_JXL_ENC_SUCCESS {
            anyhow::bail!("JXL encode failed: JxlEncoderSetParallelRunner: {}", res);
        }

        let mut info = std::mem::zeroed::<JxlBasicInfo>();
        JxlEncoderInitBasicInfo(&mut info);
        info.xsize = w;
        info.ysize = h;
        if lossless {
            info.uses_original_profile = JXL_TRUE as libc::c_int;
        }
        if channels == 1 {
            info.num_color_channels = 1;
        } else if channels == 4 {
            info.num_extra_channels = 1;
            info.alpha_bits = 8;
        }
        let res = JxlEncoderSetBasicInfo(enc.0, &info);
        if res != JxlEncoderStatus_JXL_ENC_SUCCESS {
            anyhow::bail!("JXL encode failed: JxlEncoderSetBasicInfo: {}", res);
        }

        if let Some(profile) = icc.filter(|p| !p.is_empty()) {
            let res = JxlEncoderSetICCProfile(enc.0, profile.as_ptr(), profile.len());
            if res != JxlEncoderStatus_JXL_ENC_SUCCESS {
                anyhow::bail!("JXL encode failed: JxlEncoderSetICCProfile: {}", res);
            }
        }

        let frame_settings = JxlEncoderFrameSettingsCreate(enc.0, std::ptr::null());
        if frame_settings.is_null() {
            anyhow::bail!("JXL encode failed: JxlEncoderFrameSettingsCreate returned NULL");
        }
        if lossless {
            let res = JxlEncoderSetFrameLossless(frame_settings, JXL_TRUE as libc::c_int);
            if res != JxlEncoderStatus_JXL_ENC_SUCCESS {
                anyhow::bail!("JXL encode failed: JxlEncoderSetFrameLossless: {}", res);
            }
        } else {
            let distance = JxlEncoderDistanceFromQuality(quality.clamp(1, 100) as f32);
            let res = JxlEncoderSetFrameDistance(frame_settings, distance);
            if res != JxlEncoderStatus_JXL_ENC_SUCCESS {
                anyhow::bail!("JXL encode failed: JxlEncoderSetFrameDistance: {}", res);
            }
        }

        if let Some(blob) = exif.filter(|e| !e.is_empty()) {
            let res = JxlEncoderUseBoxes(enc.0);
            if res != JxlEncoderStatus_JXL_ENC_SUCCESS {
                anyhow::bail!("JXL encode failed: JxlEncoderUseBoxes: {}", res);
            }
            let box_type: [libc::c_char; 4] = [b'E' as libc::c_char, b'x' as libc::c_char, b'i' as libc::c_char, b'f' as libc::c_char];
            let res = JxlEncoderAddBox(enc.0, &box_type, blob.as_ptr(), blob.len(), 0);
            if res != JxlEncoderStatus_JXL_ENC_SUCCESS {
                anyhow::bail!("JXL encode failed: JxlEncoderAddBox: {}", res);
            }
            JxlEncoderCloseBoxes(enc.0);
        }

        let pixel_format = JxlPixelFormat {
            num_channels: channels,
            data_type: JxlDataType_JXL_TYPE_UINT8,
            endianness: JxlEndianness_JXL_NATIVE_ENDIAN,
            align: 0,
        };
        let res = JxlEncoderAddImageFrame(frame_settings, &pixel_format, raw.as_ptr() as *const libc::c_void, raw.len());
        if res != JxlEncoderStatus_JXL_ENC_SUCCESS {
            anyhow::bail!("JXL encode failed: JxlEncoderAddImageFrame: {}", res);
        }

        JxlEncoderCloseInput(enc.0);

        let mut output: Vec<u8> = Vec::new();
        let mut chunk = 64 * 1024usize;
        loop {
            let offset = output.len();
            output.resize(offset + chunk, 0);
            let mut next_out = output.as_mut_ptr().add(offset);
            let mut avail_out = chunk;
            let res = JxlEncoderProcessOutput(enc.0, &mut next_out, &mut avail_out);
            let written = chunk - avail_out;
            output.truncate(offset + written);
            if res == JxlEncoderStatus_JXL_ENC_SUCCESS {
                break;
            }
            if res == JxlEncoderStatus_JXL_ENC_ERROR {
                anyhow::bail!("JXL encode failed: JxlEncoderProcessOutput: {}", res);
            }
            chunk = chunk.saturating_mul(2);
        }
        Ok(output)
    }
}

fn encode_jxl_to_vec(img: &image::DynamicImage, quality: u8, lossless: bool, icc: Option<&[u8]>, exif: Option<&[u8]>) -> Result<Vec<u8>> {
    if let Some(gray) = img.as_luma8() {
        return encode_jxl_raw(gray.as_raw(), gray.width(), gray.height(), 1, quality, lossless, icc, exif);
    }
    if let Some(rgb) = img.as_rgb8() {
        return encode_jxl_raw(rgb.as_raw(), rgb.width(), rgb.height(), 3, quality, lossless, icc, exif);
    }
    let owned;
    let rgba_img: &image::RgbaImage = if let Some(rgba) = img.as_rgba8() {
        rgba
    } else {
        owned = img.to_rgba8();
        &owned
    };
    encode_jxl_raw(rgba_img.as_raw(), rgba_img.width(), rgba_img.height(), 4, quality, lossless, icc, exif)
}

pub(crate) fn encode_pdf_jpeg(img: &image::DynamicImage, quality: u8) -> Result<Vec<u8>> {
    if let Some(gray) = img.as_luma8() {
        return encode_jpeg_gray(gray, quality, false, None, None);
    }
    let rgb = img.to_rgb8();
    let (w, h) = (rgb.width() as usize, rgb.height() as usize);
    let raw = rgb.into_raw();
    run_mozjpeg(mozjpeg::ColorSpace::JCS_RGB, w, h, &raw, 3, quality, false, Some(((2, 2), (2, 2))), None, None)
}
