mod assets;
mod compose;
mod zoom;

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process;

#[derive(Parser)]
#[command(name = "kineto", about = "Cross-platform cinematic video composition")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Compose a cinematic video with background, corners, shadow, and zoom
    Export(ExportArgs),
}

#[derive(Parser, Debug)]
pub struct ExportArgs {
    /// Input video file path
    #[arg(short, long)]
    pub input: String,

    /// Output video file path
    #[arg(short, long)]
    pub output: String,

    /// Video codec: h264, hevc, prores422
    #[arg(long, default_value = "hevc")]
    pub codec: String,

    /// Quality: low, medium, high
    #[arg(long, default_value = "high")]
    pub quality: String,

    /// Output width (omit to use input width)
    #[arg(long)]
    pub width: Option<u32>,

    /// Output height (omit to use input height)
    #[arg(long)]
    pub height: Option<u32>,

    // ── Background options ──

    /// Background type: none, color, gradient, image
    #[arg(long)]
    pub bg_type: Option<String>,

    /// Background solid color hex (e.g. "#1a1a2e")
    #[arg(long)]
    pub bg_color: Option<String>,

    /// Background gradient start color hex
    #[arg(long)]
    pub bg_gradient_start: Option<String>,

    /// Background gradient end color hex
    #[arg(long)]
    pub bg_gradient_end: Option<String>,

    /// Background gradient angle in degrees (0 = top-to-bottom)
    #[arg(long)]
    pub bg_gradient_angle: Option<f64>,

    /// Path to background image file
    #[arg(long)]
    pub bg_image: Option<String>,

    /// Image fit mode: cover, contain
    #[arg(long, default_value = "cover")]
    pub bg_image_fit: String,

    /// Padding as fraction of canvas (0.0-0.4)
    #[arg(long, default_value = "0.08")]
    pub padding: f64,

    /// Corner radius in pixels (0 = sharp corners)
    #[arg(long, default_value = "12")]
    pub corner_radius: u32,

    /// Enable drop shadow behind the video
    #[arg(long)]
    pub shadow: bool,

    // ── Webcam options (accepted, not implemented) ──

    /// Path to webcam recording file
    #[arg(long)]
    pub webcam: Option<String>,

    /// Webcam position and size as x,y,width,height
    #[arg(long)]
    pub webcam_pos: Option<String>,

    /// Webcam shape: circle, rounded_rect, rectangle
    #[arg(long, default_value = "circle")]
    pub webcam_shape: String,

    /// Webcam corner radius for rounded_rect shape
    #[arg(long, default_value = "16")]
    pub webcam_corner_radius: f64,

    // ── Motion effects ──

    /// Motion blur intensity (0.0 = off, up to 1.0)
    #[arg(long, default_value = "0.0")]
    pub motion_blur: f64,

    /// Path to JSON file containing zoom segments array
    #[arg(long)]
    pub zoom_segments_file: Option<String>,

    /// Zoom easing type: smooth, snappy, cinematic, spring
    #[arg(long, default_value = "smooth")]
    pub zoom_easing: String,

    // ── Audio mixing (accepted, not implemented) ──

    /// Additional audio track as "path:volume". Repeatable.
    #[arg(long)]
    pub audio_track: Vec<String>,

    /// Volume for the main video's embedded audio (0.0-1.0)
    #[arg(long, default_value = "1.0")]
    pub main_volume: f64,
}

impl ExportArgs {
    /// Warn about unsupported flags that are accepted for CLI compatibility
    pub fn warn_unsupported(&self) {
        if self.webcam.is_some() {
            eprintln!("WARNING: --webcam is not yet supported by kineto, ignoring");
        }
        if self.motion_blur > 0.0 {
            eprintln!("WARNING: --motion-blur is not yet supported by kineto, ignoring");
        }
        if !self.audio_track.is_empty() {
            eprintln!("WARNING: --audio-track is not yet supported by kineto, ignoring");
        }
        if self.bg_type.as_deref() == Some("gradient") {
            eprintln!("WARNING: --bg-type gradient is not yet supported by kineto, using color");
        }
        if self.bg_type.as_deref() == Some("image") {
            eprintln!("WARNING: --bg-type image is not yet supported by kineto, using color");
        }
    }

    /// Resolve the effective background color hex string
    pub fn effective_bg_color(&self) -> &str {
        self.bg_color.as_deref().unwrap_or("#000000")
    }

    /// Map codec + quality to encoder parameters
    pub fn encoder_params(&self) -> EncoderParams {
        let crf = match self.quality.as_str() {
            "low" => 28,
            "medium" => 23,
            _ => 18, // "high" or default
        };
        let preset = match self.quality.as_str() {
            "low" => "fast",
            _ => "medium",
        };
        EncoderParams {
            codec: self.codec.clone(),
            crf,
            preset: preset.to_string(),
        }
    }
}

#[derive(Debug)]
pub struct EncoderParams {
    pub codec: String,
    pub crf: u32,
    pub preset: String,
}

/// Layout computed from input video dimensions and padding
#[derive(Debug, Clone, Copy)]
pub struct CompositionLayout {
    pub canvas_w: u32,
    pub canvas_h: u32,
    pub video_x: u32,
    pub video_y: u32,
    pub video_w: u32,
    pub video_h: u32,
    pub padding: f64,
}

impl CompositionLayout {
    pub fn from_input(input_w: u32, input_h: u32, padding: f64) -> Self {
        let pad_x = (padding * input_w as f64).round() as u32;
        let pad_y = (padding * input_h as f64).round() as u32;
        // Ensure even dimensions for H.264 compatibility
        let video_w = (input_w - 2 * pad_x) & !1;
        let video_h = (input_h - 2 * pad_y) & !1;
        CompositionLayout {
            canvas_w: input_w,
            canvas_h: input_h,
            video_x: pad_x,
            video_y: pad_y,
            video_w,
            video_h,
            padding,
        }
    }
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Export(args) => {
            // Validate input
            let input_path = PathBuf::from(&args.input);
            if !input_path.exists() {
                eprintln!("ERROR: Input file does not exist: {}", args.input);
                process::exit(1);
            }

            args.warn_unsupported();

            // Load zoom segments if provided
            let zoom_segments = if let Some(ref path) = args.zoom_segments_file {
                match zoom::load_segments(path) {
                    Ok(segs) => {
                        eprintln!("Loaded {} zoom segments", segs.len());
                        Some(segs)
                    }
                    Err(e) => {
                        eprintln!("WARNING: Failed to load zoom segments: {}", e);
                        None
                    }
                }
            } else {
                None
            };

            // Run composition
            match compose::run(&args, zoom_segments.as_deref()) {
                Ok(()) => {
                    eprintln!("Composition complete: {}", args.output);
                }
                Err(e) => {
                    eprintln!("ERROR: Composition failed: {}", e);
                    process::exit(1);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn test_parse_mvp_args() {
        let args = Cli::parse_from([
            "kineto",
            "export",
            "-i", "input.mp4",
            "-o", "output.mp4",
            "--bg-type", "color",
            "--bg-color", "#0f0f23",
            "--padding", "0.06",
            "--corner-radius", "16",
            "--shadow",
            "--codec", "h264",
            "--quality", "high",
            "--zoom-segments-file", "zoom.json",
            "--zoom-easing", "smooth",
        ]);
        match args.command {
            Commands::Export(ref a) => {
                assert_eq!(a.input, "input.mp4");
                assert_eq!(a.output, "output.mp4");
                assert_eq!(a.bg_type.as_deref(), Some("color"));
                assert_eq!(a.bg_color.as_deref(), Some("#0f0f23"));
                assert!((a.padding - 0.06).abs() < 1e-9);
                assert_eq!(a.corner_radius, 16);
                assert!(a.shadow);
                assert_eq!(a.codec, "h264");
                assert_eq!(a.quality, "high");
                assert_eq!(a.zoom_segments_file.as_deref(), Some("zoom.json"));
                assert_eq!(a.zoom_easing, "smooth");
            }
        }
    }

    #[test]
    fn test_unsupported_flags_accepted() {
        // Non-MVP flags should parse without error
        let args = Cli::parse_from([
            "kineto",
            "export",
            "-i", "input.mp4",
            "-o", "output.mp4",
            "--webcam", "cam.mp4",
            "--webcam-pos", "50,50,200,200",
            "--motion-blur", "0.5",
            "--audio-track", "mic.wav:0.8",
            "--bg-gradient-start", "#000",
            "--bg-gradient-end", "#fff",
        ]);
        match args.command {
            Commands::Export(ref a) => {
                assert_eq!(a.webcam.as_deref(), Some("cam.mp4"));
                assert!((a.motion_blur - 0.5).abs() < 1e-9);
                assert_eq!(a.audio_track.len(), 1);
            }
        }
    }

    #[test]
    fn test_codec_quality_mapping() {
        let args = ExportArgs {
            input: "x".into(), output: "y".into(),
            codec: "h264".into(), quality: "high".into(),
            width: None, height: None,
            bg_type: None, bg_color: None,
            bg_gradient_start: None, bg_gradient_end: None,
            bg_gradient_angle: None, bg_image: None,
            bg_image_fit: "cover".into(),
            padding: 0.06, corner_radius: 16, shadow: false,
            webcam: None, webcam_pos: None,
            webcam_shape: "circle".into(), webcam_corner_radius: 16.0,
            motion_blur: 0.0,
            zoom_segments_file: None, zoom_easing: "smooth".into(),
            audio_track: vec![], main_volume: 1.0,
        };
        let params = args.encoder_params();
        assert_eq!(params.crf, 18);
        assert_eq!(params.preset, "medium");

        let args_low = ExportArgs { quality: "low".into(), ..args };
        let params_low = args_low.encoder_params();
        assert_eq!(params_low.crf, 28);
        assert_eq!(params_low.preset, "fast");
    }

    #[test]
    fn test_composition_layout() {
        let layout = CompositionLayout::from_input(1920, 1080, 0.06);
        assert_eq!(layout.canvas_w, 1920);
        assert_eq!(layout.canvas_h, 1080);
        assert_eq!(layout.video_x, 115); // round(0.06 * 1920)
        assert_eq!(layout.video_y, 65);  // round(0.06 * 1080)
        // video_w = 1920 - 2*115 = 1690, & !1 = 1690 (already even)
        assert_eq!(layout.video_w, 1690);
        // video_h = 1080 - 2*65 = 950, & !1 = 950 (already even)
        assert_eq!(layout.video_h, 950);
    }
}
