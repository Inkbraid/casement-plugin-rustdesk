//! One session with a peer: login, then pictures, sound, cursor and
//! clipboard from the peer and input, mute and clipboard from Casement.
//!
//! The message handling follows `Remote::io_loop` and `handle_msg_from_peer`
//! in RustDesk's `src/client/io_loop.rs`, the login `handle_hash` and
//! `LoginConfigHandler::create_login_msg` and muting
//! (`LoginConfigHandler::toggle_option("disable-audio")`) in `src/client.rs`
//! (at the commit in [`super::RUSTDESK_COMMIT`]), reduced to a remote-desktop
//! view without file transfer, chat, voice calls, privacy mode or multiple
//! displays. RustDesk is © Purslane Ltd. and contributors, AGPL-3.0.

use super::audio::Audio;
use super::input::{Keyboard, Mouse, Platform};
use super::media::{self, Cursors};
use super::video::Video;
use crate::address::PeerAddress;
use crate::host::Host;
use crate::protocol::{FromCasement, State, ToCasement};
use base::message_proto::{
    login_response, message, misc, option_message::BoolOption, DisplayInfo, Hash, LoginRequest,
    Message, Misc, OptionMessage, PeerInfo,
};
use hbb_common::protobuf::{EnumOrUnknown, Message as _, MessageField};
use hbb_common::sha2::{Digest, Sha256};
use hbb_common::stream::Stream;
use hbb_common::tokio::{self, sync::mpsc::UnboundedReceiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// RustDesk's login error texts (`src/client.rs`).
const LOGIN_MSG_PASSWORD_EMPTY: &str = "Empty Password";
const LOGIN_MSG_PASSWORD_WRONG: &str = "Wrong Password";
const LOGIN_MSG_2FA_WRONG: &str = "Wrong 2FA Code";
const REQUIRE_2FA: &str = "2FA Required";
const LOGIN_MSG_NO_PASSWORD_ACCESS: &str = "No Password Access";

/// No message from the peer for this long ends the session (as RustDesk).
const RECEIVE_TIMEOUT: Duration = Duration::from_secs(30);

pub enum Event {
    Casement(FromCasement),
    Disconnect,
}

/// Runs the session until it ends. Returns normally only after `disconnect`;
/// every other end reports a final state and exits the process.
pub async fn run(
    host: Arc<Host>,
    peer: PeerAddress,
    password: String,
    mut events: UnboundedReceiver<Event>,
) {
    let mut early = Early::default();
    let mut conn = tokio::select! {
        res = super::connect::connect(&peer) => match res {
            Ok(c) => c,
            Err(text) => host.finish(State::Failed, &text),
        },
        _ = wait_for_disconnect(&mut events, &mut early) => return,
    };
    let mut s = Remote {
        host,
        peer,
        password,
        info: None,
        display: DisplayInfo::new(),
        keyboard: Keyboard::default(),
        mouse: Mouse::default(),
        cursors: Cursors::default(),
        cursor_hidden: false,
        video: None,
        audio: None,
        muted: early.muted,
        last_clipboard: None,
        pending_clipboard: early.clipboard,
        refresh: None,
    };
    let (refresh_tx, mut refresh_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    s.refresh = Some(refresh_tx);
    let mut last_recv = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            res = conn.next() => match res {
                Some(Ok(bytes)) => {
                    last_recv = Instant::now();
                    s.handle_peer_message(&bytes, &mut conn).await;
                }
                Some(Err(e)) => s.end(&format!("The connection to {} broke: {e}", s.peer.id)),
                None => s.end(&format!("{} closed the connection.", s.peer.id)),
            },
            ev = events.recv() => match ev {
                Some(Event::Casement(msg)) => s.handle_casement(msg, &mut conn).await,
                Some(Event::Disconnect) | None => {
                    s.close(&mut conn).await;
                    return;
                }
            },
            Some(()) = refresh_rx.recv() => send(&mut conn, refresh_video()).await,
            _ = tick.tick() => {
                if last_recv.elapsed() >= RECEIVE_TIMEOUT {
                    s.end(&format!("{} stopped answering.", s.peer.id));
                }
            }
        }
    }
}

/// What Casement sent while the connection was being made.
#[derive(Default)]
struct Early {
    /// The latest clipboard text, for after login.
    clipboard: Option<String>,
    /// Whether sound is muted: the login asks for sound only if not.
    muted: bool,
}

/// Waits for `disconnect` while connecting, keeping the latest clipboard
/// text and mute state for after login. Other input before the connection
/// has nowhere to go.
async fn wait_for_disconnect(events: &mut UnboundedReceiver<Event>, early: &mut Early) {
    loop {
        match events.recv().await {
            Some(Event::Disconnect) | None => return,
            Some(Event::Casement(FromCasement::Clipboard { text })) => early.clipboard = Some(text),
            Some(Event::Casement(FromCasement::Mute { muted })) => early.muted = muted,
            Some(Event::Casement(_)) => {}
        }
    }
}

struct Remote {
    host: Arc<Host>,
    peer: PeerAddress,
    password: String,
    /// Set once logged in.
    info: Option<PeerInfo>,
    display: DisplayInfo,
    keyboard: Keyboard,
    mouse: Mouse,
    cursors: Cursors,
    cursor_hidden: bool,
    video: Option<Video>,
    /// Started at login.
    audio: Option<Audio>,
    /// Casement's mute state; the peer sends sound only while unmuted.
    muted: bool,
    /// The last clipboard text exchanged, to avoid echoing it back.
    last_clipboard: Option<String>,
    /// Clipboard text from Casement before login.
    pending_clipboard: Option<String>,
    refresh: Option<tokio::sync::mpsc::UnboundedSender<()>>,
}

impl Remote {
    /// The connection ended: `closed` once logged in, `failed` before.
    fn end(&mut self, text: &str) -> ! {
        let state = if self.info.is_some() {
            State::Closed
        } else {
            State::Failed
        };
        self.stop_audio();
        self.host.finish(state, text)
    }

    /// Stops the sound, which logs what was played.
    fn stop_audio(&mut self) {
        if let Some(a) = self.audio.take() {
            a.stop();
        }
    }

    fn platform(&self) -> Platform {
        Platform::from_peer(self.info.as_ref().map_or("", |i| i.platform.as_str()))
    }

    async fn handle_peer_message(&mut self, bytes: &[u8], conn: &mut Stream) {
        let Ok(msg) = Message::parse_from_bytes(bytes) else {
            log::debug!("ignoring an unreadable message from the peer");
            return;
        };
        match msg.union {
            Some(message::Union::Hash(hash)) => self.login(hash, conn).await,
            Some(message::Union::LoginResponse(lr)) => match lr.union {
                Some(login_response::Union::Error(err)) => self.login_error(&err),
                Some(login_response::Union::PeerInfo(pi)) => self.logged_in(pi, conn).await,
                _ => {}
            },
            Some(message::Union::PeerInfo(pi)) => self.set_peer_info(pi),
            Some(message::Union::VideoFrame(vf)) => {
                if self.info.is_none() || vf.display != self.current_display() {
                    return;
                }
                if let Some(v) = self.video.as_mut() {
                    if v.push(vf) {
                        send(conn, refresh_video()).await;
                    }
                }
            }
            Some(message::Union::CursorData(cd)) => match media::decode_cursor(&cd) {
                Ok(c) => {
                    self.cursors.insert(cd.id, c.clone());
                    if !self.cursor_hidden {
                        self.host.send(&ToCasement::Cursor(c));
                    }
                }
                Err(e) => log::warn!("ignoring cursor {}: {e}", cd.id),
            },
            Some(message::Union::CursorId(id)) => {
                if let Some(c) = self.cursors.get(id).filter(|_| !self.cursor_hidden) {
                    self.host.send(&ToCasement::Cursor(c.clone()));
                }
            }
            Some(message::Union::CursorPosition(cp)) => {
                self.host.send(&ToCasement::CursorPos {
                    x: cp.x - self.display.x,
                    y: cp.y - self.display.y,
                    visible: true,
                });
            }
            Some(message::Union::Clipboard(cb)) => {
                if let Some(text) = media::clipboard_text(&cb) {
                    self.clipboard_from_peer(text);
                }
            }
            Some(message::Union::MultiClipboards(mcb)) => {
                if let Some(text) = media::multi_clipboard_text(&mcb) {
                    self.clipboard_from_peer(text);
                }
            }
            Some(message::Union::AudioFrame(af)) => {
                if let Some(a) = &self.audio {
                    a.packet(af.data);
                }
            }
            Some(message::Union::TestDelay(t)) => {
                if !t.from_client {
                    let mut out = Message::new();
                    out.set_test_delay(t);
                    send(conn, out).await;
                }
            }
            Some(message::Union::Misc(m)) => self.misc(m),
            Some(message::Union::MessageBox(mb)) => {
                log::info!(
                    "peer message box ({}): {} — {}",
                    mb.msgtype,
                    mb.title,
                    mb.text
                )
            }
            Some(other) => log::trace!("ignoring {}", message_name(&other)),
            None => {}
        }
    }

    /// The peer's challenge: answer with the password, hashed as RustDesk
    /// does (sha256(sha256(password + salt) + challenge)).
    async fn login(&mut self, hash: Hash, conn: &mut Stream) {
        if self.password.is_empty() {
            // RustDesk would log in without one and let the peer's user
            // accept by click; Casement asks for the password instead.
            self.host.finish(State::AuthFailed, "A password is needed.");
        }
        let password = {
            let salted = Sha256::new()
                .chain_update(self.password.as_bytes())
                .chain_update(hash.salt.as_bytes())
                .finalize();
            Sha256::new()
                .chain_update(salted)
                .chain_update(hash.challenge.as_bytes())
                .finalize()
                .to_vec()
        };
        let msg = login_message(&self.peer.id, password, self.muted);
        log::info!("logging in to {}", self.peer.id);
        send(conn, msg).await;
    }

    fn login_error(&mut self, err: &str) {
        log::warn!("login refused: {err}");
        match err {
            LOGIN_MSG_PASSWORD_EMPTY => self.host.finish(State::AuthFailed, "A password is needed."),
            LOGIN_MSG_PASSWORD_WRONG => self.host.finish(State::AuthFailed, "The password is wrong."),
            REQUIRE_2FA | LOGIN_MSG_2FA_WRONG => self.host.finish(
                State::Failed,
                "This computer asks for a two-factor code, which the RustDesk plugin doesn't support yet.",
            ),
            // Waiting for someone at the peer to accept.
            LOGIN_MSG_NO_PASSWORD_ACCESS => {}
            _ => self.host.finish(State::Failed, &format!("{} refused the login: {err}", self.peer.id)),
        }
    }

    async fn logged_in(&mut self, pi: PeerInfo, conn: &mut Stream) {
        log::info!(
            "logged in: {} {} on {} (RustDesk {}), {} display(s)",
            pi.username,
            pi.hostname,
            pi.platform,
            pi.version,
            pi.displays.len()
        );
        // The password has done its job; don't keep it around.
        self.password.clear();
        self.set_peer_info(pi);
        self.host.send(&ToCasement::state(State::Connected, ""));
        let refresh = self.refresh.clone();
        self.video = Some(Video::start(self.host.clone(), move || {
            if let Some(r) = &refresh {
                let _ = r.send(());
            }
        }));
        self.audio = Some(Audio::start(self.muted));
        if let Some(text) = self.pending_clipboard.take() {
            self.clipboard_to_peer(text, conn).await;
        }
    }

    fn current_display(&self) -> i32 {
        self.info.as_ref().map_or(0, |i| i.current_display)
    }

    fn set_peer_info(&mut self, mut pi: PeerInfo) {
        if pi.current_display < 0 || pi.current_display as usize >= pi.displays.len() {
            pi.current_display = 0;
        }
        if let Some(d) = pi.displays.get(pi.current_display as usize) {
            self.set_display(d.clone());
        }
        self.info = Some(pi);
    }

    fn set_display(&mut self, d: DisplayInfo) {
        log::info!(
            "display {}x{} at {},{}{}",
            d.width,
            d.height,
            d.x,
            d.y,
            if d.cursor_embedded {
                ", cursor in the picture"
            } else {
                ""
            }
        );
        if d.cursor_embedded != self.cursor_hidden {
            self.cursor_hidden = d.cursor_embedded;
            if self.cursor_hidden {
                self.host.send(&ToCasement::Cursor(media::hidden_cursor()));
            }
        }
        self.display = d;
    }

    fn misc(&mut self, m: Misc) {
        match m.union {
            Some(misc::Union::CloseReason(reason)) => {
                let text = if reason.is_empty() {
                    format!("{} closed the connection.", self.peer.id)
                } else {
                    format!("{} closed the connection: {reason}", self.peer.id)
                };
                self.end(&text);
            }
            Some(misc::Union::SwitchDisplay(sd)) => {
                if let Some(info) = self.info.as_mut() {
                    info.current_display = sd.display;
                }
                self.set_display(DisplayInfo {
                    x: sd.x,
                    y: sd.y,
                    width: sd.width,
                    height: sd.height,
                    cursor_embedded: sd.cursor_embedded,
                    ..Default::default()
                });
            }
            Some(misc::Union::AudioFormat(f)) => {
                if let Some(a) = &self.audio {
                    a.format(f.sample_rate, f.channels);
                }
            }
            Some(misc::Union::PermissionInfo(p)) => {
                log::info!("permission {:?}: {}", p.permission.enum_value(), p.enabled)
            }
            Some(other) => log::trace!("ignoring misc {other:?}"),
            None => {}
        }
    }

    fn clipboard_from_peer(&mut self, text: String) {
        if self.last_clipboard.as_deref() == Some(text.as_str()) {
            return;
        }
        log::debug!("clipboard from the peer: {} chars", text.chars().count());
        self.last_clipboard = Some(text.clone());
        self.host.send(&ToCasement::Clipboard { text });
    }

    async fn clipboard_to_peer(&mut self, text: String, conn: &mut Stream) {
        let Some(info) = self.info.as_ref() else {
            self.pending_clipboard = Some(text);
            return;
        };
        if self.last_clipboard.as_deref() == Some(text.as_str()) {
            return;
        }
        let msg = media::clipboard_message(&text, &info.version, &info.platform);
        self.last_clipboard = Some(text);
        send(conn, msg).await;
    }

    async fn handle_casement(&mut self, msg: FromCasement, conn: &mut Stream) {
        match msg {
            FromCasement::Clipboard { text } => return self.clipboard_to_peer(text, conn).await,
            FromCasement::Mute { muted } => return self.mute(muted, conn).await,
            _ => {}
        }
        if self.info.is_none() {
            return;
        }
        let platform = self.platform();
        let (dx, dy) = (self.display.x, self.display.y);
        match msg {
            FromCasement::Key { scancode, down } => {
                if let Some(e) = self.keyboard.key(platform, scancode, down) {
                    let mut out = Message::new();
                    out.set_key_event(e);
                    send(conn, out).await;
                }
            }
            FromCasement::Pointer { x, y, buttons } => {
                for e in self.mouse.pointer(x + dx, y + dy, buttons) {
                    let mut out = Message::new();
                    out.set_mouse_event(e);
                    send(conn, out).await;
                }
            }
            FromCasement::Wheel { delta } => {
                let mut out = Message::new();
                out.set_mouse_event(self.mouse.wheel(delta));
                send(conn, out).await;
            }
            _ => {}
        }
    }

    /// Muting stops the sound here at once and asks the peer to stop
    /// sending it, as RustDesk's own client does; unmuting asks for it
    /// again. Before login the login asks for sound or not.
    async fn mute(&mut self, muted: bool, conn: &mut Stream) {
        if muted == self.muted {
            return;
        }
        self.muted = muted;
        if let Some(a) = &self.audio {
            a.set_muted(muted);
        }
        if self.info.is_some() {
            send(conn, sound_option(muted)).await;
        }
    }

    /// Ends the session from our side: release buttons, tell the peer, stop.
    async fn close(&mut self, conn: &mut Stream) {
        for e in self.mouse.release_all() {
            let mut out = Message::new();
            out.set_mouse_event(e);
            send(conn, out).await;
        }
        let mut misc = Misc::new();
        misc.set_close_reason(String::new());
        let mut out = Message::new();
        out.set_misc(misc);
        send(conn, out).await;
        self.stop_audio();
        log::info!("disconnected from {}", self.peer.id);
    }
}

/// `disable_audio` for an option message: sound unless muted.
fn sound(muted: bool) -> EnumOrUnknown<BoolOption> {
    EnumOrUnknown::new(if muted {
        BoolOption::Yes
    } else {
        BoolOption::No
    })
}

/// Asks the peer to stop or start sending sound.
fn sound_option(muted: bool) -> Message {
    let mut misc = Misc::new();
    misc.set_option(OptionMessage {
        disable_audio: sound(muted),
        ..Default::default()
    });
    let mut msg = Message::new();
    msg.set_misc(misc);
    msg
}

/// The login request: who we are, what we can decode and whether we want
/// sound.
fn login_message(peer_id: &str, password: Vec<u8>, muted: bool) -> Message {
    let option = OptionMessage {
        disable_audio: sound(muted),
        supported_decoding: MessageField::some(scrap::codec::Decoder::supported_decodings(
            None,
            false,
            None,
            &Vec::new(),
        )),
        ..Default::default()
    };
    let lr = LoginRequest {
        username: peer_id.to_owned(),
        password: password.into(),
        // This client has no RustDesk ID of its own.
        my_id: String::new(),
        my_name: format!("{} (Casement)", hbb_common::whoami::username()),
        my_platform: hbb_common::whoami::platform().to_string(),
        option: MessageField::some(option),
        session_id: hbb_common::rand::random(),
        version: super::RUSTDESK_VERSION.to_owned(),
        ..Default::default()
    };
    let mut msg = Message::new();
    msg.set_login_request(lr);
    msg
}

fn refresh_video() -> Message {
    let mut misc = Misc::new();
    misc.set_refresh_video(true);
    let mut msg = Message::new();
    msg.set_misc(misc);
    msg
}

async fn send(conn: &mut Stream, msg: Message) {
    if let Err(e) = conn.send(&msg).await {
        log::warn!("can't send to the peer: {e}");
    }
}

fn message_name(u: &message::Union) -> String {
    let s = format!("{u:?}");
    s.split(['(', ' ', '{']).next().unwrap_or("").to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_message_shape() {
        let msg = login_message("123", vec![1, 2, 3], false);
        let Some(message::Union::LoginRequest(lr)) = msg.union else {
            panic!()
        };
        assert_eq!(lr.username, "123");
        assert_eq!(&lr.password[..], &[1, 2, 3]);
        assert_eq!(lr.version, super::super::RUSTDESK_VERSION);
        let sd = &lr.option.supported_decoding;
        assert!(sd.ability_vp9 > 0 || sd.ability_vp8 > 0, "{sd:?}");
        assert_eq!(lr.option.disable_audio.enum_value(), Ok(BoolOption::No));
    }

    #[test]
    fn muted_login_asks_for_no_sound() {
        let msg = login_message("123", vec![], true);
        let Some(message::Union::LoginRequest(lr)) = msg.union else {
            panic!()
        };
        assert_eq!(lr.option.disable_audio.enum_value(), Ok(BoolOption::Yes));
    }

    #[test]
    fn mute_and_unmute_messages() {
        for muted in [true, false] {
            let Some(message::Union::Misc(m)) = sound_option(muted).union else {
                panic!()
            };
            let Some(misc::Union::Option(o)) = m.union else {
                panic!()
            };
            let want = if muted {
                BoolOption::Yes
            } else {
                BoolOption::No
            };
            assert_eq!(o.disable_audio.enum_value(), Ok(want));
            // Only sound: every other option stays as it is.
            let rest = OptionMessage {
                disable_audio: Default::default(),
                ..o
            };
            assert_eq!(rest, OptionMessage::default());
        }
    }
}
