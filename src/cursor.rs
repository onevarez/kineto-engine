//! Cursor physics: spring-damper smoothing of raw cursor telemetry.

use serde::Deserialize;

// ── Physics presets ──

const ADAPTIVE_RAMP_DISTANCE: f64 = 200.0;
const ADAPTIVE_DAMPING_MIN_FACTOR: f64 = 0.6;

#[derive(Debug, Clone, Copy)]
pub struct PhysicsPreset {
    pub stiffness: f64,
    pub damping: f64,
}

impl PhysicsPreset {
    pub fn from_name(name: &str) -> Self {
        match name {
            "rapid" => PhysicsPreset { stiffness: 400.0, damping: 35.0 },
            "linear" => PhysicsPreset { stiffness: 800.0, damping: 60.0 },
            "cinematic" => PhysicsPreset { stiffness: 80.0, damping: 18.0 },
            _ => PhysicsPreset { stiffness: 120.0, damping: 22.0 }, // "mellow"
        }
    }
}

// ── Telemetry data ──

#[derive(Debug, Deserialize)]
pub struct CursorTelemetry {
    #[allow(dead_code)]
    pub version: Option<u32>,
    pub display_width: f64,
    pub display_height: f64,
    pub events: Vec<CursorEvent>,
}

#[derive(Debug, Deserialize)]
pub struct CursorEvent {
    pub time_ms: f64,
    pub x: f64,
    pub y: f64,
    #[serde(rename = "type", default)]
    pub event_type: Option<String>,
}

// ── Smoothed output ──

#[derive(Debug, Clone)]
pub struct SmoothedFrame {
    pub time_secs: f64,
    pub x: f64,
    pub y: f64,
    pub velocity: f64,
    pub vel_x: f64,
    pub vel_y: f64,
    pub is_clicking: bool,
    pub click_scale: f64,
}

// ── Click animation ──

const CLICK_SCALE_UP_MS: f64 = 50.0;
const CLICK_SCALE_DOWN_MS: f64 = 100.0;
const CLICK_MAX_SCALE: f64 = 1.3;

fn click_scale_at(ms_since_click: f64) -> f64 {
    if ms_since_click < 0.0 {
        1.0
    } else if ms_since_click < CLICK_SCALE_UP_MS {
        let t = ms_since_click / CLICK_SCALE_UP_MS;
        let eased = 1.0 - (1.0 - t).powi(3); // ease-out-cubic
        1.0 + eased * (CLICK_MAX_SCALE - 1.0)
    } else if ms_since_click < CLICK_SCALE_UP_MS + CLICK_SCALE_DOWN_MS {
        let t = (ms_since_click - CLICK_SCALE_UP_MS) / CLICK_SCALE_DOWN_MS;
        let eased = t * t * t; // ease-in-cubic
        CLICK_MAX_SCALE - eased * (CLICK_MAX_SCALE - 1.0)
    } else {
        1.0
    }
}

// ── Distance-adaptive damping ──

fn adaptive_damping(base_damping: f64, displacement: f64) -> f64 {
    let factor = 1.0
        - (1.0 - ADAPTIVE_DAMPING_MIN_FACTOR)
            * (displacement / ADAPTIVE_RAMP_DISTANCE).min(1.0);
    base_damping * factor
}

// ── Core simulation ──

/// Interpolate raw events to find cursor position at a given time.
fn interpolate_target(events: &[CursorEvent], time_ms: f64) -> (f64, f64) {
    if events.is_empty() {
        return (0.0, 0.0);
    }
    if time_ms <= events[0].time_ms {
        return (events[0].x, events[0].y);
    }
    if time_ms >= events[events.len() - 1].time_ms {
        let last = &events[events.len() - 1];
        return (last.x, last.y);
    }

    // Binary search for the interval
    let idx = events
        .partition_point(|e| e.time_ms <= time_ms)
        .saturating_sub(1);
    let a = &events[idx];
    let b = &events[(idx + 1).min(events.len() - 1)];
    let span = (b.time_ms - a.time_ms).max(0.001);
    let t = ((time_ms - a.time_ms) / span).clamp(0.0, 1.0);
    (a.x + t * (b.x - a.x), a.y + t * (b.y - a.y))
}

/// Find the most recent click event at or before time_ms.
fn last_click_time(events: &[CursorEvent], time_ms: f64) -> Option<f64> {
    events
        .iter()
        .rev()
        .find(|e| {
            e.time_ms <= time_ms
                && e.event_type
                    .as_deref()
                    .map_or(false, |t| t == "click" || t == "mousedown")
        })
        .map(|e| e.time_ms)
}

pub fn smooth_cursor_path(
    telemetry: &CursorTelemetry,
    preset: PhysicsPreset,
    fps: f64,
) -> Vec<SmoothedFrame> {
    if telemetry.events.is_empty() || fps <= 0.0 {
        return Vec::new();
    }

    let dt = 1.0 / fps;
    let dt_ms = dt * 1000.0;
    let total_ms = telemetry.events.last().unwrap().time_ms;
    let num_frames = ((total_ms / dt_ms).ceil() as usize).max(1);

    let mut frames = Vec::with_capacity(num_frames);

    // Initial state
    let (init_x, init_y) = (telemetry.events[0].x, telemetry.events[0].y);
    let mut pos_x = init_x;
    let mut pos_y = init_y;
    let mut vel_x = 0.0_f64;
    let mut vel_y = 0.0_f64;

    for i in 0..num_frames {
        let time_ms = i as f64 * dt_ms;
        let (target_x, target_y) = interpolate_target(&telemetry.events, time_ms);

        // Spring-damper: F = -k * displacement - d * velocity
        let disp_x = pos_x - target_x;
        let disp_y = pos_y - target_y;
        let displacement = (disp_x * disp_x + disp_y * disp_y).sqrt();

        let d = adaptive_damping(preset.damping, displacement);

        let acc_x = -preset.stiffness * disp_x - d * vel_x;
        let acc_y = -preset.stiffness * disp_y - d * vel_y;

        // Semi-implicit Euler
        vel_x += acc_x * dt;
        vel_y += acc_y * dt;
        pos_x += vel_x * dt;
        pos_y += vel_y * dt;

        let velocity = (vel_x * vel_x + vel_y * vel_y).sqrt();

        // Click state
        let click_time = last_click_time(&telemetry.events, time_ms);
        let is_clicking = click_time.map_or(false, |ct| time_ms - ct < CLICK_SCALE_UP_MS + CLICK_SCALE_DOWN_MS);
        let click_scale = click_time
            .map(|ct| click_scale_at(time_ms - ct))
            .unwrap_or(1.0);

        frames.push(SmoothedFrame {
            time_secs: time_ms / 1000.0,
            x: pos_x,
            y: pos_y,
            velocity,
            vel_x,
            vel_y,
            is_clicking,
            click_scale,
        });
    }

    frames
}

/// Load telemetry and run physics simulation.
pub fn load_and_smooth(
    path: &str,
    preset_name: &str,
    fps: f64,
) -> Result<Vec<SmoothedFrame>, String> {
    let data =
        std::fs::read_to_string(path).map_err(|e| format!("Failed to read {}: {}", path, e))?;
    let telemetry: CursorTelemetry =
        serde_json::from_str(&data).map_err(|e| format!("Failed to parse cursor JSON: {}", e))?;
    let preset = PhysicsPreset::from_name(preset_name);
    Ok(smooth_cursor_path(&telemetry, preset, fps))
}

/// Binary search for the smoothed frame nearest to a given time.
pub fn frame_at_time(frames: &[SmoothedFrame], time_secs: f64) -> Option<&SmoothedFrame> {
    if frames.is_empty() {
        return None;
    }
    let idx = frames
        .partition_point(|f| f.time_secs <= time_secs)
        .saturating_sub(1);
    Some(&frames[idx.min(frames.len() - 1)])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_telemetry() -> CursorTelemetry {
        CursorTelemetry {
            version: Some(1),
            display_width: 1920.0,
            display_height: 1080.0,
            events: vec![
                CursorEvent { time_ms: 0.0, x: 100.0, y: 100.0, event_type: None },
                CursorEvent { time_ms: 500.0, x: 500.0, y: 300.0, event_type: None },
                CursorEvent { time_ms: 1000.0, x: 500.0, y: 300.0, event_type: None },
            ],
        }
    }

    #[test]
    fn test_spring_converges() {
        let telemetry = test_telemetry();
        let preset = PhysicsPreset::from_name("mellow");
        let frames = smooth_cursor_path(&telemetry, preset, 60.0);
        assert!(!frames.is_empty());

        // Last frame should be near target (500, 300)
        let last = frames.last().unwrap();
        assert!((last.x - 500.0).abs() < 5.0, "x should converge, got {}", last.x);
        assert!((last.y - 300.0).abs() < 5.0, "y should converge, got {}", last.y);
    }

    #[test]
    fn test_all_presets_converge() {
        let telemetry = test_telemetry();
        for name in &["mellow", "rapid", "linear", "cinematic"] {
            let preset = PhysicsPreset::from_name(name);
            let frames = smooth_cursor_path(&telemetry, preset, 60.0);
            let last = frames.last().unwrap();
            assert!(
                (last.x - 500.0).abs() < 10.0,
                "{} preset: x={} didn't converge", name, last.x
            );
        }
    }

    #[test]
    fn test_click_animation() {
        assert!((click_scale_at(-1.0) - 1.0).abs() < 1e-9);
        assert!((click_scale_at(0.0) - 1.0).abs() < 1e-9);
        // Peak near 50ms
        let peak = click_scale_at(50.0);
        assert!((peak - CLICK_MAX_SCALE).abs() < 0.01, "Peak should be ~1.3, got {}", peak);
        // Back to 1.0 at end
        let end = click_scale_at(150.0);
        assert!((end - 1.0).abs() < 0.01, "Should return to 1.0, got {}", end);
    }

    #[test]
    fn test_adaptive_damping_range() {
        let base = 22.0;
        let close = adaptive_damping(base, 10.0);
        let far = adaptive_damping(base, 300.0);
        assert!(close > far, "Close damping should be higher: {} vs {}", close, far);
        assert!((far - base * ADAPTIVE_DAMPING_MIN_FACTOR).abs() < 0.1);
    }

    #[test]
    fn test_frame_at_time() {
        let telemetry = test_telemetry();
        let preset = PhysicsPreset::from_name("mellow");
        let frames = smooth_cursor_path(&telemetry, preset, 60.0);
        let f = frame_at_time(&frames, 0.5).unwrap();
        assert!(f.time_secs <= 0.5 + 1.0 / 60.0);
    }

    #[test]
    fn test_empty_telemetry() {
        let telemetry = CursorTelemetry {
            version: Some(1),
            display_width: 1920.0,
            display_height: 1080.0,
            events: vec![],
        };
        let frames = smooth_cursor_path(&telemetry, PhysicsPreset::from_name("mellow"), 60.0);
        assert!(frames.is_empty());
    }
}
