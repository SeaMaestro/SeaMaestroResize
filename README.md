# SeaMaestro

🔱 Multiformat image resizer and converter + Background remover (SeaMaestroCut.exe) for Windows.
A single static executable — no installer, no external DLLs, no MSVC runtime.
Download, run, done.

> Relax, SeaMaestro is doing the heavy lifting...

## How it works

SeaMaestro is designed to be as simple or as advanced as you need. There are no
config files, no install, no dependencies. Three ways to use it:

**1. Drag and drop.** Drop a photo onto the exe — it is resized in the same
folder. Drop several photos — they land in a `SeaMaestroResized` folder next to
them. Drop a folder — the whole tree is rebuilt under `SeaMaestroResized`,
preserving the folder structure.

**2. Rename the exe.** Bake your settings into the file name. Rename
`SeaMaestro.exe` to `SeaMaestro_q80_w800_webp.exe`, drop a photo on it — you get
an 800px wide WebP at quality 80. Same exe, different name, different output.

**3. Command line.** Full control, plus `stdin → stdout` piping:

```text
SeaMaestro.exe holiday.jpg --size 1600 --format webp --quality 90
SeaMaestroCut.exe portrait.jpg --format png        # transparent PNG
SeaMaestro.exe scans\ --format pdf --scan --merge  # documents -> one PDF
type photo.jpg | SeaMaestro.exe --format webp > out.webp
```

## Contents

**The short version**

- [How it works](#how-it-works)
- [Which file to download](#which-file-to-download)

**Reference**

- [Features](#features) · [Supported formats](#supported-formats)
- [CLI options & EXE rename](#cli-options--exe-rename) — every flag
- [Examples](#examples)
- [PDF & merge](#pdf--merge)
- [SVG & PDF](#svg--pdf)
- [EXIF behavior](#exif-behavior) · [Languages](#languages)
- [Diagnostics](#diagnostics) — timings and cut runtime logs
- [Requirements](#requirements) — RAM, GPU and codec notes

**For developers**

- [Build from source](#build-from-source)
- [License](#license) · [Code signing policy](#code-signing-policy) · [Third-party codecs](#third-party-codecs) · [Author](#author)

## Which file to download

Take `SeaMaestro.exe` if you only need resize / convert / PDF. Take
`SeaMaestroCut.exe` if you also want the background removed.

| file | what it is | size |
| --- | --- | --- |
| `SeaMaestro.exe` | the resizer: resize, crop, smart scan, formats, PDF and merge | ~35 MB |
| `SeaMaestroCut.exe` | the same resizer plus AI background cut (BEN2, DirectML GPU or CPU) | ~300 MB |

The cut build removes the background by default, and it is the **file name** that
switches it on (`cut` inside `SeaMaestroCut.exe`). Renaming the light build to a
name containing `cut` enables nothing: the light build has no cut engine inside, it
only reminds you to take the cut build instead. To make the cut build behave
exactly like the light one, rename it without `cut`/`cutout` (`SeaMaestroRenamed.exe`)
or pass `--nocut` — then the pipeline and the output are identical, and the
inference runtime is never unpacked from the executable.

## Features

- **Batch & parallel** processing with recursive directory scan (Rayon).
- **Resize modes**: long edge (`800`), exact width (`w800`), exact height
  (`h600`), cover crop (`800x600`), percentage (`50pct`).
  - **Fast JPEG downscale**: JPEG→JPEG downscaling uses a scaled-IDCT fast path
    (no full decode), so percentage/width/height downscales of JPEG run faster,
    with the ICC profile preserved.
  - **High-quality upscale**: Lanczos3 resampling keeps raster enlargements crisp,
    and SVG/SVGZ are rasterized at the target size, so vector sources scale to
    any size without pixelation.
- **Convert** between WebP, JPEG, AVIF, JXL, PNG, ICO, TIFF, QOI, BMP, GIF.
- **PDF output** — `--format pdf` writes one PDF per input; `--merge` writes
  one PDF per folder and rebuilds the tree with **Path Compression**. Pages are
  sorted, generated chunked and in parallel, so memory stays constant.
- **Multi-drive & USB routing** — inputs are grouped by drive/network prefix,
  each disk keeps its own output; removable USB drives are never written back
  (output lands next to the program).
- **Quality control** for lossy formats, lossless WebP/JXL/PDF, progressive JPEG.
- **Grayscale** (`--bw`), **sharpen** (`--sharpen`), and **Smart Scan** (`--scan`).
- **Background cut** (`SeaMaestroCut.exe`, `--cut`): AI matting (BEN2) on the GPU
  via DirectML. The inference runtime (~38 MB) is embedded in the executable and
  unpacks once into `%LOCALAPPDATA%\SeaMaestro\ort\` — nothing is installed and
  no DLL sits next to the program.
- **Two cut looks**: `--soft` (default, raw model alpha — best for photos) and
  `--hard` (crisp, old look — best for flat art, logos and screenshots), plus
  `--plain` to keep the raw edge colour without the halo cleanup.
- **ICC color profile passthrough** for JPEG, PNG, JXL, WebP, TIFF, AVIF.
- **EXIF passthrough** (`--exif`) with orientation normalization and
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

`jpeg`/`jpg` (default), `webp`, `avif`, `png`, `jxl`, `ico`, `tiff`/`tif`,
`qoi`, `bmp`, `gif`, `pdf`.

**Metadata**

- ICC color profiles are preserved for `JPEG`, `PNG`, `JXL`, `WEBP`, `TIFF`, `AVIF`.
- EXIF is preserved only with `--exif`. It is read from `JPEG`, `PNG`,
  `WEBP`, `JXL`, `AVIF`, `HEIC/HEIF` and written to `JPEG`, `PNG`, `WEBP`,
  `JXL`, `AVIF`.

## CLI options & EXE rename

Every option has two spellings: a CLI flag (`--flag`) and an exe-name token
(no `--`). Drop the `--` and write the word into the file name — that is all.
Tokens may be separated by `_`, `-`, or glued together.

| CLI flag | EXE token | Description |
| --- | --- | --- |
| `--size 800` / `w800` / `h600` / `800x600` / `50pct` | same tokens | 800 = long edge, w800 = width, h600 = height, 800x600 = cover crop, 50pct = percentage |
| `--quality <1..100>` | `q85` | Lossy quality, default 85 |
| `--format webp\|jpg\|png\|avif\|jxl\|ico\|tiff\|qoi\|bmp\|gif\|pdf` | same token | Output format; jpeg by default, png with `cut` |
| `--bw` | `bw`, `gray`, `grey`, `mono` | Grayscale |
| `--lossless` | `lossless` | Lossless WebP / JXL / PDF (JPEG & AVIF stay lossy) |
| `--progressive` | `progressive`, `prog` | Progressive JPEG |
| `--sharpen` | `sharp` | Sharpen after resize (sigma 1.0, threshold 3) |
| `--scan` | `scan` | Smart Scan filter for document photos (combine with `pdf`/`merge`) |
| `--crop` | `crop` | Auto-crop & deskew a scanned page |
| `--cut` | `cut`, `cutout` | Background cut (cut build): remove background, keep transparency |
| `--soft` / `--hard` | `soft` / `hard` | Edge style: soft = raw model alpha (default), hard = crisp flat art |
| `--plain` | `plain` | Keep raw edge colour, no halo cleanup (alias `--no-de-fringe`) |
| `--nocut` | `nocut` | Resize only, even when the exe name or a flag asks for a cut |
| `--tile` | `tile` | Tiled inference for very large frames (off by default) |
| `--norefine` | `norefine` | Skip the edge refine: global mask only — nets, webbing, thin strands |
| `--epauto` / `--epcpu` / `--epdml` | `epauto` / `epcpu` / `epdml` | Inference provider: auto (default), CPU, DirectML |
| `--threads4` | `threads4` | CPU threads for the cut — put your core count in place of `4` (0 = all cores) |
| `--bgwhite` / `--bgnone` / `--bg#rrggbb` | same tokens | Backdrop: needed for jpeg/pdf, mattes any format once set; `bgnone` keeps alpha |
| `--exif` | `exif` | Keep EXIF metadata (cleared by default; alias `--keep-exif`) |
| `--merge` | `merge` | One PDF per folder, mirroring the tree (Path Compression; implies `--format pdf`) |
| `--name` | `name` | Keep the original file names — no `_cut`/`_w800`/`_q85` suffix (a clash gets `_1`) |
| `--output <FILE>` | — | Output file name/path (single file only) |
| `--nopause` | `nopause` | Do not wait for Enter on exit (alias `--no-pause`) |
| `--shanty` | `shanty` | Sea shanties while working |
| `--lang <CODE>` | `_en`, `_ru`, `_uk`, `_de`, `_es`, `_fr`, `_el`, `_fil` | Interface language (default English) |
| `--profile` | *(hidden)* | Print per-stage timings — see [Diagnostics](#diagnostics) |

Spaced forms (`--bg white`, `--ep cpu`, `--threads 4`) keep working too.

Usage:

```text
SeaMaestro [OPTIONS] <FILES...>
```

**Auto-crop shooting advice.** `--crop` looks for the edge of the sheet, so it
works best when the paper lies on a plain, contrasting surface. Two cases are
outside what the detector can promise:

- the sheet fills the whole frame (no background left to detect), and
- paper and background share the same tone (white sheet on white cloth) — there
  is no edge to find in brightness, so the crop keeps the full frame or trims
  inside the sheet.

Sheet orientation is never forced: the output keeps the way the photo was taken
(EXIF `Orientation` is applied before cropping, so a page shot sideways stays
sideways).

## Examples

```powershell
SeaMaestro.exe --size 800 --format webp --quality 80 photo.jpg
SeaMaestro.exe --size 1024x768 --format jpeg --progressive *.jpg
SeaMaestro.exe --size 50pct --format avif photo.heic
SeaMaestro.exe --size 300 --format png --bw --output result.png photo.jpg
SeaMaestro.exe --format pdf photo.jpg
SeaMaestro.exe --merge vacation_folder
type photo.jpg | SeaMaestro.exe --format webp > out.webp
SeaMaestro.exe --format pdf logo.svg
```

## PDF & merge

```text
SeaMaestro.exe --format pdf photo.jpg             → photo_q85.pdf
SeaMaestro.exe --format pdf --lossless photo.jpg  → photo.pdf (FlateDecode)
SeaMaestro.exe --merge vacation_folder             → vacation_folder_Merged\ (one PDF per folder)
SeaMaestro.exe --merge --lossless --bw folder      → folder_Merged\ (FlateDecode, grayscale)
```

`--merge` builds one document per folder; a folder that contains a single image
simply produces a one-page PDF (it does not need the merge path).

Single PDF keeps the normal output name, e.g. `photo_q85.pdf`.
`--merge` rebuilds the folder tree next to the source: each folder becomes one
PDF. **Path Compression** drops the shared prefix, keeps branches with several
children as real subfolders, and folds single-child paths into the PDF name
(`Trip`/`Day1` → `Trip_Day1_q85.pdf`). Over-long names are capped to ~120
characters (`first_..._last_<hash>`), and paths beyond 260 characters are handled
via the `\\?\` prefix. Pages are sorted, generated chunked and in parallel, and
written to a temp file first, so an interrupted run leaves no half-written PDF
behind.

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

## EXIF behavior

Default: EXIF is removed.

`--exif` (old spelling `--keep-exif` still works): preserves EXIF (input: JPEG, PNG, WebP, AVIF, JXL, HEIC/HEIF;
output: JPEG, PNG, WebP, AVIF, JXL); normalizes Orientation to 1 (the image
is already auto-rotated) and updates pixel dimensions to the resized size.

## Languages

`en` (default), `ru`, `uk`, `de`, `es`, `fr`, `el`, `fil`.

## Diagnostics

These switches only print information — they never change the pixels. Handy for
a bug report, or for comparing two machines or two photos.

| what | how |
| --- | --- |
| per-stage timings (decode / inference / refine / encode) | `--profile` |
| the ONNX Runtime's own log lines (cut build) | set `SEAMAESTRO_ORT_LOG=1` |

`--profile` is hidden from `--help` on purpose. It works in both builds: the light
build prints its own stages, the cut build adds the cut stages.

```text
> SeaMaestroCut.exe portrait.jpg --profile
  ⏱ read | pipeline | encode | preview | total
  ⏱ decode | effects
  ⏱ cut infer | refine
```

If the cut build refuses to start, check the two usual causes first: the inference
runtime unpacks itself into `%LOCALAPPDATA%\SeaMaestro\ort\` on first use (an
unsigned exe that drops DLLs can look suspicious to antivirus — see Windows
Security → Protection history), and SmartScreen may show "Unknown publisher" on
the very first run (see [Code signing policy](#code-signing-policy)).

## Requirements

- Windows 10 (1903+) or 11, **x64 only** — there is no 32-bit build.
- ~4 GB RAM minimum, 8 GB comfortable. The tool keeps its own memory budget at
  60 % of the free RAM (never below 256 MB) and caps a single allocation at 8 GB —
  a frame that cannot fit is refused instead of silently oversubscribing memory.
- ~300 MB for `SeaMaestroCut.exe`, plus ~40 MB in
  `%LOCALAPPDATA%\SeaMaestro\ort\` after the first cut run. The resizer build is
  ~35 MB and writes nothing to AppData.
- The background cut uses the GPU through DirectML when available and falls back
  to the CPU automatically. On the CPU expect several seconds per frame; a
  DirectML-capable GPU (Intel HD 5000+, any 2015+ NVIDIA/AMD) is far faster.
- AVIF encoding needs a CPU with AVX2 (Intel 2011+, AMD 2015+); on older CPUs the
  tool says so instead of failing with a codec error.
- **Intel UHD and DirectML.** The built-in Intel UHD graphics is the least stable
  DirectML backend: on some drivers the GPU backend runs out of memory
  (`8007000E`) or the driver itself fails (`887A0020`, `887A0005`). SeaMaestro
  detects both and continues on the CPU automatically (you will see a note), so
  the job still finishes — but on the CPU a cut frame takes seconds instead of
  fractions of a second. Nothing has to be configured for this: the fallback is
  automatic. If a machine is known to fail on the GPU every single time, the
  attempt can be skipped altogether with `epcpu` in the exe name or `--epcpu`.
- **AVIF alpha in viewers.** Some system viewers on Windows (including some
  versions of *Photos* and Explorer thumbnails) draw AVIF on a white or black
  background and do not show the transparency, while the file itself carries a
  correct alpha channel — check it in Chrome, Edge or Firefox, which render AVIF
  transparency correctly. PNG and WebP are always shown as expected.

## Build from source

> Only needed if you want to compile SeaMaestro yourself. If you came for the
> ready-made exe, use **How it works** and **Which file to download** at the top
> of this page.

Requirements:

- Rust (MSVC toolchain on Windows)
- Windows 10/11
- [vcpkg](https://github.com/microsoft/vcpkg) (manifest deps: HEIC/HEIF,
  AVIF, pkgconf)
- [NASM](https://www.nasm.us/) 2.14+ on `PATH` (dav1d/aom AV1 assembly)

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
& "$vcpkg\vcpkg.exe" install --triplet x64-windows-static --x-install-root="$vcpkg\installed"

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

Output: `target/release/SeaMaestro.exe`

### Two builds

`build_cut.bat` builds both executables: it sets the model and calls
`build_release.bat`, which runs the two cargo builds (light, then the cut build).
The cut build needs the model and the inference runtime, which are **not** stored
in the repository:

```bat
build_cut.bat                     -> dist\SeaMaestro.exe + dist\SeaMaestroCut.exe
build_cut.bat models\Other.onnx   -> the same, with another model
build_release.bat                 -> the worker: both cargo builds (needs SEAMAESTRO_CUT_MODEL)
```

Pinned inputs (their SHA-256 is verified on every CI build):

- `models\BEN2_Base.onnx` — BEN2 matting model (MIT),
  <https://huggingface.co/PramaLLC/BEN2> — 222 932 053 B, sha256
  `22cea62108ff53b7ccc20f7a008bf30494228d84b1687f29ecbe76936a998101`
- `runtime\onnxruntime.dll`, `runtime\DirectML.dll`,
  `runtime\onnxruntime_providers_shared.dll` — the runtime that is embedded into
  the cut build (the `runtime\` folder of this repository)

Both executables carry the same version, but their file metadata differs:
`SeaMaestroCut.exe` reports *SeaMaestro Multiformat Image Resizer + Background
Cut*, so the two builds can be told apart in Explorer.

GitHub Actions builds both: run the `CI` workflow manually to get the two
binaries with their SHA-256 in the job summary and as artifacts; pushing a `v*`
tag creates a release with both files and `checksums.txt`.

## License

SeaMaestro is licensed under the MIT License.

## Code signing policy

Release binaries are currently unsigned. Each release is built locally or by the GitHub workflow and scanned with VirusTotal before upload; Windows SmartScreen may show an
"Unknown publisher" warning on first run.

- **Committers and reviewers**: [Volodymyr Gumanyuk](https://github.com/SeaMaestro)
- **Approvers**: [Volodymyr Gumanyuk](https://github.com/SeaMaestro)
- **Privacy policy**: This program will not transfer any information to other
  networked systems unless specifically requested by the user or the person
  installing or operating it.

## Third-party codecs

This project links several codec libraries, each under its own license
(mostly permissive BSD/MIT/Apache): libjxl, libavif, libheif, libde265,
libwebp, mozjpeg (libjpeg-turbo), dav1d, svt-av1, oxipng, zune-jpeg,
and others.

HEIC/HEIF support uses libheif and libde265, both licensed under LGPL-3.0.
When distributing the binary you must comply with LGPL-3.0 — in particular,
make the libheif and libde265 source available and allow relinking.

mimalloc (MIT) is used as the global allocator.

The full list of third-party licenses is in
[THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md).

## Author

Captain Volodymyr Gumanyuk — seamaestro@proton.me