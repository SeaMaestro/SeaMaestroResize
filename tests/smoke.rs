use std::process::Command;

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_SeaMaestroResize")
}

#[test]
fn smoke_png_to_jpeg() {
    let dir = std::env::temp_dir().join("smr_smoke");
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("in.png");
    let out = dir.join("out.jpg");
    std::fs::write(&input, include_bytes!("fixtures/8x8.png")).unwrap();
    let _ = std::fs::remove_file(&out);

    let status = Command::new(exe())
        .arg(&input)
        .arg("--output")
        .arg(&out)
        .arg("--format")
        .arg("jpeg")
        .arg("--no-pause")
        .status()
        .unwrap();

    assert!(status.success());
    let bytes = std::fs::read(&out).unwrap();
    assert!(bytes.starts_with(&[0xFF, 0xD8]));
}