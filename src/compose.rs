//! Core composition pipeline using libav* (ffmpeg-the-third).
//!
//! 3-stage pipeline: decode thread → composite (main thread) → encode thread.
//!
//! The decode thread converts each frame to packed RGBA and queues it.
//! The main thread composites, zooms, and blurs, then calls `crate::yuv`
//! (Rayon-parallel) for the RGBA→YUV420P step — replacing the single-threaded
//! `sws_scale` path and eliminating the intermediate `rgba_frame` stride copy.
//! The encode thread receives ready-to-encode YUV data and runs x264 concurrently
//! with the next frame's composition.

use crate::assets;
use crate::cursor::{self, SmoothedFrame};
use crate::motion_blur;
use crate::yuv;
use crate::zoom::{self, ZoomSegment};
use crate::{CompositionLayout, ExportArgs};

use ffmpeg_the_third as ffmpeg;
use ffmpeg::format::Pixel;
use ffmpeg::software::scaling;
use std::sync::mpsc;

// ── Thread-safety wrappers ─────────────────────────────────────────────────
// SAFETY: each wrapper is moved to exactly one thread and never accessed
// concurrently from any other thread.  The underlying FFmpeg objects are
// not shared — only ownership is transferred.

struct SendInput(ffmpeg::format::context::Input);
unsafe impl Send for SendInput {}

struct SendDecoder(ffmpeg::codec::decoder::Video);
unsafe impl Send for SendDecoder {}

struct SendOutput(ffmpeg::format::context::Output);
unsafe impl Send for SendOutput {}

struct SendEncoder(ffmpeg::encoder::Video);
unsafe impl Send for SendEncoder {}

struct SendPacket(ffmpeg::Packet);
unsafe impl Send for SendPacket {}

// ── Pipeline messages ──────────────────────────────────────────────────────

enum DecMsg {
    /// Decoded video frame: packed RGBA, `video_w * video_h * 4` bytes.
    VideoFrame { pts: Option<i64>, data: Vec<u8> },
    /// Audio packet forwarded from the input, with its original time base.
    AudioPkt   { pkt: SendPacket, tb: (i32, i32) },
    Done,
}

enum EncMsg {
    /// Composited frame: packed YUV420P (Y plane then U then V).
    VideoFrame { pts: Option<i64>, yuv: Vec<u8> },
    /// Audio packet passthrough.
    AudioPkt   { pkt: SendPacket, tb: (i32, i32) },
    Done,
}

/// Bounded queue depth between pipeline stages (frames buffered in-flight).
const QUEUE_DEPTH: usize = 6;

/// Run the full composition pipeline.
pub fn run(args: &ExportArgs, zoom_segments: Option<&[ZoomSegment]>) -> Result<(), String> {
    ffmpeg::init().map_err(|e| format!("FFmpeg init failed: {}", e))?;

    // ── Open input ──────────────────────────────────────────────────────────
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

    let input_w     = decoder.width();
    let input_h     = decoder.height();
    let input_pix_fmt = decoder.format();

    if input_w == 0 || input_h == 0 {
        return Err("Input video has zero dimensions".into());
    }
    eprintln!("Input: {}x{}, pix_fmt: {:?}", input_w, input_h, input_pix_fmt);

    // ── Layout ──────────────────────────────────────────────────────────────
    let layout = CompositionLayout::from_input(input_w, input_h, args.padding);
    eprintln!(
        "Layout: canvas {}x{}, video {}x{} at ({}, {})",
        layout.canvas_w, layout.canvas_h,
        layout.video_w,  layout.video_h,
        layout.video_x,  layout.video_y,
    );

    // ── Pre-baked assets ─────────────────────────────────────────────────────
    eprintln!("Generating composition assets...");
    let bg_color = args.effective_bg_color();

    let canvas_rgba = assets::generate_canvas_image(
        layout.canvas_w, layout.canvas_h,
        layout.video_x,  layout.video_y,
        layout.video_w,  layout.video_h,
        bg_color, args.corner_radius, args.shadow,
    )?;
    let corner_frame_rgba = assets::generate_corner_frame(
        layout.canvas_w, layout.canvas_h,
        layout.video_x,  layout.video_y,
        layout.video_w,  layout.video_h,
        bg_color, args.corner_radius,
    )?;

    // ── Zoom / cursor / motion blur ──────────────────────────────────────────
    let has_zoom = zoom_segments.map_or(false, |s| !s.is_empty());
    if has_zoom {
        eprintln!("Zoom: {} segments, easing: {}", zoom_segments.unwrap().len(), args.zoom_easing);
    }

    let smoothed_cursor: Option<Vec<SmoothedFrame>> = if let Some(ref path) = args.cursor_file {
        let fps = input_frame_rate.0 as f64 / input_frame_rate.1 as f64;
        match cursor::load_and_smooth(path, &args.cursor_physics, fps) {
            Ok(frames) => {
                eprintln!("Cursor: {} smoothed frames", frames.len());
                Some(frames)
            }
            Err(e) => { eprintln!("WARNING: cursor: {}", e); None }
        }
    } else { None };
    let has_cursor = smoothed_cursor.is_some();

    let cursor_sprite = if has_cursor {
        if let Some(ref path) = args.cursor_image {
            match assets::load_cursor_sprite(path) {
                Ok(img) => { eprintln!("Cursor sprite from {}", path); Some(img) }
                Err(e)  => { eprintln!("WARNING: {}, using default", e); Some(assets::generate_cursor_sprite()) }
            }
        } else {
            eprintln!("Cursor sprite: default arrow");
            Some(assets::generate_cursor_sprite())
        }
    } else { None };

    let has_motion_blur = args.motion_blur > 0.0;

    // ── Encoder setup ────────────────────────────────────────────────────────
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

    let has_audio = audio_stream_index.is_some();
    if let Some(audio_idx) = audio_stream_index {
        let audio_stream_in  = ictx.stream(audio_idx).unwrap();
        let mut audio_out = octx.add_stream(ffmpeg::encoder::find(ffmpeg::codec::Id::None))
            .map_err(|e| format!("Failed to add audio stream: {}", e))?;
        audio_out.set_parameters(audio_stream_in.parameters());
    }

    octx.write_header()
        .map_err(|e| format!("Failed to write header: {}", e))?;

    // ── Channels ─────────────────────────────────────────────────────────────
    let (dec_tx, dec_rx) = mpsc::sync_channel::<DecMsg>(QUEUE_DEPTH);
    let (enc_tx, enc_rx) = mpsc::sync_channel::<EncMsg>(QUEUE_DEPTH);

    // ── Stage 1: Decode thread ────────────────────────────────────────────────
    // Owns: ictx, decoder, decode_to_rgba.
    // Produces packed RGBA video frames + forwards audio packets.
    let decode_thread = {
        let ictx_s    = SendInput(ictx);
        let decoder_s = SendDecoder(decoder);
        std::thread::spawn(move || -> Result<(), String> {
            let mut ictx          = ictx_s.0;
            let mut decoder       = decoder_s.0;
            let mut decode_to_rgba = scaling::Context::get(
                input_pix_fmt, input_w, input_h,
                Pixel::RGBA, layout.video_w, layout.video_h,
                scaling::Flags::BILINEAR,
            ).map_err(|e| format!("Decode scaler failed: {}", e))?;
            let vw = layout.video_w as usize;
            let vh = layout.video_h as usize;
            let mut decoded_frame = ffmpeg::frame::Video::empty();
            let mut rgba_video    = ffmpeg::frame::Video::new(Pixel::RGBA, layout.video_w, layout.video_h);

            for result in ictx.packets() {
                let (stream, packet) = result.map_err(|e| format!("Packet read: {}", e))?;

                if Some(stream.index()) == audio_stream_index {
                    let tb = (stream.time_base().0, stream.time_base().1);
                    let _ = dec_tx.send(DecMsg::AudioPkt { pkt: SendPacket(packet.clone()), tb });
                    continue;
                }
                if stream.index() != video_stream_index { continue; }

                decoder.send_packet(&packet)
                    .map_err(|e| format!("Decoder send: {}", e))?;

                while decoder.receive_frame(&mut decoded_frame).is_ok() {
                    let pts = decoded_frame.pts();
                    decode_to_rgba.run(&decoded_frame, &mut rgba_video)
                        .map_err(|e| format!("Decode scale: {}", e))?;

                    // Strip stride → packed RGBA
                    let stride = rgba_video.stride(0);
                    let src    = rgba_video.data(0);
                    let mut packed = vec![0u8; vw * vh * 4];
                    for row in 0..vh {
                        packed[row * vw * 4..(row + 1) * vw * 4]
                            .copy_from_slice(&src[row * stride..row * stride + vw * 4]);
                    }

                    if dec_tx.send(DecMsg::VideoFrame { pts, data: packed }).is_err() {
                        return Ok(()); // composite thread dropped; exit cleanly
                    }
                }
            }

            decoder.send_eof().ok();
            while decoder.receive_frame(&mut decoded_frame).is_ok() { /* drain */ }

            let _ = dec_tx.send(DecMsg::Done);
            Ok(())
        })
    };

    // ── Stage 3: Encode thread ────────────────────────────────────────────────
    // Owns: octx, encoder.
    // Receives packed YUV420P → fills AVFrame → encodes → writes.
    let encode_thread = {
        let octx_s    = SendOutput(octx);
        let encoder_s = SendEncoder(encoder);
        std::thread::spawn(move || -> Result<(), String> {
            let mut octx     = octx_s.0;
            let mut encoder  = encoder_s.0;
            let cw      = layout.canvas_w as usize;
            let ch      = layout.canvas_h as usize;
            let half_w  = cw / 2;
            let half_h  = ch / 2;
            let y_size  = cw * ch;
            let uv_size = half_w * half_h;
            let mut yuv_frame  = ffmpeg::frame::Video::new(Pixel::YUV420P, layout.canvas_w, layout.canvas_h);
            let mut packet_out = ffmpeg::Packet::empty();

            loop {
                match enc_rx.recv() {
                    Ok(EncMsg::VideoFrame { pts, yuv }) => {
                        // Fill YUV frame planes from packed data (handle stride).
                        {
                            let y_stride = yuv_frame.stride(0);
                            let y_plane  = yuv_frame.data_mut(0);
                            for row in 0..ch {
                                y_plane[row * y_stride..row * y_stride + cw]
                                    .copy_from_slice(&yuv[row * cw..(row + 1) * cw]);
                            }
                        }
                        {
                            let u_stride = yuv_frame.stride(1);
                            let u_plane  = yuv_frame.data_mut(1);
                            let u_base   = y_size;
                            for row in 0..half_h {
                                u_plane[row * u_stride..row * u_stride + half_w]
                                    .copy_from_slice(&yuv[u_base + row * half_w..u_base + (row + 1) * half_w]);
                            }
                        }
                        {
                            let v_stride = yuv_frame.stride(2);
                            let v_plane  = yuv_frame.data_mut(2);
                            let v_base   = y_size + uv_size;
                            for row in 0..half_h {
                                v_plane[row * v_stride..row * v_stride + half_w]
                                    .copy_from_slice(&yuv[v_base + row * half_w..v_base + (row + 1) * half_w]);
                            }
                        }

                        yuv_frame.set_pts(pts);
                        encoder.send_frame(&yuv_frame)
                            .map_err(|e| format!("Encode send: {}", e))?;

                        while encoder.receive_packet(&mut packet_out).is_ok() {
                            packet_out.set_stream(0);
                            packet_out.rescale_ts(input_time_base, octx.stream(0).unwrap().time_base());
                            packet_out.write_interleaved(&mut octx)
                                .map_err(|e| format!("Write packet: {}", e))?;
                        }
                    }

                    Ok(EncMsg::AudioPkt { mut pkt, tb }) => {
                        if has_audio {
                            let in_tb  = ffmpeg::Rational(tb.0, tb.1);
                            let out_tb = octx.stream(1).unwrap().time_base();
                            pkt.0.set_stream(1);
                            pkt.0.rescale_ts(in_tb, out_tb);
                            pkt.0.write_interleaved(&mut octx).ok();
                        }
                    }

                    Ok(EncMsg::Done) | Err(_) => {
                        encoder.send_eof().ok();
                        while encoder.receive_packet(&mut packet_out).is_ok() {
                            packet_out.set_stream(0);
                            packet_out.rescale_ts(input_time_base, octx.stream(0).unwrap().time_base());
                            packet_out.write_interleaved(&mut octx).ok();
                        }
                        octx.write_trailer()
                            .map_err(|e| format!("Write trailer: {}", e))?;
                        break;
                    }
                }
            }
            Ok(())
        })
    };

    // ── Stage 2: Composite (main thread) ─────────────────────────────────────
    let canvas_bytes = canvas_rgba.as_raw();
    let corner_bytes = corner_frame_rgba.as_raw();
    let cw = layout.canvas_w as usize;
    let ch = layout.canvas_h as usize;
    let vx = layout.video_x as usize;
    let vy = layout.video_y as usize;
    let vw = layout.video_w as usize;
    let vh = layout.video_h as usize;
    let yuv_size = cw * ch * 3 / 2;

    let mut comp_buf    = vec![0u8; cw * ch * 4];
    let mut blur_scratch: Vec<u8> = if has_motion_blur { vec![0u8; cw * ch * 4] } else { Vec::new() };
    let mut zoom_cache: Option<(u32, u32, ffmpeg::frame::Video, scaling::Context)> = None;
    // Pre-allocated RGBA canvas frame used as zoom scaler output (avoids per-frame heap allocation).
    let mut rgba_frame_zoom = ffmpeg::frame::Video::new(Pixel::RGBA, layout.canvas_w, layout.canvas_h);
    let mut yuv_out     = vec![0u8; yuv_size];
    let mut prev_transform: Option<motion_blur::CameraTransform> = None;
    let mut frame_count: u64 = 0;

    eprintln!("Encoding frames...");

    loop {
        let msg = dec_rx.recv().map_err(|_| "decode thread exited unexpectedly".to_string())?;
        match msg {
            DecMsg::AudioPkt { pkt, tb } => {
                enc_tx.send(EncMsg::AudioPkt { pkt, tb }).ok();
                continue;
            }
            DecMsg::Done => {
                enc_tx.send(EncMsg::Done).ok();
                break;
            }
            DecMsg::VideoFrame { pts, data: rgba_video_packed } => {
                // Step 1: Composite into comp_buf (packed RGBA, canvas-sized)
                comp_buf.copy_from_slice(canvas_bytes);

                for row in 0..vh {
                    let dst_offset = ((vy + row) * cw + vx) * 4;
                    let src_offset = row * vw * 4;
                    comp_buf[dst_offset..dst_offset + vw * 4]
                        .copy_from_slice(&rgba_video_packed[src_offset..src_offset + vw * 4]);
                }

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

                if has_cursor {
                    let time_secs = pts.unwrap_or(0) as f64
                        * input_time_base.0 as f64
                        / input_time_base.1 as f64;
                    if let Some(frame) = cursor::frame_at_time(smoothed_cursor.as_deref().unwrap(), time_secs) {
                        let cursor_click_only = args.cursor_display == "click";
                        if !cursor_click_only || frame.is_clicking {
                            if let Some(ref sprite) = cursor_sprite {
                                let cx = layout.video_x as f64 + (frame.x / input_w as f64) * layout.video_w as f64;
                                let cy = layout.video_y as f64 + (frame.y / input_h as f64) * layout.video_h as f64;
                                assets::draw_cursor_on_buffer(&mut comp_buf, cw, ch, sprite, cx, cy, frame.click_scale);
                            }
                        }
                    }
                }

                // Step 2: Zoom
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

                if zoomed {
                    let (cx, cy, crop_w, crop_h) = zoom::compute_crop_rect(
                        zoom_level, focus_x, focus_y,
                        layout.canvas_w, layout.canvas_h,
                    );
                    let cache_valid = zoom_cache
                        .as_ref()
                        .map_or(false, |(ccw, cch, _, _)| *ccw == crop_w && *cch == crop_h);
                    if !cache_valid {
                        let cropped_frame = ffmpeg::frame::Video::new(Pixel::RGBA, crop_w, crop_h);
                        let scaler = scaling::Context::get(
                            Pixel::RGBA, crop_w, crop_h,
                            Pixel::RGBA, layout.canvas_w, layout.canvas_h,
                            scaling::Flags::BILINEAR,
                        ).map_err(|e| format!("Zoom scaler: {}", e))?;
                        zoom_cache = Some((crop_w, crop_h, cropped_frame, scaler));
                    }
                    let entry = zoom_cache.as_mut().unwrap();
                    {
                        let cropped_stride = entry.2.stride(0);
                        let cropped_data   = entry.2.data_mut(0);
                        for row in 0..crop_h as usize {
                            let src_off = ((cy as usize + row) * cw + cx as usize) * 4;
                            let dst_off = row * cropped_stride;
                            let len     = crop_w as usize * 4;
                            cropped_data[dst_off..dst_off + len]
                                .copy_from_slice(&comp_buf[src_off..src_off + len]);
                        }
                    }
                    entry.3.run(&entry.2, &mut rgba_frame_zoom)
                        .map_err(|e| format!("Zoom scale: {}", e))?;
                    // Strip stride back to comp_buf
                    let frame_stride = rgba_frame_zoom.stride(0);
                    let frame_data   = rgba_frame_zoom.data(0);
                    for row in 0..ch {
                        let src_off = row * frame_stride;
                        let dst_off = row * cw * 4;
                        comp_buf[dst_off..dst_off + cw * 4]
                            .copy_from_slice(&frame_data[src_off..src_off + cw * 4]);
                    }
                }

                // Step 3: Motion blur
                let blurred = if has_motion_blur {
                    let curr_transform = motion_blur::CameraTransform {
                        x: focus_x, y: focus_y, scale: zoom_level,
                    };
                    let fps = input_frame_rate.0 as f64 / input_frame_rate.1.max(1) as f64;
                    let dt  = if fps > 0.0 { 1.0 / fps } else { 1.0 / 30.0 };
                    let did_blur = if let Some(ref prev) = prev_transform {
                        let (bvx, bvy, speed) = motion_blur::compute_camera_velocity(
                            prev, &curr_transform, dt, layout.canvas_w as f64,
                        );
                        let blur_px = motion_blur::compute_blur_radius(speed, args.motion_blur);
                        if blur_px >= 0.5 {
                            motion_blur::apply(&comp_buf, &mut blur_scratch, cw as u32, ch as u32, bvx, bvy, blur_px);
                            true
                        } else { false }
                    } else { false };
                    prev_transform = Some(curr_transform);
                    did_blur
                } else { false };

                // Step 4: RGBA→YUV420P (Rayon-parallel, replaces sws_scale rgba_to_yuv)
                let src_buf: &[u8] = if blurred { &blur_scratch } else { &comp_buf };
                yuv::rgba_to_yuv420p(src_buf, cw, ch, &mut yuv_out);

                // Step 5: Send to encode thread
                if enc_tx.send(EncMsg::VideoFrame { pts, yuv: yuv_out.clone() }).is_err() {
                    break; // encode thread exited early
                }

                frame_count += 1;
                if frame_count % 100 == 0 {
                    eprintln!("  {} frames", frame_count);
                }
            }
        }
    }

    // ── Join threads ─────────────────────────────────────────────────────────
    decode_thread.join().unwrap()?;
    encode_thread.join().unwrap()?;

    eprintln!("Done: {} frames encoded to {}", frame_count, args.output);
    Ok(())
}
