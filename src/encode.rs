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
    out.extend_from_slice(png.get(..33).unwrap_or(&png));
    out.extend_from_slice(&(exif.len() as u32).to_be_bytes());
    out.extend_from_slice(b"eXIf");
    out.extend_from_slice(exif);
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(b"eXIf");
    hasher.update(exif);
    out.extend_from_slice(&hasher.finalize().to_be_bytes());
    out.extend_from_slice(png.get(33..).unwrap_or(&[]));
    out
}


use image::imageops::FilterType;

use std::sync::atomic::{AtomicUsize, Ordering};

static AVIF_THREADS: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn codec_threads_for(total_files: usize, workers: usize) -> usize {
    let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    if total_files <= 1 || workers <= 1 {
        cpus.min(8)
    } else {
        1
    }
}

pub(crate) fn set_avif_threads(total_files: usize, workers: usize) {
    AVIF_THREADS.store(codec_threads_for(total_files, workers), Ordering::Relaxed);
}

#[cfg(test)]
mod codec_threads {
    use super::codec_threads_for;

    #[test]
    fn a_serial_run_lets_the_codec_use_the_cores() {
        assert_eq!(
            codec_threads_for(10, 1),
            codec_threads_for(1, 1),
            "one file in flight must get the same thread budget as a single-file run"
        );
    }

    #[test]
    fn parallel_files_keep_the_codec_single_threaded() {
        assert_eq!(codec_threads_for(10, 4), 1);
        assert_eq!(codec_threads_for(2, 2), 1);
    }

    #[test]
    fn a_single_file_is_never_serialised() {
        assert_eq!(codec_threads_for(1, 8), codec_threads_for(1, 1));
        assert!(codec_threads_for(1, 8) >= 1);
    }
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

static SVT_LOG_INIT: std::sync::OnceLock<()> = std::sync::OnceLock::new();

pub(crate) fn avx2_available() -> bool {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        std::arch::is_x86_feature_detected!("avx2")
    }
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    {
        true
    }
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
    if !avx2_available() {
        anyhow::bail!("{}", msg().warn_avif_no_avx2);
    }
    unsafe {
        SVT_LOG_INIT.get_or_init(|| {
            svt_av1_set_log_callback(Some(noop_svt_log), std::ptr::null_mut());
        });
        let encoder = avifEncoderCreate();
        if encoder.is_null() {
            anyhow::bail!("{}", msg().err_avif.replacen("{}", "avifEncoderCreate returned NULL", 1));
        }
        let encoder = AvifEncoderGuard(encoder);
        (*encoder.0).codecChoice = avifCodecChoice_AVIF_CODEC_CHOICE_SVT;
        (*encoder.0).speed = 8;
        (*encoder.0).quality = quality;
        (*encoder.0).qualityAlpha = quality.max(90);
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

const PNG_FAST_PRESET_MIN_PX: u64 = 8_000_000;

fn png_preset_for(width: u32, height: u32) -> u8 {
    if width as u64 * height as u64 > PNG_FAST_PRESET_MIN_PX {
        0
    } else {
        1
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
    let preset = png_preset_for(img.width(), img.height());
    let optimized = oxipng::optimize_from_memory(&buf, &oxipng::Options::from_preset(preset))
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
            let mut payload = Vec::with_capacity(4 + blob.len());
            payload.extend_from_slice(&0u32.to_be_bytes());
            payload.extend_from_slice(blob);
            let res = JxlEncoderAddBox(enc.0, &box_type, payload.as_ptr(), payload.len(), 0);
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
            chunk = chunk.saturating_mul(2).min(16 * 1024 * 1024);
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

#[cfg(test)]
mod tests {
    use super::{avifCodecVersions, avx2_available, encode_avif_to_vec};
    use crate::decode::{decode_avif, is_avif};
    use image::{DynamicImage, Rgba, RgbaImage};

    fn sample_rgba(w: u32, h: u32) -> RgbaImage {
        let mut img = RgbaImage::new(w, h);
        for (x, _y, px) in img.enumerate_pixels_mut() {
            let alpha = if x < w / 2 { 0u8 } else { 255u8 };
            *px = Rgba([200, 30, 30, alpha]);
        }
        img
    }

    #[test]
    fn avif_round_trip_preserves_alpha() {
        let mut versions = [0 as libc::c_char; 256];
        unsafe { avifCodecVersions(&mut versions) };
        let text: Vec<u8> = versions
            .iter()
            .take_while(|c| **c != 0)
            .map(|c| *c as u8)
            .collect();
        eprintln!("libavif codecs: {}", String::from_utf8_lossy(&text));
        if !avx2_available() {
            eprintln!("skipped: this CPU has no AVX2");
            return;
        }
        let img = DynamicImage::ImageRgba8(sample_rgba(64, 64));
        let bytes = encode_avif_to_vec(&img, 80, None, None).expect("AVIF encoding must succeed");
        assert!(is_avif(&bytes), "the encoder output must be an AVIF file");
        let (decoded, _icc, _exif) =
            decode_avif(&bytes).expect("our AVIF decoder must read our own encoder output");
        assert!(decoded.color().has_alpha(), "AVIF lost the alpha channel");
        let rgba = decoded.to_rgba8();
        let left = rgba.get_pixel(0, 0).0[3];
        let right = rgba.get_pixel(63, 0).0[3];
        assert!(left < 40, "transparent side must stay transparent, got alpha={left}");
        assert!(right > 215, "opaque side must stay opaque, got alpha={right}");
    }

    #[test]
    #[ignore = "diagnostic: run with SM_CMP_PNG and SM_CMP_AVIF set to the same cut frame"]
    fn avif_matches_png_alpha_map() {
        let png_path = match std::env::var("SM_CMP_PNG") {
            Ok(value) => value,
            Err(_) => return,
        };
        let avif_path = match std::env::var("SM_CMP_AVIF") {
            Ok(value) => value,
            Err(_) => return,
        };
        let png_img = image::open(&png_path)
            .expect("the PNG reference must be readable")
            .to_rgba8();
        let raw = std::fs::read(&avif_path).expect("the AVIF file must be readable");
        let (decoded, _icc, _exif) = decode_avif(&raw).expect("the AVIF file must decode");
        let avif_img = decoded.to_rgba8();
        assert_eq!(png_img.dimensions(), avif_img.dimensions(), "size mismatch");

        let (w, h) = png_img.dimensions();
        let total = (w as u64) * (h as u64);
        let mut worst = 0i32;
        let mut sum = 0i64;
        let (mut both_zero, mut png_zero_avif_not, mut png_not_avif_zero) = (0u64, 0u64, 0u64);
        let (mut rgb_under_zero_png, mut rgb_under_zero_avif) = (0u64, 0u64);
        for (p, a) in png_img.pixels().zip(avif_img.pixels()) {
            let diff = (p.0[3] as i32 - a.0[3] as i32).abs();
            if diff > worst {
                worst = diff;
            }
            sum += diff as i64;
            match (p.0[3], a.0[3]) {
                (0, 0) => both_zero += 1,
                (0, _) => png_zero_avif_not += 1,
                (_, 0) => png_not_avif_zero += 1,
                _ => {}
            }
            if p.0[3] == 0 && (p.0[0] != 0 || p.0[1] != 0 || p.0[2] != 0) {
                rgb_under_zero_png += 1;
            }
            if a.0[3] == 0 && (a.0[0] != 0 || a.0[1] != 0 || a.0[2] != 0) {
                rgb_under_zero_avif += 1;
            }
        }
        let lines = vec![
            format!("png={png_path}"),
            format!("avif={avif_path}"),
            format!("size={w}x{h}"),
            format!("alpha |png-avif|: max={worst} mean={:.4}", sum as f64 / total as f64),
            format!("alpha==0 in both: {both_zero}"),
            format!("png alpha==0, avif alpha>0: {png_zero_avif_not}"),
            format!("png alpha>0, avif alpha==0: {png_not_avif_zero}"),
            format!("rgb non-zero under alpha==0 (png): {rgb_under_zero_png}"),
            format!("rgb non-zero under alpha==0 (avif): {rgb_under_zero_avif}"),
        ];
        let path = std::env::temp_dir().join("sm_avif_vs_png.txt");
        let _ = std::fs::write(path, lines.join("\n"));

        assert!(
            png_zero_avif_not * 10 <= total,
            "AVIF reports transparency where the PNG does not in {png_zero_avif_not} of {total} pixels"
        );
        assert!(
            png_not_avif_zero * 10 <= total,
            "AVIF lost transparency in {png_not_avif_zero} of {total} pixels"
        );
    }
}

#[cfg(test)]
mod oxipng_probe {
    use std::time::Instant;

    #[test]
    fn png_preset_threshold() {
        use super::png_preset_for;
        assert_eq!(png_preset_for(1920, 1080), 1);
        assert_eq!(png_preset_for(2000, 2000), 1);
        assert_eq!(png_preset_for(2828, 2828), 1);
        assert_eq!(png_preset_for(2829, 2829), 0);
        assert_eq!(png_preset_for(4096, 3072), 0);
        assert_eq!(png_preset_for(5888, 4416), 0);
    }

    #[test]
    #[ignore = "diagnostic: run with SM_OXIPNG_PROBE set to a semicolon separated PNG list"]
    fn report_oxipng_cost() {
        let list = match std::env::var("SM_OXIPNG_PROBE") {
            Ok(value) if !value.trim().is_empty() => value,
            _ => return,
        };
        let mut lines: Vec<String> = Vec::new();
        for path in list.split(';').map(str::trim).filter(|p| !p.is_empty()) {
            let raw = match std::fs::read(path) {
                Ok(bytes) => bytes,
                Err(err) => {
                    lines.push(format!("file={path} read_error={err}"));
                    continue;
                }
            };
            let img = match image::load_from_memory(&raw) {
                Ok(img) => img,
                Err(err) => {
                    lines.push(format!("file={path} decode_error={err}"));
                    continue;
                }
            };
            let reference = img.to_rgba8();
            lines.push(format!(
                "file={} px={}x{} source_bytes={}",
                path,
                reference.width(),
                reference.height(),
                raw.len()
            ));

            let (bytes, ms) = encode_png_preset(&img, None);
            lines.push(format!(
                "  image_encoder_only   {ms:>9.0} ms {:>10} B pixels_equal={}",
                bytes.len(),
                pixels_equal(&reference, &bytes)
            ));
            dump(&lines);

            let pixels = reference.width() as usize * reference.height() as usize;
            for preset in [0u8, 1] {
                let (bytes, ms) = encode_png_preset(&img, Some(preset));
                lines.push(format!(
                    "  oxipng_preset_{preset}       {ms:>9.0} ms {:>10} B pixels_equal={}",
                    bytes.len(),
                    pixels_equal(&reference, &bytes)
                ));
                dump(&lines);
            }
            if pixels <= 8_000_000 {
                let (bytes, ms) = encode_png_preset(&img, Some(2));
                lines.push(format!(
                    "  oxipng_preset_2       {ms:>9.0} ms {:>10} B pixels_equal={}",
                    bytes.len(),
                    pixels_equal(&reference, &bytes)
                ));
                dump(&lines);
            } else {
                lines.push(
                    "  oxipng_preset_2       skipped (>8 MP: exceeds 10 min in this probe)"
                        .to_string(),
                );
                dump(&lines);
            }
        }
        dump(&lines);
    }

    fn dump(lines: &[String]) {
        let text = lines.join("\n");
        println!("{text}");
        let _ = std::fs::write(std::env::temp_dir().join("sm_oxipng_probe.txt"), &text);
    }

    fn encode_png_preset(img: &image::DynamicImage, preset: Option<u8>) -> (Vec<u8>, f64) {
        let rgba = img.to_rgba8();
        let mut buf = Vec::new();
        let started = Instant::now();
        let encoder = image::codecs::png::PngEncoder::new(&mut buf);
        image::ImageEncoder::write_image(
            encoder,
            rgba.as_raw(),
            rgba.width(),
            rgba.height(),
            image::ExtendedColorType::Rgba8,
        )
        .expect("png encode");
        if let Some(preset) = preset {
            buf = oxipng::optimize_from_memory(&buf, &oxipng::Options::from_preset(preset))
                .expect("oxipng optimize");
        }
        (buf, started.elapsed().as_secs_f64() * 1000.0)
    }

    #[test]
    fn jxl_exif_round_trips_through_the_encoder() {
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            4,
            4,
            image::Rgb([10, 20, 30]),
        ));
        let blob: &[u8] = b"MM\x00*\x00\x00\x00\x08\x00\x01\x01\x12\x00\x03";
        let jxl = super::encode_jxl_to_vec(&img, 90, false, None, Some(blob)).expect("jxl encode");
        let pos = jxl
            .windows(4)
            .position(|w| w == b"Exif")
            .expect("encoded JXL must carry an Exif box");
        assert_eq!(
            &jxl[pos + 4..pos + 8],
            &0u32.to_be_bytes(),
            "the TIFF offset field is zero; the TIFF header follows it"
        );
        assert_eq!(&jxl[pos + 8..pos + 8 + blob.len()], blob, "the Exif payload must follow the offset");
    }

    #[test]
    fn jxl_exif_is_read_back_by_our_own_reader() {
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            4,
            4,
            image::Rgb([10, 20, 30]),
        ));
        let blob: &[u8] = b"MM\x00*\x00\x00\x00\x08\x00\x01\x01\x12\x00\x03";
        let jxl = super::encode_jxl_to_vec(&img, 90, false, None, Some(blob)).expect("jxl encode");
        let decoded = crate::decode::jxl_exif(&jxl).expect("exif must survive the round-trip");
        assert_eq!(decoded, blob);
    }

    fn pixels_equal(reference: &image::RgbaImage, png: &[u8]) -> bool {
        match image::load_from_memory(png) {
            Ok(img) => img.to_rgba8() == *reference,
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod alpha_diff {
    use std::path::PathBuf;

    #[test]
    #[ignore = "diagnostic: run with SM_ALPHA_DIFF=\"a.png;b.png;out_dir\""]
    fn compare_alpha() {
        let spec = match std::env::var("SM_ALPHA_DIFF") {
            Ok(value) if !value.trim().is_empty() => value,
            _ => return,
        };
        let parts: Vec<&str> = spec.split(';').map(str::trim).filter(|p| !p.is_empty()).collect();
        if parts.len() < 3 {
            println!("usage: SM_ALPHA_DIFF=\"a.png;b.png;out_dir\"");
            return;
        }
        let a = image::open(parts[0]).expect("open A").to_rgba8();
        let b = image::open(parts[1]).expect("open B").to_rgba8();
        assert_eq!((a.width(), a.height()), (b.width(), b.height()), "size mismatch");
        let (w, h) = (a.width() as usize, a.height() as usize);
        let out = PathBuf::from(parts[2]);
        std::fs::create_dir_all(&out).expect("out dir");

        let cell = 256usize;
        let cols = w.div_ceil(cell);
        let rows = h.div_ceil(cell);
        let mut cells = vec![0u64; cols * rows];
        let mut col_sum = vec![0u64; w];
        let mut row_sum = vec![0u64; h];
        let mut heat = vec![0u8; w * h];
        let mut overlay = vec![0u8; w * h * 3];
        let pa = a.as_raw();
        let pb = b.as_raw();
        let mut max = 0u8;
        let mut sum = 0u64;
        let mut over = [0u64; 4];
        let mut fade_out = 0u64;
        let mut fade_in = 0u64;
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (w, h, 0usize, 0usize);
        for y in 0..h {
            for x in 0..w {
                let index = (y * w + x) * 4;
                let alpha_a = pa[index + 3];
                let alpha_b = pb[index + 3];
                let d = alpha_a.abs_diff(alpha_b);
                sum += d as u64;
                max = max.max(d);
                col_sum[x] += d as u64;
                row_sum[y] += d as u64;
                if d > 0 {
                    over[0] += 1;
                }
                if d > 1 {
                    over[1] += 1;
                }
                if d > 4 {
                    over[2] += 1;
                }
                if d > 16 {
                    over[3] += 1;
                }
                if alpha_a > 200 && alpha_b < 64 {
                    fade_out += 1;
                }
                if alpha_a < 64 && alpha_b > 200 {
                    fade_in += 1;
                }
                if d > 0 {
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                    max_x = max_x.max(x);
                    max_y = max_y.max(y);
                }
                heat[y * w + x] = (d as u32 * 8).min(255) as u8;
                let o = (y * w + x) * 3;
                overlay[o] = alpha_b;
                overlay[o + 1] = alpha_a;
                overlay[o + 2] = (d as u32 * 8).min(255) as u8;
                cells[(y / cell) * cols + (x / cell)] += d as u64;
            }
        }
        let total = (w * h) as u64;
        let mut lines = vec![
            format!("A={}", parts[0]),
            format!("B={}", parts[1]),
            format!("size={w}x{h} pixels={total}"),
            format!(
                "alpha|d|: max={max} mean={:.5} over0={} over1={} over4={} over16={}",
                sum as f64 / total as f64,
                over[0],
                over[1],
                over[2],
                over[3]
            ),
        ];
        if over[0] > 0 {
            lines.push(format!("bbox=({min_x},{min_y})..({max_x},{max_y})"));
        }
        lines.push(format!(
            "mask_flips: A_opaque->B_clear={fade_out} A_clear->B_opaque={fade_in}"
        ));
        let mut order: Vec<usize> = (0..cells.len()).collect();
        order.sort_by_key(|i| std::cmp::Reverse(cells[*i]));
        for (rank, index) in order.iter().take(6).enumerate() {
            if cells[*index] == 0 {
                break;
            }
            let gx = index % cols;
            let gy = index / cols;
            lines.push(format!(
                "hot{rank}: cell=({},{}) side={cell} sum={} center=({},{})",
                gx * cell,
                gy * cell,
                cells[*index],
                gx * cell + cell / 2,
                gy * cell + cell / 2
            ));
            let pad = 512usize;
            let x0 = (gx * cell).saturating_sub(pad);
            let y0 = (gy * cell).saturating_sub(pad);
            let x1 = (gx * cell + cell + pad).min(w);
            let y1 = (gy * cell + cell + pad).min(h);
            let (cw, ch) = ((x1 - x0) as u32, (y1 - y0) as u32);
            for (tag, img) in [("a", &a), ("b", &b)] {
                let crop = image::imageops::crop_imm(img, x0 as u32, y0 as u32, cw, ch).to_image();
                crop.save(out.join(format!("hot{rank}_{tag}_x{x0}_y{y0}.png")))
                    .expect("crop save");
            }
        }
        let stats = |v: &[u64]| -> (u64, u64) {
            let mut s = v.to_vec();
            s.sort_unstable();
            (s[s.len() / 2], s[s.len() * 99 / 100])
        };
        let (col_med, col_p99) = stats(&col_sum);
        let (row_med, row_p99) = stats(&row_sum);
        let mut cols_idx: Vec<usize> = (0..w).collect();
        cols_idx.sort_by_key(|i| std::cmp::Reverse(col_sum[*i]));
        let mut rows_idx: Vec<usize> = (0..h).collect();
        rows_idx.sort_by_key(|i| std::cmp::Reverse(row_sum[*i]));
        lines.push(format!("col_profile: median={col_med} p99={col_p99}"));
        lines.push(format!(
            "col_top10: {}",
            cols_idx
                .iter()
                .take(10)
                .map(|i| format!("x{i}={}", col_sum[*i]))
                .collect::<Vec<_>>()
                .join(" ")
        ));
        lines.push(format!("row_profile: median={row_med} p99={row_p99}"));
        lines.push(format!(
            "row_top10: {}",
            rows_idx
                .iter()
                .take(10)
                .map(|i| format!("y{i}={}", row_sum[*i]))
                .collect::<Vec<_>>()
                .join(" ")
        ));
        let heat_path = out.join("alpha_diff_heat.png");
        image::GrayImage::from_raw(w as u32, h as u32, heat)
            .expect("heat buffer")
            .save(&heat_path)
            .expect("heat save");
        let overlay_path = out.join("alpha_overlay_Rb_Ga_Bdiff.png");
        image::RgbImage::from_raw(w as u32, h as u32, overlay)
            .expect("overlay buffer")
            .save(&overlay_path)
            .expect("overlay save");
        lines.push(format!("heatmap={}", heat_path.display()));
        lines.push(format!("overlay={}", overlay_path.display()));
        let text = lines.join("\n");
        println!("{text}");
        let _ = std::fs::write(std::env::temp_dir().join("sm_alpha_diff.txt"), &text);
    }
}

