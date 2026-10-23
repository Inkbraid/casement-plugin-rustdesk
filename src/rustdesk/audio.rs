//! The peer's sound: Opus packets decoded with libopus (through RustDesk's
//! `magnum-opus` binding) on a thread of their own and played by
//! [`crate::sound::Player`].
//!
//! A peer sends an `AudioFormat` and then one `AudioFrame` per Opus packet,
//! 10 ms each, as RustDesk's `audio_service` does, and the format again
//! whenever sound starts anew. The decoding follows `AudioHandler` in
//! RustDesk's `src/client.rs` (at the commit in
//! [`super::RUSTDESK_COMMIT`]). Muting asks the peer to stop sending (see
//! `client.rs`); packets still on their way are left out here.

use crate::sound::{Format, Player};
use hbb_common::bytes::Bytes;
use magnum_opus::{Channels, Decoder};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread::JoinHandle;

/// The rates libopus decodes at.
const OPUS_RATES: [u32; 5] = [8000, 12000, 16000, 24000, 48000];
/// The longest Opus packet, 120 ms, in samples per channel at `rate`.
fn max_samples(rate: u32) -> usize {
    rate as usize * 120 / 1000
}

enum Command {
    Format(Format),
    Packet(Bytes),
    Mute(bool),
}

/// What was played, for the log and the tests.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Totals {
    pub decoded: u64,
    /// Packets that arrived while muted.
    pub left_out: u64,
    /// Packets libopus couldn't decode.
    pub bad: u64,
    pub sound: crate::sound::Stats,
}

pub struct Audio {
    tx: Option<Sender<Command>>,
    thread: Option<JoinHandle<Totals>>,
}

impl Audio {
    pub fn start(muted: bool) -> Self {
        let (tx, rx) = channel();
        let thread = std::thread::Builder::new()
            .name("sound".into())
            .spawn(move || run(rx, muted));
        let thread = match thread {
            Ok(t) => Some(t),
            Err(e) => {
                log::error!("can't start the sound thread: {e}");
                None
            }
        };
        Audio {
            tx: Some(tx),
            thread,
        }
    }

    /// An `AudioFormat` from the peer.
    pub fn format(&self, rate: u32, channels: u32) {
        let format = Format {
            rate,
            channels: channels.min(u32::from(u16::MAX)) as u16,
        };
        self.send(Command::Format(format));
    }

    /// An `AudioFrame` from the peer: one Opus packet.
    pub fn packet(&self, data: Bytes) {
        self.send(Command::Packet(data));
    }

    pub fn set_muted(&self, muted: bool) {
        self.send(Command::Mute(muted));
    }

    fn send(&self, c: Command) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(c);
        }
    }

    /// Stops playing, after logging what was played.
    pub fn stop(mut self) -> Totals {
        self.tx = None;
        self.thread
            .take()
            .and_then(|t| t.join().ok())
            .unwrap_or_default()
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(rx: Receiver<Command>, mut muted: bool) -> Totals {
    let mut player = Player::new();
    player.set_muted(muted);
    let mut totals = Totals::default();
    let mut decoder: Option<(Decoder, Format)> = None;
    let mut pcm = Vec::new();
    while let Ok(c) = rx.recv() {
        match c {
            Command::Format(f) => {
                // A new decoder each time: sound starting anew starts with a
                // new encoder on the peer, too.
                decoder = None;
                let channels = match f.channels {
                    1 => Channels::Mono,
                    2 => Channels::Stereo,
                    _ => {
                        log::warn!("sound: {} channels aren't supported", f.channels);
                        continue;
                    }
                };
                if !OPUS_RATES.contains(&f.rate) {
                    log::warn!("sound: Opus has no {} Hz", f.rate);
                    continue;
                }
                match Decoder::new(f.rate, channels) {
                    Ok(d) => {
                        log::info!("sound: Opus, {} Hz, {} channels", f.rate, f.channels);
                        pcm.resize(max_samples(f.rate) * usize::from(f.channels), 0.0);
                        player.set_format(f);
                        decoder = Some((d, f));
                    }
                    Err(e) => log::warn!("sound: can't start the Opus decoder: {e}"),
                }
            }
            Command::Packet(data) => {
                if muted {
                    totals.left_out += 1;
                    continue;
                }
                let Some((d, f)) = decoder.as_mut() else {
                    continue;
                };
                match d.decode_float(&data, &mut pcm, false) {
                    Ok(n) => {
                        totals.decoded += 1;
                        player.play(&pcm[..n * usize::from(f.channels)]);
                    }
                    Err(e) => {
                        if totals.bad == 0 {
                            log::warn!("sound: can't decode a packet: {e}");
                        }
                        totals.bad += 1;
                    }
                }
            }
            Command::Mute(m) if m != muted => {
                muted = m;
                player.set_muted(m);
                if muted {
                    log::info!("sound: muted");
                } else {
                    log::info!(
                        "sound: unmuted ({} packets arrived while muted)",
                        totals.left_out
                    );
                }
            }
            Command::Mute(_) => {}
        }
    }
    totals.sound = player.stats();
    let s = totals.sound;
    log::info!(
        "sound: {} packets decoded, {} frames played, peak level {:.3}, ran dry {} times, \
         {} frames left out, {} ms behind, {} undecodable",
        totals.decoded,
        s.frames,
        s.peak,
        s.rebuffers,
        s.dropped,
        s.delay.as_millis(),
        totals.bad
    );
    totals
}

#[cfg(test)]
mod tests {
    use super::*;
    use magnum_opus::{Application, Encoder};

    /// `n` packets of 10 ms of a 440 Hz tone at 0.5, as RustDesk's peer
    /// encodes them.
    fn tone(rate: u32, channels: Channels, n: usize) -> Vec<Bytes> {
        let ch = channels as usize;
        let mut enc = Encoder::new(rate, channels, Application::LowDelay).unwrap();
        let per = rate as usize / 100;
        (0..n)
            .map(|p| {
                let pcm: Vec<f32> = (0..per)
                    .flat_map(|i| {
                        let t = (p * per + i) as f32 / rate as f32;
                        let v = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.5;
                        std::iter::repeat_n(v, ch)
                    })
                    .collect();
                Bytes::from(enc.encode_vec_float(&pcm, pcm.len() * 6).unwrap())
            })
            .collect()
    }

    #[test]
    fn decodes_and_plays_the_peers_packets() {
        let a = Audio::start(false);
        a.format(48000, 2);
        for p in tone(48000, Channels::Stereo, 100) {
            a.packet(p);
        }
        let t = a.stop();
        assert_eq!((t.decoded, t.bad, t.left_out), (100, 0, 0));
        // What reaches the device depends on its timing; the level doesn't.
        assert!((0.4..0.6).contains(&t.sound.peak), "peak {}", t.sound.peak);
    }

    #[test]
    fn mono_and_other_rates() {
        let a = Audio::start(false);
        a.format(16000, 1);
        for p in tone(16000, Channels::Mono, 10) {
            a.packet(p);
        }
        let t = a.stop();
        assert_eq!(t.decoded, 10);
        assert!(t.sound.peak > 0.3, "peak {}", t.sound.peak);
    }

    #[test]
    fn muted_packets_are_left_out() {
        let a = Audio::start(true);
        a.format(48000, 2);
        let packets = tone(48000, Channels::Stereo, 20);
        for p in &packets[..5] {
            a.packet(p.clone());
        }
        a.set_muted(false);
        // The peer starts its sound anew after being unmuted.
        a.format(48000, 2);
        for p in &packets[5..] {
            a.packet(p.clone());
        }
        let t = a.stop();
        assert_eq!((t.left_out, t.decoded), (5, 15));
    }

    #[test]
    fn packets_without_a_usable_format_are_ignored() {
        let a = Audio::start(false);
        let packets = tone(48000, Channels::Stereo, 3);
        a.packet(packets[0].clone());
        a.format(44100, 2);
        a.packet(packets[1].clone());
        a.format(48000, 6);
        a.packet(packets[2].clone());
        let t = a.stop();
        assert_eq!(t.decoded, 0);
        assert_eq!(t.sound.frames, 0);
    }

    #[test]
    fn a_bad_packet_is_counted_and_skipped() {
        let a = Audio::start(false);
        a.format(48000, 2);
        a.packet(Bytes::from_static(&[0xFF, 0xFF, 0xFF]));
        for p in tone(48000, Channels::Stereo, 2) {
            a.packet(p);
        }
        let t = a.stop();
        assert_eq!((t.bad, t.decoded), (1, 2));
    }
}
