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

pub(crate) fn make_page(img: &DynamicImage, config: &Config) -> Result<PdfPage> {
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
    build_pdf(&[page])
}

pub(crate) fn build_pdf(pages: &[PdfPage]) -> Result<Vec<u8>> {
    let mut pdf = Pdf::new();
    let catalog_id = Ref::new(1);
    let page_tree_id = Ref::new(2);

    pdf.catalog(catalog_id).pages(page_tree_id);

    let mut page_ids = Vec::with_capacity(pages.len());
    let mut content_ids = Vec::with_capacity(pages.len());
    let mut image_ids = Vec::with_capacity(pages.len());
    for i in 0..pages.len() {
        page_ids.push(Ref::new(3 + i as i32 * 3));
        content_ids.push(Ref::new(4 + i as i32 * 3));
        image_ids.push(Ref::new(5 + i as i32 * 3));
    }

    pdf.pages(page_tree_id).kids(page_ids.iter().copied()).count(pages.len() as i32);

    for (i, page) in pages.iter().enumerate() {
        let w = page.width as f32;
        let h = page.height as f32;
        let image_name = Name(b"Im");

        let mut image = pdf.image_xobject(image_ids[i], &page.data);
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

        let mut p = pdf.page(page_ids[i]);
        p.media_box(Rect::new(0.0, 0.0, w, h));
        p.parent(page_tree_id);
        p.contents(content_ids[i]);
        p.resources().x_objects().pair(image_name, image_ids[i]);
        p.finish();

        let mut content = Content::new();
        content.save_state();
        content.transform([w, 0.0, 0.0, h, 0.0, 0.0]);
        content.x_object(image_name);
        content.restore_state();
        pdf.stream(content_ids[i], &content.finish());
    }

    Ok(pdf.finish())
}
