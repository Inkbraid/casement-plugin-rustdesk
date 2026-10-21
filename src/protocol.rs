//! Casement plugin protocol, version 1 (see `docs/plugin-protocol.md`).
//!
//! Each message is a 4-byte little-endian length followed by that many bytes of
//! UTF-8 JSON: one object with a `type` member. Unknown types and members are
//! ignored, so both enums end in an `Unknown` variant and nothing denies
//! unknown fields.

use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

/// The protocol version this plugin speaks.
pub const PROTOCOL_VERSION: u32 = 1;

/// Largest message either side may send.
pub const MAX_MESSAGE: usize = 16 << 20;

/// Capabilities declared in our `hello`.
pub const CAPABILITIES: &[&str] = &["clipboard", "cursor"];

/// Reads one message body. `Ok(None)` means the stream ended cleanly between
/// messages.
pub fn read_frame<R: Read>(r: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    let mut got = 0;
    while got < 4 {
        match r.read(&mut len[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_MESSAGE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("message of {len} bytes is over the 16 MiB limit"),
        ));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    Ok(Some(body))
}

/// Writes one message body with its length prefix and flushes.
pub fn write_frame<W: Write>(w: &mut W, body: &[u8]) -> io::Result<()> {
    if body.len() > MAX_MESSAGE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("message of {} bytes is over the 16 MiB limit", body.len()),
        ));
    }
    w.write_all(&(body.len() as u32).to_le_bytes())?;
    w.write_all(body)?;
    w.flush()
}

/// Reads and decodes one message. `Ok(None)` at a clean end of stream.
pub fn read_message<R: Read, T: for<'de> Deserialize<'de>>(r: &mut R) -> io::Result<Option<T>> {
    match read_frame(r)? {
        None => Ok(None),
        Some(body) => serde_json::from_slice(&body)
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
    }
}

/// Encodes and writes one message.
pub fn write_message<W: Write, T: Serialize>(w: &mut W, msg: &T) -> io::Result<()> {
    let body =
        serde_json::to_vec(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    write_frame(w, &body)
}

/// Messages Casement sends to the plugin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromCasement {
    Hello {
        protocol: u32,
        #[serde(default)]
        app: String,
    },
    Connect {
        address: String,
        #[serde(default)]
        username: String,
        #[serde(default)]
        password: String,
        #[serde(default)]
        title: String,
    },
    Surface {
        id: u32,
        handle: u64,
        width: u32,
        height: u32,
        stride: u32,
        size: u64,
    },
    FrameAck {
        surface: u32,
    },
    Key {
        scancode: u32,
        down: bool,
    },
    Pointer {
        x: i32,
        y: i32,
        buttons: u32,
    },
    Wheel {
        delta: i32,
    },
    Resize {
        width: u32,
        height: u32,
    },
    Mute {
        muted: bool,
    },
    Clipboard {
        text: String,
    },
    Disconnect,
    #[serde(other)]
    Unknown,
}

/// The `state` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Connected,
    AuthFailed,
    Failed,
    Closed,
}

/// Messages the plugin sends to Casement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToCasement {
    Hello {
        protocol: u32,
        name: String,
        version: String,
        capabilities: Vec<String>,
    },
    State {
        state: State,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        text: String,
    },
    SurfaceRequest {
        width: u32,
        height: u32,
    },
    Frame {
        surface: u32,
        rects: Vec<[u32; 4]>,
    },
    Cursor(Cursor),
    CursorPos {
        x: i32,
        y: i32,
        visible: bool,
    },
    Clipboard {
        text: String,
    },
    #[serde(other)]
    Unknown,
}

/// A `cursor` message: either `{"default": true}` or an RGBA image.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Cursor {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub default: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hot_x: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hot_y: Option<i32>,
    /// Base64 of `width * height * 4` bytes of straight-alpha RGBA.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rgba: Option<String>,
}

impl Cursor {
    pub fn default_arrow() -> Self {
        Cursor {
            default: true,
            ..Default::default()
        }
    }

    pub fn image(width: u32, height: u32, hot_x: i32, hot_y: i32, rgba: &[u8]) -> Self {
        use base64::Engine as _;
        Cursor {
            default: false,
            width: Some(width),
            height: Some(height),
            hot_x: Some(hot_x),
            hot_y: Some(hot_y),
            rgba: Some(base64::engine::general_purpose::STANDARD.encode(rgba)),
        }
    }
}

impl ToCasement {
    pub fn hello() -> Self {
        ToCasement::Hello {
            protocol: PROTOCOL_VERSION,
            name: "RustDesk".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            capabilities: CAPABILITIES.iter().map(|c| c.to_string()).collect(),
        }
    }

    pub fn state(state: State, text: impl Into<String>) -> Self {
        ToCasement::State {
            state,
            text: text.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decode(v: serde_json::Value) -> FromCasement {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn framing_round_trip() {
        let mut buf = Vec::new();
        write_message(&mut buf, &ToCasement::hello()).unwrap();
        let len = u32::from_le_bytes(buf[..4].try_into().unwrap()) as usize;
        assert_eq!(len, buf.len() - 4);
        let mut r = &buf[..];
        let back: ToCasement = read_message(&mut r).unwrap().unwrap();
        assert_eq!(back, ToCasement::hello());
        assert!(read_frame(&mut r).unwrap().is_none());
    }

    #[test]
    fn truncated_and_oversized_frames_are_errors() {
        let mut r: &[u8] = &[1, 0];
        assert!(read_frame(&mut r).is_err());
        let mut r: &[u8] = &[5, 0, 0, 0, b'{'];
        assert!(read_frame(&mut r).is_err());
        let big = ((MAX_MESSAGE + 1) as u32).to_le_bytes();
        let mut r: &[u8] = &big;
        assert!(read_frame(&mut r).is_err());
    }

    #[test]
    fn hello_matches_spec() {
        let v = serde_json::to_value(ToCasement::hello()).unwrap();
        assert_eq!(
            v,
            json!({"type": "hello", "protocol": 1, "name": "RustDesk",
                   "version": env!("CARGO_PKG_VERSION"), "capabilities": ["clipboard", "cursor"]})
        );
    }

    #[test]
    fn decodes_casement_messages() {
        assert_eq!(
            decode(json!({"type": "hello", "protocol": 1, "app": "Casement 0.7.1"})),
            FromCasement::Hello {
                protocol: 1,
                app: "Casement 0.7.1".into()
            }
        );
        assert_eq!(
            decode(json!({"type": "connect", "address": "rustdesk://test", "password": "x"})),
            FromCasement::Connect {
                address: "rustdesk://test".into(),
                username: String::new(),
                password: "x".into(),
                title: String::new()
            }
        );
        assert_eq!(
            decode(
                json!({"type": "surface", "id": 3, "handle": 1234, "width": 1920,
                          "height": 1080, "stride": 7680, "size": 8294400})
            ),
            FromCasement::Surface {
                id: 3,
                handle: 1234,
                width: 1920,
                height: 1080,
                stride: 7680,
                size: 8294400
            }
        );
        assert_eq!(
            decode(json!({"type": "key", "scancode": 0x11D, "down": true})),
            FromCasement::Key {
                scancode: 0x11D,
                down: true
            }
        );
        assert_eq!(
            decode(json!({"type": "disconnect"})),
            FromCasement::Disconnect
        );
    }

    #[test]
    fn unknown_types_and_members_are_ignored() {
        assert_eq!(
            decode(json!({"type": "something_new", "x": 1})),
            FromCasement::Unknown
        );
        assert_eq!(
            decode(json!({"type": "wheel", "delta": -2, "horizontal": false})),
            FromCasement::Wheel { delta: -2 }
        );
    }

    #[test]
    fn outgoing_shapes_match_spec() {
        let v = |m: ToCasement| serde_json::to_value(m).unwrap();
        assert_eq!(
            v(ToCasement::state(State::AuthFailed, "")),
            json!({"type": "state", "state": "auth_failed"})
        );
        assert_eq!(
            v(ToCasement::state(State::Failed, "No route")),
            json!({"type": "state", "state": "failed", "text": "No route"})
        );
        assert_eq!(
            v(ToCasement::SurfaceRequest {
                width: 1920,
                height: 1080
            }),
            json!({"type": "surface_request", "width": 1920, "height": 1080})
        );
        assert_eq!(
            v(ToCasement::Frame {
                surface: 3,
                rects: vec![[1, 2, 3, 4]]
            }),
            json!({"type": "frame", "surface": 3, "rects": [[1, 2, 3, 4]]})
        );
        assert_eq!(
            v(ToCasement::Cursor(Cursor::default_arrow())),
            json!({"type": "cursor", "default": true})
        );
        assert_eq!(
            v(ToCasement::Cursor(Cursor::image(1, 1, 0, 0, &[1, 2, 3, 4]))),
            json!({"type": "cursor", "width": 1, "height": 1, "hot_x": 0, "hot_y": 0, "rgba": "AQIDBA=="})
        );
        assert_eq!(
            v(ToCasement::CursorPos {
                x: 100,
                y: 200,
                visible: true
            }),
            json!({"type": "cursor_pos", "x": 100, "y": 200, "visible": true})
        );
    }
}
