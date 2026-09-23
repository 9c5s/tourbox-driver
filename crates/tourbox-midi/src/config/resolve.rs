//! 検証済みの設定から、engine とハプティクスが使う形への解決。

use std::collections::HashMap;
use std::iter;

use tourbox::protocol::{Axis, Button, HapticConfig, Modifier};

use super::schema::{ButtonKind, HapticsSection, MapSection, RotationMode};

/// 「レイヤ (基本と修飾ボタンごと) × 操作」の割り当て。チャンネルは解決済み。
///
/// レイヤは `Option<Button>` で表し、None が基本レイヤ、`Some(b)` が修飾ボタン `b` のレイヤである。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappingSet {
    modifiers: Vec<Button>,
    buttons: HashMap<(Option<Button>, Button), ButtonAssignment>,
    rotations: HashMap<(Option<Button>, Axis), RotationAssignment>,
}

/// ボタンの割り当て。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ButtonAssignment {
    /// 送信するチャンネル (0 起点)。
    pub channel: u8,
    pub kind: ButtonKind,
}

/// 回転の割り当て。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RotationAssignment {
    /// 送信するチャンネル (0 起点)。
    pub channel: u8,
    pub cc: u8,
    /// 回転方向を反転する。
    pub invert: bool,
    pub mode: RotationMode,
}

impl MappingSet {
    /// `[map]` の各項目のチャンネルを、省略時は `default_channel` (0 起点) にして解決する。
    pub(super) fn resolve(map: &MapSection, default_channel: u8) -> Self {
        let layers = iter::once((None, &map.base)).chain(
            map.with
                .iter()
                .map(|(button, layer)| (Some(*button), layer)),
        );
        let mut buttons = HashMap::new();
        let mut rotations = HashMap::new();
        for (layer, entries) in layers {
            for (button, entry) in &entries.buttons {
                let assignment = ButtonAssignment {
                    channel: entry.channel.unwrap_or(default_channel),
                    kind: entry.kind,
                };
                buttons.insert((layer, *button), assignment);
            }
            for (axis, entry) in &entries.rotations {
                let assignment = RotationAssignment {
                    channel: entry.channel.unwrap_or(default_channel),
                    cc: entry.cc,
                    invert: entry.invert,
                    mode: entry.mode,
                };
                rotations.insert((layer, *axis), assignment);
            }
        }
        let modifiers = Button::ALL
            .into_iter()
            .filter(|button| map.with.contains_key(button))
            .collect();
        Self {
            modifiers,
            buttons,
            rotations,
        }
    }

    /// 修飾ボタン (`[map.with.<ボタン>]` があるボタン) の一覧。`Button::ALL` の順に並ぶ。
    pub fn modifiers(&self) -> &[Button] {
        &self.modifiers
    }

    /// レイヤ `layer` のボタン `button` の割り当て。レイヤに定義がなければ None で、基本レイヤは参照しない。
    pub fn button(&self, layer: Option<Button>, button: Button) -> Option<&ButtonAssignment> {
        self.buttons.get(&(layer, button))
    }

    /// レイヤ `layer` の軸 `axis` の割り当て。レイヤに定義がなければ None で、基本レイヤは参照しない。
    pub fn rotation(&self, layer: Option<Button>, axis: Axis) -> Option<&RotationAssignment> {
        self.rotations.get(&(layer, axis))
    }
}

/// `[haptics]` の軸の値を全組み合わせに入れ、修飾ごとの上書きを重ねた HapticConfig を作る。
pub(super) fn haptic_config(haptics: &HapticsSection) -> HapticConfig {
    let axis_setting = |axis: Axis| haptics.axes.get(&axis).copied().unwrap_or_default();
    let mut config = HapticConfig::default();
    for axis in Axis::ALL {
        let setting = axis_setting(axis);
        config.set_axis(axis, setting.strength, setting.speed);
    }
    for (&(button, axis), overridden) in &haptics.with {
        let inherited = axis_setting(axis);
        config.set(
            axis,
            Modifier::Button(button),
            overridden.strength.unwrap_or(inherited.strength),
            overridden.speed.unwrap_or(inherited.speed),
        );
    }
    config
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;

    use tourbox::protocol::{Axis, Button, HapticConfig, Modifier, Speed, Strength};
    use tourbox::transport::{ConnectionConfig, TransportKind};

    use crate::config::{
        ButtonAssignment, ButtonKind, Config, ControlCc, HapticsControlConfig, RelativeEncoding,
        RotationAssignment, RotationMode,
    };

    /// `[midi]` (既定チャンネル 1) の後に `rest` を続けた設定を検証する。
    fn config(rest: &str) -> Config {
        let text = format!("[midi]\noutput = \"TourBox MIDI\"\nchannel = 1\n{rest}\n");
        Config::parse(&text, Path::new("config.toml"))
            .unwrap_or_else(|error| panic!("検証に通る必要があります: {error}"))
    }

    #[test]
    fn omitted_haptics_section_gives_default_haptic_config() {
        assert_eq!(
            config("[map]").to_haptic_config(),
            HapticConfig::default(),
            "[haptics] の省略時は既定の HapticConfig と等しい必要があります。"
        );
    }

    #[test]
    fn omitted_haptic_values_default_to_strong_and_medium() {
        let actual = config(
            "[map]\n[haptics]\nscroll = { strength = \"weak\" }\ndial = { speed = \"slow\" }",
        )
        .to_haptic_config();

        let mut expected = HapticConfig::default();
        expected.set_axis(Axis::Knob, Strength::Strong, Speed::Medium);
        expected.set_axis(Axis::Scroll, Strength::Weak, Speed::Medium);
        expected.set_axis(Axis::Dial, Strength::Strong, Speed::Slow);
        assert_eq!(
            actual, expected,
            "省略した軸と値は strong と medium にする必要があります。"
        );
    }

    #[test]
    fn axis_values_apply_to_every_combination_of_the_axis() {
        let actual = config(
            "[map]\n[haptics]\nknob = { strength = \"off\", speed = \"fast\" }\nscroll = { strength = \"weak\", speed = \"slow\" }",
        )
        .to_haptic_config();

        let mut expected = HapticConfig::default();
        for modifier in Modifier::ALL {
            expected.set(Axis::Knob, modifier, Strength::Off, Speed::Fast);
            expected.set(Axis::Scroll, modifier, Strength::Weak, Speed::Slow);
            expected.set(Axis::Dial, modifier, Strength::Strong, Speed::Medium);
        }
        assert_eq!(
            actual, expected,
            "軸の値は修飾なしと修飾付きの全組み合わせに入れる必要があります。"
        );
    }

    #[test]
    fn modifier_overrides_are_applied_on_top_of_axis_values() {
        let actual = config(concat!(
            "[map]\n",
            "[haptics]\n",
            "scroll = { strength = \"weak\", speed = \"fast\" }\n",
            "[haptics.with.side]\n",
            "knob = { strength = \"off\" }\n",
            "scroll = { strength = \"strong\" }\n",
            "[haptics.with.up]\n",
            "scroll = { speed = \"slow\" }\n",
        ))
        .to_haptic_config();

        let mut expected = HapticConfig::default();
        expected.set_axis(Axis::Scroll, Strength::Weak, Speed::Fast);
        // 省略した値は軸の値 (knob は既定の medium、scroll は weak と fast) を継承する
        expected.set(
            Axis::Knob,
            Modifier::Button(Button::Side),
            Strength::Off,
            Speed::Medium,
        );
        expected.set(
            Axis::Scroll,
            Modifier::Button(Button::Side),
            Strength::Strong,
            Speed::Fast,
        );
        expected.set(
            Axis::Scroll,
            Modifier::Button(Button::DpadUp),
            Strength::Weak,
            Speed::Slow,
        );
        assert_eq!(
            actual, expected,
            "修飾ごとの上書きを軸の値に重ねる必要があります。"
        );
    }

    #[test]
    fn mapping_resolves_default_and_item_channels() {
        let text = concat!(
            "[midi]\n",
            "output = \"TourBox MIDI\"\n",
            "channel = 3\n",
            "[map]\n",
            "tall = { note = 60 }\n",
            "c1 = { note = 62, velocity = 90, channel = 2 }\n",
            "top = { cc = 20 }\n",
            "knob = { cc = 1 }\n",
            "dial = { cc = 3, step = 2, invert = true, channel = 16 }\n",
            "[map.with.side]\n",
            "top = { note = 70 }\n",
            "scroll = { cc = 12, mode = \"relative\", encoding = \"binary_offset\", channel = 5 }\n",
        );
        let mapping = Config::parse(text, Path::new("config.toml"))
            .unwrap_or_else(|error| panic!("検証に通る必要があります: {error}"))
            .resolve_mapping();

        let expectations_for_buttons = [
            (
                None,
                Button::Tall,
                ButtonAssignment {
                    channel: 2,
                    kind: ButtonKind::Note {
                        note: 60,
                        velocity: 127,
                    },
                },
            ),
            (
                None,
                Button::C1,
                ButtonAssignment {
                    channel: 1,
                    kind: ButtonKind::Note {
                        note: 62,
                        velocity: 90,
                    },
                },
            ),
            (
                None,
                Button::Top,
                ButtonAssignment {
                    channel: 2,
                    kind: ButtonKind::Cc { cc: 20 },
                },
            ),
            (
                Some(Button::Side),
                Button::Top,
                ButtonAssignment {
                    channel: 2,
                    kind: ButtonKind::Note {
                        note: 70,
                        velocity: 127,
                    },
                },
            ),
        ];
        for (layer, button, expected) in expectations_for_buttons {
            assert_eq!(
                mapping.button(layer, button),
                Some(&expected),
                "レイヤ {layer:?} の {button:?} は、チャンネルを解決した割り当てである必要があります。"
            );
        }

        let expectations_for_rotations = [
            (
                None,
                Axis::Knob,
                RotationAssignment {
                    channel: 2,
                    cc: 1,
                    invert: false,
                    mode: RotationMode::Absolute {
                        step: 1,
                        initial: 0,
                    },
                },
            ),
            (
                None,
                Axis::Dial,
                RotationAssignment {
                    channel: 15,
                    cc: 3,
                    invert: true,
                    mode: RotationMode::Absolute {
                        step: 2,
                        initial: 0,
                    },
                },
            ),
            (
                Some(Button::Side),
                Axis::Scroll,
                RotationAssignment {
                    channel: 4,
                    cc: 12,
                    invert: false,
                    mode: RotationMode::Relative {
                        step: 1,
                        encoding: RelativeEncoding::BinaryOffset,
                    },
                },
            ),
        ];
        for (layer, axis, expected) in expectations_for_rotations {
            assert_eq!(
                mapping.rotation(layer, axis),
                Some(&expected),
                "レイヤ {layer:?} の {axis:?} は、チャンネルを解決した割り当てである必要があります。"
            );
        }
    }

    #[test]
    fn mapping_lookup_does_not_fall_back_to_base_layer() {
        let mapping = config(
            "[map]\ntall = { note = 60 }\nknob = { cc = 1 }\n[map.with.side]\ntop = { note = 70 }",
        )
        .resolve_mapping();

        assert_eq!(
            mapping.button(Some(Button::Side), Button::Tall),
            None,
            "レイヤに定義のないボタンは、基本レイヤにあっても None である必要があります。"
        );
        assert_eq!(
            mapping.rotation(Some(Button::Side), Axis::Knob),
            None,
            "レイヤに定義のない回転は、基本レイヤにあっても None である必要があります。"
        );
        assert_eq!(
            mapping.button(None, Button::Top),
            None,
            "修飾レイヤにしかないボタンは、基本レイヤでは None である必要があります。"
        );
        assert_eq!(
            mapping.button(Some(Button::Tour), Button::Tall),
            None,
            "修飾ボタンでないレイヤは None である必要があります。"
        );
    }

    #[test]
    fn modifiers_are_buttons_with_layers_in_button_order() {
        let mapping = config(
            "[map]\ntour = { note = 61 }\n[map.with.tour]\n[map.with.side]\ntop = { note = 70 }",
        )
        .resolve_mapping();

        assert_eq!(
            mapping.modifiers(),
            &[Button::Side, Button::Tour],
            "[map.with.<ボタン>] があるボタンを、空のレイヤも含めて Button::ALL の順に返す必要があります。"
        );
        assert!(
            config("[map]\ntall = { note = 60 }")
                .resolve_mapping()
                .modifiers()
                .is_empty(),
            "[map.with] がなければ修飾ボタンはない必要があります。"
        );
    }

    #[test]
    fn haptics_control_is_returned_with_zero_based_channel() {
        let actual = config(concat!(
            "[map]\n",
            "[haptics.control]\n",
            "channel = 16\n",
            "master = { cc = 99 }\n",
            "knob = { cc = 100 }\n",
            "scroll = { cc = 101, speed_cc = 102 }\n",
            "[haptics.control.with.side]\n",
            "knob = { cc = 110 }\n",
            "dial = {}\n",
        ))
        .haptics_control();

        let expected = HapticsControlConfig {
            channel: Some(15),
            master: Some(99),
            axes: HashMap::from([
                (
                    Axis::Knob,
                    ControlCc {
                        cc: Some(100),
                        speed_cc: None,
                    },
                ),
                (
                    Axis::Scroll,
                    ControlCc {
                        cc: Some(101),
                        speed_cc: Some(102),
                    },
                ),
            ]),
            with: HashMap::from([
                (
                    (Button::Side, Axis::Knob),
                    ControlCc {
                        cc: Some(110),
                        speed_cc: None,
                    },
                ),
                ((Button::Side, Axis::Dial), ControlCc::default()),
            ]),
        };
        assert_eq!(
            actual, expected,
            "[haptics.control] の割り当てをチャンネル 0 起点で返す必要があります。"
        );
        assert_eq!(
            config("[map]").haptics_control(),
            HapticsControlConfig::default(),
            "[haptics.control] の省略時は割り当てがない必要があります。"
        );
    }

    #[test]
    fn connection_config_keeps_both_transport_and_usb_port() {
        for (transport, expected) in [
            ("auto", TransportKind::Auto),
            ("usb", TransportKind::Usb),
            ("ble", TransportKind::Ble),
        ] {
            let actual = config(&format!(
                "[map]\n[device]\ntransport = \"{transport}\"\nusb_port = \"COM3\""
            ))
            .to_connection_config();

            assert_eq!(
                actual,
                ConnectionConfig {
                    transport: expected,
                    usb_port: Some("COM3".to_owned()),
                },
                "transport = \"{transport}\" と usb_port を両方とも接続の設定に入れる必要があります。"
            );
        }
    }
}
