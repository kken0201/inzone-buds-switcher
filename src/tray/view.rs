//! トレイのツールチップとメニューの中身（docs/design.md 4.4）。純粋関数。
//! Win32 の描画は `tray` が行い、ここは何を出すかだけを決める。

use crate::audio::Endpoint;
use crate::config::{Config, DeviceRef};
use crate::hid::protocol::Battery;
use crate::state::Link;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    NoDongle,
    /// ドングルはあるが、イヤホンの状態がまだ分からない
    Checking,
    Disconnected,
    Connected,
}

impl Status {
    pub fn of(dongle: bool, link: Link) -> Self {
        match (dongle, link) {
            (false, _) => Self::NoDongle,
            (true, Link::Unknown) => Self::Checking,
            (true, Link::Disconnected) => Self::Disconnected,
            (true, Link::Connected) => Self::Connected,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::NoDongle => "ドングル未検出",
            Self::Checking => "確認中",
            Self::Disconnected => "切断中",
            Self::Connected => "接続中",
        }
    }
}

/// 選んだときに設定へ書く値。トグルも反転後の値を持たせ、適用を冪等にする
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Setting {
    AutoSwitch(bool),
    ConnectedDevice(DeviceRef),
    DisconnectedDevice(DeviceRef),
    NotifySwitch(bool),
    NotifyBattery(bool),
    AutoUpdate(bool),
}

impl Setting {
    pub fn apply(self, config: &mut Config) {
        match self {
            Self::AutoSwitch(v) => config.auto_switch = v,
            Self::ConnectedDevice(d) => config.connected_device = Some(d),
            Self::DisconnectedDevice(d) => config.disconnected_device = Some(d),
            Self::NotifySwitch(v) => config.notify_switch = v,
            Self::NotifyBattery(v) => config.notify_battery = v,
            Self::AutoUpdate(v) => config.auto_update = v,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Set(Setting),
    /// 設定ファイルではなくレジストリの Run キーに書くため、`Setting` とは別。反転後の値を持つ
    Autostart(bool),
    OpenLogs,
    Quit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Separator,
    /// `command` が None なら選べない（グレー表示）
    Entry {
        text: String,
        checked: bool,
        command: Option<Command>,
    },
    Submenu {
        text: String,
        items: Vec<Item>,
    },
}

const NO_BATTERY: Battery = Battery { left: None, right: None, case: None };

pub fn pct(v: Option<u8>) -> String {
    v.map_or("--".into(), |v| format!("{v}%"))
}

pub fn tooltip(battery: Option<Battery>) -> String {
    let b = battery.unwrap_or(NO_BATTERY);
    format!("INZONE Buds  L {} / R {} / C {}", pct(b.left), pct(b.right), pct(b.case))
}

fn label(text: impl Into<String>) -> Item {
    Item::Entry { text: text.into(), checked: false, command: None }
}

fn entry(text: impl Into<String>, checked: bool, command: Command) -> Item {
    Item::Entry { text: text.into(), checked, command: Some(command) }
}

fn device_ref(e: &Endpoint) -> DeviceRef {
    DeviceRef { id: e.id.clone(), name: e.name.clone() }
}

/// `checked` の ID が一覧にないときは、設定されていることが分かるよう選べない項目で示す
fn device_list(
    candidates: &[&Endpoint],
    checked: Option<&DeviceRef>,
    command: fn(DeviceRef) -> Setting,
) -> Vec<Item> {
    let mut items: Vec<Item> = candidates
        .iter()
        .map(|e| {
            let on = checked.is_some_and(|c| c.id == e.id);
            entry(e.name.clone(), on, Command::Set(command(device_ref(e))))
        })
        .collect();
    if let Some(c) = checked.filter(|c| !candidates.iter().any(|e| e.id == c.id)) {
        items.push(Item::Entry {
            text: format!("{}（見つかりません）", c.name),
            checked: true,
            command: None,
        });
    }
    if items.is_empty() {
        items.push(label("（再生デバイスがありません）"));
    }
    items
}

pub fn menu(
    config: &Config,
    endpoints: &[Endpoint],
    current: Option<&str>,
    status: Status,
    battery: Option<Battery>,
    autostart: bool,
) -> Vec<Item> {
    let b = battery.unwrap_or(NO_BATTERY);
    let all: Vec<&Endpoint> = endpoints.iter().collect();
    // 未設定のときに接続時に使われるのは Game なので、それに印を付ける
    let game = endpoints.iter().find(|e| e.kind == Some(crate::audio::DongleKind::Game)).map(device_ref);
    let connected = config.connected_device.clone().or(game);
    // ドングルのデバイスは切断時の出力に選べない（docs/design.md 4.3）
    let non_dongle: Vec<&Endpoint> = endpoints.iter().filter(|e| e.kind.is_none()).collect();
    let use_current = current
        .and_then(|id| non_dongle.iter().find(|e| e.id == id))
        .map(|e| Command::Set(Setting::DisconnectedDevice(device_ref(e))));

    vec![
        label(format!("INZONE Buds: {}", status.label())),
        label(format!("L {} / R {} / ケース {}", pct(b.left), pct(b.right), pct(b.case))),
        Item::Separator,
        entry("自動切り替え", config.auto_switch, Command::Set(Setting::AutoSwitch(!config.auto_switch))),
        Item::Submenu {
            text: "接続時の出力".into(),
            items: device_list(&all, connected.as_ref(), Setting::ConnectedDevice),
        },
        Item::Submenu {
            text: "切断時の出力".into(),
            items: device_list(&non_dongle, config.disconnected_device.as_ref(), Setting::DisconnectedDevice),
        },
        Item::Entry {
            text: "今の出力を切断時の出力にする".into(), checked: false, command: use_current
        },
        entry(
            "切り替えを通知",
            config.notify_switch,
            Command::Set(Setting::NotifySwitch(!config.notify_switch)),
        ),
        entry(
            "バッテリー低下を通知",
            config.notify_battery,
            Command::Set(Setting::NotifyBattery(!config.notify_battery)),
        ),
        entry("Windows 起動時に開始", autostart, Command::Autostart(!autostart)),
        entry("自動で更新する", config.auto_update, Command::Set(Setting::AutoUpdate(!config.auto_update))),
        Item::Separator,
        label(format!("バージョン {}", env!("CARGO_PKG_VERSION"))),
        entry("ログフォルダを開く", false, Command::OpenLogs),
        entry("終了", false, Command::Quit),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::DongleKind;

    fn ep(id: &str, kind: Option<DongleKind>) -> Endpoint {
        Endpoint { id: id.into(), name: format!("{id} name"), kind }
    }

    fn dev(id: &str) -> DeviceRef {
        DeviceRef { id: id.into(), name: format!("{id} name") }
    }

    fn endpoints() -> Vec<Endpoint> {
        vec![ep("speaker", None), ep("chat", Some(DongleKind::Chat)), ep("game", Some(DongleKind::Game))]
    }

    fn submenu<'a>(items: &'a [Item], name: &str) -> &'a [Item] {
        items
            .iter()
            .find_map(|i| match i {
                Item::Submenu { text, items } if text == name => Some(items.as_slice()),
                _ => None,
            })
            .unwrap()
    }

    fn find<'a>(items: &'a [Item], name: &str) -> &'a Item {
        items.iter().find(|i| matches!(i, Item::Entry { text, .. } if text == name)).unwrap()
    }

    /// (表示名, チェック, 選べるか)
    fn summary(items: &[Item]) -> Vec<(String, bool, bool)> {
        items
            .iter()
            .map(|i| match i {
                Item::Entry { text, checked, command } => (text.clone(), *checked, command.is_some()),
                _ => panic!("{i:?}"),
            })
            .collect()
    }

    fn command(item: &Item) -> Option<Command> {
        match item {
            Item::Entry { command, .. } => command.clone(),
            _ => panic!("{item:?}"),
        }
    }

    #[test]
    fn tooltip_shows_known_and_unknown_levels() {
        let b = Battery { left: Some(80), right: None, case: Some(60) };
        assert_eq!(tooltip(Some(b)), "INZONE Buds  L 80% / R -- / C 60%");
        assert_eq!(tooltip(None), "INZONE Buds  L -- / R -- / C --");
    }

    #[test]
    fn status_needs_dongle_and_known_link() {
        assert_eq!(Status::of(false, Link::Connected), Status::NoDongle);
        assert_eq!(Status::of(true, Link::Unknown), Status::Checking);
        assert_eq!(Status::of(true, Link::Disconnected), Status::Disconnected);
        assert_eq!(Status::of(true, Link::Connected), Status::Connected);
    }

    #[test]
    fn header_lines_show_status_and_battery() {
        let b = Battery { left: Some(80), right: Some(75), case: None };
        let m = menu(&Config::default(), &endpoints(), None, Status::Connected, Some(b), false);
        assert_eq!(
            summary(&m[..2]),
            [
                ("INZONE Buds: 接続中".into(), false, false),
                ("L 80% / R 75% / ケース --".into(), false, false),
            ]
        );
    }

    #[test]
    fn toggles_carry_the_flipped_value() {
        let c = Config { auto_switch: true, notify_switch: false, ..Config::default() };
        let m = menu(&c, &endpoints(), None, Status::Connected, None, false);
        assert_eq!(command(find(&m, "自動切り替え")), Some(Command::Set(Setting::AutoSwitch(false))));
        assert_eq!(command(find(&m, "切り替えを通知")), Some(Command::Set(Setting::NotifySwitch(true))));
        assert_eq!(
            command(find(&m, "バッテリー低下を通知")),
            Some(Command::Set(Setting::NotifyBattery(false)))
        );
        assert_eq!(command(find(&m, "自動で更新する")), Some(Command::Set(Setting::AutoUpdate(false))));
        assert_eq!(command(find(&m, "終了")), Some(Command::Quit));
    }

    #[test]
    fn autostart_toggle_carries_flipped_value_and_checkmark() {
        let off = menu(&Config::default(), &endpoints(), None, Status::Connected, None, false);
        assert_eq!(command(find(&off, "Windows 起動時に開始")), Some(Command::Autostart(true)));
        let on = menu(&Config::default(), &endpoints(), None, Status::Connected, None, true);
        assert_eq!(command(find(&on, "Windows 起動時に開始")), Some(Command::Autostart(false)));
        assert!(matches!(find(&on, "Windows 起動時に開始"), Item::Entry { checked: true, .. }));
        assert_eq!(command(find(&on, "ログフォルダを開く")), Some(Command::OpenLogs));
    }

    #[test]
    fn connected_list_has_all_devices_and_marks_game_when_unset() {
        let m = menu(&Config::default(), &endpoints(), None, Status::Connected, None, false);
        assert_eq!(
            summary(submenu(&m, "接続時の出力")),
            [
                ("speaker name".into(), false, true),
                ("chat name".into(), false, true),
                ("game name".into(), true, true),
            ]
        );
    }

    #[test]
    fn connected_list_marks_configured_device() {
        let c = Config { connected_device: Some(dev("chat")), ..Config::default() };
        let m = menu(&c, &endpoints(), None, Status::Connected, None, false);
        let checked: Vec<_> = summary(submenu(&m, "接続時の出力")).into_iter().filter(|s| s.1).collect();
        assert_eq!(checked, [("chat name".into(), true, true)]);
    }

    #[test]
    fn disconnected_list_excludes_dongle_devices() {
        let c = Config { disconnected_device: Some(dev("speaker")), ..Config::default() };
        let m = menu(&c, &endpoints(), None, Status::Connected, None, false);
        let list = submenu(&m, "切断時の出力");
        assert_eq!(summary(list), [("speaker name".into(), true, true)]);
        assert_eq!(command(&list[0]), Some(Command::Set(Setting::DisconnectedDevice(dev("speaker")))));
    }

    #[test]
    fn missing_configured_device_is_shown_but_not_selectable() {
        let c = Config { disconnected_device: Some(dev("usb")), ..Config::default() };
        let m = menu(&c, &endpoints(), None, Status::Connected, None, false);
        assert_eq!(
            summary(submenu(&m, "切断時の出力")),
            [("speaker name".into(), false, true), ("usb name（見つかりません）".into(), true, false)]
        );
    }

    #[test]
    fn empty_device_list_has_placeholder() {
        let m = menu(&Config::default(), &[], None, Status::NoDongle, None, false);
        assert_eq!(
            summary(submenu(&m, "切断時の出力")),
            [("（再生デバイスがありません）".into(), false, false)]
        );
    }

    #[test]
    fn use_current_sets_non_dongle_default() {
        let m = menu(&Config::default(), &endpoints(), Some("speaker"), Status::Connected, None, false);
        assert_eq!(
            command(find(&m, "今の出力を切断時の出力にする")),
            Some(Command::Set(Setting::DisconnectedDevice(dev("speaker"))))
        );
    }

    #[test]
    fn use_current_is_disabled_for_dongle_or_unknown_default() {
        for current in [Some("game"), Some("chat"), Some("inactive"), None] {
            let m = menu(&Config::default(), &endpoints(), current, Status::Connected, None, false);
            assert_eq!(command(find(&m, "今の出力を切断時の出力にする")), None, "{current:?}");
        }
    }

    #[test]
    fn settings_apply_to_config() {
        let mut c = Config::default();
        Setting::AutoSwitch(false).apply(&mut c);
        Setting::ConnectedDevice(dev("chat")).apply(&mut c);
        Setting::DisconnectedDevice(dev("speaker")).apply(&mut c);
        Setting::NotifySwitch(false).apply(&mut c);
        Setting::NotifyBattery(false).apply(&mut c);
        Setting::AutoUpdate(false).apply(&mut c);
        assert_eq!(
            c,
            Config {
                auto_switch: false,
                connected_device: Some(dev("chat")),
                disconnected_device: Some(dev("speaker")),
                notify_switch: false,
                notify_battery: false,
                auto_update: false,
                ..Config::default()
            }
        );
    }

    #[test]
    fn version_label_is_not_selectable() {
        let m = menu(&Config::default(), &endpoints(), None, Status::Connected, None, false);
        let text = format!("バージョン {}", env!("CARGO_PKG_VERSION"));
        assert_eq!(command(find(&m, &text)), None);
    }
}
