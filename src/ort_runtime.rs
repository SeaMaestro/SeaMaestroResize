use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

const ONNX_DLL: &[u8] = include_bytes!(env!("SEAMAESTRO_ORT_DLL"));
const DML_DLL: &[u8] = include_bytes!(env!("SEAMAESTRO_DML_DLL"));
const SHARED_DLL: &[u8] = include_bytes!(env!("SEAMAESTRO_ORT_SHARED_DLL"));

const ONNX_NAME: &str = "onnxruntime.dll";
const DML_NAME: &str = "DirectML.dll";
const SHARED_NAME: &str = "onnxruntime_providers_shared.dll";

const LOAD_WITH_ALTERED_SEARCH_PATH: u32 = 0x0000_0008;
const LOAD_LIBRARY_SEARCH_DEFAULT_DIRS: u32 = 0x0000_1000;
const LOAD_LIBRARY_SEARCH_USER_DIRS: u32 = 0x0000_0400;

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryExW(file: *const u16, file_handle: *mut core::ffi::c_void, flags: u32) -> *mut core::ffi::c_void;
    fn AddDllDirectory(path: *const u16) -> *mut core::ffi::c_void;
    fn SetDefaultDllDirectories(flags: u32) -> i32;
}

fn wide(path: &Path) -> Vec<u16> {
    OsStr::new(path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn files() -> [(&'static str, &'static [u8]); 3] {
    [
        (ONNX_NAME, ONNX_DLL),
        (DML_NAME, DML_DLL),
        (SHARED_NAME, SHARED_DLL),
    ]
}

fn fingerprint(bytes: &[u8]) -> (u32, usize) {
    (crc32fast::hash(bytes), bytes.len())
}

fn matches_exactly(on_disk: &[u8], embedded: &[u8]) -> bool {
    on_disk == embedded
}

fn base_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("SEAMAESTRO_RUNTIME_DIR") {
        if !dir.trim().is_empty() {
            return Ok(PathBuf::from(dir));
        }
    }
    let local = std::env::var("LOCALAPPDATA").context("LOCALAPPDATA is not set")?;
    Ok(PathBuf::from(local).join("SeaMaestro"))
}

pub(crate) fn dir_key() -> String {
    let mut key = String::new();
    for (_, bytes) in files() {
        let (crc, len) = fingerprint(bytes);
        key.push_str(&format!("{:08x}{:08x}", crc, len));
    }
    key
}

pub(crate) fn runtime_dir() -> Result<PathBuf> {
    Ok(base_dir()?.join("ort").join(dir_key()))
}

fn present_verified(dir: &Path) -> bool {
    files()
        .iter()
        .all(|(name, bytes)| on_disk_matches(&dir.join(name), bytes))
}

fn on_disk_matches(path: &Path, bytes: &[u8]) -> bool {
    fs::read(path)
        .map(|on_disk| matches_exactly(&on_disk, bytes))
        .unwrap_or(false)
}

fn matches_embedded(dir: &Path) -> bool {
    files()
        .iter()
        .all(|(name, bytes)| on_disk_matches(&dir.join(name), bytes))
}

fn write_verified(dir: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    let target = dir.join(name);
    let temp = dir.join(format!("{}.tmp.{}", name, fastrand::u64(..)));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .with_context(|| format!("cannot create {}", temp.display()))?;
    file.write_all(bytes).with_context(|| format!("cannot write {}", temp.display()))?;
    drop(file);
    let written = fs::read(&temp).with_context(|| format!("cannot read back {}", temp.display()))?;
    if !matches_exactly(&written, bytes) {
        let _ = fs::remove_file(&temp);
        bail!(
            "runtime file {} did not survive the write ({} bytes on disk, {} embedded)",
            name,
            written.len(),
            bytes.len()
        );
    }
    match fs::rename(&temp, &target) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = fs::remove_file(&temp);
            if on_disk_matches(&target, bytes) {
                Ok(())
            } else {
                Err(err).with_context(|| format!("cannot move {} into place", target.display()))
            }
        }
    }
}

fn extract_all(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    if matches_embedded(dir) {
        return Ok(());
    }
    eprintln!(
        "{}",
        crate::msg()
            .note_unpacking_runtime
            .replacen("{}", &dir.display().to_string(), 1)
    );
    for (name, bytes) in files() {
        write_verified(dir, name, bytes)?;
    }
    Ok(())
}

/// A directory under our runtime root is ours only when its name is the hex key built by `dir_key`.
/// The key length is passed in because `dir_key` hashes the embedded runtime (tens of megabytes) and
/// must never be recomputed once per directory entry. Anything else there is somebody else's and is
/// not removed.
fn is_key_dir_name(name: &str, key_len: usize) -> bool {
    name.len() == key_len && name.bytes().all(|b| b.is_ascii_hexdigit())
}

fn cleanup_other_versions(dir: &Path, keep: &str) {
    let overridden = std::env::var("SEAMAESTRO_RUNTIME_DIR").is_ok_and(|v| !v.trim().is_empty());
    if overridden {
        return;
    }
    let key_len = dir_key().len();
    if let Some(root) = dir.parent() {
        if let Ok(entries) = fs::read_dir(root) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();
                if path.is_dir() && name != keep && is_key_dir_name(&name, key_len) {
                    let _ = fs::remove_dir_all(&path);
                }
            }
        }
    }
}

pub(crate) fn ensure_runtime() -> Result<PathBuf> {
    let dir = runtime_dir()?;
    let force_verify = std::env::var("SEAMAESTRO_RUNTIME_VERIFY")
        .map(|v| !v.trim().is_empty() && v.trim() != "0")
        .unwrap_or(false);

    let ready = if force_verify { false } else { present_verified(&dir) };
    if !ready {
        extract_all(&dir)?;
        if !matches_embedded(&dir) {
            bail!(
                "{}",
                crate::msg()
                    .err_runtime_mismatch
                    .replacen("{}", &dir.display().to_string(), 1)
            );
        }
    }
    cleanup_other_versions(&dir, &dir_key());
    Ok(dir.join(ONNX_NAME))
}

pub(crate) fn prepare(onnx: &Path) -> Result<()> {
    let dir = onnx.parent().context("runtime path has no parent directory")?;
    let dir = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    let dir_w = wide(&dir);
    let mut dml_loaded = false;
    unsafe {
        let defaults_ok = SetDefaultDllDirectories(
            LOAD_LIBRARY_SEARCH_DEFAULT_DIRS | LOAD_LIBRARY_SEARCH_USER_DIRS,
        ) != 0;
        let added = !AddDllDirectory(dir_w.as_ptr()).is_null();
        if !defaults_ok || !added {
            eprintln!(
                "{}",
                crate::msg()
                    .note_dll_search_path
                    .replacen("{}", &dir.display().to_string(), 1)
            );
        }
        for name in [SHARED_NAME, DML_NAME] {
            let path = dir.join(name);
            if !path.is_file() {
                continue;
            }
            let path_w = wide(&path);
            let handle = LoadLibraryExW(
                path_w.as_ptr(),
                std::ptr::null_mut(),
                LOAD_WITH_ALTERED_SEARCH_PATH,
            );
            if handle.is_null() {
                eprintln!(
                    "{}",
                    crate::msg()
                        .warn_preload_failed
                        .replacen("{}", &path.display().to_string(), 1)
                );
            } else if name == DML_NAME {
                dml_loaded = true;
            }
        }
    }
    if !dml_loaded {
        eprintln!(
            "{}",
            crate::msg()
                .note_dml_not_preloaded
                .replacen("{}", &dir.display().to_string(), 1)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn runtime_files_are_compared_byte_for_byte() {
        let embedded = vec![7u8; 4096];
        let mut altered = embedded.clone();
        altered[2048] = 8;
        assert!(matches_exactly(&embedded, &embedded));
        assert!(!matches_exactly(&altered, &embedded), "same length, different byte accepted");
        assert!(!matches_exactly(&embedded[..4095], &embedded), "truncated file accepted");
    }

    #[test]
    fn runtime_is_extracted_then_reused() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let tmp = std::env::temp_dir().join(format!("sm_ort_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        std::env::set_var("SEAMAESTRO_RUNTIME_DIR", &tmp);

        let onnx = ensure_runtime().expect("extract runtime");
        assert!(onnx.is_file(), "onnxruntime.dll was not unpacked");
        assert_eq!(fs::metadata(&onnx).unwrap().len(), ONNX_DLL.len() as u64);
        assert!(
            matches_embedded(onnx.parent().unwrap()),
            "unpacked files differ from the embedded ones"
        );

        let again = ensure_runtime().expect("reuse runtime");
        assert_eq!(onnx, again);

        std::env::remove_var("SEAMAESTRO_RUNTIME_DIR");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn truncated_file_is_detected_and_repaired() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let tmp = std::env::temp_dir().join(format!("sm_ort_repair_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        std::env::set_var("SEAMAESTRO_RUNTIME_DIR", &tmp);

        let onnx = ensure_runtime().expect("extract runtime");
        fs::write(&onnx, &ONNX_DLL[..ONNX_DLL.len() - 1]).unwrap();
        assert!(!on_disk_matches(&onnx, ONNX_DLL), "a truncated file must be rejected");

        let repaired = ensure_runtime().expect("repair runtime");
        assert_eq!(repaired, onnx);
        assert!(on_disk_matches(&repaired, ONNX_DLL), "the truncated file must be rewritten");

        std::env::remove_var("SEAMAESTRO_RUNTIME_DIR");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn only_our_own_key_directories_are_cleaned_up() {
        let k = dir_key().len();
        assert_eq!(k, 48);
        assert!(is_key_dir_name(&dir_key(), k), "our own key must be recognised");
        assert!(!is_key_dir_name("Documents", k), "a user directory is never ours");
        assert!(!is_key_dir_name("2f0eb0d0aa3c", k), "a shorter hex name is not ours");
        assert!(!is_key_dir_name(&"z".repeat(k), k), "a non-hex name is not ours");
        assert!(!is_key_dir_name("", k));
    }

    // On Windows `set_readonly(false)` is the only way back from the read-only bit set above, and the
    // Unix-oriented lint behind this (a file becoming world writable) cannot apply to this module.
    #[allow(clippy::permissions_set_readonly_false)]
    #[test]
    fn existing_verified_file_survives_a_lost_rename() {
        let tmp = std::env::temp_dir().join(format!("sm_ort_lock_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let target = tmp.join(ONNX_NAME);
        fs::write(&target, ONNX_DLL).unwrap();
        let mut perms = fs::metadata(&target).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&target, perms).unwrap();

        write_verified(&tmp, ONNX_NAME, ONNX_DLL)
            .expect("a verified file that cannot be replaced must be accepted");

        let mut perms = fs::metadata(&target).unwrap().permissions();
        perms.set_readonly(false);
        fs::set_permissions(&target, perms).unwrap();
        assert!(on_disk_matches(&target, ONNX_DLL));

        let _ = fs::remove_dir_all(&tmp);
    }
}
