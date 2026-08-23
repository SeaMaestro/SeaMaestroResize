# SeaMonkeyResize

⚓ Multiformat batch image resizer and converter for Windows.
A single static executable — no installer, no external DLLs, no MSVC runtime.
Download, run, done.

> Relax, SeaMonkey is doing the heavy lifting...

## The idea

No installer, no config files, no dependencies. Copy `SeaMonkeyResize.exe`
anywhere, rename it to bake in your settings, and drop photos onto it. That's
the whole workflow.

- Drop **one photo** → it's resized next to the original, in the same folder.
- Drop **several photos** → they land in a `SeaMonkeyResized` folder next to them.
- Drop a **folder with subfolders** → the whole tree is rebuilt under
  `SeaMonkeyResized`, preserving the folder structure (each subfolder keeps its
  name with a `_Resized` suffix).

## Features

- **Batch & parallel** processing with recursive directory scan (Rayon).
- **Resize modes**: long edge (`800`), exact width (`w800`), exact height
  (`h600`), cover crop (`800x600`), percentage (`50pct`).
- **Convert** between WebP, JPEG, AVIF, JXL, PNG, ICO, TIFF, QOI, BMP, GIF.
- **Quality control** for lossy formats, lossless WebP/JXL, progressive JPEG.
- **Grayscale** (`--bw`) and **sharpen** (`--sharpen`).
- **ICC color profile passthrough** for JPEG, PNG, JXL, WebP, TIFF.
- **EXIF passthrough** (`--keep-exif`) with orientation normalization and
  resized pixel-dimension update; EXIF is cleared by default.
- **Auto-rotation** from EXIF `Orientation`.
- **Drag-and-drop / EXE rename**: bake settings into the executable name.
- **stdin → stdout** pipe mode.
- **8 languages**: English, Русский, Українська, Deutsch, Español, Français,
  Ελληνικά, Filipino.
- **Sea shanties** while it works (`--shanty`).

## Supported formats

**Input**

JPEG, PNG, WebP, AVIF, JXL, ICO, TIFF, QOI, BMP, GIF, HEIC/HEIF, and RAW
(CR2, CR3, CRW, NEF, NRW, ARW, SRF, SR2, DNG, RAF, ORF, PEF, RW2, MRW, MEF,
ERF, KDC, DCS, DCR, SRW, IIQ, 3FR, MOS, X3F, ARI).

**Output**

`webp`, `jpeg`/`jpg`, `avif`, `png`, `ico`, `tiff`/`tif`, `qoi`, `bmp`,
`gif`, `jxl`.

## Build

Requirements:

- Rust (MSVC toolchain on Windows)
- Windows 10/11
- Optional: [vcpkg](https://github.com/microsoft/vcpkg) for HEIC/HEIF
  (set `VCPKG_ROOT`)

```powershell
cargo build --release
```

The release binary is built as a single static executable (static CRT). To
enable static CRT on a fresh clone, create a local `.cargo/config.toml`
(already gitignored) or export the flag:

```toml
[target.x86_64-pc-windows-msvc]
rustflags = ["-C", "target-feature=+crt-static"]
```

Output: `target/release/SeaMonkeyResize.exe`

## Usage

```text
SeaMonkeyResize [OPTIONS] <FILES...>
```

### CLI options

| Flag | Description |
| --- | --- |
| `--size <SIZE>` | 800 (long edge), w800, h600, 800x600 (cover crop), 50pct |
| `--quality <1..100>` | Lossy quality, default 85 |
| `--format <FMT>` | Output format, default webp |
| `--bw` | Grayscale (black & white) |
| `--lossless` | Lossless WebP / JXL (quality ignored) |
| `--progressive` | Progressive JPEG |
| `--sharpen` | Sharpen after resize (sigma=1.0, threshold=3) |
| `--keep-exif` | Keep EXIF metadata (cleared by default) |
| `--output <FILE>` | Output file name/path (single file only) |
| `--no-pause` | Do not wait for Enter on exit |
| `--shanty` | Sea shanties while working |
| `--lang <CODE>` | en, ru, uk, de, es, fr, el, fil |
| `--help` | Show help |

### Examples

```powershell
seamonkey --size 800 --format webp --quality 80 photo.jpg
seamonkey --size 1024x768 --format jpeg --progressive *.jpg
seamonkey --size 50pct --format avif photo.heic
seamonkey --size 300 --format png --bw --output result.png photo.jpg
cat photo.jpg | seamonkey --format webp > out.webp
```

### EXE rename (drag-and-drop)

Rename the executable to bake in settings. Tokens may be separated by `_`,
`-`, or glued together.

| Token | Meaning |
| --- | --- |
| `q80` | Quality 80 (1..100) |
| `w800` | Width 800 |
| `h600` | Height 600 |
| `800x600` | Cover crop to 800×600 |
| `50pct / 50p / 50%` | Scale to 50% |
| `800` | Long edge 800 |
| `webp jpg avif png jxl ico tiff qoi bmp gif` | Output format |
| `bw grayscale gray grey mono` | Grayscale |
| `lossless` | Lossless WebP/JXL |
| `progressive prog` | Progressive JPEG |
| `sharp` | Sharpen |
| `exif` | Keep EXIF |
| `shanty` | Sea shanties |

Examples:

- `SeaMonkeyResize_q80_w800_webp.exe` — quality 80, 800px wide, WebP
- `SeaMonkeyResize1920jpgq85_DE.exe` — 1920px wide, JPEG, quality 85, German language
- `SeaMonkeyResize_w300_h300_png_bw.exe` — 300×300 cover crop, PNG, grayscale

## EXIF behavior

Default: EXIF is removed.

`--keep-exif`: preserves EXIF for JPEG, PNG, WebP and JXL; normalizes
Orientation to 1 (the image is already auto-rotated) and updates pixel
dimensions to the resized size. AVIF EXIF write is not supported.

## Languages

`en` (default), `ru`, `uk`, `de`, `es`, `fr`, `el`, `fil`.

## License

SeaMonkeyResize is licensed under the MIT License.

## Third-party codecs

This project links several codec libraries, each under its own license
(mostly permissive BSD/MIT/Apache): libwebp, mozjpeg, rav1e (ravif),
jxl-oxide, oxipng, and others.

HEIC/HEIF support uses libheif, which is licensed under LGPL-3.0.
When distributing the binary you must comply with LGPL-3.0 — in particular,
make the libheif source available and allow relinking.

## Author

Captain Volodymyr Gumanyuk — captvg@proton.me
