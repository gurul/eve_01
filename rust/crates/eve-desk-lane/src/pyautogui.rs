//! PyAutoGUI 0.9.54's macOS backend, as far as `desktop_helpers.Helpers` uses it (`click`, `hotkey`, `press`,
//! `size`), configured as `desk_lane._real_helpers` configures the module: `FAILSAFE = True`, `PAUSE = 0.05`.
//!
//! The logic (the fail-safe corners, the pause after every public call, the 0.01 s Darwin catch-up sleep after
//! every posted move or key, shift held around shifted characters, unknown keys silently ignored) is plain code
//! over [`Quartz`], so it is the same everywhere and testable; [`RealQuartz`] posts the CGEvents on the Mac.

use eve_desktop_lanes::desktop_helpers::Gui;
use eve_desktop_lanes::LaneError;

/// `pyautogui.PAUSE` as desk_lane sets it: seconds slept after every public call.
pub const PAUSE: f64 = 0.05;
/// `pyautogui.FAILSAFE` as desk_lane sets it.
pub const FAILSAFE: bool = true;
/// `pyautogui.DARWIN_CATCH_UP_TIME`: seconds after every posted move, drag or key event.
pub const DARWIN_CATCH_UP_TIME: f64 = 0.01;

/// `pyautogui.FailSafeException`'s text.
pub const FAILSAFE_MESSAGE: &str = "PyAutoGUI fail-safe triggered from mouse moving to a corner of the screen. To \
disable this fail-safe, set pyautogui.FAILSAFE to False. DISABLING FAIL-SAFE IS NOT RECOMMENDED.";

/// The mouse events `_sendMouseEvent` posts (only the left button: Helpers clicks with the primary button).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEvent {
    /// `kCGEventMouseMoved`, button 0
    Moved,
    /// `kCGEventLeftMouseDown`
    LeftDown,
    /// `kCGEventLeftMouseUp`
    LeftUp,
}

/// What PyAutoGUI asks of Quartz and the clock.
pub trait Quartz: Send {
    /// `_position()`: the cursor in global top-left points, truncated toward zero.
    fn position(&self) -> (i64, i64);
    /// `_size()`: `CGDisplayPixelsWide/High(CGMainDisplayID())`.
    fn size(&self) -> (i64, i64);
    /// `CGEventPost(kCGHIDEventTap, CGEventCreateMouseEvent(None, ev, (x, y), button))`.
    fn post_mouse(&mut self, event: MouseEvent, x: i64, y: i64);
    /// `CGEventPost(kCGHIDEventTap, CGEventCreateKeyboardEvent(None, code, down))`.
    fn post_key(&mut self, code: u16, down: bool);
    /// `time.sleep(secs)`.
    fn sleep(&mut self, secs: f64);
}

/// `_pyautogui_osx.keyboardMapping`: the virtual key code for a key name, None for a name PyAutoGUI knows but
/// cannot press on macOS and for any other string (both are silent no-ops in `_keyDown` / `_keyUp`).
pub fn keyboard_mapping(key: &str) -> Option<u16> {
    let code = match key {
        "a" | "A" => 0x00,
        "s" | "S" => 0x01,
        "d" | "D" => 0x02,
        "f" | "F" => 0x03,
        "h" | "H" => 0x04,
        "g" | "G" => 0x05,
        "z" | "Z" => 0x06,
        "x" | "X" => 0x07,
        "c" | "C" => 0x08,
        "v" | "V" => 0x09,
        "b" | "B" => 0x0b,
        "q" | "Q" => 0x0c,
        "w" | "W" => 0x0d,
        "e" | "E" => 0x0e,
        "r" | "R" => 0x0f,
        "y" | "Y" => 0x10,
        "t" | "T" => 0x11,
        "1" | "!" => 0x12,
        "2" | "@" => 0x13,
        "3" | "#" => 0x14,
        "4" | "$" => 0x15,
        "6" | "^" => 0x16,
        "5" | "%" => 0x17,
        "=" | "+" => 0x18,
        "9" | "(" => 0x19,
        "7" | "&" => 0x1a,
        "-" | "_" => 0x1b,
        "8" | "*" => 0x1c,
        "0" | ")" => 0x1d,
        "]" | "}" => 0x1e,
        "o" | "O" => 0x1f,
        "u" | "U" => 0x20,
        "[" | "{" => 0x21,
        "i" | "I" => 0x22,
        "p" | "P" => 0x23,
        "l" | "L" => 0x25,
        "j" | "J" => 0x26,
        "'" | "\"" => 0x27,
        "k" | "K" => 0x28,
        ";" | ":" => 0x29,
        "\\" | "|" => 0x2a,
        "," | "<" => 0x2b,
        "/" | "?" => 0x2c,
        "n" | "N" => 0x2d,
        "m" | "M" => 0x2e,
        "." | ">" => 0x2f,
        "`" | "~" => 0x32,
        " " | "space" => 0x31,
        "\r" | "\n" | "enter" | "return" => 0x24,
        "\t" | "tab" => 0x30,
        // kVK_Delete, which is "Backspace" on OS X.
        "backspace" | "\u{8}" => 0x33,
        "esc" | "escape" => 0x35,
        "command" => 0x37,
        "shift" | "shiftleft" => 0x38,
        "capslock" => 0x39,
        "option" | "optionleft" | "alt" | "altleft" => 0x3a,
        "ctrl" | "ctrlleft" => 0x3b,
        "shiftright" => 0x3c,
        "optionright" => 0x3d,
        "ctrlright" => 0x3e,
        "fn" => 0x3f,
        "f17" => 0x40,
        "volumeup" => 0x48,
        "volumedown" => 0x49,
        "volumemute" => 0x4a,
        "f18" => 0x4f,
        "f19" => 0x50,
        "f20" => 0x5a,
        "f5" => 0x60,
        "f6" => 0x61,
        "f7" => 0x62,
        "f3" => 0x63,
        "f8" => 0x64,
        "f9" => 0x65,
        "f11" => 0x67,
        "f13" => 0x69,
        "f16" => 0x6a,
        "f14" => 0x6b,
        "f10" => 0x6d,
        "f12" => 0x6f,
        "f15" => 0x71,
        "help" => 0x72,
        "home" => 0x73,
        "pageup" | "pgup" => 0x74,
        "del" | "delete" => 0x75,
        "f4" => 0x76,
        "end" => 0x77,
        "f2" => 0x78,
        "pagedown" | "pgdn" => 0x79,
        "f1" => 0x7a,
        "left" => 0x7b,
        "right" => 0x7c,
        "down" => 0x7d,
        "up" => 0x7e,
        "yen" => 0x5d,
        "eisu" => 0x66,
        "kana" => 0x68,
        _ => return None,
    };
    Some(code)
}

/// `pyautogui.isShiftCharacter`: `character.isupper() or character in set('~!@#$%^&*()_+{}|:"<>?')`.
pub fn is_shift_character(character: &str) -> bool {
    // str.isupper(): at least one cased character and no lowercase one.
    let is_upper = character.chars().any(char::is_uppercase) && !character.chars().any(char::is_lowercase);
    let mut chars = character.chars();
    let symbol = match (chars.next(), chars.next()) {
        (Some(c), None) => "~!@#$%^&*()_+{}|:\"<>?".contains(c),
        _ => false,
    };
    is_upper || symbol
}

/// `press` / `hotkey` lower-case a key name longer than one character and leave a single character as it is.
fn normalize_key(key: &str) -> String {
    if key.chars().count() > 1 {
        key.to_lowercase()
    } else {
        key.to_string()
    }
}

/// The `pyautogui` module over one [`Quartz`].
pub struct PyAutoGui<Q: Quartz> {
    pub quartz: Q,
    pub failsafe: bool,
    pub pause: f64,
    /// `FAILSAFE_POINTS`: the four screen corners, from the screen size when the module was set up.
    pub failsafe_points: Vec<(i64, i64)>,
}

impl<Q: Quartz> PyAutoGui<Q> {
    /// `import pyautogui; pyautogui.FAILSAFE = True; pyautogui.PAUSE = 0.05`.
    pub fn new(quartz: Q) -> Self {
        let (right, bottom) = quartz.size();
        PyAutoGui {
            quartz,
            failsafe: FAILSAFE,
            pause: PAUSE,
            failsafe_points: vec![(0, 0), (0, bottom - 1), (right - 1, 0), (right - 1, bottom - 1)],
        }
    }

    /// `failSafeCheck()`.
    pub fn fail_safe_check(&self) -> anyhow::Result<()> {
        if self.failsafe && self.failsafe_points.contains(&self.quartz.position()) {
            return Err(LaneError::err("FailSafeException", FAILSAFE_MESSAGE));
        }
        Ok(())
    }

    /// `_handlePause(True)`.
    fn handle_pause(&mut self) {
        let pause = self.pause;
        self.quartz.sleep(pause);
    }

    /// `_pyautogui_osx._moveTo`.
    fn move_to(&mut self, x: i64, y: i64) {
        self.quartz.post_mouse(MouseEvent::Moved, x, y);
        self.quartz.sleep(DARWIN_CATCH_UP_TIME);
    }

    /// `_pyautogui_osx._normalKeyEvent` behind `_keyDown` / `_keyUp`'s mapping check.
    fn key_event(&mut self, key: &str, down: bool) {
        if keyboard_mapping(key).is_none() {
            return;
        }
        let code = if is_shift_character(key) {
            let shift = keyboard_mapping("shift").unwrap_or(0x38);
            self.quartz.post_key(shift, down);
            self.quartz.sleep(DARWIN_CATCH_UP_TIME);
            keyboard_mapping(&key.to_lowercase())
        } else {
            keyboard_mapping(key)
        };
        // Every shifted character's lower case is in the map, so Python's KeyError branch has no trigger.
        if let Some(code) = code {
            self.quartz.post_key(code, down);
            self.quartz.sleep(DARWIN_CATCH_UP_TIME);
        }
    }

    /// `pyautogui.click(x, y, clicks)` with the primary (left) button, no interval, no duration.
    pub fn click(&mut self, x: i64, y: i64, clicks: u32) -> anyhow::Result<()> {
        self.fail_safe_check()?;
        // _mouseMoveDrag("move", x, y, 0, 0, 0): one step, checked before and after unless it is a corner.
        let _start = self.quartz.position();
        let corner = self.failsafe_points.contains(&(x, y));
        if !corner {
            self.fail_safe_check()?;
        }
        self.move_to(x, y);
        if !corner {
            self.fail_safe_check()?;
        }
        for _ in 0..clicks {
            self.fail_safe_check()?;
            // _multiClick(x, y, LEFT, 1, 0.0) → _click: down then up, no click state
            self.quartz.post_mouse(MouseEvent::LeftDown, x, y);
            self.quartz.post_mouse(MouseEvent::LeftUp, x, y);
        }
        self.handle_pause();
        Ok(())
    }

    /// `pyautogui.press(key)`: one press, one release.
    pub fn press(&mut self, key: &str) -> anyhow::Result<()> {
        self.fail_safe_check()?;
        let key = normalize_key(key);
        self.fail_safe_check()?;
        self.key_event(&key, true);
        self.key_event(&key, false);
        self.handle_pause();
        Ok(())
    }

    /// `pyautogui.hotkey(*keys)`: downs in order, ups in reverse.
    pub fn hotkey(&mut self, keys: &[&str]) -> anyhow::Result<()> {
        self.fail_safe_check()?;
        for key in keys {
            self.key_event(&normalize_key(key), true);
        }
        for key in keys.iter().rev() {
            self.key_event(&normalize_key(key), false);
        }
        self.handle_pause();
        Ok(())
    }
}

impl<Q: Quartz> Gui for PyAutoGui<Q> {
    fn size(&self) -> (i64, i64) {
        self.quartz.size()
    }

    fn click(&mut self, x: i64, y: i64, clicks: u32) -> anyhow::Result<()> {
        PyAutoGui::click(self, x, y, clicks)
    }

    fn hotkey(&mut self, keys: &[&str]) -> anyhow::Result<()> {
        PyAutoGui::hotkey(self, keys)
    }

    fn press(&mut self, key: &str) -> anyhow::Result<()> {
        PyAutoGui::press(self, key)
    }
}

/// Quartz itself: CGEvents posted at the HID tap with no source, the main display's size, the cursor from an
/// empty CGEvent's location (the same point as `NSEvent.mouseLocation()` flipped against the main display).
#[cfg(target_os = "macos")]
#[derive(Debug, Default, Clone, Copy)]
pub struct RealQuartz;

#[cfg(target_os = "macos")]
impl Quartz for RealQuartz {
    fn position(&self) -> (i64, i64) {
        use objc2_core_graphics::CGEvent;

        let event = CGEvent::new(None);
        let loc = CGEvent::location(event.as_deref());
        (loc.x as i64, loc.y as i64)
    }

    fn size(&self) -> (i64, i64) {
        use objc2_core_graphics::{CGDisplayPixelsHigh, CGDisplayPixelsWide, CGMainDisplayID};

        let main = CGMainDisplayID();
        (CGDisplayPixelsWide(main) as i64, CGDisplayPixelsHigh(main) as i64)
    }

    fn post_mouse(&mut self, event: MouseEvent, x: i64, y: i64) {
        use objc2_core_foundation::CGPoint;
        use objc2_core_graphics::{CGEvent, CGEventTapLocation, CGEventType, CGMouseButton};

        let kind = match event {
            MouseEvent::Moved => CGEventType::MouseMoved,
            MouseEvent::LeftDown => CGEventType::LeftMouseDown,
            MouseEvent::LeftUp => CGEventType::LeftMouseUp,
        };
        let point = CGPoint { x: x as f64, y: y as f64 };
        let ev = CGEvent::new_mouse_event(None, kind, point, CGMouseButton::Left);
        CGEvent::post(CGEventTapLocation::HIDEventTap, ev.as_deref());
    }

    fn post_key(&mut self, code: u16, down: bool) {
        use objc2_core_graphics::{CGEvent, CGEventTapLocation};

        let ev = CGEvent::new_keyboard_event(None, code, down);
        CGEvent::post(CGEventTapLocation::HIDEventTap, ev.as_deref());
    }

    fn sleep(&mut self, secs: f64) {
        if secs > 0.0 && secs.is_finite() {
            std::thread::sleep(std::time::Duration::from_secs_f64(secs));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeQuartz {
        pos: (i64, i64),
        posts: Vec<String>,
        slept: Vec<f64>,
    }

    impl Quartz for FakeQuartz {
        fn position(&self) -> (i64, i64) {
            self.pos
        }
        fn size(&self) -> (i64, i64) {
            (1920, 1080)
        }
        fn post_mouse(&mut self, event: MouseEvent, x: i64, y: i64) {
            self.posts.push(format!("{event:?}@{x},{y}"));
        }
        fn post_key(&mut self, code: u16, down: bool) {
            self.posts.push(format!("key {code:#04x} {}", if down { "down" } else { "up" }));
        }
        fn sleep(&mut self, secs: f64) {
            self.slept.push(secs);
        }
    }

    fn gui(pos: (i64, i64)) -> PyAutoGui<FakeQuartz> {
        PyAutoGui::new(FakeQuartz { pos, ..Default::default() })
    }

    #[test]
    fn the_failsafe_corners_come_from_the_screen_size() {
        let g = gui((5, 5));
        assert_eq!(g.failsafe_points, vec![(0, 0), (0, 1079), (1919, 0), (1919, 1079)]);
    }

    #[test]
    fn a_click_moves_then_presses_and_releases_then_pauses() {
        let mut g = gui((5, 5));
        g.click(100, 200, 1).unwrap();
        assert_eq!(g.quartz.posts, vec!["Moved@100,200", "LeftDown@100,200", "LeftUp@100,200"]);
        assert_eq!(g.quartz.slept, vec![DARWIN_CATCH_UP_TIME, PAUSE]);
        let mut g = gui((5, 5));
        g.click(10, 10, 2).unwrap();
        assert_eq!(g.quartz.posts.len(), 5);
    }

    #[test]
    fn a_cursor_in_a_corner_stops_every_call_before_any_event() {
        for pos in [(0, 0), (1919, 1079)] {
            let mut g = gui(pos);
            let err = g.click(100, 200, 1).unwrap_err();
            assert_eq!(eve_desktop_lanes::error_kind(&err), "FailSafeException");
            assert!(err.to_string().starts_with("PyAutoGUI fail-safe triggered"));
            assert!(g.press("enter").is_err() && g.hotkey(&["command", "v"]).is_err());
            assert!(g.quartz.posts.is_empty());
        }
        let mut g = gui((0, 0));
        g.failsafe = false;
        assert!(g.click(1, 1, 1).is_ok());
    }

    #[test]
    fn press_lowercases_names_and_holds_shift_for_shifted_characters() {
        let mut g = gui((5, 5));
        g.press("ENTER").unwrap();
        assert_eq!(g.quartz.posts, vec!["key 0x24 down", "key 0x24 up"]);
        let mut g = gui((5, 5));
        g.press("A").unwrap();
        assert_eq!(g.quartz.posts, vec!["key 0x38 down", "key 0x00 down", "key 0x38 up", "key 0x00 up"]);
        let mut g = gui((5, 5));
        g.press("!").unwrap();
        assert_eq!(g.quartz.posts, vec!["key 0x38 down", "key 0x12 down", "key 0x38 up", "key 0x12 up"]);
    }

    #[test]
    fn an_unknown_key_is_a_silent_no_op() {
        let mut g = gui((5, 5));
        g.press("printscreen").unwrap();
        g.press("é").unwrap();
        assert!(g.quartz.posts.is_empty());
        assert_eq!(g.quartz.slept, vec![PAUSE, PAUSE]);
    }

    #[test]
    fn hotkey_releases_in_reverse_order() {
        let mut g = gui((5, 5));
        g.hotkey(&["Command", "v"]).unwrap();
        assert_eq!(g.quartz.posts, vec!["key 0x37 down", "key 0x09 down", "key 0x09 up", "key 0x37 up"]);
    }

    #[test]
    fn shift_characters_follow_python() {
        assert!(is_shift_character("A") && is_shift_character("?") && is_shift_character("~"));
        assert!(!is_shift_character("a") && !is_shift_character("1") && !is_shift_character("enter"));
        assert_eq!(keyboard_mapping("Z"), keyboard_mapping("z"));
        assert_eq!(keyboard_mapping("nope"), None);
    }
}
