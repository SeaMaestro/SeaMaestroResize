fn main() {
    #[cfg(windows)]
    windows_resources();
}

#[cfg(windows)]
fn windows_resources() {
    println!("cargo:rerun-if-changed=icon.ico");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_BG");
    println!("cargo:rerun-if-env-changed=SEAMAESTRO_CUT_MODEL");
    println!("cargo:rerun-if-env-changed=SEAMAESTRO_ORT_DLL");
    println!("cargo:rerun-if-env-changed=SEAMAESTRO_DML_DLL");
    println!("cargo:rerun-if-env-changed=SEAMAESTRO_ORT_SHARED_DLL");

    let mut res = winres::WindowsResource::new();

    if std::path::Path::new("icon.ico").exists() {
        res.set_icon("icon.ico");
    }

    let cut = std::env::var_os("CARGO_FEATURE_BG").is_some();

    if cut {
        res.set("FileDescription", "SeaMaestro Multiformat Image Resizer + Background Cut");
        res.set("ProductName", "SeaMaestro Image Resizer + Background Cut");
        res.set("InternalName", "SeaMaestroCut");
        res.set("OriginalFilename", "SeaMaestroCut.exe");
        res.set("Comments", "Multiformat batch/group image resizer with built-in AI background removal. Email: seamaestro@proton.me");
    } else {
        res.set("FileDescription", "SeaMaestro Multiformat Image Resizer");
        res.set("ProductName", "SeaMaestro Image Resizer");
        res.set("InternalName", "SeaMaestro");
        res.set("OriginalFilename", "SeaMaestro.exe");
        res.set("Comments", "Multiformat batch/group image resizer tool. Email: seamaestro@proton.me");
    }

    res.set("CompanyName", "Independent Developer — Capt. Volodymyr Gumanyuk");
    let version = format!("{}.0", env!("CARGO_PKG_VERSION"));
    res.set("FileVersion", &version);
    res.set("ProductVersion", &version);
    res.set("LegalCopyright", "Copyright (c) Captain Volodymyr Gumanyuk");

    res.set_manifest(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false" />
      </requestedPrivileges>
    </security>
  </trustInfo>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
    </windowsSettings>
  </application>
</assembly>
"#);

    res.compile().unwrap();

    println!("cargo:rustc-link-lib=advapi32");
}