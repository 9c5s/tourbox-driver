//! 送信する MIDI メッセージ (Note On、Note Off、Control Change) の 3 バイト表現。

/// ステータスバイトの上位 4 ビット。下位 4 ビットにチャンネルを入れる。
const NOTE_OFF: u8 = 0x80;
const NOTE_ON: u8 = 0x90;
const CONTROL_CHANGE: u8 = 0xb0;

/// 送信する MIDI メッセージ。チャンネルは 0 起点 (0〜15)、その他の値は 0〜127 である。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidiMessage {
    NoteOn {
        channel: u8,
        note: u8,
        velocity: u8,
    },
    /// velocity 0 で送る。
    NoteOff {
        channel: u8,
        note: u8,
    },
    ControlChange {
        channel: u8,
        cc: u8,
        value: u8,
    },
}

impl MidiMessage {
    /// ステータスバイトと 2 つのデータバイトにする。ランニングステータスは使わない。
    pub fn to_bytes(self) -> [u8; 3] {
        match self {
            MidiMessage::NoteOn {
                channel,
                note,
                velocity,
            } => [NOTE_ON | channel, note, velocity],
            MidiMessage::NoteOff { channel, note } => [NOTE_OFF | channel, note, 0],
            MidiMessage::ControlChange { channel, cc, value } => {
                [CONTROL_CHANGE | channel, cc, value]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_bytes_puts_channel_in_status_byte_followed_by_two_data_bytes() {
        let cases = [
            (
                MidiMessage::NoteOn {
                    channel: 0,
                    note: 60,
                    velocity: 127,
                },
                [0x90, 60, 127],
            ),
            (
                MidiMessage::NoteOn {
                    channel: 15,
                    note: 0,
                    velocity: 1,
                },
                [0x9f, 0, 1],
            ),
            (
                MidiMessage::NoteOff {
                    channel: 0,
                    note: 60,
                },
                [0x80, 60, 0],
            ),
            (
                MidiMessage::NoteOff {
                    channel: 15,
                    note: 127,
                },
                [0x8f, 127, 0],
            ),
            (
                MidiMessage::ControlChange {
                    channel: 0,
                    cc: 1,
                    value: 127,
                },
                [0xb0, 1, 127],
            ),
            (
                MidiMessage::ControlChange {
                    channel: 15,
                    cc: 127,
                    value: 0,
                },
                [0xbf, 127, 0],
            ),
        ];
        for (message, expected) in cases {
            assert_eq!(
                message.to_bytes(),
                expected,
                "{message:?} は {expected:02x?} の 3 バイトになる必要があります。"
            );
        }
    }
}
