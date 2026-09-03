# SeaMaestroResize

⚓ Multiformat batch image resizer and converter for Windows.
A single static executable — no installer, no external DLLs, no MSVC runtime.
Download, run, done.

> Relax, SeaMaestro is doing the heavy lifting...

## The idea

No installer, no config files, no dependencies. Copy `SeaMaestroResize.exe`
anywhere, rename it to bake in your settings, and drop photos onto it. That's
the whole workflow.

- Drop **one photo** → it's resized next to the original, in the same folder.
- Drop **several photos** → they land in a `SeaMaestroResized` folder next to them.
- Drop a **folder with subfolders** → the whole tree is rebuilt under
  `SeaMaestroResized`, preserving the folder structure (each subfolder keeps its
  name with a `_Resized` suffix).
- Drop onto a **merge-renamed exe** → one `SeaMaestroMerged{settings}.pdf`.

## Features

- **Batch & parallel** processing with recursive directory scan (Rayon).
- **Resize modes**: long edge (`800`), exact width (`w800`), exact height
  (`h600`), cover crop (`800x600`), percentage (`50pct`).
  - **High-quality upscale**: Lanczos3 resampling keeps raster enlargements crisp,
  and SVG/SVGZ are rasterized at the target size, so vector sources scale to
  any size without pixelation.
- **Convert** between WebP, JPEG, AVIF, JXL, PNG, ICO, TIFF, QOI, BMP, GIF.
- **PDF output** — `--format pdf` writes one PDF per input; `--merge` combines
  inputs into one sorted multi-page PDF. The merge streams to a temp file and
  uses chunked parallel page generation, so memory stays constant.
- **Quality control** for lossy formats, lossless WebP/JXL/PDF, progressive JPEG.
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

- Standard: `JPEG`, `PNG`, `GIF`, `WEBP`, `AVIF`, `JXL`, `HEIC/HEIF/HIF`,
  `TIFF`, `ICO`, `BMP`, `QOI`
- Vector: `SVG`, `SVGZ`
- Specialized: `TGA`, `HDR`, `EXR`, `DDS`, `PNM/PBM/PGM/PPM/PAM`,
  `Farbfeld (FF)`
- RAW: `CR2`, `CR3`, `CRW`, `NEF`, `NRW`, `ARW`, `SRF`, `SR2`, `DNG`, `RAF`,
  `ORF`, `PEF`, `RW2`, `MRW`, `MEF`, `ERF`, `KDC`, `DCS`, `DCR`, `SRW`, `IIQ`,
  `3FR`, `MOS`, `X3F`, `ARI`

**Output**

`webp` (default), `jpeg`/`jpg`, `avif`, `png`, `jxl`, `ico`, `tiff`/`tif`,
`qoi`, `bmp`, `gif`, `pdf`.

**Metadata**

- ICC color profiles are preserved for `JPEG`, `PNG`, `JXL`, `WEBP`, `TIFF`.
- EXIF is preserved only with `--keep-exif`, and only for `JPEG`, `PNG`,
  `WEBP`, `JXL`.

## Build

Requirements:

- Rust (MSVC toolchain on Windows)
- Windows 10/11
- [vcpkg](https://github.com/microsoft/vcpkg) (manifest deps: HEIC/HEIF,
  AVIF, pkgconf)
- [NASM](https://www.nasm.us/) 2.14+ on `PATH` (rav1e AVIF encoder)

```powershell
# NASM (if not already installed)
winget install --id NASM.NASM -e

$vcpkg = "C:\path\to\vcpkg"
$env:VCPKG_ROOT = $vcpkg
$env:VCPKG_DEFAULT_TRIPLET = "x64-windows-static"
$env:VCPKGRS_TRIPLET = "x64-windows-static"
$env:PKG_CONFIG = "pkgconf"
$env:PKG_CONFIG_PATH = "$vcpkg\installed\x64-windows-static\lib\pkgconfig"

# Build manifest dependencies (static triplet)
& "$vcpkg\vcpkg.exe" install --triplet x64-windows-static

# pkgconf from the vcpkg manifest must be discoverable
$env:Path = "$vcpkg\installed\x64-windows-static\tools\pkgconf;$env:Path"

cargo build --release
```

The release binary is built as a single static executable (static CRT). To
enable static CRT on a fresh clone, create a local `.cargo/config.toml`
(already gitignored) or export the flag:

```toml
[target.x86_64-pc-windows-msvc]
rustflags = ["-C", "target-feature=+crt-static"]
```

Output: `target/release/SeaMaestroResize.exe`

## Usage

```text
SeaMaestroResize [OPTIONS] <FILES...>
```

## CLI options

| Flag | Description |
| --- | --- |
| `--size <SIZE>` | 800 (long edge), w800, h600, 800x600 (cover crop), 50pct |
| `--quality <1..100>` | Lossy quality, default 85 |
| `--format <FMT>` | Output format, default webp (webp, jpeg, avif, jxl, png, ico, tiff, qoi, bmp, gif, pdf) |
| `--bw` | Grayscale (black & white) |
| `--lossless` | Lossless WebP / JXL / PDF (quality ignored) |
| `--progressive` | Progressive JPEG |
| `--sharpen` | Sharpen after resize (sigma=1.0, threshold=3) |
| `--keep-exif` | Keep EXIF metadata (cleared by default) |
| `--merge` | Combine all inputs into one sorted multi-page PDF (implies `--format pdf`) |
| `--output <FILE>` | Output file name/path (single file only) |
| `--no-pause` | Do not wait for Enter on exit |
| `--shanty` | Sea shanties while working |
| `--lang <CODE>` | en, ru, uk, de, es, fr, el, fil |
| `--help` | Show help |

## Examples

```powershell
seamaestro --size 800 --format webp --quality 80 photo.jpg
seamaestro --size 1024x768 --format jpeg --progressive *.jpg
seamaestro --size 50pct --format avif photo.heic
seamaestro --size 300 --format png --bw --output result.png photo.jpg
seamaestro --format pdf photo.jpg
seamaestro --merge vacation_folder
cat photo.jpg | seamaestro --format webp > out.webp
seamaestro --format pdf logo.svg
```

## PDF & merge

```text
SeaMaestroResize --format pdf photo.jpg             → photo_q85.pdf
SeaMaestroResize --format pdf --lossless photo.jpg  → photo.pdf (FlateDecode)
SeaMaestroResize --merge vacation_folder            → SeaMaestroMerged_q85.pdf
SeaMaestroResize --merge --lossless --bw folder     → SeaMaestroMerged_bw.pdf
```

Single PDF keeps the normal output name, e.g. `photo_q85.pdf`.
`--merge` forces PDF output, sorts inputs by path, and writes pages in sorted order.
The merged PDF is written to a temp file first and renamed at the end, so an interrupted run leaves no half-written PDF behind.

## SVG & PDF

SVG/SVGZ inputs render two ways:

- **Raster** (all non-PDF outputs, and the PDF fallback): rendered with resvg at
  the target size. Gradients, filters, masks, clip paths, patterns, embedded
  images and text are supported.
- **Vector** (`--format pdf` / `--merge`): flat graphics — solid fill/stroke,
  gradients (PDF shadings), tiling patterns, transforms, opacity, dashes and
  text — are written as native PDF, so they stay sharp at any zoom and produce
  small files.

The vector engine embeds TrueType and OpenType CFF fonts subsetted to the used
glyphs (`CIDFontType2` / `CIDFontType0C`), keeping text selectable. If a font
cannot be subsetted (e.g. color fonts) or text uses a non-solid paint, the text
is flattened to curves. Embedded JPEGs are passed through as `DCTDecode`; PNGs
are decoded and re-encoded with `/SMask` for alpha and ICC preserved when
present.

If an SVG contains anything the vector writer cannot map to PDF (masks, filters,
non-normal blend modes, transparent gradient stops, isolated groups), the whole
page falls back to raster automatically — correct output, just not vector.

Notes:
- `--sharpen` and cover-crop sizes (`WxH`) always rasterize SVG.
- `--bw` stays vector: grayscale is applied natively to the PDF vector output.
- Transparent gradient stops (`stop-opacity < 1`) rasterize the page.
- Animations, scripting and other dynamic SVG features are not supported
  (static SVG subset only).

## EXE rename (drag-and-drop)

Rename the executable to bake in settings. Tokens may be separated by `_`,
`-`, or glued together.

| Token | Meaning |
| --- | --- |
| `q80` | Quality 80 (1..100) |
| `w800` | Width 800 |
| `h600` | Height 600 |
| `800x600` | Cover crop to 800×600 |
| `50pct` / `50p` / `50%` | Scale to 50% |
| `800` | Long edge 800 |
| `webp` `jpg` `avif` `png` `jxl` `ico` `tiff` `qoi` `bmp` `gif` `pdf` | Output format |
| `bw` `grayscale` `gray` `grey` `mono` | Grayscale |
| `lossless` | Lossless WebP/JXL/PDF |
| `progressive` `prog` | Progressive JPEG |
| `sharp` | Sharpen |
| `exif` | Keep EXIF |
| `shanty` | Sea shanties |
| `merge` | Merge inputs into one PDF |

Examples:

`SeaMaestroResize_q80_w800_webp.exe` — quality 80, 800px wide, WebP
`SeaMaestroResize1920jpgq85_DE.exe` — 1920px wide, JPEG, quality 85, German language
`SeaMaestroResize_w300_h300_png_bw.exe` — 300×300 cover crop, PNG, grayscale
`SeaMaestroResize_merge.exe` — merge dropped files/folder into one PDF

## EXIF behavior

Default: EXIF is removed.

`--keep-exif`: preserves EXIF for JPEG, PNG, WebP and JXL; normalizes
Orientation to 1 (the image is already auto-rotated) and updates pixel
dimensions to the resized size. AVIF EXIF write is not supported.

## Languages

`en` (default), `ru`, `uk`, `de`, `es`, `fr`, `el`, `fil`.

## License

SeaMaestroResize is licensed under the MIT License.

## Code signing policy

Release binaries are currently unsigned. Each release is built locally and
scanned with VirusTotal before upload; Windows SmartScreen may show an
"Unknown publisher" warning on first run.

- **Committers and reviewers**: [Volodymyr Gumanyuk](https://github.com/SeaMaestro)
- **Approvers**: [Volodymyr Gumanyuk](https://github.com/SeaMaestro)
- **Privacy policy**: This program will not transfer any information to other
  networked systems unless specifically requested by the user or the person
  installing or operating it.

## Third-party codecs

This project links several codec libraries, each under its own license
(mostly permissive BSD/MIT/Apache): libwebp, mozjpeg, rav1e (ravif),
jxl-oxide, oxipng, and others.

HEIC/HEIF support uses libheif, which is licensed under LGPL-3.0.
When distributing the binary you must comply with LGPL-3.0 — in particular,
make the libheif source available and allow relinking.

mimalloc (MIT) is used as the global allocator.

## Author

Captain Volodymyr Gumanyuk — seamaestro@proton.me