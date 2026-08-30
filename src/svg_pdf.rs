#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use fontdb;
use subsetter;

use resvg::usvg;
use usvg::tiny_skia_path::PathSegment;

pub(crate) struct ShadingRes {
    pub name: String,
    pub kind: u8,
    pub coords: [f32; 6],
    pub matrix: [f32; 6],
    pub stops: Vec<(f32, [f32; 3])>,
}

pub(crate) struct PatternRes {
    pub name: String,
    pub shading: String,
}

use crate::decode::{mem_budget, MAX_SVG_DEPTH, MemPermit, vector_peak_cap, vector_peak_estimate};
pub(crate) struct VectorPage {
    pub width: u32,
    pub height: u32,
    pub content: Vec<u8>,
    pub ext_gs: Vec<(String, f32, f32)>,
    pub shadings: Vec<ShadingRes>,
    pub patterns: Vec<PatternRes>,
    pub images: Vec<ImageRes>,
    pub fonts: Vec<FontRes>,
    _permit: Option<MemPermit<'static>>,
}

#[derive(Clone, Copy)]
pub(crate) enum ImageColorSpace {
    Gray,
    Rgb,
    Cmyk,
}

pub(crate) struct ImageRes {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub colorspace: ImageColorSpace,
    pub dct: bool,
    pub data: Vec<u8>,
    pub smask: Option<Vec<u8>>,
    pub cmyk_inverted: bool,
    pub icc: Option<Vec<u8>>,
}

pub(crate) struct FontRes {
    pub font_id: fontdb::ID,
    pub name: String,
    pub subset: Vec<u8>,
    pub is_cff: bool,
    pub upem: u16,
    pub bbox: [i16; 4],
    pub ascent: i16,
    pub descent: i16,
    pub cids: BTreeMap<u16, u16>,
    pub to_unicode: BTreeMap<u16, String>,
}

struct ResolvedPaint {
    color: Option<[f32; 3]>,
    pattern: Option<String>,
    alpha: f32,
}

enum GradientPaint {
    Solid([f32; 3]),
    Pattern(String),
}

pub(crate) fn build_vector_page(tree: &usvg::Tree, target_w: u32, target_h: u32, grayscale: bool) -> Option<VectorPage> {
    let size = tree.size();
    let sw = size.width();
    if sw <= 0.0 || size.height() <= 0.0 {
        return None;
    }
    let tw = target_w.max(1) as f32;
    let th = target_h.max(1) as f32;
    let s = tw / sw;

    let mut out = String::new();
    let mut ext_gs: Vec<(String, f32, f32)> = Vec::new();
    let mut gs_names: BTreeMap<(u16, u16), String> = BTreeMap::new();
    let mut shadings: Vec<ShadingRes> = Vec::new();
    let mut patterns: Vec<PatternRes> = Vec::new();
    let mut images: Vec<ImageRes> = Vec::new();
    let mut fonts: Vec<FontRes> = Vec::new();

    let root = tree.root();
    if root.should_isolate() {
        return None;
    }

    let vec_need = vector_peak_estimate(tree, grayscale)?;
    if vec_need > vector_peak_cap() {
        return None;
    }
    let budget = mem_budget();
    budget.acquire(vec_need);
    let mut permit = Some(MemPermit { budget, need: vec_need });

    let fontdb = tree.fontdb();
    let mut text_glyphs: BTreeMap<fontdb::ID, BTreeMap<u16, String>> = BTreeMap::new();
    for node in root.children() {
        if !collect_texts(node, 0, &mut text_glyphs) {
            return None;
        }
    }
    for (font_id, texts) in &text_glyphs {
        if let Some(r) = subset_font(fontdb, *font_id, texts) {
            let name = format!("F{}", fonts.len());
            fonts.push(FontRes {
                font_id: *font_id,
                name,
                subset: r.subset,
                is_cff: r.is_cff,
                upem: r.upem,
                bbox: r.bbox,
                ascent: r.ascent,
                descent: r.descent,
                cids: r.cids,
                to_unicode: r.to_unicode,
            });
        }
    }
    for node in root.children() {
        if !emit_node(node, s, th, grayscale, &mut out, &mut ext_gs, &mut gs_names, &mut shadings, &mut patterns, &mut images, &mut fonts, fontdb, 0) {
            return None;
        }
    }

    Some(VectorPage {
        width: target_w.max(1),
        height: target_h.max(1),
        content: out.into_bytes(),
        ext_gs,
        shadings,
        patterns,
        images,
        fonts,
        _permit: permit.take(),
    })
}

fn emit_node(
    node: &usvg::Node,
    s: f32,
    th: f32,
    grayscale: bool,
    out: &mut String,
    ext_gs: &mut Vec<(String, f32, f32)>,
    gs_names: &mut BTreeMap<(u16, u16), String>,
    shadings: &mut Vec<ShadingRes>,
    patterns: &mut Vec<PatternRes>,
    images: &mut Vec<ImageRes>,
    fonts: &mut Vec<FontRes>,
    fontdb: &Arc<fontdb::Database>,
    depth: usize,
) -> bool {
    if depth > MAX_SVG_DEPTH {
        return false;
    }
    match node {
        usvg::Node::Group(g) => {
            if let Some(clip) = g.clip_path() {
                if g.mask().is_some()
                    || !g.filters().is_empty()
                    || g.blend_mode() != usvg::BlendMode::Normal
                    || g.opacity().get() < 0.999
                {
                    return false;
                }
                let group_abs = g.abs_transform();
                out.push_str("q\n");
                let mut rule = None;
                if !emit_clip_children(clip.root(), &clip.transform(), &group_abs, s, th, out, &mut rule, depth + 1) {
                    return false;
                }
                let evenodd = match rule {
                    Some(r) => r == usvg::FillRule::EvenOdd,
                    None => return false,
                };
                out.push_str(if evenodd { "W*\n" } else { "W\n" });
                out.push_str("n\n");
                if let Some(sub) = clip.clip_path() {
                    let mut sub_rule = None;
                    if !emit_clip_children(sub.root(), &sub.transform(), &group_abs, s, th, out, &mut sub_rule, depth + 1) {
                        return false;
                    }
                    let sub_evenodd = match sub_rule {
                        Some(r) => r == usvg::FillRule::EvenOdd,
                        None => return false,
                    };
                    out.push_str(if sub_evenodd { "W*\n" } else { "W\n" });
                    out.push_str("n\n");
                }
                for child in g.children() {
                    if !emit_node(child, s, th, grayscale, out, ext_gs, gs_names, shadings, patterns, images, fonts, fontdb, depth + 1) {
                        return false;
                    }
                }
                out.push_str("Q\n");
                true
            } else if g.should_isolate() {
                let op = g.opacity().get();
                let opacity_only = op < 1.0
                    && !g.isolate()
                    && g.clip_path().is_none()
                    && g.mask().is_none()
                    && g.filters().is_empty()
                    && g.blend_mode() == usvg::BlendMode::Normal;
                if !opacity_only {
                    return false;
                }
                let name = format!("GSg{}", ext_gs.len());
                out.push_str("q\n");
                out.push_str(&format!("/{name} gs\n"));
                ext_gs.push((name, op, op));
                for child in g.children() {
                    if !emit_node(child, s, th, grayscale, out, ext_gs, gs_names, shadings, patterns, images, fonts, fontdb, depth + 1) {
                        return false;
                    }
                }
                out.push_str("Q\n");
                true
            } else {
                for child in g.children() {
                    if !emit_node(child, s, th, grayscale, out, ext_gs, gs_names, shadings, patterns, images, fonts, fontdb, depth + 1) {
                        return false;
                    }
                }
                true
            }
        }
        usvg::Node::Path(p) => emit_path(p, s, th, grayscale, out, ext_gs, gs_names, shadings, patterns),
        usvg::Node::Image(img) => emit_image(img, s, th, grayscale, out, images),
        usvg::Node::Text(t) => emit_text(t, s, th, grayscale, out, ext_gs, gs_names, shadings, patterns, images, fonts, fontdb, depth),
    }
}

fn emit_clip_children(
    parent: &usvg::Group,
    clip_t: &usvg::Transform,
    group_abs: &usvg::Transform,
    s: f32,
    th: f32,
    out: &mut String,
    rule: &mut Option<usvg::FillRule>,
    depth: usize,
) -> bool {
    if depth > MAX_SVG_DEPTH {
        return false;
    }
    for child in parent.children() {
        match child {
            usvg::Node::Path(p) => {
                if !p.is_visible() {
                    continue;
                }
                let fill = match p.fill() {
                    Some(f) => f,
                    None => return false,
                };
                match *rule {
                    None => *rule = Some(fill.rule()),
                    Some(r) if r != fill.rule() => return false,
                    Some(_) => {}
                }
                let t = compose(group_abs, clip_t);
                let t = compose(&t, &p.abs_transform());
                let m = usvg::Transform::from_row(
                    s * t.sx,
                    -s * t.ky,
                    s * t.kx,
                    -s * t.sy,
                    s * t.tx,
                    th - s * t.ty,
                );
                let data = match p.data().clone().transform(m) {
                    Some(d) => d,
                    None => return false,
                };
                if data.segments().next().is_none() {
                    continue;
                }
                emit_path_data(&data, out);
            }
            usvg::Node::Group(g) => {
                if g.should_isolate() {
                    return false;
                }
                if !emit_clip_children(g, clip_t, group_abs, s, th, out, rule, depth + 1) {
                    return false;
                }
            }
            usvg::Node::Text(t) => {
                if !emit_clip_children(t.flattened(), clip_t, group_abs, s, th, out, rule, depth + 1) {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

fn emit_path(
    p: &usvg::Path,
    s: f32,
    th: f32,
    grayscale: bool,
    out: &mut String,
    ext_gs: &mut Vec<(String, f32, f32)>,
    gs_names: &mut BTreeMap<(u16, u16), String>,
    shadings: &mut Vec<ShadingRes>,
    patterns: &mut Vec<PatternRes>,
) -> bool {
    if !p.is_visible() {
        return true;
    }

    let fill = p.fill();
    let stroke = p.stroke();
    if fill.is_none() && stroke.is_none() {
        return true;
    }

    let t = p.abs_transform();
    let m = usvg::Transform::from_row(
        s * t.sx,
        -s * t.ky,
        s * t.kx,
        -s * t.sy,
        s * t.tx,
        th - s * t.ty,
    );
    let data = match p.data().clone().transform(m) {
        Some(d) => d,
        None => return false,
    };

    let fill_res = match fill {
        None => None,
        Some(f) => match resolve_paint(f.paint(), f.opacity().get(), &m, grayscale, shadings, patterns) {
            Some(rp) => Some((rp, f.rule())),
            None => return false,
        },
    };
    let stroke_res = match stroke {
        None => None,
        Some(st) => match resolve_paint(st.paint(), st.opacity().get(), &m, grayscale, shadings, patterns) {
            Some(rp) => Some(rp),
            None => return false,
        },
    };

    let fill_alpha = fill_res.as_ref().map(|(rp, _)| rp.alpha).unwrap_or(1.0);
    let stroke_alpha = stroke_res.as_ref().map(|rp| rp.alpha).unwrap_or(1.0);

    let gs_name = if fill_alpha < 0.999 || stroke_alpha < 0.999 {
        let key = (alpha_key(fill_alpha), alpha_key(stroke_alpha));
        if let Some(n) = gs_names.get(&key) {
            Some(n.clone())
        } else {
            let n = format!("GS{}", ext_gs.len());
            gs_names.insert(key, n.clone());
            ext_gs.push((n.clone(), fill_alpha, stroke_alpha));
            Some(n)
        }
    } else {
        None
    };

    let abs_scale = ((t.sx * t.sx + t.ky * t.ky).sqrt() * (t.kx * t.kx + t.sy * t.sy).sqrt()).sqrt();
    let stroke_scale = s * abs_scale;

    out.push_str("q\n");
    if let Some(n) = &gs_name {
        out.push_str(&format!("/{} gs\n", n));
    }

    emit_path_data(&data, out);

    match (&fill_res, &stroke_res, stroke) {
        (Some((fp, rule)), Some(sp), Some(st)) => {
            set_stroke_params(st, stroke_scale, out);
            set_fill(fp, grayscale, out);
            set_stroke_paint(sp, grayscale, out);
            out.push_str(if *rule == usvg::FillRule::EvenOdd { "B*\n" } else { "B\n" });
        }
        (Some((fp, rule)), None, _) => {
            set_fill(fp, grayscale, out);
            out.push_str(if *rule == usvg::FillRule::EvenOdd { "f*\n" } else { "f\n" });
        }
        (None, Some(sp), Some(st)) => {
            set_stroke_params(st, stroke_scale, out);
            set_stroke_paint(sp, grayscale, out);
            out.push_str("S\n");
        }
        _ => {}
    }

    out.push_str("Q\n");
    true
}

struct SubsetResult {
    subset: Vec<u8>,
    is_cff: bool,
    upem: u16,
    bbox: [i16; 4],
    ascent: i16,
    descent: i16,
    cids: BTreeMap<u16, u16>,
    to_unicode: BTreeMap<u16, String>,
}

fn table_offset(data: &[u8], want: &[u8; 4]) -> Option<usize> {
    if data.len() < 12 {
        return None;
    }
    let num = u16::from_be_bytes([data[4], data[5]]) as usize;
    let mut i = 12usize;
    for _ in 0..num {
        if i + 16 > data.len() {
            return None;
        }
        if &data[i..i + 4] == want {
            return Some(u32::from_be_bytes([data[i + 8], data[i + 9], data[i + 10], data[i + 11]]) as usize);
        }
        i += 16;
    }
    None
}

fn font_tables(data: &[u8]) -> Option<Vec<[u8; 4]>> {
    if data.len() < 12 {
        return None;
    }
    let num = u16::from_be_bytes([data[4], data[5]]) as usize;
    let mut tags = Vec::with_capacity(num);
    let mut i = 12usize;
    for _ in 0..num {
        if i + 16 > data.len() {
            return None;
        }
        let mut tag = [0u8; 4];
        tag.copy_from_slice(&data[i..i + 4]);
        tags.push(tag);
        i += 16;
    }
    Some(tags)
}

fn is_color_font(data: &[u8]) -> bool {
    match font_tables(data) {
        Some(tags) => tags.iter().any(|t| {
            t == b"COLR" || t == b"sbix" || t == b"CBDT" || t == b"CBLC" || t == b"SVG "
        }),
        None => true,
    }
}

fn font_head(data: &[u8]) -> Option<(u16, [i16; 4], i16, i16)> {
    let head = table_offset(data, b"head")?;
    let hhea = table_offset(data, b"hhea")?;
    if head + 44 > data.len() || hhea + 8 > data.len() {
        return None;
    }
    let upem = u16::from_be_bytes([data[head + 18], data[head + 19]]);
    if upem == 0 {
        return None;
    }
    let bbox = [
        i16::from_be_bytes([data[head + 36], data[head + 37]]),
        i16::from_be_bytes([data[head + 38], data[head + 39]]),
        i16::from_be_bytes([data[head + 40], data[head + 41]]),
        i16::from_be_bytes([data[head + 42], data[head + 43]]),
    ];
    let ascent = i16::from_be_bytes([data[hhea + 4], data[hhea + 5]]);
    let descent = i16::from_be_bytes([data[hhea + 6], data[hhea + 7]]);
    Some((upem, bbox, ascent, descent))
}

fn extract_cff_table(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 12 {
        return None;
    }
    let num = u16::from_be_bytes([data[4], data[5]]) as usize;
    let mut i = 12usize;
    for _ in 0..num {
        if i + 16 > data.len() {
            return None;
        }
        if &data[i..i + 4] == b"CFF " {
            let off = u32::from_be_bytes([data[i + 8], data[i + 9], data[i + 10], data[i + 11]]) as usize;
            let len = u32::from_be_bytes([data[i + 12], data[i + 13], data[i + 14], data[i + 15]]) as usize;
            let end = off.checked_add(len)?;
            if end > data.len() {
                return None;
            }
            return Some(data[off..end].to_vec());
        }
        i += 16;
    }
    None
}

fn subset_font(
    fontdb: &Arc<fontdb::Database>,
    font_id: fontdb::ID,
    texts: &BTreeMap<u16, String>,
) -> Option<SubsetResult> {
    fontdb.with_face_data(font_id, |data, index| {
        if is_color_font(data) {
            return None;
        }
        let glyphs: Vec<u16> = texts.keys().copied().collect();
        let remapper = subsetter::GlyphRemapper::new_from_glyphs_sorted(&glyphs);
        let subset = subsetter::subset(data, index, &remapper).ok()?;
        let is_cff = subset.len() >= 4 && &subset[0..4] == b"OTTO";
        let (upem, bbox, ascent, descent) = font_head(&subset)?;
        let subset = if is_cff {
            extract_cff_table(&subset)?
        } else {
            subset
        };
        let mut cids = BTreeMap::new();
        let mut to_unicode = BTreeMap::new();
        for (gid, text) in texts {
            let cid = remapper.get(*gid)?;
            cids.insert(*gid, cid);
            let u = if text.is_empty() { "\u{FFFD}".to_string() } else { text.clone() };
            to_unicode.insert(cid, u);
        }
        Some(SubsetResult { subset, is_cff, upem, bbox, ascent, descent, cids, to_unicode })
    })?
}

fn span_native_paint(span: &usvg::layout::Span) -> bool {
    if let Some(f) = &span.fill {
        if !matches!(f.paint(), usvg::Paint::Color(_)) {
            return false;
        }
    }
    if let Some(st) = &span.stroke {
        if !matches!(st.paint(), usvg::Paint::Color(_)) {
            return false;
        }
    }
    for dec in [&span.underline, &span.overline, &span.line_through] {
        if let Some(d) = dec.as_ref() {
            if let Some(f) = d.fill() {
                if !matches!(f.paint(), usvg::Paint::Color(_)) {
                    return false;
                }
            }
            if let Some(st) = d.stroke() {
                if !matches!(st.paint(), usvg::Paint::Color(_)) {
                    return false;
                }
            }
        }
    }
    true
}

fn collect_texts(
    node: &usvg::Node,
    depth: usize,
    acc: &mut BTreeMap<fontdb::ID, BTreeMap<u16, String>>,
) -> bool {
    if depth > MAX_SVG_DEPTH {
        return false;
    }
    match node {
        usvg::Node::Group(g) => {
            for child in g.children() {
                if !collect_texts(child, depth + 1, acc) {
                    return false;
                }
            }
            true
        }
        usvg::Node::Text(t) => {
            let spans = t.layouted();
            if spans.is_empty() {
                return true;
            }
            let mut eligible = true;
            for span in spans {
                if !span.visible {
                    continue;
                }
                if !span_native_paint(span) {
                    eligible = false;
                    break;
                }
                for pg in &span.positioned_glyphs {
                    if pg.id.0 > u16::MAX as u32 {
                        eligible = false;
                        break;
                    }
                }
                if !eligible {
                    break;
                }
            }
            if eligible {
                for span in spans {
                    if !span.visible {
                        continue;
                    }
                    for pg in &span.positioned_glyphs {
                        acc.entry(pg.font)
                            .or_default()
                            .entry(pg.id.0 as u16)
                            .or_insert_with(|| pg.text.clone());
                    }
                }
            }
            true
        }
        _ => true,
    }
}

pub(crate) fn to_unicode_cmap(map: &BTreeMap<u16, String>) -> Vec<u8> {
    let mut s = String::new();
    s.push_str("/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n");
    s.push_str("/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n");
    s.push_str("/CMapName /Adobe-Identity-UCS def\n");
    s.push_str("/CMapType 2 def\n");
    s.push_str("1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n");
    s.push_str(&format!("{} beginbfchar\n", map.len()));
    for (cid, text) in map {
        let mut hex = String::new();
        for u in text.encode_utf16() {
            hex.push_str(&format!("{:04X}", u));
        }
        if hex.is_empty() {
            hex.push_str("FFFD");
        }
        s.push_str(&format!("<{:04X}> <{}>\n", cid, hex));
    }
    s.push_str("endbfchar\n");
    s.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    s.into_bytes()
}

fn emit_text_flattened(
    t: &usvg::Text,
    s: f32,
    th: f32,
    grayscale: bool,
    out: &mut String,
    ext_gs: &mut Vec<(String, f32, f32)>,
    gs_names: &mut BTreeMap<(u16, u16), String>,
    shadings: &mut Vec<ShadingRes>,
    patterns: &mut Vec<PatternRes>,
    images: &mut Vec<ImageRes>,
    fonts: &mut Vec<FontRes>,
    fontdb: &Arc<fontdb::Database>,
    depth: usize,
) -> bool {
    let mut stack: Vec<usvg::Node> = t.flattened().children().to_vec();
    stack.reverse();
    while let Some(node) = stack.pop() {
        if let usvg::Node::Group(ref group) = node {
            let mut children: Vec<usvg::Node> = group.children().to_vec();
            children.reverse();
            stack.extend(children);
            continue;
        }
        if !matches!(node, usvg::Node::Path(_)) {
            return false;
        }
        if !emit_node(&node, s, th, grayscale, out, ext_gs, gs_names, shadings, patterns, images, fonts, fontdb, depth + 1) {
            return false;
        }
    }
    true
}

fn emit_text(
    t: &usvg::Text,
    s: f32,
    th: f32,
    grayscale: bool,
    out: &mut String,
    ext_gs: &mut Vec<(String, f32, f32)>,
    gs_names: &mut BTreeMap<(u16, u16), String>,
    shadings: &mut Vec<ShadingRes>,
    patterns: &mut Vec<PatternRes>,
    images: &mut Vec<ImageRes>,
    fonts: &mut Vec<FontRes>,
    fontdb: &Arc<fontdb::Database>,
    depth: usize,
) -> bool {
    if depth > MAX_SVG_DEPTH {
        return false;
    }
    let spans = t.layouted();
    if spans.is_empty() {
        return true;
    }

    for span in spans {
        if !span.visible {
            continue;
        }
        if !span_native_paint(span) {
            return emit_text_flattened(t, s, th, grayscale, out, ext_gs, gs_names, shadings, patterns, images, fonts, fontdb, depth);
        }
    }

    let mut idx_by_font: HashMap<fontdb::ID, usize> = HashMap::new();
    for span in spans {
        if !span.visible {
            continue;
        }
        for pg in &span.positioned_glyphs {
            if pg.id.0 > u16::MAX as u32 {
                return emit_text_flattened(t, s, th, grayscale, out, ext_gs, gs_names, shadings, patterns, images, fonts, fontdb, depth);
            }
            if !idx_by_font.contains_key(&pg.font) {
                match fonts.iter().position(|f| f.font_id == pg.font) {
                    Some(pos) => {
                        idx_by_font.insert(pg.font, pos);
                    }
                    None => return emit_text_flattened(t, s, th, grayscale, out, ext_gs, gs_names, shadings, patterns, images, fonts, fontdb, depth),
                }
            }
        }
    }

    let m0 = usvg::Transform::default();
    let abs = t.abs_transform();
    let abs_scale = ((abs.sx * abs.sx + abs.ky * abs.ky).sqrt() * (abs.kx * abs.kx + abs.sy * abs.sy).sqrt()).sqrt();
    let stroke_scale = s * abs_scale;

    out.push_str("q\n");
    for span in spans {
        if !span.visible || span.positioned_glyphs.is_empty() {
            continue;
        }

        let fill_res = span.fill.as_ref().and_then(|f| {
            resolve_paint(f.paint(), f.opacity().get(), &m0, grayscale, shadings, patterns)
        });
        let stroke_res = span.stroke.as_ref().and_then(|st| {
            resolve_paint(st.paint(), st.opacity().get(), &m0, grayscale, shadings, patterns)
        });

        let fill_alpha = fill_res.as_ref().map(|r| r.alpha).unwrap_or(1.0);
        let stroke_alpha = stroke_res.as_ref().map(|r| r.alpha).unwrap_or(1.0);
        let gs_name = if fill_alpha < 0.999 || stroke_alpha < 0.999 {
            let key = (alpha_key(fill_alpha), alpha_key(stroke_alpha));
            if let Some(n) = gs_names.get(&key) {
                Some(n.clone())
            } else {
                let n = format!("GS{}", ext_gs.len());
                gs_names.insert(key, n.clone());
                ext_gs.push((n.clone(), fill_alpha, stroke_alpha));
                Some(n)
            }
        } else {
            None
        };
        if let Some(n) = &gs_name {
            out.push_str(&format!("/{} gs\n", n));
        }

        let mut passes: Vec<bool> = Vec::new();
        let fill_first = !matches!(span.paint_order, usvg::PaintOrder::StrokeAndFill);
        if fill_first {
            if fill_res.is_some() {
                passes.push(true);
            }
            if stroke_res.is_some() {
                passes.push(false);
            }
        } else {
            if stroke_res.is_some() {
                passes.push(false);
            }
            if fill_res.is_some() {
                passes.push(true);
            }
        }

        for dec in [&span.underline, &span.overline] {
            if let Some(d) = dec.as_ref() {
                if !emit_path(d, s, th, grayscale, out, ext_gs, gs_names, shadings, patterns) {
                    return false;
                }
            }
        }

        for do_fill in passes {
            if do_fill {
                if let Some(fp) = &fill_res {
                    set_fill(fp, grayscale, out);
                }
                out.push_str("BT\n0 Tr\n");
            } else {
                if let (Some(sp), Some(st)) = (&stroke_res, span.stroke.as_ref()) {
                    set_stroke_params(st, stroke_scale, out);
                    set_stroke_paint(sp, grayscale, out);
                }
                out.push_str("BT\n1 Tr\n");
            }
            for pg in &span.positioned_glyphs {
                let pos = match idx_by_font.get(&pg.font) {
                    Some(p) => *p,
                    None => return false,
                };
                let f = &fonts[pos];
                let cid = match f.cids.get(&(pg.id.0 as u16)) {
                    Some(c) => *c,
                    None => return false,
                };
                let sx = pg.font_size() / f.upem as f32;
                let o = abs.pre_concat(pg.outline_transform());
                let d = usvg::Transform::from_row(
                    s * o.sx,
                    -s * o.ky,
                    s * o.kx,
                    -s * o.sy,
                    s * o.tx,
                    th - s * o.ty,
                );
                let tm = usvg::Transform::from_row(
                    d.sx / sx,
                    d.ky / sx,
                    d.kx / sx,
                    d.sy / sx,
                    d.tx,
                    d.ty,
                );
                out.push_str(&format!(
                    "/{} {} Tf {} {} {} {} {} {} Tm <{:04X}> Tj\n",
                    f.name,
                    fmt_num(pg.font_size()),
                    fmt_num(tm.sx),
                    fmt_num(tm.ky),
                    fmt_num(tm.kx),
                    fmt_num(tm.sy),
                    fmt_num(tm.tx),
                    fmt_num(tm.ty),
                    cid
                ));
            }
            out.push_str("ET\n");
        }

        if let Some(d) = &span.line_through {
            if !emit_path(d, s, th, grayscale, out, ext_gs, gs_names, shadings, patterns) {
                return false;
            }
        }
    }
    out.push_str("Q\n");
    true
}

fn set_fill(rp: &ResolvedPaint, gray: bool, out: &mut String) {
    match (&rp.pattern, &rp.color) {
        (Some(name), _) => out.push_str(&format!("/Pattern cs /{} scn\n", name)),
        (None, Some(c)) if gray => out.push_str(&format!("{} g\n", fmt_num(c[0]))),
        (None, Some(c)) => out.push_str(&format!("{} {} {} rg\n", fmt_num(c[0]), fmt_num(c[1]), fmt_num(c[2]))),
        (None, None) => {}
    }
}

fn set_stroke_paint(rp: &ResolvedPaint, gray: bool, out: &mut String) {
    match (&rp.pattern, &rp.color) {
        (Some(name), _) => out.push_str(&format!("/Pattern CS /{} SCN\n", name)),
        (None, Some(c)) if gray => out.push_str(&format!("{} G\n", fmt_num(c[0]))),
        (None, Some(c)) => out.push_str(&format!("{} {} {} RG\n", fmt_num(c[0]), fmt_num(c[1]), fmt_num(c[2]))),
        (None, None) => {}
    }
}

fn resolve_paint(
    paint: &usvg::Paint,
    alpha: f32,
    m: &usvg::Transform,
    grayscale: bool,
    shadings: &mut Vec<ShadingRes>,
    patterns: &mut Vec<PatternRes>,
) -> Option<ResolvedPaint> {
    match paint {
        usvg::Paint::Color(c) => Some(ResolvedPaint {
            color: Some(if grayscale {
                gray3(c.red, c.green, c.blue)
            } else {
                [fr(c.red), fr(c.green), fr(c.blue)]
            }),
            pattern: None,
            alpha,
        }),
        usvg::Paint::LinearGradient(lg) => {
            let g: &usvg::BaseGradient = lg;
            resolve_gradient(g, 2, [lg.x1(), lg.y1(), lg.x2(), lg.y2(), 0.0, 0.0], m, grayscale, shadings, patterns)
                .map(|gp| into_resolved(gp, alpha))
        }
        usvg::Paint::RadialGradient(rg) => {
            let g: &usvg::BaseGradient = rg;
            resolve_gradient(
                g,
                3,
                [rg.fx(), rg.fy(), rg.fr().get(), rg.cx(), rg.cy(), rg.r().get()],
                m,
                grayscale,
                shadings,
                patterns,
            )
            .map(|gp| into_resolved(gp, alpha))
        }
        _ => None,
    }
}

fn into_resolved(gp: GradientPaint, alpha: f32) -> ResolvedPaint {
    match gp {
        GradientPaint::Solid(c) => ResolvedPaint { color: Some(c), pattern: None, alpha },
        GradientPaint::Pattern(name) => ResolvedPaint { color: None, pattern: Some(name), alpha },
    }
}

fn resolve_gradient(
    base: &usvg::BaseGradient,
    kind: u8,
    coords: [f32; 6],
    m: &usvg::Transform,
    grayscale: bool,
    shadings: &mut Vec<ShadingRes>,
    patterns: &mut Vec<PatternRes>,
) -> Option<GradientPaint> {
    if base.spread_method() != usvg::SpreadMethod::Pad {
        return None;
    }
    let stops = base.stops();
    if stops.is_empty() {
        return None;
    }
    for st in stops {
        if st.opacity().get() < 1.0 {
            return None;
        }
    }

    let degenerate = match kind {
        2 => (coords[0] - coords[2]).abs() < 1e-6 && (coords[1] - coords[3]).abs() < 1e-6,
        3 => coords[5] <= 0.0,
        _ => true,
    };

    if degenerate || stops.len() == 1 {
        let last = stops.last().unwrap().color();
        let c = if grayscale {
            gray3(last.red, last.green, last.blue)
        } else {
            [fr(last.red), fr(last.green), fr(last.blue)]
        };
        return Some(GradientPaint::Solid(c));
    }

    let gt = base.transform();
    let matrix = compose(m, &gt);

    let sname = format!("Sh{}", shadings.len());
    let pname = format!("P{}", patterns.len());

    let mut collected = Vec::with_capacity(stops.len());
    for st in stops {
        let c = st.color();
        let col = if grayscale {
            gray3(c.red, c.green, c.blue)
        } else {
            [fr(c.red), fr(c.green), fr(c.blue)]
        };
        collected.push((st.offset().get(), col));
    }

    shadings.push(ShadingRes {
        name: sname.clone(),
        kind,
        coords,
        matrix: [matrix.sx, matrix.ky, matrix.kx, matrix.sy, matrix.tx, matrix.ty],
        stops: collected,
    });
    patterns.push(PatternRes {
        name: pname.clone(),
        shading: sname,
    });

    Some(GradientPaint::Pattern(pname))
}

fn compose(a: &usvg::Transform, b: &usvg::Transform) -> usvg::Transform {
    usvg::Transform::from_row(
        a.sx * b.sx + a.kx * b.ky,
        a.ky * b.sx + a.sy * b.ky,
        a.sx * b.kx + a.kx * b.sy,
        a.ky * b.kx + a.sy * b.sy,
        a.sx * b.tx + a.kx * b.ty + a.tx,
        a.ky * b.tx + a.sy * b.ty + a.ty,
    )
}

fn fmt_num(v: f32) -> String {
    if v == 0.0 {
        return "0".to_string();
    }
    let s = format!("{:.8}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() {
        "0".to_string()
    } else {
        s.to_string()
    }
}

fn set_stroke_params(st: &usvg::Stroke, scale: f32, out: &mut String) {
    out.push_str(&format!("{} w\n", fmt_num(st.width().get() * scale)));
    out.push_str(match st.linecap() {
        usvg::LineCap::Butt => "0 J\n",
        usvg::LineCap::Round => "1 J\n",
        usvg::LineCap::Square => "2 J\n",
    });
    out.push_str(match st.linejoin() {
        usvg::LineJoin::Miter | usvg::LineJoin::MiterClip => "0 j\n",
        usvg::LineJoin::Round => "1 j\n",
        usvg::LineJoin::Bevel => "2 j\n",
    });
    out.push_str(&format!("{} M\n", fmt_num(st.miterlimit().get())));
    if let Some(dash) = st.dasharray() {
        if !dash.is_empty() {
            let list: Vec<String> = dash.iter().map(|v| fmt_num(v * scale)).collect();
            out.push_str(&format!("[{}] {} d\n", list.join(" "), fmt_num(st.dashoffset() * scale)));
        }
    }
}

fn emit_path_data(path: &usvg::tiny_skia_path::Path, out: &mut String) {
    let mut cur = (0.0f32, 0.0f32);
    let mut sub_start = (0.0f32, 0.0f32);
    for seg in path.segments() {
        match seg {
            PathSegment::MoveTo(p) => {
                out.push_str(&format!("{} {} m\n", fmt_num(p.x), fmt_num(p.y)));
                cur = (p.x, p.y);
                sub_start = cur;
            }
            PathSegment::LineTo(p) => {
                out.push_str(&format!("{} {} l\n", fmt_num(p.x), fmt_num(p.y)));
                cur = (p.x, p.y);
            }
            PathSegment::QuadTo(c, e) => {
                let c1x = cur.0 + 2.0 / 3.0 * (c.x - cur.0);
                let c1y = cur.1 + 2.0 / 3.0 * (c.y - cur.1);
                let c2x = e.x + 2.0 / 3.0 * (c.x - e.x);
                let c2y = e.y + 2.0 / 3.0 * (c.y - e.y);
                out.push_str(&format!("{} {} {} {} {} {} c\n", fmt_num(c1x), fmt_num(c1y), fmt_num(c2x), fmt_num(c2y), fmt_num(e.x), fmt_num(e.y)));
                cur = (e.x, e.y);
            }
            PathSegment::CubicTo(c1, c2, e) => {
                out.push_str(&format!("{} {} {} {} {} {} c\n", fmt_num(c1.x), fmt_num(c1.y), fmt_num(c2.x), fmt_num(c2.y), fmt_num(e.x), fmt_num(e.y)));
                cur = (e.x, e.y);
            }
            PathSegment::Close => {
                out.push_str("h\n");
                cur = sub_start;
            }
        }
    }
}

fn fr(v: u8) -> f32 {
    v as f32 / 255.0
}

fn gray3(r: u8, g: u8, b: u8) -> [f32; 3] {
    let l = (r as u32 * 2126 + g as u32 * 7152 + b as u32 * 722) / 10000;
    let y = l as f32 / 255.0;
    [y, y, y]
}

fn alpha_key(a: f32) -> u16 {
    (a.clamp(0.0, 1.0) * 255.0).round() as u16
}

fn emit_image(
    img: &usvg::Image,
    s: f32,
    th: f32,
    grayscale: bool,
    out: &mut String,
    images: &mut Vec<ImageRes>,
) -> bool {
    if !img.is_visible() {
        return true;
    }

    let size = img.size();
    let wf = size.width().ceil().max(1.0);
    let hf = size.height().ceil().max(1.0);

    let (colorspace, dct, data, smask, cmyk_inverted, icc) = if grayscale {
        match img.kind() {
            usvg::ImageKind::JPEG(raw)
            | usvg::ImageKind::PNG(raw)
            | usvg::ImageKind::GIF(raw)
            | usvg::ImageKind::WEBP(raw) => match decode_image_gray(raw.as_slice()) {
                Some((gray, smask)) => (ImageColorSpace::Gray, false, gray, smask, false, None),
                None => return false,
            },
            _ => return false,
        }
    } else {
        match img.kind() {
            usvg::ImageKind::JPEG(raw) => {
                let icc = jpeg_icc(raw.as_slice());
                match classify_jpeg(raw.as_slice()) {
                    Some(JpegClass::Gray) => (ImageColorSpace::Gray, true, raw.as_ref().clone(), None, false, icc),
                    Some(JpegClass::Rgb) => (ImageColorSpace::Rgb, true, raw.as_ref().clone(), None, false, icc),
                    Some(JpegClass::CmykInverted) => (ImageColorSpace::Cmyk, true, raw.as_ref().clone(), None, true, icc),
                    Some(JpegClass::CmykNonInverted) => (ImageColorSpace::Cmyk, true, raw.as_ref().clone(), None, false, icc),
                    _ => return false,
                }
            },
            usvg::ImageKind::PNG(raw) => {
                let PngMeta { icc, gama, srgb } = png_meta(raw.as_slice())
                    .unwrap_or(PngMeta { icc: None, gama: None, srgb: false });
                match icc {
                    Some(icc) => match icc_color_space(&icc) {
                        Some(ImageColorSpace::Gray) => match decode_image_gray_raw(raw.as_slice()) {
                            Some((data, smask)) => (ImageColorSpace::Gray, false, data, smask, false, Some(icc)),
                            None => return false,
                        },
                        Some(_) => match decode_image_rgba(raw.as_slice(), None) {
                            Some((data, smask)) => (ImageColorSpace::Rgb, false, data, smask, false, Some(icc)),
                            None => return false,
                        },
                        None => match decode_image_rgba(raw.as_slice(), None) {
                            Some((data, smask)) => (ImageColorSpace::Rgb, false, data, smask, false, None),
                            None => return false,
                        },
                    },
                    None if srgb => match decode_image_rgba(raw.as_slice(), None) {
                        Some((data, smask)) => (ImageColorSpace::Rgb, false, data, smask, false, None),
                        None => return false,
                    },
                    None => {
                        let gama_i = gama.unwrap_or(0) as i64;
                        let lut = if gama_i > 0 && (gama_i - 45455).abs() > 100 {
                            gamma_lut(gama_i as u32)
                        } else {
                            None
                        };
                        match decode_image_rgba(raw.as_slice(), lut.as_ref()) {
                            Some((data, smask)) => (ImageColorSpace::Rgb, false, data, smask, false, None),
                            None => return false,
                        }
                    }
                }
            }
            usvg::ImageKind::WEBP(raw) => {
                let icc = webp_icc(raw.as_slice()).and_then(|i| match icc_color_space(&i) {
                    Some(ImageColorSpace::Rgb) => Some(i),
                    _ => None,
                });
                match decode_image_rgba(raw.as_slice(), None) {
                    Some((data, smask)) => (ImageColorSpace::Rgb, false, data, smask, false, icc),
                    None => return false,
                }
            }
            usvg::ImageKind::GIF(raw) => match decode_image_rgba(raw.as_slice(), None) {
                Some((data, smask)) => (ImageColorSpace::Rgb, false, data, smask, false, None),
                None => return false,
            },
            _ => return false,
        }
    };

    let t = img.abs_transform();
    let m = usvg::Transform::from_row(
        s * t.sx,
        -s * t.ky,
        s * t.kx,
        -s * t.sy,
        s * t.tx,
        th - s * t.ty,
    );

    let name = format!("Im{}", images.len());
    images.push(ImageRes {
        name: name.clone(),
        width: wf as u32,
        height: hf as u32,
        colorspace,
        dct,
        data,
        smask,
        cmyk_inverted,
        icc,
    });

    out.push_str("q\n");
    out.push_str(&format!("{} {} {} {} {} {} cm\n", fmt_num(m.sx), fmt_num(m.ky), fmt_num(m.kx), fmt_num(m.sy), fmt_num(m.tx), fmt_num(m.ty)));
    out.push_str(&format!("{} 0 0 {} 0 {} cm\n", fmt_num(wf), fmt_num(-hf), fmt_num(hf)));
    out.push_str(&format!("/{} Do\n", name));
    out.push_str("Q\n");
    true
}

enum JpegClass {
    Gray,
    Rgb,
    CmykInverted,
    CmykNonInverted,
    Other,
}

fn classify_jpeg(raw: &[u8]) -> Option<JpegClass> {
    let mut nf: Option<u8> = None;
    let mut adobe_transform: Option<u8> = None;

    let mut i = 2usize;
    while i + 4 <= raw.len() {
        if raw[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = raw[i + 1];
        if marker == 0xFF {
            i += 1;
            continue;
        }
        if marker == 0xD8 || marker == 0xD9 {
            i += 2;
            continue;
        }
        if (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        let len = ((raw[i + 2] as usize) << 8) | (raw[i + 3] as usize);
        if len < 2 || i + 2 + len > raw.len() {
            break;
        }
        match marker {
            0xC0 | 0xC1 | 0xC2 | 0xC3 | 0xC5 | 0xC6 | 0xC7 | 0xC9 | 0xCA | 0xCB | 0xCD | 0xCE | 0xCF => {
                if len >= 11 {
                    nf = Some(raw[i + 9]);
                }
            }
            0xEE => {
                if len >= 14 && &raw[i + 4..i + 9] == b"Adobe" {
                    adobe_transform = Some(raw[i + 15]);
                }
            }
            0xDA => break,
            _ => {}
        }
        i += 2 + len;
    }

    let nf = nf?;
    match nf {
        1 => Some(JpegClass::Gray),
        3 => match adobe_transform {
            Some(2) => Some(JpegClass::Other),
            _ => Some(JpegClass::Rgb),
        },
        4 => match adobe_transform {
            Some(_) => Some(JpegClass::CmykInverted),
            None => Some(JpegClass::CmykNonInverted),
        },
        _ => Some(JpegClass::Other),
    }
}

fn jpeg_icc(raw: &[u8]) -> Option<Vec<u8>> {
    if raw.len() < 4 || raw[0] != 0xFF || raw[1] != 0xD8 {
        return None;
    }
    let mut chunks = BTreeMap::new();
    let mut total: u8 = 0;
    let mut i = 2usize;
    while i + 4 <= raw.len() {
        if raw[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = raw[i + 1];
        if marker == 0xD9 || marker == 0xDA {
            break;
        }
        if marker == 0xD8 || (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            i += 2;
            continue;
        }
        if marker == 0x00 || marker == 0xFF {
            i += 1;
            continue;
        }
        let len = ((raw[i + 2] as usize) << 8) | raw[i + 3] as usize;
        if len < 2 {
            break;
        }
        let seg_end = i + 2 + len;
        if seg_end > raw.len() {
            break;
        }
        if marker == 0xE2 {
            let payload = &raw[i + 4..seg_end];
            if payload.len() > 14 && &payload[..12] == b"ICC_PROFILE\0" {
                let seq = payload[12];
                let count = payload[13];
                if seq >= 1 && count >= 1 && seq <= count {
                    total = count;
                    chunks.insert(seq, payload[14..].to_vec());
                }
            }
        }
        i = seg_end;
    }
    if total == 0 {
        return None;
    }
    let mut icc = Vec::new();
    for seq in 1..=total {
        match chunks.get(&seq) {
            Some(c) => icc.extend_from_slice(c),
            None => return None,
        }
    }
    if icc.is_empty() { None } else { Some(icc) }
}

struct PngMeta {
    icc: Option<Vec<u8>>,
    gama: Option<u32>,
    srgb: bool,
}

fn png_meta(raw: &[u8]) -> Option<PngMeta> {
    const SIG: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if raw.len() < 8 || &raw[0..8] != SIG {
        return None;
    }
    let mut meta = PngMeta { icc: None, gama: None, srgb: false };
    let mut i = 8usize;
    while i + 8 <= raw.len() {
        let len = u32::from_be_bytes([raw[i], raw[i + 1], raw[i + 2], raw[i + 3]]) as usize;
        let tag = &raw[i + 4..i + 8];
        let data_start = i + 8;
        if data_start + len > raw.len() {
            break;
        }
        let payload = &raw[data_start..data_start + len];
        if tag == b"iCCP" {
            if meta.icc.is_none() {
                if let Some(nul) = payload.iter().position(|&b| b == 0) {
                    if nul + 1 < payload.len() && payload[nul + 1] == 0 {
                        meta.icc = miniz_oxide::inflate::decompress_to_vec_zlib(&payload[nul + 2..]).ok();
                    }
                }
            }
        } else if tag == b"gAMA" {
            if len >= 4 && meta.gama.is_none() {
                meta.gama = Some(u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]));
            }
        } else if tag == b"sRGB" {
            meta.srgb = true;
        }
        i = data_start + len + 4;
    }
    Some(meta)
}

fn icc_color_space(icc: &[u8]) -> Option<ImageColorSpace> {
    if icc.len() < 128 || &icc[36..40] != b"acsp" {
        return None;
    }
    match &icc[16..20] {
        b"RGB " => Some(ImageColorSpace::Rgb),
        b"GRAY" => Some(ImageColorSpace::Gray),
        _ => None,
    }
}

fn gamma_lut(gama: u32) -> Option<[u8; 256]> {
    let g = gama as f64 / 100000.0;
    if !(g > 0.0) || !g.is_finite() {
        return None;
    }
    let mut lut = [0u8; 256];
    for i in 0..=255 {
        let x = i as f64 / 255.0;
        let linear = x.powf(1.0 / g);
        let srgb = if linear <= 0.0031308 {
            12.92 * linear
        } else {
            1.055 * linear.powf(1.0 / 2.4) - 0.055
        };
        lut[i] = (srgb * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    Some(lut)
}

fn webp_icc(raw: &[u8]) -> Option<Vec<u8>> {
    if raw.len() < 12 || &raw[0..4] != b"RIFF" || &raw[8..12] != b"WEBP" {
        return None;
    }
    let mut i = 12usize;
    while i + 8 <= raw.len() {
        let tag = &raw[i..i + 4];
        let size = u32::from_le_bytes([raw[i + 4], raw[i + 5], raw[i + 6], raw[i + 7]]) as usize;
        let data_start = i + 8;
        if data_start + size > raw.len() {
            break;
        }
        if tag == b"ICCP" {
            return Some(raw[data_start..data_start + size].to_vec());
        }
        i = data_start + size + (size & 1);
    }
    None
}

fn decode_image_rgba(raw: &[u8], lut: Option<&[u8; 256]>) -> Option<(Vec<u8>, Option<Vec<u8>>)> {
    let img = image::load_from_memory(raw).ok()?;
    let rgba = img.to_rgba8();
    let mut rgb = Vec::with_capacity((rgba.width() as usize) * (rgba.height() as usize) * 3);
    let mut alpha = Vec::with_capacity((rgba.width() as usize) * (rgba.height() as usize));
    let mut has_alpha = false;
    for px in rgba.pixels() {
        let (r, g, b) = match lut {
            Some(l) => (l[px[0] as usize], l[px[1] as usize], l[px[2] as usize]),
            None => (px[0], px[1], px[2]),
        };
        rgb.push(r);
        rgb.push(g);
        rgb.push(b);
        alpha.push(px[3]);
        if px[3] != 255 {
            has_alpha = true;
        }
    }
    let rgb = miniz_oxide::deflate::compress_to_vec_zlib(&rgb, 6);
    let smask = if has_alpha {
        Some(miniz_oxide::deflate::compress_to_vec_zlib(&alpha, 6))
    } else {
        None
    };
    Some((rgb, smask))
}

fn decode_image_gray_raw(raw: &[u8]) -> Option<(Vec<u8>, Option<Vec<u8>>)> {
    let img = image::load_from_memory(raw).ok()?;
    let rgba = img.to_rgba8();
    let mut gray = Vec::with_capacity((rgba.width() as usize) * (rgba.height() as usize));
    let mut alpha = Vec::with_capacity((rgba.width() as usize) * (rgba.height() as usize));
    let mut has_alpha = false;
    for px in rgba.pixels() {
        gray.push(px[0]);
        alpha.push(px[3]);
        if px[3] != 255 {
            has_alpha = true;
        }
    }
    let gray = miniz_oxide::deflate::compress_to_vec_zlib(&gray, 6);
    let smask = if has_alpha {
        Some(miniz_oxide::deflate::compress_to_vec_zlib(&alpha, 6))
    } else {
        None
    };
    Some((gray, smask))
}

fn decode_image_gray(raw: &[u8]) -> Option<(Vec<u8>, Option<Vec<u8>>)> {
    let img = image::load_from_memory(raw).ok()?;
    let rgba = img.to_rgba8();
    let mut gray = Vec::with_capacity((rgba.width() as usize) * (rgba.height() as usize));
    let mut alpha = Vec::with_capacity((rgba.width() as usize) * (rgba.height() as usize));
    let mut has_alpha = false;
    for px in rgba.pixels() {
        let l = (px[0] as u32 * 2126 + px[1] as u32 * 7152 + px[2] as u32 * 722) / 10000;
        gray.push(l as u8);
        alpha.push(px[3]);
        if px[3] != 255 {
            has_alpha = true;
        }
    }
    let gray = miniz_oxide::deflate::compress_to_vec_zlib(&gray, 6);
    let smask = if has_alpha {
        Some(miniz_oxide::deflate::compress_to_vec_zlib(&alpha, 6))
    } else {
        None
    };
    Some((gray, smask))
}