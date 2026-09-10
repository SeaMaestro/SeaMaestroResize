use rayon::prelude::*;

const DETECT_LONG_EDGE: u32 = 512;
const AREA_MIN: f32 = 0.12;
const AREA_MAX: f32 = 0.95;
const ASPECT_MIN: f32 = 0.4;
const ASPECT_MAX: f32 = 2.5;
const MIN_COMPONENT: usize = 64;

pub(crate) fn deskew(img: image::DynamicImage) -> image::DynamicImage {
    let (w, h) = (img.width(), img.height());
    if w < 16 || h < 16 {
        return img;
    }
    let corners = match detect_corners(&img, w, h) {
        Some(c) => c,
        None => return img,
    };
    warp(&img, &corners)
}

fn detect_corners(img: &image::DynamicImage, w: u32, h: u32) -> Option<[(f32, f32); 4]> {
    let long = w.max(h);
    let scale = (DETECT_LONG_EDGE as f32 / long as f32).min(1.0);
    let dw = ((w as f32 * scale).round() as u32).max(2);
    let dh = ((h as f32 * scale).round() as u32).max(2);

    let small = img
        .resize_exact(dw, dh, image::imageops::FilterType::Triangle)
        .to_luma8();
    let luma = small.as_raw();

    let r = (dw.min(dh) / 16).max(1) as usize;
    let blur = box_blur_gray(luma, dw as usize, dh as usize, r);

    let n = (dw * dh) as usize;
    let mut norm = vec![0f32; n];
    for i in 0..n {
        let v = luma[i] as f32 / (blur[i] as f32).max(1.0);
        norm[i] = v.clamp(0.0, 1.0);
    }

    let mut hist = [0u32; 256];
    for &v in &norm {
        hist[(v * 255.0) as usize] += 1;
    }
    let t = otsu(&hist) as f32 / 255.0;

    let mut mask = vec![0u8; n];
    for i in 0..n {
        if norm[i] > t {
            mask[i] = 1;
        }
    }

    let comp = largest_component(&mask, dw as usize, dh as usize)?;

    let mut pts: Vec<(i32, i32)> = comp.iter().map(|&(x, y)| (x as i32, y as i32)).collect();
    let hull = convex_hull(&mut pts);
    if hull.len() < 4 {
        return None;
    }

    let hull_f: Vec<(f32, f32)> = hull.iter().map(|&(x, y)| (x as f32, y as f32)).collect();
    let corners = simplify_to_4(&hull_f);
    if corners.len() != 4 {
        return None;
    }
    let c = [corners[0], corners[1], corners[2], corners[3]];
    let ordered = order_corners(&c);

    if !validate(&ordered, dw as f32, dh as f32) {
        return None;
    }

    let inv_scale = 1.0 / scale;
    Some(ordered.map(|(x, y)| (x * inv_scale, y * inv_scale)))
}

fn box_blur_gray(src: &[u8], w: usize, h: usize, r: usize) -> Vec<u8> {
    let mut tmp = vec![0u8; w * h];
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            let x0 = x.saturating_sub(r);
            let x1 = (x + r).min(w - 1);
            let mut acc = 0u32;
            for xx in x0..=x1 {
                acc += src[y * w + xx] as u32;
            }
            tmp[y * w + x] = (acc / (x1 - x0 + 1) as u32) as u8;
        }
    }
    for y in 0..h {
        let y0 = y.saturating_sub(r);
        let y1 = (y + r).min(h - 1);
        for x in 0..w {
            let mut acc = 0u32;
            for yy in y0..=y1 {
                acc += tmp[yy * w + x] as u32;
            }
            out[y * w + x] = (acc / (y1 - y0 + 1) as u32) as u8;
        }
    }
    out
}

fn otsu(hist: &[u32; 256]) -> u8 {
    let total: u32 = hist.iter().sum();
    if total == 0 {
        return 128;
    }
    let mut sum = 0u64;
    for (i, &h) in hist.iter().enumerate() {
        sum += (i as u64) * (h as u64);
    }
    let mut sum_b = 0u64;
    let mut w_b = 0u64;
    let mut best = 0f64;
    let mut best_t = 0u8;
    for (t, &h) in hist.iter().enumerate() {
        w_b += h as u64;
        if w_b == 0 {
            continue;
        }
        let w_f = total as u64 - w_b;
        if w_f == 0 {
            break;
        }
        sum_b += (t as u64) * (h as u64);
        let m_b = sum_b as f64 / w_b as f64;
        let m_f = (sum - sum_b) as f64 / w_f as f64;
        let var = (w_b as f64) * (w_f as f64) * (m_b - m_f) * (m_b - m_f);
        if var > best {
            best = var;
            best_t = t as u8;
        }
    }
    best_t
}

fn largest_component(mask: &[u8], w: usize, h: usize) -> Option<Vec<(usize, usize)>> {
    let n = w * h;
    let mut parent: Vec<usize> = (0..n).collect();
    let mut rank = vec![0u8; n];

    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if mask[i] == 0 {
                continue;
            }
            if x + 1 < w && mask[i + 1] == 1 {
                union(&mut parent, &mut rank, i, i + 1);
            }
            if y + 1 < h && mask[i + w] == 1 {
                union(&mut parent, &mut rank, i, i + w);
            }
        }
    }

    let mut sizes = vec![0usize; n];
    for (i, &m) in mask.iter().enumerate() {
        if m == 1 {
            let r = find(&mut parent, i);
            sizes[r] += 1;
        }
    }
    let mut root = usize::MAX;
    let mut best = 0usize;
    for (i, &s) in sizes.iter().enumerate() {
        if s > best {
            best = s;
            root = i;
        }
    }
    if best < MIN_COMPONENT {
        return None;
    }
    let mut comp = Vec::with_capacity(best);
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if mask[i] == 1 && find(&mut parent, i) == root {
                comp.push((x, y));
            }
        }
    }
    Some(comp)
}

fn find(parent: &mut [usize], mut x: usize) -> usize {
    while parent[x] != x {
        parent[x] = parent[parent[x]];
        x = parent[x];
    }
    x
}

fn union(parent: &mut [usize], rank: &mut [u8], a: usize, b: usize) {
    let ra = find(parent, a);
    let rb = find(parent, b);
    if ra == rb {
        return;
    }
    if rank[ra] < rank[rb] {
        parent[ra] = rb;
    } else if rank[ra] > rank[rb] {
        parent[rb] = ra;
    } else {
        parent[rb] = ra;
        rank[ra] += 1;
    }
}

fn convex_hull(points: &mut [(i32, i32)]) -> Vec<(i32, i32)> {
    if points.len() < 3 {
        return points.to_owned();
    }
    points.sort_unstable();
    let mut lower: Vec<(i32, i32)> = Vec::new();
    for &p in points.iter() {
        while lower.len() >= 2 && cross_i(lower[lower.len() - 2], lower[lower.len() - 1], p) <= 0 {
            lower.pop();
        }
        lower.push(p);
    }
    let mut upper: Vec<(i32, i32)> = Vec::new();
    for &p in points.iter().rev() {
        while upper.len() >= 2 && cross_i(upper[upper.len() - 2], upper[upper.len() - 1], p) <= 0 {
            upper.pop();
        }
        upper.push(p);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

fn cross_i(o: (i32, i32), a: (i32, i32), b: (i32, i32)) -> i64 {
    (a.0 as i64 - o.0 as i64) * (b.1 as i64 - o.1 as i64)
        - (a.1 as i64 - o.1 as i64) * (b.0 as i64 - o.0 as i64)
}

fn simplify_to_4(hull: &[(f32, f32)]) -> Vec<(f32, f32)> {
    let mut poly = hull.to_vec();
    while poly.len() > 4 {
        let n = poly.len();
        let mut best = usize::MAX;
        let mut best_area = f32::MAX;
        for i in 0..n {
            let prev = poly[(i + n - 1) % n];
            let cur = poly[i];
            let next = poly[(i + 1) % n];
            let area = triangle_area(prev, cur, next);
            if area < best_area {
                best_area = area;
                best = i;
            }
        }
        if best == usize::MAX {
            break;
        }
        poly.remove(best);
    }
    poly
}

fn triangle_area(a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> f32 {
    let ab = (b.0 - a.0, b.1 - a.1);
    let ac = (c.0 - a.0, c.1 - a.1);
    (ab.0 * ac.1 - ab.1 * ac.0).abs() * 0.5
}

fn order_corners(c: &[(f32, f32); 4]) -> [(f32, f32); 4] {
    let mut tl = 0;
    let mut tr = 0;
    let mut br = 0;
    let mut bl = 0;
    for i in 1..4 {
        if c[i].0 + c[i].1 < c[tl].0 + c[tl].1 {
            tl = i;
        }
        if c[i].0 + c[i].1 > c[br].0 + c[br].1 {
            br = i;
        }
        if c[i].0 - c[i].1 > c[tr].0 - c[tr].1 {
            tr = i;
        }
        if c[i].0 - c[i].1 < c[bl].0 - c[bl].1 {
            bl = i;
        }
    }
    [c[tl], c[tr], c[br], c[bl]]
}

fn validate(c: &[(f32, f32); 4], dw: f32, dh: f32) -> bool {
    for &(x, y) in c {
        if x < 0.0 || y < 0.0 || x > dw || y > dh {
            return false;
        }
    }
    let area = polygon_area(c);
    let ratio = area / (dw * dh);
    if !(AREA_MIN..=AREA_MAX).contains(&ratio) {
        return false;
    }
    let top = dist(c[0], c[1]);
    let bottom = dist(c[2], c[3]);
    let left = dist(c[0], c[3]);
    let right = dist(c[1], c[2]);
    let width = (top + bottom) / 2.0;
    let height = (left + right) / 2.0;
    let aspect = width / height.max(1.0);
    if !(ASPECT_MIN..=ASPECT_MAX).contains(&aspect) {
        return false;
    }
    let s1 = cross_f(c[0], c[1], c[2]);
    let s2 = cross_f(c[1], c[2], c[3]);
    let s3 = cross_f(c[2], c[3], c[0]);
    let s4 = cross_f(c[3], c[0], c[1]);
    let sign = s1.signum();
    if sign == 0.0 || s2.signum() != sign || s3.signum() != sign || s4.signum() != sign {
        return false;
    }
    true
}

fn polygon_area(c: &[(f32, f32); 4]) -> f32 {
    let mut s = 0.0;
    for i in 0..4 {
        let j = (i + 1) % 4;
        s += c[i].0 * c[j].1 - c[j].0 * c[i].1;
    }
    s.abs() * 0.5
}

fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
    let dx = a.0 - b.0;
    let dy = a.1 - b.1;
    (dx * dx + dy * dy).sqrt()
}

fn cross_f(o: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
}

fn homography(src: &[(f32, f32); 4], dst: &[(f32, f32); 4]) -> [f32; 9] {
    let mut a = [[0f32; 9]; 8];
    for i in 0..4 {
        let (x, y) = src[i];
        let (xp, yp) = dst[i];
        a[2 * i] = [x, y, 1.0, 0.0, 0.0, 0.0, -xp * x, -xp * y, xp];
        a[2 * i + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -yp * x, -yp * y, yp];
    }
    let h = solve_gauss(a);
    [h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7], 1.0]
}

fn solve_gauss(mut a: [[f32; 9]; 8]) -> [f32; 8] {
    for col in 0..8 {
        let mut piv = col;
        for r in (col + 1)..8 {
            if a[r][col].abs() > a[piv][col].abs() {
                piv = r;
            }
        }
        if a[piv][col].abs() < 1e-9 {
            continue;
        }
        a.swap(col, piv);
        let d = a[col][col];
        for v in &mut a[col][col..] {
            *v /= d;
        }
        let pivot = a[col];
        for (r, row) in a.iter_mut().enumerate() {
            if r == col {
                continue;
            }
            let f = row[col];
            for (c, &p) in pivot.iter().enumerate().skip(col) {
                row[c] -= f * p;
            }
        }
    }
    let mut x = [0f32; 8];
    for i in 0..8 {
        x[i] = a[i][8];
    }
    x
}

fn warp(img: &image::DynamicImage, corners: &[(f32, f32); 4]) -> image::DynamicImage {
    let top = dist(corners[0], corners[1]);
    let bottom = dist(corners[2], corners[3]);
    let left = dist(corners[0], corners[3]);
    let right = dist(corners[1], corners[2]);
    let dw = ((top + bottom) / 2.0).round() as u32;
    let dh = ((left + right) / 2.0).round() as u32;
    let dw = dw.clamp(1, 32768);
    let dh = dh.clamp(1, 32768);

    let dst = [
        (0.0f32, 0.0f32),
        (dw as f32, 0.0),
        (dw as f32, dh as f32),
        (0.0, dh as f32),
    ];
    let h = homography(&dst, corners);

    let src_rgb = img.to_rgb8();
    let (sw, sh) = (src_rgb.width() as i32, src_rgb.height() as i32);
    let src = src_rgb.as_raw();
    let stride3 = 3 * (sw as usize);

    let mut out = vec![0u8; (dw as usize) * (dh as usize) * 3];
    out.par_chunks_mut((dw as usize) * 3).enumerate().for_each(|(y, row)| {
        let fy = y as f32;
        let row_nx = h[1] * fy + h[2];
        let row_ny = h[4] * fy + h[5];
        let row_denom = h[7] * fy + h[8];

        for x in 0..dw as usize {
            let fx = x as f32;
            let inv = 1.0 / (h[6] * fx + row_denom);
            let sx = ((h[0] * fx + row_nx) * inv).clamp(0.0, (sw - 1) as f32);
            let sy = ((h[3] * fx + row_ny) * inv).clamp(0.0, (sh - 1) as f32);

            let xi = (sx.floor() as i32).clamp(0, sw - 2);
            let yi = (sy.floor() as i32).clamp(0, sh - 2);
            let tx = (sx - xi as f32).clamp(0.0, 1.0);
            let ty = (sy - yi as f32).clamp(0.0, 1.0);
            let tx1 = 1.0 - tx;
            let ty1 = 1.0 - ty;

            let i00 = ((yi * sw + xi) * 3) as usize;
            let i01 = i00 + stride3;
            for c in 0..3 {
                let v = src[i00 + c] as f32 * tx1 * ty1
                    + src[i00 + 3 + c] as f32 * tx * ty1
                    + src[i01 + c] as f32 * tx1 * ty
                    + src[i01 + 3 + c] as f32 * tx * ty;
                row[x * 3 + c] = v.round() as u8;
            }
        }
    });

    image::DynamicImage::ImageRgb8(image::RgbImage::from_raw(dw, dh, out).unwrap())
}
