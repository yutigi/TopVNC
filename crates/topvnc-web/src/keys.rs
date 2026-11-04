//! Keyboard input: physical keys by `KeyboardEvent.code`, mapped to the
//! same US-position keysyms as the native viewer, and text from on-screen
//! keyboards, which do not report reliable key codes.

const SHIFT_L: u32 = 0xffe1;
/// Distinct keys tracked as pressed; more are ignored until some are released.
const MAX_PRESSED: usize = 32;
/// Characters typed per call; longer text, such as a paste, is cut.
pub const MAX_TEXT_CHARS: usize = 256;

/// The keysym a physical key sends, as on a US keyboard with Shift held or
/// not. Games bind physical keys, so keypad keys keep their own keysyms.
pub fn keysym_for_code(code: &str, shift: bool) -> Option<u32> {
    let shifted = |plain: u8, shifted: u8| u32::from(if shift { shifted } else { plain });
    if let Some(letter) = code.strip_prefix("Key") {
        let &[letter] = letter.as_bytes() else {
            return None;
        };
        return letter.is_ascii_uppercase().then(|| {
            u32::from(if shift {
                letter
            } else {
                letter.to_ascii_lowercase()
            })
        });
    }
    if let Some(digit) = code.strip_prefix("Digit") {
        let &[digit] = digit.as_bytes() else {
            return None;
        };
        return digit.is_ascii_digit().then(|| {
            if shift {
                u32::from(b")!@#$%^&*("[usize::from(digit - b'0')])
            } else {
                u32::from(digit)
            }
        });
    }
    if let Some(digit) = code.strip_prefix("Numpad")
        && let &[digit] = digit.as_bytes()
    {
        return digit
            .is_ascii_digit()
            .then(|| 0xffb0 + u32::from(digit - b'0'));
    }
    if let Some(number) = code.strip_prefix('F')
        && !number.starts_with('0')
        && number.bytes().all(|byte| byte.is_ascii_digit())
        && let Ok(number @ 1..=24) = number.parse::<u32>()
    {
        return Some(0xffbe + number - 1);
    }
    Some(match code {
        "NumpadDecimal" => 0xffae,
        "NumpadDivide" => 0xffaf,
        "NumpadMultiply" => 0xffaa,
        "NumpadSubtract" => 0xffad,
        "NumpadAdd" => 0xffab,
        "NumpadEnter" => 0xff8d,
        "NumpadEqual" => 0xffbd,
        "Space" => 0x20,
        "Quote" => shifted(b'\'', b'"'),
        "Backquote" => shifted(b'`', b'~'),
        "Backslash" => shifted(b'\\', b'|'),
        "Comma" => shifted(b',', b'<'),
        "Equal" => shifted(b'=', b'+'),
        "BracketLeft" => shifted(b'[', b'{'),
        "Minus" => shifted(b'-', b'_'),
        "Period" => shifted(b'.', b'>'),
        "BracketRight" => shifted(b']', b'}'),
        "Semicolon" => shifted(b';', b':'),
        "Slash" => shifted(b'/', b'?'),
        "Enter" => 0xff0d,
        "Tab" => 0xff09,
        "Backspace" => 0xff08,
        "Escape" => 0xff1b,
        "Delete" => 0xffff,
        "Insert" => 0xff63,
        "Home" => 0xff50,
        "End" => 0xff57,
        "PageUp" => 0xff55,
        "PageDown" => 0xff56,
        "ArrowLeft" => 0xff51,
        "ArrowUp" => 0xff52,
        "ArrowRight" => 0xff53,
        "ArrowDown" => 0xff54,
        "ShiftLeft" => SHIFT_L,
        "ShiftRight" => 0xffe2,
        "ControlLeft" => 0xffe3,
        "ControlRight" => 0xffe4,
        "AltLeft" => 0xffe9,
        "AltRight" => 0xffea,
        // Browsers name the Windows/Command keys Meta (older ones OS).
        "MetaLeft" | "OSLeft" => 0xffeb,
        "MetaRight" | "OSRight" => 0xffec,
        "CapsLock" => 0xffe5,
        "NumLock" => 0xff7f,
        "ScrollLock" => 0xff14,
        "Pause" => 0xff13,
        "PrintScreen" => 0xff61,
        "ContextMenu" => 0xff67,
        _ => return None,
    })
}

/// Physical keys held, each with the keysym its press sent, so the release
/// sends the same keysym even if Shift changed in between.
#[derive(Default)]
pub struct Keyboard {
    pressed: Vec<(String, u32)>,
}

impl Keyboard {
    /// The keysym event for a physical key, or `None` for unknown keys,
    /// repeats, and releases of keys this page never saw pressed.
    pub fn event(&mut self, code: &str, down: bool) -> Option<(u32, bool)> {
        let index = self.pressed.iter().position(|(held, _)| held == code);
        if !down {
            return index.map(|index| (self.pressed.remove(index).1, false));
        }
        if index.is_some() || self.pressed.len() >= MAX_PRESSED {
            return None;
        }
        let shift = self
            .pressed
            .iter()
            .any(|(held, _)| held == "ShiftLeft" || held == "ShiftRight");
        let keysym = keysym_for_code(code, shift)?;
        self.pressed.push((code.to_owned(), keysym));
        Some((keysym, true))
    }

    /// Forget every held key, after the caller released them on the host.
    pub fn clear(&mut self) {
        self.pressed.clear();
    }

    pub fn shift_held(&self) -> bool {
        self.pressed
            .iter()
            .any(|(_, keysym)| *keysym == SHIFT_L || *keysym == 0xffe2)
    }
}

/// US-layout characters that need Shift on their physical key.
fn needs_shift(character: char) -> bool {
    character.is_ascii_uppercase() || "~!@#$%^&*()_+{}|:\"<>?".contains(character)
}

/// Key events that type `text`: ASCII on its US-layout key with Shift added
/// where the character needs it (the host maps ASCII keysyms to physical
/// keys), Latin-1 and other characters as Unicode keysyms. Control
/// characters other than newline and tab are skipped.
pub fn text_key_events(text: &str, shift_held: bool) -> Vec<(u32, bool)> {
    let mut events = Vec::new();
    for character in text.chars().take(MAX_TEXT_CHARS) {
        let keysym = match character {
            '\n' | '\r' => 0xff0d,
            '\t' => 0xff09,
            character if (character as u32) < 0x20 || character == '\u{7f}' => continue,
            character if (character as u32) <= 0xff => character as u32,
            character => 0x0100_0000 | character as u32,
        };
        let add_shift = needs_shift(character) && !shift_held;
        if add_shift {
            events.push((SHIFT_L, true));
        }
        events.extend([(keysym, true), (keysym, false)]);
        if add_shift {
            events.push((SHIFT_L, false));
        }
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_keys_match_the_native_viewer() {
        for (code, shift, keysym) in [
            ("KeyA", false, 0x61),
            ("KeyZ", true, 0x5a),
            ("Digit1", true, u32::from(b'!')),
            ("Digit0", false, 0x30),
            ("Numpad7", true, 0xffb7),
            ("F1", false, 0xffbe),
            ("F12", false, 0xffc9),
            ("F24", false, 0xffd5),
            ("Enter", true, 0xff0d),
            ("Slash", true, u32::from(b'?')),
            ("MetaLeft", false, 0xffeb),
            ("ArrowDown", false, 0xff54),
        ] {
            assert_eq!(keysym_for_code(code, shift), Some(keysym), "{code} {shift}");
        }
        for unknown in [
            "",
            "Key",
            "Keya",
            "KeyAB",
            "F0",
            "F25",
            "F01",
            "Numpad",
            "Fn",
            "Unidentified",
        ] {
            assert_eq!(keysym_for_code(unknown, false), None, "{unknown}");
        }
    }

    #[test]
    fn releases_send_the_keysym_of_the_press() {
        let mut keyboard = Keyboard::default();
        assert_eq!(keyboard.event("KeyA", true), Some((0x61, true)));
        assert_eq!(keyboard.event("KeyA", true), None, "auto-repeat");
        assert_eq!(keyboard.event("ShiftLeft", true), Some((SHIFT_L, true)));
        assert!(keyboard.shift_held());
        // Released after Shift went down: still the unshifted keysym.
        assert_eq!(keyboard.event("KeyA", false), Some((0x61, false)));
        assert_eq!(keyboard.event("KeyB", true), Some((0x42, true)));
        assert_eq!(keyboard.event("KeyC", false), None, "never pressed here");
        keyboard.clear();
        assert!(!keyboard.shift_held());
        assert_eq!(keyboard.event("KeyB", false), None);
    }

    #[test]
    fn text_adds_shift_only_where_the_layout_needs_it() {
        let shift = |down| (SHIFT_L, down);
        assert_eq!(
            text_key_events("a!", false),
            [
                (0x61, true),
                (0x61, false),
                shift(true),
                (0x21, true),
                (0x21, false),
                shift(false)
            ]
        );
        // With Shift already held (a latched toolbar key), none is added.
        assert_eq!(text_key_events("A", true), [(0x41, true), (0x41, false)]);
        assert_eq!(
            text_key_events("é😀\n\u{7}", false),
            [
                (0xe9, true),
                (0xe9, false),
                (0x0101_f600, true),
                (0x0101_f600, false),
                (0xff0d, true),
                (0xff0d, false)
            ]
        );
        assert_eq!(
            text_key_events(&"x".repeat(1000), false).len(),
            MAX_TEXT_CHARS * 2
        );
    }
}
