//! Core composition pipeline using libav* (ffmpeg-the-third).
//!
//! Approach: decode → software scale to RGBA → composite in Rust → convert to YUV420P → encode
//!
//! This avoids FFmpeg filter graph complexity by doing the actual composition
//! (background, overlay, corners, shadow) in Rust using pre-baked RGBA images.

use crate::assets;
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

    // ── Pre-allocate frames ──
    let mut decoded_frame = ffmpeg::frame::Video::empty();
    let mut rgba_video = ffmpeg::frame::Video::new(Pixel::RGBA, layout.video_w, layout.video_h);
    let mut yuv_frame = ffmpeg::frame::Video::new(Pixel::YUV420P, layout.canvas_w, layout.canvas_h);
    let mut packet_out = ffmpeg::Packet::empty();

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

    // ── Composition buffer (RGBA, canvas-sized) ──
    let mut comp_buf: Vec<u8> = vec![0u8; cw * ch * 4];

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

            // Step 2: Composite in RGBA
            // Start with canvas background (includes shadow)
            comp_buf.copy_from_slice(canvas_bytes);

            // Overlay scaled video at (video_x, video_y)
            let video_stride = rgba_video.stride(0);
            let video_data = rgba_video.data(0);
            for row in 0..vh {
                let dst_offset = ((vy + row) * cw + vx) * 4;
                let src_offset = row * video_stride;
                comp_buf[dst_offset..dst_offset + vw * 4]
                    .copy_from_slice(&video_data[src_offset..src_offset + vw * 4]);
            }

            // Overlay corner frame (alpha blend)
            for i in 0..cw * ch {
                let ca = corner_bytes[i * 4 + 3] as u32;
                if ca > 0 {
                    let inv_a = 255 - ca;
                    let idx = i * 4;
                    comp_buf[idx] = ((corner_bytes[idx] as u32 * ca + comp_buf[idx] as u32 * inv_a) / 255) as u8;
                    comp_buf[idx + 1] = ((corner_bytes[idx + 1] as u32 * ca + comp_buf[idx + 1] as u32 * inv_a) / 255) as u8;
                    comp_buf[idx + 2] = ((corner_bytes[idx + 2] as u32 * ca + comp_buf[idx + 2] as u32 * inv_a) / 255) as u8;
                    comp_buf[idx + 3] = 255;
                }
            }

            // Step 3: Copy RGBA composition into an ffmpeg frame
            let mut rgba_frame = ffmpeg::frame::Video::new(Pixel::RGBA, layout.canvas_w, layout.canvas_h);
            let frame_stride = rgba_frame.stride(0);
            let frame_data = rgba_frame.data_mut(0);
            for row in 0..ch {
                let src_offset = row * cw * 4;
                let dst_offset = row * frame_stride;
                frame_data[dst_offset..dst_offset + cw * 4]
                    .copy_from_slice(&comp_buf[src_offset..src_offset + cw * 4]);
            }

            // Step 4: Apply zoom if needed — crop from RGBA comp_buf, scale back to canvas
            if has_zoom {
                let time_secs = pts.unwrap_or(0) as f64
                    * input_time_base.0 as f64
                    / input_time_base.1 as f64;

                let (zoom_level, focus_x, focus_y) = zoom::compute_zoom_at_time(
                    zoom_segments.unwrap(), time_secs,
                    layout.canvas_w as f64, layout.canvas_h as f64,
                    layout.video_x as f64, layout.video_y as f64,
                    layout.video_w as f64, layout.video_h as f64,
                    &args.zoom_easing,
                );

                if zoom_level > 1.01 {
                    let (cx, cy, crop_w, crop_h) = zoom::compute_crop_rect(
                        zoom_level, focus_x, focus_y,
                        layout.canvas_w, layout.canvas_h,
                    );

                    // Copy cropped region from comp_buf into a smaller RGBA frame
                    let mut cropped = ffmpeg::frame::Video::new(
                        Pixel::RGBA, crop_w, crop_h,
                    );
                    let cropped_stride = cropped.stride(0);
                    let cropped_data = cropped.data_mut(0);
                    for row in 0..crop_h as usize {
                        let src_off = ((cy as usize + row) * cw + cx as usize) * 4;
                        let dst_off = row * cropped_stride;
                        let len = crop_w as usize * 4;
                        cropped_data[dst_off..dst_off + len]
                            .copy_from_slice(&comp_buf[src_off..src_off + len]);
                    }

                    // Scale cropped region back to canvas size
                    let mut zoom_scaler = scaling::Context::get(
                        Pixel::RGBA, crop_w, crop_h,
                        Pixel::RGBA, layout.canvas_w, layout.canvas_h,
                        scaling::Flags::BILINEAR,
                    ).map_err(|e| format!("Zoom scaler failed: {}", e))?;

                    zoom_scaler.run(&cropped, &mut rgba_frame)
                        .map_err(|e| format!("Zoom scale failed: {}", e))?;
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
