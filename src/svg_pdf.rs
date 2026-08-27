#![allow(dead_code)]

use std::collections::BTreeMap;

use resvg::usvg;
use usvg::tiny_skia_path::PathSegment;

pub(crate) struct VectorPage {
    pub width: u32,
    pub height: u32,
    pub content: Vec<u8>,
    pub ext_gs: Vec<(String, f32, f32)>,
}

pub(crate) fn build_vector_page(tree: &usvg::Tree, target_w: u32, target_h: u32) -> Option<VectorPage> {
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

    let root = tree.root();
    if root.should_isolate() {
        return None;
    }

    for node in root.children() {
        if !emit_node(node, s, th, &mut out, &mut ext_gs, &mut gs_names) {
            return None;
        }
    }

    Some(VectorPage {
        width: target_w.max(1),
        height: target_h.max(1),
        content: out.into_bytes(),
        ext_gs,
    })
}

fn emit_node(
    node: &usvg::Node,
    s: f32,
    th: f32,
    out: &mut String,
    ext_gs: &mut Vec<(String, f32, f32)>,
    gs_names: &mut BTreeMap<(u16, u16), String>,
) -> bool {
    match node {
        usvg::Node::Group(g) => {
            if g.should_isolate() {
                return false;
            }
            for child in g.children() {
                if !emit_node(child, s, th, out, ext_gs, gs_names) {
                    return false;
                }
            }
            true
        }
        usvg::Node::Path(p) => emit_path(p, s, th, out, ext_gs, gs_names),
        _ => false,
    }
}

fn emit_path(
    p: &usvg::Path,
    s: f32,
    th: f32,
    out: &mut String,
    ext_gs: &mut Vec<(String, f32, f32)>,
    gs_names: &mut BTreeMap<(u16, u16), String>,
) -> bool {
    if !p.is_visible() {
        return true;
    }

    let fill = p.fill();
    let stroke = p.stroke();
    if fill.is_none() && stroke.is_none() {
        return true;
    }

    let fill_info = match fill {
        None => None,
        Some(f) => match f.paint() {
            usvg::Paint::Color(c) => Some((c, f.rule(), f.opacity().get())),
            _ => return false,
        },
    };
    let stroke_info = match stroke {
        None => None,
        Some(st) => match st.paint() {
            usvg::Paint::Color(c) => Some((c, st.opacity().get())),
            _ => return false,
        },
    };

    let fill_alpha = fill_info.map(|(_, _, a)| a).unwrap_or(1.0);
    let stroke_alpha = stroke_info.map(|(_, a)| a).unwrap_or(1.0);

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

    match (fill_info, stroke_info, stroke) {
        (Some((fc, rule, _)), Some((sc, _)), Some(st)) => {
            set_stroke_params(st, stroke_scale, out);
            out.push_str(&format!("{} {} {} rg\n", fr(fc.red), fr(fc.green), fr(fc.blue)));
            out.push_str(&format!("{} {} {} RG\n", fr(sc.red), fr(sc.green), fr(sc.blue)));
            out.push_str(if rule == usvg::FillRule::EvenOdd { "B*\n" } else { "B\n" });
        }
        (Some((fc, rule, _)), None, _) => {
            out.push_str(&format!("{} {} {} rg\n", fr(fc.red), fr(fc.green), fr(fc.blue)));
            out.push_str(if rule == usvg::FillRule::EvenOdd { "f*\n" } else { "f\n" });
        }
        (None, Some((sc, _)), Some(st)) => {
            set_stroke_params(st, stroke_scale, out);
            out.push_str(&format!("{} {} {} RG\n", fr(sc.red), fr(sc.green), fr(sc.blue)));
            out.push_str("S\n");
        }
        _ => {}
    }

    out.push_str("Q\n");
    true
}

fn set_stroke_params(st: &usvg::Stroke, scale: f32, out: &mut String) {
    out.push_str(&format!("{} w\n", st.width().get() * scale));
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
    out.push_str(&format!("{} M\n", st.miterlimit().get()));
    if let Some(dash) = st.dasharray() {
        if !dash.is_empty() {
            let list: Vec<String> = dash.iter().map(|v| format!("{}", v * scale)).collect();
            out.push_str(&format!("[{}] {} d\n", list.join(" "), st.dashoffset() * scale));
        }
    }
}

fn emit_path_data(path: &usvg::tiny_skia_path::Path, out: &mut String) {
    let mut cur = (0.0f32, 0.0f32);
    let mut sub_start = (0.0f32, 0.0f32);
    for seg in path.segments() {
        match seg {
            PathSegment::MoveTo(p) => {
                out.push_str(&format!("{} {} m\n", p.x, p.y));
                cur = (p.x, p.y);
                sub_start = cur;
            }
            PathSegment::LineTo(p) => {
                out.push_str(&format!("{} {} l\n", p.x, p.y));
                cur = (p.x, p.y);
            }
            PathSegment::QuadTo(c, e) => {
                let c1x = cur.0 + 2.0 / 3.0 * (c.x - cur.0);
                let c1y = cur.1 + 2.0 / 3.0 * (c.y - cur.1);
                let c2x = e.x + 2.0 / 3.0 * (c.x - e.x);
                let c2y = e.y + 2.0 / 3.0 * (c.y - e.y);
                out.push_str(&format!("{} {} {} {} {} {} c\n", c1x, c1y, c2x, c2y, e.x, e.y));
                cur = (e.x, e.y);
            }
            PathSegment::CubicTo(c1, c2, e) => {
                out.push_str(&format!("{} {} {} {} {} {} c\n", c1.x, c1.y, c2.x, c2.y, e.x, e.y));
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

fn alpha_key(a: f32) -> u16 {
    (a.clamp(0.0, 1.0) * 255.0).round() as u16
}