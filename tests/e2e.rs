//! End to end against a real RustDesk peer behind a real hbbs/hbbr, as
//! started by `tools/e2e/up.sh`. Skipped unless `CASEMENT_E2E_ADDRESS` is
//! set. Checks on the peer's side go through `docker exec` into
//! `CASEMENT_E2E_PEER_CONTAINER`.

use casement_rustdesk::protocol::{FromCasement, State, ToCasement};
use casement_rustdesk::sim::Casement;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

const T: Duration = Duration::from_secs(30);

struct Env {
    address: String,
    password: String,
    container: String,
}

fn env() -> Option<Env> {
    let address = std::env::var("CASEMENT_E2E_ADDRESS").ok()?;
    Some(Env {
        address,
        password: std::env::var("CASEMENT_E2E_PASSWORD").unwrap_or_default(),
        container: std::env::var("CASEMENT_E2E_PEER_CONTAINER")
            .unwrap_or_else(|_| "e2e-peer".into()),
    })
}

fn on_peer(e: &Env, script: &str) -> String {
    let out = Command::new("docker")
        .args([
            "exec",
            "-e",
            "DISPLAY=:0",
            &e.container,
            "bash",
            "-c",
            script,
        ])
        .output()
        .expect("docker exec");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn wait_on_peer(e: &Env, script: &str, want: &str) -> String {
    let end = Instant::now() + T;
    loop {
        let got = on_peer(e, script);
        if got.contains(want) || Instant::now() > end {
            return got;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn start(e: &Env, password: &str) -> Casement {
    let mut c = Casement::spawn(Path::new(env!("CARGO_BIN_EXE_casement-rustdesk")), true).unwrap();
    c.send(&FromCasement::Hello {
        protocol: 1,
        app: "e2e".into(),
    })
    .unwrap();
    c.send(&FromCasement::Connect {
        address: e.address.clone(),
        username: String::new(),
        password: password.into(),
        title: "e2e".into(),
    })
    .unwrap();
    c
}

fn state(c: &Casement) -> (State, String) {
    c.expect(T, |m| match m {
        ToCasement::State { state, text } => Some((*state, text.clone())),
        _ => None,
    })
    .expect("a state")
}

fn key(c: &mut Casement, scancode: u32) {
    for down in [true, false] {
        c.send(&FromCasement::Key { scancode, down }).unwrap();
    }
}

/// One test, run in order: the steps share one peer.
#[test]
fn rustdesk_peer() {
    let Some(e) = env() else {
        eprintln!("CASEMENT_E2E_ADDRESS isn't set; see tools/e2e/up.sh");
        return;
    };
    session(&e);
    wrong_and_missing_password(&e);
    bad_server_key(e);
}

fn session(e: &Env) {
    let mut c = start(e, &e.password);
    assert_eq!(state(&c).0, State::Connected);

    // The picture: a surface for the peer's screen and a first full frame.
    let (w, h) = c
        .expect(T, |m| match m {
            ToCasement::SurfaceRequest { width, height } => Some((*width, *height)),
            _ => None,
        })
        .unwrap();
    assert!(w >= 640 && h >= 480, "{w}x{h}");
    let (surface, msg) = c.create_surface(1, w, h, 0).unwrap();
    c.send(&msg).unwrap();
    let rects = c
        .expect(T, |m| match m {
            ToCasement::Frame { surface: 1, rects } => Some(rects.clone()),
            _ => None,
        })
        .unwrap();
    assert!(rects.is_empty());
    let lit = surface
        .pixels()
        .chunks(4)
        .filter(|p| p[0] > 200 && p[1] > 200 && p[2] > 200)
        .count();
    assert!(
        lit > (w * h / 4) as usize,
        "expected the white xterm, {lit} light pixels"
    );
    c.send(&FromCasement::FrameAck { surface: 1 }).unwrap();

    // The pointer lands where it was sent.
    c.send(&FromCasement::Pointer {
        x: 321,
        y: 123,
        buttons: 0,
    })
    .unwrap();
    let loc = wait_on_peer(e, "xdotool getmouselocation", "x:321 y:123");
    assert!(loc.contains("x:321 y:123"), "{loc}");

    // A click focuses the terminal; typed keys run a command there.
    for buttons in [1, 0] {
        c.send(&FromCasement::Pointer {
            x: 600,
            y: 400,
            buttons,
        })
        .unwrap();
    }
    // "touch /tmp/typedok", Enter
    for sc in [
        0x14, 0x18, 0x16, 0x2E, 0x23, 0x39, 0x35, 0x14, 0x32, 0x19, 0x35, 0x14, 0x15, 0x19, 0x12,
        0x20, 0x18, 0x25, 0x1C,
    ] {
        key(&mut c, sc);
    }
    let typed = wait_on_peer(e, "test -f /tmp/typedok && echo yes", "yes");
    assert_eq!(typed, "yes");

    // Clipboard text both ways. The peer's change first: right after RustDesk
    // writes a clipboard it got from us it doesn't report the next change.
    let theirs = format!("from the peer {}", std::process::id());
    // Detached, so xclip keeps owning the clipboard after docker exec returns.
    let status = Command::new("docker")
        .args(["exec", "-d", "-e", "DISPLAY=:0", &e.container, "bash", "-c"])
        .arg(format!(
            "printf %s '{theirs}' | xclip -selection clipboard -quiet"
        ))
        .status()
        .expect("docker exec");
    assert!(status.success());
    let text = c
        .expect(T, |m| match m {
            ToCasement::Clipboard { text } if *text == theirs => Some(text.clone()),
            _ => None,
        })
        .expect("the peer's clipboard");
    assert_eq!(text, theirs);

    let ours = format!("from casement {}", std::process::id());
    c.send(&FromCasement::Clipboard { text: ours.clone() })
        .unwrap();
    let got = wait_on_peer(e, "timeout 2 xclip -o -selection clipboard", &ours);
    assert_eq!(got, ours);
    // Frames keep coming as the picture changes (the typed command did).
    c.send(&FromCasement::FrameAck { surface: 1 }).unwrap();

    c.send(&FromCasement::Disconnect).unwrap();
    c.close_stdin();
    assert_eq!(
        c.wait(Duration::from_secs(2)).and_then(|s| s.code()),
        Some(0)
    );
    on_peer(e, "rm -f /tmp/typedok");
}

fn wrong_and_missing_password(e: &Env) {
    for password in ["wrong-password", ""] {
        let mut c = start(e, password);
        assert_eq!(state(&c).0, State::AuthFailed, "password {password:?}");
        assert_eq!(c.wait(T).and_then(|s| s.code()), Some(0));
    }
}

fn bad_server_key(e: Env) {
    // Another valid key: hbbs refuses it.
    let other = "OeVuKk5nlHiXp+APNn0Y3pC1Iwpwn44JGqrQCsWqmBw=";
    let start_key = e.address.find("key=").unwrap() + 4;
    let end_key = e.address[start_key..]
        .find('&')
        .map_or(e.address.len(), |i| start_key + i);
    let address = format!(
        "{}{other}{}",
        &e.address[..start_key],
        &e.address[end_key..]
    );
    let mut c = start(&Env { address, ..e }, "x");
    let (s, text) = state(&c);
    assert_eq!(s, State::Failed);
    assert!(text.contains("key"), "{text}");
    assert_eq!(c.wait(T).and_then(|s| s.code()), Some(1));
}
