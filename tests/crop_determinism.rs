mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{run_cli, write_document_bmp};

fn run_crop(input: &Path, output: &PathBuf, threads: &str, format: &str) -> (Vec<u8>, String) {
    let inp = input.to_string_lossy().into_owned();
    let outp = output.to_string_lossy().into_owned();
    let res = run_cli(
        &[
            inp.as_str(),
            "--crop",
            "--scan",
            "--size",
            "2000",
            "--quality",
            "40",
            "--format",
            format,
            "--output",
            outp.as_str(),
            "--no-pause",
        ],
        Some(threads),
        true,
    );
    assert!(
        res.code == Some(0),
        "crop exit={:?} for {format}: {}",
        res.code,
        res.stderr
    );
    (
        fs::read(output).unwrap_or_else(|e| panic!("missing output {}: {e}", output.display())),
        res.stderr,
    )
}

#[test]
fn crop_output_is_thread_count_deterministic() {
    let dir = std::env::temp_dir().join("smr_det");
    fs::create_dir_all(&dir).unwrap();
    let input = dir.join("doc.bmp");
    write_document_bmp(&input, 512, 512);

    for (format, ext) in [("jpeg", "jpg"), ("png", "png")] {
        let a = dir.join(format!("r1.{ext}"));
        let b = dir.join(format!("r8.{ext}"));
        let (bytes_a, err_a) = run_crop(&input, &a, "1", format);
        let (bytes_b, _) = run_crop(&input, &b, "8", format);
        assert!(
            err_a.contains("warp dst="),
            "{format}: crop never reached the warp stage, so this run proves nothing: {err_a}"
        );
        assert!(
            !err_a.contains("warp_fallback"),
            "{format}: unexpected fallback to the original: {err_a}"
        );
        assert_eq!(
            bytes_a.len(),
            bytes_b.len(),
            "{format}: output size depends on RAYON_NUM_THREADS"
        );
        assert_eq!(
            bytes_a, bytes_b,
            "{format}: output bytes depend on RAYON_NUM_THREADS"
        );
    }
}
