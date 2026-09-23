//! デバイスから届く 1 バイトのイベントの復号。

/// 下位 6 ビットがコントロール ID。
const CONTROL_ID_MASK: u8 = 0x3f;
/// 立っていれば解放、立っていなければ押下。
const RELEASE_BIT: u8 = 0x80;
/// 回転で立っていれば CW または上、立っていなければ CCW または下。
const CLOCKWISE_BIT: u8 = 0x40;

/// ボタン 14 種。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Button {
    Tall,
    Side,
    Top,
    Short,
    ScrollPress,
    DpadUp,
    DpadDown,
    DpadLeft,
    DpadRight,
    C1,
    C2,
    Tour,
    KnobPress,
    DialPress,
}

impl Button {
    /// 全ボタン (設計書 2.3 節の表の順)。
    pub const ALL: [Button; 14] = [
        Button::Tall,
        Button::Side,
        Button::Top,
        Button::Short,
        Button::ScrollPress,
        Button::DpadUp,
        Button::DpadDown,
        Button::DpadLeft,
        Button::DpadRight,
        Button::C1,
        Button::C2,
        Button::Tour,
        Button::KnobPress,
        Button::DialPress,
    ];

    fn id(self) -> u8 {
        match self {
            Button::Tall => 0x00,
            Button::Side => 0x01,
            Button::Top => 0x02,
            Button::Short => 0x03,
            Button::ScrollPress => 0x0a,
            Button::DpadUp => 0x10,
            Button::DpadDown => 0x11,
            Button::DpadLeft => 0x12,
            Button::DpadRight => 0x13,
            Button::C1 => 0x22,
            Button::C2 => 0x23,
            Button::Tour => 0x2a,
            Button::KnobPress => 0x37,
            Button::DialPress => 0x38,
        }
    }

    fn from_id(id: u8) -> Option<Button> {
        Button::ALL.into_iter().find(|button| button.id() == id)
    }
}

/// 回転する 3 軸。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Axis {
    Knob,
    Scroll,
    Dial,
}

impl Axis {
    /// 全軸 (設計書 2.3 節の表の順)。
    pub const ALL: [Axis; 3] = [Axis::Knob, Axis::Scroll, Axis::Dial];

    fn id(self) -> u8 {
        match self {
            Axis::Knob => 0x04,
            Axis::Scroll => 0x09,
            Axis::Dial => 0x0f,
        }
    }

    fn from_id(id: u8) -> Option<Axis> {
        Axis::ALL.into_iter().find(|axis| axis.id() == id)
    }
}

/// 回転方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// 設計書 2.3 節の表の「CW または上」。
    Clockwise,
    /// 設計書 2.3 節の表の「CCW または下」。
    CounterClockwise,
}

/// 復号したイベント。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Press(Button),
    Release(Button),
    Rotate(Axis, Direction),
    /// どのコントロールにも当たらない値。受信した値をそのまま持つ。
    Unknown(u8),
}

/// 1 バイトのイベントを復号する。
pub fn decode(byte: u8) -> Event {
    let id = byte & CONTROL_ID_MASK;
    let released = byte & RELEASE_BIT != 0;
    let clockwise = byte & CLOCKWISE_BIT != 0;

    // ボタンは回転方向のビットを、回転は解放のビットを持たない
    match (Button::from_id(id), Axis::from_id(id)) {
        (Some(button), _) if !clockwise => {
            if released {
                Event::Release(button)
            } else {
                Event::Press(button)
            }
        }
        (_, Some(axis)) if !released => {
            let direction = if clockwise {
                Direction::Clockwise
            } else {
                Direction::CounterClockwise
            };
            Event::Rotate(axis, direction)
        }
        _ => Event::Unknown(byte),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 設計書 2.3 節のボタンの表 (ボタン、押下、解放)。
    const BUTTON_TABLE: [(Button, u8, u8); 14] = [
        (Button::Tall, 0x00, 0x80),
        (Button::Side, 0x01, 0x81),
        (Button::Top, 0x02, 0x82),
        (Button::Short, 0x03, 0x83),
        (Button::ScrollPress, 0x0a, 0x8a),
        (Button::DpadUp, 0x10, 0x90),
        (Button::DpadDown, 0x11, 0x91),
        (Button::DpadLeft, 0x12, 0x92),
        (Button::DpadRight, 0x13, 0x93),
        (Button::C1, 0x22, 0xa2),
        (Button::C2, 0x23, 0xa3),
        (Button::Tour, 0x2a, 0xaa),
        (Button::KnobPress, 0x37, 0xb7),
        (Button::DialPress, 0x38, 0xb8),
    ];

    /// 設計書 2.3 節の回転の表 (軸、CW または上、CCW または下)。
    const ROTATION_TABLE: [(Axis, u8, u8); 3] = [
        (Axis::Knob, 0x44, 0x04),
        (Axis::Scroll, 0x49, 0x09),
        (Axis::Dial, 0x4f, 0x0f),
    ];

    #[test]
    fn decodes_button_press_and_release_as_in_spec_table() {
        for (button, press, release) in BUTTON_TABLE {
            assert_eq!(
                decode(press),
                Event::Press(button),
                "{press:#04x} は {button:?} の押下として復号される必要があります。"
            );
            assert_eq!(
                decode(release),
                Event::Release(button),
                "{release:#04x} は {button:?} の解放として復号される必要があります。"
            );
        }
    }

    #[test]
    fn decodes_rotation_in_both_directions_as_in_spec_table() {
        for (axis, clockwise, counter_clockwise) in ROTATION_TABLE {
            assert_eq!(
                decode(clockwise),
                Event::Rotate(axis, Direction::Clockwise),
                "{clockwise:#04x} は {axis:?} の CW 回転として復号される必要があります。"
            );
            assert_eq!(
                decode(counter_clockwise),
                Event::Rotate(axis, Direction::CounterClockwise),
                "{counter_clockwise:#04x} は {axis:?} の CCW 回転として復号される必要があります。"
            );
        }
    }

    #[test]
    fn decodes_representative_undefined_values_as_unknown() {
        let cases = [
            (0x84, "tuxbox が定義する Knob の回転停止"),
            (0x89, "tuxbox が定義する Scroll の回転停止"),
            (0x8f, "tuxbox が定義する Dial の回転停止"),
            (0xc4, "Knob の ID に解放と回転方向のビット"),
            (0x40, "Tall の ID に回転方向のビット"),
            (0xc0, "Tall の ID に解放と回転方向のビット"),
            (0x05, "割り当てのない ID"),
            (0x7f, "割り当てのない ID に回転方向のビット"),
            (0xff, "割り当てのない ID に解放と回転方向のビット"),
        ];
        for (byte, case) in cases {
            assert_eq!(
                decode(byte),
                Event::Unknown(byte),
                "{byte:#04x} ({case}) は未知の値として復号される必要があります。"
            );
        }
    }

    #[test]
    fn decodes_every_value_outside_spec_table_as_unknown() {
        let known: Vec<u8> = BUTTON_TABLE
            .iter()
            .flat_map(|&(_, press, release)| [press, release])
            .chain(
                ROTATION_TABLE
                    .iter()
                    .flat_map(|&(_, clockwise, counter_clockwise)| [clockwise, counter_clockwise]),
            )
            .collect();

        for byte in (u8::MIN..=u8::MAX).filter(|byte| !known.contains(byte)) {
            assert_eq!(
                decode(byte),
                Event::Unknown(byte),
                "表にない {byte:#04x} は未知の値として復号される必要があります。"
            );
        }
    }
}
