//! 初期化で送るアンロックと、アンロック前の書き込みに対する応答文字列の検出。

/// 初期化の最初に送るアンロックのメッセージ。
pub const UNLOCK: [u8; 8] = [0x55, 0x00, 0x07, 0x88, 0x94, 0x00, 0x1a, 0xfe];

/// アンロック前に書き込むとデバイスが返す ASCII 文字列。
pub const NOT_ALLOW_CONFIG: &[u8] = b"<!not_allow_config!>";

/// `NOT_ALLOW_CONFIG` を受信のかたまりをまたいで逐次照合する検出器。
#[derive(Debug, Default)]
pub struct NotAllowConfigDetector {
    /// 文字列の先頭から一致しているバイト数。
    matched: usize,
}

impl NotAllowConfigDetector {
    /// 与えたかたまりの中で文字列が完成したら true を返す。
    pub fn feed(&mut self, chunk: &[u8]) -> bool {
        let mut detected = false;
        for &byte in chunk {
            self.matched = if byte == NOT_ALLOW_CONFIG[self.matched] {
                self.matched + 1
            } else if byte == NOT_ALLOW_CONFIG[0] {
                // `<` は文字列の先頭にしか現れないので、不一致の後は先頭からの照合だけを考えればよい
                1
            } else {
                0
            };
            if self.matched == NOT_ALLOW_CONFIG.len() {
                detected = true;
                self.matched = 0;
            }
        }
        detected
    }

    /// 一致状態を捨てる。
    pub fn reset(&mut self) {
        self.matched = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 設計書 2.2 節の応答文字列。
    const TEXT: &[u8] = b"<!not_allow_config!>";

    #[test]
    fn unlock_is_bytes_in_spec() {
        assert_eq!(
            UNLOCK,
            [0x55, 0x00, 0x07, 0x88, 0x94, 0x00, 0x1a, 0xfe],
            "UNLOCK は設計書 2.2 節の 8 バイトである必要があります。"
        );
    }

    #[test]
    fn not_allow_config_is_20_byte_ascii_text_in_spec() {
        assert_eq!(
            NOT_ALLOW_CONFIG, TEXT,
            "NOT_ALLOW_CONFIG は ASCII 文字列 <!not_allow_config!> である必要があります。"
        );
        assert_eq!(
            NOT_ALLOW_CONFIG.len(),
            20,
            "NOT_ALLOW_CONFIG は 20 バイトである必要があります。"
        );
    }

    #[test]
    fn detects_text_fed_in_one_chunk() {
        let mut detector = NotAllowConfigDetector::default();
        assert!(
            detector.feed(TEXT),
            "1 つのかたまりで届いた文字列を検出する必要があります。"
        );
    }

    #[test]
    fn detects_text_split_into_two_chunks_at_every_position() {
        // 20 バイトを 2 つのかたまりに分ける 19 通り
        for split in 1..TEXT.len() {
            let (head, tail) = TEXT.split_at(split);
            let mut detector = NotAllowConfigDetector::default();
            assert!(
                !detector.feed(head),
                "先頭 {split} バイトだけでは検出しない必要があります。"
            );
            assert!(
                detector.feed(tail),
                "{split} バイト目の後で分かれて届いた文字列を検出する必要があります。"
            );
        }
    }

    #[test]
    fn detects_text_fed_one_byte_at_a_time() {
        let mut detector = NotAllowConfigDetector::default();
        let (last, init) = TEXT.split_last().expect("TEXT は空でない必要があります。");
        for (index, &byte) in init.iter().enumerate() {
            assert!(
                !detector.feed(&[byte]),
                "{index} バイト目までの到着では検出しない必要があります。"
            );
        }
        assert!(
            detector.feed(&[*last]),
            "最後のバイトの到着で検出する必要があります。"
        );
    }

    #[test]
    fn detects_text_surrounded_by_unrelated_bytes() {
        // イベントのバイトと同じかたまりで届く場合
        let chunk = [&[0x00, 0x44, 0x80], TEXT, &[0x09, 0x3c]].concat();
        let mut detector = NotAllowConfigDetector::default();
        assert!(
            detector.feed(&chunk),
            "前後に無関係なバイトが付いた文字列を検出する必要があります。"
        );

        // 無関係なバイトが付いたうえで、かたまりの境界で分かれて届く場合
        let (head, tail) = TEXT.split_at(8);
        let mut detector = NotAllowConfigDetector::default();
        assert!(
            !detector.feed(&[&[0x00, 0x44], head].concat()),
            "文字列の途中までを含むかたまりでは検出しない必要があります。"
        );
        assert!(
            detector.feed(&[tail, &[0x09, 0x3c]].concat()),
            "かたまりをまたいで完成した文字列を、後ろに無関係なバイトがあっても検出する必要があります。"
        );
    }

    #[test]
    fn detects_text_starting_right_after_partial_match() {
        // 途中まで一致した後、不一致のバイトが文字列の先頭 `<` である場合
        let cases: [&[u8]; 2] = [b"<!not_allow_con", b"<"];
        for partial in cases {
            let chunk = [partial, TEXT].concat();
            let mut detector = NotAllowConfigDetector::default();
            assert!(
                detector.feed(&chunk),
                "{:?} の直後から始まる文字列を検出する必要があります。",
                String::from_utf8_lossy(partial)
            );
        }
    }

    #[test]
    fn does_not_detect_text_interrupted_by_other_byte() {
        let mut detector = NotAllowConfigDetector::default();
        assert!(
            !detector.feed(b"<!not_allow_conXfig!>"),
            "途中に別のバイトが挟まった文字列は検出しない必要があります。"
        );
        assert!(
            detector.feed(TEXT),
            "その後に届いた完全な文字列は検出する必要があります。"
        );
    }

    #[test]
    fn does_not_carry_match_over_after_detection() {
        let mut detector = NotAllowConfigDetector::default();
        assert!(
            detector.feed(TEXT),
            "完全な文字列を検出する必要があります。"
        );
        assert!(
            !detector.feed(&TEXT[1..]),
            "検出の後は、先頭から揃わない文字列を検出しない必要があります。"
        );
    }

    #[test]
    fn reset_discards_partial_match() {
        let (head, tail) = TEXT.split_at(10);
        let mut detector = NotAllowConfigDetector::default();
        assert!(
            !detector.feed(head),
            "文字列の前半だけでは検出しない必要があります。"
        );
        detector.reset();
        assert!(
            !detector.feed(tail),
            "reset の後に届いた後半だけでは検出しない必要があります。"
        );
        assert!(
            detector.feed(TEXT),
            "reset の後も、完全な文字列は検出する必要があります。"
        );
    }
}
