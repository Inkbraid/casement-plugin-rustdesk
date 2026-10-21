//! Reaching a peer: rendezvous with the ID server (hbbs), a relayed
//! connection through hbbr, and the handshake that verifies the peer
//! against the server's key and encrypts the stream.
//!
//! Adapted from RustDesk's `src/client.rs` (`Client::_start_inner`,
//! `request_relay`, `create_relay`, `secure_connection`) at the commit in
//! [`super::RUSTDESK_COMMIT`]; RustDesk is © Purslane Ltd. and contributors,
//! AGPL-3.0. Only the relayed path is kept (no UDP/TCP hole punching, KCP,
//! IPv6 or WebRTC yet), and the handshake fails closed: where RustDesk falls
//! back to an unencrypted connection when the peer's key can't be verified,
//! this refuses to connect.

use super::crypto;
use crate::address::{with_default_port, PeerAddress, RELAY_PORT};
use base::message_proto::{message, Message, PublicKey};
use hbb_common::protobuf::Message as _;
use hbb_common::rendezvous_proto::{
    punch_hole_response, rendezvous_message, ConnType, NatType, PunchHoleRequest,
    RendezvousMessage, RequestRelay,
};
use hbb_common::sodiumoxide::crypto::sign;
use hbb_common::stream::Stream;
use hbb_common::tcp::{FramedStream, KxTranscript};
use std::time::{Duration, Instant};

const CONNECT_TIMEOUT: u64 = 18_000;
const READ_TIMEOUT: u64 = 18_000;

/// A connection that failed, with a sentence for the user.
pub type Failure = String;

/// Connects to `peer` through the relay and completes the secure handshake.
pub async fn connect(peer: &PeerAddress) -> Result<Stream, Failure> {
    let rs_pk = crypto::server_key(&peer.key)
        .ok_or_else(|| "The server key in the address isn't valid.".to_string())?;
    let mut rv = tcp(&peer.server)
        .await
        .map_err(|e| format!("Couldn't reach the ID server {}: {e}", peer.server))?;
    log::info!("connected to the ID server {}", peer.server);

    let mut req = RendezvousMessage::new();
    req.set_punch_hole_request(PunchHoleRequest {
        id: peer.id.clone(),
        nat_type: NatType::SYMMETRIC.into(),
        licence_key: peer.key.clone(),
        conn_type: ConnType::DEFAULT_CONN.into(),
        version: super::RUSTDESK_VERSION.to_owned(),
        force_relay: true,
        ..Default::default()
    });

    // How hbbs answers a relay-only request: RelayResponse once the peer
    // has joined the relay, or PunchHoleResponse when it wants us to ask for
    // a relay ourselves (peers on the same LAN as each other).
    enum Answer {
        Relay {
            uuid: String,
            relay: String,
            signed_pk: Vec<u8>,
        },
        Punch {
            relay: String,
            signed_pk: Vec<u8>,
        },
    }
    let mut answer = None;
    'attempts: for attempt in 1..=3u64 {
        log::info!("#{attempt} relay request for {}", peer.id);
        send(&mut rv, &req)
            .await
            .map_err(|e| format!("The ID server dropped the connection: {e}"))?;
        let deadline = Instant::now() + Duration::from_millis(attempt * 3000);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let Some(msg) = next_rendezvous(&mut rv, left).await else {
                break;
            };
            match msg.union {
                Some(rendezvous_message::Union::PunchHoleResponse(ph)) => {
                    if ph.socket_addr.is_empty() {
                        return Err(punch_failure(&ph, peer));
                    }
                    answer = Some(Answer::Punch {
                        relay: ph.relay_server.clone(),
                        signed_pk: ph.pk.to_vec(),
                    });
                    break 'attempts;
                }
                Some(rendezvous_message::Union::RelayResponse(rr)) => {
                    if !rr.refuse_reason.is_empty() {
                        return Err(format!(
                            "The ID server refused the relay: {}",
                            rr.refuse_reason
                        ));
                    }
                    answer = Some(Answer::Relay {
                        signed_pk: rr.pk().to_vec(),
                        uuid: rr.uuid,
                        relay: rr.relay_server,
                    });
                    break 'attempts;
                }
                Some(rendezvous_message::Union::KeyExchange(_)) => {}
                other => log::debug!("ignoring {other:?} from the ID server"),
            }
        }
    }
    let answer = answer.ok_or_else(|| {
        format!(
            "{} didn't answer through the ID server in time. Is it online and using this server?",
            peer.id
        )
    })?;

    // Verify what the ID server says about the peer before going on.
    let (signed_pk, mut conn) = match answer {
        Answer::Relay {
            uuid,
            relay,
            signed_pk,
        } => {
            let relay = relay_address(peer, &relay)?;
            (signed_pk, join_relay(peer, &relay, uuid).await?)
        }
        Answer::Punch { relay, signed_pk } => {
            drop(rv);
            let relay = relay_for_request(peer, &relay)?;
            let uuid = request_relay(peer, &relay, !signed_pk.is_empty()).await?;
            let ours = relay_address(peer, &relay)?;
            (signed_pk, join_relay(peer, &ours, uuid).await?)
        }
    };
    secure(&mut conn, &peer.id, &signed_pk, &rs_pk).await?;
    Ok(conn)
}

fn punch_failure(
    ph: &hbb_common::rendezvous_proto::PunchHoleResponse,
    peer: &PeerAddress,
) -> Failure {
    if !ph.other_failure.is_empty() {
        return format!("The ID server said: {}", ph.other_failure);
    }
    use punch_hole_response::Failure as F;
    match ph.failure.enum_value() {
        Ok(F::ID_NOT_EXIST) => format!("The ID server doesn't know the RustDesk ID {}.", peer.id),
        Ok(F::OFFLINE) => format!("{} is offline.", peer.id),
        Ok(F::LICENSE_MISMATCH) => {
            "The ID server rejected the key in the address; it doesn't match the server's key."
                .into()
        }
        Ok(F::LICENSE_OVERUSE) => "The ID server refused the connection (key overuse).".into(),
        _ => "The ID server couldn't set up the connection.".into(),
    }
}

/// The relay we connect to: `relay=` from the address, else the one hbbs
/// named, else hbbs's host on the relay port.
fn relay_address(peer: &PeerAddress, named: &str) -> Result<String, Failure> {
    if let Some(r) = &peer.relay {
        return Ok(r.clone());
    }
    relay_for_request(peer, named)
}

/// The relay as the peer must reach it: the one hbbs named, else hbbs's host.
fn relay_for_request(peer: &PeerAddress, named: &str) -> Result<String, Failure> {
    let named = named.trim();
    if !named.is_empty() {
        return with_default_port(named, RELAY_PORT);
    }
    if let Some(r) = &peer.relay {
        return Ok(r.clone());
    }
    Ok(format!("{}:{RELAY_PORT}", server_host(&peer.server)))
}

/// The host part of `host:port` or `[v6]:port`.
fn server_host(server: &str) -> &str {
    match server.rsplit_once(':') {
        Some((host, port)) if port.parse::<u16>().is_ok() => host,
        _ => server,
    }
}

/// Asks hbbs to pair us with the peer on `relay`; returns the pairing uuid.
async fn request_relay(peer: &PeerAddress, relay: &str, secure: bool) -> Result<String, Failure> {
    for attempt in 1..=3 {
        // hbbs wants a fresh connection (and address) for each attempt.
        let mut rv = tcp(&peer.server)
            .await
            .map_err(|e| format!("Couldn't reach the ID server {}: {e}", peer.server))?;
        let uuid = hbb_common::uuid::Uuid::new_v4().to_string();
        log::info!("#{attempt} asking the ID server for relay {relay}, uuid {uuid}");
        let mut msg = RendezvousMessage::new();
        msg.set_request_relay(RequestRelay {
            id: peer.id.clone(),
            uuid: uuid.clone(),
            relay_server: relay.to_owned(),
            secure,
            ..Default::default()
        });
        send(&mut rv, &msg)
            .await
            .map_err(|e| format!("The ID server dropped the connection: {e}"))?;
        if let Some(m) = next_rendezvous(&mut rv, Duration::from_millis(CONNECT_TIMEOUT)).await {
            if let Some(rendezvous_message::Union::RelayResponse(rr)) = m.union {
                if !rr.refuse_reason.is_empty() {
                    return Err(format!(
                        "The ID server refused the relay: {}",
                        rr.refuse_reason
                    ));
                }
                return Ok(uuid);
            }
        }
    }
    Err("The ID server didn't set up a relay in time.".into())
}

/// Connects to hbbr and asks to be paired by `uuid`.
async fn join_relay(peer: &PeerAddress, relay: &str, uuid: String) -> Result<Stream, Failure> {
    log::info!("joining relay {relay}");
    let mut conn = tcp(relay)
        .await
        .map_err(|e| format!("Couldn't reach the relay server {relay}: {e}"))?;
    let mut msg = RendezvousMessage::new();
    msg.set_request_relay(RequestRelay {
        licence_key: peer.key.clone(),
        id: peer.id.clone(),
        uuid,
        conn_type: ConnType::DEFAULT_CONN.into(),
        ..Default::default()
    });
    send(&mut conn, &msg)
        .await
        .map_err(|e| format!("The relay server dropped the connection: {e}"))?;
    Ok(conn)
}

/// The peer handshake: the ID server vouches for the peer's long-term key,
/// the peer signs an ephemeral key with it, and we seal a session key to that.
/// Anything that doesn't verify ends the connection.
async fn secure(
    conn: &mut Stream,
    peer_id: &str,
    signed_pk: &[u8],
    rs_pk: &sign::PublicKey,
) -> Result<(), Failure> {
    let refused = |why: &str| {
        log::error!("handshake refused: {why}");
        format!(
            "{peer_id}'s identity couldn't be verified against the server key ({why}), so the connection was refused."
        )
    };
    if signed_pk.is_empty() {
        return Err(refused("the ID server sent no key for it"));
    }
    let vouched = crypto::open_id_pk(signed_pk, rs_pk)
        .map_err(|_| refused("the ID server's signature doesn't match the key"))?;
    if vouched.id != peer_id {
        return Err(refused("the ID server vouched for another ID"));
    }
    let peer_sign_pk = sign::PublicKey(vouched.pk);

    let bytes = match hbb_common::timeout(READ_TIMEOUT, conn.next()).await {
        Err(_) => {
            return Err(format!(
                "{peer_id} didn't answer through the relay in time."
            ))
        }
        Ok(None) => {
            return Err(format!(
                "{peer_id} closed the connection before it was set up."
            ))
        }
        Ok(Some(Err(e))) => return Err(format!("The connection to {peer_id} broke: {e}")),
        Ok(Some(Ok(b))) => b,
    };
    let msg = Message::parse_from_bytes(&bytes)
        .map_err(|_| refused("its first message isn't readable"))?;
    let Some(message::Union::SignedId(si)) = msg.union else {
        return Err(refused("it didn't sign its session key"));
    };
    let theirs = crypto::open_id_pk(&si.id, &peer_sign_pk)
        .map_err(|_| refused("its session key isn't signed by its key"))?;
    if theirs.id != peer_id {
        return Err(refused("it signed for another ID"));
    }

    let (our_pk, sealed, key) = crypto::sealed_session_key(theirs.pk);
    let picked = hbb_common::tcp::kx_version_for(theirs.kx_version);
    let mut out = Message::new();
    out.set_public_key(PublicKey {
        asymmetric_value: our_pk.clone(),
        symmetric_value: sealed,
        kx_version: picked,
        ..Default::default()
    });
    send(conn, &out)
        .await
        .map_err(|e| format!("The connection to {peer_id} broke: {e}"))?;
    conn.set_negotiated_key(
        key,
        true,
        &KxTranscript {
            initiator_pk: &our_pk,
            responder_pk: &theirs.pk,
            advertised: theirs.kx_version,
            picked,
        },
    )
    .map_err(|e| format!("Couldn't set up encryption with {peer_id}: {e}"))?;
    log::info!("secured with {peer_id} (key exchange version {picked})");
    Ok(())
}

async fn tcp(addr: &str) -> hbb_common::ResultType<Stream> {
    Ok(Stream::Tcp(
        FramedStream::new(addr, None, CONNECT_TIMEOUT).await?,
    ))
}

async fn send(
    conn: &mut Stream,
    msg: &impl hbb_common::protobuf::Message,
) -> hbb_common::ResultType<()> {
    hbb_common::timeout(CONNECT_TIMEOUT, conn.send(msg)).await?
}

/// The next rendezvous message that isn't a key exchange offer.
async fn next_rendezvous(conn: &mut Stream, wait: Duration) -> Option<RendezvousMessage> {
    let deadline = Instant::now() + wait;
    loop {
        let left = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as u64;
        if left == 0 {
            return None;
        }
        let bytes = conn.next_timeout(left).await?.ok()?;
        let msg = RendezvousMessage::parse_from_bytes(&bytes).ok()?;
        if !matches!(msg.union, Some(rendezvous_message::Union::KeyExchange(_))) {
            return Some(msg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(relay: Option<&str>) -> PeerAddress {
        PeerAddress {
            id: "1".into(),
            server: "hbbs.example.com:21116".into(),
            relay: relay.map(Into::into),
            key: String::new(),
        }
    }

    #[test]
    fn relay_choice() {
        let p = peer(None);
        assert_eq!(relay_address(&p, "").unwrap(), "hbbs.example.com:21117");
        assert_eq!(relay_address(&p, "hbbr").unwrap(), "hbbr:21117");
        assert_eq!(relay_address(&p, "hbbr:9000").unwrap(), "hbbr:9000");
        let p = peer(Some("127.0.0.1:21117"));
        assert_eq!(relay_address(&p, "hbbr").unwrap(), "127.0.0.1:21117");
        assert_eq!(relay_for_request(&p, "hbbr").unwrap(), "hbbr:21117");
        assert_eq!(relay_for_request(&p, "").unwrap(), "127.0.0.1:21117");
        assert_eq!(server_host("[fd00::1]:21116"), "[fd00::1]");
    }
}
