//! RustDesk peers through a self-hosted ID server and relay.
//!
//! The protocol work reuses RustDesk's own crates (`hbb_common` for the
//! rendezvous protocol, framing and encryption, `base` for the message
//! protocol, `scrap` for the video decoders, `magnum-opus` for the sound)
//! and code adapted from RustDesk's client, all from
//! <https://github.com/rustdesk/rustdesk> at [`RUSTDESK_COMMIT`]. RustDesk is © Purslane Ltd. and contributors and
//! licensed under the AGPL-3.0, as is this program.

mod audio;
mod client;
mod connect;
mod crypto;
mod input;
mod media;
mod video;

use crate::address::PeerAddress;
use crate::host::Host;
use crate::protocol::FromCasement;
use crate::session::Session;
use client::Event;
use hbb_common::tokio;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

/// The RustDesk commit the reused code and crates come from.
pub const RUSTDESK_COMMIT: &str = "c7f212df2c960fa74bd83d3849a5a7ce7809a047";

/// The RustDesk version this client tells peers it speaks: that of
/// [`RUSTDESK_COMMIT`]. Peers use it to decide which messages to send.
pub const RUSTDESK_VERSION: &str = "1.5.1";

/// RustDesk's crates keep settings in a config folder named after the app.
/// Point it at our own name so a RustDesk installed on the same computer is
/// neither read nor changed.
const CONFIG_APP_NAME: &str = "casement-rustdesk";

pub struct RustDeskSession {
    events: tokio::sync::mpsc::UnboundedSender<Event>,
    finished: mpsc::Receiver<()>,
}

pub fn start(host: Arc<Host>, peer: PeerAddress, password: String) -> Box<dyn Session> {
    if let Ok(mut name) = hbb_common::config::APP_NAME.write() {
        *name = CONFIG_APP_NAME.to_owned();
    }
    let (events, rx) = tokio::sync::mpsc::unbounded_channel();
    let (done, finished) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("rustdesk".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => host.finish(
                    crate::protocol::State::Failed,
                    &format!("Couldn't start: {e}"),
                ),
            };
            rt.block_on(client::run(host, peer, password, rx));
            // Leave whatever is still pending (a stuck connect) behind.
            rt.shutdown_background();
            let _ = done.send(());
        });
    if let Err(e) = spawned {
        log::error!("can't start the session thread: {e}");
    }
    Box::new(RustDeskSession { events, finished })
}

impl Session for RustDeskSession {
    fn input(&mut self, msg: &FromCasement) {
        let _ = self.events.send(Event::Casement(msg.clone()));
    }

    fn disconnect(&mut self) {
        if self.events.send(Event::Disconnect).is_ok() {
            // Casement ends the process two seconds after `disconnect`.
            let _ = self.finished.recv_timeout(Duration::from_millis(1500));
        }
    }
}
