//! RustDesk support for Casement: a Casement protocol plugin (protocol v1)
//! that shows RustDesk peers behind a self-hosted ID/relay server.

pub mod address;
pub mod display;
pub mod host;
pub mod protocol;
pub mod rustdesk;
pub mod session;
pub mod sim;
pub mod sound;
pub mod stdio;
pub mod surface;
pub mod test_pattern;

use address::Address;
use host::Host;
use protocol::{read_frame, FromCasement, State, ToCasement, PROTOCOL_VERSION};
use std::io::Read;
use std::sync::Arc;

/// Reads the next message, skipping undecodable ones. `None` when the stream
/// ends or breaks.
fn next_message(input: &mut impl Read) -> Option<FromCasement> {
    loop {
        match read_frame(input) {
            Ok(None) => return None,
            Ok(Some(body)) => match serde_json::from_slice::<FromCasement>(&body) {
                Ok(msg) => return Some(msg),
                Err(e) => log::warn!("ignoring a message I can't read: {e}"),
            },
            Err(e) => {
                log::error!("can't read from Casement: {e}");
                return None;
            }
        }
    }
}

/// Runs the plugin over `input` (Casement's messages) until the tab ends.
/// Returns the exit code.
pub fn run(host: Arc<Host>, mut input: impl Read) -> i32 {
    // Start: hello, then connect.
    let connect = loop {
        match next_message(&mut input) {
            None => return 0,
            Some(FromCasement::Hello { protocol, app }) => {
                log::info!(
                    "{app} offers protocol {protocol}; casement-rustdesk {} speaks {PROTOCOL_VERSION}",
                    env!("CARGO_PKG_VERSION")
                );
                host.send(&ToCasement::hello());
            }
            Some(c @ FromCasement::Connect { .. }) => break c,
            Some(FromCasement::Disconnect) => return 0,
            Some(other) => log::debug!("before connect, ignoring {other:?}"),
        }
    };
    let FromCasement::Connect {
        address, password, ..
    } = connect
    else {
        unreachable!()
    };

    let mut session = match address::parse(&address) {
        Err(text) => host.finish(State::Failed, &text),
        Ok(Address::Test(opts)) => test_pattern::start(host.clone(), opts, &password),
        Ok(Address::Peer(peer)) => {
            log::info!(
                "peer {} via {} (RustDesk code from {})",
                peer.id,
                peer.server,
                rustdesk::RUSTDESK_COMMIT
            );
            rustdesk::start(host.clone(), peer, password)
        }
    };

    loop {
        match next_message(&mut input) {
            None | Some(FromCasement::Disconnect) => {
                session.disconnect();
                return 0;
            }
            Some(m @ (FromCasement::Surface { .. } | FromCasement::FrameAck { .. })) => {
                host.display().handle(&m)
            }
            Some(
                FromCasement::Hello { .. } | FromCasement::Connect { .. } | FromCasement::Unknown,
            ) => {}
            Some(m) => session.input(&m),
        }
    }
}
