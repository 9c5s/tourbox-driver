//! 受信した Control Change によるハプティクス設定の切り替え (設計書 6.2 節)。

use std::collections::HashMap;

use tourbox::protocol::{Axis, Button, HapticConfig, Modifier, Speed, Strength};

use crate::config::HapticsControlConfig;

/// master の値のうち、全軸をなしにする値の上限。これより大きい値で設定値に戻す。
const MASTER_OFF_MAX: u8 = 63;

/// `[haptics.control]` の割り当てに従い、受信した CC からデバイスに送るハプティクス設定を作る。
///
/// 受信した変更は基準の設定への上書きとして持ち、設定ファイルには書き戻さない。
pub struct HapticsController {
    control: HapticsControlConfig,
    base: HapticConfig,
    /// 受信した CC による、組み合わせ (軸、修飾) ごとの強度の上書き。
    strengths: HashMap<(Axis, Modifier), Strength>,
    /// 受信した CC による、組み合わせ (軸、修飾) ごとの速度の上書き。
    speeds: HashMap<(Axis, Modifier), Speed>,
    /// master でなしにしている間は true。
    muted: bool,
}

impl HapticsController {
    pub fn new(control: HapticsControlConfig, base: HapticConfig) -> Self {
        Self {
            control,
            base,
            strengths: HashMap::new(),
            speeds: HashMap::new(),
            muted: false,
        }
    }

    /// 受信した CC を反映し、送る設定が変わったら新しい設定を返す。`channel` は 0 起点である。
    ///
    /// 受信しないチャンネル、割り当てのない CC、送る設定が変わらない CC では None を返す。
    pub fn on_cc(&mut self, channel: u8, cc: u8, value: u8) -> Option<HapticConfig> {
        if self
            .control
            .channel
            .is_some_and(|accepted| accepted != channel)
        {
            return None;
        }
        let target = self.target(cc)?;
        let before = self.output();
        match target {
            Target::Master => self.muted = value <= MASTER_OFF_MAX,
            Target::Strength(scope) => {
                let strength = strength_of(value);
                for combination in self.combinations(scope) {
                    self.strengths.insert(combination, strength);
                }
            }
            Target::Speed(scope) => {
                let speed = speed_of(value);
                for combination in self.combinations(scope) {
                    self.speeds.insert(combination, speed);
                }
            }
        }
        let after = self.output();
        (after != before).then_some(after)
    }

    /// 割り当てと基準の設定を差し替え、受信した変更と master の状態を捨てて、基準の設定を返す。
    pub fn reset(&mut self, control: HapticsControlConfig, base: HapticConfig) -> HapticConfig {
        *self = Self::new(control, base);
        self.base.clone()
    }

    /// CC 番号の割り当て先。検証で CC 番号の重複を除いてあるので、割り当て先は高々 1 つである。
    fn target(&self, cc: u8) -> Option<Target> {
        if self.control.master == Some(cc) {
            return Some(Target::Master);
        }
        let axes = self
            .control
            .axes
            .iter()
            .map(|(&axis, control)| (Scope::Axis(axis), control));
        let combinations = self
            .control
            .with
            .iter()
            .map(|(&(button, axis), control)| (Scope::Combination(button, axis), control));
        axes.chain(combinations).find_map(|(scope, control)| {
            if control.cc == Some(cc) {
                Some(Target::Strength(scope))
            } else if control.speed_cc == Some(cc) {
                Some(Target::Speed(scope))
            } else {
                None
            }
        })
    }

    /// 変更を適用する組み合わせ (軸、修飾)。
    fn combinations(&self, scope: Scope) -> Vec<(Axis, Modifier)> {
        match scope {
            Scope::Axis(axis) => Modifier::ALL
                .into_iter()
                .filter(|&modifier| match modifier {
                    Modifier::None => true,
                    Modifier::Button(button) => !self.control.with.contains_key(&(button, axis)),
                })
                .map(|modifier| (axis, modifier))
                .collect(),
            Scope::Combination(button, axis) => vec![(axis, Modifier::Button(button))],
        }
    }

    /// 送る設定。master でなしにしている間は、全組み合わせの強度も速度も 0 にする。
    fn output(&self) -> HapticConfig {
        let mut config = self.base.clone();
        if self.muted {
            for axis in Axis::ALL {
                config.set_axis(axis, Strength::Off, Speed::Fast);
            }
            return config;
        }
        for (&(axis, modifier), &strength) in &self.strengths {
            config.set_strength(axis, modifier, strength);
        }
        for (&(axis, modifier), &speed) in &self.speeds {
            config.set_speed(axis, modifier, speed);
        }
        config
    }
}

/// CC 番号の割り当て先。
#[derive(Debug, Clone, Copy)]
enum Target {
    Master,
    Strength(Scope),
    Speed(Scope),
}

/// 変更を適用する範囲。
#[derive(Debug, Clone, Copy)]
enum Scope {
    /// 軸の全組み合わせ。同じ軸の個別制御がある組み合わせを除く。
    Axis(Axis),
    /// 修飾と軸の組み合わせ 1 つ。
    Combination(Button, Axis),
}

/// `cc` の値に対応する強度 (設計書 6.2 節)。
fn strength_of(value: u8) -> Strength {
    match value {
        0 => Strength::Off,
        1..=63 => Strength::Weak,
        _ => Strength::Strong,
    }
}

/// `speed_cc` の値に対応する速度 (設計書 6.2 節)。
fn speed_of(value: u8) -> Speed {
    match value {
        0..=42 => Speed::Fast,
        43..=85 => Speed::Medium,
        _ => Speed::Slow,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::config::Config;

    const SIDE: Modifier = Modifier::Button(Button::Side);
    const TALL: Modifier = Modifier::Button(Button::Tall);

    /// `[haptics.control]` の本文 `body` から割り当てを作る。
    fn control(body: &str) -> HapticsControlConfig {
        let text = format!(
            "[midi]\noutput = \"TourBox MIDI\"\nchannel = 1\n[map]\n[haptics.control]\n{body}"
        );
        Config::parse(&text, Path::new("config.toml"))
            .unwrap_or_else(|error| panic!("検証に通る必要があります: {error}"))
            .haptics_control()
    }

    /// 基準の設定。全組み合わせの「強、中速」から、Knob と Side、Scroll 単体の値を変えてある。
    fn base() -> HapticConfig {
        let mut config = HapticConfig::default();
        config.set(Axis::Knob, SIDE, Strength::Weak, Speed::Slow);
        config.set(Axis::Scroll, Modifier::None, Strength::Off, Speed::Fast);
        config
    }

    /// 全組み合わせの強度も速度も 0 にした設定。
    fn muted() -> HapticConfig {
        let mut config = HapticConfig::default();
        for axis in Axis::ALL {
            for modifier in Modifier::ALL {
                config.set(axis, modifier, Strength::Off, Speed::Fast);
            }
        }
        config
    }

    /// `config` の `axis` の全組み合わせのうち、`excluded` 以外の強度を `strength` にする。
    fn with_strength(
        mut config: HapticConfig,
        axis: Axis,
        excluded: &[Modifier],
        strength: Strength,
    ) -> HapticConfig {
        for modifier in Modifier::ALL {
            if !excluded.contains(&modifier) {
                config.set_strength(axis, modifier, strength);
            }
        }
        config
    }

    /// `config` の `axis` の全組み合わせのうち、`excluded` 以外の速度を `speed` にする。
    fn with_speed(
        mut config: HapticConfig,
        axis: Axis,
        excluded: &[Modifier],
        speed: Speed,
    ) -> HapticConfig {
        for modifier in Modifier::ALL {
            if !excluded.contains(&modifier) {
                config.set_speed(axis, modifier, speed);
            }
        }
        config
    }

    #[test]
    fn configured_channel_accepts_only_that_channel() {
        let mut controller =
            HapticsController::new(control("channel = 16\nknob = { cc = 100 }\n"), base());

        assert_eq!(
            controller.on_cc(0, 100, 0),
            None,
            "channel と異なるチャンネルの CC は無視する必要があります。"
        );
        assert_eq!(
            controller.on_cc(15, 100, 0),
            Some(with_strength(base(), Axis::Knob, &[], Strength::Off)),
            "channel = 16 (0 起点で 15) の CC は反映する必要があります。"
        );
    }

    #[test]
    fn omitted_channel_accepts_every_channel() {
        for channel in 0..16 {
            let mut controller = HapticsController::new(control("knob = { cc = 100 }\n"), base());

            assert_eq!(
                controller.on_cc(channel, 100, 0),
                Some(with_strength(base(), Axis::Knob, &[], Strength::Off)),
                "channel の省略時は、0 起点で {channel} のチャンネルの CC も反映する必要があります。"
            );
        }
    }

    #[test]
    fn axis_cc_value_selects_strength_of_axis() {
        // 値と強度。各段で前段と強度が変わる順に並べる
        let steps = [
            (0, Strength::Off),
            (1, Strength::Weak),
            (64, Strength::Strong),
            (63, Strength::Weak),
            (127, Strength::Strong),
        ];
        for (axis, cc) in [(Axis::Knob, 100), (Axis::Scroll, 101), (Axis::Dial, 102)] {
            let mut controller = HapticsController::new(
                control("knob = { cc = 100 }\nscroll = { cc = 101 }\ndial = { cc = 102 }\n"),
                base(),
            );
            for (value, strength) in steps {
                assert_eq!(
                    controller.on_cc(0, cc, value),
                    Some(with_strength(base(), axis, &[], strength)),
                    "{axis:?} の cc の値 {value} は、速度を保ったまま全組み合わせの強度を {strength:?} にする必要があります。"
                );
            }
        }
    }

    #[test]
    fn axis_speed_cc_value_selects_speed_of_axis() {
        // 値と速度。各段で前段と速度が変わる順に並べる
        let steps = [
            (0, Speed::Fast),
            (43, Speed::Medium),
            (86, Speed::Slow),
            (42, Speed::Fast),
            (85, Speed::Medium),
            (127, Speed::Slow),
        ];
        for (axis, speed_cc) in [(Axis::Knob, 110), (Axis::Scroll, 111), (Axis::Dial, 112)] {
            let mut controller = HapticsController::new(
                control(
                    "knob = { speed_cc = 110 }\nscroll = { speed_cc = 111 }\ndial = { speed_cc = 112 }\n",
                ),
                base(),
            );
            for (value, speed) in steps {
                assert_eq!(
                    controller.on_cc(0, speed_cc, value),
                    Some(with_speed(base(), axis, &[], speed)),
                    "{axis:?} の speed_cc の値 {value} は、強度を保ったまま全組み合わせの速度を {speed:?} にする必要があります。"
                );
            }
        }
    }

    #[test]
    fn axis_change_skips_combinations_with_individual_control_of_same_axis() {
        let mut controller = HapticsController::new(
            control(concat!(
                "knob = { cc = 100, speed_cc = 101 }\n",
                "[haptics.control.with.side]\n",
                "knob = { cc = 110 }\n",
                "[haptics.control.with.tall]\n",
                "knob = {}\n",
                "[haptics.control.with.top]\n",
                "scroll = { cc = 120 }\n",
            )),
            base(),
        );
        // Top は Scroll だけを個別に制御するので、Knob と Top の組み合わせは軸の制御に含まれる
        let excluded = [SIDE, TALL];

        let strength_changed = with_strength(base(), Axis::Knob, &excluded, Strength::Off);
        assert_eq!(
            controller.on_cc(0, 100, 0),
            Some(strength_changed.clone()),
            "軸の cc は、同じ軸の個別制御がある組み合わせを除く全組み合わせの強度を変える必要があります。"
        );
        assert_eq!(
            controller.on_cc(0, 101, 127),
            Some(with_speed(strength_changed, Axis::Knob, &excluded, Speed::Slow)),
            "軸の speed_cc は、同じ軸の個別制御がある組み合わせを除く全組み合わせの速度を変える必要があります。"
        );
    }

    #[test]
    fn individual_control_changes_only_its_combination() {
        let mut controller = HapticsController::new(
            control(concat!(
                "knob = { cc = 100 }\n",
                "[haptics.control.with.side]\n",
                "knob = { cc = 110, speed_cc = 111 }\n",
            )),
            base(),
        );

        let mut expected = base();
        expected.set_strength(Axis::Knob, SIDE, Strength::Off);
        assert_eq!(
            controller.on_cc(0, 110, 0),
            Some(expected.clone()),
            "個別制御の cc は、その組み合わせの強度だけを変える必要があります。"
        );

        expected.set_speed(Axis::Knob, SIDE, Speed::Fast);
        assert_eq!(
            controller.on_cc(0, 111, 0),
            Some(expected.clone()),
            "個別制御の speed_cc は、その組み合わせの速度だけを変える必要があります。"
        );

        assert_eq!(
            controller.on_cc(0, 100, 1),
            Some(with_strength(expected, Axis::Knob, &[SIDE], Strength::Weak)),
            "軸の cc は、個別制御した組み合わせの値を変えない必要があります。"
        );
    }

    #[test]
    fn master_off_sends_zero_and_keeps_changes_until_master_returns() {
        let mut controller = HapticsController::new(
            control(concat!(
                "master = { cc = 99 }\n",
                "knob = { cc = 100 }\n",
                "[haptics.control.with.side]\n",
                "scroll = { speed_cc = 111 }\n",
            )),
            base(),
        );

        assert_eq!(
            controller.on_cc(0, 99, 63),
            Some(muted()),
            "master の値 63 は、全組み合わせの強度も速度も 0 にした設定を返す必要があります。"
        );
        assert_eq!(
            controller.on_cc(0, 99, 0),
            None,
            "なしの間に master の値 0 を受けても、送る設定は変わらない必要があります。"
        );
        assert_eq!(
            controller.on_cc(0, 100, 1),
            None,
            "なしの間の軸の変更は、送る設定を変えない必要があります。"
        );
        assert_eq!(
            controller.on_cc(0, 111, 127),
            None,
            "なしの間の組み合わせの変更は、送る設定を変えない必要があります。"
        );

        let mut restored = with_strength(base(), Axis::Knob, &[], Strength::Weak);
        restored.set_speed(Axis::Scroll, SIDE, Speed::Slow);
        assert_eq!(
            controller.on_cc(0, 99, 64),
            Some(restored),
            "master の値 64 は、なしの間に受けた変更を反映した設定を返す必要があります。"
        );
        assert_eq!(
            controller.on_cc(0, 99, 127),
            None,
            "設定値に戻した後の master の値 127 は、送る設定を変えない必要があります。"
        );
    }

    #[test]
    fn unassigned_or_unchanging_cc_returns_none() {
        let mut controller =
            HapticsController::new(control("knob = { cc = 100 }\n"), HapticConfig::default());

        assert_eq!(
            controller.on_cc(0, 101, 0),
            None,
            "割り当てのない CC は無視する必要があります。"
        );
        assert_eq!(
            controller.on_cc(0, 100, 127),
            None,
            "基準と同じ強度にする CC では、送る設定が変わらないので None を返す必要があります。"
        );
        assert!(
            controller.on_cc(0, 100, 0).is_some(),
            "強度を変える CC では新しい設定を返す必要があります。"
        );
        assert_eq!(
            controller.on_cc(0, 100, 0),
            None,
            "直前と同じ値の CC では、送る設定が変わらないので None を返す必要があります。"
        );
    }

    #[test]
    fn reset_replaces_assignments_and_base_and_discards_received_changes() {
        let mut controller = HapticsController::new(
            control("master = { cc = 99 }\nknob = { cc = 100 }\n"),
            base(),
        );
        controller.on_cc(0, 100, 0);
        controller.on_cc(0, 99, 0);
        let mut new_base = HapticConfig::default();
        new_base.set(Axis::Dial, Modifier::None, Strength::Weak, Speed::Fast);

        assert_eq!(
            controller.reset(control("scroll = { cc = 100 }\n"), new_base.clone()),
            new_base,
            "reset は新しい基準の設定をそのまま返す必要があります。"
        );
        assert_eq!(
            controller.on_cc(0, 99, 127),
            None,
            "reset の後は、古い割り当ての master の CC を無視する必要があります。"
        );
        assert_eq!(
            controller.on_cc(0, 100, 0),
            Some(with_strength(new_base, Axis::Scroll, &[], Strength::Off)),
            "reset の後は、新しい割り当てと基準を使い、master の状態と Knob の上書きを捨てる必要があります。"
        );
    }
}
