//! What a key sends to the shell (xterm sequences), and how pastes are framed. Pure: the
//! terminal's key handler feeds it the key, the layout's unmodified character and the
//! terminal's mode.

use alacritty_terminal::term::TermMode;
use rune::{Key, KeyEvent, KeyPlatform};

/// Whether `ev` is the platform's terminal copy chord: ⌘C on macOS, Ctrl+Shift+C elsewhere
/// (Ctrl+C is the shell's interrupt).
pub fn is_copy(ev: &KeyEvent, platform: KeyPlatform) -> bool {
    clipboard_chord(ev, platform, 'c')
}

/// Whether `ev` is the platform's terminal paste chord: ⌘V on macOS, Ctrl+Shift+V elsewhere.
pub fn is_paste(ev: &KeyEvent, platform: KeyPlatform) -> bool {
    clipboard_chord(ev, platform, 'v')
}

fn clipboard_chord(ev: &KeyEvent, platform: KeyPlatform, c: char) -> bool {
    let m = ev.mods;
    if !ev.is_command(c) || m.alt {
        return false;
    }
    match platform {
        KeyPlatform::Mac => !m.shift && !m.ctrl,
        _ => m.shift,
    }
}

/// `text` as typed input: bracketed when the program asked for it, else with newlines as
/// Enter.
pub fn paste_input(text: &str, mode: TermMode) -> String {
    if mode.contains(TermMode::BRACKETED_PASTE) {
        // A pasted end marker must not end the paste early.
        let text = text.replace('\x1b', "");
        format!("\x1b[200~{text}\x1b[201~")
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r")
    }
}

fn meta(alt: bool, text: &str) -> String {
    if alt {
        format!("\x1b{text}")
    } else {
        text.to_owned()
    }
}

/// The C0 control byte for Ctrl+`c` (xterm's table), if it has one.
fn control_byte(c: char) -> Option<u8> {
    Some(match c.to_ascii_lowercase() {
        c @ 'a'..='z' => c as u8 - b'a' + 1,
        '@' | '2' | ' ' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '-' | '/' | '7' => 0x1f,
        '?' | '8' => 0x7f,
        _ => return None,
    })
}

/// The single character of `text`, if it is one.
fn single(text: &str) -> Option<char> {
    let mut chars = text.chars();
    chars.next().filter(|_| chars.next().is_none())
}

/// What `ev` sends to the shell: typed text as itself; editing and cursor keys, F-keys, and
/// Ctrl and Alt (as Meta: an ESC prefix) combinations as xterm sends them. Cursor keys follow
/// application cursor mode; modified ones use the `CSI 1;m` form. `base` is the character
/// the key types with no modifiers (`Tx::key_base`), so macOS Option compositions (⌥S types
/// ß) still send Meta and the key. `None` for ⌘ shortcuts on macOS, bare modifiers and keys
/// a terminal has no sequence for.
pub fn key_input(
    ev: &KeyEvent,
    base: Option<char>,
    mode: TermMode,
    platform: KeyPlatform,
) -> Option<String> {
    let m = ev.mods;
    if platform == KeyPlatform::Mac && m.cmd {
        return None;
    }
    let (shift, alt, control) = (m.shift, m.alt, m.ctrl);
    let code = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(control);
    let cursor = |end: char| {
        if code > 1 {
            format!("\x1b[1;{code}{end}")
        } else if mode.contains(TermMode::APP_CURSOR) {
            format!("\x1bO{end}")
        } else {
            format!("\x1b[{end}")
        }
    };
    let function = |end: char| {
        if code > 1 {
            format!("\x1b[1;{code}{end}")
        } else {
            format!("\x1bO{end}")
        }
    };
    let tilde = |n: u8| {
        if code > 1 {
            format!("\x1b[{n};{code}~")
        } else {
            format!("\x1b[{n}~")
        }
    };
    Some(match &ev.key {
        Key::Up => cursor('A'),
        Key::Down => cursor('B'),
        Key::Right => cursor('C'),
        Key::Left => cursor('D'),
        Key::Home => cursor('H'),
        Key::End => cursor('F'),
        Key::Insert => tilde(2),
        Key::Delete => tilde(3),
        Key::PageUp => tilde(5),
        Key::PageDown => tilde(6),
        Key::Function(1) => function('P'),
        Key::Function(2) => function('Q'),
        Key::Function(3) => function('R'),
        Key::Function(4) => function('S'),
        Key::Function(5) => tilde(15),
        Key::Function(6) => tilde(17),
        Key::Function(7) => tilde(18),
        Key::Function(8) => tilde(19),
        Key::Function(9) => tilde(20),
        Key::Function(10) => tilde(21),
        Key::Function(11) => tilde(23),
        Key::Function(12) => tilde(24),
        Key::Enter => meta(alt, "\r"),
        Key::Tab if shift => "\x1b[Z".to_owned(),
        Key::Tab => meta(alt, "\t"),
        Key::Escape => meta(alt, "\x1b"),
        Key::Backspace if control => meta(alt, "\x08"),
        Key::Backspace => meta(alt, "\x7f"),
        // Off macOS, Ctrl+letter arrives as a command chord on the US key.
        Key::Command(c) if control => meta(alt, &char::from(control_byte(*c)?).to_string()),
        Key::Text(text) => {
            let c = single(text);
            if control {
                // A control character the platform already made (macOS), else Ctrl's byte.
                let byte = match c {
                    Some(c) if c.is_ascii_control() => c as u8,
                    Some(c) => control_byte(c).or_else(|| base.and_then(control_byte))?,
                    None => return None,
                };
                return Some(meta(alt, &char::from(byte).to_string()));
            }
            if !alt {
                return Some(text.clone());
            }
            // Option composes characters on macOS (⌥s is ß); as Meta it sends ESC and the
            // key itself.
            let key = match base {
                Some(base) if !text.is_ascii() => {
                    if shift {
                        base.to_uppercase().collect()
                    } else {
                        base.to_string()
                    }
                }
                _ => text.clone(),
            };
            meta(true, &key)
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use rune::Mods;

    use super::*;

    /// `spec` ("ctrl-alt-x", "shift-up", "f5", "a") as the platform delivers it.
    fn event(spec: &str, platform: KeyPlatform) -> KeyEvent {
        let mut mods = Mods::default();
        let mut parts: Vec<&str> = spec.split('-').collect();
        let name = if spec.ends_with("--") {
            parts.truncate(parts.len() - 2);
            "-"
        } else {
            parts.pop().unwrap()
        };
        for part in parts {
            match part {
                "ctrl" => mods.ctrl = true,
                "alt" => mods.alt = true,
                "shift" => mods.shift = true,
                "cmd" => mods.logo = true,
                other => panic!("modifier {other}"),
            }
        }
        mods.cmd = if platform == KeyPlatform::Mac {
            mods.logo
        } else {
            mods.ctrl
        };
        let key = match name {
            "up" => Key::Up,
            "down" => Key::Down,
            "left" => Key::Left,
            "right" => Key::Right,
            "home" => Key::Home,
            "end" => Key::End,
            "insert" => Key::Insert,
            "delete" => Key::Delete,
            "pageup" => Key::PageUp,
            "pagedown" => Key::PageDown,
            "enter" => Key::Enter,
            "tab" => Key::Tab,
            "escape" => Key::Escape,
            "backspace" => Key::Backspace,
            "space" => Key::Text(" ".into()),
            f if f.len() > 1 && f.starts_with('f') => Key::Function(f[1..].parse().unwrap()),
            c if mods.cmd => Key::Command(c.chars().next().unwrap()),
            c if mods.shift => Key::Text(c.to_uppercase()),
            c => Key::Text(c.to_owned()),
        };
        KeyEvent { key, mods }
    }

    fn keys_in(spec: &str, mode: TermMode) -> Option<String> {
        let ev = event(spec, KeyPlatform::Linux);
        let base = match &ev.key {
            Key::Text(t) => t.to_lowercase().chars().next(),
            Key::Command(c) => Some(*c),
            _ => None,
        };
        key_input(&ev, base, mode, KeyPlatform::Linux)
    }

    fn keys(spec: &str) -> Option<String> {
        keys_in(spec, TermMode::default())
    }

    #[test]
    fn typed_text_goes_to_the_shell_as_is() {
        assert_eq!(keys("a").as_deref(), Some("a"));
        assert_eq!(keys("shift-a").as_deref(), Some("A"));
        assert_eq!(keys("space").as_deref(), Some(" "));
        assert_eq!(keys("1").as_deref(), Some("1"));
        let composed = KeyEvent {
            key: Key::Text("é".into()),
            mods: Mods::default(),
        };
        let typed = key_input(
            &composed,
            Some('e'),
            TermMode::default(),
            KeyPlatform::Linux,
        );
        assert_eq!(typed.as_deref(), Some("é"));
        // Space with a modifier is still a key.
        assert_eq!(keys("ctrl-space").as_deref(), Some("\0"));
        assert_eq!(keys("alt-space").as_deref(), Some("\x1b "));
    }

    #[test]
    fn editing_keys_send_control_characters() {
        assert_eq!(keys("enter").as_deref(), Some("\r"));
        assert_eq!(keys("backspace").as_deref(), Some("\x7f"));
        assert_eq!(keys("ctrl-backspace").as_deref(), Some("\x08"));
        assert_eq!(keys("tab").as_deref(), Some("\t"));
        assert_eq!(keys("shift-tab").as_deref(), Some("\x1b[Z"));
        assert_eq!(keys("escape").as_deref(), Some("\x1b"));
        assert_eq!(keys("delete").as_deref(), Some("\x1b[3~"));
        assert_eq!(keys("insert").as_deref(), Some("\x1b[2~"));
        assert_eq!(keys("pageup").as_deref(), Some("\x1b[5~"));
        assert_eq!(keys("pagedown").as_deref(), Some("\x1b[6~"));
    }

    #[test]
    fn cursor_keys_follow_application_cursor_mode() {
        assert_eq!(keys("up").as_deref(), Some("\x1b[A"));
        assert_eq!(keys("left").as_deref(), Some("\x1b[D"));
        assert_eq!(keys("home").as_deref(), Some("\x1b[H"));
        assert_eq!(keys("end").as_deref(), Some("\x1b[F"));
        assert_eq!(
            keys_in("down", TermMode::APP_CURSOR).as_deref(),
            Some("\x1bOB")
        );
        assert_eq!(
            keys_in("home", TermMode::APP_CURSOR).as_deref(),
            Some("\x1bOH")
        );
        // Modified cursor keys use the CSI 1;m form in either mode.
        assert_eq!(keys("shift-up").as_deref(), Some("\x1b[1;2A"));
        assert_eq!(
            keys_in("ctrl-right", TermMode::APP_CURSOR).as_deref(),
            Some("\x1b[1;5C")
        );
        assert_eq!(keys("alt-left").as_deref(), Some("\x1b[1;3D"));
    }

    #[test]
    fn function_keys() {
        assert_eq!(keys("f1").as_deref(), Some("\x1bOP"));
        assert_eq!(keys("f4").as_deref(), Some("\x1bOS"));
        assert_eq!(keys("f5").as_deref(), Some("\x1b[15~"));
        assert_eq!(keys("f12").as_deref(), Some("\x1b[24~"));
        assert_eq!(keys("shift-f1").as_deref(), Some("\x1b[1;2P"));
        assert_eq!(keys("ctrl-f5").as_deref(), Some("\x1b[15;5~"));
        assert_eq!(keys("f13"), None);
    }

    #[test]
    fn control_and_meta_combinations() {
        assert_eq!(keys("ctrl-c").as_deref(), Some("\x03"));
        assert_eq!(keys("ctrl-a").as_deref(), Some("\x01"));
        assert_eq!(keys("ctrl-z").as_deref(), Some("\x1a"));
        assert_eq!(keys("ctrl-[").as_deref(), Some("\x1b"));
        assert_eq!(keys("ctrl-\\").as_deref(), Some("\x1c"));
        assert_eq!(keys("ctrl--").as_deref(), Some("\x1f"));
        assert_eq!(keys("ctrl-alt-x").as_deref(), Some("\x1b\x18"));
        assert_eq!(keys("alt-b").as_deref(), Some("\x1bb"));
        assert_eq!(keys("alt-shift-b").as_deref(), Some("\x1bB"));
        assert_eq!(keys("alt-backspace").as_deref(), Some("\x1b\x7f"));
        assert_eq!(keys("ctrl-é"), None);
    }

    #[test]
    fn macos_option_is_meta_and_command_is_the_apps() {
        let mac = |spec: &str, base: Option<char>, text: Option<&str>| {
            let mut ev = event(spec, KeyPlatform::Mac);
            if let Some(text) = text {
                ev.key = Key::Text(text.into());
            }
            key_input(&ev, base, TermMode::default(), KeyPlatform::Mac)
        };
        // ⌥s types ß; as Meta it is ESC s (and ESC S with Shift).
        assert_eq!(mac("alt-s", Some('s'), Some("ß")).as_deref(), Some("\x1bs"));
        assert_eq!(
            mac("alt-shift-s", Some('s'), Some("Í")).as_deref(),
            Some("\x1bS")
        );
        // Control is its own key on macOS: Ctrl+C is the interrupt byte, whichever text the
        // platform reports for it.
        assert_eq!(mac("ctrl-c", Some('c'), None).as_deref(), Some("\x03"));
        assert_eq!(
            mac("ctrl-c", Some('c'), Some("\x03")).as_deref(),
            Some("\x03")
        );
        assert_eq!(mac("cmd-c", Some('c'), None), None);
        assert_eq!(mac("cmd-v", Some('v'), None), None);
    }

    #[test]
    fn bare_modifiers_are_not_input() {
        let shift = KeyEvent {
            key: Key::Modifier {
                modifier: rune::Modifier::ShiftLeft,
                pressed: true,
            },
            mods: Mods {
                shift: true,
                ..Mods::default()
            },
        };
        assert_eq!(
            key_input(&shift, None, TermMode::default(), KeyPlatform::Linux),
            None
        );
    }

    #[test]
    fn copy_and_paste_chords_follow_the_platform() {
        let linux = |spec| event(spec, KeyPlatform::Linux);
        let mac = |spec| event(spec, KeyPlatform::Mac);
        assert!(is_copy(&linux("ctrl-shift-c"), KeyPlatform::Linux));
        assert!(!is_copy(&linux("ctrl-c"), KeyPlatform::Linux));
        assert!(is_paste(&linux("ctrl-shift-v"), KeyPlatform::Linux));
        assert!(!is_paste(&linux("ctrl-v"), KeyPlatform::Linux));
        assert!(is_copy(&mac("cmd-c"), KeyPlatform::Mac));
        assert!(is_paste(&mac("cmd-v"), KeyPlatform::Mac));
        assert!(!is_copy(&mac("ctrl-c"), KeyPlatform::Mac));
    }

    #[test]
    fn pastes_are_bracketed_when_asked() {
        assert_eq!(paste_input("a\nb", TermMode::default()), "a\rb");
        assert_eq!(paste_input("a\r\nb", TermMode::default()), "a\rb");
        assert_eq!(
            paste_input("x\x1b[201~y", TermMode::BRACKETED_PASTE),
            "\x1b[200~x[201~y\x1b[201~"
        );
    }
}
