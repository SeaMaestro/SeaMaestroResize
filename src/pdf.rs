use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use anyhow::Result;
use image::DynamicImage;
use miniz_oxide::deflate::compress_to_vec_zlib;

use crate::encode::encode_pdf_jpeg;
use crate::svg_pdf::{ImageColorSpace, ShadingRes, VectorPage};
use crate::Config;

const SRGB_CALRGB: &str = "[/CalRGB << /WhitePoint [0.9505 1 1.089] /Gamma [2.2 2.2 2.2] /Matrix [0.4124 0.2126 0.0193 0.3576 0.7152 0.1192 0.1805 0.0722 0.9505] >>]";

fn document_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:032x}", nanos)
}

pub(crate) enum PdfPage {
    Raster {
        width: u32,
        height: u32,
        gray: bool,
        dct: bool,
        data: Vec<u8>,
    },
    Vector(VectorPage),
}

impl PdfPage {
    pub(crate) fn size_hint(&self) -> usize {
        match self {
            PdfPage::Raster { data, .. } => data.len(),
            PdfPage::Vector(vp) => vp.content.len(),
        }
    }
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
        Ok(PdfPage::Raster { width: w, height: h, gray, dct: false, data })
    } else {
        let data = encode_pdf_jpeg(img, config.quality)?;
        Ok(PdfPage::Raster { width: w, height: h, gray, dct: true, data })
    }
}

pub(crate) fn page_pdf(page: PdfPage) -> Result<Vec<u8>> {
    let mut sink = PdfSink::new(Vec::new())?;
    sink.write_catalog()?;
    sink.write_page(&page, 0)?;
    sink.finish(1)?;
    Ok(sink.into_inner())
}

pub(crate) fn single_page_pdf(img: &DynamicImage, config: &Config) -> Result<Vec<u8>> {
    page_pdf(make_page(img, config)?)
}

pub(crate) struct PdfSink<W: Write> {
    w: W,
    pos: u64,
    offsets: BTreeMap<u32, u64>,
    next_id: u32,
    page_ids: Vec<u32>,
}

impl<W: Write> PdfSink<W> {
    pub(crate) fn new(mut w: W) -> Result<Self> {
        let header = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n";
        w.write_all(header)?;
        Ok(Self {
            w,
            pos: header.len() as u64,
            offsets: BTreeMap::new(),
            next_id: 3,
            page_ids: Vec::new(),
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

    fn alloc_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub(crate) fn write_catalog(&mut self) -> Result<()> {
        self.begin_obj(1)?;
        self.raw("<< /Type /Catalog /Pages 2 0 R >>\n")?;
        self.end_obj()
    }

    pub(crate) fn write_page(&mut self, page: &PdfPage, _index: usize) -> Result<()> {
        match page {
            PdfPage::Raster { width, height, gray, dct, data } => {
                self.write_raster_page(*width, *height, *gray, *dct, data)
            }
            PdfPage::Vector(vp) => self.write_vector_page(vp),
        }
    }

    fn write_raster_page(
        &mut self,
        width: u32,
        height: u32,
        gray: bool,
        dct: bool,
        data: &[u8],
    ) -> Result<()> {
        let page_id = self.alloc_id();
        self.page_ids.push(page_id);
        let content_id = self.alloc_id();
        let image_id = self.alloc_id();

        let cs = if gray { "/DeviceGray" } else { SRGB_CALRGB };
        let filter = if dct { "/DCTDecode" } else { "/FlateDecode" };

        self.begin_obj(page_id)?;
        self.raw(&format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Resources << /XObject << /Im {} 0 R >> >> /Contents {} 0 R >>\n",
            width, height, image_id, content_id
        ))?;
        self.end_obj()?;

        let content = format!("q\n{} 0 0 {} 0 0 cm\n/Im Do\nQ\n", width, height);
        let content_data = compress_to_vec_zlib(content.as_bytes(), 6);
        self.begin_obj(content_id)?;
        self.raw(&format!("<< /Filter /FlateDecode /Length {} >>\nstream\n", content_data.len()))?;
        self.bytes(&content_data)?;
        self.raw("\nendstream\n")?;
        self.end_obj()?;

        self.begin_obj(image_id)?;
        self.raw(&format!(
            "<< /Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace {} /BitsPerComponent 8 /Filter {} /Length {} >>\nstream\n",
            width, height, cs, filter, data.len()
        ))?;
        self.bytes(data)?;
        self.raw("\nendstream\n")?;
        self.end_obj()
    }

    fn write_vector_page(&mut self, vp: &VectorPage) -> Result<()> {
        let mut gs_refs: Vec<(String, u32)> = Vec::new();
        for (name, fa, sa) in &vp.ext_gs {
            let id = self.alloc_id();
            gs_refs.push((name.clone(), id));
            let mut dict = String::from("<< /Type /ExtGState");
            if *fa < 0.999 {
                dict.push_str(&format!(" /ca {}", fa));
            }
            if *sa < 0.999 {
                dict.push_str(&format!(" /CA {}", sa));
            }
            dict.push_str(" >>\n");
            self.begin_obj(id)?;
            self.raw(&dict)?;
            self.end_obj()?;
        }

        let mut sh_refs: Vec<(String, u32)> = Vec::new();
        for sh in &vp.shadings {
            let sh_id = self.write_shading(sh)?;
            sh_refs.push((sh.name.clone(), sh_id));
        }

        let mut pat_refs: Vec<(String, u32)> = Vec::new();
        for pat in &vp.patterns {
            let sh_obj_id = sh_refs
                .iter()
                .find(|(n, _)| n == &pat.shading)
                .map(|(_, i)| *i)
                .ok_or_else(|| anyhow::anyhow!("shading '{}' not found", pat.shading))?;
            let shading = vp.shadings.iter().find(|s| &s.name == &pat.shading).unwrap();
            let m = shading.matrix;

            let id = self.alloc_id();
            pat_refs.push((pat.name.clone(), id));
            self.begin_obj(id)?;
            self.raw(&format!(
                "<< /Type /Pattern /PatternType 2 /Matrix [{} {} {} {} {} {}] /Shading {} 0 R >>\n",
                m[0], m[1], m[2], m[3], m[4], m[5], sh_obj_id
            ))?;
            self.end_obj()?;
        }

        let mut img_refs: Vec<(String, u32)> = Vec::new();
        for img in &vp.images {
            let smask_opt_id = if img.smask.is_some() {
                Some(self.alloc_id())
            } else {
                None
            };
            let id = self.alloc_id();
            img_refs.push((img.name.clone(), id));

            let device_cs = match img.colorspace {
                ImageColorSpace::Gray => "/DeviceGray",
                ImageColorSpace::Rgb => "/DeviceRGB",
                ImageColorSpace::Cmyk => "/DeviceCMYK",
            };
            let filter = if img.dct { "/DCTDecode" } else { "/FlateDecode" };
            let icc_opt_id = if img.icc.is_some() {
                Some(self.alloc_id())
            } else {
                None
            };
            let cs = match (icc_opt_id, img.colorspace) {
                (Some(icc_id), _) => format!("[/ICCBased {} 0 R]", icc_id),
                (None, ImageColorSpace::Rgb) => SRGB_CALRGB.to_string(),
                (None, ImageColorSpace::Gray) => "/DeviceGray".to_string(),
                (None, ImageColorSpace::Cmyk) => "/DeviceCMYK".to_string(),
            };

            if let (Some(smask_data), Some(smask_id)) = (&img.smask, smask_opt_id) {
                self.begin_obj(smask_id)?;
                self.raw(&format!(
                    "<< /Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode /Length {} >>\nstream\n",
                    img.width, img.height, smask_data.len()
                ))?;
                self.bytes(smask_data)?;
                self.raw("\nendstream\n")?;
                self.end_obj()?;
            }

            if let (Some(icc_data), Some(icc_id)) = (&img.icc, icc_opt_id) {
                let n = match img.colorspace {
                    ImageColorSpace::Gray => 1,
                    ImageColorSpace::Rgb => 3,
                    ImageColorSpace::Cmyk => 4,
                };
                self.begin_obj(icc_id)?;
                self.raw(&format!(
                    "<< /N {} /Alternate {} /Length {} >>\nstream\n",
                    n, device_cs, icc_data.len()
                ))?;
                self.bytes(icc_data)?;
                self.raw("\nendstream\n")?;
                self.end_obj()?;
            }

            let mut dict = format!(
                "<< /Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace {} ",
                img.width, img.height, cs
            );
            if let ImageColorSpace::Cmyk = img.colorspace {
                dict.push_str(if img.cmyk_inverted {
                    "/Decode [1 0 1 0 1 0 1 0] "
                } else {
                    "/Decode [0 1 0 1 0 1 0 1] "
                });
            }
            dict.push_str(&format!("/BitsPerComponent 8 /Filter {} /Length {}", filter, img.data.len()));
            if let Some(smask_id) = smask_opt_id {
                dict.push_str(&format!(" /SMask {} 0 R", smask_id));
            }
            dict.push_str(" >>\nstream\n");

            self.begin_obj(id)?;
            self.raw(&dict)?;
            self.bytes(&img.data)?;
            self.raw("\nendstream\n")?;
            self.end_obj()?;
        }

        let mut resources = String::from("<< ");
        if !gs_refs.is_empty() {
            resources.push_str("/ExtGState <<");
            for (name, id) in &gs_refs {
                resources.push_str(&format!(" /{} {} 0 R", name, id));
            }
            resources.push_str(" >> ");
        }
        if !sh_refs.is_empty() {
            resources.push_str("/Shading <<");
            for (name, id) in &sh_refs {
                resources.push_str(&format!(" /{} {} 0 R", name, id));
            }
            resources.push_str(" >> ");
        }
        if !pat_refs.is_empty() {
            resources.push_str("/Pattern <<");
            for (name, id) in &pat_refs {
                resources.push_str(&format!(" /{} {} 0 R", name, id));
            }
            resources.push_str(" >> ");
        }
        if !img_refs.is_empty() {
            resources.push_str("/XObject <<");
            for (name, id) in &img_refs {
                resources.push_str(&format!(" /{} {} 0 R", name, id));
            }
            resources.push_str(" >> ");
        }
        resources.push_str("/ColorSpace << /DefaultRGB ");
        resources.push_str(SRGB_CALRGB);
        resources.push_str(" >> ");
        resources.push_str(">>");

        let page_id = self.alloc_id();
        self.page_ids.push(page_id);
        let content_id = self.alloc_id();

        self.begin_obj(page_id)?;
        self.raw(&format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Resources {} /Contents {} 0 R >>\n",
            vp.width, vp.height, resources, content_id
        ))?;
        self.end_obj()?;

        let content_data = compress_to_vec_zlib(&vp.content, 6);
        self.begin_obj(content_id)?;
        self.raw(&format!("<< /Filter /FlateDecode /Length {} >>\nstream\n", content_data.len()))?;
        self.bytes(&content_data)?;
        self.raw("\nendstream\n")?;
        self.end_obj()
    }

    fn function2_dict(c0: [f32; 3], c1: [f32; 3]) -> String {
        format!("<< /FunctionType 2 /Domain [0 1] /C0 [{} {} {}] /C1 [{} {} {}] /N 1 >>\n", c0[0], c0[1], c0[2], c1[0], c1[1], c1[2])
    }

    fn write_shading(&mut self, sh: &ShadingRes) -> Result<u32> {
        let n = sh.stops.len();

        let func_ref = if n == 2 {
            let func_id = self.alloc_id();
            let (_, c0) = sh.stops[0];
            let (_, c1) = sh.stops[1];
            self.begin_obj(func_id)?;
            self.raw(&Self::function2_dict(c0, c1))?;
            self.end_obj()?;
            format!("{} 0 R", func_id)
        } else {
            let mut sub_refs: Vec<String> = Vec::with_capacity(n - 1);
            for i in 0..n - 1 {
                let (_, c0) = sh.stops[i];
                let (_, c1) = sh.stops[i + 1];
                let sub_id = self.alloc_id();
                self.begin_obj(sub_id)?;
                self.raw(&Self::function2_dict(c0, c1))?;
                self.end_obj()?;
                sub_refs.push(format!("{} 0 R", sub_id));
            }

            let mut bounds: Vec<String> = Vec::new();
            for (off, _) in &sh.stops[1..n - 1] {
                bounds.push(format!("{}", off));
            }
            let mut encode: Vec<String> = Vec::with_capacity((n - 1) * 2);
            for i in 0..(n - 1) * 2 {
                encode.push(if i % 2 == 0 { "0".to_string() } else { "1".to_string() });
            }

            let stitch_id = self.alloc_id();
            self.begin_obj(stitch_id)?;
            self.raw(&format!(
                "<< /FunctionType 3 /Domain [0 1] /Functions [{}] /Bounds [{}] /Encode [{}] >>\n",
                sub_refs.join(" "),
                bounds.join(" "),
                encode.join(" ")
            ))?;
            self.end_obj()?;
            format!("{} 0 R", stitch_id)
        };

        let c = sh.coords;
        let coords_str = if sh.kind == 2 {
            format!("{} {} {} {}", c[0], c[1], c[2], c[3])
        } else {
            format!("{} {} {} {} {} {}", c[0], c[1], c[2], c[3], c[4], c[5])
        };

        let sh_id = self.alloc_id();
        self.begin_obj(sh_id)?;
        self.raw(&format!(
            "<< /ShadingType {} /ColorSpace {} /Coords [{}] /Function {} /Extend [true true] >>\n",
            sh.kind, SRGB_CALRGB, coords_str, func_ref
        ))?;
        self.end_obj()?;

        Ok(sh_id)
    }

pub(crate) fn finish(&mut self, _page_count: usize) -> Result<u64> {
        let kids: Vec<String> = self.page_ids.iter().map(|id| format!("{} 0 R", id)).collect();
        self.begin_obj(2)?;
        self.raw(&format!(
            "<< /Type /Pages /Kids [{}] /Count {} >>\n",
            kids.join(" "),
            self.page_ids.len()
        ))?;
        self.end_obj()?;

        let max_id = self.next_id - 1;
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