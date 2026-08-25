use anyhow::Result;
use image::DynamicImage;
use miniz_oxide::deflate::compress_to_vec_zlib;
use pdf_writer::{Content, Filter, Finish, Name, Pdf, Rect, Ref};

use crate::encode::encode_pdf_jpeg;
use crate::Config;

pub(crate) struct PdfPage {
    pub width: u32,
    pub height: u32,
    pub gray: bool,
    pub dct: bool,
    pub data: Vec<u8>,
}

fn flatten_alpha_white(img: &DynamicImage) -> DynamicImage {
    let rgba = img.to_rgba8();
    let mut rgb = image::RgbImage::new(rgba.width(), rgba.height());
    for (src, dst) in rgba.pixels().zip(rgb.pixels_mut()) {
        let a = src[3] as u32;
        let r = (src[0] as u32 * a + 255 * (255 - a)) / 255;
        let g = (src[1] as u32 * a + 255 * (255 - a)) / 255;
        let b = (src[2] as u32 * a + 255 * (255 - a)) / 255;
        *dst = image::Rgb([r as u8, g as u8, b as u8]);
    }
    DynamicImage::ImageRgb8(rgb)
}

pub(crate) fn make_page(img: &DynamicImage, config: &Config) -> Result<PdfPage> {
    let owned;
    let img: &DynamicImage = if img.color().has_alpha() {
        owned = flatten_alpha_white(img);
        &owned
    } else {
        img
    };
    let (w, h) = (img.width(), img.height());
    let gray = img.as_luma8().is_some();
    if config.lossless {
        let raw: Vec<u8> = if let Some(g) = img.as_luma8() {
            g.as_raw().to_vec()
        } else {
            img.to_rgb8().into_raw()
        };
        let data = compress_to_vec_zlib(&raw, 6);
        Ok(PdfPage { width: w, height: h, gray, dct: false, data })
    } else {
        let data = encode_pdf_jpeg(img, config.quality)?;
        Ok(PdfPage { width: w, height: h, gray, dct: true, data })
    }
}

pub(crate) fn single_page_pdf(img: &DynamicImage, config: &Config) -> Result<Vec<u8>> {
    let page = make_page(img, config)?;
    let mut builder = PdfBuilder::new();
    builder.add_page(&page);
    Ok(builder.finish())
}

pub(crate) struct PdfBuilder {
    pdf: Pdf,
    page_tree_id: Ref,
    next_ref: i32,
    page_refs: Vec<Ref>,
}

impl PdfBuilder {
    pub(crate) fn new() -> Self {
        let mut pdf = Pdf::new();
        pdf.catalog(Ref::new(1)).pages(Ref::new(2));
        Self {
            pdf,
            page_tree_id: Ref::new(2),
            next_ref: 3,
            page_refs: Vec::new(),
        }
    }

    pub(crate) fn add_page(&mut self, page: &PdfPage) {
        let page_id = Ref::new(self.next_ref);
        let content_id = Ref::new(self.next_ref + 1);
        let image_id = Ref::new(self.next_ref + 2);
        self.next_ref += 3;

        let w = page.width as f32;
        let h = page.height as f32;
        let image_name = Name(b"Im");

        let mut image = self.pdf.image_xobject(image_id, &page.data);
        image.filter(if page.dct { Filter::DctDecode } else { Filter::FlateDecode });
        image.width(page.width as i32);
        image.height(page.height as i32);
        if page.gray {
            image.color_space().device_gray();
        } else {
            image.color_space().device_rgb();
        }
        image.bits_per_component(8);
        image.finish();

        let mut p = self.pdf.page(page_id);
        p.media_box(Rect::new(0.0, 0.0, w, h));
        p.parent(self.page_tree_id);
        p.contents(content_id);
        p.resources().x_objects().pair(image_name, image_id);
        p.finish();

        let mut content = Content::new();
        content.save_state();
        content.transform([w, 0.0, 0.0, h, 0.0, 0.0]);
        content.x_object(image_name);
        content.restore_state();
        self.pdf.stream(content_id, &content.finish());

        self.page_refs.push(page_id);
    }

    pub(crate) fn finish(mut self) -> Vec<u8> {
        self.pdf
            .pages(self.page_tree_id)
            .kids(self.page_refs.iter().copied())
            .count(self.page_refs.len() as i32);
        self.pdf.finish()
    }
}