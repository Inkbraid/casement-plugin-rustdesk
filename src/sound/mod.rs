//! Sound output: decoded samples played on the computer's default output
//! device, through a jitter buffer that rides out uneven arrival.
//!
//! Peers send sound unevenly: RustDesk on Windows sends a packet every 10 ms
//! or so, give or take the network, but RustDesk on Linux records with
//! PulseAudio's default buffering and sends about two seconds at a time.
//! So sound waits in a [`Buffer`] and starts playing a playout delay after
//! it starts arriving; the delay grows when the buffer runs dry because
//! sound came late, and shrinks again while it doesn't. What stays queued
//! well beyond the delay (after a stall, say) is skipped, which keeps the
//! latency down.
//!
//! A render thread keeps a small device buffer filled from the jitter
//! buffer, with silence while there's nothing to play. On Windows the device
//! is a WASAPI stream in shared mode ([`wasapi`]), as in Casement's Moonlight
//! plugin; elsewhere, where the plugin only runs for tests, a stand-in takes
//! the sound at the real rate and plays nothing.

#[cfg(windows)]
mod wasapi;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The playout delay to start with, and the least it shrinks to.
pub const MIN_DELAY: Duration = Duration::from_millis(60);
/// The most the playout delay grows to.
pub const MAX_DELAY: Duration = Duration::from_millis(500);
/// How much the delay grows each time the buffer runs dry because sound
/// came late; it shrinks by a quarter of that per quiet [`WINDOW`].
const DELAY_STEP: Duration = Duration::from_millis(40);
/// Sound resuming within this long after the buffer ran dry came late;
/// later than that, the peer had paused its sound.
const LATE: Duration = Duration::from_millis(500);
/// The most sound the buffer holds, as in RustDesk's own client; the oldest
/// goes beyond that.
pub const CAPACITY: Duration = Duration::from_secs(3);
/// How often the buffer checks whether it holds more than it needs and
/// whether the delay can shrink.
const WINDOW: Duration = Duration::from_secs(10);
/// How much more than the delay may stay queued for a whole window before
/// the excess is skipped.
const SLACK: Duration = Duration::from_millis(40);
/// The device buffer the render thread keeps filled.
pub const DEVICE_BUFFER: Duration = Duration::from_millis(40);
/// With nothing to play for this long, the device stops until sound
/// arrives again.
const IDLE: Duration = Duration::from_secs(1);
/// How long after a failed open the next one is tried, and how often the
/// default output device is checked.
const RECHECK: Duration = Duration::from_secs(2);

/// A stream's format: interleaved 32-bit float samples, -1 to 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Format {
    pub rate: u32,
    pub channels: u16,
}

impl Format {
    fn frames(&self, d: Duration) -> usize {
        (d.as_nanos() * u128::from(self.rate) / 1_000_000_000) as usize
    }

    fn duration(&self, frames: usize) -> Duration {
        Duration::from_nanos(frames as u64 * 1_000_000_000 / u64::from(self.rate.max(1)))
    }
}

/// What has been played, for the log.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Stats {
    /// Frames (one sample per channel) of sound written to the device.
    pub frames: u64,
    /// The loudest sample, 0 to 1.
    pub peak: f32,
    /// Times the buffer ran dry while playing.
    pub rebuffers: u64,
    /// Frames left out: beyond the capacity, or skipped to cut the delay.
    pub dropped: u64,
    /// The playout delay in the end.
    pub delay: Duration,
}

/// The jitter buffer between the decoder and the render thread. Time is
/// passed in, so the tests can move it.
pub struct Buffer {
    format: Format,
    queue: VecDeque<f32>,
    playing: bool,
    /// While not playing: when sound started arriving.
    first: Option<Instant>,
    /// When the buffer last ran dry.
    dry: Option<Instant>,
    delay: Duration,
    muted: bool,
    /// The lowest level (in frames) while playing in this window, and
    /// whether the buffer ran dry in it.
    low: Option<usize>,
    ran_dry: bool,
    window: Instant,
    /// Whether sound has played since the start or the last unmute, for
    /// the log.
    started: bool,
    stats: Stats,
}

impl Buffer {
    pub fn new(format: Format, now: Instant) -> Self {
        Buffer {
            format,
            queue: VecDeque::new(),
            playing: false,
            first: None,
            dry: None,
            delay: MIN_DELAY,
            muted: false,
            low: None,
            ran_dry: false,
            window: now,
            started: false,
            stats: Stats::default(),
        }
    }

    fn channels(&self) -> usize {
        usize::from(self.format.channels.max(1))
    }

    /// Frames waiting to be played.
    pub fn queued(&self) -> usize {
        self.queue.len() / self.channels()
    }

    /// Whether there is sound to play, or soon will be.
    pub fn busy(&self) -> bool {
        self.playing || self.first.is_some()
    }

    pub fn stats(&self) -> Stats {
        Stats {
            delay: self.delay,
            ..self.stats
        }
    }

    /// Notes frames of sound the device took.
    pub fn played(&mut self, frames: usize) {
        self.stats.frames += frames as u64;
    }

    /// Muting drops what is queued and leaves sound out until unmuted.
    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        if muted {
            self.queue.clear();
            self.playing = false;
            self.first = None;
            self.dry = None;
            self.started = false;
        }
    }

    /// Adds decoded sound: interleaved samples in the stream's format.
    pub fn push(&mut self, samples: &[f32], now: Instant) {
        let ch = self.channels();
        let samples = &samples[..samples.len() / ch * ch];
        if self.muted || samples.is_empty() {
            return;
        }
        for s in samples {
            self.stats.peak = self.stats.peak.max(s.abs());
        }
        if !self.playing && self.first.is_none() {
            self.first = Some(now);
            if let Some(dry) = self.dry.take() {
                if now.duration_since(dry) < LATE {
                    // Ran dry because sound came late: wait longer.
                    self.delay = (self.delay + DELAY_STEP).min(MAX_DELAY);
                    self.ran_dry = true;
                }
            }
        }
        self.queue.extend(samples);
        let cap = self.format.frames(CAPACITY) * ch;
        if self.queue.len() > cap {
            let over = self.queue.len() - cap;
            self.queue.drain(..over);
            self.stats.dropped += (over / ch) as u64;
        }
    }

    /// Fills `out` (interleaved, in the stream's format) with what is due:
    /// sound once the delay has passed, silence otherwise. Returns the
    /// frames of sound.
    pub fn pull(&mut self, out: &mut [f32], now: Instant) -> usize {
        let ch = self.channels();
        let want = out.len() / ch;
        if !self.playing {
            match self.first {
                Some(t) if now.duration_since(t) >= self.delay => {
                    self.playing = true;
                    self.first = None;
                    let delay = self.delay.as_millis();
                    if self.started {
                        log::debug!("sound: playing again, {delay} ms behind");
                    } else {
                        log::info!("sound: playing, {delay} ms behind");
                        self.started = true;
                    }
                }
                _ => {
                    out.fill(0.0);
                    self.end_window(now);
                    return 0;
                }
            }
        }
        let n = want.min(self.queued());
        for (o, s) in out.iter_mut().zip(self.queue.drain(..n * ch)) {
            *o = s;
        }
        out[n * ch..].fill(0.0);
        if n < want {
            // Ran dry: play again once the delay has passed after more
            // sound arrives.
            self.playing = false;
            self.dry = Some(now);
            self.stats.rebuffers += 1;
        } else {
            let level = self.queued();
            self.low = Some(self.low.map_or(level, |l| l.min(level)));
        }
        self.end_window(now);
        n
    }

    /// Every [`WINDOW`]: skips what stayed queued beyond the delay the whole
    /// time, and shrinks the delay when the buffer didn't run dry.
    fn end_window(&mut self, now: Instant) {
        if now.duration_since(self.window) < WINDOW {
            return;
        }
        let keep = self.format.frames(self.delay);
        if let Some(low) = self.low.filter(|_| self.playing) {
            if low > keep + self.format.frames(SLACK) {
                let skip = low - keep;
                let ch = self.channels();
                self.queue.drain(..skip * ch);
                self.stats.dropped += skip as u64;
                log::debug!(
                    "sound: skipped {} ms that stayed queued",
                    self.format.duration(skip).as_millis()
                );
            }
        }
        if !self.ran_dry {
            self.delay = self.delay.saturating_sub(DELAY_STEP / 4).max(MIN_DELAY);
        }
        self.low = None;
        self.ran_dry = false;
        self.window = now;
    }
}

/// A sound device as the render thread uses it: made on that thread and
/// used only there.
pub trait Device {
    /// Waits until the device wants more sound, or `timeout` passed.
    fn wait(&mut self, timeout: Duration);
    /// Frames the device takes now.
    fn room(&mut self) -> Result<usize, String>;
    /// Queues interleaved samples in the stream's format.
    fn write(&mut self, samples: &[f32]) -> Result<(), String>;
    fn start(&mut self) -> Result<(), String>;
    /// Stops and drops what the device holds.
    fn stop(&mut self);
    /// Whether this is still the device to use: the user may have chosen
    /// another default output.
    fn current(&mut self) -> bool {
        true
    }
}

/// Opens a device for a stream format, on the render thread.
pub type Opener = Box<dyn FnMut(Format) -> Result<Box<dyn Device>, String> + Send>;

struct Shared {
    buffer: Mutex<Buffer>,
    quit: AtomicBool,
}

impl Shared {
    fn buffer(&self) -> MutexGuard<'_, Buffer> {
        self.buffer.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Plays a stream on the default output: [`Player::play`] puts decoded
/// sound into the jitter buffer, and a render thread takes it to the
/// device.
pub struct Player {
    shared: Option<Arc<Shared>>,
    render: Option<JoinHandle<Opener>>,
    /// The opener, while no render thread has it.
    open: Option<Opener>,
    muted: bool,
    /// Totals of earlier formats.
    done: Stats,
}

impl Default for Player {
    fn default() -> Self {
        Self::new()
    }
}

impl Player {
    /// A player for the default output device.
    pub fn new() -> Self {
        #[cfg(windows)]
        let open: Opener =
            Box::new(|f| wasapi::Wasapi::open(f).map(|d| Box::new(d) as Box<dyn Device>));
        #[cfg(not(windows))]
        let open: Opener = Box::new(|f| Ok(Box::new(Clock::new(f)) as Box<dyn Device>));
        Self::with_device(open)
    }

    /// A player for the devices `open` makes.
    pub fn with_device(open: Opener) -> Self {
        Player {
            shared: None,
            render: None,
            open: Some(open),
            muted: false,
            done: Stats::default(),
        }
    }

    /// The stream's format, before its first sound and whenever it changes.
    /// A new format starts a new buffer and device.
    pub fn set_format(&mut self, format: Format) {
        if let Some(s) = &self.shared {
            if s.buffer().format == format {
                return;
            }
        }
        self.stop_render();
        let Some(open) = self.open.take() else {
            return;
        };
        let mut buffer = Buffer::new(format, Instant::now());
        buffer.set_muted(self.muted);
        let shared = Arc::new(Shared {
            buffer: Mutex::new(buffer),
            quit: AtomicBool::new(false),
        });
        let s = shared.clone();
        let spawned = std::thread::Builder::new()
            .name("sound output".into())
            .spawn(move || render(s, open, format));
        match spawned {
            Ok(t) => {
                self.render = Some(t);
                self.shared = Some(shared);
            }
            Err(e) => log::error!("can't start the sound output thread: {e}"),
        }
    }

    /// Adds a packet of decoded sound: interleaved samples in the stream's
    /// format.
    pub fn play(&mut self, samples: &[f32]) {
        if let Some(s) = &self.shared {
            s.buffer().push(samples, Instant::now());
        }
    }

    /// Muting stops the sound at once and drops what was queued; sound is
    /// left out until unmuted.
    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        if let Some(s) = &self.shared {
            s.buffer().set_muted(muted);
        }
    }

    pub fn stats(&self) -> Stats {
        let mut t = self.done;
        if let Some(s) = &self.shared {
            add(&mut t, s.buffer().stats());
        }
        t
    }

    fn stop_render(&mut self) {
        let Some(shared) = self.shared.take() else {
            return;
        };
        shared.quit.store(true, Ordering::Relaxed);
        if let Some(t) = self.render.take() {
            match t.join() {
                Ok(open) => self.open = Some(open),
                Err(_) => log::error!("the sound output thread panicked"),
            }
        }
        add(&mut self.done, shared.buffer().stats());
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop_render();
    }
}

fn add(total: &mut Stats, s: Stats) {
    total.frames += s.frames;
    total.peak = total.peak.max(s.peak);
    total.rebuffers += s.rebuffers;
    total.dropped += s.dropped;
    total.delay = s.delay;
}

/// The render thread: keeps the device buffer filled from the jitter
/// buffer, opening the device (again) as needed, until told to quit.
/// Returns the opener for the next format.
fn render(shared: Arc<Shared>, mut open: Opener, format: Format) -> Opener {
    #[cfg(windows)]
    let _com = wasapi::Com::init();
    let ch = usize::from(format.channels.max(1));
    let mut device: Option<Box<dyn Device>> = None;
    let mut started = false;
    let mut warned = false;
    let mut next_check = Instant::now();
    let mut last_lost: Option<Instant> = None;
    let mut idle_since: Option<Instant> = None;
    let mut last = Instant::now();
    let mut samples = Vec::new();
    while !shared.quit.load(Ordering::Relaxed) {
        let now = Instant::now();
        if now >= next_check {
            next_check = now + RECHECK;
            if device.as_mut().is_some_and(|d| !d.current()) {
                log::info!("sound: the default output changed");
                device = None;
                started = false;
            }
            if device.is_none() {
                match open(format) {
                    Ok(d) => {
                        device = Some(d);
                        warned = false;
                    }
                    Err(e) if !warned => {
                        log::warn!("sound: {e}");
                        warned = true;
                    }
                    Err(e) => log::debug!("sound: {e}"),
                }
            }
        }
        let Some(d) = device.as_mut() else {
            // No device: let the sound go at the real rate rather than pile
            // up.
            std::thread::sleep(Duration::from_millis(10));
            let now = Instant::now();
            samples.resize(format.frames(now - last) * ch, 0.0);
            shared.buffer().pull(&mut samples, now);
            last = now;
            continue;
        };
        d.wait(Duration::from_millis(10));
        let now = Instant::now();
        last = now;
        let mut buffer = shared.buffer();
        if buffer.busy() {
            idle_since = None;
        } else if started && now.duration_since(*idle_since.get_or_insert(now)) >= IDLE {
            // Nothing to play for a while: let the device rest.
            d.stop();
            started = false;
        }
        if !started && !buffer.busy() {
            continue;
        }
        let room = match d.room() {
            Ok(r) => r,
            Err(e) => {
                drop(buffer);
                next_check = lost(&e, &mut last_lost, now);
                device = None;
                started = false;
                continue;
            }
        };
        if room == 0 {
            continue;
        }
        samples.resize(room * ch, 0.0);
        let sound = buffer.pull(&mut samples, now);
        drop(buffer);
        let result = d.write(&samples).and_then(|()| {
            if !started {
                d.start()?;
                started = true;
            }
            Ok(())
        });
        match result {
            Ok(()) => shared.buffer().played(sound),
            Err(e) => {
                next_check = lost(&e, &mut last_lost, now);
                device = None;
                started = false;
            }
        }
    }
    if let Some(d) = device.as_mut() {
        d.stop();
    }
    open
}

/// The device stopped working: logs it and says when to open it again,
/// right away unless it failed just before, too.
fn lost(e: &str, last_lost: &mut Option<Instant>, now: Instant) -> Instant {
    let again = last_lost.is_some_and(|t| now.duration_since(t) < RECHECK);
    *last_lost = Some(now);
    if again {
        log::debug!("sound: the output stopped working again: {e}");
        now + Duration::from_millis(500)
    } else {
        log::warn!("sound: the output stopped working: {e}");
        now
    }
}

/// Off Windows: a device with a [`DEVICE_BUFFER`] that takes sound at the
/// stream's rate and plays nothing.
#[cfg(not(windows))]
struct Clock {
    format: Format,
    level: Duration,
    /// When playing, since when `level` is counted down.
    since: Option<Instant>,
}

#[cfg(not(windows))]
impl Clock {
    fn new(format: Format) -> Self {
        Clock {
            format,
            level: Duration::ZERO,
            since: None,
        }
    }

    fn update(&mut self) {
        if let Some(since) = self.since {
            let now = Instant::now();
            self.level = self.level.saturating_sub(now - since);
            self.since = Some(now);
        }
    }
}

#[cfg(not(windows))]
impl Device for Clock {
    fn wait(&mut self, timeout: Duration) {
        std::thread::sleep(timeout.min(Duration::from_millis(5)));
    }

    fn room(&mut self) -> Result<usize, String> {
        self.update();
        Ok(self.format.frames(DEVICE_BUFFER.saturating_sub(self.level)))
    }

    fn write(&mut self, samples: &[f32]) -> Result<(), String> {
        self.update();
        self.level += self
            .format
            .duration(samples.len() / usize::from(self.format.channels.max(1)));
        Ok(())
    }

    fn start(&mut self) -> Result<(), String> {
        self.since = Some(Instant::now());
        Ok(())
    }

    fn stop(&mut self) {
        self.since = None;
        self.level = Duration::ZERO;
    }
}

/// The sample types a device may insist on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sample {
    F32,
    I16,
    I32,
}

impl Sample {
    pub fn bytes(self) -> usize {
        match self {
            Sample::I16 => 2,
            Sample::F32 | Sample::I32 => 4,
        }
    }
}

/// A device's own format, when it takes no other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceFormat {
    pub rate: u32,
    pub channels: u16,
    pub sample: Sample,
}

/// From the stream's format to one a device insists on: linear
/// interpolation for the rate, then the channels (mono goes to the first
/// two, a mono device gets the average of the first two), then the sample
/// type.
pub struct Convert {
    from: Format,
    to: DeviceFormat,
    /// Input frames per output frame.
    step: f64,
    /// The next output frame's position in the current input packet; -1 is
    /// the last frame of the previous one.
    pos: f64,
    last: Vec<f32>,
    resampled: Vec<f32>,
}

impl Convert {
    pub fn new(from: Format, to: DeviceFormat) -> Self {
        Convert {
            from,
            to,
            step: f64::from(from.rate) / f64::from(to.rate),
            pos: 0.0,
            last: vec![0.0; usize::from(from.channels)],
            resampled: Vec::new(),
        }
    }

    /// Converts one packet into `out` (replacing what was there) and returns
    /// the number of frames.
    pub fn run(&mut self, input: &[f32], out: &mut Vec<u8>) -> usize {
        let ch = usize::from(self.from.channels);
        let frames = input.len() / ch;
        out.clear();
        if frames == 0 {
            return 0;
        }
        let input = &input[..frames * ch];
        let samples: &[f32] = if self.from.rate == self.to.rate {
            input
        } else {
            self.resampled.clear();
            loop {
                let i = self.pos.floor();
                if i + 1.0 >= frames as f64 {
                    break;
                }
                let f = (self.pos - i) as f32;
                for c in 0..ch {
                    let a = if i < 0.0 {
                        self.last[c]
                    } else {
                        input[i as usize * ch + c]
                    };
                    let b = input[(i + 1.0) as usize * ch + c];
                    self.resampled.push(a + (b - a) * f);
                }
                self.pos += self.step;
            }
            self.pos -= frames as f64;
            self.last.copy_from_slice(&input[(frames - 1) * ch..]);
            &self.resampled
        };
        let to_ch = usize::from(self.to.channels);
        for frame in samples.chunks_exact(ch) {
            for c in 0..to_ch {
                let v = match (to_ch, ch) {
                    (1, 1) => frame[0],
                    (1, _) => (frame[0] + frame[1]) / 2.0,
                    (_, 1) if c < 2 => frame[0],
                    (_, 1) => 0.0,
                    _ => frame.get(c).copied().unwrap_or(0.0),
                }
                .clamp(-1.0, 1.0);
                match self.to.sample {
                    Sample::F32 => out.extend_from_slice(&v.to_le_bytes()),
                    Sample::I16 => out.extend_from_slice(&((v * 32767.0) as i16).to_le_bytes()),
                    // Just short of 2^31, which f32 can't hold one below.
                    Sample::I32 => {
                        out.extend_from_slice(&((v * 2_147_483_520.0) as i32).to_le_bytes())
                    }
                }
            }
        }
        samples.len() / ch
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STEREO: Format = Format {
        rate: 48000,
        channels: 2,
    };

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// 10 ms of stereo at 48 kHz, at `level`.
    fn packet(level: f32) -> Vec<f32> {
        vec![level; 960]
    }

    /// The device takes 10 ms; returns the frames of sound in it.
    fn tick(b: &mut Buffer, now: Instant) -> usize {
        let mut out = vec![1.0; 960];
        let n = b.pull(&mut out, now);
        assert!(
            out[n * 2..].iter().all(|s| *s == 0.0),
            "silence after the sound"
        );
        n
    }

    #[test]
    fn steady_sound_plays_after_the_delay_and_keeps_up() {
        let t0 = Instant::now();
        let mut b = Buffer::new(STEREO, t0);
        let mut sound = 0;
        for i in 0..300 {
            let now = t0 + ms(10 * i);
            b.push(&packet(0.5), now);
            let n = tick(&mut b, now);
            assert_eq!(n, if i < 6 { 0 } else { 480 }, "at {i}");
            sound += n;
        }
        assert_eq!(sound, 294 * 480);
        // The delay is all that stays queued.
        assert_eq!(b.queued(), 6 * 480);
        let s = b.stats();
        assert_eq!((s.rebuffers, s.dropped, s.peak), (0, 0, 0.5));
    }

    #[test]
    fn two_second_bursts_play_without_a_gap() {
        // As RustDesk on Linux sends sound with PulseAudio's default
        // buffering.
        let t0 = Instant::now();
        let mut b = Buffer::new(STEREO, t0);
        let mut sound = 0;
        for i in 0..3000 {
            let now = t0 + ms(10 * i);
            if i % 200 == 0 {
                for _ in 0..200 {
                    b.push(&packet(0.5), now);
                }
            }
            sound += tick(&mut b, now);
        }
        assert_eq!(sound, (3000 - 6) * 480);
        let s = b.stats();
        assert_eq!((s.rebuffers, s.dropped, s.delay), (0, 0, MIN_DELAY));
    }

    #[test]
    fn late_sound_lengthens_the_delay_and_quiet_shortens_it() {
        let t0 = Instant::now();
        let mut b = Buffer::new(STEREO, t0);
        let mut late = 0;
        for i in 0..6000 {
            let now = t0 + ms(10 * i);
            // 100 ms nothing at 1 s, then the backlog at once.
            match i {
                100..=109 => late += 1,
                110 => {
                    for _ in 0..=late {
                        b.push(&packet(0.5), now);
                    }
                }
                _ => b.push(&packet(0.5), now),
            }
            tick(&mut b, now);
            if i == 120 {
                assert_eq!(b.stats().rebuffers, 1);
                assert_eq!(b.stats().delay, MIN_DELAY + DELAY_STEP);
            }
        }
        // A minute without running dry: back to the least delay.
        assert_eq!(b.stats().rebuffers, 1);
        assert_eq!(b.stats().delay, MIN_DELAY);
    }

    #[test]
    fn a_pause_in_the_sound_keeps_the_delay() {
        let t0 = Instant::now();
        let mut b = Buffer::new(STEREO, t0);
        for i in 0..500 {
            let now = t0 + ms(10 * i);
            if !(100..300).contains(&i) {
                b.push(&packet(0.5), now);
            }
            let n = tick(&mut b, now);
            if (306..500).contains(&i) {
                assert_eq!(n, 480, "at {i}");
            }
        }
        assert_eq!(b.stats().rebuffers, 1);
        assert_eq!(b.stats().delay, MIN_DELAY);
    }

    #[test]
    fn beyond_the_capacity_the_oldest_goes() {
        let t0 = Instant::now();
        let mut b = Buffer::new(STEREO, t0);
        for i in 0..400 {
            b.push(&packet(i as f32 / 1000.0), t0);
        }
        assert_eq!(b.queued(), 3 * 48000);
        assert_eq!(b.stats().dropped, 48000);
        let mut out = vec![0.0; 2];
        b.pull(&mut out, t0 + MIN_DELAY);
        assert_eq!(out[0], 0.1);
    }

    #[test]
    fn what_stays_queued_beyond_the_delay_is_skipped() {
        let t0 = Instant::now();
        let mut b = Buffer::new(STEREO, t0);
        for i in 0..3100 {
            let now = t0 + ms(10 * i);
            b.push(&packet(0.5), now);
            if i == 1500 {
                // A burst after a stall that never showed: 300 ms extra.
                for _ in 0..30 {
                    b.push(&packet(0.5), now);
                }
            }
            tick(&mut b, now);
            if i == 2999 {
                // The window it arrived in had less queued before it.
                assert_eq!(b.stats().dropped, 0);
            }
        }
        // Skipped at the end of the first whole window with it queued.
        assert_eq!(b.stats().dropped, 30 * 480);
        assert_eq!(b.queued(), 6 * 480);
        assert_eq!(b.stats().rebuffers, 0);
    }

    #[test]
    fn muting_drops_and_leaves_out_the_sound() {
        let t0 = Instant::now();
        let mut b = Buffer::new(STEREO, t0);
        for i in 0..20 {
            b.push(&packet(0.5), t0 + ms(10 * i));
            tick(&mut b, t0 + ms(10 * i));
        }
        b.set_muted(true);
        assert_eq!(b.queued(), 0);
        assert!(!b.busy());
        for i in 20..40 {
            b.push(&packet(0.9), t0 + ms(10 * i));
            assert_eq!(tick(&mut b, t0 + ms(10 * i)), 0);
        }
        assert_eq!(b.stats().peak, 0.5);
        b.set_muted(false);
        for i in 40..60 {
            b.push(&packet(0.5), t0 + ms(10 * i));
            let n = tick(&mut b, t0 + ms(10 * i));
            assert_eq!(n, if i < 46 { 0 } else { 480 }, "at {i}");
        }
        assert_eq!(b.stats().rebuffers, 0);
    }

    /// A device that takes sound at the real rate and keeps what it got.
    struct Recorder {
        format: Format,
        level: Duration,
        since: Option<Instant>,
        got: Arc<Mutex<Vec<f32>>>,
    }

    impl Device for Recorder {
        fn wait(&mut self, timeout: Duration) {
            std::thread::sleep(timeout.min(ms(5)));
        }
        fn room(&mut self) -> Result<usize, String> {
            if let Some(since) = self.since {
                let now = Instant::now();
                self.level = self.level.saturating_sub(now - since);
                self.since = Some(now);
            }
            Ok(self.format.frames(DEVICE_BUFFER.saturating_sub(self.level)))
        }
        fn write(&mut self, samples: &[f32]) -> Result<(), String> {
            self.level += self.format.duration(samples.len() / 2);
            self.got.lock().unwrap().extend_from_slice(samples);
            Ok(())
        }
        fn start(&mut self) -> Result<(), String> {
            self.since = Some(Instant::now());
            Ok(())
        }
        fn stop(&mut self) {
            self.since = None;
            self.level = Duration::ZERO;
        }
    }

    #[test]
    fn the_render_thread_plays_what_arrives() {
        let got = Arc::new(Mutex::new(Vec::new()));
        let g = got.clone();
        let mut p = Player::with_device(Box::new(move |format| {
            Ok(Box::new(Recorder {
                format,
                level: Duration::ZERO,
                since: None,
                got: g.clone(),
            }) as Box<dyn Device>)
        }));
        p.set_format(STEREO);
        for _ in 0..30 {
            p.play(&packet(0.5));
        }
        std::thread::sleep(ms(600));
        let s = p.stats();
        drop(p);
        // All 300 ms of sound, in order, with silence around it.
        assert_eq!(s.frames, 30 * 480);
        let got = got.lock().unwrap();
        let first = got.iter().position(|v| *v != 0.0).unwrap();
        assert!(got[first..first + 30 * 960].iter().all(|v| *v == 0.5));
        assert!(got[first + 30 * 960..].iter().all(|v| *v == 0.0));
    }

    #[test]
    fn nothing_plays_without_a_format() {
        let mut p = Player::with_device(Box::new(|_| panic!("no format, no device")));
        p.play(&packet(0.1));
        assert_eq!(p.stats(), Stats::default());
    }

    fn floats(bytes: &[u8]) -> Vec<f32> {
        bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
            .collect()
    }

    #[test]
    fn converts_channels_and_sample_types() {
        let mono = Format {
            rate: 48000,
            channels: 1,
        };
        let device = |channels, sample| DeviceFormat {
            rate: 48000,
            channels,
            sample,
        };
        let mut out = Vec::new();
        let mut c = Convert::new(STEREO, device(2, Sample::F32));
        assert_eq!(c.run(&[0.5, -0.25, 2.0, -2.0], &mut out), 2);
        assert_eq!(floats(&out), [0.5, -0.25, 1.0, -1.0]);

        let mut c = Convert::new(mono, device(4, Sample::F32));
        assert_eq!(c.run(&[0.5], &mut out), 1);
        assert_eq!(floats(&out), [0.5, 0.5, 0.0, 0.0]);

        let mut c = Convert::new(STEREO, device(1, Sample::F32));
        c.run(&[0.5, 0.25], &mut out);
        assert_eq!(floats(&out), [0.375]);

        let mut c = Convert::new(STEREO, device(2, Sample::I16));
        c.run(&[1.0, -1.0], &mut out);
        assert_eq!(out, [0xFF, 0x7F, 0x01, 0x80]);

        let mut c = Convert::new(mono, device(1, Sample::I32));
        c.run(&[1.0], &mut out);
        assert_eq!(
            i32::from_le_bytes(out[..4].try_into().unwrap()),
            2_147_483_520
        );
    }

    #[test]
    fn resampling_keeps_the_rate_and_the_wave_across_packets() {
        let to = DeviceFormat {
            rate: 44100,
            channels: 2,
            sample: Sample::F32,
        };
        let mut c = Convert::new(STEREO, to);
        let mut out = Vec::new();
        let mut got = Vec::new();
        let mut n = 0;
        for p in 0..100 {
            let input: Vec<f32> = (0..480)
                .flat_map(|i| {
                    let t = (p * 480 + i) as f32 / 48000.0;
                    let v = (2.0 * std::f32::consts::PI * 441.0 * t).sin() * 0.8;
                    [v, -v]
                })
                .collect();
            n += c.run(&input, &mut out);
            got.extend(floats(&out));
        }
        // One second in, one second out (give or take the frame still
        // waiting for the next packet).
        assert!((44099..=44100).contains(&n), "{n} frames");
        // Still a 441 Hz wave at 0.8: compare with the ideal one.
        let worst = got
            .chunks_exact(2)
            .enumerate()
            .map(|(i, f)| {
                let t = i as f32 / 44100.0;
                let want = (2.0 * std::f32::consts::PI * 441.0 * t).sin() * 0.8;
                assert_eq!(f[0], -f[1]);
                (f[0] - want).abs()
            })
            .fold(0.0f32, f32::max);
        assert!(worst < 0.01, "off by up to {worst}");
    }
}
