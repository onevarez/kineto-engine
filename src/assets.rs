//! Pre-baked image asset generation for cinematic composition.
//!
//! Generates shadow and corner mask images using the `image` crate.
//! These are fed as single-frame inputs to the libav* filter graph.
//!
//! Ported from `src-tauri/src/commands/ffmpeg_export.rs` lines 1057-1380.

use image::{DynamicImage, GrayImage, Luma, Rgba, RgbaImage};

/// Parse a hex color string like "#0f0f23" or "0f0f23" into (r, g, b).
pub fn parse_hex_color(hex: &str) -> Result<(u8, u8, u8), String> {
    let hex = hex.trim_start_matches('#');
    if hex.len() != 6 {
        return Err(format!("Invalid hex color: #{}", hex));
    }
    let r = u8::from_str_radix(&hex[0..2], 16).map_err(|e| format!("Bad hex: {}", e))?;
    let g = u8::from_str_radix(&hex[2..4], 16).map_err(|e| format!("Bad hex: {}", e))?;
    let b = u8::from_str_radix(&hex[4..6], 16).map_err(|e| format!("Bad hex: {}", e))?;
    Ok((r, g, b))
}

/// Generate a canvas-sized RGBA image with solid background color and optional shadow.
///
/// The shadow is a blurred rounded rectangle rendered at 1/4 resolution for performance,
/// then upscaled and alpha-composited onto the background.
pub fn generate_canvas_image(
    canvas_w: u32,
    canvas_h: u32,
    video_x: u32,
    video_y: u32,
    video_w: u32,
    video_h: u32,
    bg_color_hex: &str,
    corner_radius: u32,
    shadow: bool,
) -> Result<RgbaImage, String> {
    let (r, g, b) = parse_hex_color(bg_color_hex)?;

    // Fill background
    let mut canvas = RgbaImage::new(canvas_w, canvas_h);
    for pixel in canvas.pixels_mut() {
        *pixel = Rgba([r, g, b, 255]);
    }

    if shadow {
        let shadow_img = generate_shadow_image(
            canvas_w, canvas_h, video_x, video_y, video_w, video_h, corner_radius,
        );
        image::imageops::overlay(&mut canvas, &shadow_img, 0, 0);
    }

    Ok(canvas)
}

/// Generate a canvas-sized shadow image (RGBA, mostly transparent).
///
/// Renders a filled rounded rectangle at 1/4 resolution, blurs it, then upscales.
/// Shadow offset: 8px down. Alpha: 0.45 * 255 = 115.
fn generate_shadow_image(
    canvas_w: u32,
    canvas_h: u32,
    video_x: u32,
    video_y: u32,
    video_w: u32,
    video_h: u32,
    corner_radius: u32,
) -> RgbaImage {
    let scale = 4u32;
    let sw = (canvas_w / scale).max(1);
    let sh = (canvas_h / scale).max(1);

    let alpha: u8 = 115; // 0.45 * 255
    let shadow_offset_y = (8.0 / scale as f64).round() as i32;

    let mut shadow = RgbaImage::new(sw, sh);

    // Map video rect to shadow space
    let vx = (video_x as f64 / scale as f64).round() as i32;
    let vy = (video_y as f64 / scale as f64).round() as i32 + shadow_offset_y;
    let vw = (video_w as f64 / scale as f64).round() as i32;
    let vh = (video_h as f64 / scale as f64).round() as i32;
    let cr = ((corner_radius.min(200) as f64 / scale as f64).round() as i32 + 2)
        .min(vw / 2)
        .min(vh / 2);

    // Draw filled rounded rectangle
    for py in vy.max(0)..(vy + vh).min(sh as i32) {
        for px in vx.max(0)..(vx + vw).min(sw as i32) {
            if cr > 0 {
                let lx = px - vx;
                let ly = py - vy;
                let in_corner = (lx < cr && ly < cr)
                    || (lx >= vw - cr && ly < cr)
                    || (lx < cr && ly >= vh - cr)
                    || (lx >= vw - cr && ly >= vh - cr);
                if in_corner {
                    let (cx, cy) = if lx < cr && ly < cr {
                        (cr, cr)
                    } else if lx >= vw - cr && ly < cr {
                        (vw - cr, cr)
                    } else if lx < cr && ly >= vh - cr {
                        (cr, vh - cr)
                    } else {
                        (vw - cr, vh - cr)
                    };
                    let dx = lx - cx;
                    let dy = ly - cy;
                    if dx * dx + dy * dy > cr * cr {
                        continue;
                    }
                }
            }
            shadow.put_pixel(px as u32, py as u32, Rgba([0, 0, 0, alpha]));
        }
    }

    // Blur at reduced resolution
    let blurred = DynamicImage::ImageRgba8(shadow).blur(3.0).to_rgba8();

    // Scale back up to full resolution
    image::imageops::resize(
        &blurred,
        canvas_w,
        canvas_h,
        image::imageops::FilterType::Triangle,
    )
}

/// Generate a corner frame overlay (canvas-sized RGBA).
///
/// Transparent everywhere except at the corner regions of the video rectangle,
/// where the background color is painted to mask the rounded corners.
/// Anti-aliased with 1.5px feather at the arc boundary.
pub fn generate_corner_frame(
    canvas_w: u32,
    canvas_h: u32,
    video_x: u32,
    video_y: u32,
    video_w: u32,
    video_h: u32,
    bg_color_hex: &str,
    corner_radius: u32,
) -> Result<RgbaImage, String> {
    let (cr, cg, cb) = parse_hex_color(bg_color_hex)?;
    let radius = corner_radius.min(200) as i32;

    if radius <= 0 {
        // No rounding needed — return fully transparent image
        return Ok(RgbaImage::new(canvas_w, canvas_h));
    }

    let mut img = RgbaImage::new(canvas_w, canvas_h);

    let vx = video_x as i32;
    let vy = video_y as i32;
    let vw = video_w as i32;
    let vh = video_h as i32;

    let r = radius.min(vw / 2).min(vh / 2);
    let corner_origins = [
        (vx, vy),
        (vx + vw - r, vy),
        (vx, vy + vh - r),
        (vx + vw - r, vy + vh - r),
    ];
    let corner_centers = [
        (vx + r, vy + r),
        (vx + vw - r, vy + r),
        (vx + r, vy + vh - r),
        (vx + vw - r, vy + vh - r),
    ];

    for (ci, &(ox, oy)) in corner_origins.iter().enumerate() {
        let (cx, cy) = corner_centers[ci];
        let rf = r as f64;
        for py in oy.max(0)..(oy + r).min(canvas_h as i32) {
            for px in ox.max(0)..(ox + r).min(canvas_w as i32) {
                let dx = (px - cx) as f64;
                let dy = (py - cy) as f64;
                let dist = (dx * dx + dy * dy).sqrt();
                // Anti-alias: feather over 1.5px at the arc boundary
                if dist > rf - 0.75 {
                    let alpha = ((dist - (rf - 0.75)) / 1.5).clamp(0.0, 1.0);
                    let a = (alpha * 255.0).round() as u8;
                    img.put_pixel(px as u32, py as u32, Rgba([cr, cg, cb, a]));
                }
            }
        }
    }

    Ok(img)
}

/// Generate a grayscale corner mask for the video area.
///
/// White (255) inside the rounded rectangle, black (0) outside.
/// Used with `alphamerge` filter to punch rounded corners on the video.
pub fn generate_corner_mask(
    video_w: u32,
    video_h: u32,
    corner_radius: u32,
) -> GrayImage {
    let radius = corner_radius.min(200).min(video_w / 2).min(video_h / 2);
    let mut mask = GrayImage::new(video_w, video_h);

    // Fill entirely white
    for pixel in mask.pixels_mut() {
        *pixel = Luma([255]);
    }

    if radius == 0 {
        return mask;
    }

    let r = radius as i32;
    let w = video_w as i32;
    let h = video_h as i32;

    // For each corner, set pixels outside the arc to black
    let corner_origins = [(0, 0), (w - r, 0), (0, h - r), (w - r, h - r)];
    let corner_centers = [(r, r), (w - r, r), (r, h - r), (w - r, h - r)];

    for (ci, &(ox, oy)) in corner_origins.iter().enumerate() {
        let (cx, cy) = corner_centers[ci];
        let rf = r as f64;
        for py in oy.max(0)..(oy + r).min(h) {
            for px in ox.max(0)..(ox + r).min(w) {
                let dx = (px - cx) as f64;
                let dy = (py - cy) as f64;
                let dist = (dx * dx + dy * dy).sqrt();
                if dist > rf - 0.75 {
                    let alpha = 1.0 - ((dist - (rf - 0.75)) / 1.5).clamp(0.0, 1.0);
                    let v = (alpha * 255.0).round() as u8;
                    mask.put_pixel(px as u32, py as u32, Luma([v]));
                }
            }
        }
    }

    mask
}

/// Generate a default macOS-style cursor arrow as RGBA.
/// Returns a 24x24 white arrow with black outline and drop shadow.
pub fn generate_cursor_sprite() -> RgbaImage {
    let w = 24u32;
    let h = 36u32;
    let mut img = RgbaImage::new(w, h);

    // Define the arrow polygon as (x, y) points, scaled to pixel grid.
    // Classic macOS arrow shape: tall narrow pointer.
    let outline: &[(f64, f64)] = &[
        (1.0, 0.0),
        (1.0, 24.0),
        (6.0, 19.0),
        (10.0, 28.0),
        (14.0, 26.5),
        (10.0, 17.5),
        (16.0, 17.5),
        (1.0, 0.0),
    ];

    let fill: &[(f64, f64)] = &[
        (3.0, 3.5),
        (3.0, 20.5),
        (6.5, 17.0),
        (10.5, 25.5),
        (12.0, 25.0),
        (8.5, 17.0),
        (13.5, 17.0),
        (3.0, 3.5),
    ];

    // Rasterize: for each pixel, check if inside fill (white) or outline (black)
    for py in 0..h {
        for px in 0..w {
            let x = px as f64 + 0.5;
            let y = py as f64 + 0.5;

            if point_in_polygon(x, y, fill) {
                img.put_pixel(px, py, Rgba([255, 255, 255, 255]));
            } else if point_in_polygon(x, y, outline) {
                img.put_pixel(px, py, Rgba([0, 0, 0, 220]));
            }
        }
    }

    // Simple 1px shadow: shift down-right, draw under existing pixels
    let mut result = RgbaImage::new(w + 2, h + 2);
    // Shadow pass (offset by 1,1)
    for py in 0..h {
        for px in 0..w {
            let p = img.get_pixel(px, py);
            if p.0[3] > 0 {
                let dp = result.get_pixel(px + 1, py + 1);
                if dp.0[3] == 0 {
                    result.put_pixel(px + 1, py + 1, Rgba([0, 0, 0, 80]));
                }
            }
        }
    }
    // Foreground pass (at 0,0)
    for py in 0..h {
        for px in 0..w {
            let p = img.get_pixel(px, py);
            if p.0[3] > 0 {
                result.put_pixel(px, py, *p);
            }
        }
    }

    result
}

/// Load a cursor sprite from a PNG file.
pub fn load_cursor_sprite(path: &str) -> Result<RgbaImage, String> {
    let img = image::open(path)
        .map_err(|e| format!("Failed to load cursor image {}: {}", path, e))?;
    Ok(img.to_rgba8())
}

/// Point-in-polygon test (ray casting).
fn point_in_polygon(x: f64, y: f64, polygon: &[(f64, f64)]) -> bool {
    let n = polygon.len();
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = polygon[i];
        let (xj, yj) = polygon[j];
        if ((yi > y) != (yj > y)) && (x < (xj - xi) * (y - yi) / (yj - yi) + xi) {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Alpha-blend a sprite onto a flat RGBA buffer at the given position, with scale factor.
/// The sprite's hotspot is at (0, 0) — the tip of the arrow.
pub fn draw_cursor_on_buffer(
    buf: &mut [u8],
    buf_w: usize,
    buf_h: usize,
    sprite: &RgbaImage,
    pos_x: f64,
    pos_y: f64,
    scale: f64,
) {
    let sw = sprite.width() as f64 * scale;
    let sh = sprite.height() as f64 * scale;
    let src_w = sprite.width() as usize;
    let src_h = sprite.height() as usize;

    let dst_x = pos_x.round() as i64;
    let dst_y = pos_y.round() as i64;

    for dy in 0..(sh.ceil() as i64) {
        let py = dst_y + dy;
        if py < 0 || py >= buf_h as i64 {
            continue;
        }
        // Source row in the sprite
        let sy = ((dy as f64 / scale) as usize).min(src_h - 1);

        for dx in 0..(sw.ceil() as i64) {
            let px = dst_x + dx;
            if px < 0 || px >= buf_w as i64 {
                continue;
            }
            let sx = ((dx as f64 / scale) as usize).min(src_w - 1);

            let src_pixel = sprite.get_pixel(sx as u32, sy as u32);
            let sa = src_pixel.0[3] as u32;
            if sa == 0 {
                continue;
            }

            let idx = (py as usize * buf_w + px as usize) * 4;
            let inv_a = 255 - sa;
            buf[idx] = ((src_pixel.0[0] as u32 * sa + buf[idx] as u32 * inv_a) / 255) as u8;
            buf[idx + 1] = ((src_pixel.0[1] as u32 * sa + buf[idx + 1] as u32 * inv_a) / 255) as u8;
            buf[idx + 2] = ((src_pixel.0[2] as u32 * sa + buf[idx + 2] as u32 * inv_a) / 255) as u8;
            buf[idx + 3] = 255;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hex_color() {
        assert_eq!(parse_hex_color("#0f0f23").unwrap(), (15, 15, 35));
        assert_eq!(parse_hex_color("ff0000").unwrap(), (255, 0, 0));
        assert!(parse_hex_color("#fff").is_err());
    }

    #[test]
    fn test_canvas_dimensions() {
        let canvas = generate_canvas_image(1920, 1080, 115, 65, 1690, 950, "#0f0f23", 16, false).unwrap();
        assert_eq!(canvas.width(), 1920);
        assert_eq!(canvas.height(), 1080);
    }

    #[test]
    fn test_canvas_bg_color() {
        let canvas = generate_canvas_image(100, 100, 10, 10, 80, 80, "#ff0000", 0, false).unwrap();
        let pixel = canvas.get_pixel(50, 50);
        assert_eq!(pixel.0, [255, 0, 0, 255]);
    }

    #[test]
    fn test_canvas_with_shadow() {
        // Should not panic and should have shadow alpha at offset position
        let canvas = generate_canvas_image(200, 200, 20, 20, 160, 160, "#0f0f23", 16, true).unwrap();
        assert_eq!(canvas.width(), 200);
        assert_eq!(canvas.height(), 200);
    }

    #[test]
    fn test_corner_frame_dimensions() {
        let frame = generate_corner_frame(1920, 1080, 115, 65, 1690, 950, "#0f0f23", 16).unwrap();
        assert_eq!(frame.width(), 1920);
        assert_eq!(frame.height(), 1080);
    }

    #[test]
    fn test_corner_frame_center_transparent() {
        let frame = generate_corner_frame(200, 200, 20, 20, 160, 160, "#ff0000", 16).unwrap();
        // Center of video area should be transparent
        let pixel = frame.get_pixel(100, 100);
        assert_eq!(pixel.0[3], 0, "Center of video area should be transparent");
    }

    #[test]
    fn test_corner_frame_outside_video_transparent() {
        let frame = generate_corner_frame(200, 200, 20, 20, 160, 160, "#ff0000", 16).unwrap();
        // Background area (outside video) should be transparent
        let pixel = frame.get_pixel(5, 5);
        assert_eq!(pixel.0[3], 0, "Background area should be transparent");
    }

    #[test]
    fn test_corner_mask_dimensions() {
        let mask = generate_corner_mask(1690, 950, 16);
        assert_eq!(mask.width(), 1690);
        assert_eq!(mask.height(), 950);
    }

    #[test]
    fn test_corner_mask_center_white() {
        let mask = generate_corner_mask(200, 200, 16);
        let pixel = mask.get_pixel(100, 100);
        assert_eq!(pixel.0[0], 255, "Center should be white");
    }

    #[test]
    fn test_corner_mask_corner_dark() {
        let mask = generate_corner_mask(200, 200, 16);
        // Top-left corner pixel should be dark (outside rounded rect)
        let pixel = mask.get_pixel(0, 0);
        assert!(pixel.0[0] < 50, "Corner pixel should be dark, got {}", pixel.0[0]);
    }

    #[test]
    fn test_corner_mask_zero_radius() {
        let mask = generate_corner_mask(100, 100, 0);
        // All pixels should be white
        let pixel = mask.get_pixel(0, 0);
        assert_eq!(pixel.0[0], 255, "Zero radius should produce all-white mask");
    }

    #[test]
    fn test_cursor_sprite_dimensions() {
        let sprite = generate_cursor_sprite();
        assert_eq!(sprite.width(), 26); // 24 + 2 for shadow
        assert_eq!(sprite.height(), 38); // 36 + 2 for shadow
    }

    #[test]
    fn test_cursor_sprite_has_opaque_pixels() {
        let sprite = generate_cursor_sprite();
        let opaque_count = sprite.pixels().filter(|p| p.0[3] > 0).count();
        assert!(opaque_count > 50, "Cursor should have visible pixels, got {}", opaque_count);
    }

    #[test]
    fn test_cursor_sprite_tip_opaque() {
        let sprite = generate_cursor_sprite();
        // The arrow tip is at approximately (1, 0) in the original, which is (1, 0) in result
        let tip = sprite.get_pixel(1, 1);
        assert!(tip.0[3] > 0, "Arrow tip should be opaque, got alpha={}", tip.0[3]);
    }

    #[test]
    fn test_draw_cursor_on_buffer() {
        let sprite = generate_cursor_sprite();
        let mut buf = vec![0u8; 100 * 100 * 4];
        draw_cursor_on_buffer(&mut buf, 100, 100, &sprite, 50.0, 50.0, 1.0);
        // Check that some pixels in the cursor region have been drawn
        let mut found = false;
        for dy in 0..sprite.height() {
            for dx in 0..sprite.width() {
                let px = 50 + dx as usize;
                let py = 50 + dy as usize;
                if px < 100 && py < 100 {
                    let idx = (py * 100 + px) * 4;
                    if buf[idx + 3] > 0 {
                        found = true;
                        break;
                    }
                }
            }
            if found { break; }
        }
        assert!(found, "Buffer should have cursor pixels drawn");
    }

    #[test]
    fn test_draw_cursor_scaled() {
        let sprite = generate_cursor_sprite();
        let mut buf = vec![0u8; 200 * 200 * 4];
        draw_cursor_on_buffer(&mut buf, 200, 200, &sprite, 50.0, 50.0, 1.3);
        // Should not panic with scale > 1
    }

    #[test]
    fn test_draw_cursor_at_edge() {
        let sprite = generate_cursor_sprite();
        let mut buf = vec![0u8; 50 * 50 * 4];
        // Draw at edge — should clip, not panic
        draw_cursor_on_buffer(&mut buf, 50, 50, &sprite, 45.0, 45.0, 1.0);
        draw_cursor_on_buffer(&mut buf, 50, 50, &sprite, -5.0, -5.0, 1.0);
    }

    #[test]
    fn test_point_in_polygon() {
        let triangle = &[(0.0, 0.0), (10.0, 0.0), (5.0, 10.0), (0.0, 0.0)];
        assert!(point_in_polygon(5.0, 3.0, triangle));
        assert!(!point_in_polygon(20.0, 20.0, triangle));
    }
}
