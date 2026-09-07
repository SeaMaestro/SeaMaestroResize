use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=wrapper.h");

    #[cfg(feature = "use-bindgen")]
    {
        let include_paths = find_libjxl();
        generate_bindings(&include_paths);
    }

    #[cfg(not(feature = "use-bindgen"))]
    {
        find_libjxl();
    }
}

fn ensure_vcpkg_updates_dir() {
    if let Ok(root) = std::env::var("VCPKG_ROOT") {
        let dir = std::path::Path::new(&root)
            .join("installed")
            .join("vcpkg")
            .join("updates");
        let _ = std::fs::create_dir_all(dir);
    }
}

fn find_libjxl() -> Vec<PathBuf> {
    ensure_vcpkg_updates_dir();
    vcpkg::Config::new()
        .emit_includes(true)
        .find_package("libjxl")
        .unwrap_or_else(|e| panic!("failed to find libjxl via vcpkg: {}", e))
        .include_paths
}

#[cfg(feature = "use-bindgen")]
fn generate_bindings(include_paths: &[PathBuf]) {
    use std::env;

    let mut builder = bindgen::Builder::default()
        .header("wrapper.h")
        .generate_comments(true)
        .formatter(bindgen::Formatter::Rustfmt)
        .generate_cstr(true)
        .disable_name_namespacing()
        .array_pointers_in_arguments(true)
        .ctypes_prefix("libc")
        .size_t_is_usize(true)
        .allowlist_function("Jxl.*")
        .allowlist_type("Jxl.*")
        .allowlist_var("JXL_.*");

    for path in include_paths {
        builder = builder.clang_arg(format!("-I{}", path.display()));
    }

    let bindings = builder
        .generate()
        .expect("Unable to generate bindings for jxl headers");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out.join("bindings.rs"))
        .expect("Couldn't write bindings.rs");
}
