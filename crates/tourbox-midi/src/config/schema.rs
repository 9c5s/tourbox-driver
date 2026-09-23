//! 検証済みの設定の型。
//!
//! チャンネルはすべて 0 起点 (設定ファイルの 1〜16 を 0〜15 にした値) で持つ。
//! MIDI のステータスバイトの下位 4 ビットにそのまま使える。

use std::collections::HashMap;

use tourbox::protocol::{Axis, Button, Speed, Strength};
use tourbox::transport::ConnectionConfig;

/// 検証済みの設定ファイル全体。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// `[device]`。省略時は auto でポートの明示指定なし。
    pub device: ConnectionConfig,
    /// `[midi]`。
    pub midi: MidiSection,
    /// `[haptics]`。`[haptics.control]` を含む。
    pub haptics: HapticsSection,
    /// `[map]`。
    pub map: MapSection,
}

/// `[midi]` セクション。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidiSection {
    /// 出力ポートの名前。
    pub output: String,
    /// ハプティクス制御を受信する入力ポートの名前。None は入力機能を使わない。
    pub input: Option<String>,
    /// 既定のチャンネル (0 起点)。
    pub channel: u8,
}

/// `[haptics]` セクション。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HapticsSection {
    /// 軸ごとの強度と速度。設定ファイルに書かれていない軸は含まない。
    pub axes: HashMap<Axis, HapticSetting>,
    /// `[haptics.with.<ボタン>]` による、修飾と軸の組み合わせの上書き。
    pub with: HashMap<(Button, Axis), HapticOverride>,
    /// `[haptics.control]`。
    pub control: HapticsControlConfig,
}

/// 軸の強度と速度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HapticSetting {
    pub strength: Strength,
    pub speed: Speed,
}

impl Default for HapticSetting {
    /// 設定ファイルで省略した場合の値 (strong、medium)。
    fn default() -> Self {
        Self {
            strength: Strength::Strong,
            speed: Speed::Medium,
        }
    }
}

/// 修飾と軸の組み合わせの上書き。None の値は軸の値を継承する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HapticOverride {
    pub strength: Option<Strength>,
    pub speed: Option<Speed>,
}

/// `[haptics.control]` による、受信した CC でのハプティクス制御の割り当て (設計書 6.2 節)。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HapticsControlConfig {
    /// 受信するチャンネル (0 起点)。None は全チャンネル。
    pub channel: Option<u8>,
    /// 全軸のハプティクスをなしと設定値の間で切り替える CC 番号。
    pub master: Option<u8>,
    /// 軸の全組み合わせを制御する CC。
    pub axes: HashMap<Axis, ControlCc>,
    /// `[haptics.control.with.<ボタン>]` による、修飾と軸の組み合わせの個別制御。
    /// ここにある組み合わせは `axes` の制御から除外する。
    pub with: HashMap<(Button, Axis), ControlCc>,
}

/// 強度と速度を制御する CC 番号の組。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ControlCc {
    /// 強度を制御する CC 番号。
    pub cc: Option<u8>,
    /// 速度を制御する CC 番号。
    pub speed_cc: Option<u8>,
}

/// `[map]` セクション。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MapSection {
    /// 修飾なしのレイヤ (基本レイヤ)。
    pub base: MapLayer,
    /// `[map.with.<ボタン>]` のレイヤ。キーのボタンが修飾ボタンになる。
    pub with: HashMap<Button, MapLayer>,
}

/// 1 つのレイヤの割り当て。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MapLayer {
    pub buttons: HashMap<Button, ButtonEntry>,
    pub rotations: HashMap<Axis, RotationEntry>,
}

/// ボタンの項目。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ButtonEntry {
    /// チャンネルの上書き (0 起点)。None は `midi.channel` を使う。
    pub channel: Option<u8>,
    pub kind: ButtonKind,
}

/// ボタンが送るメッセージ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    /// 押下で Note On、解放で Note Off を送る。
    Note { note: u8, velocity: u8 },
    /// 押下で値 127、解放で値 0 の CC を送る。
    Cc { cc: u8 },
}

/// 回転の項目。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RotationEntry {
    /// チャンネルの上書き (0 起点)。None は `midi.channel` を使う。
    pub channel: Option<u8>,
    pub cc: u8,
    /// 回転方向を反転する。
    pub invert: bool,
    pub mode: RotationMode,
}

/// 回転を CC の値にする方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationMode {
    /// 内部値 (0〜127) を 1 ノッチあたり `step` ずつ増減して送る。内部値は `initial` から始まる。
    Absolute { step: u8, initial: u8 },
    /// 1 ノッチごとに ±`step` を `encoding` で符号化して送る。
    Relative {
        step: u8,
        encoding: RelativeEncoding,
    },
}

/// 相対 CC の符号化。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelativeEncoding {
    /// 7 ビットの 2 の補数。+1 は 1、-1 は 127。
    TwosComplement,
    /// 64 を 0 とする。+1 は 65、-1 は 63。
    BinaryOffset,
}
