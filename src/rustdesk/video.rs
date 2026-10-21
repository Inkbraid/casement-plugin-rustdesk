//! Video: decoding with RustDesk's `scrap` and drawing into the display.
//!
//! Frames are decoded on their own thread (as in RustDesk's
//! `start_video_thread`), so a slow decode never holds up input. The codecs
//! report whole pictures, so changed areas are found by comparing 64×64
//! tiles with the previous picture; only those are copied and reported.

use crate::display::Display;
use crate::host::Host;
use base::message_proto::{video_frame, VideoFrame};
use scrap::codec::Decoder;
use scrap::{CodecFormat, ImageFormat, ImageRgb, ImageTexture};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

const TILE: usize = 64;
/// Frames waiting to be decoded before the queue is dropped and a key frame
/// requested instead.
const MAX_QUEUED: usize = 90;
/// Decode failures in a row before a key frame is requested.
const MAX_FAILURES: usize = 3;

pub struct Video {
    tx: Sender<VideoFrame>,
    queued: Arc<AtomicUsize>,
    /// Dropping frames until the next key frame.
    skipping: bool,
}

impl Video {
    /// Starts the decoder thread. `refresh` asks the peer for a key frame.
    pub fn start(host: Arc<Host>, refresh: impl Fn() + Send + 'static) -> Self {
        let (tx, rx) = channel();
        let queued = Arc::new(AtomicUsize::new(0));
        let q = queued.clone();
        let spawned = std::thread::Builder::new()
            .name("video".into())
            .spawn(move || decode_loop(host, rx, q, refresh));
        if let Err(e) = spawned {
            log::error!("can't start the video thread: {e}");
        }
        Video {
            tx,
            queued,
            skipping: false,
        }
    }

    /// Queues a frame. Returns `true` when the queue overflowed and a key
    /// frame should be requested.
    pub fn push(&mut self, vf: VideoFrame) -> bool {
        let key = contains_key_frame(&vf);
        if self.skipping && !key {
            return false;
        }
        self.skipping = false;
        if !key && self.queued.load(Ordering::Relaxed) >= MAX_QUEUED {
            log::warn!("decoding is falling behind; waiting for a key frame");
            self.skipping = true;
            return true;
        }
        self.queued.fetch_add(1, Ordering::Relaxed);
        if self.tx.send(vf).is_err() {
            self.queued.fetch_sub(1, Ordering::Relaxed);
        }
        false
    }
}

fn contains_key_frame(vf: &VideoFrame) -> bool {
    use video_frame::Union::*;
    match &vf.union {
        Some(Vp8s(f) | Vp9s(f) | Av1s(f) | H264s(f) | H265s(f)) => f.frames.iter().any(|f| f.key),
        _ => false,
    }
}

fn decode_loop(
    host: Arc<Host>,
    rx: Receiver<VideoFrame>,
    queued: Arc<AtomicUsize>,
    refresh: impl Fn(),
) {
    let mut decoder: Option<Decoder> = None;
    // ARGB in libyuv's naming is B, G, R, A in memory: Casement's BGRX.
    let mut rgb = ImageRgb::new(ImageFormat::ARGB, 1);
    let mut texture = ImageTexture::default();
    let mut failures = 0;
    let mut first = true;
    while let Ok(vf) = rx.recv() {
        queued.fetch_sub(1, Ordering::Relaxed);
        let Some(frame) = vf.union.as_ref() else {
            continue;
        };
        let format = CodecFormat::from(frame);
        if decoder.as_ref().is_none_or(|d| d.format() != format) {
            log::info!("decoding {format:?}");
            decoder = Some(Decoder::new(format, None));
        }
        let Some(dec) = decoder.as_mut() else {
            continue;
        };
        let mut pixelbuffer = true;
        let mut chroma = None;
        match dec.handle_video_frame(frame, &mut rgb, &mut texture, &mut pixelbuffer, &mut chroma) {
            Ok(true) => {
                failures = 0;
                if first {
                    first = false;
                    log::info!(
                        "first picture: {}x{} ({format:?}, {chroma:?})",
                        rgb.w,
                        rgb.h
                    );
                }
                let mut d = host.display();
                blit(&mut d, &rgb);
                d.flush();
            }
            Ok(false) => {}
            Err(e) => {
                failures += 1;
                log::warn!("can't decode a {format:?} frame: {e}");
                if failures == MAX_FAILURES {
                    decoder = None;
                    refresh();
                }
            }
        }
    }
}

/// Copies the parts of `img` that differ from the back buffer and marks
/// them; a size change replaces the whole picture.
fn blit(d: &mut Display, img: &ImageRgb) {
    let (w, h) = (img.w, img.h);
    if w == 0 || h == 0 || img.raw.len() < w * h * 4 {
        return;
    }
    let src_stride = img.raw.len() / h;
    if d.size() != (w as u32, h as u32) {
        d.set_size(w as u32, h as u32);
        let (back, stride) = d.buffer_mut();
        for y in 0..h {
            back[y * stride..y * stride + w * 4]
                .copy_from_slice(&img.raw[y * src_stride..y * src_stride + w * 4]);
        }
        d.damage_all();
        return;
    }
    let mut rects = Vec::new();
    {
        let (back, stride) = d.buffer_mut();
        for ty in (0..h).step_by(TILE) {
            let th = TILE.min(h - ty);
            let mut run: Option<(usize, usize)> = None;
            for tx in (0..w).step_by(TILE) {
                let tw = TILE.min(w - tx);
                let mut changed = false;
                for y in ty..ty + th {
                    let s = &img.raw[y * src_stride + tx * 4..y * src_stride + (tx + tw) * 4];
                    let b = &mut back[y * stride + tx * 4..y * stride + (tx + tw) * 4];
                    if changed || s != &b[..] {
                        changed = true;
                        b.copy_from_slice(s);
                    }
                }
                run = match (changed, run) {
                    (true, Some((x0, _))) => Some((x0, tx + tw)),
                    (true, None) => Some((tx, tx + tw)),
                    (false, Some((x0, x1))) => {
                        rects.push([x0, ty, x1 - x0, th]);
                        None
                    }
                    (false, None) => None,
                };
            }
            if let Some((x0, x1)) = run {
                rects.push([x0, ty, x1 - x0, th]);
            }
        }
    }
    for [x, y, rw, rh] in rects {
        d.damage(x as u32, y as u32, rw as u32, rh as u32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::Output;

    fn image(w: usize, h: usize, f: impl Fn(usize, usize) -> u8) -> ImageRgb {
        let mut img = ImageRgb::new(ImageFormat::ARGB, 1);
        img.w = w;
        img.h = h;
        let f = &f;
        img.raw = (0..h)
            .flat_map(|y| (0..w).flat_map(move |x| [f(x, y); 4]))
            .collect();
        img
    }

    #[test]
    fn blit_reports_changed_tiles() {
        let out = Arc::new(Output::new(Box::new(std::io::sink())));
        let mut d = Display::new(out);
        blit(&mut d, &image(200, 100, |_, _| 1));
        assert_eq!(d.size(), (200, 100));
        assert_eq!(d.pending(), (true, vec![]));
        d.clear_pending();

        // One pixel in tile (1, 0) and one in tile (3, 1).
        blit(
            &mut d,
            &image(200, 100, |x, y| {
                if (x, y) == (70, 3) || (x, y) == (199, 99) {
                    2
                } else {
                    1
                }
            }),
        );
        assert_eq!(
            d.pending(),
            (false, vec![[64, 0, 64, 64], [192, 64, 8, 36]])
        );
        let (buf, stride) = d.buffer_mut();
        assert_eq!(buf[3 * stride + 70 * 4], 2);
        d.clear_pending();

        // Nothing changed: nothing to report.
        blit(
            &mut d,
            &image(200, 100, |x, y| {
                if (x, y) == (70, 3) || (x, y) == (199, 99) {
                    2
                } else {
                    1
                }
            }),
        );
        assert_eq!(d.pending(), (false, vec![]));
    }
}
