# Kineto

Cinematic video composition from the command line. Takes a screen recording and produces a polished video with a colored background, padding, rounded corners, drop shadow, and smooth auto-zoom — all in a single binary with no runtime dependencies.

Built on statically linked FFmpeg (libav*). Decodes H.264/VP8/HEVC, composites in RGBA, encodes to H.264 or HEVC via libx264/libx265. Runs on macOS, Linux, and Windows — no runtime dependencies on any platform.



https://github.com/user-attachments/assets/d7c97940-3e4b-4528-b356-e7d09cd3964b




## Install

Download from [Releases](https://github.com/onevarez/kineto-engine/releases):

```bash
# macOS (Apple Silicon)
curl -sL https://github.com/onevarez/kineto-engine/releases/latest/download/kineto-darwin-arm64.tar.gz | tar -xz

# macOS (Intel)
curl -sL https://github.com/onevarez/kineto-engine/releases/latest/download/kineto-darwin-x64.tar.gz | tar -xz

# Linux (x64)
curl -sL https://github.com/onevarez/kineto-engine/releases/latest/download/kineto-linux-x64.tar.gz | tar -xz
```

```powershell
# Windows (x64) — PowerShell
Invoke-WebRequest -Uri https://github.com/onevarez/kineto-engine/releases/latest/download/kineto-windows-x64.zip -OutFile kineto-windows-x64.zip
Expand-Archive kineto-windows-x64.zip -DestinationPath .
```

Or build from source:

```bash
# Requires static FFmpeg libraries — set FFMPEG_DIR to your install prefix
FFMPEG_DIR=/path/to/ffmpeg cargo build --release
```

## Usage

```bash
kineto export -i recording.mp4 -o cinematic.mp4 \
  --bg-color "#0f0f23" \
  --padding 0.06 \
  --corner-radius 16 \
  --shadow \
  --codec h264 \
  --quality high
```

## Examples

### Basic composition

Dark background, rounded corners, drop shadow:

```bash
kineto export -i screen.mp4 -o output.mp4 \
  --bg-color "#1a1a2e" --padding 0.08 --corner-radius 12 --shadow
```

### With auto-zoom

Pass a JSON file with zoom segments for smooth zoom-in/zoom-out on areas of interest:

```bash
kineto export -i screen.mp4 -o output.mp4 \
  --bg-color "#0f0f23" --padding 0.06 --corner-radius 16 --shadow \
  --zoom-segments-file zoom.json --zoom-easing smooth
```

### Tight framing for social media

Minimal padding, large corners:

```bash
kineto export -i demo.mp4 -o social.mp4 \
  --bg-color "#000000" --padding 0.02 --corner-radius 24 --shadow \
  --codec h264 --quality high
```

### Light background for documentation

```bash
kineto export -i tutorial.mp4 -o docs.mp4 \
  --bg-color "#f5f5f5" --padding 0.10 --corner-radius 8
```

### Low quality for quick previews

```bash
kineto export -i raw.mp4 -o preview.mp4 \
  --bg-color "#0f0f23" --padding 0.06 --corner-radius 16 \
  --codec h264 --quality low
```

## Options

| Option | Default | Description |
|---|---|---|
| `-i, --input` | required | Input video file (H.264, VP8/WebM) |
| `-o, --output` | required | Output MP4 path |
| `--bg-type` | `color` | Background type (`none`, `color`) |
| `--bg-color` | `#000000` | Background hex color |
| `--padding` | `0.08` | Padding as fraction of canvas (0.0–0.4) |
| `--corner-radius` | `12` | Corner radius in pixels |
| `--shadow` | off | Enable drop shadow |
| `--codec` | `hevc` | Video codec (`h264`, `hevc`) |
| `--quality` | `high` | Encoding quality (`low`, `medium`, `high`) |
| `--zoom-segments-file` | none | Path to zoom segments JSON |
| `--zoom-easing` | `smooth` | Easing type (`smooth`, `snappy`, `cinematic`, `spring`) |

## Zoom Segments Format

```json
[
  {
    "start_time": 2.5,
    "end_time": 5.0,
    "zoom_level": 2.0,
    "center_x": 0.3,
    "center_y": 0.6,
    "easing": "smooth",
    "hold_duration_secs": 1.5
  }
]
```

Each segment defines a time range where the camera zooms into a region. `center_x` and `center_y` are normalized coordinates (0.0–1.0) relative to the video frame. The zoom transitions in over 0.35s, holds, then transitions out over 0.35s.

**Easing types:**

| Type | Feel | Math |
|---|---|---|
| `smooth` | Professional, calm | Cubic hermite: `t²(3-2t)` |
| `snappy` | Fast settle, energetic | Ease-out cubic: `1-(1-t)³` |
| `cinematic` | Dramatic, slow edges | Ease-in-out quintic |
| `spring` | Playful, bouncy | Damped spring with ~5% overshoot |

## How It Works

Kineto processes each frame through a Rust-side composition pipeline:

1. **Decode** input via libavcodec (H.264, VP8)
2. **Scale** to the padded video dimensions (lanczos)
3. **Composite** in RGBA: background → shadow → video overlay → corner mask (anti-aliased)
4. **Zoom** (if segments provided): per-frame crop + scale with easing
5. **Convert** RGBA → YUV420P
6. **Encode** via libx264 with CRF quality control

The shadow is rendered at 1/4 resolution, blurred, and upscaled — fast even for 4K. Corner masks use 1.5px anti-aliasing at arc boundaries for smooth edges.

## Platforms

| Platform | Binary | Status |
|---|---|---|
| macOS arm64 | `kineto-darwin-arm64.tar.gz` | Supported |
| macOS x64 | `kineto-darwin-x64.tar.gz` | Supported |
| Linux x64 | `kineto-linux-x64.tar.gz` | Supported |
| Windows x64 | `kineto-windows-x64.zip` | Supported |

## License

GPL-2.0-or-later (x264/x265 static linkage)
