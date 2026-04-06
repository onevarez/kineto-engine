//! Zoom computation and easing for cinematic composition.
//!
//! Ported from `so-engine/src/zoom.rs` and `so-engine/src/easing.rs`.
//! Standalone — no dependency on so-engine.

use serde::Deserialize;

/// Default duration of the zoom-in transition in seconds.
pub const ZOOM_IN_DURATION_S: f64 = 1.0;

/// Default duration of the zoom-out transition in seconds.
pub const ZOOM_OUT_DURATION_S: f64 = 0.7;

/// Maximum gap between segments to trigger connected panning instead of bounce.
const CONNECTED_PAN_GAP_S: f64 = 1.5;

/// Fraction of viewport range that uses soft cubic easing at boundaries.
const SOFT_CLAMP_FRACTION: f64 = 0.12;

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

/// Cubic ease into boundary: -t^3 + 2t^2 (smooth deceleration near edges).
fn ease_into_boundary(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    -t * t * t + 2.0 * t * t
}

/// Soft clamp: hard limit at min/max, but within the soft zone (inside the valid range),
/// movement decelerates via cubic easing so the viewport doesn't snap to the boundary.
fn soft_clamp(value: f64, min: f64, max: f64, soft_zone: f64) -> f64 {
    if soft_zone <= 0.0 || max <= min {
        return value.clamp(min, max);
    }
    if value <= min {
        min
    } else if value >= max {
        max
    } else if value < min + soft_zone {
        // Approaching min from inside — ease
        let t = (value - min) / soft_zone; // 0..1 (0 = at boundary, 1 = left soft zone)
        min + soft_zone * ease_into_boundary(t)
    } else if value > max - soft_zone {
        // Approaching max from inside — ease
        let t = (max - value) / soft_zone; // 0..1 (0 = at boundary, 1 = left soft zone)
        max - soft_zone * ease_into_boundary(t)
    } else {
        value
    }
}

/// Clamp zoom focus so the viewport stays within canvas bounds.
/// Uses soft cubic clamping near edges for smoother panning.
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
        let range_x = max_x - min_x;
        let range_y = max_y - min_y;
        let soft_x = range_x * SOFT_CLAMP_FRACTION;
        let soft_y = range_y * SOFT_CLAMP_FRACTION;
        (
            soft_clamp(focus_x, min_x, max_x, soft_x),
            soft_clamp(focus_y, min_y, max_y, soft_y),
        )
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
    zoom_in_duration: f64,
    zoom_out_duration: f64,
) -> (f64, f64, f64) {
    let half_in = zoom_in_duration / 2.0;
    let half_out = zoom_out_duration / 2.0;
    let default_fx = canvas_w / 2.0;
    let default_fy = canvas_h / 2.0;

    for (i, segment) in segments.iter().enumerate() {
        let zoom_in_start = (segment.start_time - half_in).max(0.0);
        let zoom_in_end = segment.start_time + half_in;

        let zoom_out_start = if let Some(hold_secs) = segment.hold_duration_secs {
            (zoom_in_end + hold_secs).min(segment.end_time + half_out)
        } else {
            segment.end_time - half_out
        };
        let zoom_out_end = zoom_out_start + zoom_out_duration;

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
            // Connected panning: if next segment is close, pan directly instead of bounce
            if let Some(next) = segments.get(i + 1) {
                let gap = next.start_time - segment.end_time;
                if gap < CONNECTED_PAN_GAP_S {
                    let pan_start = zoom_out_start;
                    let pan_end = next.start_time + half_in;
                    let pan_duration = (pan_end - pan_start).max(0.001);

                    let next_raw_fx = video_x + next.center_x * video_w;
                    let next_raw_fy = video_y + next.center_y * video_h;
                    let (next_fx, next_fy) = clamp_zoom_focus(
                        next_raw_fx, next_raw_fy, next.zoom_level, canvas_w, canvas_h,
                    );

                    if time_seconds < pan_end {
                        let t_raw = ((time_seconds - pan_start) / pan_duration).clamp(0.0, 1.0);
                        let t = ease_in_out_quint(t_raw);

                        let zoom = segment.zoom_level + t * (next.zoom_level - segment.zoom_level);
                        let fx = clamped_fx + t * (next_fx - clamped_fx);
                        let fy = clamped_fy + t * (next_fy - clamped_fy);
                        return (zoom, fx, fy);
                    }
                }
            }

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
        let (z, fx, fy) = compute_zoom_at_time(
            &[], 1.0, 1920.0, 1080.0, 115.0, 65.0, 1690.0, 950.0, "smooth",
            ZOOM_IN_DURATION_S, ZOOM_OUT_DURATION_S,
        );
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
            ZOOM_IN_DURATION_S, ZOOM_OUT_DURATION_S,
        );
        assert!((z - 2.0).abs() < 1e-9);
    }

    #[test]
    fn test_asymmetric_zoom_timing() {
        let segments = vec![ZoomSegment {
            start_time: 2.0,
            end_time: 5.0,
            zoom_level: 2.0,
            center_x: 0.5,
            center_y: 0.5,
            easing: None,
            hold_duration_secs: None,
        }];
        // With zoom_in=2.0s, half_in=1.0, zoom starts at t=1.0
        let (z1, _, _) = compute_zoom_at_time(
            &segments, 1.0, 1920.0, 1080.0, 115.0, 65.0, 1690.0, 950.0, "smooth",
            2.0, 0.5,
        );
        assert!((z1 - 1.0).abs() < 0.01, "Should be near 1.0 at zoom_in_start");

        // At midpoint of zoom-in
        let (z2, _, _) = compute_zoom_at_time(
            &segments, 1.5, 1920.0, 1080.0, 115.0, 65.0, 1690.0, 950.0, "smooth",
            2.0, 0.5,
        );
        assert!(z2 > 1.0 && z2 < 2.0, "Should be mid-zoom at 1.5s");
    }

    #[test]
    fn test_connected_panning() {
        let segments = vec![
            ZoomSegment {
                start_time: 1.0, end_time: 3.0, zoom_level: 2.0,
                center_x: 0.3, center_y: 0.5, easing: None, hold_duration_secs: None,
            },
            ZoomSegment {
                start_time: 3.5, end_time: 6.0, zoom_level: 2.5,
                center_x: 0.7, center_y: 0.5, easing: None, hold_duration_secs: None,
            },
        ];
        // Gap is 0.5s < 1.5s, so should use connected panning
        // At t=3.2 (between segments), should be panning, not at zoom=1.0
        let (z, _, _) = compute_zoom_at_time(
            &segments, 3.2, 1920.0, 1080.0, 115.0, 65.0, 1690.0, 950.0, "smooth",
            ZOOM_IN_DURATION_S, ZOOM_OUT_DURATION_S,
        );
        assert!(z > 1.5, "Connected pan should maintain zoom, got {}", z);
    }

    #[test]
    fn test_soft_clamp_within_bounds() {
        let result = soft_clamp(500.0, 200.0, 800.0, 72.0);
        assert!((result - 500.0).abs() < 1e-9, "Should pass through unchanged");
    }

    #[test]
    fn test_soft_clamp_at_boundary() {
        // Exactly at max should return max
        let at_max = soft_clamp(800.0, 200.0, 800.0, 72.0);
        assert!((at_max - 800.0).abs() < 1e-9, "At max should be exact, got {}", at_max);

        // Beyond max should be clamped to max
        let beyond = soft_clamp(900.0, 200.0, 800.0, 72.0);
        assert!((beyond - 800.0).abs() < 1e-9, "Beyond max should clamp, got {}", beyond);
    }

    #[test]
    fn test_soft_clamp_deceleration() {
        // In the soft zone, linear input should produce sub-linear output
        // Mid soft zone: input at max - soft_zone/2 should be pulled toward max
        let result = soft_clamp(764.0, 200.0, 800.0, 72.0);
        assert!(result >= 764.0, "Soft clamp should not reduce, got {}", result);
        assert!(result <= 800.0, "Soft clamp should stay in bounds, got {}", result);
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
