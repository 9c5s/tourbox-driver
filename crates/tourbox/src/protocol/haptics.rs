//! ハプティクス設定の 94 バイトメッセージの組み立て。

use super::event::{Axis, Button};

/// 触覚の強度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Strength {
    Off,
    Weak,
    Strong,
}

impl Strength {
    fn bits(self) -> u8 {
        match self {
            Strength::Off => 0x00,
            Strength::Weak => 0x04,
            Strength::Strong => 0x08,
        }
    }
}

/// 回転速度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Speed {
    Fast,
    Medium,
    Slow,
}

impl Speed {
    fn bits(self) -> u8 {
        match self {
            Speed::Fast => 0x00,
            Speed::Medium => 0x01,
            Speed::Slow => 0x02,
        }
    }
}

/// 回転と組み合わせる修飾。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Modifier {
    /// 修飾なし (軸の単体操作)。
    None,
    Button(Button),
}

impl Modifier {
    /// 全修飾 (なしの後に Button::ALL の順)。
    pub const ALL: [Modifier; 15] = [
        Modifier::None,
        Modifier::Button(Button::Tall),
        Modifier::Button(Button::Side),
        Modifier::Button(Button::Top),
        Modifier::Button(Button::Short),
        Modifier::Button(Button::ScrollPress),
        Modifier::Button(Button::DpadUp),
        Modifier::Button(Button::DpadDown),
        Modifier::Button(Button::DpadLeft),
        Modifier::Button(Button::DpadRight),
        Modifier::Button(Button::C1),
        Modifier::Button(Button::C2),
        Modifier::Button(Button::Tour),
        Modifier::Button(Button::KnobPress),
        Modifier::Button(Button::DialPress),
    ];
}

/// 全値 0 のメッセージ。ヘッダ `b5 00 5d`、(ID、値) の 45 組、終端 `fe` からなる。
const TEMPLATE: [u8; 94] = [
    0xb5, 0x00, 0x5d, 0x04, 0x00, 0x05, 0x00, 0x06, 0x00, 0x07, 0x00, 0x08, 0x00, 0x09, 0x00, 0x0b,
    0x00, 0x0c, 0x00, 0x0d, 0x00, 0x0e, 0x00, 0x0f, 0x00, 0x26, 0x00, 0x27, 0x00, 0x28, 0x00, 0x29,
    0x00, 0x3b, 0x00, 0x3c, 0x00, 0x3d, 0x00, 0x3e, 0x00, 0x3f, 0x00, 0x40, 0x00, 0x41, 0x00, 0x42,
    0x00, 0x43, 0x00, 0x44, 0x00, 0x45, 0x00, 0x46, 0x00, 0x47, 0x00, 0x48, 0x00, 0x49, 0x00, 0x4a,
    0x00, 0x4b, 0x00, 0x4c, 0x00, 0x4d, 0x00, 0x4e, 0x00, 0x4f, 0x00, 0x50, 0x00, 0x51, 0x00, 0x52,
    0x00, 0x53, 0x00, 0x54, 0x00, 0xa8, 0x00, 0xa9, 0x00, 0xaa, 0x00, 0xab, 0x00, 0xfe,
];

/// 組み合わせの値バイトのメッセージ内オフセット (設計書 2.4 節の表)。
fn value_offset(axis: Axis, modifier: Modifier) -> usize {
    let [knob, scroll, dial] = match modifier {
        Modifier::None => [4, 14, 24],
        Modifier::Button(Button::Tall) => [6, 16, 86],
        Modifier::Button(Button::Short) => [8, 18, 88],
        Modifier::Button(Button::Top) => [10, 20, 90],
        Modifier::Button(Button::Side) => [12, 22, 92],
        Modifier::Button(Button::DpadUp) => [42, 26, 74],
        Modifier::Button(Button::DpadDown) => [44, 28, 76],
        Modifier::Button(Button::DpadLeft) => [46, 30, 78],
        Modifier::Button(Button::DpadRight) => [48, 32, 80],
        Modifier::Button(Button::KnobPress) => [34, 56, 68],
        Modifier::Button(Button::ScrollPress) => [36, 54, 70],
        Modifier::Button(Button::DialPress) => [38, 58, 66],
        Modifier::Button(Button::Tour) => [40, 60, 72],
        Modifier::Button(Button::C1) => [50, 62, 82],
        Modifier::Button(Button::C2) => [52, 64, 84],
    };
    match axis {
        Axis::Knob => knob,
        Axis::Scroll => scroll,
        Axis::Dial => dial,
    }
}

/// 「軸 × 修飾」ごとの強度と速度。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HapticConfig {
    /// 設定を反映済みのメッセージ。
    message: [u8; 94],
}

impl HapticConfig {
    /// 組み合わせ 1 つの強度と速度を設定する。
    pub fn set(&mut self, axis: Axis, modifier: Modifier, strength: Strength, speed: Speed) {
        self.message[value_offset(axis, modifier)] = strength.bits() + speed.bits();
    }

    /// 軸の全組み合わせの強度と速度を設定する。
    pub fn set_axis(&mut self, axis: Axis, strength: Strength, speed: Speed) {
        for modifier in Modifier::ALL {
            self.set(axis, modifier, strength, speed);
        }
    }

    /// 送信する 94 バイトのメッセージを組み立てる。
    pub fn encode(&self) -> [u8; 94] {
        self.message
    }
}

impl Default for HapticConfig {
    /// 全組み合わせを「強、中速」にする。
    fn default() -> Self {
        let mut config = HapticConfig { message: TEMPLATE };
        for axis in Axis::ALL {
            config.set_axis(axis, Strength::Strong, Speed::Medium);
        }
        config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 実機キャプチャの資料。
    const CAPTURES: &str = include_str!("../../../../docs/protocol/haptic-captures.md");

    /// 資料の見出し `heading` の後にある最初のコードブロックを 94 バイトとして読む。
    fn documented_message(heading: &str) -> [u8; 94] {
        let (_, section) = CAPTURES
            .split_once(heading)
            .unwrap_or_else(|| panic!("資料に見出し「{heading}」がありません。"));
        let block = section
            .split("```")
            .nth(1)
            .unwrap_or_else(|| panic!("見出し「{heading}」の後にコードブロックがありません。"));
        let hex: String = block.chars().filter(|c| !c.is_whitespace()).collect();
        let bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| {
                u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or_else(|_| {
                    panic!("見出し「{heading}」のコードブロックに 16 進でない文字があります。")
                })
            })
            .collect();
        bytes.try_into().unwrap_or_else(|bytes: Vec<u8>| {
            panic!(
                "見出し「{heading}」のコードブロックは 94 バイトである必要がありますが、{} バイトです。",
                bytes.len()
            )
        })
    }

    /// 資料のキャプチャ `number` のバイト列。
    fn captured_message(number: u32) -> [u8; 94] {
        documented_message(&format!("## キャプチャ {number}:"))
    }

    /// 設計書 2.4 節のオフセット表 (修飾、Knob、Scroll、Dial)。
    const SPEC_OFFSETS: [(Modifier, usize, usize, usize); 15] = [
        (Modifier::None, 4, 14, 24),
        (Modifier::Button(Button::Tall), 6, 16, 86),
        (Modifier::Button(Button::Short), 8, 18, 88),
        (Modifier::Button(Button::Top), 10, 20, 90),
        (Modifier::Button(Button::Side), 12, 22, 92),
        (Modifier::Button(Button::DpadUp), 42, 26, 74),
        (Modifier::Button(Button::DpadDown), 44, 28, 76),
        (Modifier::Button(Button::DpadLeft), 46, 30, 78),
        (Modifier::Button(Button::DpadRight), 48, 32, 80),
        (Modifier::Button(Button::KnobPress), 34, 56, 68),
        (Modifier::Button(Button::ScrollPress), 36, 54, 70),
        (Modifier::Button(Button::DialPress), 38, 58, 66),
        (Modifier::Button(Button::Tour), 40, 60, 72),
        (Modifier::Button(Button::C1), 50, 62, 82),
        (Modifier::Button(Button::C2), 52, 64, 84),
    ];

    /// SPEC_OFFSETS を (軸、修飾、オフセット) の 45 組に展開する。
    fn spec_offsets() -> impl Iterator<Item = (Axis, Modifier, usize)> {
        SPEC_OFFSETS
            .into_iter()
            .flat_map(|(modifier, knob, scroll, dial)| {
                [
                    (Axis::Knob, modifier, knob),
                    (Axis::Scroll, modifier, scroll),
                    (Axis::Dial, modifier, dial),
                ]
            })
    }

    /// 全組み合わせを同じ強度と速度にした設定。
    fn config_with_all(strength: Strength, speed: Speed) -> HapticConfig {
        let mut config = HapticConfig::default();
        for axis in Axis::ALL {
            for modifier in Modifier::ALL {
                config.set(axis, modifier, strength, speed);
            }
        }
        config
    }

    /// キャプチャ 1 (公式コンソールの既定状態) を再現する設定。値は資料の備考による。
    fn capture_1_config() -> HapticConfig {
        let mut config = config_with_all(Strength::Strong, Speed::Medium);
        let strong_and_fast = [
            (Axis::Knob, Modifier::Button(Button::Top)),
            (Axis::Knob, Modifier::Button(Button::Side)),
            (Axis::Scroll, Modifier::Button(Button::Tall)),
            (Axis::Scroll, Modifier::Button(Button::Side)),
        ];
        for (axis, modifier) in strong_and_fast {
            config.set(axis, modifier, Strength::Strong, Speed::Fast);
        }
        for modifier in Modifier::ALL {
            config.set(Axis::Dial, modifier, Strength::Strong, Speed::Fast);
        }
        config
    }

    /// キャプチャ 2〜5 でコンソールが書き換えた Knob の 12 組み合わせ (資料の備考)。
    const KNOB_COMBINATIONS_CHANGED_BY_CONSOLE: [Modifier; 12] = [
        Modifier::None,
        Modifier::Button(Button::Short),
        Modifier::Button(Button::KnobPress),
        Modifier::Button(Button::ScrollPress),
        Modifier::Button(Button::DialPress),
        Modifier::Button(Button::Tour),
        Modifier::Button(Button::DpadUp),
        Modifier::Button(Button::DpadDown),
        Modifier::Button(Button::DpadLeft),
        Modifier::Button(Button::DpadRight),
        Modifier::Button(Button::C1),
        Modifier::Button(Button::C2),
    ];

    #[test]
    fn modifier_all_lists_none_then_every_button_in_order() {
        let expected: Vec<Modifier> = std::iter::once(Modifier::None)
            .chain(Button::ALL.map(Modifier::Button))
            .collect();
        assert_eq!(
            Modifier::ALL.to_vec(),
            expected,
            "Modifier::ALL は None の後に Button::ALL の順で全ボタンを並べる必要があります。"
        );
    }

    #[test]
    fn encode_matches_fixed_template_when_every_combination_is_off_and_fast() {
        assert_eq!(
            config_with_all(Strength::Off, Speed::Fast).encode(),
            documented_message("## 固定テンプレート"),
            "全組み合わせが「なし、速い」の組み立て結果は、資料の固定テンプレートと一致する必要があります。"
        );
    }

    #[test]
    fn encode_matches_capture_1_console_default() {
        assert_eq!(
            capture_1_config().encode(),
            captured_message(1),
            "キャプチャ 1 の設定を再現した組み立て結果は、資料のバイト列と一致する必要があります。"
        );
    }

    #[test]
    fn encode_matches_captures_2_to_5_knob_changed_in_console() {
        let cases = [
            (2, Strength::Strong, Speed::Fast),
            (3, Strength::Strong, Speed::Slow),
            (4, Strength::Weak, Speed::Medium),
            (5, Strength::Off, Speed::Medium),
        ];
        for (number, strength, speed) in cases {
            let mut config = capture_1_config();
            for modifier in KNOB_COMBINATIONS_CHANGED_BY_CONSOLE {
                config.set(Axis::Knob, modifier, strength, speed);
            }
            assert_eq!(
                config.encode(),
                captured_message(number),
                "キャプチャ {number} の設定を再現した組み立て結果は、資料のバイト列と一致する必要があります。"
            );
        }
    }

    #[test]
    fn encode_matches_capture_6_feedback_disabled() {
        let mut config = config_with_all(Strength::Off, Speed::Fast);
        // 作動 OFF でも Knob 単体、Tall+Knob、Scroll 単体の速度は中速のまま残る
        let off_and_medium = [
            (Axis::Knob, Modifier::None),
            (Axis::Knob, Modifier::Button(Button::Tall)),
            (Axis::Scroll, Modifier::None),
        ];
        for (axis, modifier) in off_and_medium {
            config.set(axis, modifier, Strength::Off, Speed::Medium);
        }
        assert_eq!(
            config.encode(),
            captured_message(6),
            "キャプチャ 6 の設定を再現した組み立て結果は、資料のバイト列と一致する必要があります。"
        );
    }

    #[test]
    fn encode_sets_value_as_sum_of_strength_and_speed() {
        // 設計書 2.4 節の値の規則 (強度、速度、値バイト)
        let cases = [
            (Strength::Off, Speed::Fast, 0x00),
            (Strength::Off, Speed::Medium, 0x01),
            (Strength::Off, Speed::Slow, 0x02),
            (Strength::Weak, Speed::Fast, 0x04),
            (Strength::Weak, Speed::Medium, 0x05),
            (Strength::Weak, Speed::Slow, 0x06),
            (Strength::Strong, Speed::Fast, 0x08),
            (Strength::Strong, Speed::Medium, 0x09),
            (Strength::Strong, Speed::Slow, 0x0a),
        ];
        for (strength, speed, value) in cases {
            let mut config = HapticConfig::default();
            config.set(Axis::Knob, Modifier::None, strength, speed);
            // オフセット 4 は Knob 単体の値バイト
            assert_eq!(
                config.encode()[4],
                value,
                "{strength:?} と {speed:?} の値バイトは {value:#04x} である必要があります。"
            );
        }
    }

    #[test]
    fn default_sets_every_combination_to_strong_and_medium() {
        let mut expected = documented_message("## 固定テンプレート");
        for (_, _, offset) in spec_offsets() {
            expected[offset] = 0x09;
        }
        assert_eq!(
            HapticConfig::default().encode(),
            expected,
            "既定値は全組み合わせが「強、中速」(0x09) である必要があります。"
        );
    }

    #[test]
    fn set_changes_only_offset_in_spec_table_for_each_combination() {
        let base = HapticConfig::default();
        let base_message = base.encode();
        for (axis, modifier, offset) in spec_offsets() {
            let mut config = base.clone();
            config.set(axis, modifier, Strength::Weak, Speed::Slow);
            let message = config.encode();
            let changed: Vec<usize> = (0..message.len())
                .filter(|&i| message[i] != base_message[i])
                .collect();
            assert_eq!(
                changed,
                [offset],
                "{axis:?} と {modifier:?} の組み合わせの設定は、オフセット {offset} だけを変える必要があります。"
            );
            assert_eq!(
                message[offset],
                0x06,
                "{axis:?} と {modifier:?} の組み合わせの値バイトは「弱、遅い」(0x06) である必要があります。"
            );
        }
    }

    #[test]
    fn set_axis_rewrites_every_combination_of_target_axis_only() {
        for target in Axis::ALL {
            let mut config = HapticConfig::default();
            config.set_axis(target, Strength::Weak, Speed::Slow);
            let message = config.encode();
            for (axis, modifier, offset) in spec_offsets() {
                let expected = if axis == target { 0x06 } else { 0x09 };
                assert_eq!(
                    message[offset],
                    expected,
                    "{target:?} の set_axis の後、{axis:?} と {modifier:?} の組み合わせの値バイトは {expected:#04x} である必要があります。"
                );
            }
        }
    }
}
