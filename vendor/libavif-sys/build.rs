use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=wrapper.h");

    #[cfg(feature = "use-bindgen")]
    {
        let include_paths = find_libavif();
        generate_bindings(&include_paths);
    }

    #[cfg(not(feature = "use-bindgen"))]
    {
        find_libavif();
    }
}

fn find_libavif() -> Vec<PathBuf> {
    vcpkg::Config::new()
        .emit_includes(true)
        .find_package("libavif")
        .unwrap_or_else(|e| panic!("failed to find libavif via vcpkg: {}", e))
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
        .allowlist_function("avif.*")
        .allowlist_type("avif.*")
        .allowlist_var("AVIF_.*");

    for path in include_paths {
        builder = builder.clang_arg(format!("-I{}", path.display()));
    }

    let bindings = builder
        .generate()
        .expect("Unable to generate bindings for avif.h");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out.join("bindings.rs"))
        .expect("Couldn't write bindings.rs");
}
