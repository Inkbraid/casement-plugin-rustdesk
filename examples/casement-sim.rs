//! Drives the plugin the way Casement does, for trying addresses by hand:
//!
//! ```text
//! cargo run --example casement-sim -- <address> [--password P] [--seconds S]
//!     [--out DIR] [--clipboard TEXT] [--click X,Y] [--type SCANCODES]
//! ```
//!
//! It answers every `surface_request`, acknowledges every frame, prints the
//! other messages, and writes the last picture to `DIR/last-frame.ppm`.
//! `--type` sends key presses, e.g. `--type 0x1E,0x30` for "ab".

use casement_rustdesk::protocol::{FromCasement, ToCasement};
use casement_rustdesk::sim::{Casement, HostSurface};
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn main() {
    let mut args = std::env::args().skip(1);
    let mut address = None;
    let mut password = String::new();
    let mut seconds = 10.0;
    let mut out = PathBuf::from(".");
    let mut clipboard = None;
    let mut click = None;
    let mut keys = Vec::new();
    while let Some(a) = args.next() {
        let mut val = || args.next().expect("a value after the option");
        match a.as_str() {
            "--password" => password = val(),
            "--seconds" => seconds = val().parse().expect("seconds"),
            "--out" => out = val().into(),
            "--clipboard" => clipboard = Some(val()),
            "--click" => {
                let v = val();
                let (x, y) = v.split_once(',').expect("X,Y");
                click = Some((x.parse::<i32>().expect("x"), y.parse::<i32>().expect("y")));
            }
            "--type" => {
                keys = val()
                    .split(',')
                    .map(|k| {
                        u32::from_str_radix(k.trim_start_matches("0x"), 16).expect("hex scancode")
                    })
                    .collect()
            }
            _ => address = Some(a),
        }
    }
    let address = address.expect("usage: casement-sim <address> [options]");

    let exe = std::env::var_os("CASEMENT_PLUGIN_EXE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let me = std::env::current_exe().expect("current exe");
            let dir = me.parent().and_then(|d| d.parent()).expect("target dir");
            dir.join(format!("casement-rustdesk{}", std::env::consts::EXE_SUFFIX))
        });
    let mut c = Casement::spawn(&exe, true).expect("start the plugin");
    c.send(&FromCasement::Hello {
        protocol: 1,
        app: "casement-sim".into(),
    })
    .unwrap();
    c.send(&FromCasement::Connect {
        address,
        username: String::new(),
        password,
        title: "sim".into(),
    })
    .unwrap();

    let start = Instant::now();
    let end = start + Duration::from_secs_f64(seconds);
    let mut surface: Option<(HostSurface, u32, u32, u32)> = None;
    let mut next_id = 1;
    let mut frames = 0u64;
    let mut input_sent = false;
    while Instant::now() < end {
        let Some(m) = c.recv(Duration::from_millis(100)) else {
            if c.wait(Duration::ZERO).is_some() {
                break;
            }
            continue;
        };
        match m {
            ToCasement::SurfaceRequest { width, height } => {
                eprintln!("sim: surface_request {width}x{height}");
                let (s, msg) = c.create_surface(next_id, width, height, 0).unwrap();
                surface = Some((s, next_id, width, height));
                next_id += 1;
                c.send(&msg).unwrap();
            }
            ToCasement::Frame { surface: id, rects } => {
                frames += 1;
                if frames <= 3 || frames.is_multiple_of(100) {
                    eprintln!("sim: frame {frames} on surface {id}, {} rects", rects.len());
                }
                c.send(&FromCasement::FrameAck { surface: id }).unwrap();
                if !input_sent {
                    input_sent = true;
                    if let Some((x, y)) = click {
                        for buttons in [0, 1, 0] {
                            c.send(&FromCasement::Pointer { x, y, buttons }).unwrap();
                        }
                    }
                    for &k in &keys {
                        c.send(&FromCasement::Key {
                            scancode: k,
                            down: true,
                        })
                        .unwrap();
                        c.send(&FromCasement::Key {
                            scancode: k,
                            down: false,
                        })
                        .unwrap();
                    }
                    if let Some(text) = clipboard.take() {
                        c.send(&FromCasement::Clipboard { text }).unwrap();
                    }
                }
            }
            ToCasement::Cursor(cur) => eprintln!(
                "sim: cursor default={} {:?}x{:?} hot {:?},{:?}",
                cur.default, cur.width, cur.height, cur.hot_x, cur.hot_y
            ),
            other => eprintln!("sim: {other:?}"),
        }
    }
    eprintln!(
        "sim: {frames} frames in {:.1} s",
        start.elapsed().as_secs_f64()
    );
    if let Some((s, _, w, h)) = &surface {
        let path = out.join("last-frame.ppm");
        write_ppm(&path, s, *w, *h).expect("write ppm");
        eprintln!("sim: wrote {}", path.display());
    }
    let _ = c.send(&FromCasement::Disconnect);
    c.close_stdin();
    match c.wait(Duration::from_secs(2)) {
        Some(s) => eprintln!("sim: plugin exited with {s}"),
        None => eprintln!("sim: plugin still running after 2 s"),
    }
}

fn write_ppm(path: &std::path::Path, s: &HostSurface, w: u32, h: u32) -> std::io::Result<()> {
    let mut data = format!("P6\n{w} {h}\n255\n").into_bytes();
    let px = s.pixels();
    for y in 0..h as usize {
        for x in 0..w as usize {
            let i = (y * w as usize + x) * 4;
            data.extend_from_slice(&[px[i + 2], px[i + 1], px[i]]);
        }
    }
    std::fs::write(path, data)
}
