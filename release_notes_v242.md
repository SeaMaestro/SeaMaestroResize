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