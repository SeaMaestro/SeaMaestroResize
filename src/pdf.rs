use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use anyhow::Result;
use image::DynamicImage;
use miniz_oxide::deflate::compress_to_vec_zlib;

use crate::encode::encode_pdf_jpeg;
use crate::Config;

fn document_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:032x}", nanos)
}

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
    let mut sink = PdfSink::new(Vec::new())?;
    sink.write_catalog()?;
    sink.write_page(&page, 0)?;
    sink.finish(1)?;
    Ok(sink.into_inner())
}

pub(crate) struct PdfSink<W: Write> {
    w: W,
    pos: u64,
    offsets: BTreeMap<u32, u64>,
}

impl<W: Write> PdfSink<W> {
    pub(crate) fn new(mut w: W) -> Result<Self> {
        let header = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n";
        w.write_all(header)?;
        Ok(Self {
            w,
            pos: header.len() as u64,
            offsets: BTreeMap::new(),
        })
    }

    fn raw(&mut self, s: &str) -> Result<()> {
        self.w.write_all(s.as_bytes())?;
        self.pos += s.len() as u64;
        Ok(())
    }

    fn bytes(&mut self, b: &[u8]) -> Result<()> {
        self.w.write_all(b)?;
        self.pos += b.len() as u64;
        Ok(())
    }

    fn begin_obj(&mut self, id: u32) -> Result<()> {
        self.offsets.insert(id, self.pos);
        self.raw(&format!("{} 0 obj\n", id))
    }

    fn end_obj(&mut self) -> Result<()> {
        self.raw("endobj\n")
    }

    pub(crate) fn write_catalog(&mut self) -> Result<()> {
        self.begin_obj(1)?;
        self.raw("<< /Type /Catalog /Pages 2 0 R >>\n")?;
        self.end_obj()
    }

    pub(crate) fn write_page(&mut self, page: &PdfPage, index: usize) -> Result<()> {
        let page_id = 3 + 3 * index as u32;
        let content_id = page_id + 1;
        let image_id = page_id + 2;

        let cs = if page.gray { "/DeviceGray" } else { "/DeviceRGB" };
        let filter = if page.dct { "/DCTDecode" } else { "/FlateDecode" };

        self.begin_obj(page_id)?;
        self.raw(&format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Resources << /XObject << /Im {} 0 R >> >> /Contents {} 0 R >>\n",
            page.width, page.height, image_id, content_id
        ))?;
        self.end_obj()?;

        let content = format!("q\n{} 0 0 {} 0 0 cm\n/Im Do\nQ\n", page.width, page.height);
        self.begin_obj(content_id)?;
        self.raw(&format!("<< /Length {} >>\nstream\n", content.len()))?;
        self.raw(&content)?;
        self.raw("\nendstream\n")?;
        self.end_obj()?;

        self.begin_obj(image_id)?;
        self.raw(&format!(
            "<< /Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace {} /BitsPerComponent 8 /Filter {} /Length {} >>\nstream\n",
            page.width, page.height, cs, filter, page.data.len()
        ))?;
        self.bytes(&page.data)?;
        self.raw("\nendstream\n")?;
        self.end_obj()
    }

    pub(crate) fn finish(&mut self, page_count: usize) -> Result<u64> {
        let kids: Vec<String> = (0..page_count)
            .map(|i| format!("{} 0 R", 3 + 3 * i as u32))
            .collect();
        self.begin_obj(2)?;
        self.raw(&format!(
            "<< /Type /Pages /Kids [{}] /Count {} >>\n",
            kids.join(" "),
            page_count
        ))?;
        self.end_obj()?;

        let max_id = 2 + 3 * page_count as u32;
        let xref_start = self.pos;
        self.raw(&format!("xref\n0 {}\n", max_id + 1))?;
        self.raw("0000000000 65535 f\r\n")?;
        for id in 1..=max_id {
            let off = self.offsets.get(&id).copied().unwrap_or(0);
            self.raw(&format!("{:010} 00000 n\r\n", off))?;
        }
        let id = document_id();
        self.raw(&format!(
            "trailer\n<< /Size {} /Root 1 0 R /ID [<{}> <{}>] >>\nstartxref\n{}\n%%EOF\n",
            max_id + 1,
            id,
            id,
            xref_start
        ))?;
        self.w.flush()?;
        Ok(self.pos)
    }

    pub(crate) fn into_inner(self) -> W {
        self.w
    }
}

impl PdfSink<BufWriter<File>> {
    pub(crate) fn create(path: &Path) -> Result<Self> {
        let f = File::create(path)?;
        Self::new(BufWriter::new(f))
    }
}