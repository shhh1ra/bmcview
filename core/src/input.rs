//! Клавиатура и мышь: USB HID-репорт и абсолютные координаты.
//!
//! Порт `PackData.keyPressedPack/keyReleasedPack` и `mousePackNew_abs`. Источник событий —
//! `KeyboardEvent.code` (физическая клавиша, не зависит от раскладки ОС), его перевод
//! в HID usage — [`hid_usage`].

/// 8-байтный boot-репорт клавиатуры: `[modifiers, 0, k1..k6]`.
pub type KeyReport = [u8; 8];

/// Бит модификатора в байте 0 репорта.
fn modifier_bit(code: &str) -> Option<u8> {
    Some(match code {
        "ControlLeft" => 0x01,
        "ShiftLeft" => 0x02,
        "AltLeft" => 0x04,
        "MetaLeft" | "OSLeft" => 0x08,
        "ControlRight" => 0x10,
        "ShiftRight" => 0x20,
        "AltRight" => 0x40,
        "MetaRight" | "OSRight" => 0x80,
        _ => return None,
    })
}

/// HID usage (Keyboard/Keypad page) для `KeyboardEvent.code`.
pub fn hid_usage(code: &str) -> Option<u8> {
    if let Some(c) = code.strip_prefix("Key") {
        if let [b @ b'A'..=b'Z'] = c.as_bytes() {
            return Some(0x04 + (b - b'A'));
        }
    }
    if let Some(d) = code.strip_prefix("Digit") {
        if let [b @ b'0'..=b'9'] = d.as_bytes() {
            return Some(if *b == b'0' { 0x27 } else { 0x1E + (b - b'1') });
        }
    }
    if let Some(n) = code.strip_prefix('F') {
        if let Ok(n @ 1..=24) = n.parse::<u8>() {
            return Some(if n <= 12 { 0x3A + n - 1 } else { 0x68 + n - 13 });
        }
    }
    if let Some(d) = code.strip_prefix("Numpad") {
        if let [b @ b'0'..=b'9'] = d.as_bytes() {
            return Some(if *b == b'0' { 0x62 } else { 0x59 + (b - b'1') });
        }
    }
    Some(match code {
        "Enter" => 0x28,
        "Escape" => 0x29,
        "Backspace" => 0x2A,
        "Tab" => 0x2B,
        "Space" => 0x2C,
        "Minus" => 0x2D,
        "Equal" => 0x2E,
        "BracketLeft" => 0x2F,
        "BracketRight" => 0x30,
        "Backslash" => 0x31,
        "Semicolon" => 0x33,
        "Quote" => 0x34,
        "Backquote" => 0x35,
        "Comma" => 0x36,
        "Period" => 0x37,
        "Slash" => 0x38,
        "CapsLock" => 0x39,
        "PrintScreen" => 0x46,
        "ScrollLock" => 0x47,
        "Pause" => 0x48,
        "Insert" => 0x49,
        "Home" => 0x4A,
        "PageUp" => 0x4B,
        "Delete" => 0x4C,
        "End" => 0x4D,
        "PageDown" => 0x4E,
        "ArrowRight" => 0x4F,
        "ArrowLeft" => 0x50,
        "ArrowDown" => 0x51,
        "ArrowUp" => 0x52,
        "NumLock" => 0x53,
        "NumpadDivide" => 0x54,
        "NumpadMultiply" => 0x55,
        "NumpadSubtract" => 0x56,
        "NumpadAdd" => 0x57,
        "NumpadEnter" => 0x58,
        "NumpadDecimal" => 0x63,
        "IntlBackslash" => 0x64,
        "ContextMenu" => 0x65,
        "NumpadEqual" => 0x67,
        "IntlRo" => 0x87,
        "IntlYen" => 0x89,
        _ => return None,
    })
}

/// Текущее состояние клавиатуры. Как в оригинале, нажатая клавиша занимает первый
/// свободный слот `k1..k6`, отпущенная освобождает свой; седьмая одновременная
/// клавиша не передаётся.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Keyboard {
    report: KeyReport,
}

impl Keyboard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Обрабатывает событие клавиши. Возвращает новый репорт, если он изменился
    /// (автоповтор нажатой клавиши и неизвестные коды ничего не отправляют).
    pub fn key(&mut self, code: &str, down: bool) -> Option<KeyReport> {
        let before = self.report;
        if let Some(bit) = modifier_bit(code) {
            if down {
                self.report[0] |= bit;
            } else {
                self.report[0] &= !bit;
            }
        } else {
            let usage = hid_usage(code)?;
            let slots = &mut self.report[2..];
            if down {
                if !slots.contains(&usage) {
                    if let Some(free) = slots.iter_mut().find(|s| **s == 0) {
                        *free = usage;
                    }
                }
            } else if let Some(held) = slots.iter_mut().find(|s| **s == usage) {
                *held = 0;
            }
        }
        (self.report != before).then_some(self.report)
    }

    /// Отпустить всё (`PackData.clearKey`) — при потере фокуса окна.
    pub fn release_all(&mut self) -> KeyReport {
        self.report = [0; 8];
        self.report
    }

    pub fn report(&self) -> KeyReport {
        self.report
    }
}

/// Готовые комбинации из тулбара оригинала (`combinKey*`): нажатие и отпускание.
pub mod combo {
    use super::KeyReport;

    pub const CTRL_ALT_DEL: KeyReport = [0x05, 0, 0x4C, 0, 0, 0, 0, 0];
}

/// Кнопки мыши (байт `btn`).
pub mod button {
    pub const LEFT: u8 = 0x01;
    pub const RIGHT: u8 = 0x02;
    pub const MIDDLE: u8 = 0x04;
}

/// Абсолютные координаты BMC: 0..3000 по каждой оси (`x * 3000 / imageWidth`).
pub fn absolute(x: u32, y: u32, width: u32, height: u32) -> (u16, u16) {
    let scale = |v: u32, size: u32| (v.min(size.saturating_sub(1)) * 3000 / size.max(1)) as u16;
    (scale(x, width), scale(y, height))
}

/// Открытая часть пакета мыши: `btn xH xL yH yL wheel`.
pub fn mouse_report(buttons: u8, x: u16, y: u16, wheel: i8) -> [u8; 6] {
    let [xh, xl] = x.to_be_bytes();
    let [yh, yl] = y.to_be_bytes();
    [buttons, xh, xl, yh, yl, wheel as u8]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_table() {
        assert_eq!(hid_usage("KeyA"), Some(0x04));
        assert_eq!(hid_usage("KeyZ"), Some(0x1D));
        assert_eq!(hid_usage("Digit1"), Some(0x1E));
        assert_eq!(hid_usage("Digit0"), Some(0x27));
        assert_eq!(hid_usage("F1"), Some(0x3A));
        assert_eq!(hid_usage("F12"), Some(0x45));
        assert_eq!(hid_usage("F13"), Some(0x68));
        assert_eq!(hid_usage("Numpad1"), Some(0x59));
        assert_eq!(hid_usage("Numpad0"), Some(0x62));
        assert_eq!(hid_usage("Delete"), Some(0x4C));
        assert_eq!(hid_usage("KeyAA"), None);
        assert_eq!(hid_usage("F25"), None);
        assert_eq!(hid_usage("Fn"), None);
    }

    #[test]
    fn keys_take_first_free_slot_and_release_their_own() {
        let mut kb = Keyboard::new();
        assert_eq!(kb.key("ShiftLeft", true), Some([0x02, 0, 0, 0, 0, 0, 0, 0]));
        assert_eq!(kb.key("KeyA", true), Some([0x02, 0, 0x04, 0, 0, 0, 0, 0]));
        assert_eq!(
            kb.key("KeyB", true),
            Some([0x02, 0, 0x04, 0x05, 0, 0, 0, 0])
        );
        assert_eq!(kb.key("KeyA", true), None, "автоповтор");
        assert_eq!(kb.key("KeyA", false), Some([0x02, 0, 0, 0x05, 0, 0, 0, 0]));
        assert_eq!(
            kb.key("KeyC", true),
            Some([0x02, 0, 0x06, 0x05, 0, 0, 0, 0])
        );
        assert_eq!(kb.key("Unidentified", true), None);
        assert_eq!(kb.release_all(), [0; 8]);
    }

    #[test]
    fn seventh_key_is_dropped() {
        let mut kb = Keyboard::new();
        for code in ["KeyA", "KeyB", "KeyC", "KeyD", "KeyE", "KeyF"] {
            kb.key(code, true);
        }
        assert_eq!(kb.key("KeyG", true), None);
    }

    #[test]
    fn absolute_scaling_matches_capture() {
        // Из дампа: экран 1024×768.
        assert_eq!(absolute(0, 0, 1024, 768), (0, 0));
        assert_eq!(absolute(512, 384, 1024, 768), (1500, 1500));
        assert_eq!(absolute(5000, 5000, 1024, 768), (2997, 2996));
        assert_eq!(
            mouse_report(1, 0x059b, 0x0591, -1),
            [1, 0x05, 0x9b, 0x05, 0x91, 0xFF]
        );
    }
}
