use anyhow::{Context, Result};
use std::path::Path;

use crate::msg;
use crate::Config;
use crate::ImageFormat;
use crate::metadata::{encode_tiff_icc, normalize_exif, webp_embed_metadata};

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
    let mut crc_input = Vec::with_capacity(4 + exif.len());
    crc_input.extend_from_slice(b"eXIf");
    crc_input.extend_from_slice(exif);
    out.extend_from_slice(&crc32fast::hash(&crc_input).to_be_bytes());
    out.extend_from_slice(&png[33..]);
    out
}


use image::imageops::FilterType;

pub(crate) fn encode_to_vec(img: &image::DynamicImage, config: &Config, icc: Option<&[u8]>, exif: Option<&[u8]>) -> Result<Vec<u8>> {
    let exif = exif.and_then(|blob| normalize_exif(blob.to_vec(), img.width(), img.height()));
    let exif = exif.as_deref();
    match config.format {
        ImageFormat::Jpeg => encode_jpeg_to_vec(img, config.quality, config.progressive, icc, exif),
        ImageFormat::WebP => encode_webp_to_vec(img, config.quality, config.lossless, icc, exif),
        ImageFormat::Avif => encode_avif_to_vec(img, config.quality),
        ImageFormat::Png => encode_png_to_vec(img, icc, exif),
        ImageFormat::Ico => encode_ico_to_vec(img),
        ImageFormat::Tiff => encode_tiff_to_vec(img, icc),
        ImageFormat::Qoi => encode_qoi_to_vec(img),
        ImageFormat::Bmp => encode_bmp_to_vec(img),
        ImageFormat::Gif => encode_gif_to_vec(img),
        ImageFormat::Jxl => encode_jxl_to_vec(img, config.quality, config.lossless, icc, exif),
    }
}pub(crate) fn encode_bmp(img: &image::DynamicImage, out: &Path) -> Result<()> {
    img.save_with_format(out, image::ImageFormat::Bmp)?;
    Ok(())
}

fn encode_jpeg_to_vec(img: &image::DynamicImage, quality: u8, progressive: bool, icc: Option<&[u8]>, exif: Option<&[u8]>) -> Result<Vec<u8>> {
    if let Some(gray) = img.as_luma8() {
        return encode_jpeg_gray(gray, quality, progressive, icc, exif);
    }
    let rgb = img.to_rgb8();
    let (w, h) = (rgb.width() as usize, rgb.height() as usize);
    let raw = rgb.into_raw();
    let mut comp = mozjpeg::Compress::new(mozjpeg::ColorSpace::JCS_RGB);
    comp.set_size(w, h);
    if progressive { comp.set_progressive_mode(); }
    comp.set_quality(quality.clamp(1, 100) as f32);
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
        comp.write_scanlines(&raw[line * w * 3..(line + 1) * w * 3])?;
    }
    Ok(comp.finish()?)
}fn encode_jpeg_gray(gray: &image::GrayImage, quality: u8, progressive: bool, icc: Option<&[u8]>, exif: Option<&[u8]>) -> Result<Vec<u8>> {
    let (w, h) = (gray.width() as usize, gray.height() as usize);
    let raw: &[u8] = gray.as_raw();
    let mut comp = mozjpeg::Compress::new(mozjpeg::ColorSpace::JCS_GRAYSCALE);
    comp.set_size(w, h);
    if progressive { comp.set_progressive_mode(); }
    comp.set_quality(quality.clamp(1, 100) as f32);
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
        comp.write_scanlines(&raw[line * w..(line + 1) * w])?;
    }
    Ok(comp.finish()?)
}fn webp_encode(enc: webp::Encoder<'_>, quality: f32, lossless: bool) -> Result<webp::WebPMemory> {
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
    let has_icc = icc.map_or(false, |p| !p.is_empty());
    let has_exif = exif.map_or(false, |e| !e.is_empty());
    if has_icc || has_exif {
        webp_embed_metadata(bytes, icc, exif, w, h)
    } else {
        Ok(bytes)
    }
}fn encode_avif_to_vec(img: &image::DynamicImage, quality: u8) -> Result<Vec<u8>> {
    let q = quality.clamp(1, 100) as f32;
    if img.color().has_alpha() {
        let owned;
        let rgba_img: &image::RgbaImage = if let Some(rgba) = img.as_rgba8() {
            rgba
        } else {
            owned = img.to_rgba8();
            &owned
        };
        let (w, h) = (rgba_img.width() as usize, rgba_img.height() as usize);
        let pixels: &[rgb::RGBA8] = rgb::bytemuck::cast_slice(rgba_img.as_raw().as_slice());
        let enc = ravif::Encoder::new()
            .with_quality(q)
            .with_speed(6)
            .with_num_threads(Some(1))
            .encode_rgba(ravif::Img::new(pixels, w, h))
            .map_err(|e| anyhow::anyhow!("{}", msg().err_avif.replacen("{}", &e.to_string(), 1)))?;
        Ok(enc.avif_file)
    } else {
        let owned;
        let rgb_img: &image::RgbImage = if let Some(rgb) = img.as_rgb8() {
            rgb
        } else {
            owned = img.to_rgb8();
            &owned
        };
        let (w, h) = (rgb_img.width() as usize, rgb_img.height() as usize);
        let pixels: &[rgb::RGB8] = rgb::bytemuck::cast_slice(rgb_img.as_raw().as_slice());
        let enc = ravif::Encoder::new()
            .with_quality(q)
            .with_speed(6)
            .with_num_threads(Some(1))
            .encode_rgb(ravif::Img::new(pixels, w, h))
            .map_err(|e| anyhow::anyhow!("{}", msg().err_avif.replacen("{}", &e.to_string(), 1)))?;
        Ok(enc.avif_file)
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
    if let Some(blob) = exif {
        if !blob.is_empty() {
            buf = png_embed_exif(buf, blob);
        }
    }
    oxipng::optimize_from_memory(&buf, &oxipng::Options::from_preset(1))
        .map_err(|e| anyhow::anyhow!("{}", msg().err_oxipng.replacen("{}", &e.to_string(), 1)))
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
    if ico_dir.entries().len() == 0 {
        let image = ico::IconImage::from_rgba_data(w, h, rgba.into_raw());
        ico_dir.add_entry(ico::IconDirEntry::encode(&image).context(msg().err_ico_entry)?);
    }
    let mut buf = std::io::Cursor::new(Vec::new());
    ico_dir.write(&mut buf).context(msg().err_ico_write)?;
    Ok(buf.into_inner())
}

fn encode_tiff_to_vec(img: &image::DynamicImage, icc: Option<&[u8]>) -> Result<Vec<u8>> {
    if let Some(profile) = icc.filter(|p| !p.is_empty()) {
        if let Some(rgb) = img.as_rgb8() {
            return encode_tiff_icc(rgb.width(), rgb.height(), rgb.as_raw(), 3, profile);
        }
        if let Some(rgba) = img.as_rgba8() {
            return encode_tiff_icc(rgba.width(), rgba.height(), rgba.as_raw(), 4, profile);
        }
    }
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Tiff)?;
    Ok(buf.into_inner())
}

fn encode_qoi_to_vec(img: &image::DynamicImage) -> Result<Vec<u8>> {
    let rgba = img.to_rgba8();
    let w = rgba.width();
    let h = rgba.height();
    qoi::encode_to_vec(&rgba.into_raw(), w, h).context(msg().err_qoi)
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

fn encode_jxl_raw(raw: &[u8], w: u32, h: u32, layout: jxl_encoder::PixelLayout, quality: u8, lossless: bool, icc: Option<&[u8]>, exif: Option<&[u8]>) -> Result<Vec<u8>> {
    let mut metadata = icc.filter(|p| !p.is_empty()).map(|p| jxl_encoder::ImageMetadata::new().with_icc_profile(p));
    if let Some(blob) = exif.filter(|e| !e.is_empty()) {
        metadata = Some(metadata.unwrap_or_else(jxl_encoder::ImageMetadata::new).with_exif(blob));
    }
    if lossless {
        let config = jxl_encoder::LosslessConfig::new().with_effort(5);
        if let Some(meta) = &metadata {
            config.encode_request(w, h, layout).with_metadata(meta).encode(raw)
        } else {
            config.encode_request(w, h, layout).encode(raw)
        }
        .map_err(|e| anyhow::anyhow!("JXL encode failed: {}", e))
    } else {
        let distance = jxl_encoder::quality_to_distance(quality.clamp(1, 100) as f32);
        let config = jxl_encoder::LossyConfig::new(distance).with_effort(5);
        if let Some(meta) = &metadata {
            config.encode_request(w, h, layout).with_metadata(meta).encode(raw)
        } else {
            config.encode_request(w, h, layout).encode(raw)
        }
        .map_err(|e| anyhow::anyhow!("JXL encode failed: {}", e))
    }
}fn encode_jxl_to_vec(img: &image::DynamicImage, quality: u8, lossless: bool, icc: Option<&[u8]>, exif: Option<&[u8]>) -> Result<Vec<u8>> {
    if let Some(gray) = img.as_luma8() {
        return encode_jxl_raw(gray.as_raw(), gray.width(), gray.height(), jxl_encoder::PixelLayout::Gray8, quality, lossless, icc, exif);
    }
    if let Some(rgb) = img.as_rgb8() {
        return encode_jxl_raw(rgb.as_raw(), rgb.width(), rgb.height(), jxl_encoder::PixelLayout::Rgb8, quality, lossless, icc, exif);
    }
    let owned;
    let rgba_img: &image::RgbaImage = if let Some(rgba) = img.as_rgba8() {
        rgba
    } else {
        owned = img.to_rgba8();
        &owned
    };
    encode_jxl_raw(rgba_img.as_raw(), rgba_img.width(), rgba_img.height(), jxl_encoder::PixelLayout::Rgba8, quality, lossless, icc, exif)
}