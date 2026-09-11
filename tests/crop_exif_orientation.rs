mod common;

use std::fs;
use std::path::Path;

use common::{inject_orientation6, png_size, read_bytes, run_cli, write_document_bmp};

const W: u32 = 320;
const H: u32 = 480;

fn convert(input: &Path, output: &Path, format: &str, extra: &[&str]) -> common::Run {
    let inp = input.to_string_lossy().into_owned();
    let outp = output.to_string_lossy().into_owned();
    let mut args: Vec<&str> = vec![inp.as_str()];
    args.extend_from_slice(extra);
    args.extend_from_slice(&["--format", format, "--output", outp.as_str(), "--no-pause"]);
    let r = run_cli(&args, None, false);
    assert_eq!(r.code, Some(0), "exit={:?}: {}", r.code, r.stderr);
    r
}

#[test]
fn orientation6_is_applied_before_crop() {
    let dir = std::env::temp_dir().join("smr_exif");
    fs::create_dir_all(&dir).unwrap();

    let bmp = dir.join("doc.bmp");
    write_document_bmp(&bmp, W, H);

    let flat = dir.join("flat.jpg");
    convert(&bmp, &flat, "jpeg", &[]);
    let tagged = dir.join("tagged.jpg");
    fs::write(&tagged, inject_orientation6(&read_bytes(&flat))).unwrap();

    let plain_png = dir.join("plain.png");
    convert(&flat, &plain_png, "png", &[]);
    let plain = png_size(&read_bytes(&plain_png));

    let tagged_png = dir.join("tagged.png");
    convert(&tagged, &tagged_png, "png", &[]);
    let rotated = png_size(&read_bytes(&tagged_png));

    assert_eq!(plain, (W, H), "untagged input must keep its stored size");
    assert_eq!(
        rotated,
        (H, W),
        "Orientation=6 must swap the output dimensions, got {rotated:?}"
    );

    let cropped = dir.join("tagged_crop.png");
    let r = convert(&tagged, &cropped, "png", &["--crop", "--scan"]);
    let c = png_size(&read_bytes(&cropped));
    assert_ne!(
        c,
        (W, H),
        "crop handed back the raw, un-rotated frame {c:?}; last log line: {}",
        r.stderr.lines().last().unwrap_or("")
    );
    assert!(
        c.0 <= H && c.1 <= W,
        "cropped frame {c:?} does not fit inside the oriented frame {H}x{W}"
    );
}
