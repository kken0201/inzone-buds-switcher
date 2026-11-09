//! 通知の要否と文面を決める（docs/design.md 4.5）。純粋関数。
//! シェルへの表示は `tray` が行う。

use crate::config::Config;
use crate::switch::{Outcome, Skip};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Switch,
    Warning,
    Battery,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub kind: Kind,
    pub text: String,
}

/// 左右のうち、しきい値以下になった側の残量
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LowAlert {
    pub left: Option<u8>,
    pub right: Option<u8>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LowFlags {
    pub left: bool,
    pub right: bool,
}

/// 左右それぞれ、しきい値以下で未通知なら通知してフラグを立て、しきい値 + 10% 以上で解除する。
/// 間の値や不明な値（None）ではフラグを変えない。5% と 6% の間を行き来しても再通知しないため
pub fn battery_alert(
    flags: LowFlags,
    left: Option<u8>,
    right: Option<u8>,
    threshold: u8,
) -> (LowFlags, Option<LowAlert>) {
    let (fl, al) = side(flags.left, left, threshold);
    let (fr, ar) = side(flags.right, right, threshold);
    let alert = (al.is_some() || ar.is_some()).then_some(LowAlert { left: al, right: ar });
    (LowFlags { left: fl, right: fr }, alert)
}

fn side(flagged: bool, level: Option<u8>, threshold: u8) -> (bool, Option<u8>) {
    match level {
        Some(v) if v <= threshold && !flagged => (true, Some(v)),
        Some(v) if v >= threshold.saturating_add(10) => (false, None),
        _ => (flagged, None),
    }
}

/// 警告はオフにできない。期待した切り替えが起きなかった理由が分からなくなるため
pub fn for_outcome(outcome: &Outcome, config: &Config) -> Option<Notice> {
    let (kind, text) = match outcome {
        Outcome::Switched(d) => (Kind::Switch, format!("🎧 {} に切り替えました", d.name)),
        Outcome::Restored(d) => (Kind::Switch, format!("🔈 {}に戻しました", d.name)),
        Outcome::Remembered(d) => (
            Kind::Switch,
            format!("切断時の出力を『{}』に設定しました（メニューで変更できます）", d.name),
        ),
        Outcome::Skipped(Skip::NotConfigured) => (Kind::Warning, "⚠ 切断時の出力が未設定です".into()),
        Outcome::Skipped(Skip::Missing(_)) => (Kind::Warning, "⚠ 戻し先のデバイスが見つかりません".into()),
        Outcome::Skipped(Skip::NoGameDevice) => {
            (Kind::Warning, "⚠ INZONE Buds の出力デバイスが見つかりません".into())
        }
        Outcome::Failed(_) => (Kind::Warning, "⚠ 出力を切り替えられませんでした".into()),
        Outcome::Skipped(Skip::Disabled | Skip::UserChanged) => return None,
    };
    let enabled = match kind {
        Kind::Switch => config.notify_switch,
        Kind::Warning => true,
        Kind::Battery => config.notify_battery,
    };
    enabled.then_some(Notice { kind, text })
}

pub fn for_low_battery(alert: LowAlert, config: &Config) -> Option<Notice> {
    if !config.notify_battery {
        return None;
    }
    let levels: Vec<String> = [("L", alert.left), ("R", alert.right)]
        .into_iter()
        .filter_map(|(name, v)| v.map(|v| format!("{name} {v}%")))
        .collect();
    Some(Notice {
        kind: Kind::Battery,
        text: format!("🔋 イヤホンの残量が少なくなっています（{}）", levels.join(" / ")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DeviceRef;

    fn dev(name: &str) -> DeviceRef {
        DeviceRef { id: "id".into(), name: name.into() }
    }

    fn notice(o: Outcome, c: &Config) -> Option<(Kind, String)> {
        for_outcome(&o, c).map(|n| (n.kind, n.text))
    }

    fn quiet() -> Config {
        Config { notify_switch: false, notify_battery: false, ..Config::default() }
    }

    #[test]
    fn switch_outcomes_follow_notify_switch() {
        let on = Config::default();
        assert_eq!(
            notice(Outcome::Switched(dev("INZONE Buds")), &on),
            Some((Kind::Switch, "🎧 INZONE Buds に切り替えました".into()))
        );
        assert_eq!(
            notice(Outcome::Restored(dev("スピーカー")), &on),
            Some((Kind::Switch, "🔈 スピーカーに戻しました".into()))
        );
        assert_eq!(
            notice(Outcome::Remembered(dev("スピーカー")), &on),
            Some((Kind::Switch, "切断時の出力を『スピーカー』に設定しました（メニューで変更できます）".into()))
        );
        for o in [Outcome::Switched(dev("a")), Outcome::Restored(dev("a")), Outcome::Remembered(dev("a"))] {
            assert_eq!(notice(o, &quiet()), None);
        }
    }

    #[test]
    fn warnings_ignore_notify_switch() {
        let cases = [
            (Outcome::Skipped(Skip::NotConfigured), "⚠ 切断時の出力が未設定です"),
            (Outcome::Skipped(Skip::Missing(dev("usb"))), "⚠ 戻し先のデバイスが見つかりません"),
            (Outcome::Skipped(Skip::NoGameDevice), "⚠ INZONE Buds の出力デバイスが見つかりません"),
            (Outcome::Failed("e".into()), "⚠ 出力を切り替えられませんでした"),
        ];
        for (o, text) in cases {
            assert_eq!(notice(o.clone(), &quiet()), Some((Kind::Warning, text.into())), "{o:?}");
        }
    }

    #[test]
    fn disabled_and_user_changed_are_silent() {
        for s in [Skip::Disabled, Skip::UserChanged] {
            assert_eq!(notice(Outcome::Skipped(s), &Config::default()), None);
        }
    }

    #[test]
    fn low_battery_notice_lists_only_low_sides() {
        let c = Config::default();
        let text = |a| for_low_battery(a, &c).unwrap().text;
        assert_eq!(
            text(LowAlert { left: Some(5), right: Some(4) }),
            "🔋 イヤホンの残量が少なくなっています（L 5% / R 4%）"
        );
        assert_eq!(
            text(LowAlert { left: None, right: Some(3) }),
            "🔋 イヤホンの残量が少なくなっています（R 3%）"
        );
        assert_eq!(for_low_battery(LowAlert { left: Some(5), right: None }, &quiet()), None);
    }

    fn step(f: LowFlags, l: Option<u8>, r: Option<u8>) -> (LowFlags, Option<LowAlert>) {
        battery_alert(f, l, r, 5)
    }

    fn flags(left: bool, right: bool) -> LowFlags {
        LowFlags { left, right }
    }

    #[test]
    fn alerts_once_at_or_below_threshold() {
        let (f, a) = step(flags(false, false), Some(5), Some(80));
        assert_eq!((f, a), (flags(true, false), Some(LowAlert { left: Some(5), right: None })));
        assert_eq!(step(f, Some(4), Some(80)), (flags(true, false), None));
    }

    #[test]
    fn above_threshold_does_not_alert() {
        assert_eq!(step(flags(false, false), Some(6), Some(100)), (flags(false, false), None));
    }

    #[test]
    fn flag_clears_only_at_threshold_plus_ten() {
        let f = flags(true, true);
        assert_eq!(step(f, Some(14), Some(6)), (flags(true, true), None));
        assert_eq!(step(f, Some(15), Some(6)), (flags(false, true), None));
        assert_eq!(step(f, Some(15), Some(15)), (flags(false, false), None));
    }

    #[test]
    fn alerts_again_after_recovery() {
        let (f, _) = step(flags(true, false), Some(20), None);
        assert_eq!(step(f, Some(5), None).1, Some(LowAlert { left: Some(5), right: None }));
    }

    #[test]
    fn both_sides_in_one_report_make_one_alert() {
        assert_eq!(
            step(flags(false, false), Some(5), Some(4)),
            (flags(true, true), Some(LowAlert { left: Some(5), right: Some(4) }))
        );
    }

    #[test]
    fn second_side_alerts_alone_when_first_is_flagged() {
        assert_eq!(
            step(flags(true, false), Some(4), Some(5)),
            (flags(true, true), Some(LowAlert { left: None, right: Some(5) }))
        );
    }

    #[test]
    fn unknown_levels_keep_flags() {
        assert_eq!(step(flags(true, false), None, None), (flags(true, false), None));
        assert_eq!(step(flags(false, true), None, Some(1)), (flags(false, true), None));
    }

    #[test]
    fn threshold_near_max_does_not_overflow() {
        assert_eq!(battery_alert(flags(true, false), Some(100), None, 250), (flags(true, false), None));
    }
}
