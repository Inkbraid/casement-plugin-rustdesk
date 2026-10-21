//! `rustdesk://test`: a synthetic session that draws a moving test pattern and
//! logs input, so Casement's side can be tested without any RustDesk server.
//!
//! The picture is eight colour bars over a grey ramp, a bouncing box (two
//! dirty rectangles per frame) and a cross at the last pointer position. The
//! pointer is echoed as `cursor_pos`, F1 switches between a cross-hair cursor
//! and the default arrow, and clipboard text is echoed back once, prefixed
//! with `echo: `.

use crate::address::TestOptions;
use crate::host::Host;
use crate::protocol::{Cursor, FromCasement, State, ToCasement};
use crate::session::Session;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

const BOX: u32 = 64;
const MARK: u32 = 9;
const F1: u32 = 0x3B;
const ECHO: &str = "echo: ";

/// The colour bars, BGRX.
pub const BARS: [[u8; 4]; 8] = [
    [0xC0, 0xC0, 0xC0, 0xFF],
    [0x00, 0xC0, 0xC0, 0xFF],
    [0xC0, 0xC0, 0x00, 0xFF],
    [0x00, 0xC0, 0x00, 0xFF],
    [0xC0, 0x00, 0xC0, 0xFF],
    [0x00, 0x00, 0xC0, 0xFF],
    [0xC0, 0x00, 0x00, 0xFF],
    [0x10, 0x10, 0x10, 0xFF],
];

/// The static background at `(x, y)` of a `w`×`h` picture, BGRX.
pub fn background(x: u32, y: u32, w: u32, h: u32) -> [u8; 4] {
    if y < h * 2 / 3 {
        BARS[(x * 8 / w.max(1)) as usize % 8]
    } else if x.is_multiple_of(64) || y.is_multiple_of(64) {
        [0x80, 0x80, 0x80, 0xFF]
    } else {
        let v = (x * 255 / w.max(1)) as u8;
        [v, v, v, 0xFF]
    }
}

/// Where the box starts in a `w`×`h` picture.
pub fn box_start(w: u32, h: u32) -> (u32, u32) {
    (w.saturating_sub(BOX) / 2, h.saturating_sub(BOX) / 2)
}

enum Event {
    Pointer(i32, i32),
    Stop,
}

pub struct TestSession {
    host: Arc<Host>,
    tx: Sender<Event>,
    thread: Option<std::thread::JoinHandle<()>>,
    custom_cursor: bool,
    last_echo: String,
}

pub fn start(host: Arc<Host>, opts: TestOptions, password: &str) -> Box<dyn Session> {
    if let Some(text) = &opts.fail {
        host.finish(State::Failed, text);
    }
    if let Some(p) = &opts.password {
        if p != password {
            host.finish(
                State::AuthFailed,
                if password.is_empty() {
                    "A password is needed."
                } else {
                    "Wrong password."
                },
            );
        }
    }
    log::info!(
        "test pattern {}x{} at {} fps",
        opts.width,
        opts.height,
        opts.fps
    );
    host.send(&ToCasement::state(State::Connected, ""));
    host.send(&ToCasement::Cursor(cross_cursor()));
    let (tx, rx) = mpsc::channel();
    let h = host.clone();
    let thread = std::thread::Builder::new()
        .name("test-pattern".into())
        .spawn(move || run(h, opts, rx))
        .ok();
    Box::new(TestSession {
        host,
        tx,
        thread,
        custom_cursor: true,
        last_echo: String::new(),
    })
}

impl Session for TestSession {
    fn input(&mut self, msg: &FromCasement) {
        match msg {
            FromCasement::Key { scancode, down } => {
                log::info!("key scancode={scancode:#x} down={down}");
                if *scancode == F1 && *down {
                    self.custom_cursor = !self.custom_cursor;
                    let c = if self.custom_cursor {
                        cross_cursor()
                    } else {
                        Cursor::default_arrow()
                    };
                    self.host.send(&ToCasement::Cursor(c));
                }
            }
            FromCasement::Pointer { x, y, buttons } => {
                log::info!("pointer x={x} y={y} buttons={buttons}");
                let _ = self.tx.send(Event::Pointer(*x, *y));
                self.host.send(&ToCasement::CursorPos {
                    x: *x,
                    y: *y,
                    visible: true,
                });
            }
            FromCasement::Wheel { delta } => log::info!("wheel delta={delta}"),
            FromCasement::Clipboard { text } => {
                log::info!(
                    "clipboard {} chars: {:?}",
                    text.chars().count(),
                    preview(text)
                );
                if !text.starts_with(ECHO) && *text != self.last_echo {
                    self.last_echo = text.clone();
                    self.host.send(&ToCasement::Clipboard {
                        text: format!("{ECHO}{text}"),
                    });
                }
            }
            other => log::info!("ignored {other:?}"),
        }
    }

    fn disconnect(&mut self) {
        log::info!("disconnect");
        let _ = self.tx.send(Event::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn preview(text: &str) -> String {
    let mut p: String = text.chars().take(80).collect();
    if p.len() < text.len() {
        p.push('…');
    }
    p
}

/// A 15×15 cross-hair: white with a black outline, hot spot in the middle.
fn cross_cursor() -> Cursor {
    const S: u32 = 15;
    let mut rgba = vec![0u8; (S * S * 4) as usize];
    for y in 0..S {
        for x in 0..S {
            let on_line = |a: u32| a.abs_diff(S / 2) <= 1;
            let (px, inner) = if on_line(x) || on_line(y) {
                (true, x == S / 2 || y == S / 2)
            } else {
                (false, false)
            };
            if px {
                let i = ((y * S + x) * 4) as usize;
                let v = if inner { 255 } else { 0 };
                rgba[i..i + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
    }
    Cursor::image(S, S, (S / 2) as i32, (S / 2) as i32, &rgba)
}

struct Picture {
    w: u32,
    h: u32,
    box_pos: (u32, u32),
    vel: (i32, i32),
    mark: Option<(u32, u32)>,
    frame: u32,
}

impl Picture {
    fn new(w: u32, h: u32) -> Self {
        Picture {
            w,
            h,
            box_pos: box_start(w, h),
            vel: (5, 3),
            mark: None,
            frame: 0,
        }
    }

    fn fill(
        buf: &mut [u8],
        stride: usize,
        (w, h): (u32, u32),
        rect: [u32; 4],
        px: impl Fn(u32, u32) -> [u8; 4],
    ) {
        let [x0, y0, rw, rh] = rect;
        for y in y0..(y0 + rh).min(h) {
            for x in x0..(x0 + rw).min(w) {
                let i = y as usize * stride + x as usize * 4;
                buf[i..i + 4].copy_from_slice(&px(x, y));
            }
        }
    }

    fn draw_all(&self, host: &Host) {
        let mut d = host.display();
        let size = d.size();
        let (buf, stride) = d.buffer_mut();
        Self::fill(buf, stride, size, [0, 0, size.0, size.1], |x, y| {
            background(x, y, size.0, size.1)
        });
        self.draw_box(buf, stride, size);
        d.damage_all();
        d.flush();
    }

    fn box_colour(&self) -> [u8; 4] {
        let t = (self.frame % 96) * 8;
        let v = if t < 384 {
            (t / 3) as u8
        } else {
            ((767 - t) / 3) as u8
        };
        [255 - v, v, 0xFF, 0xFF]
    }

    fn draw_box(&self, buf: &mut [u8], stride: usize, size: (u32, u32)) {
        let c = self.box_colour();
        Self::fill(
            buf,
            stride,
            size,
            [self.box_pos.0, self.box_pos.1, BOX, BOX],
            |_, _| c,
        );
    }

    fn step(&mut self, host: &Host, pointer: Option<(i32, i32)>) {
        self.frame = self.frame.wrapping_add(1);
        let (w, h) = (self.w, self.h);
        let mut d = host.display();
        let size = d.size();
        let (buf, stride) = d.buffer_mut();
        let bg = |x, y| background(x, y, w, h);
        let mut damage = Vec::new();

        // Erase the old box and mark, then move and redraw them.
        let old = self.box_pos;
        Self::fill(buf, stride, size, [old.0, old.1, BOX, BOX], bg);
        damage.push([old.0, old.1, BOX, BOX]);
        if let Some(p) = pointer {
            if let Some((mx, my)) = self.mark {
                Self::fill(buf, stride, size, [mx, my, MARK, MARK], bg);
                damage.push([mx, my, MARK, MARK]);
            }
            let clamp = |v: i32, max: u32| {
                (v - (MARK / 2) as i32).clamp(0, max.saturating_sub(MARK) as i32) as u32
            };
            self.mark = Some((clamp(p.0, w), clamp(p.1, h)));
        }
        let max = (w.saturating_sub(BOX) as i32, h.saturating_sub(BOX) as i32);
        let mut nx = old.0 as i32 + self.vel.0;
        let mut ny = old.1 as i32 + self.vel.1;
        if nx < 0 || nx > max.0 {
            self.vel.0 = -self.vel.0;
            nx = nx.clamp(0, max.0);
        }
        if ny < 0 || ny > max.1 {
            self.vel.1 = -self.vel.1;
            ny = ny.clamp(0, max.1);
        }
        self.box_pos = (nx as u32, ny as u32);
        self.draw_box(buf, stride, size);
        damage.push([self.box_pos.0, self.box_pos.1, BOX, BOX]);
        if let Some((mx, my)) = self.mark {
            Self::fill(buf, stride, size, [mx, my, MARK, MARK], |x, y| {
                if x == mx + MARK / 2 || y == my + MARK / 2 {
                    [0, 0, 0, 0xFF]
                } else {
                    bg(x, y)
                }
            });
            damage.push([mx, my, MARK, MARK]);
        }
        for [x, y, rw, rh] in damage {
            d.damage(x, y, rw, rh);
        }
        d.flush();
    }
}

fn run(host: Arc<Host>, opts: TestOptions, rx: Receiver<Event>) {
    let start = Instant::now();
    let period = Duration::from_secs_f64(1.0 / opts.fps as f64);
    let sizes = [
        (opts.width, opts.height),
        (opts.width * 3 / 4, opts.height * 3 / 4),
    ];
    let mut size_index = 0;
    let mut next_resize = opts
        .resize_every
        .map(|s| start + Duration::from_secs_f64(s));
    let close_at = opts.close_after.map(|s| start + Duration::from_secs_f64(s));

    host.display().set_size(sizes[0].0, sizes[0].1);
    let mut pic = Picture::new(sizes[0].0, sizes[0].1);
    pic.draw_all(&host);

    let mut next = Instant::now() + period;
    loop {
        let mut pointer = None;
        loop {
            let wait = next.saturating_duration_since(Instant::now());
            match rx.recv_timeout(wait) {
                Ok(Event::Pointer(x, y)) => pointer = Some((x, y)),
                Ok(Event::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => break,
            }
        }
        next += period;
        let now = Instant::now();
        if next < now {
            next = now + period;
        }
        if close_at.is_some_and(|t| now >= t) {
            host.finish(State::Closed, "The test pattern ended.");
        }
        if let Some(t) = next_resize.filter(|t| now >= *t) {
            size_index ^= 1;
            let (w, h) = sizes[size_index];
            log::info!("test pattern now {w}x{h}");
            host.display().set_size(w, h);
            pic = Picture::new(w, h);
            pic.draw_all(&host);
            next_resize = opts.resize_every.map(|s| t + Duration::from_secs_f64(s));
            continue;
        }
        pic.step(&host, pointer);
    }
}
