use std::os::raw::{c_int, c_ulong};

const JPEG_HEADER_OK: c_int = 1;

unsafe extern "C-unwind" fn jpeg_error_exit(_cinfo: &mut mozjpeg_sys::jpeg_common_struct) {
    std::panic::panic_any("jpeg_fatal_error");
}

pub(crate) fn decode_scaled_jpeg(
    raw: &[u8],
    scale_num: u32,
    scale_denom: u32,
) -> Option<(image::DynamicImage, Option<Vec<u8>>)> {
    if raw.len() > c_ulong::MAX as usize {
        return None;
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        run_scaled_decode(raw, scale_num, scale_denom)
    }))
    .ok()
    .flatten()
}

unsafe fn run_scaled_decode(
    raw: &[u8],
    scale_num: u32,
    scale_denom: u32,
) -> Option<(image::DynamicImage, Option<Vec<u8>>)> {
    use mozjpeg_sys::*;

    let mut err: jpeg_error_mgr = std::mem::zeroed();
    jpeg_std_error(&mut err);
    err.error_exit = Some(jpeg_error_exit);

    let mut dinfo: jpeg_decompress_struct = std::mem::zeroed();
    dinfo.common.err = &mut err;
    jpeg_create_decompress(&mut dinfo);

    jpeg_mem_src(&mut dinfo, raw.as_ptr(), raw.len() as c_ulong);
    jpeg_save_markers(&mut dinfo, 0xE2, 0xFFFF);

    if jpeg_read_header(&mut dinfo, 1) != JPEG_HEADER_OK {
        jpeg_destroy_decompress(&mut dinfo);
        return None;
    }

    let mut icc: Option<Vec<u8>> = None;
    {
        let mut icc_ptr: *mut u8 = std::ptr::null_mut();
        let mut icc_len: c_uint = 0;
        if jpeg_read_icc_profile(&mut dinfo, &mut icc_ptr, &mut icc_len) != 0 {
            if !icc_ptr.is_null() && icc_len > 0 {
                icc = Some(std::slice::from_raw_parts(icc_ptr, icc_len as usize).to_vec());
            }
        }
        if !icc_ptr.is_null() {
            libc::free(icc_ptr as *mut libc::c_void);
        }
    }

    dinfo.scale_num = scale_num;
    dinfo.scale_denom = scale_denom;
    jpeg_calc_output_dimensions(&mut dinfo);

    if jpeg_start_decompress(&mut dinfo) == 0 {
        jpeg_destroy_decompress(&mut dinfo);
        return None;
    }

    let w = dinfo.output_width as usize;
    let h = dinfo.output_height as usize;
    let comps = dinfo.output_components as usize;
    if comps != 1 && comps != 3 {
        jpeg_destroy_decompress(&mut dinfo);
        return None;
    }

    let row_stride = w * comps;
    let mut buf = vec![0u8; row_stride * h];
    let mut rows: Vec<*mut u8> = (0..h).map(|i| buf.as_mut_ptr().add(i * row_stride)).collect();
    while dinfo.output_scanline < dinfo.output_height {
        let offset = dinfo.output_scanline as usize;
        let left = dinfo.output_height - dinfo.output_scanline;
        if jpeg_read_scanlines(&mut dinfo, rows.as_mut_ptr().add(offset), left) == 0 {
            break;
        }
    }
    let complete = dinfo.output_scanline >= dinfo.output_height;
    jpeg_finish_decompress(&mut dinfo);
    jpeg_destroy_decompress(&mut dinfo);

    if !complete {
        return None;
    }

    let img = match comps {
        1 => image::DynamicImage::ImageLuma8(image::GrayImage::from_raw(w as u32, h as u32, buf)?),
        _ => image::DynamicImage::ImageRgb8(image::RgbImage::from_raw(w as u32, h as u32, buf)?),
    };

    Some((img, icc))
}
