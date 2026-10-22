//! Cursor shapes and clipboard text between RustDesk messages and the
//! Casement protocol.
//!
//! Cursor decoding follows `decode_cursor_data` in RustDesk's
//! `src/client/io_loop.rs`, clipboard encoding `src/clipboard.rs` (both at
//! the commit in [`super::RUSTDESK_COMMIT`]); RustDesk is © Purslane Ltd. and
//! contributors, AGPL-3.0.

use crate::protocol::Cursor;
use base::message_proto::{Clipboard, ClipboardFormat, CursorData, Message, MultiClipboards};
use hbb_common::get_version_number;
use std::collections::HashMap;

const MAX_CURSOR_SIZE: i32 = 512;
const MAX_CACHED_CURSORS: usize = 256;

/// Decodes a `CursorData` (zstd-compressed straight-alpha RGBA) into a
/// Casement cursor.
pub fn decode_cursor(cd: &CursorData) -> Result<Cursor, String> {
    if !(1..=MAX_CURSOR_SIZE).contains(&cd.width) || !(1..=MAX_CURSOR_SIZE).contains(&cd.height) {
        return Err(format!("invalid size {}x{}", cd.width, cd.height));
    }
    if !(0..cd.width).contains(&cd.hotx) || !(0..cd.height).contains(&cd.hoty) {
        return Err(format!(
            "hot spot {},{} outside the image",
            cd.hotx, cd.hoty
        ));
    }
    let expected = cd.width as usize * cd.height as usize * 4;
    if cd.colors.len() > zstd::zstd_safe::compress_bound(expected) {
        return Err("compressed data too large".into());
    }
    let rgba = zstd::bulk::decompress(&cd.colors, expected).map_err(|e| e.to_string())?;
    if rgba.len() != expected {
        return Err(format!("{} bytes of RGBA, expected {expected}", rgba.len()));
    }
    Ok(Cursor::image(
        cd.width as u32,
        cd.height as u32,
        cd.hotx,
        cd.hoty,
        &rgba,
    ))
}

/// A fully transparent cursor, for pictures that carry the cursor already.
pub fn hidden_cursor() -> Cursor {
    Cursor::image(1, 1, 0, 0, &[0, 0, 0, 0])
}

/// The peer's cursor shapes by id; RustDesk sends each shape once and then
/// refers to it by id.
#[derive(Default)]
pub struct Cursors {
    shapes: HashMap<u64, Cursor>,
}

impl Cursors {
    pub fn insert(&mut self, id: u64, cursor: Cursor) {
        if self.shapes.len() >= MAX_CACHED_CURSORS && !self.shapes.contains_key(&id) {
            self.shapes.clear();
        }
        self.shapes.insert(id, cursor);
    }

    pub fn get(&self, id: u64) -> Option<&Cursor> {
        self.shapes.get(&id)
    }
}

/// Text from a clipboard entry, if it is text.
pub fn clipboard_text(cb: &Clipboard) -> Option<String> {
    if cb.format.enum_value() != Ok(ClipboardFormat::Text) {
        return None;
    }
    let data = if cb.compress {
        hbb_common::compress::decompress(&cb.content)
    } else {
        cb.content.to_vec()
    };
    String::from_utf8(data).ok()
}

/// Text from whichever entry of a multi-clipboard is text.
pub fn multi_clipboard_text(mcb: &MultiClipboards) -> Option<String> {
    mcb.clipboards.iter().find_map(clipboard_text)
}

fn text_entry(text: &str) -> Clipboard {
    let compressed = hbb_common::compress::compress(text.as_bytes());
    let compress = !compressed.is_empty() && compressed.len() < text.len();
    Clipboard {
        compress,
        content: if compress {
            compressed
        } else {
            text.as_bytes().to_vec()
        }
        .into(),
        format: ClipboardFormat::Text.into(),
        ..Default::default()
    }
}

/// A clipboard message for the peer: `MultiClipboards` for peers from 1.3.0
/// on (1.3.3 on Android), the older single `Clipboard` before.
pub fn clipboard_message(text: &str, peer_version: &str, peer_platform: &str) -> Message {
    let v = get_version_number(peer_version);
    let multi = v >= get_version_number("1.3.0")
        && !peer_platform.is_empty()
        && peer_platform != "iOS"
        && !(peer_platform == "Android" && v < get_version_number("1.3.3"));
    let mut msg = Message::new();
    if multi {
        msg.set_multi_clipboards(MultiClipboards {
            clipboards: vec![text_entry(text)],
            ..Default::default()
        });
    } else {
        msg.set_clipboard(text_entry(text));
    }
    msg
}

#[cfg(test)]
mod tests {
    use super::*;
    use base::message_proto::message;

    #[test]
    fn cursor_round_trip() {
        let rgba: Vec<u8> = (0..2 * 3 * 4).map(|i| i as u8).collect();
        let cd = CursorData {
            id: 7,
            hotx: 1,
            hoty: 2,
            width: 2,
            height: 3,
            colors: zstd::bulk::compress(&rgba, 3).unwrap().into(),
            ..Default::default()
        };
        assert_eq!(
            decode_cursor(&cd).unwrap(),
            Cursor::image(2, 3, 1, 2, &rgba)
        );
        let bad = CursorData {
            hotx: 5,
            ..cd.clone()
        };
        assert!(decode_cursor(&bad).is_err());
        let short = CursorData {
            colors: zstd::bulk::compress(&rgba[..8], 3).unwrap().into(),
            ..cd
        };
        assert!(decode_cursor(&short).is_err());
    }

    #[test]
    fn clipboard_both_ways() {
        let text = "hello ".repeat(100);
        let msg = clipboard_message(&text, "1.4.0", "Windows");
        let Some(message::Union::MultiClipboards(m)) = msg.union else {
            panic!()
        };
        assert!(m.clipboards[0].compress);
        assert_eq!(multi_clipboard_text(&m).as_deref(), Some(text.as_str()));

        let msg = clipboard_message("hi", "1.2.0", "Windows");
        let Some(message::Union::Clipboard(c)) = msg.union else {
            panic!()
        };
        assert!(!c.compress);
        assert_eq!(clipboard_text(&c).as_deref(), Some("hi"));

        let png = Clipboard {
            format: ClipboardFormat::ImagePng.into(),
            ..Default::default()
        };
        assert_eq!(clipboard_text(&png), None);
    }
}
