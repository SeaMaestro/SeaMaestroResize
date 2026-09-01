# Third-Party Licenses

SeaMaestroResize links the following third-party codec libraries. Their
licenses require these notices to be included with binary distributions.

## libheif — HEIC/HEIF

- License: GNU Lesser General Public License v3.0 (LGPL-3.0)
- Project: https://github.com/strukturag/libheif
- License text: https://www.gnu.org/licenses/lgpl-3.0.html

libheif is distributed under LGPL-3.0. The source code of libheif is
available at the project link above, and the source code of SeaMaestroResize
is available in this repository, so the application may be relinked against
a modified libheif.

## libde265 — HEIC/HEIF decoding

- License: GNU Lesser General Public License v3.0 (LGPL-3.0)
- Project: https://github.com/strukturag/libde265
- License text: https://www.gnu.org/licenses/lgpl-3.0.html

libde265 is linked into libheif for HEIC/HEIF decoding. It is distributed
under LGPL-3.0. The source code of libde265 is available at the project link
above, and the source code of SeaMaestroResize is available in this
repository, so the application may be relinked against a modified libde265.

## aom — AV1 decoding (linked into libheif)

- License: BSD 2-Clause
- Project: https://aomedia.googlesource.com/aom
- License text: https://aomedia.googlesource.com/aom/+/refs/heads/main/LICENSE

## mozjpeg / libjpeg-turbo — JPEG

- License: IJG License and Modified (3-clause) BSD License
- Project: https://github.com/mozilla/mozjpeg

This software is based in part on the work of the Independent JPEG Group.

## libwebp — WebP

- License: BSD 3-Clause
- Project: https://chromium.googlesource.com/webm/libwebp

## rav1e — AVIF

- License: BSD 2-Clause
- Project: https://github.com/xiph/rav1e

## ravif — AVIF encoding

- License: BSD 3-Clause
- Project: https://github.com/kornelski/ravif

## avif-serialize — AVIF container serialization

- License: BSD 3-Clause
- Project: https://github.com/kornelski/avif-serialize

## dav1d — AVIF decoding

- License: BSD 2-Clause
- Project: https://code.videolan.org/videolan/dav1d

The Rust bindings (`dav1d`, `dav1d-sys`) are MIT-licensed; the libdav1d
decoder linked into the binary is BSD 2-Clause.

## mp4parse — AVIF/HEIF container parsing

- License: Mozilla Public License 2.0 (MPL-2.0)
- Project: https://github.com/mozilla/mp4parse-rust
- License text: https://www.mozilla.org/en-US/MPL/2.0/

The source code of the MPL-2.0-licensed files is available at the project
link above, and the source code of SeaMaestroResize is available in this
repository.

## resvg / usvg — SVG rendering

- License: Mozilla Public License 2.0 (MPL-2.0)
- Project: https://github.com/RazrFalcon/resvg
- License text: https://www.mozilla.org/en-US/MPL/2.0/

The source code of the MPL-2.0-licensed files is available at the project
link above, and the source code of SeaMaestroResize is available in this
repository.

## jxl-oxide — JPEG XL

- License: Apache-2.0
- Project: https://github.com/imazen/jxl-oxide

## zune-jpeg — JPEG decoding

- License: BSD 3-Clause
- Project: https://github.com/etemesi254/zune-image

## mimalloc — memory allocator

- License: MIT
- Project: https://github.com/microsoft/mimalloc

Copyright (c) Microsoft Corporation, Daan Leijen. Licensed under the MIT License.

## Other Rust crates

The remaining Rust dependencies are licensed under MIT, Apache-2.0,
BSD-style, or MPL-2.0 licenses. MPL-2.0 crates with their own code
(`mp4parse`, `resvg`, `usvg`) are listed above. The full list is recorded in
`Cargo.lock`.
