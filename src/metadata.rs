use anyhow::Result;

pub(crate) fn webp_embed_metadata(webp: Vec<u8>, icc: Option<&[u8]>, exif: Option<&[u8]>, w: u32, h: u32, has_alpha: bool) -> Result<Vec<u8>> {
    let has_icc = icc.map_or(false, |p| !p.is_empty());
    let has_exif = exif.map_or(false, |e| !e.is_empty());
    if webp.len() < 12 || (!has_icc && !has_exif) {
        return Ok(webp);
    }
    let riff_size = u32::from_le_bytes([webp[4], webp[5], webp[6], webp[7]]) as usize;
    let end = 8usize.saturating_add(riff_size);
    if riff_size < 10 || end > webp.len() {
        return Ok(webp);
    }
    let has_vp8x = end >= 30
        && &webp[12..16] == b"VP8X"
        && u32::from_le_bytes([webp[16], webp[17], webp[18], webp[19]]) == 10;
    let mut flags = if has_vp8x { webp[20] } else { 0u8 };
    if has_icc { flags |= 0x20; }
    if has_exif { flags |= 0x08; }
    if has_alpha { flags |= 0x10; }
    let mut chunks: Vec<u8> = Vec::new();
    if has_icc {
        let profile = icc.unwrap();
        chunks.extend_from_slice(b"ICCP");
        chunks.extend_from_slice(&(profile.len() as u32).to_le_bytes());
        chunks.extend_from_slice(profile);
        if profile.len() & 1 == 1 { chunks.push(0); }
    }
    if has_exif {
        let blob = exif.unwrap();
        chunks.extend_from_slice(b"EXIF");
        chunks.extend_from_slice(&(blob.len() as u32).to_le_bytes());
        chunks.extend_from_slice(blob);
        if blob.len() & 1 == 1 { chunks.push(0); }
    }
    let mut vp8x = Vec::with_capacity(18);
    vp8x.extend_from_slice(b"VP8X");
    vp8x.extend_from_slice(&10u32.to_le_bytes());
    vp8x.push(flags);
    vp8x.extend_from_slice(&[0, 0, 0]);
    let w1 = w.wrapping_sub(1) & 0xFFFFFF;
    vp8x.extend_from_slice(&[w1 as u8, (w1 >> 8) as u8, (w1 >> 16) as u8]);
    let h1 = h.wrapping_sub(1) & 0xFFFFFF;
    vp8x.extend_from_slice(&[h1 as u8, (h1 >> 8) as u8, (h1 >> 16) as u8]);
    let mut out = Vec::with_capacity(webp.len() + vp8x.len() + chunks.len());
    out.extend_from_slice(&webp[..12]);
    out.extend_from_slice(&vp8x);
    out.extend_from_slice(&chunks);
    if has_vp8x {
        out.extend_from_slice(&webp[30..end]);
    } else {
        out.extend_from_slice(&webp[12..end]);
    }
    let new_size = (out.len() - 8) as u32;
    out[4..8].copy_from_slice(&new_size.to_le_bytes());
    Ok(out)
}

pub(crate) fn encode_tiff_icc(
    width: u32,
    height: u32,
    pixels: &[u8],
    channels: u8,
    icc: &[u8],
) -> Result<Vec<u8>> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut encoder = tiff::encoder::TiffEncoder::new(&mut buf)?;
        match channels {
            3 => {
                let mut image = encoder.new_image::<tiff::encoder::colortype::RGB8>(width, height)?;
                image
                    .encoder()
                    .write_tag(tiff::tags::Tag::IccProfile, icc)?;
                image.write_data(pixels)?;
            }
            4 => {
                let mut image = encoder.new_image::<tiff::encoder::colortype::RGBA8>(width, height)?;
                image
                    .encoder()
                    .write_tag(tiff::tags::Tag::IccProfile, icc)?;
                image.write_data(pixels)?;
            }
            _ => return Err(anyhow::anyhow!("unsupported TIFF channel count: {}", channels)),
        }
    }
    Ok(buf.into_inner())
}

fn read16(b: &[u8], off: usize, little: bool) -> Option<u16> {
    let s = b.get(off..off + 2)?;
    let a = [s[0], s[1]];
    Some(if little { u16::from_le_bytes(a) } else { u16::from_be_bytes(a) })
}

fn read32(b: &[u8], off: usize, little: bool) -> Option<u32> {
    let s = b.get(off..off + 4)?;
    let a = [s[0], s[1], s[2], s[3]];
    Some(if little { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) })
}

fn write16(b: &mut [u8], off: usize, v: u16, little: bool) {
    let a = if little { v.to_le_bytes() } else { v.to_be_bytes() };
    if let Some(dst) = b.get_mut(off..off + 2) {
        dst.copy_from_slice(&a);
    }
}

fn write32(b: &mut [u8], off: usize, v: u32, little: bool) {
    let a = if little { v.to_le_bytes() } else { v.to_be_bytes() };
    if let Some(dst) = b.get_mut(off..off + 4) {
        dst.copy_from_slice(&a);
    }
}

pub(crate) fn normalize_exif(mut blob: Vec<u8>, w: u32, h: u32) -> Option<Vec<u8>> {
    let little = match blob.get(..2) {
        Some(b) if b == b"II" => true,
        Some(b) if b == b"MM" => false,
        _ => return None,
    };
    let ifd0 = read32(&blob, 4, little)? as usize;
    if ifd0 == 0 {
        return None;
    }
    let n = read16(&blob, ifd0, little)? as usize;
    let entries_end = ifd0.checked_add(2)?.checked_add(n.checked_mul(12)?)?;
    if entries_end > blob.len() {
        return None;
    }
    let mut exif_ifd: Option<usize> = None;
    for i in 0..n {
        let e = ifd0 + 2 + i * 12;
        let tag = read16(&blob, e, little)?;
        match tag {
            0x0112 => {
                if read16(&blob, e + 2, little)? != 3 || read32(&blob, e + 4, little)? != 1 {
                    return None;
                }
                write16(&mut blob, e + 8, 1, little);
            }
            0x8769 => {
                if read16(&blob, e + 2, little)? == 4 && read32(&blob, e + 4, little)? == 1 {
                    let off = read32(&blob, e + 8, little)? as usize;
                    if off != 0 {
                        exif_ifd = Some(off);
                    }
                }
            }
            0xA002 | 0xA003 => {
                let v = if tag == 0xA002 { w } else { h };
                let typ = read16(&blob, e + 2, little)?;
                let count = read32(&blob, e + 4, little)?;
                match typ {
                    3 if count == 1 => write16(&mut blob, e + 8, v as u16, little),
                    4 if count == 1 => write32(&mut blob, e + 8, v, little),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    if let Some(ifd1) = exif_ifd {
        let n1 = read16(&blob, ifd1, little)? as usize;
        let end1 = ifd1.checked_add(2)?.checked_add(n1.checked_mul(12)?)?;
        if end1 > blob.len() {
            return None;
        }
        for i in 0..n1 {
            let e = ifd1 + 2 + i * 12;
            let tag = read16(&blob, e, little)?;
            if tag == 0xA002 || tag == 0xA003 {
                let v = if tag == 0xA002 { w } else { h };
                let typ = read16(&blob, e + 2, little)?;
                let count = read32(&blob, e + 4, little)?;
                match typ {
                    3 if count == 1 => write16(&mut blob, e + 8, v as u16, little),
                    4 if count == 1 => write32(&mut blob, e + 8, v, little),
                    _ => {}
                }
            }
        }
    }
    Some(blob)
}
