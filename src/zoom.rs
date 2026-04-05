//! Zoom computation and easing for cinematic composition.
//!
//! Ported from `so-engine/src/zoom.rs` and `so-engine/src/easing.rs`.
//! Standalone — no dependency on so-engine.

use serde::Deserialize;

/// Duration of the zoom-in and zoom-out transitions in seconds.
pub const ZOOM_TRANSITION_DURATION_S: f64 = 0.7;

#[derive(Debug, Clone, Deserialize)]
pub struct ZoomSegment {
    pub start_time: f64,
    pub end_time: f64,
    pub zoom_level: f64,
    pub center_x: f64,
    pub center_y: f64,
    pub easing: Option<String>,
    pub hold_duration_secs: Option<f64>,
}

/// Load zoom segments from a JSON file.
pub fn load_segments(path: &str) -> Result<Vec<ZoomSegment>, String> {
    let data = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {}: {}", path, e))?;
    serde_json::from_str(&data)
        .map_err(|e| format!("Failed to parse zoom segments: {}", e))
}

// ── Easing functions ──

/// Resolve easing name to function. Defaults to smoothstep.
fn resolve_easing(name: &str) -> fn(f64) -> f64 {
    match name {
        "snappy" => ease_out_cubic,
        "cinematic" => ease_in_out_quint,
        "spring" => spring_damped,
        _ => smoothstep, // "smooth" or unknown
    }
}

/// Smoothstep: t^2(3 - 2t)
pub fn smoothstep(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Ease-out cubic: 1 - (1-t)^3
pub fn ease_out_cubic(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// Ease-in-out quintic
pub fn ease_in_out_quint(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        16.0 * t.powi(5)
    } else {
        1.0 - (-2.0 * t + 2.0).powi(5) / 2.0
    }
}

/// Damped spring: 1 - e^(-zeta*omega*t) * cos(omega_d * t)
pub fn spring_damped(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    if t <= 0.0 { return 0.0; }
    if t >= 1.0 { return 1.0; }
    let zeta = 0.6;
    let omega = 12.0;
    let omega_d = omega * (1.0_f64 - zeta * zeta).sqrt();
    1.0 - (-zeta * omega * t).exp() * (omega_d * t).cos()
}

/// Clamp zoom focus so the viewport stays within canvas bounds.
pub fn clamp_zoom_focus(
    focus_x: f64,
    focus_y: f64,
    zoom: f64,
    canvas_w: f64,
    canvas_h: f64,
) -> (f64, f64) {
    if zoom <= 1.01 {
        return (focus_x, focus_y);
    }
    let vp_half_w = canvas_w / (2.0 * zoom);
    let vp_half_h = canvas_h / (2.0 * zoom);
    let min_x = vp_half_w;
    let max_x = canvas_w - vp_half_w;
    let min_y = vp_half_h;
    let max_y = canvas_h - vp_half_h;

    if max_x >= min_x && max_y >= min_y {
        (focus_x.clamp(min_x, max_x), focus_y.clamp(min_y, max_y))
    } else {
        (canvas_w / 2.0, canvas_h / 2.0)
    }
}

/// Compute zoom parameters at a given time.
///
/// Returns `(zoom_level, focus_x, focus_y)` in canvas pixel coordinates.
/// Uses top-left origin (ffmpeg convention). center_y from segments is
/// already top-left (frontend convention), so no Y-flip needed.
pub fn compute_zoom_at_time(
    segments: &[ZoomSegment],
    time_seconds: f64,
    canvas_w: f64,
    canvas_h: f64,
    video_x: f64,
    video_y: f64,
    video_w: f64,
    video_h: f64,
    session_easing: &str,
) -> (f64, f64, f64) {
    let half_t = ZOOM_TRANSITION_DURATION_S / 2.0;
    let default_fx = canvas_w / 2.0;
    let default_fy = canvas_h / 2.0;

    for segment in segments {
        let seg_half_t = half_t;

        let zoom_in_start = (segment.start_time - seg_half_t).max(0.0);
        let zoom_in_end = segment.start_time + seg_half_t;

        let zoom_out_start = if let Some(hold_secs) = segment.hold_duration_secs {
            (zoom_in_end + hold_secs).min(segment.end_time + seg_half_t)
        } else {
            segment.end_time - seg_half_t
        };
        let zoom_out_end = zoom_out_start + seg_half_t * 2.0;

        // Per-segment easing override or session default
        let easing_name = segment.easing.as_deref().unwrap_or(session_easing);
        let easing = resolve_easing(easing_name);

        // Focus point: top-left origin (no Y-flip for ffmpeg)
        let raw_fx = video_x + segment.center_x * video_w;
        let raw_fy = video_y + segment.center_y * video_h;

        let (clamped_fx, clamped_fy) = clamp_zoom_focus(
            raw_fx, raw_fy, segment.zoom_level, canvas_w, canvas_h,
        );

        if time_seconds >= zoom_in_start && time_seconds < zoom_in_end {
            let duration = (zoom_in_end - zoom_in_start).max(0.001);
            let t_raw = ((time_seconds - zoom_in_start) / duration).clamp(0.0, 1.0);
            let t = easing(t_raw);
            let zoom = 1.0 + t * (segment.zoom_level - 1.0);
            let fx = default_fx + t * (clamped_fx - default_fx);
            let fy = default_fy + t * (clamped_fy - default_fy);
            return (zoom, fx, fy);
        } else if time_seconds >= zoom_in_end && time_seconds < zoom_out_start {
            return (segment.zoom_level, clamped_fx, clamped_fy);
        } else if time_seconds >= zoom_out_start && time_seconds < zoom_out_end {
            let duration = (zoom_out_end - zoom_out_start).max(0.001);
            let t_raw = ((time_seconds - zoom_out_start) / duration).clamp(0.0, 1.0);
            let t = easing(t_raw);
            let zoom = segment.zoom_level - t * (segment.zoom_level - 1.0);
            let fx = clamped_fx + t * (default_fx - clamped_fx);
            let fy = clamped_fy + t * (default_fy - clamped_fy);
            return (zoom, fx, fy);
        }
    }

    (1.0, default_fx, default_fy)
}

/// Apply zoom crop to a frame: compute the crop region for a given zoom level and focus.
///
/// Returns `(crop_x, crop_y, crop_w, crop_h)` in pixels.
pub fn compute_crop_rect(
    zoom: f64,
    focus_x: f64,
    focus_y: f64,
    canvas_w: u32,
    canvas_h: u32,
) -> (u32, u32, u32, u32) {
    if zoom <= 1.01 {
        return (0, 0, canvas_w, canvas_h);
    }

    let crop_w = (canvas_w as f64 / zoom).round() as u32;
    let crop_h = (canvas_h as f64 / zoom).round() as u32;

    // Center crop on focus point, clamped to canvas bounds
    let crop_x = (focus_x - crop_w as f64 / 2.0)
        .max(0.0)
        .min((canvas_w - crop_w) as f64) as u32;
    let crop_y = (focus_y - crop_h as f64 / 2.0)
        .max(0.0)
        .min((canvas_h - crop_h) as f64) as u32;

    (crop_x, crop_y, crop_w, crop_h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_segments_returns_identity() {
        let (z, fx, fy) = compute_zoom_at_time(&[], 1.0, 1920.0, 1080.0, 115.0, 65.0, 1690.0, 950.0, "smooth");
        assert!((z - 1.0).abs() < 1e-9);
        assert!((fx - 960.0).abs() < 1e-9);
        assert!((fy - 540.0).abs() < 1e-9);
    }

    #[test]
    fn test_single_segment_hold_phase() {
        let segments = vec![ZoomSegment {
            start_time: 2.0,
            end_time: 5.0,
            zoom_level: 2.0,
            center_x: 0.3,
            center_y: 0.6,
            easing: None,
            hold_duration_secs: None,
        }];
        // At t=3.5 (well within hold phase)
        let (z, _fx, _fy) = compute_zoom_at_time(
            &segments, 3.5, 1920.0, 1080.0, 115.0, 65.0, 1690.0, 950.0, "smooth",
        );
        assert!((z - 2.0).abs() < 1e-9);
    }

    #[test]
    fn test_transition_easing_smoothstep() {
        // Smoothstep at t=0.5 should produce 0.5
        assert!((smoothstep(0.5) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn test_all_easing_types_dont_panic() {
        for name in &["smooth", "snappy", "cinematic", "spring"] {
            let f = resolve_easing(name);
            for i in 0..=100 {
                let t = i as f64 / 100.0;
                let _ = f(t); // should not panic
            }
        }
    }

    #[test]
    fn test_easings_start_zero_end_one() {
        for f in &[smoothstep, ease_out_cubic, ease_in_out_quint, spring_damped] {
            assert!(f(0.0).abs() < 1e-9, "Easing should start at 0");
            assert!((f(1.0) - 1.0).abs() < 1e-9, "Easing should end at 1");
        }
    }

    #[test]
    fn test_spring_overshoots() {
        let mut found = false;
        for i in 1..100 {
            if spring_damped(i as f64 / 100.0) > 1.0 {
                found = true;
                break;
            }
        }
        assert!(found, "Spring should overshoot 1.0");
    }

    #[test]
    fn test_crop_rect_no_zoom() {
        let (x, y, w, h) = compute_crop_rect(1.0, 960.0, 540.0, 1920, 1080);
        assert_eq!((x, y, w, h), (0, 0, 1920, 1080));
    }

    #[test]
    fn test_crop_rect_2x_zoom_center() {
        let (x, y, w, h) = compute_crop_rect(2.0, 960.0, 540.0, 1920, 1080);
        assert_eq!(w, 960);
        assert_eq!(h, 540);
        assert_eq!(x, 480); // 960 - 960/2
        assert_eq!(y, 270); // 540 - 540/2
    }

    #[test]
    fn test_crop_rect_clamped_to_bounds() {
        // Focus at top-left corner with 2x zoom
        let (x, y, w, h) = compute_crop_rect(2.0, 0.0, 0.0, 1920, 1080);
        assert_eq!(x, 0);
        assert_eq!(y, 0);
        assert_eq!(w, 960);
        assert_eq!(h, 540);
    }

    #[test]
    fn test_load_segments_roundtrip() {
        let json = r#"[
            {
                "start_time": 2.5,
                "end_time": 5.0,
                "zoom_level": 2.0,
                "center_x": 0.3,
                "center_y": 0.6,
                "easing": "smooth",
                "hold_duration_secs": 1.5
            }
        ]"#;
        let segments: Vec<ZoomSegment> = serde_json::from_str(json).unwrap();
        assert_eq!(segments.len(), 1);
        assert!((segments[0].start_time - 2.5).abs() < 1e-9);
        assert_eq!(segments[0].easing.as_deref(), Some("smooth"));
    }
}
