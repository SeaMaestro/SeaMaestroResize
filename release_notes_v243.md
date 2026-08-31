## v2.4.3

### Fixes
- Dual-tier memory budgeting: separate memory budgeting for input decoding and output encoding (max + raw.len()).
- JXL: out_tier (16,512) — the encoder reserves ~2 GB/file, eliminating 16 GB of swap.
- AVIF: out_tier (4,128) — removed the inflated ~2 GB/file allocation; in batches, it's now limited by the CPU rather than a false memory limit.
- JXL input: in_tier (8,256) — unchanged.
- Dead probe_image removed, probe_dims/is_raw_bytes available for compute_need.
- Regression: JPG->JXL and JPG->AVIF on 16 GB without swap; merge JXL->PDF without deadlock.