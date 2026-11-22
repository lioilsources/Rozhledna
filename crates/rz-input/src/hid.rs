//! USB HID keyboard usages (page 0x07) → macOS virtual key codes (`kVK_*`, HIToolbox/Events.h).
//!
//! ANSI + ISO + JIS. ISO note: Apple ISO keyboards swap the two "extra" keys at the HID level; macOS reports the key
//! left of `1` (`§`) as `kVK_ISO_Section` and the key right of left Shift (`<`) as `kVK_ANSI_Grave`. The hub
//! (Flutter `PhysicalKeyboardKey`) maps `kVK_ISO_Section` → HID 0x64 and `kVK_ANSI_Grave` → HID 0x35; this table is
//! the exact inverse, so Mac → Mac is a round trip whatever the keyboard.

/// Strips the usage page: `(0x07 << 16) | u` and bare `u` both mean keyboard usage `u`.
pub fn keyboard_usage(hid: u32) -> Option<u16> {
    match hid >> 16 {
        0 | 0x07 => Some((hid & 0xFFFF) as u16),
        _ => None,
    }
}

pub const LEFT_CTRL: u16 = 0xE0;
pub const LEFT_SHIFT: u16 = 0xE1;
pub const LEFT_ALT: u16 = 0xE2;
pub const LEFT_GUI: u16 = 0xE3;
pub const RIGHT_CTRL: u16 = 0xE4;
pub const RIGHT_SHIFT: u16 = 0xE5;
pub const RIGHT_ALT: u16 = 0xE6;
pub const RIGHT_GUI: u16 = 0xE7;
pub const CAPS_LOCK: u16 = 0x39;

pub fn is_modifier(u: u16) -> bool {
    (LEFT_CTRL..=RIGHT_GUI).contains(&u)
}

/// HID keyboard usage → macOS `CGKeyCode`.
pub fn to_mac(u: u16) -> Option<u16> {
    Some(match u {
        0x04 => 0x00, // a
        0x05 => 0x0B, // b
        0x06 => 0x08, // c
        0x07 => 0x02, // d
        0x08 => 0x0E, // e
        0x09 => 0x03, // f
        0x0A => 0x05, // g
        0x0B => 0x04, // h
        0x0C => 0x22, // i
        0x0D => 0x26, // j
        0x0E => 0x28, // k
        0x0F => 0x25, // l
        0x10 => 0x2E, // m
        0x11 => 0x2D, // n
        0x12 => 0x1F, // o
        0x13 => 0x23, // p
        0x14 => 0x0C, // q
        0x15 => 0x0F, // r
        0x16 => 0x01, // s
        0x17 => 0x11, // t
        0x18 => 0x20, // u
        0x19 => 0x09, // v
        0x1A => 0x0D, // w
        0x1B => 0x07, // x
        0x1C => 0x10, // y
        0x1D => 0x06, // z
        0x1E => 0x12, // 1
        0x1F => 0x13, // 2
        0x20 => 0x14, // 3
        0x21 => 0x15, // 4
        0x22 => 0x17, // 5
        0x23 => 0x16, // 6
        0x24 => 0x1A, // 7
        0x25 => 0x1C, // 8
        0x26 => 0x19, // 9
        0x27 => 0x1D, // 0
        0x28 => 0x24, // Return
        0x29 => 0x35, // Escape
        0x2A => 0x33, // Backspace (kVK_Delete)
        0x2B => 0x30, // Tab
        0x2C => 0x31, // Space
        0x2D => 0x1B, // -
        0x2E => 0x18, // =
        0x2F => 0x21, // [
        0x30 => 0x1E, // ]
        0x31 => 0x2A, // backslash
        0x32 => 0x2A, // non-US # (ISO, key left of Return) = same physical key as backslash on Mac
        0x33 => 0x29, // ;
        0x34 => 0x27, // '
        0x35 => 0x32, // ` (kVK_ANSI_Grave)
        0x36 => 0x2B, // ,
        0x37 => 0x2F, // .
        0x38 => 0x2C, // /
        0x39 => 0x39, // Caps Lock
        0x3A => 0x7A, // F1
        0x3B => 0x78, // F2
        0x3C => 0x63, // F3
        0x3D => 0x76, // F4
        0x3E => 0x60, // F5
        0x3F => 0x61, // F6
        0x40 => 0x62, // F7
        0x41 => 0x64, // F8
        0x42 => 0x65, // F9
        0x43 => 0x6D, // F10
        0x44 => 0x67, // F11
        0x45 => 0x6F, // F12
        0x46 => 0x69, // Print Screen → F13
        0x47 => 0x6B, // Scroll Lock → F14
        0x48 => 0x71, // Pause → F15
        0x49 => 0x72, // Insert → Help
        0x4A => 0x73, // Home
        0x4B => 0x74, // Page Up
        0x4C => 0x75, // Delete forward
        0x4D => 0x77, // End
        0x4E => 0x79, // Page Down
        0x4F => 0x7C, // Right
        0x50 => 0x7B, // Left
        0x51 => 0x7D, // Down
        0x52 => 0x7E, // Up
        0x53 => 0x47, // Num Lock → Keypad Clear
        0x54 => 0x4B, // KP /
        0x55 => 0x43, // KP *
        0x56 => 0x4E, // KP -
        0x57 => 0x45, // KP +
        0x58 => 0x4C, // KP Enter
        0x59 => 0x53, // KP 1
        0x5A => 0x54, // KP 2
        0x5B => 0x55, // KP 3
        0x5C => 0x56, // KP 4
        0x5D => 0x57, // KP 5
        0x5E => 0x58, // KP 6
        0x5F => 0x59, // KP 7
        0x60 => 0x5B, // KP 8
        0x61 => 0x5C, // KP 9
        0x62 => 0x52, // KP 0
        0x63 => 0x41, // KP .
        0x64 => 0x0A, // non-US backslash (ISO `<`) → kVK_ISO_Section
        0x67 => 0x51, // KP =
        0x68 => 0x69, // F13
        0x69 => 0x6B, // F14
        0x6A => 0x71, // F15
        0x6B => 0x6A, // F16
        0x6C => 0x40, // F17
        0x6D => 0x4F, // F18
        0x6E => 0x50, // F19
        0x6F => 0x5A, // F20
        0x75 => 0x72, // Help
        0x7F => 0x4A, // Mute
        0x80 => 0x48, // Volume Up
        0x81 => 0x49, // Volume Down
        0x85 => 0x5F, // KP , (JIS)
        0x87 => 0x5E, // International1 (JIS _ ro)
        0x89 => 0x5D, // International3 (JIS ¥)
        0x90 => 0x68, // LANG1 (Kana)
        0x91 => 0x66, // LANG2 (Eisu)
        0xE0 => 0x3B, // Left Control
        0xE1 => 0x38, // Left Shift
        0xE2 => 0x3A, // Left Option
        0xE3 => 0x37, // Left Command
        0xE4 => 0x3E, // Right Control
        0xE5 => 0x3C, // Right Shift
        0xE6 => 0x3D, // Right Option
        0xE7 => 0x36, // Right Command
        _ => return None,
    })
}

/// macOS `CGKeyCode` → HID keyboard usage (inverse of [`to_mac`]; for aliases the primary usage wins:
/// backslash 0x31 over non-US # 0x32, F13–F15 over PrintScreen/ScrollLock/Pause, Help over Insert).
pub fn from_mac(code: u16) -> Option<u16> {
    static TABLE: std::sync::OnceLock<std::collections::HashMap<u16, u16>> = std::sync::OnceLock::new();
    let t = TABLE.get_or_init(|| {
        let mut m = std::collections::HashMap::new();
        // Higher usages first so that the lower (primary) one overwrites: 0x31 over 0x32, 0x68 over 0x46, …
        for u in (0u16..=0xFF).rev() {
            if let Some(k) = to_mac(u) {
                m.insert(k, u);
            }
        }
        // Help key: report as Insert's sibling HID Help (0x75), the dedicated usage.
        m.insert(0x72, 0x75);
        // F13–F15: the dedicated F-key usages, not PrintScreen/ScrollLock/Pause.
        m.insert(0x69, 0x68);
        m.insert(0x6B, 0x69);
        m.insert(0x71, 0x6A);
        m
    });
    t.get(&code).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_roundtrip() {
        for u in 0u16..=0xFF {
            if let Some(k) = to_mac(u) {
                let back = from_mac(k).unwrap();
                assert_eq!(to_mac(back), Some(k), "usage {u:#x} → {k:#x} → {back:#x}");
            }
        }
        assert_eq!(from_mac(0x2A), Some(0x31), "backslash, not non-US #");
        assert_eq!(from_mac(0x0A), Some(0x64), "ISO section ↔ non-US backslash (Flutter's mapping)");
        assert_eq!(from_mac(0x37), Some(0xE3), "left Command");
    }

    #[test]
    fn mapping_is_injective_except_known_alias() {
        let mut seen = std::collections::HashMap::new();
        for u in 0u16..=0xFF {
            if let Some(k) = to_mac(u)
                && let Some(prev) = seen.insert(k, u)
            {
                // Allowed aliases: backslash / non-US #, and F13–F15 reachable via PrintScreen/ScrollLock/Pause.
                let pair = (prev.min(u), prev.max(u));
                assert!(
                    matches!(pair, (0x31, 0x32) | (0x46, 0x68) | (0x47, 0x69) | (0x48, 0x6A) | (0x49, 0x75)),
                    "{prev:#x} and {u:#x} both map to {k:#x}"
                );
            }
        }
        assert!(seen.len() > 110);
    }

    #[test]
    fn letters_and_page_prefix() {
        assert_eq!(to_mac(0x04), Some(0x00)); // a
        assert_eq!(to_mac(0x1D), Some(0x06)); // z (physical position; Czech QWERTZ gives "y" there)
        assert_eq!(keyboard_usage(0x0007_0004), Some(0x04));
        assert_eq!(keyboard_usage(0x04), Some(0x04));
        assert_eq!(keyboard_usage(0x000C_00E9), None, "consumer page is not keyboard");
    }
}
