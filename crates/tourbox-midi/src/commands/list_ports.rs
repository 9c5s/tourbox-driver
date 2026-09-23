//! `list-ports` サブコマンド。MIDI の入出力ポートの名前を一覧表示する。

use crate::midi::{self, PortMode};

/// 出力ポートと入力ポートの名前を標準出力に表示する。
pub fn run() -> anyhow::Result<()> {
    let outputs = midi::list_output_names()?;
    let inputs = midi::list_input_names()?;
    print!("{}", render(&outputs, &inputs, PortMode::default_for_os()));
    Ok(())
}

/// 一覧の表示内容を作る。
fn render(outputs: &[String], inputs: &[String], mode: PortMode) -> String {
    let mut text = String::new();
    push_section(&mut text, "MIDI 出力ポート:", outputs);
    push_section(&mut text, "MIDI 入力ポート:", inputs);
    if mode == PortMode::Virtual {
        text.push_str("この OS では設定の output と input の名前で仮想ポートを作成するので、設定する名前はこの一覧にある必要はありません。\n");
    }
    text
}

/// 見出しと名前の一覧を 1 項目 1 行で追加する。0 件なら (なし) と書く。
fn push_section(text: &mut String, heading: &str, names: &[String]) {
    text.push_str(heading);
    text.push('\n');
    if names.is_empty() {
        text.push_str("  (なし)\n");
    }
    for name in names {
        text.push_str("  ");
        text.push_str(name);
        text.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_lists_names_under_headings_and_marks_empty_list() {
        let outputs = ["loopMIDI Port", "Microsoft GS Wavetable Synth"].map(String::from);

        assert_eq!(
            render(&outputs, &[], PortMode::Existing),
            "MIDI 出力ポート:\n  loopMIDI Port\n  Microsoft GS Wavetable Synth\nMIDI 入力ポート:\n  (なし)\n",
            "見出しの下に名前を一覧の順に並べ、0 件なら (なし) と表示する必要があります。"
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
