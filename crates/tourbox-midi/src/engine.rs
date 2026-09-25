//! デバイスのイベントを、設定の割り当てに従って MIDI メッセージ列に変換する状態機械。

use std::collections::HashMap;
use std::iter;

use tourbox::device::DeviceEvent;
use tourbox::protocol::{Axis, Button, Direction, Event};

use crate::config::{
    ButtonAssignment, ButtonKind, MappingSet, RelativeEncoding, RotationAssignment, RotationMode,
};
use crate::midi_msg::MidiMessage;

/// MIDI のデータバイトの最大値。
const DATA_MAX: u8 = 127;

/// engine が返す送信単位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outgoing {
    pub message: MidiMessage,
    pub origin: Origin,
}

/// メッセージの由来。出力層の台帳はボタン由来だけを扱う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    ButtonOn,
    ButtonOff,
    Rotation,
}

/// 修飾状態、押下中のボタン、絶対 CC の内部値を持つ変換器。
pub struct Engine {
    mapping: MappingSet,
    /// 有効な修飾ボタン。同時に 1 つだけで、先に押したものが有効になる。
    modifier: Option<Button>,
    /// 押下中のボタンを押下の順に並べたもの。
    pressed: Vec<Pressed>,
    /// 絶対 CC の内部値。キーは割り当てが見つかったレイヤと軸で、回転がまだなければ持たない。
    absolute_values: HashMap<(Option<Button>, Axis), u8>,
}

/// 押下中のボタンと、解放時に送る Off。
struct Pressed {
    button: Button,
    /// 割り当てがなかった押下では None。
    off: Option<MidiMessage>,
}

impl Engine {
    pub fn new(mapping: MappingSet) -> Self {
        Self {
            mapping,
            modifier: None,
            pressed: Vec::new(),
            absolute_values: HashMap::new(),
        }
    }

    /// イベントを変換する。`Disconnected` では `release_all` と同じ結果を返す。
    pub fn handle(&mut self, event: DeviceEvent) -> Vec<Outgoing> {
        match event {
            DeviceEvent::Input(Event::Press(button)) => self.press(button),
            DeviceEvent::Input(Event::Release(button)) => self.release(button),
            DeviceEvent::Input(Event::Rotate(axis, direction)) => self.rotate(axis, direction),
            DeviceEvent::Input(Event::Unknown(_)) | DeviceEvent::Connected => Vec::new(),
            DeviceEvent::Disconnected => self.release_all(),
        }
    }

    /// 押下中のボタンすべての Off を返し、修飾状態と押下の記憶を初期化する。
    pub fn release_all(&mut self) -> Vec<Outgoing> {
        self.modifier = None;
        self.pressed
            .drain(..)
            .filter_map(|pressed| pressed.off)
            .map(|message| Outgoing {
                message,
                origin: Origin::ButtonOff,
            })
            .collect()
    }

    /// 割り当てを差し替える。絶対 CC の内部値は、同じレイヤと軸に同じ CC の絶対 CC が残るものだけ引き継ぐ。
    pub fn replace_mapping(&mut self, mapping: MappingSet) {
        let current = &self.mapping;
        self.absolute_values.retain(|&(layer, axis), _| {
            absolute_cc(current, layer, axis)
                .is_some_and(|cc| absolute_cc(&mapping, layer, axis) == Some(cc))
        });
        self.mapping = mapping;
    }

    fn press(&mut self, button: Button) -> Vec<Outgoing> {
        if self.pressed.iter().any(|pressed| pressed.button == button) {
            return Vec::new();
        }
        let messages = self.button_assignment(button).map(on_and_off);
        if self.modifier.is_none() && self.mapping.modifiers().contains(&button) {
            self.modifier = Some(button);
        }
        self.pressed.push(Pressed {
            button,
            off: messages.map(|(_, off)| off),
        });
        messages
            .map(|(on, _)| Outgoing {
                message: on,
                origin: Origin::ButtonOn,
            })
            .into_iter()
            .collect()
    }

    fn release(&mut self, button: Button) -> Vec<Outgoing> {
        let Some(index) = self
            .pressed
            .iter()
            .position(|pressed| pressed.button == button)
        else {
            return Vec::new();
        };
        let pressed = self.pressed.remove(index);
        if self.modifier == Some(button) {
            self.modifier = None;
        }
        pressed
            .off
            .map(|message| Outgoing {
                message,
                origin: Origin::ButtonOff,
            })
            .into_iter()
            .collect()
    }

    fn rotate(&mut self, axis: Axis, direction: Direction) -> Vec<Outgoing> {
        let Some((layer, assignment)) = self.rotation_assignment(axis) else {
            return Vec::new();
        };
        let increase = (direction == Direction::Clockwise) != assignment.invert;
        let value = match assignment.mode {
            RotationMode::Absolute { step, initial } => {
                let value = self.absolute_values.entry((layer, axis)).or_insert(initial);
                *value = if increase {
                    value.saturating_add(step).min(DATA_MAX)
                } else {
                    value.saturating_sub(step)
                };
                *value
            }
            RotationMode::Relative { step, encoding } => relative_value(step, encoding, increase),
        };
        vec![Outgoing {
            message: MidiMessage::ControlChange {
                channel: assignment.channel,
                cc: assignment.cc,
                value,
            },
            origin: Origin::Rotation,
        }]
    }

    /// 割り当てを探すレイヤ。有効な修飾のレイヤ、基本レイヤの順。
    fn layers(&self) -> impl Iterator<Item = Option<Button>> {
        self.modifier.map(Some).into_iter().chain(iter::once(None))
    }

    fn button_assignment(&self, button: Button) -> Option<ButtonAssignment> {
        self.layers()
            .find_map(|layer| self.mapping.button(layer, button).copied())
    }

    /// 回転の割り当てと、それが見つかったレイヤ。
    fn rotation_assignment(&self, axis: Axis) -> Option<(Option<Button>, RotationAssignment)> {
        self.layers().find_map(|layer| {
            self.mapping
                .rotation(layer, axis)
                .map(|assignment| (layer, *assignment))
        })
    }
}

/// ボタンの割り当てから、押下時に送る On と解放時に送る Off を作る。
fn on_and_off(assignment: ButtonAssignment) -> (MidiMessage, MidiMessage) {
    let channel = assignment.channel;
    match assignment.kind {
        ButtonKind::Note { note, velocity } => (
            MidiMessage::NoteOn {
                channel,
                note,
                velocity,
            },
            MidiMessage::NoteOff { channel, note },
        ),
        ButtonKind::Cc { cc } => (
            MidiMessage::ControlChange {
                channel,
                cc,
                value: DATA_MAX,
            },
            MidiMessage::ControlChange {
                channel,
                cc,
                value: 0,
            },
        ),
    }
}

/// 相対 CC の 1 ノッチ分の値。`increase` が偽なら -`step` を符号化する。
fn relative_value(step: u8, encoding: RelativeEncoding, increase: bool) -> u8 {
    match (encoding, increase) {
        // 7 ビットの 2 の補数
        (RelativeEncoding::TwosComplement, true) => step,
        (RelativeEncoding::TwosComplement, false) => 128 - step,
        // 64 を 0 とする
        (RelativeEncoding::BinaryOffset, true) => 64 + step,
        (RelativeEncoding::BinaryOffset, false) => 64 - step,
    }
}

/// レイヤ `layer` の軸 `axis` が絶対 CC なら、その CC 番号。
fn absolute_cc(mapping: &MappingSet, layer: Option<Button>, axis: Axis) -> Option<u8> {
    mapping
        .rotation(layer, axis)
        .filter(|assignment| matches!(assignment.mode, RotationMode::Absolute { .. }))
        .map(|assignment| assignment.cc)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tourbox::protocol::{Axis, Button, Direction, Event};

    use super::*;
    use crate::config::Config;

    /// `[midi]` の既定チャンネルを `channel` (1 起点) にして、`[map]` の中身 `map` を解決する。
    fn mapping_on(channel: u8, map: &str) -> MappingSet {
        let text =
            format!("[midi]\noutput = \"TourBox MIDI\"\nchannel = {channel}\n[map]\n{map}\n");
        Config::parse(&text, Path::new("config.toml"))
            .unwrap_or_else(|error| panic!("検証に通る必要があります: {error}"))
            .resolve_mapping()
    }

    /// 既定チャンネル 1 (0 起点で 0) で `[map]` の中身 `map` を解決する。
    fn mapping(map: &str) -> MappingSet {
        mapping_on(1, map)
    }

    fn press(button: Button) -> DeviceEvent {
        DeviceEvent::Input(Event::Press(button))
    }

    fn release(button: Button) -> DeviceEvent {
        DeviceEvent::Input(Event::Release(button))
    }

    fn cw(axis: Axis) -> DeviceEvent {
        DeviceEvent::Input(Event::Rotate(axis, Direction::Clockwise))
    }

    fn ccw(axis: Axis) -> DeviceEvent {
        DeviceEvent::Input(Event::Rotate(axis, Direction::CounterClockwise))
    }

    fn note_on(channel: u8, note: u8, velocity: u8) -> MidiMessage {
        MidiMessage::NoteOn {
            channel,
            note,
            velocity,
        }
    }

    fn note_off(channel: u8, note: u8) -> MidiMessage {
        MidiMessage::NoteOff { channel, note }
    }

    fn cc(channel: u8, cc: u8, value: u8) -> MidiMessage {
        MidiMessage::ControlChange { channel, cc, value }
    }

    fn on(message: MidiMessage) -> Outgoing {
        Outgoing {
            message,
            origin: Origin::ButtonOn,
        }
    }

    fn off(message: MidiMessage) -> Outgoing {
        Outgoing {
            message,
            origin: Origin::ButtonOff,
        }
    }

    fn turn(message: MidiMessage) -> Outgoing {
        Outgoing {
            message,
            origin: Origin::Rotation,
        }
    }

    /// 回転の方向と、その 1 ノッチで送る CC の値の列。
    type Notches = &'static [(Direction, u8)];

    /// 操作を順に渡し、各操作の出力を期待値と比べる。
    fn assert_steps(engine: &mut Engine, steps: Vec<(DeviceEvent, Vec<Outgoing>)>, case: &str) {
        for (index, (event, expected)) in steps.into_iter().enumerate() {
            let description = format!("{event:?}");
            assert_eq!(
                engine.handle(event),
                expected,
                "{case}: {} 番目の操作 {description} の出力が期待と異なります。",
                index + 1
            );
        }
    }

    #[test]
    fn buttons_send_on_when_pressed_and_off_when_released_on_resolved_channel() {
        let mapping = mapping_on(
            3,
            concat!(
                "tall = { note = 60 }\n",
                "c1 = { note = 62, velocity = 90, channel = 16 }\n",
                "top = { cc = 20 }\n",
                "c2 = { cc = 21, channel = 5 }\n",
            ),
        );
        let cases = [
            (
                "既定チャンネルの Note",
                Button::Tall,
                on(note_on(2, 60, 127)),
                off(note_off(2, 60)),
            ),
            (
                "velocity とチャンネルを上書きした Note",
                Button::C1,
                on(note_on(15, 62, 90)),
                off(note_off(15, 62)),
            ),
            (
                "既定チャンネルの CC",
                Button::Top,
                on(cc(2, 20, 127)),
                off(cc(2, 20, 0)),
            ),
            (
                "チャンネルを上書きした CC",
                Button::C2,
                on(cc(4, 21, 127)),
                off(cc(4, 21, 0)),
            ),
        ];
        for (case, button, pressed, released) in cases {
            let mut engine = Engine::new(mapping.clone());
            assert_steps(
                &mut engine,
                vec![
                    (press(button), vec![pressed]),
                    (release(button), vec![released]),
                ],
                case,
            );
        }
    }

    #[test]
    fn absolute_cc_steps_internal_value_within_0_to_127() {
        use Direction::{Clockwise as Cw, CounterClockwise as Ccw};

        let cases: [(&str, &str, Notches); 7] = [
            (
                "既定 (initial 0、step 1)",
                "knob = { cc = 1 }",
                &[(Cw, 1), (Cw, 2), (Ccw, 1)],
            ),
            (
                "下限の 0 で飽和する",
                "knob = { cc = 1, initial = 1 }",
                &[(Ccw, 0), (Ccw, 0), (Cw, 1)],
            ),
            (
                "上限の 127 で飽和する",
                "knob = { cc = 1, initial = 126 }",
                &[(Cw, 127), (Cw, 127), (Ccw, 126)],
            ),
            (
                "step の単位で増減する",
                "knob = { cc = 1, step = 5, initial = 10 }",
                &[(Cw, 15), (Ccw, 10), (Ccw, 5), (Ccw, 0), (Ccw, 0)],
            ),
            (
                "大きな step でも 0〜127 で飽和する",
                "knob = { cc = 1, step = 100, initial = 64 }",
                &[(Cw, 127), (Ccw, 27), (Ccw, 0)],
            ),
            (
                "invert で方向が反転する",
                "knob = { cc = 1, invert = true, initial = 64 }",
                &[(Cw, 63), (Cw, 62), (Ccw, 63)],
            ),
            (
                "step 127 の最大値",
                "knob = { cc = 1, step = 127 }",
                &[(Cw, 127), (Ccw, 0)],
            ),
        ];
        for (case, map, rotations) in cases {
            let mut engine = Engine::new(mapping(map));
            let steps = rotations
                .iter()
                .map(|&(direction, value)| {
                    (
                        DeviceEvent::Input(Event::Rotate(Axis::Knob, direction)),
                        vec![turn(cc(0, 1, value))],
                    )
                })
                .collect();
            assert_steps(&mut engine, steps, case);
        }
    }

    #[test]
    fn relative_cc_encodes_signed_step_in_both_directions() {
        use Direction::{Clockwise as Cw, CounterClockwise as Ccw};

        let cases: [(&str, &str, Notches); 7] = [
            (
                "2 の補数 (既定)、step 1",
                "scroll = { cc = 2, mode = \"relative\" }",
                &[(Cw, 1), (Cw, 1), (Ccw, 127), (Ccw, 127)],
            ),
            (
                "2 の補数、step 5",
                "scroll = { cc = 2, mode = \"relative\", step = 5 }",
                &[(Cw, 5), (Ccw, 123)],
            ),
            (
                "2 の補数、step 63",
                "scroll = { cc = 2, mode = \"relative\", encoding = \"twos_complement\", step = 63 }",
                &[(Cw, 63), (Ccw, 65)],
            ),
            (
                "binary_offset、step 1",
                "scroll = { cc = 2, mode = \"relative\", encoding = \"binary_offset\" }",
                &[(Cw, 65), (Cw, 65), (Ccw, 63), (Ccw, 63)],
            ),
            (
                "binary_offset、step 63",
                "scroll = { cc = 2, mode = \"relative\", encoding = \"binary_offset\", step = 63 }",
                &[(Cw, 127), (Ccw, 1)],
            ),
            (
                "2 の補数、invert",
                "scroll = { cc = 2, mode = \"relative\", invert = true }",
                &[(Cw, 127), (Ccw, 1)],
            ),
            (
                "binary_offset、step 3、invert",
                "scroll = { cc = 2, mode = \"relative\", encoding = \"binary_offset\", step = 3, invert = true }",
                &[(Cw, 61), (Ccw, 67)],
            ),
        ];
        for (case, map, rotations) in cases {
            let mut engine = Engine::new(mapping(map));
            let steps = rotations
                .iter()
                .map(|&(direction, value)| {
                    (
                        DeviceEvent::Input(Event::Rotate(Axis::Scroll, direction)),
                        vec![turn(cc(0, 2, value))],
                    )
                })
                .collect();
            assert_steps(&mut engine, steps, case);
        }
    }

    #[test]
    fn rotation_uses_resolved_channel_of_each_axis() {
        let mut engine = Engine::new(mapping_on(
            3,
            "knob = { cc = 1 }\ndial = { cc = 3, mode = \"relative\", channel = 16 }",
        ));
        assert_steps(
            &mut engine,
            vec![
                (cw(Axis::Knob), vec![turn(cc(2, 1, 1))]),
                (cw(Axis::Dial), vec![turn(cc(15, 3, 1))]),
            ],
            "既定チャンネルと上書きしたチャンネル",
        );
    }

    #[test]
    fn origin_is_button_on_off_for_buttons_and_rotation_for_every_rotation_value() {
        let mut engine = Engine::new(mapping(concat!(
            "tall = { note = 60 }\n",
            "top = { cc = 20 }\n",
            "knob = { cc = 1, initial = 126 }\n",
            "dial = { cc = 3 }\n",
            "scroll = { cc = 2, mode = \"relative\" }\n",
        )));
        let expectations = [
            (press(Button::Tall), note_on(0, 60, 127), Origin::ButtonOn),
            (release(Button::Tall), note_off(0, 60), Origin::ButtonOff),
            (press(Button::Top), cc(0, 20, 127), Origin::ButtonOn),
            (release(Button::Top), cc(0, 20, 0), Origin::ButtonOff),
            // ボタンの CC と同じ値でも回転由来である
            (cw(Axis::Knob), cc(0, 1, 127), Origin::Rotation),
            (ccw(Axis::Dial), cc(0, 3, 0), Origin::Rotation),
            // 相対 CC の -1 は 127 になる
            (ccw(Axis::Scroll), cc(0, 2, 127), Origin::Rotation),
        ];
        for (event, message, origin) in expectations {
            let description = format!("{event:?}");
            assert_eq!(
                engine.handle(event),
                vec![Outgoing { message, origin }],
                "{description} の由来は {origin:?} である必要があります。"
            );
        }
    }

    #[test]
    fn unassigned_controls_send_nothing() {
        let mut engine = Engine::new(mapping("tall = { note = 60 }"));
        assert_steps(
            &mut engine,
            vec![
                (press(Button::Top), vec![]),
                (release(Button::Top), vec![]),
                (cw(Axis::Knob), vec![]),
                (ccw(Axis::Scroll), vec![]),
            ],
            "割り当てのない操作",
        );
    }

    #[test]
    fn connected_and_unknown_events_send_nothing_and_keep_state() {
        let mut engine = Engine::new(mapping("tall = { note = 60 }"));
        assert_steps(
            &mut engine,
            vec![
                (press(Button::Tall), vec![on(note_on(0, 60, 127))]),
                (DeviceEvent::Connected, vec![]),
                (DeviceEvent::Input(Event::Unknown(0x84)), vec![]),
                (release(Button::Tall), vec![off(note_off(0, 60))]),
            ],
            "Connected と未知の値",
        );
    }

    #[test]
    fn repeated_press_and_release_without_counterpart_send_nothing() {
        let mut engine = Engine::new(mapping("tall = { note = 60 }\ntop = { cc = 20 }"));
        assert_steps(
            &mut engine,
            vec![
                (release(Button::Tall), vec![]),
                (press(Button::Tall), vec![on(note_on(0, 60, 127))]),
                (press(Button::Tall), vec![]),
                (release(Button::Tall), vec![off(note_off(0, 60))]),
                (release(Button::Tall), vec![]),
                (press(Button::Top), vec![on(cc(0, 20, 127))]),
                (press(Button::Top), vec![]),
                (release(Button::Top), vec![off(cc(0, 20, 0))]),
                (release(Button::Top), vec![]),
            ],
            "解放のない再押下と押下のない解放",
        );
    }

    #[test]
    fn modifier_layer_is_used_while_held_and_base_layer_is_used_otherwise() {
        let mut engine = Engine::new(mapping(concat!(
            "tall = { note = 60 }
",
            "top = { cc = 20 }
",
            "knob = { cc = 1 }
",
            "scroll = { cc = 2, mode = \"relative\" }
",
            "[map.with.side]
",
            "top = { note = 70 }
",
            "knob = { cc = 11 }
",
        )));
        assert_steps(
            &mut engine,
            vec![
                (press(Button::Side), vec![]),
                // レイヤに定義がある操作はレイヤの割り当てを使う
                (press(Button::Top), vec![on(note_on(0, 70, 127))]),
                (release(Button::Top), vec![off(note_off(0, 70))]),
                (cw(Axis::Knob), vec![turn(cc(0, 11, 1))]),
                // レイヤに定義がない操作は基本レイヤにフォールバックする
                (press(Button::Tall), vec![on(note_on(0, 60, 127))]),
                (release(Button::Tall), vec![off(note_off(0, 60))]),
                (cw(Axis::Scroll), vec![turn(cc(0, 2, 1))]),
                (release(Button::Side), vec![]),
                // 修飾を離すと基本レイヤに戻る
                (press(Button::Top), vec![on(cc(0, 20, 127))]),
                (release(Button::Top), vec![off(cc(0, 20, 0))]),
                (cw(Axis::Knob), vec![turn(cc(0, 1, 1))]),
            ],
            "Side のレイヤ",
        );
    }

    #[test]
    fn absolute_cc_values_are_kept_per_layer_and_fallback_uses_base_layer_value() {
        let mut engine = Engine::new(mapping(concat!(
            "knob = { cc = 1 }
",
            "dial = { cc = 3, initial = 10 }
",
            "[map.with.side]
",
            "knob = { cc = 11, initial = 50 }
",
        )));
        assert_steps(
            &mut engine,
            vec![
                (cw(Axis::Knob), vec![turn(cc(0, 1, 1))]),
                (cw(Axis::Dial), vec![turn(cc(0, 3, 11))]),
                (press(Button::Side), vec![]),
                // Side のレイヤの knob は基本レイヤと別の内部値を持つ
                (cw(Axis::Knob), vec![turn(cc(0, 11, 51))]),
                (cw(Axis::Knob), vec![turn(cc(0, 11, 52))]),
                // フォールバックした dial は基本レイヤの内部値を進める
                (cw(Axis::Dial), vec![turn(cc(0, 3, 12))]),
                (release(Button::Side), vec![]),
                (cw(Axis::Knob), vec![turn(cc(0, 1, 2))]),
                (cw(Axis::Dial), vec![turn(cc(0, 3, 13))]),
                (press(Button::Side), vec![]),
                (ccw(Axis::Knob), vec![turn(cc(0, 11, 51))]),
            ],
            "レイヤごとの内部値とフォールバック",
        );
    }

    #[test]
    fn only_first_pressed_modifier_is_active_and_later_one_is_not_promoted() {
        let mut engine = Engine::new(mapping(concat!(
            "tour = { note = 61 }
",
            "top = { cc = 20 }
",
            "[map.with.side]
",
            "top = { note = 70 }
",
            "tour = { note = 72 }
",
            "[map.with.tour]
",
            "top = { note = 80 }
",
        )));
        assert_steps(
            &mut engine,
            vec![
                (press(Button::Side), vec![]),
                // Side の修飾中の Tour は Side のレイヤの通常ボタンになる
                (press(Button::Tour), vec![on(note_on(0, 72, 127))]),
                (press(Button::Top), vec![on(note_on(0, 70, 127))]),
                (release(Button::Top), vec![off(note_off(0, 70))]),
                (release(Button::Side), vec![]),
                // Tour を押したままでも修飾に昇格せず、基本レイヤに戻る
                (press(Button::Top), vec![on(cc(0, 20, 127))]),
                (release(Button::Top), vec![off(cc(0, 20, 0))]),
                (release(Button::Tour), vec![off(note_off(0, 72))]),
                // 修飾がないときに押した Tour は修飾になる
                (press(Button::Tour), vec![on(note_on(0, 61, 127))]),
                (press(Button::Side), vec![]),
                (press(Button::Top), vec![on(note_on(0, 80, 127))]),
                (release(Button::Top), vec![off(note_off(0, 80))]),
                (release(Button::Tour), vec![off(note_off(0, 61))]),
                (press(Button::Top), vec![on(cc(0, 20, 127))]),
            ],
            "Side と Tour の同時押し",
        );
    }

    #[test]
    fn modifier_button_sends_its_own_message_while_switching_layer() {
        let cases = [
            (
                "Note の修飾ボタン",
                "side = { note = 50 }
[map.with.side]
top = { note = 70 }",
                Button::Side,
                on(note_on(0, 50, 127)),
                off(note_off(0, 50)),
            ),
            (
                "CC の修飾ボタン",
                "tour = { cc = 30, channel = 2 }
[map.with.tour]
top = { note = 70 }",
                Button::Tour,
                on(cc(1, 30, 127)),
                off(cc(1, 30, 0)),
            ),
        ];
        for (case, map, modifier, pressed, released) in cases {
            let mut engine = Engine::new(mapping(map));
            assert_steps(
                &mut engine,
                vec![
                    (press(modifier), vec![pressed]),
                    (press(Button::Top), vec![on(note_on(0, 70, 127))]),
                    (release(Button::Top), vec![off(note_off(0, 70))]),
                    (release(modifier), vec![released]),
                ],
                case,
            );
        }
    }

    #[test]
    fn release_sends_off_for_message_sent_on_press_even_after_layer_changes() {
        let map = "top = { cc = 20 }
[map.with.side]
top = { note = 70 }";
        let cases = [
            (
                "修飾中に押して修飾を先に離す",
                vec![
                    (press(Button::Side), vec![]),
                    (press(Button::Top), vec![on(note_on(0, 70, 127))]),
                    (release(Button::Side), vec![]),
                    (release(Button::Top), vec![off(note_off(0, 70))]),
                ],
            ),
            (
                "基本レイヤで押して修飾中に離す",
                vec![
                    (press(Button::Top), vec![on(cc(0, 20, 127))]),
                    (press(Button::Side), vec![]),
                    (release(Button::Top), vec![off(cc(0, 20, 0))]),
                    (release(Button::Side), vec![]),
                ],
            ),
        ];
        for (case, steps) in cases {
            let mut engine = Engine::new(mapping(map));
            assert_steps(&mut engine, steps, case);
        }
    }

    #[test]
    fn release_all_returns_off_for_held_buttons_in_press_order_and_resets_state() {
        let mut engine = Engine::new(mapping(concat!(
            "tall = { note = 60 }\n",
            "top = { cc = 20 }\n",
            "c2 = { cc = 21 }\n",
            "knob = { cc = 1 }\n",
            "[map.with.side]\n",
            "top = { note = 70 }\n",
            "c1 = { cc = 40 }\n",
        )));
        assert_steps(
            &mut engine,
            vec![
                (cw(Axis::Knob), vec![turn(cc(0, 1, 1))]),
                (press(Button::Tall), vec![on(note_on(0, 60, 127))]),
                (press(Button::Side), vec![]),
                (press(Button::Top), vec![on(note_on(0, 70, 127))]),
                (press(Button::C1), vec![on(cc(0, 40, 127))]),
                (release(Button::C1), vec![off(cc(0, 40, 0))]),
                (press(Button::C2), vec![on(cc(0, 21, 127))]),
            ],
            "release_all の前",
        );

        assert_eq!(
            engine.release_all(),
            vec![
                off(note_off(0, 60)),
                off(note_off(0, 70)),
                off(cc(0, 21, 0)),
            ],
            "押下中のボタンの Off を押下の順に返す必要があります。"
        );

        assert_steps(
            &mut engine,
            vec![
                // 押下の記憶は初期化されているので、解放しても何も送らない
                (release(Button::Top), vec![]),
                (release(Button::Tall), vec![]),
                // 修飾状態も初期化されているので、Side を離す前でも基本レイヤを使う
                (press(Button::Top), vec![on(cc(0, 20, 127))]),
                // 絶対 CC の内部値は保持する
                (cw(Axis::Knob), vec![turn(cc(0, 1, 2))]),
                (release(Button::Side), vec![]),
            ],
            "release_all の後",
        );
        assert_eq!(
            engine.release_all(),
            vec![off(cc(0, 20, 0))],
            "release_all の後に押したボタンだけの Off を返す必要があります。"
        );
        assert_eq!(
            engine.release_all(),
            vec![],
            "押下中のボタンがなければ空を返す必要があります。"
        );
    }

    #[test]
    fn disconnected_returns_same_as_release_all_and_resets_state() {
        let map = mapping(concat!(
            "tall = { note = 60 }\n",
            "top = { cc = 20 }\n",
            "knob = { cc = 1 }\n",
            "[map.with.side]\n",
            "top = { note = 70 }\n",
        ));
        let held = [
            press(Button::Tall),
            press(Button::Side),
            press(Button::Top),
            cw(Axis::Knob),
        ];
        let expected_offs = vec![off(note_off(0, 60)), off(note_off(0, 70))];

        let mut by_release_all = Engine::new(map.clone());
        let mut by_disconnected = Engine::new(map.clone());
        for event in held {
            by_release_all.handle(event.clone());
            by_disconnected.handle(event);
        }
        assert_eq!(
            by_release_all.release_all(),
            expected_offs,
            "release_all は押下中のボタンの Off を返す必要があります。"
        );
        assert_eq!(
            by_disconnected.handle(DeviceEvent::Disconnected),
            expected_offs,
            "Disconnected は release_all と同じ Off を返す必要があります。"
        );
        assert_steps(
            &mut by_disconnected,
            vec![
                (release(Button::Top), vec![]),
                (press(Button::Top), vec![on(cc(0, 20, 127))]),
                (cw(Axis::Knob), vec![turn(cc(0, 1, 2))]),
            ],
            "Disconnected の後",
        );

        assert_eq!(
            Engine::new(map).handle(DeviceEvent::Disconnected),
            vec![],
            "押下中のボタンがなければ Disconnected は空を返す必要があります。"
        );
    }

    #[test]
    fn replace_mapping_keeps_absolute_value_only_for_same_cc_in_same_layer_and_axis() {
        const OLD: &str = "knob = { cc = 1 }\n[map.with.side]\nknob = { cc = 11 }";
        let cases: [(&str, &[&str], Outgoing, Outgoing); 9] = [
            (
                "同じ割り当て",
                &[OLD],
                turn(cc(0, 1, 4)),
                turn(cc(0, 11, 3)),
            ),
            (
                "step と initial の変更",
                &["knob = { cc = 1, step = 2, initial = 100 }\n[map.with.side]\nknob = { cc = 11, initial = 100 }"],
                turn(cc(0, 1, 5)),
                turn(cc(0, 11, 3)),
            ),
            (
                "invert の変更",
                &["knob = { cc = 1, invert = true }\n[map.with.side]\nknob = { cc = 11 }"],
                turn(cc(0, 1, 2)),
                turn(cc(0, 11, 3)),
            ),
            (
                "チャンネルの変更",
                &["knob = { cc = 1, channel = 2 }\n[map.with.side]\nknob = { cc = 11 }"],
                turn(cc(1, 1, 4)),
                turn(cc(0, 11, 3)),
            ),
            (
                "修飾レイヤの cc の変更",
                &["knob = { cc = 1 }\n[map.with.side]\nknob = { cc = 12, initial = 30 }"],
                turn(cc(0, 1, 4)),
                turn(cc(0, 12, 31)),
            ),
            (
                "基本レイヤの cc の変更",
                &["knob = { cc = 5, initial = 50 }\n[map.with.side]\nknob = { cc = 11 }"],
                turn(cc(0, 5, 51)),
                turn(cc(0, 11, 3)),
            ),
            (
                "relative を経由して絶対に戻す",
                &[
                    "knob = { cc = 1, mode = \"relative\" }\n[map.with.side]\nknob = { cc = 11 }",
                    "knob = { cc = 1, initial = 20 }\n[map.with.side]\nknob = { cc = 11 }",
                ],
                turn(cc(0, 1, 21)),
                turn(cc(0, 11, 3)),
            ),
            (
                "レイヤの割り当ての削除を経由して戻す",
                &[
                    "knob = { cc = 1 }\n[map.with.side]\ntop = { note = 70 }",
                    "knob = { cc = 1 }\n[map.with.side]\nknob = { cc = 11, initial = 40 }",
                ],
                turn(cc(0, 1, 4)),
                turn(cc(0, 11, 41)),
            ),
            (
                "修飾レイヤごとの削除を経由して戻す",
                &["knob = { cc = 1 }", OLD],
                turn(cc(0, 1, 4)),
                turn(cc(0, 11, 1)),
            ),
        ];
        for (case, replacements, base, side) in cases {
            let mut engine = Engine::new(mapping(OLD));
            // 内部値を基本レイヤ 3、Side のレイヤ 2 にする
            for event in [
                cw(Axis::Knob),
                cw(Axis::Knob),
                cw(Axis::Knob),
                press(Button::Side),
                cw(Axis::Knob),
                cw(Axis::Knob),
                release(Button::Side),
            ] {
                engine.handle(event);
            }
            for map in replacements {
                engine.replace_mapping(mapping(map));
            }
            assert_steps(
                &mut engine,
                vec![
                    (cw(Axis::Knob), vec![base]),
                    (press(Button::Side), vec![]),
                    (cw(Axis::Knob), vec![side]),
                    (release(Button::Side), vec![]),
                ],
                case,
            );
        }
    }

    #[test]
    fn replace_mapping_keeps_modifier_and_pressed_buttons() {
        let mut engine = Engine::new(mapping(
            "top = { cc = 20 }\n[map.with.side]\ntop = { note = 70 }\nknob = { cc = 11 }",
        ));
        assert_steps(
            &mut engine,
            vec![
                (press(Button::Side), vec![]),
                (press(Button::Top), vec![on(note_on(0, 70, 127))]),
            ],
            "差し替えの前",
        );

        engine.replace_mapping(mapping(
            "top = { cc = 21 }\n[map.with.side]\ntop = { note = 71 }\nknob = { cc = 12 }",
        ));

        assert_steps(
            &mut engine,
            vec![
                // 押下時に送った Note 70 の Off を送る
                (release(Button::Top), vec![off(note_off(0, 70))]),
                // Side の修飾は有効なまま、新しい割り当てを使う
                (cw(Axis::Knob), vec![turn(cc(0, 12, 1))]),
                (press(Button::Top), vec![on(note_on(0, 71, 127))]),
                (release(Button::Top), vec![off(note_off(0, 71))]),
                (release(Button::Side), vec![]),
                (press(Button::Top), vec![on(cc(0, 21, 127))]),
            ],
            "差し替えの後",
        );
    }
}
