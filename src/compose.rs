//! Core composition pipeline using libav* (ffmpeg-the-third).
//!
//! Approach: decode → software scale to RGBA → composite in Rust → convert to YUV420P → encode
//!
//! This avoids FFmpeg filter graph complexity by doing the actual composition
//! (background, overlay, corners, shadow) in Rust using pre-baked RGBA images.

use crate::assets;
use crate::cursor::{self, SmoothedFrame};
use crate::motion_blur;
use crate::zoom::{self, ZoomSegment};
use crate::{CompositionLayout, ExportArgs};

use ffmpeg_the_third as ffmpeg;
use ffmpeg::format::Pixel;
use ffmpeg::software::scaling;

/// Run the full composition pipeline.
pub fn run(args: &ExportArgs, zoom_segments: Option<&[ZoomSegment]>) -> Result<(), String> {
    ffmpeg::init().map_err(|e| format!("FFmpeg init failed: {}", e))?;

    // ── Open input ──
    let mut ictx = ffmpeg::format::input(&args.input)
        .map_err(|e| format!("Failed to open input: {}", e))?;

    let video_stream_index = ictx
        .streams()
        .best(ffmpeg::media::Type::Video)
        .ok_or("No video stream found")?
        .index();

    let audio_stream_index = ictx
        .streams()
        .best(ffmpeg::media::Type::Audio)
        .map(|s| s.index());

    // Get stream info and create decoder
    let (input_time_base, input_frame_rate, mut decoder) = {
        let stream = ictx.stream(video_stream_index).unwrap();
        let tb = stream.time_base();
        let fr = stream.avg_frame_rate();
        let params = stream.parameters();
        let dec = ffmpeg::codec::Context::from_parameters(params)
            .map_err(|e| format!("Failed to create decoder: {}", e))?
            .decoder()
            .video()
            .map_err(|e| format!("Failed to open decoder: {}", e))?;
        (tb, fr, dec)
    };

    let input_w = decoder.width();
    let input_h = decoder.height();
    let input_pix_fmt = decoder.format();

    if input_w == 0 || input_h == 0 {
        return Err("Input video has zero dimensions".into());
    }

    eprintln!("Input: {}x{}, pix_fmt: {:?}", input_w, input_h, input_pix_fmt);

    // ── Layout ──
    let layout = CompositionLayout::from_input(input_w, input_h, args.padding);
    eprintln!(
        "Layout: canvas {}x{}, video {}x{} at ({}, {})",
        layout.canvas_w, layout.canvas_h, layout.video_w, layout.video_h, layout.video_x, layout.video_y,
    );

    // ── Generate pre-baked assets as RGBA ──
    eprintln!("Generating composition assets...");
    let bg_color = args.effective_bg_color();

    let canvas_rgba = assets::generate_canvas_image(
        layout.canvas_w, layout.canvas_h,
        layout.video_x, layout.video_y,
        layout.video_w, layout.video_h,
        bg_color, args.corner_radius, args.shadow,
    )?;

    let corner_frame_rgba = assets::generate_corner_frame(
        layout.canvas_w, layout.canvas_h,
        layout.video_x, layout.video_y,
        layout.video_w, layout.video_h,
        bg_color, args.corner_radius,
    )?;

    // ── Zoom ──
    let has_zoom = zoom_segments.map_or(false, |s| !s.is_empty());
    if has_zoom {
        eprintln!("Zoom: {} segments, easing: {}", zoom_segments.unwrap().len(), args.zoom_easing);
    }

    // ── Cursor ──
    let smoothed_cursor: Option<Vec<SmoothedFrame>> = if let Some(ref path) = args.cursor_file {
        let fps = input_frame_rate.0 as f64 / input_frame_rate.1 as f64;
        match cursor::load_and_smooth(path, &args.cursor_physics, fps) {
            Ok(frames) => {
                eprintln!("Cursor: {} smoothed frames, preset: {}", frames.len(), args.cursor_physics);
                Some(frames)
            }
            Err(e) => {
                eprintln!("WARNING: Failed to load cursor: {}", e);
                None
            }
        }
    } else {
        None
    };
    let has_cursor = smoothed_cursor.is_some();

    // Load or generate cursor sprite
    let cursor_sprite = if has_cursor {
        if let Some(ref path) = args.cursor_image {
            match assets::load_cursor_sprite(path) {
                Ok(img) => {
                    eprintln!("Cursor sprite: {}x{} from {}", img.width(), img.height(), path);
                    Some(img)
                }
                Err(e) => {
                    eprintln!("WARNING: {}, using default cursor", e);
                    Some(assets::generate_cursor_sprite())
                }
            }
        } else {
            eprintln!("Cursor sprite: default arrow");
            Some(assets::generate_cursor_sprite())
        }
    } else {
        None
    };

    // ── Motion blur ──
    let has_motion_blur = args.motion_blur > 0.0;

    // ── Scalers ──
    // Decode to RGBA for composition
    let mut decode_to_rgba = scaling::Context::get(
        input_pix_fmt, input_w, input_h,
        Pixel::RGBA, layout.video_w, layout.video_h,
        scaling::Flags::BILINEAR,
    ).map_err(|e| format!("Decode scaler failed: {}", e))?;

    // RGBA canvas to YUV420P for encoding
    let mut rgba_to_yuv = scaling::Context::get(
        Pixel::RGBA, layout.canvas_w, layout.canvas_h,
        Pixel::YUV420P, layout.canvas_w, layout.canvas_h,
        scaling::Flags::BILINEAR,
    ).map_err(|e| format!("Output scaler failed: {}", e))?;

    // ── Encoder ──
    let encoder_codec = ffmpeg::encoder::find_by_name("libx264")
        .ok_or("libx264 encoder not found")?;

    let mut octx = ffmpeg::format::output(&args.output)
        .map_err(|e| format!("Failed to create output: {}", e))?;

    let global_header = octx.format().flags().contains(ffmpeg::format::Flags::GLOBAL_HEADER);

    let mut video_stream_out = octx.add_stream(encoder_codec)
        .map_err(|e| format!("Failed to add output stream: {}", e))?;

    let enc_params = args.encoder_params();
    let mut encoder = ffmpeg::codec::Context::new_with_codec(encoder_codec)
        .encoder()
        .video()
        .map_err(|e| format!("Failed to create encoder: {}", e))?;

    encoder.set_width(layout.canvas_w);
    encoder.set_height(layout.canvas_h);
    encoder.set_format(Pixel::YUV420P);
    encoder.set_time_base(input_time_base);
    encoder.set_frame_rate(Some(input_frame_rate));

    let mut opts = ffmpeg::Dictionary::new();
    opts.set("preset", &enc_params.preset);
    opts.set("crf", &enc_params.crf.to_string());

    if global_header {
        unsafe {
            (*encoder.as_mut_ptr()).flags |= ffmpeg::codec::Flags::GLOBAL_HEADER.bits() as i32;
        }
    }

    let mut encoder = encoder.open_with(opts)
        .map_err(|e| format!("Failed to open encoder: {}", e))?;

    unsafe {
        ffmpeg::ffi::avcodec_parameters_from_context(
            (*video_stream_out.as_mut_ptr()).codecpar,
            encoder.as_ptr(),
        );
    }

    // Audio stream (copy codec parameters from input)
    if let Some(audio_idx) = audio_stream_index {
        let audio_stream_in = ictx.stream(audio_idx).unwrap();
        let mut audio_out = octx.add_stream(ffmpeg::encoder::find(ffmpeg::codec::Id::None))
            .map_err(|e| format!("Failed to add audio stream: {}", e))?;
        audio_out.set_parameters(audio_stream_in.parameters());
    }

    octx.write_header()
        .map_err(|e| format!("Failed to write header: {}", e))?;

    // ── Pre-allocate frames (outside loop) ──
    let mut decoded_frame  = ffmpeg::frame::Video::empty();
    let mut rgba_video     = ffmpeg::frame::Video::new(Pixel::RGBA, layout.video_w, layout.video_h);
    let mut rgba_frame     = ffmpeg::frame::Video::new(Pixel::RGBA, layout.canvas_w, layout.canvas_h);
    let mut yuv_frame      = ffmpeg::frame::Video::new(Pixel::YUV420P, layout.canvas_w, layout.canvas_h);
    let mut packet_out     = ffmpeg::Packet::empty();

    let canvas_bytes = canvas_rgba.as_raw();
    let corner_bytes = corner_frame_rgba.as_raw();
    let cw = layout.canvas_w as usize;
    let ch = layout.canvas_h as usize;
    let vx = layout.video_x as usize;
    let vy = layout.video_y as usize;
    let vw = layout.video_w as usize;
    let vh = layout.video_h as usize;

    eprintln!("Encoding {} frames...", "?");
    let mut frame_count: u64 = 0;

    // ── Composition buffer (RGBA, canvas-sized, packed — no stride padding) ──
    let mut comp_buf: Vec<u8> = vec![0u8; cw * ch * 4];

    // ── Blur scratch buffer — used as apply() output to avoid aliasing ──
    let mut blur_scratch: Vec<u8> = if has_motion_blur {
        vec![0u8; cw * ch * 4]
    } else {
        Vec::new()
    };

    // ── Zoom output buffer — packed RGBA, canvas-sized ──
    let mut zoom_buf: Vec<u8> = if has_zoom { vec![0u8; cw * ch * 4] } else { Vec::new() };

    let mut prev_transform: Option<motion_blur::CameraTransform> = None;

    for result in ictx.packets() {
        let (stream, packet) = result.map_err(|e| format!("Read error: {}", e))?;
        // Copy audio packets directly
        if Some(stream.index()) == audio_stream_index {
            let audio_out_idx = 1usize; // audio is stream 1 (video is 0)
            let mut pkt = packet.clone();
            let in_tb = stream.time_base();
            let out_tb = octx.stream(audio_out_idx).unwrap().time_base();
            pkt.set_stream(audio_out_idx);
            pkt.rescale_ts(in_tb, out_tb);
            pkt.write_interleaved(&mut octx).ok(); // best-effort audio copy
            continue;
        }

        if stream.index() != video_stream_index {
            continue;
        }

        decoder.send_packet(&packet)
            .map_err(|e| format!("Decoder send failed: {}", e))?;

        while decoder.receive_frame(&mut decoded_frame).is_ok() {
            let pts = decoded_frame.pts();

            // Step 1: Scale decoded frame to video_w × video_h in RGBA
            decode_to_rgba.run(&decoded_frame, &mut rgba_video)
                .map_err(|e| format!("Scale to RGBA failed: {}", e))?;

            // Step 2: Composite into comp_buf (packed RGBA, canvas-sized)
            comp_buf.copy_from_slice(canvas_bytes);

            let video_stride = rgba_video.stride(0);
            let video_data = rgba_video.data(0);
            for row in 0..vh {
                let dst_offset = ((vy + row) * cw + vx) * 4;
                let src_offset = row * video_stride;
                comp_buf[dst_offset..dst_offset + vw * 4]
                    .copy_from_slice(&video_data[src_offset..src_offset + vw * 4]);
            }

            // Alpha-blend corner frame
            for i in 0..cw * ch {
                let ca = corner_bytes[i * 4 + 3] as u32;
                if ca > 0 {
                    let inv_a = 255 - ca;
                    let idx = i * 4;
                    comp_buf[idx]     = ((corner_bytes[idx]     as u32 * ca + comp_buf[idx]     as u32 * inv_a) / 255) as u8;
                    comp_buf[idx + 1] = ((corner_bytes[idx + 1] as u32 * ca + comp_buf[idx + 1] as u32 * inv_a) / 255) as u8;
                    comp_buf[idx + 2] = ((corner_bytes[idx + 2] as u32 * ca + comp_buf[idx + 2] as u32 * inv_a) / 255) as u8;
                    comp_buf[idx + 3] = 255;
                }
            }

            // Step 2b: Draw cursor sprite
            if has_cursor {
                let time_secs = pts.unwrap_or(0) as f64
                    * input_time_base.0 as f64
                    / input_time_base.1 as f64;

                if let Some(frame) = cursor::frame_at_time(smoothed_cursor.as_deref().unwrap(), time_secs) {
                    let cursor_click_only = args.cursor_display == "click";
                    let should_draw = !cursor_click_only || frame.is_clicking;

                    if should_draw {
                        if let Some(ref sprite) = cursor_sprite {
                            let cursor_canvas_x = layout.video_x as f64
                                + (frame.x / input_w as f64) * layout.video_w as f64;
                            let cursor_canvas_y = layout.video_y as f64
                                + (frame.y / input_h as f64) * layout.video_h as f64;

                            assets::draw_cursor_on_buffer(
                                &mut comp_buf,
                                cw, ch,
                                sprite,
                                cursor_canvas_x,
                                cursor_canvas_y,
                                frame.click_scale,
                            );
                        }
                    }
                }
            }

            // Step 3: Build rgba_frame for encoding.
            //
            // When zooming, the scaler writes directly into rgba_frame — skip
            // the plain comp_buf copy so it isn't immediately overwritten.
            let time_secs = pts.unwrap_or(0) as f64
                * input_time_base.0 as f64
                / input_time_base.1 as f64;

            let (zoom_level, focus_x, focus_y) = if has_zoom {
                zoom::compute_zoom_at_time(
                    zoom_segments.unwrap(), time_secs,
                    layout.canvas_w as f64, layout.canvas_h as f64,
                    layout.video_x as f64, layout.video_y as f64,
                    layout.video_w as f64, layout.video_h as f64,
                    &args.zoom_easing,
                    args.zoom_in_duration, args.zoom_out_duration,
                )
            } else {
                (1.0, layout.canvas_w as f64 / 2.0, layout.canvas_h as f64 / 2.0)
            };

            let zoomed = zoom_level > 1.01;

            // Step 4: Zoom — direct Rayon-parallel bilinear blit from crop region
            // of comp_buf into zoom_buf.  No intermediate FFmpeg frame, no sws_scale.
            if zoomed {
                let (cx, cy, crop_w, crop_h) = zoom::compute_crop_rect(
                    zoom_level, focus_x, focus_y,
                    layout.canvas_w, layout.canvas_h,
                );
                zoom::blit_bilinear(
                    &comp_buf, cw,
                    cx as usize, cy as usize, crop_w as usize, crop_h as usize,
                    &mut zoom_buf, cw, ch,
                );
            }

            // post_zoom points to the packed RGBA buffer that has the current frame
            // (zoom_buf if we zoomed, comp_buf otherwise).
            let post_zoom: &[u8] = if zoomed { &zoom_buf } else { &comp_buf };

            // Step 4b: Motion blur (after zoom, before YUV conversion).
            let blurred = if has_motion_blur {
                let curr_transform = motion_blur::CameraTransform {
                    x: focus_x,
                    y: focus_y,
                    scale: zoom_level,
                };
                let fps = input_frame_rate.0 as f64 / input_frame_rate.1.max(1) as f64;
                let dt = if fps > 0.0 { 1.0 / fps } else { 1.0 / 30.0 };

                let did_blur = if let Some(ref prev) = prev_transform {
                    let (blur_vx, blur_vy, speed) = motion_blur::compute_camera_velocity(
                        prev, &curr_transform, dt, layout.canvas_w as f64,
                    );
                    let blur_px = motion_blur::compute_blur_radius(speed, args.motion_blur);
                    if blur_px >= 0.5 {
                        motion_blur::apply(
                            post_zoom, &mut blur_scratch,
                            cw as u32, ch as u32,
                            blur_vx, blur_vy, blur_px,
                        );
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };
                prev_transform = Some(curr_transform);
                did_blur
            } else {
                false
            };

            // Step 5 (prep): write final packed buffer → rgba_frame with stride.
            let output_buf: &[u8] = if blurred { &blur_scratch } else { post_zoom };
            {
                let frame_stride = rgba_frame.stride(0);
                let frame_data   = rgba_frame.data_mut(0);
                for row in 0..ch {
                    let src_off = row * cw * 4;
                    let dst_off = row * frame_stride;
                    frame_data[dst_off..dst_off + cw * 4]
                        .copy_from_slice(&output_buf[src_off..src_off + cw * 4]);
                }
            }

            // Step 5: Convert RGBA to YUV420P for encoder
            rgba_to_yuv.run(&rgba_frame, &mut yuv_frame)
                .map_err(|e| format!("RGBA to YUV conversion failed: {}", e))?;
            yuv_frame.set_pts(pts);

            // Step 6: Encode
            encoder.send_frame(&yuv_frame)
                .map_err(|e| format!("Encoder send failed: {}", e))?;

            while encoder.receive_packet(&mut packet_out).is_ok() {
                packet_out.set_stream(0);
                packet_out.rescale_ts(input_time_base, octx.stream(0).unwrap().time_base());
                packet_out.write_interleaved(&mut octx)
                    .map_err(|e| format!("Write packet failed: {}", e))?;
            }

            frame_count += 1;
            if frame_count % 100 == 0 {
                eprintln!("  {} frames", frame_count);
            }
        }
    }

    // Flush decoder
    decoder.send_eof().ok();
    while decoder.receive_frame(&mut decoded_frame).is_ok() {
        // Process remaining frames (simplified — no composition for flush frames)
        frame_count += 1;
    }

    // Flush encoder
    encoder.send_eof().ok();
    while encoder.receive_packet(&mut packet_out).is_ok() {
        packet_out.set_stream(0);
        packet_out.rescale_ts(input_time_base, octx.stream(0).unwrap().time_base());
        packet_out.write_interleaved(&mut octx).ok();
    }

    octx.write_trailer()
        .map_err(|e| format!("Failed to write trailer: {}", e))?;

    eprintln!("Done: {} frames encoded to {}", frame_count, args.output);
    Ok(())
}
