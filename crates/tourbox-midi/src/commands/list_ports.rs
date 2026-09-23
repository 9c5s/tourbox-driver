//! `list-ports` サブコマンド。MIDI の入出力ポートの名前を一覧表示する。

use crate::midi::{self, PortInfo, PortMode};

/// 出力ポートと入力ポートの名前を標準出力に表示する。
pub fn run() -> anyhow::Result<()> {
    let outputs = midi::list_output_ports()?;
    let inputs = midi::list_input_ports()?;
    print!("{}", render(&outputs, &inputs, PortMode::default_for_os()));
    Ok(())
}

/// 一覧の表示内容を作る。
fn render(outputs: &[PortInfo], inputs: &[PortInfo], mode: PortMode) -> String {
    let mut text = String::new();
    push_section(&mut text, "MIDI 出力ポート:", outputs);
    push_section(&mut text, "MIDI 入力ポート:", inputs);
    if outputs
        .iter()
        .chain(inputs)
        .any(|port| midi::device_key(&port.id).is_some())
    {
        text.push_str("括弧内は機器の識別子です。設定の output に名前が一致する出力ポートがなければ、output に名前が一致する入力ポートと識別子が同じ出力ポートを使います。\n");
    }
    if mode == PortMode::Virtual {
        text.push_str("この OS では設定の output と input の名前で仮想ポートを作成するので、設定する名前はこの一覧にある必要はありません。\n");
    }
    text
}

/// 見出しと名前の一覧を 1 項目 1 行で追加する。機器の識別子があれば名前の後に括弧で添える。
/// 0 件なら (なし) と書く。
fn push_section(text: &mut String, heading: &str, ports: &[PortInfo]) {
    text.push_str(heading);
    text.push('\n');
    if ports.is_empty() {
        text.push_str("  (なし)\n");
    }
    for port in ports {
        text.push_str("  ");
        text.push_str(&port.name);
        if let Some(key) = midi::device_key(&port.id) {
            text.push_str(" (機器 ");
            text.push_str(key);
            text.push(')');
        }
        text.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn infos(ports: &[(&str, &str)]) -> Vec<PortInfo> {
        ports
            .iter()
            .map(|&(name, id)| PortInfo {
                name: name.to_owned(),
                id: id.to_owned(),
            })
            .collect()
    }

    #[test]
    fn render_lists_names_under_headings_and_marks_empty_list() {
        let outputs = infos(&[
            ("loopMIDI Port", "1001"),
            ("Microsoft GS Wavetable Synth", "1002"),
        ]);

        assert_eq!(
            render(&outputs, &[], PortMode::Existing),
            "MIDI 出力ポート:\n  loopMIDI Port\n  Microsoft GS Wavetable Synth\nMIDI 入力ポート:\n  (なし)\n",
            "見出しの下に名前を一覧の順に並べ、0 件なら (なし) と表示する必要があります。"
        );
    }

    #[test]
    fn render_shows_device_key_after_name_and_explains_it() {
        // Windows 10 と loopMIDI 1.0.16 で、WinRT の列挙が返したポートの一部
        let outputs = infos(&[
            (
                "Microsoft GS Wavetable Synth",
                r"\\?\SWD#MMDEVAPI#MicrosoftGSWavetableSynth#{6dc23320-ab33-4ce4-80d4-bbb3ebbf2814}",
            ),
            (
                "MIDI",
                r"\\?\SWD#MMDEVAPI#MIDII_F20B738C.P_0000#{6dc23320-ab33-4ce4-80d4-bbb3ebbf2814}",
            ),
        ]);
        let inputs = infos(&[(
            "TourBox MIDI Out [1]",
            r"\\?\SWD#MMDEVAPI#MIDII_F20B738C.P_0004#{504be32c-ccf6-4d2c-b73f-6f8b3747e22b}",
        )]);

        assert_eq!(
            render(&outputs, &inputs, PortMode::Existing),
            "MIDI 出力ポート:\n  Microsoft GS Wavetable Synth\n  MIDI (機器 F20B738C)\nMIDI 入力ポート:\n  TourBox MIDI Out [1] (機器 F20B738C)\n括弧内は機器の識別子です。設定の output に名前が一致する出力ポートがなければ、output に名前が一致する入力ポートと識別子が同じ出力ポートを使います。\n",
            "識別子のあるポートは名前の後に識別子を表示し、識別子の意味を注記する必要があります。"
        );
    }

    #[test]
    fn render_notes_virtual_port_creation_only_in_virtual_mode() {
        assert!(
            render(&[], &[], PortMode::Virtual).contains("仮想ポートを作成"),
            "仮想ポートを作成する方式では、設定名が一覧になくてよいことを注記する必要があります。"
        );
        assert!(
            !render(&[], &[], PortMode::Existing).contains("仮想ポート"),
            "既存ポートを選ぶ方式では仮想ポートの注記を出さない必要があります。"
        );
    }
}
