use rayon::prelude::*;

const DETECT_LONG_EDGE: u32 = 512;
const AREA_MIN: f32 = 0.12;
const AREA_MAX: f32 = 0.95;
const ASPECT_MIN: f32 = 0.4;
const ASPECT_MAX: f32 = 2.5;
const MIN_COMPONENT: usize = 64;

const COMP_MIN_FRAC: usize = 600;
const MAX_HYPOTHESES: usize = 96;
const EDGE_TOP_COMPONENTS: usize = 8;
const EDGE_DILATE_RX: usize = 2;
const EDGE_DILATE_RY: usize = 2;
const APPROX_EPS: [f32; 3] = [0.012, 0.02, 0.032];
const EDGE_SUP_SAMPLES: usize = 32;
const EDGE_SUP_OFFSETS: [f32; 3] = [-1.0, 0.0, 1.0];
const EDGE_SUP_STRONG_FRAC: f32 = 0.35;
const EDGE_SUP_RUN_MIN: usize = 16;
const EDGE_SUP_FRAC_MIN: usize = 22;
const FRAME_EPS: f32 = 3.0;
const CANNY_HI_FRAC: f32 = 0.90;
const CANNY_HI_FLOOR: u8 = 24;
const CANNY_LO_RATIO: f32 = 0.40;
const STEP_K: [i32; 3] = [3, 6, 10];
const STEP_SHIFT: i32 = 4;
const RING_OFF: i32 = 8;
const RING_SAMPLES_PER_SIDE: usize = 24;
const REGION_CONTRAST_FLOOR: f32 = 0.25;
const REGION_CONTRAST_W_DEFAULT: f32 = 0.75;
const AREA_PRIOR_PEAK: f32 = 0.30;
const AREA_PRIOR_BELOW: f32 = 1.0;
const AREA_PRIOR_ABOVE: f32 = 0.10;
const TIEBREAK_REL_DEFAULT: f32 = 0.90;
const SCORE_MODE_ENV: &str = "SEAMAESTRO_CROP_SCORE";
const STEP_THR_ENV: &str = "SEAMAESTRO_CROP_STEP_THR";
const CONTRAST_W_ENV: &str = "SEAMAESTRO_CROP_CONTRAST_W";
const TIEBREAK_REL_ENV: &str = "SEAMAESTRO_CROP_TIEBREAK";
const WARP_RES_TOL_PX: f32 = 1.0;

pub(crate) fn deskew(img: image::DynamicImage) -> image::DynamicImage {
    let (w, h) = (img.width(), img.height());
    if w < 16 || h < 16 {
        return img;
    }
    let corners = match detect_corners(&img, w, h) {
        Some(c) => c,
        None => return img,
    };
    warp(&img, &corners).unwrap_or(img)
}

struct Hypothesis {
    quad: [(f32, f32); 4],
    area_ratio: f32,
    ok_edges: u32,
    frame_touch: bool,
    score: f32,
    source: &'static str,
    step_ok: u32,
    ring_fg: f32,
    ring_bg: f32,
}

struct VerifyCtx<'a> {
    luma: &'a [u8],
    strong: u8,
    otsu: u8,
    step_thr: u8,
    contrast_w: f32,
    enabled: bool,
}

struct Scratch {
    mask: Vec<u8>,
    edge: Vec<u8>,
    tmp: Vec<u8>,
    closed: Vec<u8>,
    parent: Vec<u32>,
    rank: Vec<u8>,
    area: Vec<u32>,
    seed: Vec<u32>,
    visited: Vec<u8>,
    contour: Vec<(i32, i32)>,
    poly: Vec<(f32, f32)>,
    keep: Vec<bool>,
    pstack: Vec<(usize, usize)>,
    stack: Vec<(usize, usize)>,
}

impl Scratch {
    fn new(n: usize) -> Self {
        Self {
            mask: vec![0u8; n],
            edge: vec![0u8; n],
            tmp: vec![0u8; n],
            closed: vec![0u8; n],
            parent: vec![0u32; n],
            rank: vec![0u8; n],
            area: vec![0u32; n],
            seed: vec![0u32; n],
            visited: vec![0u8; n],
            contour: Vec::with_capacity(4096),
            poly: Vec::with_capacity(4096),
            keep: Vec::with_capacity(4096),
            pstack: Vec::with_capacity(1024),
            stack: Vec::with_capacity(4096),
        }
    }
}

fn crop_debug() -> bool {
    match std::env::var("SEAMAESTRO_CROP_DEBUG") {
        Ok(v) => !v.is_empty() && v != "0",
        Err(_) => false,
    }
}

fn region_score_enabled() -> bool {
    match std::env::var(SCORE_MODE_ENV) {
        Ok(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "region" | "new" | "1"
        ),
        Err(_) => false,
    }
}

fn env_u8(key: &str, default: u8) -> u8 {
    match std::env::var(key) {
        Ok(v) => v.trim().parse::<u8>().unwrap_or(default),
        Err(_) => default,
    }
}

fn env_f32(key: &str, default: f32) -> f32 {
    match std::env::var(key) {
        Ok(v) => v.trim().parse::<f32>().unwrap_or(default),
        Err(_) => default,
    }
}

fn detect_corners(img: &image::DynamicImage, w: u32, h: u32) -> Option<[(f32, f32); 4]> {
    let long = w.max(h);
    let scale = (DETECT_LONG_EDGE as f32 / long as f32).min(1.0);
    let dw = ((w as f32 * scale).round() as u32).max(2);
    let dh = ((h as f32 * scale).round() as u32).max(2);

    let small = img
        .resize_exact(dw, dh, image::imageops::FilterType::Triangle)
        .to_luma8();
    let (dwi, dhi) = (dw as usize, dh as usize);
    let n = dwi * dhi;
    let luma = small.as_raw();

    let sharp = box_blur_gray(luma, dwi, dhi, 0);
    let soft = box_blur_gray(luma, dwi, dhi, 1);

    let mag_sharp = sobel_l1(&sharp, dwi, dhi);
    let mag_soft = sobel_l1(&soft, dwi, dhi);
    let strong = strong_threshold(&mag_sharp);

    let vctx = VerifyCtx {
        luma: &soft,
        strong,
        otsu: otsu(&hist256(&soft)),
        step_thr: env_u8(STEP_THR_ENV, strong.max(6)),
        contrast_w: env_f32(CONTRAST_W_ENV, REGION_CONTRAST_W_DEFAULT),
        enabled: region_score_enabled(),
    };

    let mut sc = Scratch::new(n);
    let mut hyp: Vec<Hypothesis> = Vec::with_capacity(MAX_HYPOTHESES);

    detect_by_edges(&mag_soft, &mag_sharp, dwi, dhi, &mut sc, &mut hyp, &vctx);
    detect_by_threshold(&soft, &mag_sharp, dwi, dhi, &mut sc, &mut hyp, &vctx);

    if hyp.is_empty() {
        if crop_debug() {
            eprintln!(
                "  [crop] proxy {}x{} img={}x{} strong={} hypotheses=0",
                dwi, dhi, w, h, strong
            );
        }
        return None;
    }

    hyp.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    if crop_debug() {
        eprintln!(
            "  [crop] proxy {}x{} img={}x{} strong={} hypotheses={}",
            dwi,
            dhi,
            w,
            h,
            strong,
            hyp.len()
        );
        if vctx.enabled {
            eprintln!(
                "  [crop] mode=region otsu={} step_thr={} contrast_w={:.2}",
                vctx.otsu, vctx.step_thr, vctx.contrast_w
            );
        }
        let dbg_inv_scale = 1.0 / scale;
        for (i, c) in hyp.iter().take(12).enumerate() {
            let q = c.quad;
            let quad = format!(
                "quad=[{:.1},{:.1};{:.1},{:.1};{:.1},{:.1};{:.1},{:.1}]",
                q[0].0 * dbg_inv_scale,
                q[0].1 * dbg_inv_scale,
                q[1].0 * dbg_inv_scale,
                q[1].1 * dbg_inv_scale,
                q[2].0 * dbg_inv_scale,
                q[2].1 * dbg_inv_scale,
                q[3].0 * dbg_inv_scale,
                q[3].1 * dbg_inv_scale
            );
            if vctx.enabled {
                eprintln!(
                    "  [crop]   #{} {:<6} area={:.3} edges={} frame={} score={:.4} step={}/4 fg={:.2} bg={:.2} {}",
                    i,
                    c.source,
                    c.area_ratio,
                    c.ok_edges,
                    u8::from(c.frame_touch),
                    c.score,
                    c.step_ok,
                    c.ring_fg,
                    c.ring_bg,
                    quad
                );
            } else {
                eprintln!(
                    "  [crop]   #{} {:<6} area={:.3} edges={} frame={} score={:.4} {}",
                    i,
                    c.source,
                    c.area_ratio,
                    c.ok_edges,
                    u8::from(c.frame_touch),
                    c.score,
                    quad
                );
            }
        }
    }

    let inv_scale = 1.0 / scale;
    let pick = if vctx.enabled {
        let tol = hyp[0].score * env_f32(TIEBREAK_REL_ENV, TIEBREAK_REL_DEFAULT);
        hyp.iter()
            .filter(|c| c.score >= tol)
            .max_by(|a, b| {
                a.area_ratio
                    .partial_cmp(&b.area_ratio)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|c| c.quad)
            .unwrap_or(hyp[0].quad)
    } else {
        hyp[0].quad
    };
    Some(pick.map(|(x, y)| (x * inv_scale, y * inv_scale)))
}

fn detect_by_edges(
    mag_soft: &[u8],
    mag_sharp: &[u8],
    w: usize,
    h: usize,
    sc: &mut Scratch,
    out: &mut Vec<Hypothesis>,
    vctx: &VerifyCtx,
) {
    let n = w * h;
    let hi = hist_pct(&hist256(mag_soft), CANNY_HI_FRAC).max(CANNY_HI_FLOOR);
    let lo = ((hi as f32) * CANNY_LO_RATIO) as u8;
    let dbg = crop_debug();
    canny_hysteresis(mag_soft, w, h, lo, hi, &mut sc.edge, &mut sc.stack);
    let mask_px = sc.edge.iter().filter(|&&v| v != 0).count();
    dilate_rect(
        &sc.edge,
        w,
        h,
        EDGE_DILATE_RX,
        EDGE_DILATE_RY,
        &mut sc.tmp,
        &mut sc.closed,
    );
    let comps = label_components(
        &sc.closed,
        w,
        h,
        &mut sc.parent,
        &mut sc.rank,
        &mut sc.area,
        &mut sc.seed,
    );
    if dbg {
        eprintln!(
            "  [crop] edge hi={} lo={} mask_px={} comps={}",
            hi,
            lo,
            mask_px,
            comps.len()
        );
    }
    sc.visited.clear();
    sc.visited.resize(n, 0);
    for &(size, root) in comps.iter().take(EDGE_TOP_COMPONENTS) {
        let seed = sc.seed[root as usize];
        if seed == u32::MAX {
            continue;
        }
        let s = seed as usize;
        let before = out.len();
        trace_contour(
            &sc.closed,
            w,
            h,
            ((s % w) as i32, (s / w) as i32),
            &mut sc.visited,
            &mut sc.contour,
        );
        if sc.contour.len() >= 8 {
            push_poly_hypotheses("edge", w, h, mag_sharp, sc, out, vctx, false);
        }
        if dbg {
            eprintln!(
                "  [crop] edge comp size={} contour={} pushed={}",
                size,
                sc.contour.len(),
                out.len() - before
            );
        }
    }
}

fn detect_by_threshold(
    soft: &[u8],
    mag_sharp: &[u8],
    w: usize,
    h: usize,
    sc: &mut Scratch,
    out: &mut Vec<Hypothesis>,
    vctx: &VerifyCtx,
) {
    let n = w * h;
    let base_t = otsu(&hist256(soft)) as i32;
    let thresholds = [
        base_t,
        base_t - 30,
        base_t + 30,
        base_t - 60,
        base_t + 60,
        128,
        96,
        160,
        64,
        192,
    ];
    let min_px = (n / COMP_MIN_FRAC).max(MIN_COMPONENT);
    let dbg = crop_debug();

    for &t_raw in &thresholds {
        let t = t_raw.clamp(0, 255) as u8;
        for (i, &v) in soft.iter().enumerate() {
            sc.mask[i] = u8::from(v > t);
        }
        morph_close_into(&sc.mask, w, h, &mut sc.tmp, &mut sc.closed);
        let comps = label_components(
            &sc.closed,
            w,
            h,
            &mut sc.parent,
            &mut sc.rank,
            &mut sc.area,
            &mut sc.seed,
        );
        let before = out.len();
        let top = comps.first().map(|c| c.0).unwrap_or(0);
        let mut contour_len = 0usize;
        if let Some(&(count, root)) = comps.first() {
            if (count as usize) >= min_px {
                let seed = sc.seed[root as usize];
                if seed != u32::MAX {
                    let s = seed as usize;
                    sc.visited.clear();
                    sc.visited.resize(n, 0);
                    trace_contour(
                        &sc.closed,
                        w,
                        h,
                        ((s % w) as i32, (s / w) as i32),
                        &mut sc.visited,
                        &mut sc.contour,
                    );
                    contour_len = sc.contour.len();
                    if contour_len >= 8 {
                        push_poly_hypotheses("sweep", w, h, mag_sharp, sc, out, vctx, false);
                    }
                }
            }
        }
        if dbg {
            eprintln!(
                "  [crop] sweep t={:<3} top={:<7} min={:<6} contour={:<6} pushed={}",
                t,
                top,
                min_px,
                contour_len,
                out.len() - before
            );
        }
    }
    if crop_debug() {
        detect_dark_diagnostics(soft, mag_sharp, w, h, sc, out, vctx);
    }
}

fn detect_dark_diagnostics(
    soft: &[u8],
    mag_sharp: &[u8],
    w: usize,
    h: usize,
    sc: &mut Scratch,
    out: &mut Vec<Hypothesis>,
    vctx: &VerifyCtx,
) {
    let n = w * h;
    let base_t = otsu(&hist256(soft)) as i32;
    let mut thresholds: Vec<i32> = (96..=200).step_by(4).collect();
    thresholds.extend_from_slice(&[
        base_t,
        base_t - 30,
        base_t + 30,
        base_t - 60,
        base_t + 60,
        192,
    ]);
    let min_px = (n / COMP_MIN_FRAC).max(MIN_COMPONENT);
    for &t_raw in &thresholds {
        let t = t_raw.clamp(0, 255) as u8;
        for (i, &v) in soft.iter().enumerate() {
            sc.mask[i] = u8::from(v < t);
        }
        morph_close_into(&sc.mask, w, h, &mut sc.tmp, &mut sc.closed);
        let comps = label_components(
            &sc.closed,
            w,
            h,
            &mut sc.parent,
            &mut sc.rank,
            &mut sc.area,
            &mut sc.seed,
        );
        let top = comps.first().map(|c| c.0).unwrap_or(0);
        let mut contour_len = 0usize;
        if let Some(&(count, root)) = comps.first() {
            if (count as usize) >= min_px {
                let seed = sc.seed[root as usize];
                if seed != u32::MAX {
                    let s = seed as usize;
                    sc.visited.clear();
                    sc.visited.resize(n, 0);
                    trace_contour(
                        &sc.closed,
                        w,
                        h,
                        ((s % w) as i32, (s / w) as i32),
                        &mut sc.visited,
                        &mut sc.contour,
                    );
                    contour_len = sc.contour.len();
                    if contour_len >= 8 {
                        push_poly_hypotheses("dark", w, h, mag_sharp, sc, out, vctx, true);
                    }
                }
            }
        }
        eprintln!(
            "  [crop] dark t={:<3} top={:<7} min={:<6} contour={:<6}",
            t, top, min_px, contour_len
        );
    }
}

fn push_poly_hypotheses(
    source: &'static str,
    w: usize,
    h: usize,
    mag: &[u8],
    sc: &mut Scratch,
    out: &mut Vec<Hypothesis>,
    vctx: &VerifyCtx,
    dry: bool,
) {
    let per = contour_perimeter(&sc.contour);
    if per < 8.0 {
        return;
    }
    let dbg = crop_debug();
    let mut eps4 = 0usize;

    for &eps_r in APPROX_EPS.iter() {
        let eps = per * eps_r;
        approx_polygon(&sc.contour, eps, &mut sc.keep, &mut sc.pstack, &mut sc.poly);
        let raw_n = sc.poly.len();
        let mut poly = sc.poly.clone();
        dedup_closed(&mut poly, eps.max(1.0));
        let dedup_n = poly.len();
        if poly.len() > 4 && poly.len() <= 7 {
            poly = simplify_to_4(&poly);
        }
        if dbg {
            let dmin = if poly.len() == 4 {
                min_pair_dist(&[poly[0], poly[1], poly[2], poly[3]])
            } else {
                0.0
            };
            let pts: String = poly
                .iter()
                .map(|p| format!("{:.2},{:.2};", p.0, p.1))
                .collect();
            eprintln!(
                "  [crop] poly {} {} stage=eps eps_r={} per={:.1} raw={} dedup={} n={} dmin={:.4} pts=[{}]",
                source,
                if dry { "dry" } else { "wet" },
                eps_r,
                per,
                raw_n,
                dedup_n,
                poly.len(),
                dmin,
                pts
            );
        }
        if poly.len() == 4 {
            eps4 += 1;
            let q = order_corners(&[poly[0], poly[1], poly[2], poly[3]]);
            push_quad(q, source, w, h, mag, out, vctx, dry);
        }
    }

    let mut ip: Vec<(i32, i32)> = sc.contour.clone();
    let hull = convex_hull(&mut ip);
    if dbg {
        eprintln!(
            "  [crop] poly {} {} stage=hull eps4={} hull_n={}",
            source,
            if dry { "dry" } else { "wet" },
            eps4,
            hull.len()
        );
    }
    if hull.len() < 4 {
        return;
    }
    let hull_f: Vec<(f32, f32)> = hull.iter().map(|&(x, y)| (x as f32, y as f32)).collect();
    let corners = simplify_to_4(&hull_f);
    if dbg {
        let pts: String = corners
            .iter()
            .map(|p| format!("{:.2},{:.2};", p.0, p.1))
            .collect();
        let dmin = if corners.len() == 4 {
            min_pair_dist(&[corners[0], corners[1], corners[2], corners[3]])
        } else {
            0.0
        };
        eprintln!(
            "  [crop] poly {} {} stage=hull4 n={} dmin={:.4} pts=[{}]",
            source,
            if dry { "dry" } else { "wet" },
            corners.len(),
            dmin,
            pts
        );
    }
    if corners.len() == 4 {
        let q = order_corners(&[corners[0], corners[1], corners[2], corners[3]]);
        push_quad(q, source, w, h, mag, out, vctx, dry);
    }
    if let Some(q) = min_area_rect(&hull_f) {
        if dbg {
            let pts: String = q.iter().map(|p| format!("{:.2},{:.2};", p.0, p.1)).collect();
            eprintln!(
                "  [crop] poly {} {} stage=rect dmin={:.4} pts=[{}]",
                source,
                if dry { "dry" } else { "wet" },
                min_pair_dist(&q),
                pts
            );
        }
        push_quad(q, source, w, h, mag, out, vctx, dry);
    }
}

fn push_quad(
    quad: [(f32, f32); 4],
    source: &'static str,
    w: usize,
    h: usize,
    mag: &[u8],
    out: &mut Vec<Hypothesis>,
    vctx: &VerifyCtx,
    dry: bool,
) {
    if !dry && out.len() >= MAX_HYPOTHESES {
        return;
    }
    if !validate(&quad, w as f32, h as f32) {
        if crop_debug() {
            if let Some(reason) = validate_reason(&quad, w as f32, h as f32) {
                let pts: String = quad
                    .iter()
                    .map(|p| format!("{:.2},{:.2};", p.0, p.1))
                    .collect();
                let dmin = min_pair_dist(&quad);
                let a = polygon_area(&quad) / (w * h) as f32;
                if dry {
                    eprintln!(
                        "  [crop]   reject dry {:<7} area={:.3} dmin={:.4} src={} pts=[{}]",
                        reason, a, dmin, source, pts
                    );
                } else {
                    eprintln!(
                        "  [crop]   reject {:<7} area={:.3} dmin={:.4} src={} pts=[{}]",
                        reason, a, dmin, source, pts
                    );
                }
            }
        }
        return;
    }
    let area_ratio = polygon_area(&quad) / (w * h) as f32;
    let (ok_edges, _) = edge_stats(&quad, mag, w, h, vctx.strong);
    let frame_touch = frame_hugging(&quad, w as f32, h as f32);
    let mut step_ok = 0u32;
    let mut ring_fg = 0.0f32;
    let mut ring_bg = 0.0f32;
    let mut score = 0.05 + 0.30 * (ok_edges as f32 / 4.0);
    if vctx.enabled {
        let (s_ok, s_stats) = step_stats(&quad, vctx.luma, w, h, vctx.step_thr);
        let (fg, bg, contrast) = ring_exterior_contrast(&quad, vctx.luma, w, h, vctx.otsu);
        step_ok = s_ok;
        ring_fg = fg;
        ring_bg = bg;
        let support: f32 = s_stats
            .iter()
            .map(|&(cnt, _)| (cnt as f32 / EDGE_SUP_SAMPLES as f32).min(1.0))
            .sum::<f32>()
            / 4.0;
        score = 0.05 + 0.30 * support;
        score *= (REGION_CONTRAST_FLOOR + vctx.contrast_w * contrast.clamp(0.0, 1.0)).clamp(0.05, 1.0);
        let area_prior = if area_ratio < AREA_PRIOR_PEAK {
            1.0 - (AREA_PRIOR_PEAK - area_ratio) * AREA_PRIOR_BELOW
        } else {
            1.0 - (area_ratio - AREA_PRIOR_PEAK) * AREA_PRIOR_ABOVE
        };
        score *= area_prior.clamp(0.15, 1.0);
    } else {
        score *= (1.0 - (area_ratio - 0.55).abs() * 0.6).max(0.15);
    }
    if frame_touch {
        score *= 0.25;
    }
    if dry {
        if crop_debug() {
            if vctx.enabled {
                eprintln!(
                    "  [crop]   {:<7} area={:.3} edges={} frame={} score={:.4} step={}/4 fg={:.2} bg={:.2} quad=[{:.1},{:.1};{:.1},{:.1};{:.1},{:.1};{:.1},{:.1}]",
                    source,
                    area_ratio,
                    ok_edges,
                    u8::from(frame_touch),
                    score,
                    step_ok,
                    ring_fg,
                    ring_bg,
                    quad[0].0,
                    quad[0].1,
                    quad[1].0,
                    quad[1].1,
                    quad[2].0,
                    quad[2].1,
                    quad[3].0,
                    quad[3].1
                );
            } else {
                eprintln!(
                    "  [crop]   {:<7} area={:.3} edges={} frame={} score={:.4} quad=[{:.1},{:.1};{:.1},{:.1};{:.1},{:.1};{:.1},{:.1}]",
                    source,
                    area_ratio,
                    ok_edges,
                    u8::from(frame_touch),
                    score,
                    quad[0].0,
                    quad[0].1,
                    quad[1].0,
                    quad[1].1,
                    quad[2].0,
                    quad[2].1,
                    quad[3].0,
                    quad[3].1
                );
            }
        }
        return;
    }
    out.push(Hypothesis {
        quad,
        area_ratio,
        ok_edges,
        frame_touch,
        score,
        source,
        step_ok,
        ring_fg,
        ring_bg,
    });
}

fn frame_hugging(q: &[(f32, f32); 4], w: f32, h: f32) -> bool {
    let mut hits = 0;
    for i in 0..4 {
        let a = q[i];
        let b = q[(i + 1) & 3];
        let left = a.0 <= FRAME_EPS && b.0 <= FRAME_EPS;
        let right = a.0 >= w - FRAME_EPS && b.0 >= w - FRAME_EPS;
        let top = a.1 <= FRAME_EPS && b.1 <= FRAME_EPS;
        let bottom = a.1 >= h - FRAME_EPS && b.1 >= h - FRAME_EPS;
        if left || right || top || bottom {
            hits += 1;
        }
    }
    hits >= 3
}

fn edge_stats(
    q: &[(f32, f32); 4],
    mag: &[u8],
    w: usize,
    h: usize,
    strong: u8,
) -> (u32, [(usize, usize); 4]) {
    let mut ok = 0u32;
    let mut stats = [(0usize, 0usize); 4];
    for e in 0..4 {
        let a = q[e];
        let b = q[(e + 1) & 3];
        let dx = b.0 - a.0;
        let dy = b.1 - a.1;
        let len = (dx * dx + dy * dy).sqrt().max(1e-3);
        let nx = -dy / len;
        let ny = dx / len;
        let sx = dx / EDGE_SUP_SAMPLES as f32;
        let sy = dy / EDGE_SUP_SAMPLES as f32;
        let mut cnt = 0usize;
        let mut run = 0usize;
        let mut best_run = 0usize;
        for s in 0..EDGE_SUP_SAMPLES {
            let px = a.0 + sx * (s as f32 + 0.5);
            let py = a.1 + sy * (s as f32 + 0.5);
            let mut v = 0u8;
            for &o in EDGE_SUP_OFFSETS.iter() {
                let x = px + nx * o;
                let y = py + ny * o;
                if x < 0.0 || y < 0.0 || x >= w as f32 || y >= h as f32 {
                    continue;
                }
                let m = mag[y as usize * w + x as usize];
                if m > v {
                    v = m;
                }
            }
            if v >= strong {
                cnt += 1;
                run += 1;
                if run > best_run {
                    best_run = run;
                }
            } else {
                run = 0;
            }
        }
        stats[e] = (cnt, best_run);
        if cnt >= EDGE_SUP_FRAC_MIN && best_run >= EDGE_SUP_RUN_MIN {
            ok += 1;
        }
    }
    (ok, stats)
}

fn sample_luma(luma: &[u8], w: usize, h: usize, x: f32, y: f32) -> Option<u8> {
    if x < 0.0 || y < 0.0 || x >= w as f32 || y >= h as f32 {
        return None;
    }
    Some(luma[y as usize * w + x as usize])
}

fn side_plateau_support(
    a: (f32, f32),
    b: (f32, f32),
    luma: &[u8],
    w: usize,
    h: usize,
    thr: u8,
) -> (usize, usize) {
    let dx = b.0 - a.0;
    let dy = b.1 - a.1;
    let len = (dx * dx + dy * dy).sqrt().max(1e-3);
    let nx = -dy / len;
    let ny = dx / len;
    let sx = dx / EDGE_SUP_SAMPLES as f32;
    let sy = dy / EDGE_SUP_SAMPLES as f32;
    let mut cnt = 0usize;
    let mut run = 0usize;
    let mut best_run = 0usize;
    for s in 0..EDGE_SUP_SAMPLES {
        let px = a.0 + sx * (s as f32 + 0.5);
        let py = a.1 + sy * (s as f32 + 0.5);
        let mut best = 0u8;
        for t in -STEP_SHIFT..=STEP_SHIFT {
            let tf = t as f32;
            for k in STEP_K.iter() {
                let kf = *k as f32;
                let m = sample_luma(luma, w, h, px + nx * (tf - kf), py + ny * (tf - kf));
                let p = sample_luma(luma, w, h, px + nx * (tf + kf), py + ny * (tf + kf));
                if let (Some(m), Some(p)) = (m, p) {
                    let d = m.abs_diff(p);
                    if d > best {
                        best = d;
                    }
                }
            }
        }
        if best >= thr {
            cnt += 1;
            run += 1;
            if run > best_run {
                best_run = run;
            }
        } else {
            run = 0;
        }
    }
    (cnt, best_run)
}

fn step_stats(
    q: &[(f32, f32); 4],
    luma: &[u8],
    w: usize,
    h: usize,
    thr: u8,
) -> (u32, [(usize, usize); 4]) {
    let mut ok = 0u32;
    let mut stats = [(0usize, 0usize); 4];
    for e in 0..4 {
        let (cnt, best_run) = side_plateau_support(q[e], q[(e + 1) & 3], luma, w, h, thr);
        stats[e] = (cnt, best_run);
        if cnt >= EDGE_SUP_FRAC_MIN && best_run >= EDGE_SUP_RUN_MIN {
            ok += 1;
        }
    }
    (ok, stats)
}

fn ring_exterior_contrast(
    q: &[(f32, f32); 4],
    luma: &[u8],
    w: usize,
    h: usize,
    otsu_t: u8,
) -> (f32, f32, f32) {
    let cx = (q[0].0 + q[1].0 + q[2].0 + q[3].0) * 0.25;
    let cy = (q[0].1 + q[1].1 + q[2].1 + q[3].1) * 0.25;
    let off = RING_OFF as f32;
    let mut in_hit = 0usize;
    let mut in_cnt = 0usize;
    let mut out_hit = 0usize;
    let mut out_cnt = 0usize;
    for e in 0..4 {
        let a = q[e];
        let b = q[(e + 1) & 3];
        let dx = b.0 - a.0;
        let dy = b.1 - a.1;
        let len = (dx * dx + dy * dy).sqrt().max(1e-3);
        let mut nx = -dy / len;
        let mut ny = dx / len;
        let mx = (a.0 + b.0) * 0.5;
        let my = (a.1 + b.1) * 0.5;
        if nx * (mx - cx) + ny * (my - cy) < 0.0 {
            nx = -nx;
            ny = -ny;
        }
        let sx = dx / RING_SAMPLES_PER_SIDE as f32;
        let sy = dy / RING_SAMPLES_PER_SIDE as f32;
        for s in 0..RING_SAMPLES_PER_SIDE {
            let px = a.0 + sx * (s as f32 + 0.5);
            let py = a.1 + sy * (s as f32 + 0.5);
            if let Some(v) = sample_luma(luma, w, h, px - nx * off, py - ny * off) {
                in_cnt += 1;
                if v > otsu_t {
                    in_hit += 1;
                }
            }
            if let Some(v) = sample_luma(luma, w, h, px + nx * off, py + ny * off) {
                out_cnt += 1;
                if v > otsu_t {
                    out_hit += 1;
                }
            }
        }
    }
    if in_cnt < 8 || out_cnt < 8 {
        return (0.0, 0.0, 0.0);
    }
    let fg = in_hit as f32 / in_cnt as f32;
    let bg = out_hit as f32 / out_cnt as f32;
    (fg, bg, fg - bg)
}

fn strong_threshold(mag: &[u8]) -> u8 {
    let hi = hist_pct(&hist256(mag), CANNY_HI_FRAC).max(CANNY_HI_FLOOR);
    (((hi as f32) * EDGE_SUP_STRONG_FRAC) as u8).max(6)
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

fn label_components(
    mask: &[u8],
    w: usize,
    h: usize,
    parent: &mut Vec<u32>,
    rank: &mut Vec<u8>,
    area: &mut Vec<u32>,
    seed: &mut Vec<u32>,
) -> Vec<(u32, u32)> {
    let n = w * h;
    parent.clear();
    parent.extend(0..n as u32);
    rank.clear();
    rank.resize(n, 0);
    area.clear();
    area.resize(n, 0);
    seed.clear();
    seed.resize(n, u32::MAX);

    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if mask[i] == 0 {
                continue;
            }
            if x + 1 < w && mask[i + 1] != 0 {
                union(parent, rank, i, i + 1);
            }
            if y + 1 < h {
                if mask[i + w] != 0 {
                    union(parent, rank, i, i + w);
                }
                if x > 0 && mask[i + w - 1] != 0 {
                    union(parent, rank, i, i + w - 1);
                }
                if x + 1 < w && mask[i + w + 1] != 0 {
                    union(parent, rank, i, i + w + 1);
                }
            }
        }
    }

    for (i, &m) in mask.iter().enumerate() {
        if m == 0 {
            continue;
        }
        let r = find(parent, i) as usize;
        area[r] += 1;
        if (i as u32) < seed[r] {
            seed[r] = i as u32;
        }
    }

    let mut out: Vec<(u32, u32)> = Vec::new();
    for (i, &a) in area.iter().enumerate() {
        if a > 0 {
            out.push((a, i as u32));
        }
    }
    out.sort_unstable_by_key(|b| std::cmp::Reverse(b.0));
    out
}

fn find(parent: &mut [u32], mut x: usize) -> u32 {
    while parent[x] != x as u32 {
        parent[x] = parent[parent[x] as usize];
        x = parent[x] as usize;
    }
    x as u32
}

fn union(parent: &mut [u32], rank: &mut [u8], a: usize, b: usize) {
    let ra = find(parent, a) as usize;
    let rb = find(parent, b) as usize;
    if ra == rb {
        return;
    }
    if rank[ra] < rank[rb] {
        parent[ra] = rb as u32;
    } else if rank[ra] > rank[rb] {
        parent[rb] = ra as u32;
    } else {
        parent[rb] = ra as u32;
        rank[ra] += 1;
    }
}

fn contour_perimeter(pts: &[(i32, i32)]) -> f32 {
    let n = pts.len();
    if n < 2 {
        return 0.0;
    }
    let mut sum = 0f32;
    for i in 0..n {
        let a = pts[i];
        let b = pts[(i + 1) % n];
        let dx = (b.0 - a.0) as f32;
        let dy = (b.1 - a.1) as f32;
        sum += (dx * dx + dy * dy).sqrt();
    }
    sum
}

fn dedup_closed(poly: &mut Vec<(f32, f32)>, eps: f32) {
    let mut guard = 0usize;
    let mut i = 0usize;
    while i < poly.len() && poly.len() > 3 && guard < 64 {
        let j = (i + 1) % poly.len();
        if dist(poly[i], poly[j]) < eps {
            poly.remove(j);
            guard += 1;
        } else {
            i += 1;
        }
    }
}

fn point_seg_dist(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let dx = b.0 - a.0;
    let dy = b.1 - a.1;
    let l2 = dx * dx + dy * dy;
    if l2 < 1e-9 {
        return dist(p, a);
    }
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2).clamp(0.0, 1.0);
    dist(p, (a.0 + t * dx, a.1 + t * dy))
}

fn approx_polygon(
    pts: &[(i32, i32)],
    eps: f32,
    keep: &mut Vec<bool>,
    stack: &mut Vec<(usize, usize)>,
    out: &mut Vec<(f32, f32)>,
) {
    out.clear();
    let n = pts.len();
    if n < 4 {
        for &(x, y) in pts.iter() {
            out.push((x as f32, y as f32));
        }
        return;
    }
    keep.clear();
    keep.resize(n, false);
    keep[0] = true;
    keep[n - 1] = true;
    stack.clear();
    stack.push((0, n - 1));
    while let Some((a, b)) = stack.pop() {
        if b <= a + 1 {
            continue;
        }
        let pa = (pts[a].0 as f32, pts[a].1 as f32);
        let pb = (pts[b].0 as f32, pts[b].1 as f32);
        let mut best = 0f32;
        let mut bi = a;
        for (i, &pt) in pts.iter().enumerate().take(b).skip(a + 1) {
            let d = point_seg_dist((pt.0 as f32, pt.1 as f32), pa, pb);
            if d > best {
                best = d;
                bi = i;
            }
        }
        if best > eps {
            keep[bi] = true;
            stack.push((a, bi));
            stack.push((bi, b));
        }
    }
    for i in 0..n {
        if keep[i] {
            out.push((pts[i].0 as f32, pts[i].1 as f32));
        }
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

fn trace_contour(
    mask: &[u8],
    w: usize,
    h: usize,
    start: (i32, i32),
    visited: &mut [u8],
    out: &mut Vec<(i32, i32)>,
) {
    const DX: [i32; 8] = [1, 1, 0, -1, -1, -1, 0, 1];
    const DY: [i32; 8] = [0, 1, 1, 1, 0, -1, -1, -1];
    out.clear();
    if start.0 < 0 || start.1 < 0 || start.0 >= w as i32 || start.1 >= h as i32 {
        return;
    }
    if mask[start.1 as usize * w + start.0 as usize] == 0 {
        return;
    }
    let limit = w * h;
    let mut p = start;
    let mut d_in = 0i32;
    let mut first_from: (i32, i32) = (-1, -1);
    let mut first_to: (i32, i32) = (-1, -1);
    let mut steps = 0usize;
    loop {
        let idx = p.1 as usize * w + p.0 as usize;
        if visited[idx] == 0 {
            visited[idx] = 1;
            out.push(p);
        }
        let d_back = (d_in + 4) & 7;
        let mut found = -1i32;
        for k in 1..=8 {
            let d = (d_back + k) & 7;
            let nx = p.0 + DX[d as usize];
            let ny = p.1 + DY[d as usize];
            if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                continue;
            }
            if mask[ny as usize * w + nx as usize] != 0 {
                found = d;
                break;
            }
        }
        if found < 0 {
            break;
        }
        let np = (p.0 + DX[found as usize], p.1 + DY[found as usize]);
        if first_from.0 < 0 {
            first_from = p;
            first_to = np;
        } else if p == first_from && np == first_to {
            break;
        }
        d_in = found;
        p = np;
        steps += 1;
        if steps > limit {
            break;
        }
    }
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
    validate_reason(c, dw, dh).is_none()
}

fn validate_reason(c: &[(f32, f32); 4], dw: f32, dh: f32) -> Option<&'static str> {
    for &(x, y) in c {
        if x < 0.0 || y < 0.0 || x > dw || y > dh {
            return Some("frame");
        }
    }
    let area = polygon_area(c);
    let ratio = area / (dw * dh);
    if !(AREA_MIN..=AREA_MAX).contains(&ratio) {
        return Some("area");
    }
    let top = dist(c[0], c[1]);
    let bottom = dist(c[2], c[3]);
    let left = dist(c[0], c[3]);
    let right = dist(c[1], c[2]);
    let width = (top + bottom) / 2.0;
    let height = (left + right) / 2.0;
    let aspect = width / height.max(1.0);
    if !(ASPECT_MIN..=ASPECT_MAX).contains(&aspect) {
        return Some("aspect");
    }
    let s1 = cross_f(c[0], c[1], c[2]);
    let s2 = cross_f(c[1], c[2], c[3]);
    let s3 = cross_f(c[2], c[3], c[0]);
    let s4 = cross_f(c[3], c[0], c[1]);
    let sign = s1.signum();
    if sign == 0.0 || s2.signum() != sign || s3.signum() != sign || s4.signum() != sign {
        return Some("convex");
    }
    None
}

fn polygon_area(c: &[(f32, f32); 4]) -> f32 {
    let mut s = 0.0;
    for i in 0..4 {
        let j = (i + 1) % 4;
        s += c[i].0 * c[j].1 - c[j].0 * c[i].1;
    }
    s.abs() * 0.5
}

fn min_pair_dist(c: &[(f32, f32); 4]) -> f32 {
    let mut m = f32::MAX;
    for i in 0..4 {
        for j in (i + 1)..4 {
            let d = dist(c[i], c[j]);
            if d < m {
                m = d;
            }
        }
    }
    m
}

fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
    let dx = a.0 - b.0;
    let dy = a.1 - b.1;
    (dx * dx + dy * dy).sqrt()
}

fn dilate_rect(
    src: &[u8],
    w: usize,
    h: usize,
    rx: usize,
    ry: usize,
    tmp: &mut [u8],
    dst: &mut [u8],
) {
    for y in 0..h {
        for x in 0..w {
            let x0 = x.saturating_sub(rx);
            let x1 = (x + rx).min(w - 1);
            let mut m = 0u8;
            for xx in x0..=x1 {
                let v = src[y * w + xx];
                if v > m {
                    m = v;
                }
            }
            tmp[y * w + x] = m;
        }
    }
    for y in 0..h {
        let y0 = y.saturating_sub(ry);
        let y1 = (y + ry).min(h - 1);
        for x in 0..w {
            let mut m = 0u8;
            for yy in y0..=y1 {
                let v = tmp[yy * w + x];
                if v > m {
                    m = v;
                }
            }
            dst[y * w + x] = m;
        }
    }
}

fn canny_hysteresis(
    mag: &[u8],
    w: usize,
    h: usize,
    lo: u8,
    hi: u8,
    out: &mut [u8],
    stack: &mut Vec<(usize, usize)>,
) {
    let n = w * h;
    for i in 0..n {
        out[i] = if mag[i] >= hi {
            1
        } else if mag[i] >= lo {
            2
        } else {
            0
        };
    }
    stack.clear();
    for y in 0..h {
        for x in 0..w {
            if out[y * w + x] == 1 {
                stack.push((x, y));
            }
        }
    }
    while let Some((x, y)) = stack.pop() {
        for dy in -1i32..=1 {
            for dx in -1i32..=1 {
                let nx = x as i32 + dx;
                let ny = y as i32 + dy;
                if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                    continue;
                }
                let i = ny as usize * w + nx as usize;
                if out[i] == 2 {
                    out[i] = 1;
                    stack.push((nx as usize, ny as usize));
                }
            }
        }
    }
    for v in out.iter_mut() {
        *v = u8::from(*v == 1);
    }
}

fn cross_f(o: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
}

fn hull_is_quad(c: &[(f32, f32); 4]) -> bool {
    for i in 0..4 {
        let p = c[i];
        let a = c[(i + 1) % 4];
        let b = c[(i + 2) % 4];
        let d = c[(i + 3) % 4];
        let s1 = cross_f(a, b, p);
        let s2 = cross_f(b, d, p);
        let s3 = cross_f(d, a, p);
        let nonneg = s1 >= 0.0 && s2 >= 0.0 && s3 >= 0.0;
        let nonpos = s1 <= 0.0 && s2 <= 0.0 && s3 <= 0.0;
        if nonneg || nonpos {
            return false;
        }
    }
    true
}

fn sobel_l1(src: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * h];
    if w < 3 || h < 3 {
        return out;
    }
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let i = y * w + x;
            let a = src[i - w - 1] as i32;
            let b = src[i - w] as i32;
            let c = src[i - w + 1] as i32;
            let d = src[i - 1] as i32;
            let f = src[i + 1] as i32;
            let g = src[i + w - 1] as i32;
            let hh = src[i + w] as i32;
            let k = src[i + w + 1] as i32;
            let gx = -a + c - 2 * d + 2 * f - g + k;
            let gy = -a - 2 * b - c + g + 2 * hh + k;
            out[i] = (((gx.abs() + gy.abs()) >> 3).min(255)) as u8;
        }
    }
    out
}

fn hist256(src: &[u8]) -> [u32; 256] {
    let mut hist = [0u32; 256];
    for &v in src.iter() {
        hist[v as usize] += 1;
    }
    hist
}

fn hist_pct(hist: &[u32; 256], frac: f32) -> u8 {
    let total: u64 = hist.iter().map(|&v| v as u64).sum();
    if total == 0 {
        return 0;
    }
    let goal = (total as f64 * frac as f64) as u64;
    let mut acc = 0u64;
    for (i, &v) in hist.iter().enumerate() {
        acc += v as u64;
        if acc >= goal {
            return i as u8;
        }
    }
    255
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

fn morph_close_into(src: &[u8], w: usize, h: usize, tmp: &mut [u8], dst: &mut [u8]) {
    for y in 0..h {
        for x in 0..w {
            let mut hit = false;
            'outer: for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let xx = (x as i32 + dx).clamp(0, w as i32 - 1) as usize;
                    let yy = (y as i32 + dy).clamp(0, h as i32 - 1) as usize;
                    if src[yy * w + xx] != 0 {
                        hit = true;
                        break 'outer;
                    }
                }
            }
            tmp[y * w + x] = u8::from(hit);
        }
    }
    for y in 0..h {
        for x in 0..w {
            let mut hole = false;
            'outer: for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let xx = (x as i32 + dx).clamp(0, w as i32 - 1) as usize;
                    let yy = (y as i32 + dy).clamp(0, h as i32 - 1) as usize;
                    if tmp[yy * w + xx] == 0 {
                        hole = true;
                        break 'outer;
                    }
                }
            }
            dst[y * w + x] = u8::from(!hole);
        }
    }
}

fn min_area_rect(hull: &[(f32, f32)]) -> Option<[(f32, f32); 4]> {
    let n = hull.len();
    if n < 3 {
        return None;
    }
    let mut best: Option<(f32, [(f32, f32); 4])> = None;
    for i in 0..n {
        let a = hull[i];
        let b = hull[(i + 1) % n];
        let dx = b.0 - a.0;
        let dy = b.1 - a.1;
        let len = (dx * dx + dy * dy).sqrt();
        if len < 1e-6 {
            continue;
        }
        let ux = dx / len;
        let uy = dy / len;
        let vx = -uy;
        let vy = ux;
        let mut min_u = f32::MAX;
        let mut max_u = f32::MIN;
        let mut min_v = f32::MAX;
        let mut max_v = f32::MIN;
        for &(px, py) in hull {
            let pu = px * ux + py * uy;
            let pv = px * vx + py * vy;
            min_u = min_u.min(pu);
            max_u = max_u.max(pu);
            min_v = min_v.min(pv);
            max_v = max_v.max(pv);
        }
        let area = (max_u - min_u) * (max_v - min_v);
        let corner = |u: f32, v: f32| (u * ux + v * vx, u * uy + v * vy);
        let rect = [
            corner(min_u, min_v),
            corner(max_u, min_v),
            corner(max_u, max_v),
            corner(min_u, max_v),
        ];
        if best.is_none_or(|(ba, _)| area < ba) {
            best = Some((area, rect));
        }
    }
    let rect = best.map(|(_, r)| r)?;
    Some(order_corners(&rect))
}

fn homography(from: &[(f32, f32); 4], to: &[(f32, f32); 4]) -> ([f32; 9], bool) {
    let mut a = [[0f32; 9]; 8];
    for i in 0..4 {
        let (x, y) = from[i];
        let (xp, yp) = to[i];
        a[2 * i] = [x, y, 1.0, 0.0, 0.0, 0.0, -xp * x, -xp * y, xp];
        a[2 * i + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -yp * x, -yp * y, yp];
    }
    let (h, degenerate) = solve_gauss(a);
    (
        [h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7], 1.0],
        degenerate,
    )
}

fn solve_gauss(mut a: [[f32; 9]; 8]) -> ([f32; 8], bool) {
    let mut degenerate = false;
    for col in 0..8 {
        let mut piv = col;
        for r in (col + 1)..8 {
            if a[r][col].abs() > a[piv][col].abs() {
                piv = r;
            }
        }
        if a[piv][col].abs() < 1e-9 {
            degenerate = true;
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
    (x, degenerate)
}

// Residual = max |h(from[i]) - to[i]| in SOURCE px over the 4 corners.
// It is a solver self-check, not a geometric one: 4-point DLT satisfies its own
// system exactly for ANY correspondence set, so this catches numerical failure
// (ill-conditioned f32 solve without Hartley normalization) and it CANNOT catch a
// corner-ORDER error - that is validate_reason's job (convex + sign consistent,
// a bowtie is rejected there). Rationale and measured distribution: G0_GATE.md
fn warp_residual(
    h: &[f32; 9],
    from: &[(f32, f32); 4],
    to: &[(f32, f32); 4],
) -> (f32, f32, f32) {
    let mut worst = 0.0f32;
    let mut sum = 0.0f32;
    let mut diag = 0.0f32;
    for i in 0..4 {
        let dx = to[i].0 - to[(i + 1) % 4].0;
        let dy = to[i].1 - to[(i + 1) % 4].1;
        diag = diag.max((dx * dx + dy * dy).sqrt());
    }
    for i in 0..4 {
        let (u, v) = from[i];
        let w = h[6] * u + h[7] * v + h[8];
        if !w.is_finite() || w.abs() < 1e-30 {
            return (f32::INFINITY, f32::INFINITY, f32::INFINITY);
        }
        let x = (h[0] * u + h[1] * v + h[2]) / w;
        let y = (h[3] * u + h[4] * v + h[5]) / w;
        if !x.is_finite() || !y.is_finite() {
            return (f32::INFINITY, f32::INFINITY, f32::INFINITY);
        }
        let d = dist((x, y), to[i]);
        worst = worst.max(d);
        sum += d;
    }
    let rel = if diag > 0.0 { worst / diag } else { f32::INFINITY };
    (worst, sum / 4.0, rel)
}

fn warp_degeneracy(h: &[f32; 9], dw: f32, dh: f32) -> (f32, f32) {
    let mut denom_min = f32::INFINITY;
    let mut jac_min = f32::INFINITY;
    for (u, v) in [(0.0f32, 0.0f32), (dw, 0.0), (dw, dh), (0.0, dh)] {
        let w = h[6] * u + h[7] * v + h[8];
        denom_min = denom_min.min(w.abs());
        let nx = h[0] * u + h[1] * v + h[2];
        let ny = h[3] * u + h[4] * v + h[5];
        let a = h[0] * w - nx * h[6];
        let b = h[1] * w - nx * h[7];
        let c = h[3] * w - ny * h[6];
        let d = h[4] * w - ny * h[7];
        let w2 = (w * w).max(1e-30);
        jac_min = jac_min.min((a * d - b * c).abs() / (w2 * w2).max(1e-30));
    }
    (denom_min, jac_min)
}

fn warp(img: &image::DynamicImage, corners: &[(f32, f32); 4]) -> Option<image::DynamicImage> {
    let top = dist(corners[0], corners[1]);
    let bottom = dist(corners[2], corners[3]);
    let left = dist(corners[0], corners[3]);
    let right = dist(corners[1], corners[2]);
    let dwf = ((top + bottom) / 2.0).round();
    let dhf = ((left + right) / 2.0).round();
    if !(dwf >= 2.0) || !(dhf >= 2.0) {
        if crop_debug() {
            eprintln!(
                "  [crop] warp fallback size dw={:.3e} dh={:.3e} -> original",
                dwf, dhf
            );
        }
        return None;
    }
    let dw = (dwf as u32).clamp(2, 32768);
    let dh = (dhf as u32).clamp(2, 32768);

    let dst = [
        (0.0f32, 0.0f32),
        (dw as f32, 0.0),
        (dw as f32, dh as f32),
        (0.0, dh as f32),
    ];
    if !hull_is_quad(corners) {
        if crop_debug() {
            eprintln!("  [crop] warp_fallback reason=hull -> original");
        }
        return None;
    }
    let (h, gauss_degenerate) = homography(&dst, corners);
    let (res, res_mean, res_rel) = warp_residual(&h, &dst, corners);
    if crop_debug() {
        let (denom_min, jac_min) = warp_degeneracy(&h, dw as f32, dh as f32);
        eprintln!(
            "  [crop] warp dst={}x{} gauss_degen={} denom_min={:.3e} jac_min={:.3e} res={:.3e} res_rel={:.3e} res_mean={:.3e}",
            dw,
            dh,
            u8::from(gauss_degenerate),
            denom_min,
            jac_min,
            res,
            res_rel,
            res_mean
        );
    }
    if !res.is_finite() || res > WARP_RES_TOL_PX {
        if crop_debug() {
            eprintln!(
                "  [crop]   warp_fallback res={:.3e} tol={:.1e} -> original",
                res, WARP_RES_TOL_PX
            );
        }
        return None;
    }

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

    Some(image::DynamicImage::ImageRgb8(
        image::RgbImage::from_raw(dw, dh, out)?,
    ))
}

// G2 entry gate for 2.6: privacy-safe unit tests for the deskew geometry core.
// Structural invariants (grep-provable; see warp_has_single_call_site below):
//   1. the warp helper is called from exactly one place, `deskew`, so the
//      perspective stage cannot be reached without detect_corners;
//   2. every candidate quad is canonicalised by order_corners (detect_corners
//      returns order_corners(&rect)) and gated by validate_reason before it is
//      scored or warped.
// Violating either makes the stage unreachable or unsound, so both assumptions
// are asserted here instead of being trusted.
#[cfg(test)]
mod tests {
    use super::*;

    const SKEWED_QUAD: [(f32, f32); 4] = [
        (10.0, 12.0),
        (110.0, 10.0),
        (112.0, 90.0),
        (8.0, 92.0),
    ];

    fn permutations4() -> Vec<[usize; 4]> {
        let mut out = Vec::new();
        for a in 0..4 {
            for b in 0..4 {
                for c in 0..4 {
                    for d in 0..4 {
                        let p = [a, b, c, d];
                        let mut uniq = true;
                        for i in 0..4 {
                            for j in (i + 1)..4 {
                                if p[i] == p[j] {
                                    uniq = false;
                                }
                            }
                        }
                        if uniq {
                            out.push(p);
                        }
                    }
                }
            }
        }
        out
    }

    #[test]
    fn order_corners_is_permutation_invariant() {
        let perms = permutations4();
        assert_eq!(perms.len(), 24);
        let canon = order_corners(&SKEWED_QUAD);
        for p in perms {
            let q = [
                SKEWED_QUAD[p[0]],
                SKEWED_QUAD[p[1]],
                SKEWED_QUAD[p[2]],
                SKEWED_QUAD[p[3]],
            ];
            assert_eq!(order_corners(&q), canon, "perm={:?}", p);
        }
    }

    #[test]
    fn order_corners_is_idempotent() {
        let c1 = order_corners(&SKEWED_QUAD);
        assert_eq!(order_corners(&c1), c1);
    }

    #[test]
    fn order_corners_is_start_and_direction_stable() {
        let base = order_corners(&SKEWED_QUAD);
        let rotated = [
            SKEWED_QUAD[1],
            SKEWED_QUAD[2],
            SKEWED_QUAD[3],
            SKEWED_QUAD[0],
        ];
        let reversed = [
            SKEWED_QUAD[3],
            SKEWED_QUAD[2],
            SKEWED_QUAD[1],
            SKEWED_QUAD[0],
        ];
        assert_eq!(order_corners(&rotated), base);
        assert_eq!(order_corners(&reversed), base);
    }

    #[test]
    fn order_corners_returns_tl_tr_br_bl() {
        let tl = (10.0f32, 10.0f32);
        let tr = (100.0f32, 14.0f32);
        let br = (96.0f32, 104.0f32);
        let bl = (12.0f32, 100.0f32);
        assert_eq!(order_corners(&[br, bl, tl, tr]), [tl, tr, br, bl]);
    }

    #[test]
    fn cross_f_is_positive_counter_clockwise() {
        assert!(cross_f((0.0, 0.0), (1.0, 0.0), (0.0, 1.0)) > 0.0);
        assert!(cross_f((0.0, 0.0), (0.0, 1.0), (1.0, 0.0)) < 0.0);
    }

    #[test]
    fn hull_is_quad_accepts_convex_quad() {
        let square = [(0.0f32, 0.0f32), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        assert!(hull_is_quad(&square));
        let diamond = [(0.0f32, 10.0f32), (10.0, 0.0), (20.0, 10.0), (10.0, 20.0)];
        assert!(hull_is_quad(&diamond));
    }

    #[test]
    fn hull_is_quad_rejects_interior_point() {
        let c = [(0.0f32, 0.0f32), (10.0, 0.0), (10.0, 10.0), (5.0, 5.0)];
        assert!(!hull_is_quad(&c));
    }

    #[test]
    fn hull_is_quad_rejects_collinear_points() {
        let c = [(0.0f32, 0.0f32), (10.0, 0.0), (20.0, 0.0), (30.0, 0.0)];
        assert!(!hull_is_quad(&c));
    }

    #[test]
    fn hull_is_quad_is_permutation_invariant() {
        let c = [(0.0f32, 0.0f32), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        for p in permutations4() {
            let q = [c[p[0]], c[p[1]], c[p[2]], c[p[3]]];
            assert!(hull_is_quad(&q), "perm={:?}", p);
        }
    }

    #[test]
    fn polygon_area_matches_known_square() {
        let square = [(0.0f32, 0.0f32), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        assert!((polygon_area(&square) - 100.0).abs() < 1e-3);
    }

    #[test]
    fn validate_accepts_healthy_quad() {
        let c = [
            (100.0f32, 100.0f32),
            (400.0, 110.0),
            (390.0, 540.0),
            (95.0, 520.0),
        ];
        assert_eq!(validate_reason(&c, 640.0, 640.0), None);
        assert!(validate(&c, 640.0, 640.0));
    }

    #[test]
    fn validate_reason_rejects_frame() {
        let c = [
            (-5.0f32, 100.0f32),
            (400.0, 100.0),
            (400.0, 500.0),
            (0.0, 500.0),
        ];
        assert_eq!(validate_reason(&c, 640.0, 640.0), Some("frame"));
        assert!(!validate(&c, 640.0, 640.0));
    }

    #[test]
    fn validate_reason_rejects_area() {
        let c = [
            (300.0f32, 300.0f32),
            (400.0, 300.0),
            (400.0, 400.0),
            (300.0, 400.0),
        ];
        assert_eq!(validate_reason(&c, 640.0, 640.0), Some("area"));
    }

    #[test]
    fn validate_reason_rejects_aspect() {
        let c = [
            (50.0f32, 50.0f32),
            (150.0, 50.0),
            (150.0, 550.0),
            (50.0, 550.0),
        ];
        assert_eq!(validate_reason(&c, 640.0, 640.0), Some("aspect"));
    }

    #[test]
    fn validate_reason_rejects_selfintersecting_order() {
        let c = [
            (0.0f32, 0.0f32),
            (639.0, 0.0),
            (0.0, 639.0),
            (100.0, 100.0),
        ];
        assert_eq!(validate_reason(&c, 640.0, 640.0), Some("convex"));
        assert!(!validate(&c, 640.0, 640.0));
    }

    #[test]
    fn validate_tracks_reason_exactly() {
        let cases: [([(f32, f32); 4], f32, f32); 5] = [
            (
                [
                    (100.0, 100.0),
                    (400.0, 110.0),
                    (390.0, 540.0),
                    (95.0, 520.0),
                ],
                640.0,
                640.0,
            ),
            (
                [(-5.0, 100.0), (400.0, 100.0), (400.0, 500.0), (0.0, 500.0)],
                640.0,
                640.0,
            ),
            (
                [
                    (300.0, 300.0),
                    (400.0, 300.0),
                    (400.0, 400.0),
                    (300.0, 400.0),
                ],
                640.0,
                640.0,
            ),
            (
                [(50.0, 50.0), (150.0, 50.0), (150.0, 550.0), (50.0, 550.0)],
                640.0,
                640.0,
            ),
            (
                [(0.0, 0.0), (639.0, 0.0), (0.0, 639.0), (100.0, 100.0)],
                640.0,
                640.0,
            ),
        ];
        for (c, dw, dh) in cases {
            assert_eq!(validate(&c, dw, dh), validate_reason(&c, dw, dh).is_none());
        }
    }

    #[test]
    fn validate_implies_consistent_cross_signs() {
        let cases: [([(f32, f32); 4], f32, f32); 4] = [
            (
                [
                    (100.0, 100.0),
                    (400.0, 110.0),
                    (390.0, 540.0),
                    (95.0, 520.0),
                ],
                640.0,
                640.0,
            ),
            (
                [(50.0, 50.0), (600.0, 90.0), (590.0, 600.0), (60.0, 560.0)],
                640.0,
                640.0,
            ),
            (
                [
                    (300.0, 300.0),
                    (400.0, 300.0),
                    (400.0, 400.0),
                    (300.0, 400.0),
                ],
                640.0,
                640.0,
            ),
            (
                [(0.0, 0.0), (639.0, 0.0), (0.0, 639.0), (100.0, 100.0)],
                640.0,
                640.0,
            ),
        ];
        for (c, dw, dh) in cases {
            if !validate(&c, dw, dh) {
                continue;
            }
            let signs = [
                cross_f(c[0], c[1], c[2]).signum(),
                cross_f(c[1], c[2], c[3]).signum(),
                cross_f(c[2], c[3], c[0]).signum(),
                cross_f(c[3], c[0], c[1]).signum(),
            ];
            assert!(signs[0] != 0.0, "degenerate corner set accepted: {:?}", c);
            for s in signs {
                assert_eq!(s, signs[0], "inconsistent cross sign for {:?}", c);
            }
        }
    }

    #[test]
    fn warp_residual_is_zero_for_identity() {
        let h = [1.0f32, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
        let q = [(0.0f32, 0.0f32), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        let (worst, mean, rel) = warp_residual(&h, &q, &q);
        assert!(worst.abs() < 1e-4, "worst={worst}");
        assert!(mean.abs() < 1e-4, "mean={mean}");
        assert!(rel.abs() < 1e-4, "rel={rel}");
    }

    #[test]
    fn warp_residual_reports_worst_mismatch() {
        let h = [1.0f32, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
        let from = [(0.0f32, 0.0f32), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        let to = [(0.0f32, 0.0f32), (12.5, 0.0), (12.5, 10.0), (0.0, 10.0)];
        let (worst, _, _) = warp_residual(&h, &from, &to);
        assert!((worst - 2.5).abs() < 1e-3, "worst={worst}");
    }

    #[test]
    fn warp_residual_is_infinite_when_denominator_vanishes() {
        let h = [1.0f32, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
        let from = [(1.0f32, 1.0f32), (2.0, 2.0), (3.0, 3.0), (4.0, 4.0)];
        let to = [(0.0f32, 0.0f32); 4];
        let (worst, mean, rel) = warp_residual(&h, &from, &to);
        assert!(worst.is_infinite() && mean.is_infinite() && rel.is_infinite());
    }

    #[test]
    fn warp_degeneracy_is_one_for_identity() {
        let h = [1.0f32, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
        let (denom_min, jac_min) = warp_degeneracy(&h, 100.0, 100.0);
        assert!((denom_min - 1.0).abs() < 1e-6, "denom_min={denom_min}");
        assert!((jac_min - 1.0).abs() < 1e-6, "jac_min={jac_min}");
    }

    #[test]
    fn warp_degeneracy_collapses_on_zero_matrix() {
        let h = [0.0f32; 9];
        let (denom_min, jac_min) = warp_degeneracy(&h, 100.0, 100.0);
        assert!(denom_min == 0.0, "denom_min={denom_min}");
        assert!(jac_min == 0.0, "jac_min={jac_min}");
    }

    #[test]
    fn homography_reproduces_its_own_correspondence() {
        let dst = [(0.0f32, 0.0f32), (100.0, 0.0), (100.0, 50.0), (0.0, 50.0)];
        let src = [(10.0f32, 12.0f32), (120.0, 8.0), (118.0, 60.0), (14.0, 58.0)];
        let (h, degenerate) = homography(&dst, &src);
        assert!(!degenerate, "well-conditioned 4-point DLT flagged degenerate");
        let (worst, _, _) = warp_residual(&h, &dst, &src);
        assert!(worst < WARP_RES_TOL_PX, "worst={worst}");
    }

    #[test]
    fn homography_on_coincident_corners_yields_no_solution() {
        let from = [(5.0f32, 5.0f32), (5.0, 5.0), (5.0, 5.0), (5.0, 5.0)];
        let to = [(0.0f32, 0.0f32), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
        let (h, degenerate) = homography(&from, &to);
        let (worst, _, _) = warp_residual(&h, &from, &to);
        assert!(
            degenerate || !worst.is_finite() || worst > WARP_RES_TOL_PX,
            "degenerate input produced an accepted solve: degen={degenerate} worst={worst}"
        );
    }

    #[test]
    fn warp_has_single_call_site() {
        let src = include_str!("crop.rs");
        let production = src.split("#[cfg(test)]").next().unwrap_or(src);
        let hits = production.lines().filter(|l| l.contains("warp(")).count();
        assert_eq!(hits, 2, "expected one definition and one call site, got {hits}");
    }
}
