#![allow(dead_code)]

use std::fs;
use std::path::Path;
use std::process::Command;

pub fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_SeaMaestro")
}

pub struct Run {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

pub fn run_cli(args: &[&str], threads: Option<&str>, crop_debug: bool) -> Run {
    let mut cmd = Command::new(exe());
    for a in args {
        cmd.arg(a);
    }
    if let Some(t) = threads {
        cmd.env("RAYON_NUM_THREADS", t);
    }
    if crop_debug {
        cmd.env("SEAMAESTRO_CROP_DEBUG", "1");
    }
    let out = cmd.output().unwrap();
    Run {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

pub fn read_bytes(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap_or_else(|e| panic!("missing output {}: {e}", path.display()))
}

fn inside_quad(px: f32, py: f32, q: &[(f32, f32); 4]) -> bool {
    let mut neg = 0i32;
    let mut pos = 0i32;
    for i in 0..4 {
        let a = q[i];
        let b = q[(i + 1) % 4];
        let cross = (b.0 - a.0) * (py - a.1) - (b.1 - a.1) * (px - a.0);
        if cross < 0.0 {
            neg += 1;
        } else {
            pos += 1;
        }
    }
    neg == 0 || pos == 0
}

pub fn write_document_bmp(path: &Path, w: u32, h: u32) {
    let (wf, hf) = (w as f32, h as f32);
    let quad = [
        (wf * 0.10, hf * 0.14),
        (wf * 0.88, hf * 0.10),
        (wf * 0.92, hf * 0.86),
        (wf * 0.13, hf * 0.90),
    ];
    let stride = ((w as usize * 3 + 3) / 4) * 4;
    let mut data = Vec::with_capacity(stride * h as usize);
    for y in (0..h).rev() {
        let mut row = Vec::with_capacity(stride);
        for x in 0..w {
            let v = if inside_quad(x as f32 + 0.5, y as f32 + 0.5, &quad) {
                40u8
            } else {
                240u8
            };
            row.extend_from_slice(&[v, v, v]);
        }
        row.resize(stride, 0);
        data.extend_from_slice(&row);
    }
    let img_size = data.len() as u32;
    let mut out = Vec::with_capacity(54 + data.len());
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(54 + img_size).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&h.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&img_size.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&data);
    fs::write(path, out).unwrap();
}

pub fn png_size(bytes: &[u8]) -> (u32, u32) {
    const SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    assert!(
        bytes.len() > 24 && bytes.get(..8) == Some(&SIG[..]),
        "not a PNG stream"
    );
    assert_eq!(
        bytes.get(12..16),
        Some(&b"IHDR"[..]),
        "first PNG chunk is not IHDR"
    );
    (
        u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]),
        u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]),
    )
}

pub fn inject_orientation6(jpeg: &[u8]) -> Vec<u8> {
    assert!(
        jpeg.len() > 2 && jpeg[0] == 0xFF && jpeg[1] == 0xD8,
        "not a JPEG stream"
    );
    let mut tiff: Vec<u8> = Vec::with_capacity(26);
    tiff.extend_from_slice(b"II");
    tiff.extend_from_slice(&0x002Au16.to_le_bytes());
    tiff.extend_from_slice(&8u32.to_le_bytes());
    tiff.extend_from_slice(&1u16.to_le_bytes());
    tiff.extend_from_slice(&0x0112u16.to_le_bytes());
    tiff.extend_from_slice(&3u16.to_le_bytes());
    tiff.extend_from_slice(&1u32.to_le_bytes());
    tiff.extend_from_slice(&6u16.to_le_bytes());
    tiff.extend_from_slice(&0u16.to_le_bytes());
    tiff.extend_from_slice(&0u32.to_le_bytes());
    let mut payload = Vec::with_capacity(6 + tiff.len());
    payload.extend_from_slice(b"Exif\0\0");
    payload.extend_from_slice(&tiff);
    let mut out = Vec::with_capacity(jpeg.len() + payload.len() + 4);
    out.extend_from_slice(&jpeg[..2]);
    out.push(0xFF);
    out.push(0xE1);
    out.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(&payload);
    out.extend_from_slice(&jpeg[2..]);
    out
}
