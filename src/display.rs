//! The remote picture and its way into Casement's surfaces.
//!
//! Sessions draw into a private BGRX back buffer and mark what changed. The
//! display copies the changed rectangles into the current surface and sends
//! `frame` whenever no frame is in flight, so decoding never waits for
//! Casement and Casement always gets the latest picture.

use crate::host::Output;
use crate::protocol::{FromCasement, ToCasement};
use crate::surface::Surface;
use std::sync::Arc;

/// More rectangles than this are merged into their bounding box.
const MAX_RECTS: usize = 32;

pub struct Display {
    out: Arc<Output>,
    width: u32,
    height: u32,
    back: Vec<u8>,
    surface: Option<Surface>,
    /// A `surface_request` is outstanding; surfaces arriving meanwhile may be
    /// for an older size, so nothing is drawn until the answer comes.
    requested: bool,
    in_flight: bool,
    full: bool,
    rects: Vec<[u32; 4]>,
}

impl Display {
    pub fn new(out: Arc<Output>) -> Self {
        Display {
            out,
            width: 0,
            height: 0,
            back: Vec::new(),
            surface: None,
            requested: false,
            in_flight: false,
            full: false,
            rects: Vec::new(),
        }
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Sets the remote display's size, asking Casement for a matching surface
    /// when it changes. The back buffer is cleared to black.
    pub fn set_size(&mut self, width: u32, height: u32) {
        if (width, height) == (self.width, self.height) {
            return;
        }
        self.width = width;
        self.height = height;
        self.back = vec![0; width as usize * height as usize * 4];
        self.full = true;
        self.rects.clear();
        self.requested = true;
        self.out.send(&ToCasement::SurfaceRequest { width, height });
    }

    /// The back buffer, `width * 4` bytes per row, and its stride.
    pub fn buffer_mut(&mut self) -> (&mut [u8], usize) {
        let stride = self.width as usize * 4;
        (&mut self.back, stride)
    }

    /// Marks a rectangle as changed. It is clipped to the display.
    pub fn damage(&mut self, x: u32, y: u32, w: u32, h: u32) {
        let (x, y) = (x.min(self.width), y.min(self.height));
        let (w, h) = (w.min(self.width - x), h.min(self.height - y));
        if self.full || w == 0 || h == 0 {
            return;
        }
        if x == 0 && y == 0 && w == self.width && h == self.height {
            self.full = true;
            self.rects.clear();
            return;
        }
        self.rects.push([x, y, w, h]);
        if self.rects.len() > MAX_RECTS {
            let mut b = self.rects[0];
            for r in &self.rects[1..] {
                let (x0, y0) = (b[0].min(r[0]), b[1].min(r[1]));
                let x1 = (b[0] + b[2]).max(r[0] + r[2]);
                let y1 = (b[1] + b[3]).max(r[1] + r[3]);
                b = [x0, y0, x1 - x0, y1 - y0];
            }
            self.rects = vec![b];
        }
    }

    pub fn damage_all(&mut self) {
        self.full = true;
        self.rects.clear();
    }

    /// Handles `surface` and `frame_ack`; other messages are ignored.
    pub fn handle(&mut self, msg: &FromCasement) {
        match *msg {
            FromCasement::Surface {
                id,
                handle,
                width,
                height,
                stride,
                size,
            } => {
                // Unmap and close the old surface first.
                self.surface = None;
                self.in_flight = false;
                match Surface::open(id, handle, width, height, stride, size) {
                    Ok(s) => {
                        log::debug!("surface {id}: {width}x{height}");
                        if (width, height) != (self.width, self.height) {
                            log::warn!(
                                "surface {id} is {width}x{height}, the display {}x{}",
                                self.width,
                                self.height
                            );
                        }
                        self.surface = Some(s);
                        self.requested = false;
                        self.damage_all();
                        self.flush();
                    }
                    Err(e) => log::error!("can't map surface {id}: {e}"),
                }
            }
            FromCasement::FrameAck { surface }
                if self.surface.as_ref().is_some_and(|s| s.id == surface) =>
            {
                self.in_flight = false;
                self.flush();
            }
            _ => {}
        }
    }

    /// Copies what changed into the surface and sends `frame`, if a surface
    /// is free.
    pub fn flush(&mut self) {
        if self.in_flight || self.requested || (!self.full && self.rects.is_empty()) {
            return;
        }
        let Some(surface) = self.surface.as_mut() else {
            return;
        };
        let w = self.width.min(surface.width);
        let h = self.height.min(surface.height);
        let src_stride = self.width as usize * 4;
        let dst_stride = surface.stride as usize;
        let rects = if self.full {
            vec![[0, 0, w, h]]
        } else {
            std::mem::take(&mut self.rects)
        };
        let dst = surface.pixels_mut();
        let mut sent = Vec::with_capacity(rects.len());
        for [x, y, rw, rh] in rects {
            if x >= w || y >= h {
                continue;
            }
            let (rw, rh) = (rw.min(w - x), rh.min(h - y));
            let bytes = rw as usize * 4;
            for row in y..y + rh {
                let s = row as usize * src_stride + x as usize * 4;
                let d = row as usize * dst_stride + x as usize * 4;
                dst[d..d + bytes].copy_from_slice(&self.back[s..s + bytes]);
            }
            sent.push([x, y, rw, rh]);
        }
        let full = self.full;
        self.full = false;
        self.rects.clear();
        if sent.is_empty() {
            return;
        }
        self.in_flight = true;
        self.out.send(&ToCasement::Frame {
            surface: surface.id,
            rects: if full { Vec::new() } else { sent },
        });
    }
}
