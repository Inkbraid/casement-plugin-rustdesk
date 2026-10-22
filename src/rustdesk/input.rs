//! Casement's keyboard and mouse input as RustDesk messages.
//!
//! Keys use RustDesk's Map mode: the key's position, as the code the peer's
//! platform uses, so the peer's own layout decides the character. This
//! mirrors `_map_keyboard_mode` in RustDesk's `src/keyboard.rs` for a Windows
//! client (Casement sends PC set-1 scancodes, which is what Windows reports),
//! using the same rdev key tables. RustDesk is © Purslane Ltd. and
//! contributors, AGPL-3.0.

use base::message_proto::{ControlKey, KeyEvent, KeyboardMode, MouseEvent};
use rdev::Key;

// Mouse masks: the low three bits are the event type, the buttons above.
pub const MOUSE_TYPE_MOVE: i32 = 0;
pub const MOUSE_TYPE_DOWN: i32 = 1;
pub const MOUSE_TYPE_UP: i32 = 2;
pub const MOUSE_TYPE_WHEEL: i32 = 3;
pub const MOUSE_BUTTON_LEFT: i32 = 0x01;
pub const MOUSE_BUTTON_RIGHT: i32 = 0x02;
pub const MOUSE_BUTTON_WHEEL: i32 = 0x04;

/// The peer's platform, from `PeerInfo.platform`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Windows,
    MacOs,
    Linux,
    Android,
}

impl Platform {
    pub fn from_peer(platform: &str) -> Self {
        // RustDesk sends "Windows", "Linux", "Mac OS", "Android".
        match platform.to_ascii_lowercase().replace(' ', "").as_str() {
            "windows" => Platform::Windows,
            "macos" => Platform::MacOs,
            "android" => Platform::Android,
            _ => Platform::Linux,
        }
    }
}

// macOS kVK_JIS_Eisu and kVK_JIS_Kana (rdev exports them on macOS only).
const KVK_JIS_EISU: u32 = 0x66;
const KVK_JIS_KANA: u32 = 0x68;

const PAUSE_PREFIX: u32 = 0x21D;
const PAUSE: u32 = 0x45;

/// Turns Casement `key` messages into `KeyEvent`s.
#[derive(Default)]
pub struct Keyboard {
    /// Casement sends Pause as 0x21D followed by 0x45.
    pause_prefix: bool,
    pause_down: bool,
}

impl Keyboard {
    pub fn key(&mut self, platform: Platform, scancode: u32, down: bool) -> Option<KeyEvent> {
        if scancode == PAUSE_PREFIX {
            self.pause_prefix = down;
            return None;
        }
        if scancode == PAUSE && (self.pause_prefix || (!down && self.pause_down)) {
            self.pause_prefix = false;
            self.pause_down = down;
            return pause(platform, down);
        }
        let win = windows_scancode(scancode)?;
        let code = match platform {
            Platform::Windows => win,
            other => code_for(other, rdev::win_key_from_scancode(win))?,
        };
        Some(map_event(code, down))
    }
}

/// Casement's scancode (0x100 for E0) as Windows writes it (0xE000 for E0).
fn windows_scancode(scancode: u32) -> Option<u32> {
    match scancode >> 8 {
        0 => Some(scancode),
        1 => Some(0xE000 | (scancode & 0xFF)),
        _ => None,
    }
}

fn code_for(platform: Platform, key: Key) -> Option<u32> {
    if matches!(key, Key::Unknown(_) | Key::RawKey(_)) {
        return None;
    }
    match platform {
        Platform::Windows => rdev::win_scancode_from_key(key),
        Platform::MacOs => match key {
            // JIS conversion keys sit where macOS has its input-mode keys.
            Key::NonConvert => Some(KVK_JIS_EISU),
            Key::Convert => Some(KVK_JIS_KANA),
            _ => rdev::macos_keycode_from_key(key),
        },
        Platform::Android => rdev::android_keycode_from_key(key),
        Platform::Linux => rdev::linux_keycode_from_key(key),
    }
}

fn map_event(code: u32, down: bool) -> KeyEvent {
    let mut e = KeyEvent::new();
    e.mode = KeyboardMode::Map.into();
    e.down = down;
    e.set_chr(code);
    e
}

/// Pause has no ordinary scancode on Windows, so RustDesk sends it to
/// Windows peers as a legacy control key.
fn pause(platform: Platform, down: bool) -> Option<KeyEvent> {
    if platform == Platform::Windows {
        let mut e = KeyEvent::new();
        e.mode = KeyboardMode::Legacy.into();
        e.down = down;
        e.set_control_key(ControlKey::Pause);
        return Some(e);
    }
    code_for(platform, Key::Pause).map(|c| map_event(c, down))
}

/// Turns Casement `pointer` and `wheel` messages into `MouseEvent`s.
#[derive(Default)]
pub struct Mouse {
    buttons: u32,
    pos: Option<(i32, i32)>,
}

impl Mouse {
    /// `x`, `y` are in the peer's screen coordinates already.
    pub fn pointer(&mut self, x: i32, y: i32, buttons: u32) -> Vec<MouseEvent> {
        let mut out = Vec::new();
        let changed = self.buttons ^ buttons;
        if self.pos != Some((x, y)) || changed == 0 {
            out.push(mouse(MOUSE_TYPE_MOVE, x, y));
        }
        for (bit, button) in [
            (1, MOUSE_BUTTON_LEFT),
            (2, MOUSE_BUTTON_WHEEL),
            (4, MOUSE_BUTTON_RIGHT),
        ] {
            if changed & bit != 0 {
                let kind = if buttons & bit != 0 {
                    MOUSE_TYPE_DOWN
                } else {
                    MOUSE_TYPE_UP
                };
                out.push(mouse(kind | (button << 3), x, y));
            }
        }
        self.buttons = buttons;
        self.pos = Some((x, y));
        out
    }

    /// `delta` in notches, positive away from the user (as RustDesk's `y`).
    pub fn wheel(&self, delta: i32) -> MouseEvent {
        mouse(MOUSE_TYPE_WHEEL, 0, delta)
    }

    /// Releases held buttons, e.g. before disconnecting.
    pub fn release_all(&mut self) -> Vec<MouseEvent> {
        match self.pos {
            Some((x, y)) if self.buttons != 0 => self.pointer(x, y, 0),
            _ => Vec::new(),
        }
    }
}

fn mouse(mask: i32, x: i32, y: i32) -> MouseEvent {
    MouseEvent {
        mask,
        x,
        y,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chr(e: Option<KeyEvent>) -> Option<(u32, bool)> {
        e.map(|e| {
            assert_eq!(e.mode.enum_value(), Ok(KeyboardMode::Map));
            (e.chr(), e.down)
        })
    }

    #[test]
    fn platforms() {
        assert_eq!(Platform::from_peer("Windows"), Platform::Windows);
        assert_eq!(Platform::from_peer("Mac OS"), Platform::MacOs);
        assert_eq!(Platform::from_peer("Linux"), Platform::Linux);
        assert_eq!(Platform::from_peer("Android"), Platform::Android);
    }

    #[test]
    fn windows_peers_get_scancodes() {
        let mut k = Keyboard::default();
        assert_eq!(
            chr(k.key(Platform::Windows, 0x1E, true)),
            Some((0x1E, true))
        );
        assert_eq!(
            chr(k.key(Platform::Windows, 0x11D, false)),
            Some((0xE01D, false))
        );
        assert_eq!(
            chr(k.key(Platform::Windows, 0x148, true)),
            Some((0xE048, true))
        );
    }

    #[test]
    fn linux_and_mac_peers_get_their_key_codes() {
        let mut k = Keyboard::default();
        // A: X11 keycode 38, macOS kVK_ANSI_A 0.
        assert_eq!(chr(k.key(Platform::Linux, 0x1E, true)), Some((38, true)));
        assert_eq!(chr(k.key(Platform::MacOs, 0x1E, true)), Some((0, true)));
        // Right Ctrl (E0 1D): X11 105.
        assert_eq!(chr(k.key(Platform::Linux, 0x11D, true)), Some((105, true)));
        // Up arrow (E0 48): X11 111, macOS 126.
        assert_eq!(chr(k.key(Platform::Linux, 0x148, true)), Some((111, true)));
        assert_eq!(chr(k.key(Platform::MacOs, 0x148, true)), Some((126, true)));
    }

    #[test]
    fn pause_is_special() {
        let mut k = Keyboard::default();
        assert!(k.key(Platform::Windows, 0x21D, true).is_none());
        let e = k.key(Platform::Windows, 0x45, true).unwrap();
        assert_eq!(e.mode.enum_value(), Ok(KeyboardMode::Legacy));
        assert_eq!(e.control_key(), ControlKey::Pause);
        assert!(k.key(Platform::Windows, 0x21D, false).is_none());
        let e = k.key(Platform::Windows, 0x45, false).unwrap();
        assert_eq!((e.control_key(), e.down), (ControlKey::Pause, false));
        // Without the prefix 0x45 is NumLock.
        assert_eq!(
            chr(k.key(Platform::Windows, 0x45, true)),
            Some((0x45, true))
        );
        // Linux: Pause is X11 127.
        k.key(Platform::Linux, 0x21D, true);
        assert_eq!(chr(k.key(Platform::Linux, 0x45, true)), Some((127, true)));
    }

    #[test]
    fn mouse_buttons_become_down_and_up_events() {
        let mut m = Mouse::default();
        let masks = |v: Vec<MouseEvent>| v.iter().map(|e| (e.mask, e.x, e.y)).collect::<Vec<_>>();
        assert_eq!(masks(m.pointer(10, 20, 0)), vec![(0, 10, 20)]);
        assert_eq!(masks(m.pointer(10, 20, 1)), vec![(1 | (1 << 3), 10, 20)]);
        assert_eq!(masks(m.pointer(11, 20, 1)), vec![(0, 11, 20)]);
        assert_eq!(masks(m.pointer(11, 20, 5)), vec![(1 | (2 << 3), 11, 20)]);
        assert_eq!(
            masks(m.pointer(12, 21, 2)),
            vec![
                (0, 12, 21),
                (2 | (1 << 3), 12, 21),
                (1 | (4 << 3), 12, 21),
                (2 | (2 << 3), 12, 21)
            ]
        );
        assert_eq!(masks(m.release_all()), vec![(2 | (4 << 3), 12, 21)]);
        assert!(m.release_all().is_empty());
        let w = m.wheel(-2);
        assert_eq!((w.mask, w.x, w.y), (3, 0, -2));
    }
}
