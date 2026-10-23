//! Plays a 440 Hz tone at 0.5 through the plugin's sound output, in 10 ms
//! packets as a peer sends them:
//!
//!   sound-check steady   1.8 s, a 300 ms stall, muted for 0.4 s, 0.8 s more
//!   sound-check bursts   6 s, two seconds at a time (RustDesk on Linux)
//!
//! and prints what the player counted. check.sh records what comes out.

#[path = "../../../src/sound/mod.rs"]
#[allow(dead_code)]
mod sound;

use std::time::{Duration, Instant};

fn main() {
    env_logger::Builder::from_env(env_logger::Env::new().default_filter_or("info")).init();
    let bursts = match std::env::args().nth(1).as_deref() {
        Some("steady") => false,
        Some("bursts") => true,
        _ => {
            eprintln!("usage: sound-check steady|bursts");
            std::process::exit(2);
        }
    };
    let format = sound::Format {
        rate: 48000,
        channels: 2,
    };
    let mut player = sound::Player::new();
    player.set_format(format);
    let start = Instant::now();
    let packets = if bursts { 600 } else { 300 };
    for n in 0..packets {
        let due = if bursts {
            start + Duration::from_millis(n / 200 * 2000)
        } else {
            start + Duration::from_millis(n * 10 + if n >= 100 { 300 } else { 0 })
        };
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        if !bursts && n == 180 {
            player.set_muted(true);
        }
        if !bursts && n == 220 {
            player.set_muted(false);
        }
        let pcm: Vec<f32> = (0..480)
            .flat_map(|i| {
                let t = (n as usize * 480 + i) as f32 / 48000.0;
                let v = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.5;
                [v, v]
            })
            .collect();
        player.play(&pcm);
    }
    // Let what is queued play out.
    std::thread::sleep(Duration::from_secs(3));
    let s = player.stats();
    println!(
        "frames {} peak {:.3} rebuffers {} dropped {}",
        s.frames, s.peak, s.rebuffers, s.dropped
    );
}
