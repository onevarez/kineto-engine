//! Velocity-based directional motion blur for camera movement.

const DEADZONE_PX_PER_S: f64 = 12.0;
const MAX_BLUR_PX: f64 = 14.0;
const PEAK_VELOCITY: f64 = 1400.0;

#[derive(Debug, Clone, Copy)]
pub struct CameraTransform {
    pub x: f64,
    pub y: f64,
    pub scale: f64,
}

/// Compute camera velocity from consecutive transforms.
/// Returns (vx, vy, speed) in pixels/second.
pub fn compute_camera_velocity(
    prev: &CameraTransform,
    curr: &CameraTransform,
    dt: f64,
    canvas_size: f64,
) -> (f64, f64, f64) {
    if dt <= 0.0 {
        return (0.0, 0.0, 0.0);
    }
    let dx = (curr.x - prev.x) / dt;
    let dy = (curr.y - prev.y) / dt;
    // Scale velocity: convert to equivalent pixel displacement
    let ds = ((curr.scale - prev.scale) / dt) * canvas_size * 0.5;
    let vx = dx + ds;
    let vy = dy;
    let speed = (vx * vx + vy * vy).sqrt();
    (vx, vy, speed)
}

/// Non-linear slider response: a * (1 + 1.2 * a).
fn blur_amount_response(a: f64) -> f64 {
    let a = a.clamp(0.0, 1.0);
    a * (1.0 + 1.2 * a)
}

/// Compute blur radius in pixels from velocity and slider intensity.
pub fn compute_blur_radius(speed: f64, slider: f64) -> f64 {
    if speed < DEADZONE_PX_PER_S || slider <= 0.0 {
        return 0.0;
    }
    let effective_speed = speed - DEADZONE_PX_PER_S;
    let normalized = (effective_speed / PEAK_VELOCITY).min(1.0);
    let intensity = normalized * normalized; // quadratic curve
    intensity * MAX_BLUR_PX * blur_amount_response(slider)
}

/// Apply directional box blur along the motion vector.
///
/// Reads from `src` and writes the blurred result to `dst` — they must be
/// separate buffers of equal size (`width * height * 4` bytes, packed RGBA,
/// no stride padding).  No intermediate allocation is performed.
pub fn apply(
    src: &[u8],
    dst: &mut [u8],
    width: u32,
    height: u32,
    vx: f64,
    vy: f64,
    blur_px: f64,
) {
    if blur_px < 0.5 {
        return;
    }

    let w = width as usize;
    let h = height as usize;

    // Adaptive kernel size based on blur radius
    let taps: usize = if blur_px < 4.0 { 7 } else if blur_px < 8.0 { 11 } else { 15 };

    // Normalize direction vector
    let speed = (vx * vx + vy * vy).sqrt();
    if speed < 0.001 {
        return;
    }
    let dir_x = vx / speed;
    let dir_y = vy / speed;

    let step = blur_px / taps as f64;
    let half_taps = taps as f64 / 2.0;
    let half_taps_u32 = (taps / 2) as u32;
    let taps_u32 = taps as u32;

    // Precompute integer (dx, dy) per tap — identical for every pixel.
    // This removes `taps` float multiplications + round() calls from the hot loop.
    let tap_deltas: Vec<(i32, i32)> = (0..taps)
        .map(|t| {
            let offset = (t as f64 - half_taps) * step;
            (
                (dir_x * offset).round() as i32,
                (dir_y * offset).round() as i32,
            )
        })
        .collect();

    for y in 0..h {
        for x in 0..w {
            let mut r: u32 = 0;
            let mut g: u32 = 0;
            let mut b: u32 = 0;
            let mut a: u32 = 0;

            for &(tdx, tdy) in &tap_deltas {
                let sx = (x as i32 + tdx).clamp(0, w as i32 - 1) as usize;
                let sy = (y as i32 + tdy).clamp(0, h as i32 - 1) as usize;
                let idx = (sy * w + sx) * 4;
                r += src[idx]     as u32;
                g += src[idx + 1] as u32;
                b += src[idx + 2] as u32;
                a += src[idx + 3] as u32;
            }

            // Rounding integer division: (sum + taps/2) / taps
            let idx = (y * w + x) * 4;
            dst[idx]     = ((r + half_taps_u32) / taps_u32) as u8;
            dst[idx + 1] = ((g + half_taps_u32) / taps_u32) as u8;
            dst[idx + 2] = ((b + half_taps_u32) / taps_u32) as u8;
            dst[idx + 3] = ((a + half_taps_u32) / taps_u32) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zero_velocity_no_blur() {
        assert!(compute_blur_radius(0.0, 0.5) < 0.01);
    }

    #[test]
    fn test_deadzone() {
        assert!(compute_blur_radius(10.0, 1.0) < 0.01);
    }

    #[test]
    fn test_blur_increases_with_speed() {
        let low = compute_blur_radius(100.0, 0.5);
        let high = compute_blur_radius(800.0, 0.5);
        assert!(high > low, "Higher speed should produce more blur");
    }

    #[test]
    fn test_blur_capped() {
        let max = compute_blur_radius(2000.0, 1.0);
        assert!(max <= MAX_BLUR_PX * 2.5, "Blur should be bounded, got {}", max);
    }

    #[test]
    fn test_slider_response() {
        assert!(blur_amount_response(0.0).abs() < 1e-9);
        assert!(blur_amount_response(1.0) > 1.0);
    }

    #[test]
    fn test_camera_velocity() {
        let prev = CameraTransform { x: 100.0, y: 200.0, scale: 1.0 };
        let curr = CameraTransform { x: 200.0, y: 200.0, scale: 1.0 };
        let (vx, vy, speed) = compute_camera_velocity(&prev, &curr, 1.0 / 30.0, 1920.0);
        assert!(vx > 0.0);
        assert!(vy.abs() < 0.01);
        assert!(speed > 0.0);
    }

    #[test]
    fn test_apply_no_crash() {
        let src = vec![128u8; 16 * 16 * 4];
        let mut dst = vec![0u8; 16 * 16 * 4];
        apply(&src, &mut dst, 16, 16, 100.0, 0.0, 5.0);
        // Output should be non-zero and within u8 range
        assert!(dst.iter().any(|&v| v > 0));
    }

    #[test]
    fn test_apply_separate_buffers() {
        // Verify src is not modified and dst receives blurred output
        let src = vec![200u8; 32 * 32 * 4];
        let mut dst = vec![0u8; 32 * 32 * 4];
        apply(&src, &mut dst, 32, 32, 50.0, 30.0, 6.0);
        assert!(src.iter().all(|&v| v == 200), "src must not be modified");
        assert!(dst.iter().any(|&v| v > 0), "dst must be written");
    }
}
