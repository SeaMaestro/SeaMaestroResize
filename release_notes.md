## SeaMaestro v2.5.6

## ✨ What's New

**Background cut now overlaps frames when the machine has the memory for it.** The
cut build used to process strictly one frame at a time, which left the CPU
idle while the GPU was thinking. SeaMaestro now looks at the frames you are about to
process (their dimensions, the output format and the `--size` you asked for), works out
how much memory one finished frame needs, and overlaps as many frames as the free-memory
budget allows — never more than that. Measured on a three-frame 26 MP batch: **314 s
instead of 447 s** with JXL output (−30 %), and single-digit percent on PNG. On a
modest machine nothing changes — the memory budget lets only the work that fits start,
exactly as it already does for every other stage. Output bytes are identical either way.

**A downscaled cut is no longer refused.** With `--cut --size 4000` the memory estimate
was made from the *original* frame, so a large photo could be turned away on a machine
that had room for the work it actually asked for. The estimate now follows the size you
asked for.

**Very large frames get a real answer instead of a scare.** A 201-megapixel frame
(16384×12288) is handled in plain words: either the *frame* is too large for the memory
budget, or the *input file* exceeds the size cap — two different messages now, because
they are two different problems. Cutting 201 MP into JXL needs about 15.9 GB, so on an
ordinary laptop it is refused **before a single pixel is decoded** rather than dying
half-way through with the process killed by the system.

## 🐛 Bug Fixes

**Batch jobs no longer run the codec single-threaded by mistake.** The thread count
handed to the JXL/AVIF codec was derived from "how many files are coming", so a job that
processed files one at a time (background cut, and merged PDFs) told the codec to use one
thread even when a single file was in flight. The count now follows the *actual* number of
workers: a lone file keeps every core, a parallel batch gives each worker one thread so
the cores are not oversubscribed, and the `--merge` path sets its own. Output bytes are
unchanged — verified byte-for-byte against the sealed 26 MP references, both for a single
file and for a batch, and identical between 1 and 2 workers.

**TGA files were listed as supported but never decoded.** `--help` has always advertised
TGA among the input formats, yet every real `.tga` was refused as an unsupported file.
The reason is structural: TGA is the only promised format with no signature bytes, so the
decoder — which picks a format by looking at the content — had nothing to recognise it by.
TGA is now dispatched by file name, and a stream that has no name (a pipe) is recognised by its
18-byte header instead — so both routes work. A file is still never *guessed* to be a TGA when its
name says something else: the header is only consulted when there is no name at all.

**AVIF files that announce themselves as `mif1` were refused.** ISO/IEC 14496-12 lets a file name a
primary brand and then list the formats it is compatible with, and some writers put `mif1` first with
`avif` in that list — which is a perfectly valid AVIF. SeaMaestro only ever looked at the primary
brand, so such files fell through to the HEIF reader, which cannot decode AV1, and came back as
unsupported. Both brand lists are now read, and a file that claims AVIF anywhere in them is decoded
as AVIF, before the generic HEIF reader gets a chance at it. Verified against a third-party AVIF with
its brand rewritten: it decodes to the same pixels as the original.

**EXIF written by another tool was dropped from AVIF files.** Files produced by our own
encoder were fine, which is exactly why this stayed hidden: our writer and our reader shared
the same assumption about where the TIFF header starts. Other writers keep a 4-byte offset
field in front of it, so their metadata was silently discarded. The payload is now trimmed to
its TIFF header — the same normalisation the HEIF path has used all along. Verified with a
file tagged by `exiftool`, an entirely separate writer.

**`--exif` is no longer silently ignored.** When the target format cannot store EXIF
(TIFF, PDF, BMP, GIF, ICO, QOI), or when a metadata blob cannot be rewritten for the new frame
size, SeaMaestro now says so instead of producing a file without metadata and no comment.

**A file name containing `{}` could corrupt the per-file progress line.** Every `{}` was
filled one call at a time, re-scanning text that had already been inserted, so a name like
`my{}file.jpg` shifted all the following fields and left a literal `{}` behind. Placeholders
are now filled in a single pass over the template; inserted text is never looked at again.

## 🔧 Under the hood

Determinism was re-checked end to end on the 26 MP reference frame: 1 worker and 2 workers
produce the same bytes, and the cut-PNG output still matches the sealed reference exactly.
Peak memory for two overlapping frames grows by well under 10 % on a 32 GB machine,
because the budget and the single inference session keep roughly one frame in flight. Two
frames at a time is also where the gain stops: on PNG three workers change nothing and four
are measurably slower (the encoders and the shared work queues start competing), so the cut
build stays at two.

Two diagnostic knobs were added for measuring this on real hardware. `SEAMAESTRO_RAM_MB=4096`
makes SeaMaestro behave like a machine with that much RAM (every memory-adaptive decision
follows it), and `SEAMAESTRO_CUT_WORKERS=1` pins the number of cut workers. Both are for
measurement only and are ignored when unset — normal runs are unaffected.

**The background-cut runtime is unpacked more carefully.** On first use the cut build writes
its inference runtime into `%LOCALAPPDATA%\SeaMaestro\ort\...`. Two things changed: the
version-cleaning step now removes *only* directories whose name matches the exact pattern
SeaMaestro itself created (and does nothing at all when `SEAMAESTRO_RUNTIME_DIR` points at a
directory you chose — that one is not ours to tidy), and temporary files are written under an
unpredictable name and created exclusively, so a write can never follow a link planted by
another account, and a leftover file from a crashed run cannot block the next start.

**Release acceptance grew from 10 checks to 21**, and each of the new ones has now actually
been executed against a real build. Added gates: crafted broken containers must produce a
truthful verdict (never a memory scare), PPM and TGA must decode, HEIF baselines must be
unchanged, EXIF must survive when the tags were written by a *different* tool (`exiftool`),
orientation must be normalised in the output, the EXIF size tags must follow the resized
image, a file name containing braces must not corrupt the log, an unsupported file must be
named as unsupported — and a 201 MP cut+JXL job must still be refused before a single pixel is
decoded. One of them rewrites the brand of a third-party AVIF into the `mif1` form described above
and requires the same pixels out of it, so the sample lives inside the harness instead of in a temp
folder where nobody would remember it. The sealed 26 MP PNG and JXL references are unchanged by this
release.

**A vector PDF page is never drawn half-finished.** If a coordinate or a gradient turns out to be
non-finite, the whole page is rasterised instead — the same outcome as for a gradient the generator
cannot represent. A PDF has two correct results, vector and raster, and the vector one is preferred
only when it is *also* correct.

**The memory accounting for the scan path stopped being optimistic.** The scratch buffers used by
chroma denoising were allocated per call and were not covered by the reservation the tool makes
before it starts: it asked for five planes' worth and used eleven. Those buffers are now reused and
the request matches what is actually held. The reservation therefore went up while the real
consumption went down — which means a machine that is borderline on memory may now decline that step
instead of overcommitting. That is the intended trade: skipping a denoise beats lying about our own
appetite.

## 🔔 Signing

This release is **unsigned**. Windows SmartScreen may show an "Unknown
publisher" warning on first run.

SHA-256 (light build, SeaMaestro.exe): TO_BE_FILLED
SHA-256 (cut build, SeaMaestroCut.exe): TO_BE_FILLED
Size: TO_BE_FILLED MB (PE executable, 64-bit)
VirusTotal (light build): TO_BE_FILLED
VirusTotal (cut build): TO_BE_FILLED

The release contains two files: `SeaMaestro.exe` (resizer) and
`SeaMaestroCut.exe` (resizer + background cut). The cut build writes its
inference runtime into `%LOCALAPPDATA%\SeaMaestro\ort\` on first use; an
unsigned executable that drops DLLs can trip antivirus heuristics — if the cut
fails to start, check Windows Security → Protection history.

## 📄 License

MIT. See LICENSE and THIRD_PARTY_LICENSES.md.

---


## SeaMaestro v2.5.5

## ✨ What's New

**Keep the original file names — `--name`.** By default SeaMaestro marks its
output (`photo_cut.png`, `photo_w800_q85.jpg`, …) so you can tell a converted
file from the original at a glance. When you want the names out of the way, add
`--name` (or put `name` in the exe name, e.g. `SeaMaestroCut_w800_name.exe`): the
output keeps the original stem — `photo.png`, `photo.jpg` — with no `_cut`,
`_w800`, `_q85`, `_bw`, `_crop` or `_scan` appended. Everything else is
unchanged: the same output folder, the same folder structure for whole
directories, the same pixels. A name clash is still resolved with a counter
(`photo_1.png`), so nothing is ever overwritten on disk. The flag works in both
builds and also covers merged PDFs (`--merge`: `report_Merged.pdf` instead of
`report_Merged_cut.pdf`). If you pass `--output` explicitly, that path still wins.

**Crisper edges where the refine used to give up (adaptive scale).** Refining the
edge at native resolution was all-or-nothing: if the edge band needed more tiles
than one pass allows — a 26 MP frame on the CPU, or an extremely tall frame (up to
10000 px) on the GPU — the refine switched off completely and the whole edge
stayed soft. Now SeaMaestro steps the tile size up instead (×2, then ×4) until the
work fits the budget, and says so in plain words: *"The edge is refined at reduced
scale s=2 — that much fine rigging can't be overhauled in one watch (6 tiles,
limit 12). Sharper than no refine at all."* The default 26 MP frame on the GPU is
untouched (scale 1, byte-for-byte the same output), and the reduced scale only
ever replaces a soft edge, never a sharp one.

## 🐛 Bug Fixes

**`--bg` now works for every format.** The backdrop colour used to be applied only
where transparency is impossible (JPEG/PDF); for PNG, WebP, AVIF, JXL or TIFF it was
silently ignored, so `--cut --format png --bg gray` still gave you a transparent PNG.
If you ask for a colour, you get it: any format is now matted on the colour you set
(alpha is dropped, exactly as with JPEG). Asking for transparency explicitly
(`--bg transparent`, `--bg none`) keeps the alpha, exactly as before. PDF honours the
colour too — with `--cut` and for a transparent source converted straight to PDF, which
used to be forced onto white (that covers the raster path and the vector SVG→PDF path,
where the page is now painted before the artwork is drawn). `--preview` keeps using the colour as before.
Without `--bg` nothing changes — alpha formats stay transparent, JPEG/PDF stay on white.

**A transparent source no longer loses its alpha in silence.** Converting a transparent
PNG straight to JPEG (or PDF) used to drop the alpha channel with no backdrop at all,
which left those pixels black; the frame is now matted — on the colour you set, or on
white when you don't. The same validation now covers `--bg transparent` for a format that
cannot hold transparency (JPEG/PDF): instead of a silent white you get the plain-language
refusal that already existed for `--cut`.

**JXL and HEIF round-trips are complete now.** An EXIF block written into a `.jxl`
used to be unreadable — SeaMaestro's own reader (and every other tool) skips the 4-byte
TIFF offset the format requires, and the encoder was not writing one; it is written now.
HEIF frames whose last row is not padded to the stride lost that row and failed to decode
entirely; the row is kept. Images embedded in an SVG are now decoded under the same memory
limits as everything else, instead of being able to allocate gigabytes behind the budget.

**Piped input is now checked against the memory budget while it is read.** An
oversized pipe used to be pulled into RAM in full before the budget was consulted,
so on a small machine — or in a container — the OS could kill the process before it
ever got the chance to report that the input was too large. The stream is now read
in 64 KiB chunks, each chunk is checked against the budget as it arrives and the
memory it needs is held until the input has been processed, so the read stops at the
budget instead of overshooting it; the input-size cap is enforced in the same loop.
The `Press Enter to exit` prompt is no longer printed when there is no console to
press it in (piped or redirected input), and `--nopause` is now honoured on the
error path as well.

**Two smaller fixes.** `w800profile` in an exe name is no longer split as `prog` + `ile`
(keep-names and profile tokens are matched whole), and an output path written as
`out/../photo.png` is recognised as the input file, so the overwrite guard fires.

**The banner now lists every setting in force.** Flags that used to run without
ever showing up in the header — no-refine, the plain (raw) edge, tiled cutting,
the hard edge, the backend (`--ep cpu`), `--threads`, `--bg`, `--preview` and the
new keep-names — are printed now, each one only when it differs from the default.
You can see at a glance what the current run is actually doing.

**The banner no longer cuts long lines short.** The header box used to be a fixed
width, so the Spanish and German titles lost their last words to "…". The box now
sizes itself to the longest line it has to print (localized labels included), so
every language fits.

## 🔧 Under the hood

- Tiled cut and reduced-scale refine report the tile size they really use (×2 and
  ×4 at reduced scale), instead of the base 1024 px constant.

- PNG saving on large frames is faster. Above 8 MP the lossless PNG optimizer runs
  at a lighter preset: on the 26 MP reference frame the save stage drops from
  ~57 s to ~8 s at the cost of a ~10–15 % larger file (33.2 MB vs 29.6 MB). The
  pixels are identical — this is compression effort, not image quality. Frames of
  8 MP and below are encoded exactly as before.
- Edge cleanup (de-fringe) now checks free memory instead of a hard 4 MP pixel
  threshold, so a strip can still run in parallel on a machine that has headroom.
  The result is byte-identical; only the peak memory is lower.
- Cutting on the CPU is announced once: *"Main engine's out — we're on the oars
  (CPU). Slow going, but every seam gets stitched by hand: the edge comes out
  crisp and clean."*, together with the reminder that `--norefine` (or `_norefine`
  in the exe name) makes port sooner, at the cost of a softer edge. The note is
  printed at most once per run.
- Memory accounting is tighter. The frame being read now stays inside the memory
  budget for the whole run instead of only while the file is being read, and the
  perspective crop (`--crop`) reserves its output buffer before allocating it — a
  huge deskew used to be able to ask for ~3 GB behind the budget's back. On a busy
  machine that is the difference between a slow run and a swap storm.
- Edge cases in the deskew were closed too: a document photographed at roughly 45°
  (a diamond-shaped corner order) is now ordered correctly instead of silently
  skipping the perspective correction, and the AVIF encoder's logging hook is set
  up once instead of from every worker thread.
- `--profile` is new — a hidden measuring switch (`--profile`, not listed in
  `--help`, also available as `_profile` in the exe name). It prints where the time
  went: `read / decode / effects / cut (infer / refine / tiled / de-fringe) /
  encode / preview / total`. It changes nothing in the output, it works in both
  builds — the resizer shows the stages it has, while the cut stages appear only in
  the cut build — and it is the first thing to switch on when a run feels slow.
- The raw ONNX Runtime log lines no longer spill into the console. When the GPU
  runs out of memory or the driver hiccups, you used to see the runtime's own
  red `[E:onnxruntime:…]` text just before SeaMaestro's own note — true, but it
  looked like a crash report. The runtime logger is now routed through the tool,
  so you get only the plain-language message (the fallback to the CPU happens
  exactly as before). Need the technical log for a bug report? Set
  `SEAMAESTRO_ORT_LOG=1` in the environment and the full runtime output comes back.

## 📦 The two files

| file | what it is | size |
| --- | --- | --- |
| `SeaMaestro.exe` | the resizer: resize, crop, smart scan, formats, PDF and merge | ~35 MB |
| `SeaMaestroCut.exe` | the same resizer plus AI background cut (BEN2, DirectML GPU or CPU) | ~300 MB |

The cut build is a background remover by default, and the file name is what switches
it on (`cut` inside `SeaMaestroCut.exe`), so no flag is needed: drop a photo onto it —
or run `SeaMaestroCut.exe photo.jpg` — and you get a transparent PNG. Renaming the
light build to a name containing `cut` enables nothing: the light build has no cut
engine in it, it only reminds you to take the cut build instead.

Want the cut build to behave exactly like the light one? Rename it so the name has no
`cut`/`cutout` in it (`SeaMaestroRenamed.exe`), or pass `--nocut`. Then the pipeline,
the options and the output are identical to `SeaMaestro.exe`, and the inference
runtime is never unpacked from the executable.


## 🔔 Signing

This release is **unsigned**. Windows SmartScreen may show an "Unknown
publisher" warning on first run.

SHA-256 (light build, SeaMaestro.exe): TO_BE_FILLED
SHA-256 (cut build, SeaMaestroCut.exe): TO_BE_FILLED
Size: TO_BE_FILLED MB (PE executable, 64-bit)
VirusTotal (light build): TO_BE_FILLED
VirusTotal (cut build): TO_BE_FILLED

The release contains two files: `SeaMaestro.exe` (resizer) and
`SeaMaestroCut.exe` (resizer + background cut). The cut build writes its
inference runtime into `%LOCALAPPDATA%\SeaMaestro\ort\` on first use; an
unsigned executable that drops DLLs can trip antivirus heuristics — if the cut
fails to start, check Windows Security → Protection history.

## 📄 License

MIT. See LICENSE and THIRD_PARTY_LICENSES.md.

---

## SeaMaestro v2.5.4

## ✨ What's New

**Crisper edges on large photos — the biggest change in this release.** BEN2 looks
at a 1024×1024 view of the frame, so on a 5568×4872 photo the matte used to be
upscaled 5.4× and every edge came out soft. Now, after the usual full-frame pass,
SeaMaestro finds the band where the edge actually lies and re-cuts **only those
tiles at native resolution, 1:1**, then blends them back. Thin branches, hairs and
fine detail stop turning into mush. Frames up to 1024 px are handled exactly as
before (byte-for-byte), so nothing regresses for small pictures. If a frame needs
more tiles than one pass allows, the tool says so in plain words instead of
silently mixing sharp and soft edges.

**Edge cleanup for very large frames.** The halo/edge polish used to be skipped
above 4096 px. It now runs in strips, which keeps memory around 226 MB instead of
~1 GB — and gives a byte-identical result to the old whole-frame code (there is a
unit test proving exactly that, with hard edges placed right on the strip seams).

**Cutting a resized frame no longer pays for the full-size mask.** When you ask for
`--cut` together with a smaller size (and no crop/scan/tile/merge/pdf), the frame is
now resized *first* and the model runs on the size you actually asked for — the matte
is no longer upscaled from a much larger frame, so edges come out at least as clean
and a 5568×4872 photo headed for 1600 px does a fraction of the work. The documented
stage order (cut → crop/scan → resize) still applies to every other combination.
Because the mask is now computed on the final resolution, cut results for
`--size N --cut` differ from v2.5.3 — everything else is unchanged.

**One voice, from launch to finish.** Every message you normally see — startup,
model warm-up, progress line, skipped steps, hints — now speaks in the same warm
captain's-bridge voice as the banner, in all eight languages. Errors that used to
read like a system log now say what happened, why, and what to do: *"Full ahead is
off — the graphics card drank her tanks dry (GPU: low on memory). Dropping the
telegraph to slow ahead (CPU)…"*

**A graphics-card crash now falls back too.** Besides running out of memory, some
Intel/AMD drivers fail with `887A0020` / `887A0005` mid-run. Both are recognised
now, so the job continues on the CPU instead of stopping with `MAYDAY!`.

**Print preview no longer enlarges.** `--preview` upscaled small frames to 2000 px;
it now only downscales, so the preview really is "how it will print".

## 🐛 Bug Fixes

**AVIF transparency.** Files produced by SeaMaestro always carried a correct alpha
channel — but some Windows viewers (including certain versions of *Photos* and
Explorer thumbnails) paint AVIF on a solid background, which made cutouts look as
if the background was never removed. Check such files in Chrome, Edge or Firefox.
This is now also documented in the README, together with the Intel UHD note: on
built-in Intel graphics the GPU backend can fail, and the recommended stable
setting there is `--ep cpu`.

**Settings baked into the exe name survive a command-line flag.** The name was
already honoured in drop mode, but as soon as a single `--` flag was present only a
handful of its tokens were read: `q90`, `w800`, `1920x1080`, `50pct`, `bw`,
`lossless`, `progressive`, `sharp`, `scan`, `crop`, `merge`, `exif`, `shanty`,
`tile`, `soft`, `hard`, `plain` were dropped without a word. Every preset token is
now applied in both modes, and the command line overrides only what you actually
typed: `SeaMaestro_q90.exe --nopause photo.jpg` is quality 90, while
`--quality 80` still wins. A name token the tool cannot parse is now reported
instead of vanishing. The same goes for the interface language: a name with the
language glued to the numbers, like `SeaMaestrode1920jpgq85.exe`, opens in German
even with flags present, and `--lang` keeps the last word.

## 🔧 Under the hood

- `v2.5.4` is a drop-in update — no reinstalling, and no new flags are needed for
  what used to work: `SeaMaestro.exe` still just resizes and converts,
  `SeaMaestroCut.exe` still cuts, both self-contained.
- All eight languages were brought in line, including the 23 messages that are
  seen during normal work — not only the errors.
- The cut runtime is now prepared once, in the main thread, before any parallel work
  starts, instead of being unpacked lazily from inside a worker. The unpack note can
  therefore appear a little earlier in a run that later stops for another reason; the
  runtime itself is unchanged and still self-contained.
- `--norefine` is new: it keeps the global mask and skips the edge refine pass, for
  netted, webbed or thin-strand subjects. This is the manual override for the
  all-or-nothing refine rule described above. The resizer build does not cut and does
  not show it.
- One word everywhere: `bgwhite`, `epcpu`, `threads4` are spelled the same on the
  command line and in the exe name — `--bgwhite` / `..._bgwhite.exe`, `--epcpu` /
  `..._epcpu.exe`, `--threads4` / `..._threads4.exe`. The spaced forms
  (`--bg white`, `--ep cpu`, `--threads 4`) keep working.
- The cut model is fed the ImageNet mean/std normalisation it was trained with — the
  same setting the reference frames in the release gate were produced with.

---

## SeaMaestro v2.5.3

## ✨ What's New

**The background cut build is self-contained** — the inference runtime (ONNX
Runtime + DirectML, ~38 MB) now lives inside `SeaMaestroCut.exe`. The first cut
unpacks it into `%LOCALAPPDATA%\SeaMaestro\ort\` once, verifies what was written,
and every later run reuses it; nothing is installed and no DLL sits next to the
program. The release is therefore **two files**: `SeaMaestro.exe` (resizer) and
`SeaMaestroCut.exe` (resizer + cut, ~285 MB). Advanced: `ONNXRUNTIME_DLL` points
the cut at an existing runtime, `SEAMAESTRO_RUNTIME_DIR` moves the unpack
location, `SEAMAESTRO_RUNTIME_VERIFY=1` re-checks the unpacked files.

**Background cut: cleaner edges by default (`--cut`)** — the matte keeps the raw
model alpha instead of the old hard levels curve, so soft masks (fur, thin hair,
fabric) no longer turn into speckle and the light rim around the subject is
gone. The old look is one flag away: `--hard`. The new default is also cheaper
to store: +2…5 % PNG instead of up to +445 % without the edge cleanup.

**Edge colour blend** — the de-fringe estimate is now mixed with the original
pixel colour (75 / 25) instead of replacing it. On the reference frame the
letter edges went from a dark outline to neutral: edge bias −6.65 → −0.17,
rim −40.8 → −4.2, band speckle 7433 → 4444, PNG 822 → 795 KB.

**EXIF-safe cut** — rotated photos (phone JPEGs with an Orientation tag) are
oriented before the model sees them. Measured on a rotated frame: 27.9 % of the
alpha was wrong before, 0.00 % after (3 pixels, JPEG re-encode noise).

**Faster batches** — the inference session is locked only while the model runs;
the edge cleanup now happens on the CPU while the GPU computes the next frame.
The memory budget also accounts for the cut stage, so a batch cannot
over-allocate: peak RSS on 10 × 4000×3000 photos is 5.8 GB (6.3 s per frame).

**One-word command line** — every visible flag is a single word: `--nopause`,
`--exif`, `--soft`, `--hard`, `--plain`, `--nocut`, `--threads` (total cores).
The old spellings (`--no-pause`, `--keep-exif`, `--raw-alpha`,
`--no-de-fringe`) keep working as hidden aliases, so existing scripts and batch
files are safe. A unit test keeps the rule from regressing.

**Cut help in 8 languages** — the whole CUT section (and the renamed flags) is
localized for en, ru, uk, de, es, fr, el, fil, with one line per flag.

**The executable name is a config** — `SeaMaestroCut.exe photo.jpg` cuts,
`..._png.exe` picks PNG, `..._hard.exe` the hard edge, `..._nocut.exe` resizes
only. New: the name is also honoured when command-line flags are present
(`SeaMaestroCut.exe --nopause photo.jpg` still cuts; `SeaMaestroCut_png.exe
--cut` now produces PNG instead of a JPEG error).

## 🐛 Bug Fixes

**Background colour in the exe name** — `..._bgwhite.exe`, `..._bgblack.exe`,
`..._bggrey.exe`, `..._bgred.exe` or `..._bg#RRGGBB.exe` set the colour a cut
subject is composited onto (any name accepted by `--bg`). Without it the default
is still white, with a note.

**PDF and SVG fast paths skipped the cut** — `--cut --format pdf` (and the
vector SVG path) could pass the original images through without removing the
background. Both paths now respect `--cut`.

**Colour typos failed late** — `--bg whiet` used to load the model first and
fail at the end; now it is rejected in about a second, before any work starts.
`--bg none` with a format that cannot store transparency is rejected too,
instead of silently compositing onto white.

**A crash no longer kills the batch** — if inference panics, the session is
rebuilt for the next file instead of poisoning every remaining file.

**Exe-name parsing is transactional** — a name that only partially parses no
longer applies half of its settings.

**`--merge` help** — the merge description is now translated in all 8 languages.

**The cut build speaks all 8 languages** — the background-cut console messages
(engine choice and fallbacks, cold start, tiles, runtime unpacking, de-fringe
skips, and load errors) used to be English-only; they are now translated like
the rest of the tool.

**`--size 4000` refused to enlarge** — the long-edge form (a bare number, the
one used in the exe-rename examples) silently kept the original size whenever
the target was larger than the image, while `w4000`, `h4000`, `800x600` and
`150pct` always scaled in both directions. All size forms behave the same now:
**no size given — the frame is left exactly as it is; a size given — the frame is
fitted to it, up or down** (a cropped document reaches the requested long side
too), and the `(downscale only)` note is gone from the help.

**A renamed exe could not composite a cut subject** — dropping files onto
`SeaMaestroCut_jpg.exe` (or any name with a format that cannot store
transparency) failed asking for `--bg`, which drop mode has no way to pass. It
now composites the subject onto white and says so; PNG names keep transparency.

**Long-edge size in the exe name (`l4000`) works, and unknown tokens are
reported** — `SeaMaestro_l4000.exe` used to ignore that token silently; the long
edge is now applied. Any name token the tool cannot understand produces a single
`note:` line listing it, instead of silence.

**`--output <existing folder>` is rejected up front** — the file is refused
before any decoding; it used to fail late, once per file, after the work.

**Failure reporting** — a batch with at least one failed file always exits
non-zero now (a later successful drive could previously overwrite the error
flag), and each failure is printed once instead of twice.

**Low-end hardware** — the memory budget now refuses (instead of silently
granting) a frame it cannot fit, `--scan` denoising and the SVG vector path skip
themselves with a message when they would exceed it, and a CPU-only cut prints
what to expect (`note: background cut runs on the CPU here …`) instead of looking
frozen.

**AVIF on old CPUs** — encoding needs AVX2 (Intel 2011+, AMD 2015+). The tool now
says that up front instead of failing later with a codec error.

**DirectML out of memory no longer fails the run** — on machines with little free
RAM the built-in GPU (Intel/AMD) shares memory with the system, so the GPU session
can be created and then fail during inference. SeaMaestro now detects exactly that,
drops the GPU session, rebuilds it on the CPU and finishes the job — one clear note
instead of `MAYDAY! inference failed`. With an explicit `--ep dml` you still get the
run, but the message says your choice was not honoured.

**The cut build's console is localized end to end** — the progress line
(`cutting subjects…`), the inference errors (`inference failed`,
`cannot create the inference session`, `cannot build the input tensor`, …) and the
PDF progress line are translated in all 8 languages. The technical lines printed by
ONNX Runtime itself are left as they are: they are diagnostics, not user text.

## 📦 The two files

| file | what it is | download |
|---|---|---|
| `SeaMaestro.exe` | the resizer: resize, crop, smart scan, formats, PDF and merge | ~35 MB |
| `SeaMaestroCut.exe` | the same resizer **plus AI background cut** (BEN2, DirectML GPU or CPU) | ~300 MB |

**The cut build is a background remover by default, and the file name is what
switches it on** (`cut` inside `SeaMaestroCut.exe`), so no flag is needed: drop a
photo onto it — or run `SeaMaestroCut.exe photo.jpg` — and you get a transparent
PNG. Renaming the light build to a name containing `cut` enables nothing: it only
tells you to take the cut build instead.

**Want the cut build to behave exactly like the light one?** Rename it so the name
has no `cut`/`cutout` in it (`SeaMaestroRenamed.exe`), or pass `--nocut`. Then the
pipeline, the options and the output are identical to `SeaMaestro.exe`, and the
inference runtime is never unpacked from the executable.

## 🔔 Signing

This release is **unsigned**. Windows SmartScreen may show an "Unknown
publisher" warning on first run.

SHA-256 (light build, SeaMaestro.exe): TO_BE_FILLED
SHA-256 (cut build, SeaMaestroCut.exe): TO_BE_FILLED
Size: TO_BE_FILLED MB (PE executable, 64-bit)
VirusTotal (light build): TO_BE_FILLED
VirusTotal (cut build): TO_BE_FILLED

The release contains two files: `SeaMaestro.exe` (resizer) and
`SeaMaestroCut.exe` (resizer + background cut). The cut build writes its
inference runtime into `%LOCALAPPDATA%\SeaMaestro\ort\` on first use; an
unsigned executable that drops DLLs can trip antivirus heuristics — if the cut
fails to start, check Windows Security → Protection history.

License
MIT. See LICENSE and THIRD_PARTY_LICENSES.md.

---

## SeaMaestro v2.5.2

## 🧹 Maintenance

**New application icon** — the executable ships with the updated SeaMaestro icon.

**Internal cleanup in `--crop`** — the auto-crop module went through an audit
pass: constants renamed to match their meaning, doc comments completed, a
redundant blur pass and one full-frame copy removed, diagnostic probes kept behind the debug switch.
**Crop behaviour is unchanged** — on the 49-photo regression set the output is
bit-identical to v2.5.1.

## 🔓 Signing

This release is **unsigned**. Windows SmartScreen may show an "Unknown
publisher" warning on first run.

VirusTotal — clean: 0/70 security vendors flagged the file.
SHA-256: 8435ee87dea57fd77c50a808c3ee30a362b10e3a21ba13845c90eff4926acd89
Size: 33.41 MB (PE executable, 64-bit)

License
MIT. See LICENSE and THIRD_PARTY_LICENSES.md.

---

## SeaMaestro v2.5.1

## ✨ What's New

**Auto-crop & deskew (`--crop`)** — finds the document in a photo, straightens
the perspective and cuts the background away. Combines with `--scan`, `--size`
and `pdf`/`merge`; also available as a rename keyword (`SeaMaestro_crop.exe`).

## 🐛 Bug Fixes

**The whole page, not a fragment** — auto-crop used to lock onto a strong inner
rectangle (a table, a form, a bright patch inside the sheet) and cut the rest
of the page away. It now prefers the sheet itself, so footers, margins and
headings survive.

**No crop when in doubt** — if the winning quad turns out to be a small island
of paper surrounded by the same paper (a fragment inside the sheet), the frame
is kept as shot instead of returning a partial crop. A little extra background
beats a lost corner of a document.

**Degenerate geometry fallback** — if the detected quad is not a proper
quadrilateral (duplicate vertex, self-intersection) or the perspective warp
leaves too much residual, the original photo is used instead of a skewed
result.

**Smart Scan white point** — the adaptive white point was raised
(0.85 → 0.95), so that background grain no longer survives as grey patches.

## 📷 Better auto-crop results

Shoot documents on a **contrasting background** — a dark desk, a coloured
folder or a dark sheet under white paper. The detector needs the sheet edge to
stand out; white paper on a white table can be undetectable, and in that case
`--crop` deliberately leaves the photo untouched rather than guessing.
Orientation is kept as shot — no forced rotation.

## 🔓 Signing

This release is **unsigned**. Windows SmartScreen may show an "Unknown
publisher" warning on first run.

VirusTotal — clean: 0/70 security vendors flagged the file.
SHA-256: 73a62c7a437d6fe0a087396332512cbafe61d4a1319395a8a438e6ca206716b5
Size: 33.38 MB (PE executable, 64-bit)

License
MIT. See LICENSE and THIRD_PARTY_LICENSES.md.

---

## SeaMaestro v2.5.0

## ✨ What's New

**Full branding in `--help`** — the about section now shows the complete
SeaMaestro identity: banner title, tagline, author, version, contact email and
repository URL.

**`--version` flag removed** — the redundant `-V`/`--version` flag is gone;
the version is now shown directly in the about section.

**Smart Scan (`--scan`)** — a filter for document photos: flattens uneven
lighting and shadows into a crisp white background, keeps text black and
preserves colored stamps/signatures. Combines with `pdf`/`merge`.

## 🔓 Signing

This release is **unsigned**. Windows SmartScreen may show an "Unknown
publisher" warning on first run.

VirusTotal — clean: 0/71 security vendors flagged the file.
SHA-256: d5bffd1d358bb57a684a42cb3326be108bb4ce01401cb4852ac604c67abada26
Size: 33.24 MB (PE executable, 64-bit)

License
MIT. See LICENSE and THIRD_PARTY_LICENSES.md.

---

## SeaMaestro v2.4.12

## ✨ What's New

**Renamed to `SeaMaestro`** — the executable is now `SeaMaestro.exe`
(was `SeaMaestroResize.exe`). Rename-based settings still work, e.g.
`SeaMaestro_q80_w800_webp.exe`.

**HEIC/HEIF EXIF** — `--keep-exif` now preserves EXIF from HEIC/HEIF photos
(e.g. iPhone → JPEG/WebP keeps GPS, date and camera info).

**TIFF compression** — TIFF output is now Deflate-compressed (lossless,
~5–10× smaller files).

**`--output` for batch** — `--output <dir>` now works as an output directory
for batch processing.

**Elapsed time** — the "Voyage complete" summary now shows the run duration
(e.g. `… processed in 12.3s.`).

**Full format list in `--help`** — `--help` now lists every supported input
format, matching the in-app help table.

## 🔓 Signing

This release is **unsigned**. Windows SmartScreen may show an "Unknown
publisher" warning on first run.

VirusTotal — clean: 0/65 security vendors flagged the file.
SHA-256: ac1774464d232203f802e733392da5675fb8e59d938225fa311e412e8c78b0d8
Size: 33.20 MB (PE executable, 64-bit)

License
MIT. See LICENSE and THIRD_PARTY_LICENSES.md.

---

## SeaMaestroResize v2.4.11

## 🎨 New Icon

**New app icon** — refreshed multi-resolution icon (16–256 px) for the
executable and Windows shell.

## 🔓 Signing

This release is **unsigned**. Windows SmartScreen may show an "Unknown
publisher" warning on first run.

VirusTotal — clean: 0/70 security vendors flagged the file.

SHA-256: `1398803d5ab82ebac61b2ce862a5c16a8108ddba903350cc64c2da7f947d18be`
Size: 33.19 MB (PE executable, 64-bit)

License
MIT. See LICENSE and THIRD_PARTY_LICENSES.md.

---

## SeaMaestroResize v2.4.10

## 🐛 Bug Fixes

**Merge output folder** — `--merge` with multiple input folders now places the
merged PDFs next to each source folder (each root is processed independently),
instead of jumping to the parent of the common root.

**Help text** — the help table now correctly shows JPG (not WebP) as the
default output format.

## 🔓 Signing

This release is **unsigned**. Windows SmartScreen may show an "Unknown
publisher" warning on first run.

**VirusTotal** — clean: 0/70 security vendors flagged the file.

- SHA-256: `a8c4540fd9dbf782f07e48731de8f8d07a16fd800dd969253eb0f0fe30185d48`
- Size: 33.19 MB (PE executable, 64-bit)

---

## SeaMaestroResize v2.4.9

## 🎉 New Formats

**JXL (JPEG XL) encode & decode** — full JPEG XL support via the libjxl FFI:
lossy (`--quality`) and lossless (`--lossless`) encoding, plus decode with ICC
and EXIF passthrough.

## ⚡ Smaller Binary

**Removed `aom` from libheif** — AVIF/HEIF now encode through SVT-AV1 only.
The static binary dropped from ~42 MB to ~33 MB.

## 🐛 Bug Fixes

**JXL lossless** — the encoder now sets `uses_original_profile` (required by
libjxl for lossless); previously `--format jxl --lossless` failed.

**PNG EXIF loss** — `--keep-exif` no longer silently drops EXIF: the `eXIf`
chunk is embedded after oxipng optimization so it survives compression.

## ✨ Improvements

**AVIF EXIF & ICC** — AVIF now passes EXIF and ICC through on encode and
extracts EXIF on decode (previously dropped on both ends).

**UI sync for AVIF EXIF** — the banner shows `EXIF: on` for AVIF, and the
`--keep-exif` help lists AVIF in all 8 languages.

## 🔓 Signing

This release is **unsigned**. Windows SmartScreen may show an "Unknown
publisher" warning on first run.

**VirusTotal** — clean: 0/71 security vendors flagged the file.

- SHA-256: `451f2515cb77b65d366d0f6b28c50ef19ccfba9fcc21456cd05f3ef1287585b2`
- Size: 33.17 MB (PE executable, 64-bit)

---

## SeaMaestroResize v2.4.8

## ⚡ Performance

**Fast DCT decode for cover-crop** — the scaled-IDCT fast path now also runs
for `WxH` cover-crop sizes. The cover bounding box is computed up front, so a
heavy JPEG is decoded at the exact downscaled resolution and cropped from that
buffer — no full-res decode just to crop.

**HEIF fast routing** — HEIC/HEIF inputs are routed straight to libheif,
skipping the generic decoder's format-guess pass (which can't read HEIF
anyway), so iPhone photos decode with one wasted step removed.

**zlib-ng compression** — the compression backend moved from miniz_oxide to
zlib-ng (via flate2): faster, SIMD-accelerated zlib, linked with the static
CRT so the executable stays fully self-contained.

**Zero-loss JPEG→PDF** — baseline JPEGs written to PDF keep their original DCT
coefficients (DCTDecode) instead of being re-encoded, so `--format pdf
--lossless` is lossless for JPEG sources.

## 🐛 Bug Fixes

**Crop shortcut** — fixed a latent bug where cover-crop at exact scales (0.5,
0.25…) could return the uncropped intermediate instead of the final `WxH` box.

**mozjpeg panics** — libjpeg error-exit panics are now silenced (no console
trace).

## ✨ Improvements

**`--merge` output folder** — merged PDFs now land in a `SeaMaestro_Merged`
folder (previously named after the common parent); the help text now reads
"one PDF per folder, preserving directory structure" in all 8 languages.

**Cleaner code** — mozjpeg encode paths deduplicated into one helper; PNG EXIF
embedding computes its CRC incrementally (no temporary buffer).

## 🔓 Signing

This release is **unsigned**. Windows SmartScreen may show an "Unknown
publisher" warning on first run.

**VirusTotal** — clean: 0/71 security vendors flagged the file.

- SHA-256: `da0012886948435e3fa5c5d83b0e2f40af6e56565b105a8aab993e162240847a`
- Size: 35.91 MB (PE executable, 64-bit)

---

## SeaMaestroResize v2.4.7

## ✨ Improvements

**`--merge` Path Compression** — merging a folder now writes one PDF per
folder, rebuilding the source tree instead of one flat file. A shared prefix
is dropped, branches with several children stay as real subfolders, and
single-child paths fold into the PDF name (`Trip`/`Day1` →
`Trip_Day1_q85.pdf`). Loose files at the root land at the output root.

**Disk & network pooling** — inputs are split into independent pools by
drive/network prefix before processing. Each pool writes next to its own
source (`D:\`, `E:\`, `\\server\share` each stay on their own disk), and
removable USB drives are never written back — their output lands next to the
program instead.

**Long paths & name cap** — output paths beyond 260 characters now work on
Windows via the `\\?\` verbatim prefix, so deep source folders no longer fail
with "path too long". Over-long merged PDF names are capped to ~120 characters
with a `first_..._last_<hash>` pattern, keeping them readable and under the
NTFS 255-character file-name limit.

**Safety & UX** — oversized input files are rejected before loading into
memory (no more OOM on a stray multi-GB file), and errors print instantly
during processing instead of only at the end.

## 🔓 Signing

This release is **unsigned**. Windows SmartScreen may show an "Unknown
publisher" warning on first run.

**VirusTotal** — clean: 0/70 security vendors flagged the file.

- SHA-256: `98a2628aea155862ced52ccd9d679868e8d4edcfac2434c4507ffa2fe8935c4a`
- Size: 35.56 MB (PE executable, 64-bit)

---

## SeaMaestroResize v2.4.6

## ⚡ Performance

**JPEG scaled-IDCT fast-path** — JPEG→JPEG downscaling now decodes through
libjpeg's scaled IDCT (1/2, 1/4, 1/8) instead of decoding the full image and
then resizing it. Arbitrary scales use the nearest N/8 step plus a final
Lanczos pass. This removes the full-decode + resize overhead from JPEG
downscales.

## 🐛 Bug Fixes

**Double downscale** — `--size 50pct` (and other JPEG downscales) no longer
applied the scale twice, producing a quarter instead of a half. The fast-path
now resizes to the exact target dimensions.

**Scanline panic** — fixed a panic in the JPEG fast-path when
`jpeg_read_scanlines` returned fewer rows than expected (libjpeg reads in MCU
blocks). Rows are now read in a loop until the full output height is reached.

**`50pct` parsing** — fixed the percent-regex alternation order so `50pct`,
`50p` and `50%` all parse correctly; previously `50pct` was rejected as an
invalid size.

**EXIF orientation** — the JPEG fast-path now accounts for EXIF orientation
5–8 by computing target dimensions from the logical (rotated) size, so
portrait phone photos stay portrait.

## ✨ Improvements

**ICC preserved** — the JPEG fast-path now reads and re-embeds the ICC color
profile (`jpeg_save_markers` + `jpeg_read_icc_profile`).

**Silent fallback** — a corrupt or unsupported JPEG now falls back to the
normal decode path without printing a panic trace to the console.

**Banner** — the header now shows the version, the support email and the
GitHub link (🖂 and ⎇ markers) across all 8 languages. Nautical remark icons
are now consistent with the other languages (📦 / 💦).

## 🔓 Signing

This release is **unsigned**. Windows SmartScreen may show an "Unknown
publisher" warning on first run.

**VirusTotal** — clean: 0/71 security vendors flagged the file.

- SHA-256: `1c77c748b42fca11d27297a228a61044cb5f1120e32226c8af7076725d8f439f`
- Size: 35.50 MB (PE executable, 64-bit)

---

## SeaMaestroResize v2.4.5

## ⚡ Performance

**JXL single parse** — JPEG XL files are now parsed once for dimensions,
EXIF and decoding, instead of multiple full parses per file. This removes
redundant work from the JXL pipeline.

## 🐛 Bug Fixes

**EXE rename casing** — rename mode now matches the exact `SeaMaestroResize`
casing when detecting baked-in settings.

## 📝 Documentation

**Format list** — PDF is no longer listed as a "convert between" format;
PDF is output-only.

## 🔓 Signing

This release is **unsigned**. Windows SmartScreen may show an "Unknown
publisher" warning on first run.

**VirusTotal** — clean: 0/70 security vendors flagged the file.

- SHA-256: `63a8e2226869668be5677787b70e13399f2cd2ac7f2afd5efb1dc01ac828a543`
- Size: 35.36 MB (PE executable, 64-bit)

---

## SeaMaestroResize v2.4.4

## 🔱 Rebranding & UI

**New Name** — The project has been officially renamed from
**SeaMonkeyResize** to **SeaMaestroResize**, reflecting its role as a
multi-format image orchestrator. The repository now lives at
`github.com/SeaMaestro/SeaMaestroResize`.

**Nautical Aesthetic** — The console output is cleared of the old
monkey/banana mascot. It now uses the trident (🔱) and anchor (⚓), and the
old "banana break" jokes are replaced with nautical "shore leave" and
"dropping anchor" messages across all 8 supported languages.

**Clean Artifacts** — The GitHub Release asset now downloads as a clean
`SeaMaestroResize.exe` (the versioned `SeaMaestroResize_v2.4.4.exe` remains
the display label), making it easier to grab and immediately rename with your
desired configuration.

## ⚙️ Core & Build System

**CI Stabilization (GitHub Actions)** — The Windows-2022 build pipeline has
been overhauled; the heavy C-library compilation crashes are resolved.

**AVIF & AV1 Support** — Added NASM to the runner, fixing rav1e (AVIF
encoder) assembly compilation. Restored dav1d decoder linking via `pkgconf`
in `vcpkg.json`. AVIF reading and writing are now fully operational in
release builds.

**libheif-sys Fix** — Resolved the critical build bug (`os error 3`) caused
by vcpkg-rs failing to locate the package tree. The pipeline now installs to
the default vcpkg root for reliable dependency resolution.

**Size Optimization** — Changed the Rust compiler opt-level to `"s"`, further
shrinking the static `.exe`. The native codec stack (rav1e/dav1d/libheif/aom)
keeps its C/asm SIMD throughput, and the resize path runs on
`fast_image_resize`'s SIMD-optimized Rust kernels, so real-world performance
is effectively unchanged.

**Smoke Test** — Added a post-build smoke test to the release pipeline.

## 🐛 Bug Fixes

**EXE Rename Language Suffix** — Fixed parsing of multi-token executable
names that carry a language code (e.g. `SeaMaestroResize1920jpgq85_DE.exe`).
The language and all glued settings now apply correctly instead of being
skipped.

## 📝 Documentation & Licenses

**Build Docs** — The README now lists explicit source-build requirements:
Rust MSVC toolchain, vcpkg manifest dependencies, and NASM, plus the
static-CRT setup.

**Upscale** — The README documents the Lanczos3 filter used for crisp raster
enlargements.

**SVG & PDF** — The README now details the vector rendering pipeline for
SVG/SVGZ: native PDF primitives (paths, gradients, text) keep output sharp at
any zoom, with automatic raster fallback for unsupported features.

**License Compliance** — `THIRD_PARTY_LICENSES.md` now documents `libde265`
(LGPL-3.0), `aom` (BSD-2-Clause), and `resvg`/`usvg` (MPL-2.0).

Code signing policy: https://github.com/SeaMaestro/SeaMaestroResize#code-signing-policy

---

Captain's log: The ship is fully rigged, the hold is secure, and the Kraken
sleeps. 🌊

## v2.4.3

### Fixes
- Dual-tier memory budgeting: separate memory budgeting for input decoding and output encoding (max + raw.len()).
- JXL: out_tier (16,512) — the encoder reserves ~2 GB/file, eliminating 16 GB of swap.
- AVIF: out_tier (4,128) — removed the inflated ~2 GB/file allocation; in batches, it's now limited by the CPU rather than a false memory limit.
- JXL input: in_tier (8,256) — unchanged.
- Dead probe_image removed, probe_dims/is_raw_bytes available for compute_need.
- Regression: JPG->JXL and JPG->AVIF on 16 GB without swap; merge JXL->PDF without deadlock.

## v2.4.2

### Fixes
- Fixed a hang in SVG->PDF merge: the memory permit was stored inside each page result and released only after the whole merge finished, causing workers to block on budget acquisition.
- Moved the batch file queue off rayon's pool to a native OS thread queue, fixing batch deadlocks under memory pressure.
- Fixed result-collection types in merge and batch paths after the queue migration.
- Added raw JXL codestream (FF 0A) detection for dimensions and EXIF extraction.
- AVIF encoding concurrency now respects the memory budget instead of a hardcoded worker cap.

### Deferred
- Triple JXL metadata parse optimization.
- PDF ZLIB compression transient buffer optimization.

## v2.4.1

### AVIF decoding fixed

- AVIF input now decodes via the native dav1d/mp4parse decoder. Previously a
  global libheif image hook hijacked `ftypavif` and failed with
  "No decoding plugin installed". The hook is removed; HEIC/HEIF still
  decodes through the libheif fallback.

### AVIF encoding

- Parallelized across files: thread pool capped at 8, scaled by batch size.

### Docs & licenses

- THIRD_PARTY_LICENSES: added dav1d, mp4parse, ravif, avif-serialize.

## v2.4.0

### Vector PDF engine (SVG → PDF)

- **Vector text** — glyphs are now vector instead of rasterized: outlines, plus
  native selectable text via TrueType (Type0/CIDFontType2 + ToUnicode +
  FontFile2) and OTF/CFF (CIDFontType0), with per-document font subsetting.
  Solid fill/stroke text stays vector; gradient/pattern/decorations/CFF/color
  fonts fall back to curves.
- **Embedded raster images** in vector PDF, preserving JPEG ICC profiles;
  PNG iCCP/gAMA and WebP ICCP passthrough; sRGB color management for text.
- **Vector clip-path** support.
- **FlateDecode-compressed** vector/raster content streams → smaller PDFs.
- **Grayscale vector output** — `--bw` stays fully vector (BT.709 luma,
  DeviceGray solids/strokes, Flate gray images).
- **OOM protection** and hostile-SVG depth limits: pre-flight peak estimate,
  XML depth pre-scan (`MAX_SVG_DEPTH=32`), SVGZ decompressed before scanning.
- Fixes: XObject resources, dropped illegal FontMatrix, text placement /
  resource / float-formatting fixes, absolute transforms applied to native
  glyphs.

### New input formats

- TGA, PNM (PBM/PGM/PPM/PAM), DDS, HDR, EXR, FF (farbfeld).

### Performance

- mimalloc global allocator — SVG→PDF benchmark ~0.341s → ~0.235s.

### Fixes & UI

- Honest banners: `--lossless` reported only for WebP/JXL/PDF (AVIF and JPEG
  no longer falsely show "Lossless: on"); EXIF banner only for JPEG/PNG/WebP/
  JXL; progressive banner only for JPEG.
- Help table now wraps long lines instead of truncating them (all 8
  languages).

### Docs & infra

- README: supported formats and SVG & PDF behavior notes.
- THIRD_PARTY_LICENSES: added mimalloc.
- .gitattributes for cross-platform EOL normalization.

## v2.3.0

- SVG/SVGZ input (raster via resvg; gradients, filters, masks, clip paths,
  patterns, embedded images and text are supported).
- SVG → PDF vector output for flat graphics (solid fill/stroke, transforms,
  opacity, dash), for both single-file `--format pdf` and `--merge`.
  Any unsupported SVG feature falls back to raster per page.
- `--bw`, `--sharpen` and cover-crop (`WxH`) force raster for SVG; fixed crop
  producing a wrong page size in vector mode.
- Hardening: per-file panic isolation, SVG parse guards, unsharp hoist,
  memory/IO guards, atomic writes.
- libheif static linking; CMYK JPEG handling; reduced temp-folder telemetry noise.

## v2.2.1

⚓ Multiformat batch image resizer and converter for Windows.
A single static executable — no installer, no external DLLs, no MSVC runtime.
Download, run, done.

## What's new in 2.2

- **PDF output** — `--format pdf` writes one PDF per input; `--merge` combines
  all inputs into a single PDF sorted by path.
- **Streaming merge** (2.2.1) — the merged PDF is written to a temp file as a
  stream and renamed at the end, so merging thousands of files stays at
  constant memory instead of holding the whole PDF in RAM.
- **Chunked parallel merge** (2.2.1) — pages are decoded, resized and encoded
  on all cores in memory-budgeted chunks, then written in sorted order.

## The idea

Copy `SeaMonkeyResize.exe` anywhere, rename it to bake in settings, drop photos
on it. Done.

- One photo → resized next to the original.
- Several photos → a `SeaMonkeyResized` folder next to them.
- A folder with subfolders → structure preserved under `SeaMonkeyResized`.
- `--merge` → one `SeaMonkeyMerged{settings}.pdf` next to the source folder.

## Highlights

- **Full color fidelity** — ICC color profiles are preserved for JPEG, PNG,
  JXL, WebP and TIFF, so converted images keep their original colors instead
  of being flattened to sRGB.
- **EXIF passthrough** (`--keep-exif`) — keeps metadata for JPEG, PNG, WebP
  and JXL, normalizes Orientation to 1 and updates pixel dimensions to the
  resized size. EXIF is cleared by default for privacy.
- **Auto-rotation** — honors EXIF Orientation, so portrait photos come out
  upright without manual steps.
- **Parallel batch processing** — multicore via Rayon, recursive folder scan,
  with a RAM guard so huge RAW files never cause an out-of-memory crash.
- **PDF generation** — single-file PDFs and merged multi-page PDFs.
  `--quality` controls lossy JPEG (4:2:0) compression, `--lossless` uses
  FlateDecode, `--bw` makes grayscale pages, and transparency is flattened
  to white.
- **Drag-and-drop** — rename the exe to bake in settings, then drop photos
  onto it.
- **Smart output layout** — one photo lands next to the original; several
  photos go into a `SeaMonkeyResized` folder; a folder with subfolders keeps
  its tree under `SeaMonkeyResized`.
- **Scriptable** — stdin/stdout pipe mode:
  `cat photo.jpg | SeaMonkeyResize --format webp > out.webp`.
- **8 languages** — English (`en`), Русский (`ru`), Українська (`uk`),
  Deutsch (`de`), Español (`es`), Français (`fr`), Ελληνικά (`el`),
  Filipino (`fil`).
- **Maritime charm** — progress bars, sea shanties, and a captain's log.

## Formats

**Input**

JPEG, PNG, WebP, AVIF, JXL, ICO, TIFF, QOI, BMP, GIF, HEIC/HEIF, and RAW
(CR2, CR3, NEF, NRW, ARW, SRF, SR2, DNG, RAF, ORF, PEF, RW2, MRW, MEF, ERF,
KDC, DCS, DCR, SRW, IIQ, 3FR, MOS, X3F, ARI).

**Output**

WebP, JPEG, AVIF, JXL, PNG, ICO, TIFF, QOI, BMP, GIF, PDF.

## Resize & adjust

- `--size 800` (long edge), `w800`, `h600`, `800x600` (cover crop), `50pct`
- `--quality 1..100` (default 85), lossless WebP/JXL/PDF, progressive JPEG
- `--bw` grayscale, `--sharpen` after resize (sigma 1.0)

## PDF & merge

```text
SeaMonkeyResize --format pdf photo.jpg             → photo_q85.pdf
SeaMonkeyResize --format pdf --lossless photo.jpg  → photo.pdf (FlateDecode)
SeaMonkeyResize --merge vacation_folder            → SeaMonkeyMerged_q85.pdf
SeaMonkeyResize --merge --lossless --bw folder     → SeaMonkeyMerged_bw.pdf
Single PDF keeps the normal output name, e.g. photo_q85.pdf.
--merge forces PDF output, sorts inputs by path, and writes pages in sorted order.
Merge writes to a temp file first and renames it at the end, so an interrupted run leaves no half-written PDF behind.
Usage examples
Command line
text
SeaMonkeyResize --size 800 --format webp --quality 80 photo.jpg
SeaMonkeyResize --size 1024x768 --format jpeg --progressive *.jpg
SeaMonkeyResize --size 50pct --format avif photo.heic
SeaMonkeyResize --size 300 --format png --bw --output result.png photo.jpg
SeaMonkeyResize --format pdf photo.jpg
SeaMonkeyResize --merge vacation_folder
cat photo.jpg | SeaMonkeyResize --format webp > out.webp
Drag-and-drop (rename the exe, then drop photos onto it)
text
SeaMonkeyResize_q80_w800_webp.exe        → quality 80, 800px wide, WebP
SeaMonkeyResize_w300_h300_png_bw.exe     → 300×300 cover crop, PNG, grayscale
SeaMonkeyResize_w800_sharp.exe           → 800px wide, sharpen after resize
SeaMonkeyResize_w800_exif.exe            → 800px wide, keep EXIF
SeaMonkeyResize_q80_w800_webp_exif.exe   → quality 80, 800px wide, WebP, keep EXIF
SeaMonkeyResize_merge.exe                → merge dropped files/folder into one PDF
SeaMonkeyResize_ua.exe                   → Ukrainian interface (all defaults)
SeaMonkeyResize_q80_w800_jpeg_ru.exe     → quality 80, 800px wide, JPEG, Russian
SeaMonkeyResize_w800_sharp_de.exe        → 800px wide, sharpen, German
Language suffixes: _en _ru _uk _ua _de _es _fr _el _fil — or full words
like english, russian, ukrainian, german.

Build
Static CRT, LTO, stripped — single ∼27 MB executable.

Version
2.2.1

License
MIT. See LICENSE and THIRD_PARTY_LICENSES.md.