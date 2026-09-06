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

## libavif — AVIF

- License: BSD-2-Clause AND Apache-2.0
- Project: https://github.com/AOMediaCodec/libavif

## dav1d — AV1 decoding (linked into libavif)

- License: BSD-2-Clause AND ISC
- Project: https://code.videolan.org/videolan/dav1d

## svt-av1 — AV1 encoding (linked into libavif)

- License: BSD-3-Clause-Clear, with the Alliance for Open Media Patent License 1.0
- Project: https://gitlab.com/AOMediaCodec/SVT-AV1

## libjxl — JPEG XL

- License: BSD-3-Clause
- Project: https://github.com/libjxl/libjxl

## brotli — compression (linked into libjxl)

- License: MIT
- Project: https://github.com/google/brotli

## highway — SIMD (linked into libjxl)

- License: Apache-2.0
- Project: https://github.com/google/highway

## lcms — color management (linked into libjxl)

- License: MIT
- Project: https://github.com/mm2/Little-CMS

## libyuv — YUV conversion (linked into libavif)

- License: BSD-3-Clause
- Project: https://chromium.googlesource.com/libyuv/libyuv

## fastfeat — FAST feature detection (linked into svt-av1)

- License: BSD-3-Clause

## mozjpeg / libjpeg-turbo — JPEG

- License: IJG License and Modified (3-clause) BSD License
- Project: https://github.com/mozilla/mozjpeg

This software is based in part on the work of the Independent JPEG Group.

## libwebp — WebP

- License: BSD 3-Clause
- Project: https://chromium.googlesource.com/webm/libwebp

## resvg / usvg — SVG rendering

- License: Mozilla Public License 2.0 (MPL-2.0)
- Project: https://github.com/RazrFalcon/resvg
- License text: https://www.mozilla.org/en-US/MPL/2.0/

The source code of the MPL-2.0-licensed files is available at the project
link above, and the source code of SeaMaestroResize is available in this
repository.

## jxl-oxide — JPEG XL (RAW decoding, via rawler)

- License: Apache-2.0
- Project: https://github.com/imazen/jxl-oxide

## zune-jpeg — JPEG decoding

- License: BSD 3-Clause
- Project: https://github.com/etemesi254/zune-image

## zlib-ng — zlib compression

- License: zlib License
- Project: https://github.com/zlib-ng/zlib-ng
- License text: https://github.com/zlib-ng/zlib-ng/blob/develop/LICENSE.md

## mimalloc — memory allocator

- License: MIT
- Project: https://github.com/microsoft/mimalloc

Copyright (c) Microsoft Corporation, Daan Leijen. Licensed under the MIT License.

## Other Rust crates

The remaining Rust dependencies are licensed under MIT, Apache-2.0,
BSD-style, or MPL-2.0 licenses. MPL-2.0 crates with their own code
(`resvg`, `usvg`) are listed above. The full list is recorded in
`Cargo.lock`.
