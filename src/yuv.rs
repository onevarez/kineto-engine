//! Rayon-parallel RGBA → YUV420P colour-space conversion.
//!
//! Replaces the single-threaded `sws_scale` path for the canvas-to-encoder
//! step, spreading the work across all available cores.
//!
//! Coefficients: BT.601 limited range (Y 16–235, U/V 16–240), matching
//! libswscale's default output for H.264 / YUV420P streams.

use rayon::prelude::*;

/// Convert packed RGBA to YUV420P (planes stored contiguously in `out`).
///
/// * `rgba`   — packed RGBA input, `w * h * 4` bytes, no stride padding.
/// * `w`, `h` — canvas dimensions; both must be even.
/// * `out`    — Y plane (`w * h` bytes) followed by U then V
///              (each `(w/2) * (h/2)` bytes).  Total = `w * h * 3 / 2`.
pub fn rgba_to_yuv420p(rgba: &[u8], w: usize, h: usize, out: &mut [u8]) {
    debug_assert_eq!(w % 2, 0);
    debug_assert_eq!(h % 2, 0);

    let uv_size = (w / 2) * (h / 2);
    let (y_plane, rest) = out.split_at_mut(w * h);
    let (u_plane, v_plane) = rest.split_at_mut(uv_size);

    // ── Y plane: one value per pixel, parallel over rows ──────────────────
    y_plane
        .par_chunks_mut(w)
        .enumerate()
        .for_each(|(row, y_row)| {
            let base = row * w * 4;
            for x in 0..w {
                let s = base + x * 4;
                let r = rgba[s]     as i32;
                let g = rgba[s + 1] as i32;
                let b = rgba[s + 2] as i32;
                // Y = (66R + 129G + 25B + 128) >> 8 + 16
                y_row[x] = (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16) as u8;
            }
        });

    // ── Chroma planes: 2×2 averaged, parallel over row-pairs ──────────────
    let half_w = w / 2;
    let u_rows: Vec<&mut [u8]> = u_plane.chunks_mut(half_w).collect();
    let v_rows: Vec<&mut [u8]> = v_plane.chunks_mut(half_w).collect();

    u_rows
        .into_par_iter()
        .zip(v_rows.into_par_iter())
        .enumerate()
        .for_each(|(cy, (u_row, v_row))| {
            let row0  = cy * 2;
            let row1  = row0 + 1;
            let base0 = row0 * w * 4;
            let base1 = row1 * w * 4;
            for cx in 0..half_w {
                let x0  = cx * 2;
                let x1  = x0 + 1;
                let s00 = base0 + x0 * 4;
                let s01 = base0 + x1 * 4;
                let s10 = base1 + x0 * 4;
                let s11 = base1 + x1 * 4;
                let r = (rgba[s00]     as i32 + rgba[s01]     as i32
                       + rgba[s10]     as i32 + rgba[s11]     as i32) >> 2;
                let g = (rgba[s00 + 1] as i32 + rgba[s01 + 1] as i32
                       + rgba[s10 + 1] as i32 + rgba[s11 + 1] as i32) >> 2;
                let b = (rgba[s00 + 2] as i32 + rgba[s01 + 2] as i32
                       + rgba[s10 + 2] as i32 + rgba[s11 + 2] as i32) >> 2;
                // Cb = (-38R - 74G + 112B + 128) >> 8 + 128
                u_row[cx] = (((-38 * r -  74 * g + 112 * b + 128) >> 8) + 128) as u8;
                // Cr = (112R - 94G - 18B + 128) >> 8 + 128
                v_row[cx] = (((112 * r -  94 * g -  18 * b + 128) >> 8) + 128) as u8;
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_black() {
        let rgba = vec![0u8; 4 * 4 * 4];
        let mut out = vec![0u8; 4 * 4 * 3 / 2];
        rgba_to_yuv420p(&rgba, 4, 4, &mut out);
        assert_eq!(out[0], 16, "Y for black");
        assert_eq!(out[16], 128, "U for black");
        assert_eq!(out[20], 128, "V for black");
    }

    #[test]
    fn test_white() {
        let rgba = vec![255u8; 4 * 4 * 4];
        let mut out = vec![0u8; 4 * 4 * 3 / 2];
        rgba_to_yuv420p(&rgba, 4, 4, &mut out);
        // Y = (66*255+129*255+25*255+128)>>8+16 = (56100+128)>>8+16 = 219+16 = 235
        assert_eq!(out[0], 235, "Y for white");
    }

    #[test]
    fn test_size_contract() {
        let w = 8; let h = 8;
        let rgba = vec![128u8; w * h * 4];
        let mut out = vec![0u8; w * h * 3 / 2];
        rgba_to_yuv420p(&rgba, w, h, &mut out); // must not panic
    }
}
