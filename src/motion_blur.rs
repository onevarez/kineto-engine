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
/// `buf` is the RGBA buffer (modified in place), `scratch` is a pre-allocated temp buffer.
pub fn apply(
    buf: &mut [u8],
    scratch: &mut [u8],
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

    // Adaptive kernel size based on blur_px
    let taps: usize = if blur_px < 4.0 {
        7
    } else if blur_px < 8.0 {
        11
    } else {
        15
    };

    // Normalize direction vector
    let speed = (vx * vx + vy * vy).sqrt();
    if speed < 0.001 {
        return;
    }
    let dir_x = vx / speed;
    let dir_y = vy / speed;

    // Step size along the direction
    let step = blur_px / taps as f64;
    let half_taps = taps as f64 / 2.0;
    let inv_taps = 1.0 / taps as f64;

    scratch[..w * h * 4].copy_from_slice(&buf[..w * h * 4]);

    for y in 0..h {
        for x in 0..w {
            let mut r_sum: f64 = 0.0;
            let mut g_sum: f64 = 0.0;
            let mut b_sum: f64 = 0.0;
            let mut a_sum: f64 = 0.0;

            for tap in 0..taps {
                let offset = (tap as f64 - half_taps) * step;
                let sx = (x as f64 + dir_x * offset).round() as i64;
                let sy = (y as f64 + dir_y * offset).round() as i64;

                let sx = sx.clamp(0, w as i64 - 1) as usize;
                let sy = sy.clamp(0, h as i64 - 1) as usize;

                let idx = (sy * w + sx) * 4;
                r_sum += scratch[idx] as f64;
                g_sum += scratch[idx + 1] as f64;
                b_sum += scratch[idx + 2] as f64;
                a_sum += scratch[idx + 3] as f64;
            }

            let idx = (y * w + x) * 4;
            buf[idx] = (r_sum * inv_taps).round() as u8;
            buf[idx + 1] = (g_sum * inv_taps).round() as u8;
            buf[idx + 2] = (b_sum * inv_taps).round() as u8;
            buf[idx + 3] = (a_sum * inv_taps).round() as u8;
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
        let mut buf = vec![128u8; 16 * 16 * 4];
        let mut scratch = vec![0u8; 16 * 16 * 4];
        apply(&mut buf, &mut scratch, 16, 16, 100.0, 0.0, 5.0);
        // Should not crash, and buffer should be modified
    }
}
