use rayon::prelude::*;

const DOWNSAMPLE: usize = 16;
const BLUR_RADIUS: usize = 2;
const BLACK_PT: f32 = 0.25;
const WHITE_PT: f32 = 0.85;
const TOP_FRACTION: f32 = 0.90;
const CHROMA_RADIUS: usize = 1;

pub(crate) fn smart_scan(mut img: image::DynamicImage) -> image::DynamicImage {
    let done = if let Some(luma) = img.as_mut_luma8() {
        let (w, h) = (luma.width() as usize, luma.height() as usize);
        scan_planes(luma.as_mut(), w, h, 1, 1);
        true
    } else if let Some(la) = img.as_mut_luma_alpha8() {
        let (w, h) = (la.width() as usize, la.height() as usize);
        scan_planes(la.as_mut(), w, h, 2, 1);
        true
    } else if let Some(rgb) = img.as_mut_rgb8() {
        let (w, h) = (rgb.width() as usize, rgb.height() as usize);
        scan_planes(rgb.as_mut(), w, h, 3, 3);
        true
    } else if let Some(rgba) = img.as_mut_rgba8() {
        let (w, h) = (rgba.width() as usize, rgba.height() as usize);
        scan_planes(rgba.as_mut(), w, h, 4, 3);
        true
    } else {
        false
    };
    if done {
        img
    } else {
        let mut rgb = img.to_rgb8();
        let (w, h) = (rgb.width() as usize, rgb.height() as usize);
        scan_planes(rgb.as_mut(), w, h, 3, 3);
        image::DynamicImage::ImageRgb8(rgb)
    }
}

fn scan_planes(buf: &mut [u8], w: usize, h: usize, ch: usize, ncolor: usize) {
    if w == 0 || h == 0 || buf.len() < w * h * ch {
        return;
    }
    let stride = w * ch;

    if ncolor == 3 {
        chroma_denoise(buf, w, h, ch, CHROMA_RADIUS);
    }

    let bg_w = (w / DOWNSAMPLE).max(1);
    let bg_h = (h / DOWNSAMPLE).max(1);

    let mut bg_small = vec![0u8; bg_w * bg_h * ncolor];
    for by in 0..bg_h {
        let y0 = by * h / bg_h;
        let y1 = ((by + 1) * h / bg_h).max(y0 + 1);
        for bx in 0..bg_w {
            let x0 = bx * w / bg_w;
            let x1 = ((bx + 1) * w / bg_w).max(x0 + 1);

            let mut max_l = 0u32;
            for y in y0..y1 {
                let row = &buf[y * stride..(y + 1) * stride];
                for x in x0..x1 {
                    let l = luma(row, x, ch, ncolor);
                    if l > max_l {
                        max_l = l;
                    }
                }
            }

            let thresh = (max_l as f32 * TOP_FRACTION).round() as u32;
            let mut sums = [0u32; 3];
            let mut cnt = 0u32;
            for y in y0..y1 {
                let row = &buf[y * stride..(y + 1) * stride];
                for x in x0..x1 {
                    if luma(row, x, ch, ncolor) >= thresh {
                        for c in 0..ncolor {
                            sums[c] += row[x * ch + c] as u32;
                        }
                        cnt += 1;
                    }
                }
            }

            let base = (by * bg_w + bx) * ncolor;
            let n = cnt.max(1);
            for c in 0..ncolor {
                bg_small[base + c] = (sums[c] / n) as u8;
            }
        }
    }

    let bg_blur = box_blur_chan(&bg_small, bg_w, bg_h, ncolor, ncolor, BLUR_RADIUS);

    let inv_range = 1.0 / (WHITE_PT - BLACK_PT);

    buf.par_chunks_mut(stride).enumerate().for_each(|(y, row)| {
        let fy = (y as f32 * (bg_h as f32 - 1.0) / (h as f32).max(1.0)).clamp(0.0, (bg_h - 1) as f32);
        let y0 = fy as usize;
        let y1 = (y0 + 1).min(bg_h - 1);
        let ty = fy - y0 as f32;

        for x in 0..w {
            let fx = (x as f32 * (bg_w as f32 - 1.0) / (w as f32).max(1.0)).clamp(0.0, (bg_w - 1) as f32);
            let x0 = fx as usize;
            let x1 = (x0 + 1).min(bg_w - 1);
            let tx = fx - x0 as f32;

            for c in 0..ncolor {
                let v00 = bg_blur[(y0 * bg_w + x0) * ncolor + c] as f32;
                let v10 = bg_blur[(y0 * bg_w + x1) * ncolor + c] as f32;
                let v01 = bg_blur[(y1 * bg_w + x0) * ncolor + c] as f32;
                let v11 = bg_blur[(y1 * bg_w + x1) * ncolor + c] as f32;

                let top = v00 + (v10 - v00) * tx;
                let bot = v01 + (v11 - v01) * tx;
                let bg_val = top + (bot - top) * ty;

                let normalized = row[x * ch + c] as f32 / bg_val.max(1.0);
                let t = ((normalized - BLACK_PT) * inv_range).clamp(0.0, 1.0);
                row[x * ch + c] = (t * 255.0).round() as u8;
            }
        }
    });
}

fn luma(row: &[u8], x: usize, ch: usize, ncolor: usize) -> u32 {
    let p = x * ch;
    if ncolor == 1 {
        row[p] as u32
    } else {
        let r = row[p] as u32;
        let g = row[p + 1] as u32;
        let b = row[p + 2] as u32;
        (r * 54 + g * 183 + b * 19) >> 8
    }
}

fn box_blur_chan(src: &[u8], w: usize, h: usize, stride: usize, ncolor: usize, r: usize) -> Vec<u8> {
    let mut tmp = vec![0u8; w * h * ncolor];
    let mut out = vec![0u8; w * h * ncolor];
    for c in 0..ncolor {
        tmp.par_chunks_mut(w * ncolor).enumerate().for_each(|(y, row)| {
            for x in 0..w {
                let x0 = x.saturating_sub(r);
                let x1 = (x + r).min(w - 1);
                let mut acc = 0u32;
                for xx in x0..=x1 {
                    acc += src[y * w * stride + xx * stride + c] as u32;
                }
                row[x * ncolor + c] = (acc / (x1 - x0 + 1) as u32) as u8;
            }
        });
        for y in 0..h {
            let y0 = y.saturating_sub(r);
            let y1 = (y + r).min(h - 1);
            for x in 0..w {
                let mut acc = 0u32;
                for yy in y0..=y1 {
                    acc += tmp[(yy * w + x) * ncolor + c] as u32;
                }
                out[(y * w + x) * ncolor + c] = (acc / (y1 - y0 + 1) as u32) as u8;
            }
        }
    }
    out
}

fn chroma_denoise(buf: &mut [u8], w: usize, h: usize, ch: usize, r: usize) {
    if w < 2 || h < 2 {
        return;
    }
    let stride = w * ch;

    let mut luma_map = vec![0u8; w * h];
    for y in 0..h {
        let row = &buf[y * stride..(y + 1) * stride];
        for x in 0..w {
            luma_map[y * w + x] = luma(row, x, ch, 3) as u8;
        }
    }

    let bl = box_blur_chan(&luma_map, w, h, 1, 1, r);
    let br = box_blur_chan(buf, w, h, ch, 3, r);

    buf.par_chunks_mut(stride).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let l = luma_map[y * w + x] as i32;
            let bl_val = bl[y * w + x] as i32;
            for c in 0..3 {
                let v = l + br[(y * w + x) * 3 + c] as i32 - bl_val;
                row[x * ch + c] = v.clamp(0, 255) as u8;
            }
        }
    });
}


