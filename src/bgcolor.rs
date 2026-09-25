use anyhow::{bail, Result};
use image::DynamicImage;

#[allow(dead_code)]
pub(crate) const WHITE: [u8; 3] = [255, 255, 255];

const NAMED: [(&str, [u8; 3]); 10] = [
    ("white", [255, 255, 255]),
    ("black", [0, 0, 0]),
    ("red", [255, 0, 0]),
    ("green", [0, 128, 0]),
    ("blue", [0, 0, 255]),
    ("yellow", [255, 255, 0]),
    ("cyan", [0, 255, 255]),
    ("magenta", [255, 0, 255]),
    ("gray", [128, 128, 128]),
    ("grey", [128, 128, 128]),
];

pub(crate) const NONE: &str = "none";
pub(crate) const TRANSPARENT: &str = "transparent";

pub(crate) fn color_names() -> String {
    let mut names: Vec<&str> = NAMED.iter().map(|(name, _)| *name).collect();
    names.dedup();
    format!("{}, {}, #RRGGBB, #RGB", names.join(", "), TRANSPARENT)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn parse_hex(text: &str) -> Option<[u8; 3]> {
    let digits: Vec<u8> = text.as_bytes().iter().map(|b| hex_digit(*b)).collect::<Option<Vec<u8>>>()?;
    match digits.len() {
        3 => Some([
            digits[0] * 17,
            digits[1] * 17,
            digits[2] * 17,
        ]),
        6 => Some([
            digits[0] * 16 + digits[1],
            digits[2] * 16 + digits[3],
            digits[4] * 16 + digits[5],
        ]),
        _ => None,
    }
}

pub(crate) fn parse_color(text: &str) -> Result<Option<[u8; 3]>> {
    let value = text.trim().to_ascii_lowercase();
    if value == NONE || value == TRANSPARENT {
        return Ok(None);
    }
    if let Some((_, rgb)) = NAMED.iter().find(|(name, _)| *name == value) {
        return Ok(Some(*rgb));
    }
    if let Some(hex) = value.strip_prefix('#') {
        if let Some(rgb) = parse_hex(hex) {
            return Ok(Some(rgb));
        }
    }
    bail!(
        "unknown background '{}': expected {} or #RRGGBB",
        text,
        color_names()
    )
}

#[allow(dead_code)]
pub(crate) fn format_color(rgb: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2])
}

pub(crate) fn flatten_to_color(img: &DynamicImage, rgb: [u8; 3]) -> DynamicImage {
    if !img.color().has_alpha() {
        return img.to_rgb8().into();
    }
    let rgba = img.to_rgba8();
    let mut out = image::RgbImage::new(rgba.width(), rgba.height());
    for (src, dst) in rgba.pixels().zip(out.pixels_mut()) {
        let a = src[3] as u32;
        let inv = 255 - a;
        *dst = image::Rgb([
            ((src[0] as u32 * a + rgb[0] as u32 * inv) / 255) as u8,
            ((src[1] as u32 * a + rgb[1] as u32 * inv) / 255) as u8,
            ((src[2] as u32 * a + rgb[2] as u32 * inv) / 255) as u8,
        ]);
    }
    DynamicImage::ImageRgb8(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::RgbaImage;

    #[test]
    fn named_and_hex_colors_parse() {
        assert_eq!(parse_color("white").unwrap(), Some([255, 255, 255]));
        assert_eq!(parse_color("BLACK").unwrap(), Some([0, 0, 0]));
        assert_eq!(parse_color("#ff0000").unwrap(), Some([255, 0, 0]));
        assert_eq!(parse_color("#f00").unwrap(), Some([255, 0, 0]));
        assert_eq!(parse_color("none").unwrap(), None);
        assert_eq!(parse_color("transparent").unwrap(), None);
        assert!(parse_color("chartreuse").is_err());
        assert!(parse_color("#12345").is_err());
    }

    #[test]
    fn flatten_blends_alpha_onto_color() {
        let mut img = RgbaImage::new(2, 1);
        img.put_pixel(0, 0, image::Rgba([0, 0, 0, 0]));
        img.put_pixel(1, 0, image::Rgba([0, 0, 0, 255]));
        let flat = flatten_to_color(&DynamicImage::ImageRgba8(img), WHITE);
        let rgb = flat.to_rgb8();
        assert_eq!(rgb.get_pixel(0, 0).0, [255, 255, 255]);
        assert_eq!(rgb.get_pixel(1, 0).0, [0, 0, 0]);
    }

    #[test]
    fn flatten_keeps_opaque_images_untouched() {
        let img = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(1, 1, image::Rgb([7, 8, 9])));
        let flat = flatten_to_color(&img, WHITE);
        assert_eq!(flat.to_rgb8().get_pixel(0, 0).0, [7, 8, 9]);
    }
}
