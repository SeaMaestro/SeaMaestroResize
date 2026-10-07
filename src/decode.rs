use anyhow::{Context, Result};
use std::io::Read;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use flate2::read::GzDecoder;
use image::ImageDecoder;
use libavif_sys::*;
use libjxl_sys::*;

use crate::encode::avif_threads;
use crate::msg;
use crate::usable_ram;
use crate::util::{read16, read32};

use resvg::{tiny_skia, usvg};

const GIB: u64 = 1024 * 1024 * 1024;
const HARD_CAP: u64 = 8 * GIB;
const RAM_FRACTION: f64 = 0.6;

struct RuntimeLimits {
    max_alloc: u64,
    #[allow(dead_code)]
    budget: u64,
}

fn runtime_limits() -> &'static RuntimeLimits {
    static LIMITS: OnceLock<RuntimeLimits> = OnceLock::new();
    LIMITS.get_or_init(|| {
        let usable = ((usable_ram() as f64 * RAM_FRACTION) as u64).max(256 * 1024 * 1024);
        RuntimeLimits {
            max_alloc: usable.min(HARD_CAP),
            budget: usable,
        }
    })
}

pub(crate) struct MemBudget {
    total: u64,
    used: Mutex<u64>,
    cv: Condvar,
}

pub(crate) struct MemPermit<'a> {
    pub(crate) budget: &'a MemBudget,
    pub(crate) need: u64,
}

impl Drop for MemPermit<'_> {
    fn drop(&mut self) {
        self.budget.release(self.need);
    }
}

impl MemBudget {
    pub(crate) fn acquire(&self, need: u64) -> bool {
        if need > self.total {
            return false;
        }
        let mut used = self.used.lock().unwrap_or_else(|e| e.into_inner());
        while *used + need > self.total {
            used = self.cv.wait(used).unwrap_or_else(|e| e.into_inner());
        }
        *used += need;
        true
    }

    pub(crate) fn try_acquire(&self, need: u64) -> bool {
        if need > self.total {
            return false;
        }
        let mut used = self.used.lock().unwrap_or_else(|e| e.into_inner());
        if *used + need > self.total {
            return false;
        }
        *used += need;
        true
    }

    fn release(&self, need: u64) {
        let need = need.min(self.total.max(1));
        let mut used = self.used.lock().unwrap();
        *used = used.saturating_sub(need);
        self.cv.notify_all();
    }
}

pub(crate) fn mem_budget() -> &'static MemBudget {
    static BUDGET: OnceLock<MemBudget> = OnceLock::new();
    BUDGET.get_or_init(|| MemBudget {
        total: runtime_limits().budget,
        used: Mutex::new(0),
        cv: Condvar::new(),
    })
}

pub(crate) fn budget_total() -> u64 {
    runtime_limits().budget
}

pub(crate) fn probe_dims(raw: &[u8]) -> Option<(u32, u32)> {
    if looks_like_svg(raw) {
        return probe_svg_dims(raw);
    }
    if raw.len() >= 24 && raw.starts_with(b"\x89PNG\r\n\x1a\n") {
        let w = u32::from_be_bytes([raw[16], raw[17], raw[18], raw[19]]);
        let h = u32::from_be_bytes([raw[20], raw[21], raw[22], raw[23]]);
        return Some((w, h));
    }
    if raw.len() >= 10 && (raw.starts_with(b"GIF87a") || raw.starts_with(b"GIF89a")) {
        let w = u16::from_le_bytes([raw[6], raw[7]]) as u32;
        let h = u16::from_le_bytes([raw[8], raw[9]]) as u32;
        return Some((w, h));
    }
    if raw.starts_with(b"BM") {
        let header = if raw.len() >= 18 {
            u32::from_le_bytes([raw[14], raw[15], raw[16], raw[17]])
        } else {
            0
        };
        if header == 12 && raw.len() >= 22 {
            let w = u16::from_le_bytes([raw[18], raw[19]]) as u32;
            let h = u16::from_le_bytes([raw[20], raw[21]]) as u32;
            return Some((w, h));
        }
        if header >= 40 && raw.len() >= 26 {
            let w = i32::from_le_bytes([raw[18], raw[19], raw[20], raw[21]]).unsigned_abs();
            let h = i32::from_le_bytes([raw[22], raw[23], raw[24], raw[25]]).unsigned_abs();
            return Some((w, h));
        }
        return None;
    }
    if raw.len() >= 14 && raw.starts_with(b"qoif") {
        let w = u32::from_be_bytes([raw[4], raw[5], raw[6], raw[7]]);
        let h = u32::from_be_bytes([raw[8], raw[9], raw[10], raw[11]]);
        return Some((w, h));
    }
    if raw.len() >= 3 && raw[0] == 0xFF && raw[1] == 0xD8 && raw[2] == 0xFF {
        return jpeg_dims(raw);
    }
    if raw.len() >= 30 && raw.starts_with(b"RIFF") && &raw[8..12] == b"WEBP" {
        return webp_dims(raw);
    }
    if is_jxl(raw) {
        if let Some(d) = probe_jxl_dims(raw) {
            return Some(d);
        }
    }
    if raw.len() >= 12 && &raw[4..12] == b"ftypcrx " {
        return bmff_tkhd_dims(raw);
    }
    if is_avif(raw) {
        if let Some(d) = probe_avif_dims(raw) {
            return Some(d);
        }
    }
    if raw.len() >= 12 && &raw[4..8] == b"ftyp" {
        if let Ok(ctx) = libheif_rs::HeifContext::read_from_bytes(raw) {
            if let Ok(handle) = ctx.primary_image_handle() {
                return Some((handle.width(), handle.height()));
            }
        }
    }
    if raw.len() >= 8 && (raw.starts_with(b"II*\0") || raw.starts_with(b"MM\0*")) {
        return tiff_dims(raw);
    }
    if is_raw_bytes(raw) {
        return probe_raw_dims(raw);
    }
    if raw.len() >= 6 && raw[0..4] == [0, 0, 1, 0] {
        return ico_dims(raw);
    }
    None
}

fn jpeg_dims(raw: &[u8]) -> Option<(u32, u32)> {
    use zune_core::bytestream::ZCursor;
    use zune_core::options::DecoderOptions;
    use zune_jpeg::JpegDecoder;

    let mut decoder = JpegDecoder::new(ZCursor::new(raw));
    decoder.set_options(
        DecoderOptions::new_fast()
            .set_max_width(65535)
            .set_max_height(65535),
    );
    decoder.decode_headers().ok()?;
    let info = decoder.info()?;
    Some((u32::from(info.width), u32::from(info.height)))
}

fn webp_dims(raw: &[u8]) -> Option<(u32, u32)> {
    match &raw[12..16] {
        b"VP8X" if raw.len() >= 30 => {
            let w = 1 + u32::from_le_bytes([raw[24], raw[25], raw[26], 0]);
            let h = 1 + u32::from_le_bytes([raw[27], raw[28], raw[29], 0]);
            Some((w, h))
        }
        b"VP8L" if raw.len() >= 25 => {
            let v = u32::from_le_bytes([raw[21], raw[22], raw[23], raw[24]]);
            Some(((v & 0x3FFF) + 1, ((v >> 14) & 0x3FFF) + 1))
        }
        b"VP8 " if raw.len() >= 30 && raw[23..26] == [0x9D, 0x01, 0x2A] => {
            let w = u16::from_le_bytes([raw[26], raw[27]]) & 0x3FFF;
            let h = u16::from_le_bytes([raw[28], raw[29]]) & 0x3FFF;
            Some((w as u32, h as u32))
        }
        _ => None,
    }
}

fn tiff_dims(raw: &[u8]) -> Option<(u32, u32)> {
    let little = raw.starts_with(b"II*\0");
    let ifd0 = read32(raw, 4, little)? as usize;
    let n = read16(raw, ifd0, little)? as usize;
    let mut w = None;
    let mut h = None;
    for i in 0..n {
        let e = ifd0 + 2 + i * 12;
        if e + 12 > raw.len() { break; }
        let tag = read16(raw, e, little)?;
        let typ = read16(raw, e + 2, little)?;
        let val = match typ {
            3 => read16(raw, e + 8, little)? as u32,
            4 => read32(raw, e + 8, little)?,
            _ => continue,
        };
        if tag == 0x0100 { w = Some(val); }
        else if tag == 0x0101 { h = Some(val); }
    }
    Some((w?, h?))
}

fn probe_raw_dims(raw: &[u8]) -> Option<(u32, u32)> {
    let source = rawler::rawsource::RawSource::new_from_slice(raw);
    let rawimage = rawler::decode_dummy(&source).ok()?;
    Some((rawimage.width as u32, rawimage.height as u32))
}

fn ico_dims(raw: &[u8]) -> Option<(u32, u32)> {
    let count = u16::from_le_bytes([raw[4], raw[5]]) as usize;
    let mut mw = 0u32;
    let mut mh = 0u32;
    for i in 0..count {
        let off = 6 + i * 16;
        if off + 2 > raw.len() { break; }
        let w = if raw[off] == 0 { 256 } else { raw[off] as u32 };
        let h = if raw[off + 1] == 0 { 256 } else { raw[off + 1] as u32 };
        mw = mw.max(w);
        mh = mh.max(h);
    }
    if mw == 0 || mh == 0 { None } else { Some((mw, mh)) }
}

fn bmff_tkhd_dims(raw: &[u8]) -> Option<(u32, u32)> {
    let mut best: Option<(u32, u32)> = None;
    bmff_tkhd_scan(raw, 0, raw.len() as u64, 0, &mut best);
    best
}

fn bmff_tkhd_scan(
    raw: &[u8],
    start: u64,
    end: u64,
    depth: u32,
    best: &mut Option<(u32, u32)>,
) {
    if depth > 8 {
        return;
    }
    let mut off = start;
    while off + 8 <= end {
        let size32 = match raw.get(off as usize..off as usize + 4) {
            Some(s) => u32::from_be_bytes([s[0], s[1], s[2], s[3]]),
            None => return,
        };
        let ftype = match raw.get(off as usize + 4..off as usize + 8) {
            Some(s) => s,
            None => return,
        };
        let (hdr, size) = if size32 == 1 {
            let ls = match raw.get(off as usize + 8..off as usize + 16) {
                Some(s) => u64::from_be_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]),
                None => return,
            };
            (16u64, ls)
        } else if size32 == 0 {
            (8u64, end - off)
        } else {
            (8u64, size32 as u64)
        };
        if size < hdr || off + size > end {
            return;
        }
        let box_end = off + size;
        let payload = off + hdr;

        if ftype == b"tkhd" && payload < box_end {
            let ver = raw[payload as usize];
            let woff = if ver == 1 { payload + 88 } else { payload + 76 };
            if woff + 8 <= box_end {
                let wf = u32::from_be_bytes([
                    raw[woff as usize],
                    raw[woff as usize + 1],
                    raw[woff as usize + 2],
                    raw[woff as usize + 3],
                ]);
                let hf = u32::from_be_bytes([
                    raw[woff as usize + 4],
                    raw[woff as usize + 5],
                    raw[woff as usize + 6],
                    raw[woff as usize + 7],
                ]);
                let (w, h) = (wf >> 16, hf >> 16);
                if w > 0 && h > 0 {
                    let area = (w as u64) * (h as u64);
                    let better = match *best {
                        Some((bw, bh)) => (bw as u64) * (bh as u64) < area,
                        None => true,
                    };
                    if better {
                        *best = Some((w, h));
                    }
                }
            }
        }

        if is_bmff_container(ftype) {
            let child = if ftype == b"meta" { payload + 4 } else { payload };
            if child <= box_end {
                bmff_tkhd_scan(raw, child, box_end, depth + 1, best);
            }
        }
        off = box_end;
    }
}

fn is_bmff_container(ftype: &[u8]) -> bool {
    const CONTAINERS: [&[u8]; 18] = [
        b"moov", b"trak", b"mdia", b"minf", b"stbl", b"edts", b"udta",
        b"moof", b"traf", b"mfra", b"meta", b"iprp", b"ipco", b"dinf",
        b"wave", b"ilst", b"ipro", b"keys",
    ];
    CONTAINERS.contains(&ftype)
}

fn jpeg_exif(raw: &[u8]) -> Option<Vec<u8>> {
    if raw.len() < 4 {
        return None;
    }
    let mut i = 2usize;
    while i + 4 <= raw.len() {
        if raw[i] != 0xFF {
            return None;
        }
        let marker = raw[i + 1];
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        if marker == 0xD9 || marker == 0xDA {
            return None;
        }
        if marker == 0xFF {
            i += 1;
            continue;
        }
        let len = u16::from_be_bytes([raw[i + 2], raw[i + 3]]) as usize;
        if len < 2 || i + 2 + len > raw.len() {
            return None;
        }
        if marker == 0xE1 && len >= 8 && &raw[i + 4..i + 10] == b"Exif\0\0" {
            return Some(raw[i + 10..i + 2 + len].to_vec());
        }
        i += 2 + len;
    }
    None
}

fn png_exif(raw: &[u8]) -> Option<Vec<u8>> {
    if raw.len() < 8 || &raw[0..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let mut i = 8usize;
    while i + 12 <= raw.len() {
        let len = u32::from_be_bytes([raw[i], raw[i + 1], raw[i + 2], raw[i + 3]]) as usize;
        let data_end = i.checked_add(8)?.checked_add(len)?;
        let chunk_end = data_end.checked_add(4)?;
        if chunk_end > raw.len() {
            return None;
        }
        if &raw[i + 4..i + 8] == b"eXIf" {
            return Some(raw[i + 8..data_end].to_vec());
        }
        i = chunk_end;
    }
    None
}

fn webp_exif(raw: &[u8]) -> Option<Vec<u8>> {
    if raw.len() < 20 || &raw[0..4] != b"RIFF" || &raw[8..12] != b"WEBP" {
        return None;
    }
    let mut i = 12usize;
    while i + 8 <= raw.len() {
        let fourcc = &raw[i..i + 4];
        let size = u32::from_le_bytes([raw[i + 4], raw[i + 5], raw[i + 6], raw[i + 7]]) as usize;
        let start = i.checked_add(8)?;
        let end = start.checked_add(size)?;
        if end > raw.len() {
            return None;
        }
        if fourcc == b"EXIF" {
            return Some(raw[start..end].to_vec());
        }
        i = end + (size & 1);
    }
    None
}

fn probe_jxl_dims(raw: &[u8]) -> Option<(u32, u32)> {
    unsafe {
        let dec = JxlDecoderCreate(std::ptr::null());
        if dec.is_null() {
            return None;
        }
        let res = JxlDecoderSubscribeEvents(dec, JxlDecoderStatus_JXL_DEC_BASIC_INFO);
        if res != JxlDecoderStatus_JXL_DEC_SUCCESS {
            JxlDecoderDestroy(dec);
            return None;
        }
        let res = JxlDecoderSetInput(dec, raw.as_ptr(), raw.len());
        if res != JxlDecoderStatus_JXL_DEC_SUCCESS {
            JxlDecoderDestroy(dec);
            return None;
        }
        JxlDecoderCloseInput(dec);
        let mut info = std::mem::zeroed::<JxlBasicInfo>();
        loop {
            let status = JxlDecoderProcessInput(dec);
            if status == JxlDecoderStatus_JXL_DEC_ERROR {
                JxlDecoderDestroy(dec);
                return None;
            }
            if status & JxlDecoderStatus_JXL_DEC_BASIC_INFO != 0
                && JxlDecoderGetBasicInfo(dec, &mut info) == JxlDecoderStatus_JXL_DEC_SUCCESS
            {
                break;
            }
            if status == JxlDecoderStatus_JXL_DEC_SUCCESS {
                break;
            }
        }
        let w = info.xsize;
        let h = info.ysize;
        JxlDecoderDestroy(dec);
        if w == 0 || h == 0 {
            return None;
        }
        Some((w, h))
    }
}

pub(crate) fn jxl_exif(raw: &[u8]) -> Option<Vec<u8>> {
    const CHUNK: usize = 1 << 20;
    const MAX_BOX: u64 = 256 * 1024 * 1024;

    unsafe {
        let dec = JxlDecoderCreate(std::ptr::null());
        if dec.is_null() {
            return None;
        }
        let events = JxlDecoderStatus_JXL_DEC_BOX | JxlDecoderStatus_JXL_DEC_BOX_COMPLETE;
        if JxlDecoderSubscribeEvents(dec, events) != JxlDecoderStatus_JXL_DEC_SUCCESS {
            JxlDecoderDestroy(dec);
            return None;
        }
        if JxlDecoderSetInput(dec, raw.as_ptr(), raw.len()) != JxlDecoderStatus_JXL_DEC_SUCCESS {
            JxlDecoderDestroy(dec);
            return None;
        }
        JxlDecoderCloseInput(dec);

        let exif_type: [libc::c_char; 4] = [
            b'E' as libc::c_char,
            b'x' as libc::c_char,
            b'i' as libc::c_char,
            b'f' as libc::c_char,
        ];

        let mut acc: Vec<u8> = Vec::new();
        let mut chunk: Vec<u8> = vec![0u8; CHUNK];
        let mut capacity: usize = 0;
        let mut total: u64 = 0;
        let mut received: u64 = 0;
        let mut reading = false;
        let mut finished = false;
        let mut last_status = i32::MIN;
        let mut stalled = 0u32;

        loop {
            let status = JxlDecoderProcessInput(dec);
            if status == JxlDecoderStatus_JXL_DEC_ERROR
                || status == JxlDecoderStatus_JXL_DEC_SUCCESS
            {
                break;
            }
            if status == last_status {
                stalled += 1;
                if stalled > 64 {
                    break;
                }
            } else {
                last_status = status;
                stalled = 0;
            }

            if status == JxlDecoderStatus_JXL_DEC_BOX {
                let mut box_type = [0i8; 4];
                let is_exif = JxlDecoderGetBoxType(dec, &mut box_type, JXL_FALSE as libc::c_int)
                    == JxlDecoderStatus_JXL_DEC_SUCCESS
                    && box_type == exif_type;
                let mut box_size: u64 = 0;
                if is_exif
                    && JxlDecoderGetBoxSizeContents(dec, &mut box_size)
                        == JxlDecoderStatus_JXL_DEC_SUCCESS
                    && box_size > 0
                    && box_size <= MAX_BOX
                {
                    acc.clear();
                    reading = true;
                    total = box_size;
                    received = 0;
                    capacity = (CHUNK as u64).min(box_size) as usize;
                    JxlDecoderReleaseBoxBuffer(dec);
                    if JxlDecoderSetBoxBuffer(dec, chunk.as_mut_ptr(), capacity)
                        != JxlDecoderStatus_JXL_DEC_SUCCESS
                    {
                        reading = false;
                    }
                } else {
                    reading = false;
                    JxlDecoderReleaseBoxBuffer(dec);
                    JxlDecoderSetBoxBuffer(dec, std::ptr::null_mut(), 0);
                }
            } else if status == JxlDecoderStatus_JXL_DEC_BOX_NEED_MORE_OUTPUT
                || status == JxlDecoderStatus_JXL_DEC_BOX_COMPLETE
            {
                if reading {
                    let unused = JxlDecoderReleaseBoxBuffer(dec);
                    let written = capacity.saturating_sub(unused);
                    acc.extend_from_slice(&chunk[..written]);
                    received += written as u64;
                    if status == JxlDecoderStatus_JXL_DEC_BOX_COMPLETE || received >= total {
                        reading = false;
                        finished = true;
                        JxlDecoderSetBoxBuffer(dec, std::ptr::null_mut(), 0);
                    } else {
                        capacity = ((total - received).min(CHUNK as u64)) as usize;
                        if JxlDecoderSetBoxBuffer(dec, chunk.as_mut_ptr(), capacity)
                            != JxlDecoderStatus_JXL_DEC_SUCCESS
                        {
                            reading = false;
                        }
                    }
                } else {
                    JxlDecoderReleaseBoxBuffer(dec);
                }
                if finished {
                    break;
                }
            }
        }
        JxlDecoderDestroy(dec);

        if !finished || acc.len() < 4 {
            return None;
        }
        let off = u32::from_be_bytes([acc[0], acc[1], acc[2], acc[3]]) as usize;
        acc.get(4 + off..).map(|b| b.to_vec())
    }
}

pub(crate) fn extract_exif(raw: &[u8]) -> Option<Vec<u8>> {
    if raw.len() >= 3 && raw[0] == 0xFF && raw[1] == 0xD8 && raw[2] == 0xFF {
        return jpeg_exif(raw);
    }
    if raw.len() >= 8 && &raw[0..8] == b"\x89PNG\r\n\x1a\n" {
        return png_exif(raw);
    }
    if raw.len() >= 12 && &raw[0..4] == b"RIFF" && &raw[8..12] == b"WEBP" {
        return webp_exif(raw);
    }
    if is_heif(raw) {
        return heif_exif(raw);
    }
    if (raw.len() >= 8 && &raw[4..8] == b"JXL ") || raw.starts_with(&[0xFF, 0x0A]) {
        return jxl_exif(raw);
    }
    None
}

// ── decode_image ──────────────────────────────────────────────

pub(crate) fn is_jxl(raw: &[u8]) -> bool {
    (raw.len() >= 8 && &raw[4..8] == b"JXL ") || raw.starts_with(&[0xFF, 0x0A])
}

struct JxlDecoderGuard(*mut JxlDecoder);

impl Drop for JxlDecoderGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { JxlDecoderDestroy(self.0) };
        }
    }
}

struct JxlRunnerGuard(*mut libc::c_void);

impl Drop for JxlRunnerGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { JxlThreadParallelRunnerDestroy(self.0) };
        }
    }
}

pub(crate) struct JxlPrepared {
    raw: Vec<u8>,
    w: u32,
    h: u32,
    grayscale: bool,
    alpha: bool,
}

impl JxlPrepared {
    pub(crate) fn prepare(raw: &[u8]) -> Option<Self> {
        if !is_jxl(raw) {
            return None;
        }
        unsafe {
            let dec = JxlDecoderCreate(std::ptr::null());
            if dec.is_null() {
                return None;
            }
            let res = JxlDecoderSubscribeEvents(dec, JxlDecoderStatus_JXL_DEC_BASIC_INFO);
            if res != JxlDecoderStatus_JXL_DEC_SUCCESS {
                JxlDecoderDestroy(dec);
                return None;
            }
            let res = JxlDecoderSetInput(dec, raw.as_ptr(), raw.len());
            if res != JxlDecoderStatus_JXL_DEC_SUCCESS {
                JxlDecoderDestroy(dec);
                return None;
            }
            JxlDecoderCloseInput(dec);

            let mut info = std::mem::zeroed::<JxlBasicInfo>();
            loop {
                let status = JxlDecoderProcessInput(dec);
                if status == JxlDecoderStatus_JXL_DEC_ERROR {
                    JxlDecoderDestroy(dec);
                    return None;
                }
                if status & JxlDecoderStatus_JXL_DEC_BASIC_INFO != 0
                    && JxlDecoderGetBasicInfo(dec, &mut info) == JxlDecoderStatus_JXL_DEC_SUCCESS
                {
                    break;
                }
                if status == JxlDecoderStatus_JXL_DEC_SUCCESS {
                    break;
                }
            }

            let w = info.xsize;
            let h = info.ysize;
            JxlDecoderDestroy(dec);

            if w == 0 || h == 0 || w > 32768 || h > 32768 {
                return None;
            }

            Some(Self {
                raw: raw.to_vec(),
                w,
                h,
                grayscale: info.num_color_channels == 1,
                alpha: info.num_extra_channels > 0,
            })
        }
    }

    pub(crate) fn dims(&self) -> (u32, u32) {
        (self.w, self.h)
    }

    #[allow(clippy::type_complexity)]
    pub(crate) fn decode(self) -> Result<(image::DynamicImage, Option<Vec<u8>>, Option<Vec<u8>>)> {
        let Self { raw, w, h, grayscale, alpha } = self;
        let exif_blob = jxl_exif(&raw);
        unsafe {
            let dec_ptr = JxlDecoderCreate(std::ptr::null());
            if dec_ptr.is_null() {
                anyhow::bail!("JXL decode failed: JxlDecoderCreate returned NULL");
            }
            let runner_ptr = JxlThreadParallelRunnerCreate(std::ptr::null(), avif_threads());
            let runner = JxlRunnerGuard(runner_ptr);
            let dec = JxlDecoderGuard(dec_ptr);
            if !runner.0.is_null() {
                JxlDecoderSetParallelRunner(dec.0, Some(JxlThreadParallelRunner), runner.0);
            }

            let events = JxlDecoderStatus_JXL_DEC_COLOR_ENCODING
                | JxlDecoderStatus_JXL_DEC_FULL_IMAGE;
            let res = JxlDecoderSubscribeEvents(dec.0, events);
            if res != JxlDecoderStatus_JXL_DEC_SUCCESS {
                anyhow::bail!("JXL decode failed: JxlDecoderSubscribeEvents: {}", res);
            }

            let res = JxlDecoderSetInput(dec.0, raw.as_ptr(), raw.len());
            if res != JxlDecoderStatus_JXL_DEC_SUCCESS {
                anyhow::bail!("JXL decode failed: JxlDecoderSetInput: {}", res);
            }
            JxlDecoderCloseInput(dec.0);

            let channels: u32 = match (grayscale, alpha) {
                (false, false) => 3,
                (false, true) => 4,
                (true, false) => 1,
                (true, true) => 2,
            };
            let format = JxlPixelFormat {
                num_channels: channels,
                data_type: JxlDataType_JXL_TYPE_UINT8,
                endianness: JxlEndianness_JXL_NATIVE_ENDIAN,
                align: 0,
            };

            let mut pixels: Vec<u8> = Vec::new();
            let mut icc: Option<Vec<u8>> = None;

            let mut last_status = i32::MIN;
            let mut stalled = 0u32;
            loop {
                let status = JxlDecoderProcessInput(dec.0);
                if status == JxlDecoderStatus_JXL_DEC_ERROR {
                    anyhow::bail!("JXL decode failed: JxlDecoderProcessInput: {}", status);
                }
                if status == JxlDecoderStatus_JXL_DEC_SUCCESS {
                    break;
                }
                if status == last_status {
                    stalled += 1;
                    if stalled > 64 {
                        anyhow::bail!("JXL decode stalled on status {}", status);
                    }
                } else {
                    last_status = status;
                    stalled = 0;
                }
                if status & JxlDecoderStatus_JXL_DEC_COLOR_ENCODING != 0 {
                    let mut icc_size: usize = 0;
                    if JxlDecoderGetICCProfileSize(
                        dec.0,
                        JxlColorProfileTarget_JXL_COLOR_PROFILE_TARGET_DATA,
                        &mut icc_size,
                    ) == JxlDecoderStatus_JXL_DEC_SUCCESS
                        && icc_size > 0
                    {
                        let mut buf = vec![0u8; icc_size];
                        if JxlDecoderGetColorAsICCProfile(
                            dec.0,
                            JxlColorProfileTarget_JXL_COLOR_PROFILE_TARGET_DATA,
                            buf.as_mut_ptr(),
                            icc_size,
                        ) == JxlDecoderStatus_JXL_DEC_SUCCESS
                        {
                            icc = Some(buf);
                        }
                    }
                }

                if status & JxlDecoderStatus_JXL_DEC_NEED_IMAGE_OUT_BUFFER != 0 {
                    let mut size: usize = 0;
                    JxlDecoderImageOutBufferSize(dec.0, &format, &mut size);
                    pixels = vec![0u8; size];
                    let res = JxlDecoderSetImageOutBuffer(
                        dec.0,
                        &format,
                        pixels.as_mut_ptr() as *mut libc::c_void,
                        size,
                    );
                    if res != JxlDecoderStatus_JXL_DEC_SUCCESS {
                        anyhow::bail!("JXL decode failed: JxlDecoderSetImageOutBuffer: {}", res);
                    }
                }
                if status & JxlDecoderStatus_JXL_DEC_FULL_IMAGE != 0 {
                    break;
                }
            }

            let exif = exif_blob;

            let img = match (grayscale, alpha) {
                (false, false) => image::RgbImage::from_raw(w, h, pixels)
                    .map(image::DynamicImage::ImageRgb8)
                    .context("JXL buffer size mismatch")?,
                (false, true) => image::RgbaImage::from_raw(w, h, pixels)
                    .map(image::DynamicImage::ImageRgba8)
                    .context("JXL buffer size mismatch")?,
                (true, false) => image::GrayImage::from_raw(w, h, pixels)
                    .map(image::DynamicImage::ImageLuma8)
                    .context("JXL buffer size mismatch")?,
                (true, true) => image::GrayAlphaImage::from_raw(w, h, pixels)
                    .map(image::DynamicImage::ImageLumaA8)
                    .context("JXL buffer size mismatch")?,
            };

            Ok((img, icc, exif))
        }
    }
}

/// The 18-byte TGA header is the only way to recognise a TGA that arrives without a file name (a
/// pipe). It is consulted last and only when there is no name at all, so it can never override a
/// real extension, and its four checks never see a file another format already claimed.
fn looks_like_tga(raw: &[u8]) -> bool {
    if raw.len() < 18 {
        return false;
    }
    matches!(raw[2], 1 | 2 | 3 | 9 | 10 | 11)
        && matches!(raw[16], 8 | 15 | 16 | 24 | 32)
        && u16::from_le_bytes([raw[12], raw[13]]) > 0
        && u16::from_le_bytes([raw[14], raw[15]]) > 0
}

#[allow(clippy::type_complexity)]
pub(crate) fn decode_image(
    raw: &[u8],
    path: Option<&Path>,
    target: Option<(u32, u32)>,
    svg: Option<&usvg::Tree>,
) -> Result<(image::DynamicImage, Option<Vec<u8>>, Option<Vec<u8>>)> {
    if let Some(tree) = svg {
        let (tw, th) = match target {
            Some(d) => d,
            None => {
                let size = tree.size();
                (size.width().ceil() as u32, size.height().ceil() as u32)
            }
        };
        let img = decode_svg(tree, tw, th).map_err(anyhow::Error::msg)?;
        return Ok((img, None, None));
    }

    let exif = extract_exif(raw);
    if raw.len() >= 3 && raw[0] == 0xFF && raw[1] == 0xD8 && raw[2] == 0xFF {
        if let Some((img, icc)) = decode_jpeg_fast(raw) {
            return Ok((img, icc, exif));
        }
    }
    if is_raw_bytes(raw) {
        let raw_img = match path {
            Some(p) => decode_raw(p).ok(),
            None => decode_raw_bytes(raw).ok(),
        };
        if let Some(img) = raw_img {
            return Ok((img, None, exif));
        }
    }
    // `avif` goes first: a file whose primary brand is `mif1` but which lists `avif` among its
    // compatible brands is an AVIF — and `mif1` matches `is_heif` as well, while libheif cannot decode
    // AV1. The more specific claim has to win; if the avif decoder fails we fall through to heif,
    // which then gets its attempt.
    if is_avif(raw) {
        if let Ok((img, icc, avif_exif)) = decode_avif(raw) {
            return Ok((img, icc, avif_exif));
        }
    }
    if is_heif(raw) {
        if let Ok(img) = decode_heif_manual(raw, path) {
            return Ok((img, None, exif));
        }
    }
    if let Ok((img, icc)) = decode_with_limits(raw) {
        return Ok((img, icc, exif));
    }
    if let Some(p) = path {
        if let Ok(img) = decode_raw(p) {
            return Ok((img, None, exif));
        }
    }
    // TGA is the only promised input format with no magic bytes, so content sniffing cannot recognise
    // it. A file name wins: `.tga` is decoded as TGA, and a named `.tga` that fails is *not* retried
    // through the header path (one attempt, one message). Only a nameless stream (a pipe) is allowed
    // to fall back to the header check. Neither path ever touches `probe_dims`, so a random binary is
    // never sized as a TGA container.
    let named_tga = path.is_some_and(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("tga")));
    let nameless_tga = path.is_none() && looks_like_tga(raw);
    if named_tga || nameless_tga {
        if let Ok((img, icc)) = decode_with_limits_fmt(raw, Some(image::ImageFormat::Tga)) {
            return Ok((img, icc, exif));
        }
    }
    anyhow::bail!("{}", msg().err_unsupported)
}

pub(crate) fn decode_with_limits(raw: &[u8]) -> image::ImageResult<(image::DynamicImage, Option<Vec<u8>>)> {
    decode_with_limits_fmt(raw, None)
}

fn decode_with_limits_fmt(
    raw: &[u8],
    format: Option<image::ImageFormat>,
) -> image::ImageResult<(image::DynamicImage, Option<Vec<u8>>)> {
    // `ImageReader::with_format` is an associated function taking the reader first (not a method):
    // see image-0.25.10/src/io/image_reader_type.rs:101.
    let mut reader = match format {
        Some(f) => image::ImageReader::with_format(std::io::Cursor::new(raw), f),
        None => image::ImageReader::new(std::io::Cursor::new(raw)).with_guessed_format()?,
    };
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(32768);
    limits.max_image_height = Some(32768);
    limits.max_alloc = Some(runtime_limits().max_alloc);
    reader.limits(limits);
    let mut decoder = reader.into_decoder()?;
    let keep_icc = matches!(
        decoder.original_color_type(),
        image::ExtendedColorType::Rgb8
            | image::ExtendedColorType::Rgba8
            | image::ExtendedColorType::Rgb16
            | image::ExtendedColorType::Rgba16
            | image::ExtendedColorType::Rgb32F
            | image::ExtendedColorType::Rgba32F
            | image::ExtendedColorType::Bgr8
            | image::ExtendedColorType::Bgra8
    );
    let icc = if keep_icc { decoder.icc_profile()? } else { None };
    let img = image::DynamicImage::from_decoder(decoder)?;
    Ok((img, icc))
}

fn decode_jpeg_fast(raw: &[u8]) -> Option<(image::DynamicImage, Option<Vec<u8>>)> {
    let (dw, dh) = jpeg_dims(raw)?;
    let worst = (dw as u64).saturating_mul(dh as u64).saturating_mul(4);
    if worst > runtime_limits().max_alloc {
        return None;
    }

    let mut decoder = zune_jpeg::JpegDecoder::new(zune_core::bytestream::ZCursor::new(raw));
    let data = decoder.decode().ok()?;
    let info = decoder.info()?;
    let px = decoder.output_colorspace()?;
    let (w, h) = (info.width as u32, info.height as u32);
    let icc = match px {
        zune_core::colorspace::ColorSpace::RGB | zune_core::colorspace::ColorSpace::RGBA => {
            decoder.icc_profile()
        }
        _ => None,
    };
    let img = match px {
        zune_core::colorspace::ColorSpace::RGB => {
            image::DynamicImage::ImageRgb8(image::RgbImage::from_raw(w, h, data)?)
        }
        zune_core::colorspace::ColorSpace::Luma => {
            image::DynamicImage::ImageLuma8(image::GrayImage::from_raw(w, h, data)?)
        }
        zune_core::colorspace::ColorSpace::RGBA => {
            image::DynamicImage::ImageRgba8(image::RgbaImage::from_raw(w, h, data)?)
        }
        _ => return None,
    };
    Some((img, icc))
}

fn svg_font_db() -> Arc<fontdb::Database> {
    static DB: OnceLock<Arc<fontdb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        Arc::new(db)
    })
    .clone()
}

fn svg_options(path: Option<&Path>) -> usvg::Options<'static> {
    usvg::Options {
        fontdb: svg_font_db(),
        resources_dir: path.and_then(|p| p.parent().map(Path::to_path_buf)),
        ..Default::default()
    }
}

/// A gzip payload is taken as SVGZ only when its decompressed head contains markup: a plain
/// .gz/.tar.gz archive is not an image and must fall through to the normal format checks instead
/// of being reported as "SVG parse failed". A real SVGZ (including one that trips the expansion
/// limit) always contains markup in its head, so it keeps its own message.
fn gzipped_head_mentions_markup(raw: &[u8]) -> bool {
    const HEAD: usize = 256;
    let mut head = Vec::with_capacity(HEAD);
    if GzDecoder::new(raw)
        .take(HEAD as u64)
        .read_to_end(&mut head)
        .is_err()
    {
        return false;
    }
    // `<svg` (not a bare `<`) is the practical marker: every real SVGZ has that root element in
    // its head, while a plain archive (even one with '<' in a tar file name) or a gzipped non-SVG
    // document simply has nothing to find.
    find_subslice(&head, 0, b"<svg").is_some()
}

pub(crate) fn looks_like_svg(raw: &[u8]) -> bool {
    if raw.starts_with(&[0x1f, 0x8b]) {
        return gzipped_head_mentions_markup(raw);
    }
    let raw = raw.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(raw);
    let first = raw.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(raw.len());
    let head = &raw[first..];
    head.starts_with(b"<svg") || head.starts_with(b"<?xml")
}

pub(crate) fn raster_need(w: u32, h: u32) -> u64 {
    (w as u64)
        .saturating_mul(h as u64)
        .saturating_mul(4)
        .clamp(1, runtime_limits().max_alloc)
}

fn estimate_image_peak(w: u32, h: u32, raw_len: usize) -> Option<u64> {
    let px = (w as u64).checked_mul(h as u64)?;
    px.checked_mul(12)?.checked_add(raw_len as u64)
}

pub(crate) const MAX_SVG_DEPTH: usize = 32;

fn collect_raster_images(group: &usvg::Group, depth: usize, out: &mut Vec<(u32, u32, usize, bool)>) -> Option<()> {
    if depth > MAX_SVG_DEPTH {
        return None;
    }
    for node in group.children() {
        collect_node_raster_images(node, depth + 1, out)?;
    }
    Some(())
}

fn collect_node_raster_images(node: &usvg::Node, depth: usize, out: &mut Vec<(u32, u32, usize, bool)>) -> Option<()> {
    if depth > MAX_SVG_DEPTH {
        return None;
    }
    if let usvg::Node::Image(img) = node {
        let w = img.size().width().ceil().max(1.0) as u32;
        let h = img.size().height().ceil().max(1.0) as u32;
        match img.kind() {
            usvg::ImageKind::JPEG(raw) => {
                out.push((w, h, raw.len(), true));
            }
            usvg::ImageKind::PNG(raw)
            | usvg::ImageKind::GIF(raw)
            | usvg::ImageKind::WEBP(raw) => {
                out.push((w, h, raw.len(), false));
            }
            usvg::ImageKind::SVG(tree) => {
                collect_raster_images(tree.root(), depth + 1, out)?;
            }
        }
    }
    if let usvg::Node::Group(g) = node {
        if let Some(clip) = g.clip_path() {
            collect_raster_images(clip.root(), depth + 1, out)?;
            if let Some(sub) = clip.clip_path() {
                collect_raster_images(sub.root(), depth + 1, out)?;
            }
        }
        if let Some(mask) = g.mask() {
            collect_raster_images(mask.root(), depth + 1, out)?;
        }
        collect_raster_images(g, depth + 1, out)?;
    }
    let mut ok = true;
    node.subroots(|sub| {
        if ok
            && collect_raster_images(sub, depth + 1, out).is_none() {
                ok = false;
            }
    });
    if ok { Some(()) } else { None }
}

pub(crate) fn vector_peak_cap() -> u64 {
    runtime_limits().max_alloc.saturating_mul(3) / 4
}

pub(crate) fn vector_peak_estimate(tree: &usvg::Tree, grayscale: bool) -> Option<u64> {
    let mut images = Vec::new();
    collect_raster_images(tree.root(), 0, &mut images)?;
    let mut sum = 0u64;
    for (w, h, raw_len, is_jpeg) in images {
        let est = if is_jpeg && !grayscale {
            u64::try_from(raw_len).ok()?
        } else {
            estimate_image_peak(w, h, raw_len)?
        };
        sum = sum.checked_add(est)?;
    }
    Some(sum)
}

pub(crate) fn max_raster_image_peak(tree: &usvg::Tree) -> Option<u64> {
    let mut images = Vec::new();
    collect_raster_images(tree.root(), 0, &mut images)?;
    let mut max = 0u64;
    for (w, h, raw_len, _) in images {
        let est = estimate_image_peak(w, h, raw_len)?;
        max = max.max(est);
    }
    Some(max)
}

pub(crate) struct ParsedSvg {
    pub tree: usvg::Tree,
    pub width: u32,
    pub height: u32,
}

fn find_subslice(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    hay.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

fn skip_tag(raw: &[u8], mut i: usize) -> Option<usize> {
    let n = raw.len();
    let mut quote: Option<u8> = None;
    while i < n {
        let b = raw[i];
        if let Some(q) = quote {
            if b == q {
                quote = None;
            }
        } else if b == b'"' || b == b'\'' {
            quote = Some(b);
        } else if b == b'>' {
            return Some(i + 1);
        }
        i += 1;
    }
    None
}

fn skip_open_tag(raw: &[u8], from: usize) -> Option<(usize, bool)> {
    let close = skip_tag(raw, from)?;
    let self_closing = close > from + 1 && raw[close - 2] == b'/';
    Some((close, self_closing))
}

fn svg_xml_max_depth(raw: &[u8]) -> Option<usize> {
    let mut depth = 0usize;
    let mut max = 0usize;
    let mut i = 0usize;
    let n = raw.len();
    while i < n {
        if raw[i] != b'<' {
            i += 1;
            continue;
        }
        if i + 1 >= n {
            return None;
        }
        match raw[i + 1] {
            b'?' => {
                let close = find_subslice(raw, i + 2, b"?>")?;
                i = close + 2;
            }
            b'!' => {
                if raw[i..].starts_with(b"<!--") {
                    let close = find_subslice(raw, i + 4, b"-->")?;
                    i = close + 3;
                } else if raw[i..].starts_with(b"<![CDATA[") {
                    let close = find_subslice(raw, i + 9, b"]]>")?;
                    i = close + 3;
                } else {
                    i = skip_tag(raw, i + 2)?;
                }
            }
            b'/' => {
                i = skip_tag(raw, i + 2)?;
                depth = depth.checked_sub(1)?;
            }
            _ => {
                let (close, self_closing) = skip_open_tag(raw, i + 1)?;
                if self_closing {
                    max = max.max(depth.saturating_add(1));
                } else {
                    depth = depth.checked_add(1)?;
                    max = max.max(depth);
                }
                i = close;
            }
        }
    }
    if depth != 0 {
        return None;
    }
    Some(max)
}

const MAX_SVG_DECOMPRESSED: u64 = 256 * 1024 * 1024;

pub(crate) fn parse_svg(raw: &[u8], path: Option<&Path>) -> anyhow::Result<ParsedSvg> {
    let mut gz_buf = Vec::new();
    let svg_bytes: &[u8] = if raw.starts_with(&[0x1f, 0x8b]) {
        GzDecoder::new(raw)
            .take(MAX_SVG_DECOMPRESSED + 1)
            .read_to_end(&mut gz_buf)
            .map_err(|_| anyhow::anyhow!("SVG decompression failed"))?;
        if gz_buf.len() as u64 > MAX_SVG_DECOMPRESSED {
            anyhow::bail!(
                "SVG expands past the {} MB limit — refusing to decompress",
                MAX_SVG_DECOMPRESSED / (1024 * 1024)
            );
        }
        &gz_buf
    } else {
        raw
    };
    if let Some(depth) = svg_xml_max_depth(svg_bytes) {
        if depth > MAX_SVG_DEPTH {
            anyhow::bail!("SVG nesting too deep (limit {})", MAX_SVG_DEPTH);
        }
    }
    let tree = usvg::Tree::from_data(svg_bytes, &svg_options(path))
        .map_err(|_| anyhow::anyhow!("SVG parse failed"))?;
    let size = tree.size();
    if size.width() <= 0.0 || size.height() <= 0.0 {
        anyhow::bail!("SVG has invalid size");
    }
    Ok(ParsedSvg {
        tree,
        width: size.width().ceil() as u32,
        height: size.height().ceil() as u32,
    })
}

pub(crate) fn probe_svg_dims(raw: &[u8]) -> Option<(u32, u32)> {
    parse_svg(raw, None).ok().map(|s| (s.width, s.height))
}

pub fn decode_svg(
    tree: &usvg::Tree,
    target_w: u32,
    target_h: u32,
) -> Result<image::DynamicImage, String> {
    let size = tree.size();
    let src_w = size.width();
    let src_h = size.height();
    if src_w <= 0.0 || src_h <= 0.0 {
        return Err("SVG has invalid size".to_string());
    }

    let tw = target_w.max(1);
    let th = target_h.max(1);
    let worst = (tw as u64).saturating_mul(th as u64).saturating_mul(4);
    if worst > runtime_limits().max_alloc {
        return Err("SVG target size too large".to_string());
    }
    if let Some(peak) = max_raster_image_peak(tree) {
        if peak > vector_peak_cap() {
            return Err(format!("embedded SVG image too large: {} bytes peak", peak));
        }
    } else {
        return Err("embedded SVG image size overflow".to_string());
    }
    let mut pixmap = tiny_skia::Pixmap::new(tw, th)
        .ok_or_else(|| "SVG target size too large".to_string())?;
    let transform = tiny_skia::Transform::from_scale(tw as f32 / src_w, th as f32 / src_h);
    resvg::render(tree, transform, &mut pixmap.as_mut());

    unpremultiply_rgba(pixmap.data_mut());
    let rgba = pixmap.data().to_vec();
    let img = image::RgbaImage::from_raw(tw, th, rgba)
        .ok_or_else(|| "failed to build SVG raster buffer".to_string())?;

    Ok(image::DynamicImage::ImageRgba8(img))
}

#[allow(clippy::manual_checked_ops)]
fn unpremultiply_rgba(buf: &mut [u8]) {
    for px in buf.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a == 0 {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
        } else {
            for p in px.iter_mut().take(3) {
                let v = *p as u32;
                *p = ((v * 255 + a / 2) / a) as u8;
            }
        }
    }
}

// ── HEIF helpers (libheif-rs 2.7 + image feature) ─────────────

/// Brands a `ftyp` box declares: the major brand plus the compatible ones (ISO/IEC 14496-12). A box
/// size of 0 means "extends to the end of the file" — MP4 muxers do write that, so it is honoured
/// instead of swallowing the brand list. A truncated box (a declared size larger than the buffer)
/// yields None instead of a panic, and `chunks_exact` ignores a trailing partial brand. `min_len` is
/// the shortest header still trusted: 12 for AVIF, 13 for HEIF — see the predicates.
fn ftyp_brands(raw: &[u8], min_len: usize) -> Option<Vec<[u8; 4]>> {
    if raw.len() < min_len || &raw[4..8] != b"ftyp" {
        return None;
    }
    let declared = u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]);
    let size = if declared == 0 { raw.len() } else { declared as usize };
    // The major brand needs 12 bytes; a real box is >= 16, but a size-0 box in a short buffer (and a
    // synthetic header in a test) may stop right after the major brand. A too-small or truncated box
    // yields None, so the brand list is never read out of bounds.
    if size < 12 || size > raw.len() {
        return None;
    }
    let mut out = Vec::with_capacity((size.saturating_sub(16)) / 4 + 1);
    out.push([raw[8], raw[9], raw[10], raw[11]]);
    if size > 16 {
        for c in raw[16..size].chunks_exact(4) {
            out.push([c[0], c[1], c[2], c[3]]);
        }
    }
    Some(out)
}

/// True when the file declares one of `wanted` as its major brand or anywhere in the compatible list.
fn has_brand(raw: &[u8], wanted: &[[u8; 4]], min_len: usize) -> bool {
    ftyp_brands(raw, min_len).is_some_and(|brands| brands.iter().any(|b| wanted.contains(b)))
}

/// True when any declared brand starts with one of `prefixes` (`hei`/`hev` families).
fn has_brand_prefix(raw: &[u8], prefixes: &[[u8; 3]], min_len: usize) -> bool {
    ftyp_brands(raw, min_len).is_some_and(|brands| {
        brands.iter().any(|b| prefixes.iter().any(|p| b[..3] == *p))
    })
}

/// HEIF needs 13 bytes. This is not derived from a principle: it is the contract the existing test
/// `avif_keeps_its_own_decoder` asserts (`a 12-byte header must be refused`), and it is kept on
/// purpose — a short header carries only the major brand, so we simply do not claim it. Changing it
/// without a symptom is not wanted.
pub(crate) fn is_heif(buf: &[u8]) -> bool {
    has_brand(buf, &[*b"mif1", *b"msf1"], 13) || has_brand_prefix(buf, &[*b"hei", *b"hev"], 13)
}

fn heif_exif(raw: &[u8]) -> Option<Vec<u8>> {
    use libheif_rs::HeifContext;
    let ctx = HeifContext::read_from_bytes(raw).ok()?;
    let handle = ctx.primary_image_handle().ok()?;
    for meta in handle.all_metadata() {
        if meta.item_type.to_string() == "Exif" {
            let payload = &meta.raw_data;
            for i in 0..16.min(payload.len().saturating_sub(4)) {
                let s = &payload[i..i + 4];
                if s == b"II*\0" || s == b"MM\0*" {
                    return Some(payload[i..].to_vec());
                }
            }
        }
    }
    None
}

fn pack_heif_rows(data: &[u8], stride: usize, row_size: usize, height: usize) -> Result<Vec<u8>> {
    let expected = row_size.saturating_mul(height);
    let mut packed = Vec::with_capacity(expected);
    for row in data.chunks(stride).take(height) {
        if row.len() < row_size {
            anyhow::bail!("{}", msg().err_heif_plane);
        }
        packed.extend_from_slice(&row[..row_size]);
    }
    if packed.len() != expected {
        anyhow::bail!("{}", msg().err_heif_plane);
    }
    Ok(packed)
}

fn decode_heif_manual(buf: &[u8], _path: Option<&Path>) -> Result<image::DynamicImage> {
    use libheif_rs::{HeifContext, LibHeif, ColorSpace, RgbChroma};

    let libheif = LibHeif::new();
    let ctx = HeifContext::read_from_bytes(buf).context(msg().err_heif_read)?;
    let handle = ctx.primary_image_handle().context(msg().err_heif_primary)?;

    let has_alpha = handle.has_alpha_channel();
    let color_space = if has_alpha {
        ColorSpace::Rgb(RgbChroma::Rgba)
    } else {
        ColorSpace::Rgb(RgbChroma::Rgb)
    };

    let heif_img = libheif
        .decode(&handle, color_space, None)
        .context(msg().err_heif_decode)?;

    let planes = heif_img.planes();
    let plane = planes.interleaved.context(msg().err_heif_plane)?;

    let width = plane.width;
    let height = plane.height;
    let stride = plane.stride;
    let bpp = (plane.storage_bits_per_pixel / 8) as usize;
    let row_size = width as usize * bpp;

    if stride == 0 || stride < row_size {
        anyhow::bail!("{}", msg().err_heif_plane);
    }

    let packed = pack_heif_rows(plane.data, stride, row_size, height as usize)?;

    let img = match (has_alpha, bpp) {
        (false, 3) => image::RgbImage::from_raw(width, height, packed).map(image::DynamicImage::ImageRgb8),
        (true, 4) => image::RgbaImage::from_raw(width, height, packed).map(image::DynamicImage::ImageRgba8),
        (false, 6) | (true, 8) => {
            let channels = if has_alpha { 4 } else { 3 };
            let mut down = Vec::with_capacity(width as usize * height as usize * channels);
            for px in packed.chunks_exact(bpp) {
                for c in 0..channels {
                    down.push(px[c * 2 + 1]);
                }
            }
            if has_alpha {
                image::RgbaImage::from_raw(width, height, down).map(image::DynamicImage::ImageRgba8)
            } else {
                image::RgbImage::from_raw(width, height, down).map(image::DynamicImage::ImageRgb8)
            }
        }
        _ => anyhow::bail!("{}", msg().err_heif_decode),
    };

    img.context(msg().err_heif_decode)
}

// ── AVIF helpers (libavif-sys) ────────────────────────────────

struct AvifDecoderGuard(*mut avifDecoder);

impl Drop for AvifDecoderGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { avifDecoderDestroy(self.0) };
        }
    }
}

struct AvifImageGuard(*mut avifImage);

impl Drop for AvifImageGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { avifImageDestroy(self.0) };
        }
    }
}

pub(crate) fn is_avif(buf: &[u8]) -> bool {
    // 12 is the old contract too (`is_avif` accepted a 12-byte header before the brand scan was
    // added), and it is enough for a major brand; the compatible list needs 16 bytes anyway.
    has_brand(buf, &[*b"avif", *b"avis", *b"av01"], 12)
}

pub(crate) fn probe_avif_dims(buf: &[u8]) -> Option<(u32, u32)> {
    unsafe {
        let decoder = avifDecoderCreate();
        if decoder.is_null() {
            return None;
        }
        let decoder = AvifDecoderGuard(decoder);
        let res = avifDecoderSetIOMemory(decoder.0, buf.as_ptr(), buf.len());
        if res != avifResult_AVIF_RESULT_OK {
            return None;
        }
        let res = avifDecoderParse(decoder.0);
        if res != avifResult_AVIF_RESULT_OK {
            return None;
        }
        let image = (*decoder.0).image;
        if image.is_null() {
            return None;
        }
        Some(((*image).width, (*image).height))
    }
}

/// An Exif payload can carry a leading field before the TIFF header: HEIF/AVIF items hold a 4-byte
/// offset there (ISO/IEC 23008-12 Annex A — and writers differ: libavif hands us an already-trimmed
/// payload, while items written by other tools arrive with the offset intact), and APP1-style blobs
/// start with `Exif\0\0`. `normalize_exif` expects the TIFF header first, so drop whatever precedes
/// it. The window is 64 bytes because the spec-conformant offset is 0 (TIFF at byte 4) while
/// non-conformant writers put it further along — `exiftool` reads such files, so we do too. Do not
/// "simplify" this to a fixed `blob[4..]` (breaks offset != 4) or to a plain `blob` (breaks every
/// trimmed payload): a false TIFF-magic found in a foreign blob is harmless anyway, because
/// `normalize_exif` validates the structure and returns None for anything that is not real EXIF.
/// `heif_exif` applies the same rule.
pub(crate) fn trim_to_tiff_header(blob: Vec<u8>) -> Vec<u8> {
    for i in 0..64.min(blob.len().saturating_sub(4)) {
        let s = &blob[i..i + 4];
        if s == b"II*\0" || s == b"MM\0*" {
            return blob[i..].to_vec();
        }
    }
    blob
}

#[allow(clippy::type_complexity)]
pub(crate) fn decode_avif(buf: &[u8]) -> Result<(image::DynamicImage, Option<Vec<u8>>, Option<Vec<u8>>)> {
    unsafe {
        let decoder = avifDecoderCreate();
        if decoder.is_null() {
            anyhow::bail!("{}", msg().err_avif_decode.replacen("{}", "avifDecoderCreate returned NULL", 1));
        }
        let decoder = AvifDecoderGuard(decoder);

        let image = avifImageCreateEmpty();
        if image.is_null() {
            anyhow::bail!("{}", msg().err_avif_decode.replacen("{}", "avifImageCreateEmpty returned NULL", 1));
        }
        let image = AvifImageGuard(image);

        let res = avifDecoderReadMemory(decoder.0, image.0, buf.as_ptr(), buf.len());
        if res != avifResult_AVIF_RESULT_OK {
            anyhow::bail!("{}", msg().err_avif_decode.replacen("{}", &format!("avifDecoderReadMemory: {}", res), 1));
        }

        let icc = if (*image.0).icc.size > 0 && !(*image.0).icc.data.is_null() {
            Some(std::slice::from_raw_parts((*image.0).icc.data, (*image.0).icc.size).to_vec())
        } else {
            None
        };

        let exif = if (*image.0).exif.size > 0 && !(*image.0).exif.data.is_null() {
            let blob = std::slice::from_raw_parts((*image.0).exif.data, (*image.0).exif.size).to_vec();
            Some(trim_to_tiff_header(blob))
        } else {
            None
        };

        let w = (*image.0).width;
        let h = (*image.0).height;
        let has_alpha = !(*image.0).alphaPlane.is_null();
        let channels: u32 = if has_alpha { 4 } else { 3 };

        let mut rgb = std::mem::zeroed::<avifRGBImage>();
        avifRGBImageSetDefaults(&mut rgb, image.0);
        rgb.depth = 8;
        rgb.format = if has_alpha {
            avifRGBFormat_AVIF_RGB_FORMAT_RGBA
        } else {
            avifRGBFormat_AVIF_RGB_FORMAT_RGB
        };
        rgb.rowBytes = w * channels;

        let size = (w as usize) * (h as usize) * (channels as usize);
        let mut pixels = vec![0u8; size];
        rgb.pixels = pixels.as_mut_ptr();

        let res = avifImageYUVToRGB(image.0, &mut rgb);
        if res != avifResult_AVIF_RESULT_OK {
            anyhow::bail!("{}", msg().err_avif_decode.replacen("{}", &format!("avifImageYUVToRGB: {}", res), 1));
        }

        let img = if has_alpha {
            image::RgbaImage::from_raw(w, h, pixels)
                .map(image::DynamicImage::ImageRgba8)
                .context(msg().err_avif_decode)?
        } else {
            image::RgbImage::from_raw(w, h, pixels)
                .map(image::DynamicImage::ImageRgb8)
                .context(msg().err_avif_decode)?
        };
        Ok((img, icc, exif))
    }
}

// ── RAW helpers ───────────────────────────────────────────────

pub(crate) fn is_raw_bytes(raw: &[u8]) -> bool {
    if raw.len() >= 4 && (&raw[0..4] == b"II*\0" || &raw[0..4] == b"MM\0*") {
        return true;
    }
    if raw.len() >= 12 && &raw[4..12] == b"ftypcrx " { return true; }
    if raw.len() >= 15 && &raw[0..15] == b"FUJIFILMCCD-RAW" { return true; }
    if raw.len() >= 4 && (&raw[0..4] == b"IIRO" || &raw[0..4] == b"MMOR") { return true; }
    if raw.len() >= 4 && &raw[0..4] == b"FOVb" { return true; }
    if raw.len() >= 4 && &raw[0..4] == b"\0MRM" { return true; }
    if raw.len() >= 4 && (&raw[0..4] == b"IIII" || &raw[0..4] == b"MMMM") { return true; }
    if raw.len() >= 4 && &raw[0..4] == b"ARRI" { return true; }
    if raw.len() >= 14 && &raw[0..14] == b"II\x1A\0\0\0HEAPCCDR" { return true; }
    if raw.len() >= 4 && &raw[0..4] == b"IIU\0" { return true; }
    false
}

fn decode_raw(path: &Path) -> Result<image::DynamicImage> {
    let rawimage = rawler::decode_file(path).context(msg().err_raw_decode)?;
    develop_raw(rawimage)
}

fn decode_raw_bytes(raw: &[u8]) -> Result<image::DynamicImage> {
    let source = rawler::rawsource::RawSource::new_from_slice(raw);
    let rawimage = rawler::decode(&source, &rawler::decoders::RawDecodeParams::default())
        .context(msg().err_raw_decode)?;
    develop_raw(rawimage)
}

fn develop_raw(rawimage: rawler::RawImage) -> Result<image::DynamicImage> {
    let intermediate = rawler::imgop::develop::RawDevelop::default()
        .develop_intermediate(&rawimage)
        .context(msg().err_raw_decode)?;
    intermediate.to_dynamic_image().context(msg().err_raw_build)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ftyp_header(brand: &[u8]) -> Vec<u8> {
        let mut buf = vec![0u8; 16];
        buf[4..8].copy_from_slice(b"ftyp");
        buf[8..8 + brand.len()].copy_from_slice(brand);
        buf
    }

    #[test]
    fn heif_brands_are_recognised_by_their_magic() {
        for brand in ["heic", "heix", "heim", "heis", "hevc", "hevx", "mif1", "msf1"] {
            assert!(
                is_heif(&ftyp_header(brand.as_bytes())),
                "brand {brand} was not recognised as heif"
            );
        }
    }

    #[test]
    fn avif_keeps_its_own_decoder() {
        for brand in ["avif", "avis", "isom"] {
            assert!(
                !is_heif(&ftyp_header(brand.as_bytes())),
                "brand {brand} must not be routed to the heif path"
            );
        }
        assert!(
            !is_heif(&ftyp_header(b"heic")[..12]),
            "a 12-byte header must be refused"
        );
    }

    fn oversized_bmp() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"BM");
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&54u32.to_le_bytes());
        b.extend_from_slice(&40u32.to_le_bytes());
        b.extend_from_slice(&40000i32.to_le_bytes());
        b.extend_from_slice(&40000i32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&24u16.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&0i32.to_le_bytes());
        b.extend_from_slice(&0i32.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b
    }

    #[test]
    fn decode_with_limits_rejects_oversized_embeds() {
        let err = decode_with_limits(&oversized_bmp()).expect_err("oversized input must be rejected");
        assert!(matches!(err, image::ImageError::Limits(_)), "expected a limit error, got {err:?}");
    }

    #[test]
    fn decode_with_limits_accepts_a_normal_small_input() {
        let png = include_bytes!("../tests/fixtures/8x8.png");
        assert!(decode_with_limits(png).is_ok());
    }

    fn gzip_bytes(payload: &[u8]) -> Vec<u8> {
        use flate2::write::GzEncoder;
        use std::io::Write;
        let mut enc = GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(payload).unwrap();
        enc.finish().unwrap()
    }

    #[test]
    fn tar_gz_with_angle_bracket_in_name_is_not_svg() {
        let mut tar_header = vec![0u8; 512];
        tar_header[0..13].copy_from_slice(b"<unnamed>.txt");
        let archive = gzip_bytes(&tar_header);
        assert!(
            !looks_like_svg(&archive),
            "a tar header containing '<' must not be routed to the svg path"
        );
    }

    #[test]
    fn plain_gzip_archive_is_not_treated_as_svg() {
        let archive = gzip_bytes(b"not an svg at all, just a plain archive payload");
        assert!(
            !looks_like_svg(&archive),
            "a plain gzip archive must not be reported as svg"
        );
    }

    #[test]
    fn gzipped_svg_stays_on_the_svg_path() {
        let svgz = gzip_bytes(
            b"<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\" width=\"4\" height=\"4\"/>",
        );
        assert!(looks_like_svg(&svgz), "a gzipped svg must keep its own path");
    }

    fn tga_bytes() -> Vec<u8> {
        let mut b = vec![0u8; 18];
        b[2] = 2;
        b[12] = 4;
        b[14] = 4;
        b[16] = 24;
        b[17] = 32;
        b.extend(vec![0x40u8; 4 * 4 * 3]);
        b
    }

    #[test]
    fn tga_is_dispatched_by_name_or_header_for_a_pipe() {
        let bytes = tga_bytes();
        let ok = decode_image(&bytes, Some(Path::new("tiny.tga")), None, None);
        assert!(
            ok.is_ok(),
            "a valid .tga must decode by extension, got {:?}",
            ok.err().map(|e| e.to_string())
        );
        let wrong = decode_image(&bytes, Some(Path::new("tiny.png")), None, None);
        assert!(wrong.is_err(), "the same bytes named .png must stay unsupported");
        let piped = decode_image(&bytes, None, None, None);
        assert!(
            piped.is_ok(),
            "a nameless stream is recognised by its TGA header, got {:?}",
            piped.err().map(|e| e.to_string())
        );
        let mut garbage = vec![0u8; 64];
        garbage[2] = 0x7F;
        assert!(
            decode_image(&garbage, None, None, None).is_err(),
            "a nameless stream that is not a TGA header must still be refused"
        );
    }

    #[test]
    fn mif1_with_a_compatible_avif_brand_is_recognised() {
        let mut raw = vec![0u8; 32];
        raw[4..8].copy_from_slice(b"ftyp");
        raw[8..12].copy_from_slice(b"mif1");
        raw[16..20].copy_from_slice(b"avif");
        raw[20..24].copy_from_slice(b"miaf");
        assert!(is_avif(&raw), "avif in the compatible list must be seen");
        assert!(is_heif(&raw), "mif1 as the major brand is a heif claim as well");
    }

    #[test]
    fn a_zero_ftyp_size_means_to_the_end_of_the_file() {
        let mut raw = vec![0u8; 24];
        raw[4..8].copy_from_slice(b"ftyp");
        raw[8..12].copy_from_slice(b"mif1");
        raw[16..20].copy_from_slice(b"avif");
        assert!(is_avif(&raw), "size 0 means 'to the end of file', as mp4 muxers write");
        raw[0..4].copy_from_slice(&12u32.to_be_bytes());
        assert!(!is_avif(&raw), "a too-small ftyp box must not be trusted");
    }

    #[test]
    fn exif_payload_is_trimmed_to_its_tiff_header() {
        let tiff = b"II*\0\x08\0\0\0rest of the ifd".to_vec();
        assert_eq!(
            trim_to_tiff_header(tiff.clone()),
            tiff,
            "an already trimmed payload must stay as it is"
        );
        let mut with_offset = vec![0u8; 4];
        with_offset.extend_from_slice(&tiff);
        assert_eq!(
            trim_to_tiff_header(with_offset),
            tiff,
            "the 4-byte item offset must be dropped"
        );
        let mut non_conformant = vec![0u8; 20];
        non_conformant.extend_from_slice(&tiff);
        assert_eq!(
            trim_to_tiff_header(non_conformant),
            tiff,
            "an offset beyond the spec (20) must be dropped too - exiftool reads such files"
        );
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend_from_slice(&tiff);
        assert_eq!(
            trim_to_tiff_header(app1),
            tiff,
            "the app1 marker must be dropped"
        );
        assert_eq!(
            trim_to_tiff_header(vec![1, 2, 3]),
            vec![1, 2, 3],
            "a short blob is returned untouched"
        );
    }

    #[test]
    fn avif_is_recognised_by_every_primary_brand_encoders_write() {
        for brand in ["avif", "avis", "av01"] {
            let mut raw = vec![0u8; 16];
            raw[4..8].copy_from_slice(b"ftyp");
            raw[8..12].copy_from_slice(brand.as_bytes());
            assert!(is_avif(&raw), "brand {brand} must reach the avif decoder");
        }
        let mut raw = vec![0u8; 16];
        raw[4..8].copy_from_slice(b"ftyp");
        raw[8..12].copy_from_slice(b"heic");
        assert!(!is_avif(&raw), "heic must not be routed to the avif decoder");
    }

    #[test]
    fn webp_with_an_unknown_chunk_has_no_guessed_size() {
        let mut raw = vec![0u8; 32];
        raw[0..4].copy_from_slice(b"RIFF");
        raw[8..12].copy_from_slice(b"WEBP");
        raw[12..16].copy_from_slice(b"XXXX");
        assert!(
            probe_dims(&raw).is_none(),
            "an unreadable webp header must not invent a 16383x16383 size"
        );
    }

    #[test]
    fn os2_core_header_bmp_reports_its_real_size() {
        let mut raw = vec![0u8; 74];
        raw[0..2].copy_from_slice(b"BM");
        raw[14..18].copy_from_slice(&12u32.to_le_bytes());
        raw[18..20].copy_from_slice(&4u16.to_le_bytes());
        raw[20..22].copy_from_slice(&4u16.to_le_bytes());
        assert_eq!(probe_dims(&raw), Some((4, 4)));
    }

    #[test]
    fn gzipped_svg_bomb_is_refused() {
        use flate2::write::GzEncoder;
        use std::io::Write;
        let mut enc = GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let block = vec![b'a'; 1024 * 1024];
        for _ in 0..300 {
            enc.write_all(&block).unwrap();
        }
        let bomb = enc.finish().unwrap();
        let err = parse_svg(&bomb, None).err().expect("a decompression bomb must be refused");
        assert!(err.to_string().contains("limit"), "unexpected error: {err}");
    }

    #[test]
    fn heif_row_packing_keeps_the_last_unpadded_row() {
        let data = [1u8, 2, 3, 0, 4, 5, 6];
        assert_eq!(pack_heif_rows(&data, 4, 3, 2).unwrap(), vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn heif_row_packing_rejects_a_short_row() {
        let data = [1u8, 2];
        assert!(pack_heif_rows(&data, 4, 3, 2).is_err());
    }

    #[test]
    fn a_growing_permit_keeps_holding_the_budget() {
        use std::sync::{Condvar, Mutex};
        let budget = MemBudget { total: 4, used: Mutex::new(0), cv: Condvar::new() };
        let mut permit = MemPermit { budget: &budget, need: 0 };
        assert!(budget.try_acquire(3));
        permit.need += 3;
        assert!(!budget.try_acquire(2));
        assert!(budget.try_acquire(1));
        permit.need += 1;
        assert_eq!(*budget.used.lock().unwrap(), 4);
        drop(permit);
        assert_eq!(*budget.used.lock().unwrap(), 0);
    }
}

