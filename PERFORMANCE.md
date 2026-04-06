# Performance

Benchmarks run on a 10-second, 60 fps H.264 source (600 frames) at three
resolutions. The **full** scenario is the worst case: shadow + 4-point zoom +
motion blur all enabled simultaneously.

## Export speed vs. real-time

| Scenario | Export time | Ratio |
|---|---|---|
| 360p — full pipeline | 2.9 s | **0.29×** |
| 720p — full pipeline | 9.8 s | **0.98×** |
| 1080p — full pipeline | 21.8 s | **2.18×** |

A ratio below **1.0×** means the export finishes faster than the source plays.
720p exports at essentially real-time; 360p at more than 3× real-time.

## Platform

All numbers from a 2-core Linux (ubuntu-22.04) CI runner — a deliberately
conservative environment. On a modern desktop (8+ cores) the pipeline and
Rayon parallel stages scale further; on Apple Silicon or with an NVENC/VAAPI
GPU encoder, 1080p crosses below 1× as well.

## How we got here

Starting from the initial release, a series of targeted optimisations brought
the 1080p full-pipeline time from **50.8 s down to 21.8 s** (+133%):

| Step | Change | 1080p-full |
|---|---|---|
| Baseline | — | 50.8 s |
| Eliminate per-frame allocations | pre-alloc RGBA frame, cache zoom scaler | 26.9 s |
| Parallel motion blur | Rayon row-parallel blur loop | 24.3 s |
| 3-stage pipeline + Rayon YUV | decode / composite / encode overlap; Rust BT.601 converter replaces sws_scale | 24.6 s |
| Decoder threading + SIMD unlock | H.264 frame-parallel decode; removed `--enable-small` flag to restore AVX2/SSE4 paths in libswscale | **21.8 s** |

The pipeline step shows modest gains on the 2-core CI runner (encode already
saturates both cores) but delivers larger improvements on machines with 4+
cores where stages run truly in parallel.
