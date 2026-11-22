//! US keyboard layout used to turn DOM key names into CDP key events
//! (same rules as Puppeteer's `keyboard.press`).

pub struct KeyDef {
    pub key: String,
    pub code: &'static str,
    pub key_code: u32,
    pub text: String,
    pub location: u32,
}

pub const MOD_ALT: u32 = 1;
pub const MOD_CTRL: u32 = 2;
pub const MOD_META: u32 = 4;
pub const MOD_SHIFT: u32 = 8;

/// (unshifted, shifted, code, keyCode) for printable keys.
const PRINTABLE: &[(char, char, &str, u32)] = &[
    ('0', ')', "Digit0", 48),
    ('1', '!', "Digit1", 49),
    ('2', '@', "Digit2", 50),
    ('3', '#', "Digit3", 51),
    ('4', '$', "Digit4", 52),
    ('5', '%', "Digit5", 53),
    ('6', '^', "Digit6", 54),
    ('7', '&', "Digit7", 55),
    ('8', '*', "Digit8", 56),
    ('9', '(', "Digit9", 57),
    (';', ':', "Semicolon", 186),
    ('=', '+', "Equal", 187),
    (',', '<', "Comma", 188),
    ('-', '_', "Minus", 189),
    ('.', '>', "Period", 190),
    ('/', '?', "Slash", 191),
    ('`', '~', "Backquote", 192),
    ('[', '{', "BracketLeft", 219),
    ('\\', '|', "Backslash", 220),
    (']', '}', "BracketRight", 221),
    ('\'', '"', "Quote", 222),
];

const LETTER_CODES: [&str; 26] = [
    "KeyA", "KeyB", "KeyC", "KeyD", "KeyE", "KeyF", "KeyG", "KeyH", "KeyI", "KeyJ", "KeyK", "KeyL", "KeyM",
    "KeyN", "KeyO", "KeyP", "KeyQ", "KeyR", "KeyS", "KeyT", "KeyU", "KeyV", "KeyW", "KeyX", "KeyY", "KeyZ",
];

/// (key, code, keyCode, text, location) for named keys.
const NAMED: &[(&str, &str, u32, &str, u32)] = &[
    ("Backspace", "Backspace", 8, "", 0),
    ("Tab", "Tab", 9, "", 0),
    ("Enter", "Enter", 13, "\r", 0),
    ("Shift", "ShiftLeft", 16, "", 1),
    ("Control", "ControlLeft", 17, "", 1),
    ("Alt", "AltLeft", 18, "", 1),
    ("Pause", "Pause", 19, "", 0),
    ("CapsLock", "CapsLock", 20, "", 0),
    ("Escape", "Escape", 27, "", 0),
    ("PageUp", "PageUp", 33, "", 0),
    ("PageDown", "PageDown", 34, "", 0),
    ("End", "End", 35, "", 0),
    ("Home", "Home", 36, "", 0),
    ("ArrowLeft", "ArrowLeft", 37, "", 0),
    ("ArrowUp", "ArrowUp", 38, "", 0),
    ("ArrowRight", "ArrowRight", 39, "", 0),
    ("ArrowDown", "ArrowDown", 40, "", 0),
    ("Insert", "Insert", 45, "", 0),
    ("Delete", "Delete", 46, "", 0),
    ("Meta", "MetaLeft", 91, "", 1),
    ("ContextMenu", "ContextMenu", 93, "", 0),
    ("F1", "F1", 112, "", 0),
    ("F2", "F2", 113, "", 0),
    ("F3", "F3", 114, "", 0),
    ("F4", "F4", 115, "", 0),
    ("F5", "F5", 116, "", 0),
    ("F6", "F6", 117, "", 0),
    ("F7", "F7", 118, "", 0),
    ("F8", "F8", 119, "", 0),
    ("F9", "F9", 120, "", 0),
    ("F10", "F10", 121, "", 0),
    ("F11", "F11", 122, "", 0),
    ("F12", "F12", 123, "", 0),
];

/// Describe a key press. `modifiers` is the CDP modifier mask currently held.
/// Returns `None` for keys that are not on the layout.
pub fn describe(key: &str, modifiers: u32) -> Option<KeyDef> {
    let shift = modifiers & MOD_SHIFT != 0;
    let mut def = lookup(key, shift)?;
    // Like Puppeteer: shortcuts (Ctrl/Alt/Meta held) produce no text.
    if modifiers & !MOD_SHIFT != 0 {
        def.text.clear();
    }
    Some(def)
}

fn lookup(key: &str, shift: bool) -> Option<KeyDef> {
    if let Some(&(k, code, key_code, text, location)) = NAMED.iter().find(|(k, ..)| *k == key) {
        return Some(KeyDef { key: k.into(), code, key_code, text: text.into(), location });
    }
    let mut chars = key.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    let printable = |key: char, code: &'static str, key_code: u32| KeyDef {
        key: key.to_string(),
        code,
        key_code,
        text: key.to_string(),
        location: 0,
    };
    match c {
        '\r' | '\n' => lookup("Enter", shift),
        '\t' => lookup("Tab", shift),
        ' ' => Some(printable(' ', "Space", 32)),
        'a'..='z' | 'A'..='Z' => {
            let upper = c.to_ascii_uppercase();
            let code = LETTER_CODES[(upper as u8 - b'A') as usize];
            let out = if shift { upper } else { c };
            Some(printable(out, code, upper as u32))
        }
        _ => PRINTABLE.iter().find(|(lo, hi, ..)| *lo == c || *hi == c).map(|&(lo, hi, code, key_code)| {
            let out = if shift || c == hi { hi } else { lo };
            printable(out, code, key_code)
        }),
    }
}

/// Modifier key name and its mask bit, in the order they are pressed.
pub const MODIFIERS: [(&str, &str, u32); 4] = [
    ("alt", "Alt", MOD_ALT),
    ("ctrl", "Control", MOD_CTRL),
    ("meta", "Meta", MOD_META),
    ("shift", "Shift", MOD_SHIFT),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_and_shortcuts() {
        let a = describe("a", 0).unwrap();
        assert_eq!((a.key.as_str(), a.code, a.key_code, a.text.as_str()), ("a", "KeyA", 65, "a"));
        let ctrl_a = describe("a", MOD_CTRL).unwrap();
        assert_eq!(ctrl_a.text, "");
        let shift_a = describe("a", MOD_SHIFT).unwrap();
        assert_eq!(shift_a.text, "A");
    }

    #[test]
    fn named_and_punctuation() {
        assert_eq!(describe("Enter", 0).unwrap().text, "\r");
        assert_eq!(describe("ArrowLeft", 0).unwrap().key_code, 37);
        let q = describe("?", 0).unwrap();
        assert_eq!((q.key.as_str(), q.code), ("?", "Slash"));
        assert!(describe("é", 0).is_none());
        assert!(describe("NotAKey", 0).is_none());
    }
}
