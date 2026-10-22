//! Plays Casement's side of the protocol against the real executable.

use casement_rustdesk::protocol::{Cursor, FromCasement, State, ToCasement};
use casement_rustdesk::sim::{Casement, HostSurface};
use casement_rustdesk::test_pattern::{background, box_start};
use std::path::Path;
use std::time::Duration;

const T: Duration = Duration::from_secs(5);

fn start() -> Casement {
    Casement::spawn(Path::new(env!("CARGO_BIN_EXE_casement-rustdesk")), true).unwrap()
}

fn hello(c: &mut Casement) {
    c.send(&FromCasement::Hello {
        protocol: 1,
        app: "Casement test".into(),
    })
    .unwrap();
    assert_eq!(c.recv(T), Some(ToCasement::hello()));
}

fn connect(c: &mut Casement, address: &str, password: &str) {
    c.send(&FromCasement::Connect {
        address: address.into(),
        username: String::new(),
        password: password.into(),
        title: "Test".into(),
    })
    .unwrap();
}

fn state(c: &Casement) -> (State, String) {
    c.expect(T, |m| match m {
        ToCasement::State { state, text } => Some((*state, text.clone())),
        _ => None,
    })
    .expect("a state message")
}

fn surface_request(c: &Casement) -> (u32, u32) {
    c.expect(T, |m| match m {
        ToCasement::SurfaceRequest { width, height } => Some((*width, *height)),
        _ => None,
    })
    .expect("a surface_request")
}

fn frame(c: &Casement, timeout: Duration) -> Option<(u32, Vec<[u32; 4]>)> {
    c.expect(timeout, |m| match m {
        ToCasement::Frame { surface, rects } => Some((*surface, rects.clone())),
        _ => None,
    })
}

fn pixel(s: &HostSurface, stride: u32, x: u32, y: u32) -> [u8; 4] {
    let i = (y * stride + x * 4) as usize;
    s.pixels()[i..i + 4].try_into().unwrap()
}

#[test]
fn test_pattern_end_to_end() {
    let mut c = start();
    hello(&mut c);
    connect(&mut c, "rustdesk://test?width=320&height=240&fps=60", "");
    assert_eq!(state(&c).0, State::Connected);
    let cursor = c
        .expect(T, |m| match m {
            ToCasement::Cursor(cur) => Some(cur.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!((cursor.width, cursor.height), (Some(15), Some(15)));
    assert_eq!(surface_request(&c), (320, 240));

    // A padded stride, as Casement may use.
    let (s1, msg) = c.create_surface(1, 320, 240, 64).unwrap();
    let stride = 320 * 4 + 64;
    c.send(&msg).unwrap();
    let (id, rects) = frame(&c, T).expect("first frame");
    assert_eq!(id, 1);
    assert!(rects.is_empty(), "the first frame covers everything");
    for x in [0, 20, 60, 100, 140, 180, 220, 260, 300, 319] {
        assert_eq!(
            pixel(&s1, stride, x, 2),
            background(x, 2, 320, 240),
            "x={x}"
        );
    }
    assert_eq!(pixel(&s1, stride, 10, 230), background(10, 230, 320, 240));
    let (bx, by) = box_start(320, 240);
    assert_ne!(
        pixel(&s1, stride, bx + 5, by + 5),
        background(bx + 5, by + 5, 320, 240)
    );

    // One frame in flight: nothing more until frame_ack.
    assert_eq!(frame(&c, Duration::from_millis(300)), None);
    for _ in 0..5 {
        c.send(&FromCasement::FrameAck { surface: 1 }).unwrap();
        let (id, rects) = frame(&c, T).expect("frame after ack");
        assert_eq!(id, 1);
        assert!(!rects.is_empty());
        for [x, y, w, h] in rects {
            assert!(
                w > 0 && h > 0 && x + w <= 320 && y + h <= 240,
                "{x} {y} {w} {h}"
            );
        }
    }
    // A stale ack for another surface doesn't release the current one.
    c.send(&FromCasement::FrameAck { surface: 99 }).unwrap();
    assert_eq!(frame(&c, Duration::from_millis(300)), None);

    // A new surface replaces the old one and gets a full frame.
    let (s2, msg) = c.create_surface(2, 320, 240, 0).unwrap();
    c.send(&msg).unwrap();
    let (id, rects) = frame(&c, T).expect("frame on the new surface");
    assert_eq!((id, rects.len()), (2, 0));
    assert_eq!(pixel(&s2, 320 * 4, 0, 0), background(0, 0, 320, 240));
    c.send(&FromCasement::FrameAck { surface: 2 }).unwrap();

    // Input is echoed: pointer as cursor_pos, clipboard once, F1 cursor.
    c.send(&FromCasement::Pointer {
        x: 10,
        y: 20,
        buttons: 1,
    })
    .unwrap();
    let pos = c
        .expect(T, |m| match m {
            ToCasement::CursorPos { x, y, visible } => Some((*x, *y, *visible)),
            _ => None,
        })
        .unwrap();
    assert_eq!(pos, (10, 20, true));
    c.send(&FromCasement::Key {
        scancode: 0x1E,
        down: true,
    })
    .unwrap();
    c.send(&FromCasement::Key {
        scancode: 0x11D,
        down: false,
    })
    .unwrap();
    c.send(&FromCasement::Wheel { delta: -1 }).unwrap();
    c.send(&FromCasement::Clipboard {
        text: "hello".into(),
    })
    .unwrap();
    let echoed = c
        .expect(T, |m| match m {
            ToCasement::Clipboard { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(echoed, "echo: hello");
    c.send(&FromCasement::Key {
        scancode: 0x3B,
        down: true,
    })
    .unwrap();
    let cursor = c
        .expect(T, |m| match m {
            ToCasement::Cursor(cur) => Some(cur.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(cursor, Cursor::default_arrow());

    c.send(&FromCasement::Disconnect).unwrap();
    c.close_stdin();
    let status = c.wait(Duration::from_secs(2)).expect("exits within 2 s");
    assert_eq!(status.code(), Some(0));
    let log = c.stderr().join("\n");
    for line in [
        "pointer x=10 y=20 buttons=1",
        "key scancode=0x1e down=true",
        "key scancode=0x11d down=false",
        "wheel delta=-1",
        "clipboard 5 chars",
        "disconnect",
    ] {
        assert!(log.contains(line), "stderr lacks {line:?}:\n{log}");
    }
}

#[test]
fn password_is_checked() {
    let mut c = start();
    hello(&mut c);
    connect(&mut c, "rustdesk://test?password=sesame", "");
    assert_eq!(state(&c).0, State::AuthFailed);
    assert_eq!(c.wait(T).and_then(|s| s.code()), Some(0));

    let mut c = start();
    hello(&mut c);
    connect(&mut c, "rustdesk://test?password=sesame", "wrong");
    assert_eq!(state(&c).0, State::AuthFailed);

    let mut c = start();
    hello(&mut c);
    connect(&mut c, "rustdesk://test?password=sesame", "sesame");
    assert_eq!(state(&c).0, State::Connected);
}

#[test]
fn bad_addresses_fail_with_text() {
    for address in [
        "vnc://host",
        "rustdesk://123456789",
        "rustdesk://123456789?server=hbbs.example.com",
        "rustdesk://test?fail=Simulated%20failure.",
    ] {
        let mut c = start();
        hello(&mut c);
        connect(&mut c, address, "");
        let (s, text) = state(&c);
        assert_eq!(s, State::Failed, "{address}");
        assert!(!text.is_empty(), "{address}");
        assert_eq!(c.wait(T).and_then(|s| s.code()), Some(1), "{address}");
    }
}

#[test]
fn closed_after_a_while() {
    let mut c = start();
    hello(&mut c);
    connect(
        &mut c,
        "rustdesk://test?width=64&height=64&close_after=0.3",
        "",
    );
    assert_eq!(state(&c).0, State::Connected);
    assert_eq!(state(&c).0, State::Closed);
    assert_eq!(c.wait(T).and_then(|s| s.code()), Some(0));
}

#[test]
fn resizing_asks_for_a_new_surface() {
    let mut c = start();
    hello(&mut c);
    connect(
        &mut c,
        "rustdesk://test?width=200&height=100&resize_every=0.3",
        "",
    );
    assert_eq!(surface_request(&c), (200, 100));
    let (_s1, msg) = c.create_surface(1, 200, 100, 0).unwrap();
    c.send(&msg).unwrap();
    assert!(frame(&c, T).is_some());
    c.send(&FromCasement::FrameAck { surface: 1 }).unwrap();
    assert_eq!(surface_request(&c), (150, 75));
    let (s2, msg) = c.create_surface(2, 150, 75, 0).unwrap();
    c.send(&msg).unwrap();
    let (id, rects) = c
        .expect(T, |m| match m {
            ToCasement::Frame { surface: 2, rects } => Some((2, rects.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!((id, rects.len()), (2, 0));
    assert_eq!(pixel(&s2, 150 * 4, 149, 0), background(149, 0, 150, 75));
}

#[test]
fn version_and_unknown_messages() {
    let mut c = start();
    // Junk and future message types are skipped.
    c.send_raw(b"not json").unwrap();
    c.send_raw(br#"{"type": "from_the_future", "x": [1, 2]}"#)
        .unwrap();
    // A newer Casement gets our highest version back.
    c.send(&FromCasement::Hello {
        protocol: 2,
        app: "Casement 9".into(),
    })
    .unwrap();
    match c.recv(T) {
        Some(ToCasement::Hello { protocol, .. }) => assert_eq!(protocol, 1),
        other => panic!("{other:?}"),
    }
    // Closing stdin ends the plugin.
    c.close_stdin();
    assert_eq!(c.wait(T).and_then(|s| s.code()), Some(0));
}
