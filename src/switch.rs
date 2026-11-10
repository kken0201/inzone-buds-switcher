//! 接続・切断時にどの出力へ切り替えるかの判断（docs/design.md 4.3）。純粋関数。

use crate::audio::{DongleKind, Endpoint};
use crate::config::{Config, DeviceRef};

/// 切り替えた・切り替えなかった結果。通知（docs/design.md 4.5）と 1 対 1 に対応する
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// 接続して接続時の出力に切り替えた
    Switched(DeviceRef),
    /// 切断して切断時の出力に戻した
    Restored(DeviceRef),
    /// 切断時の出力を自動で設定した
    Remembered(DeviceRef),
    Skipped(Skip),
    /// 既定デバイスの取得・設定が失敗した
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skip {
    /// `auto_switch` がオフ
    Disabled,
    /// 接続時の出力が未設定で、ドングルの Game デバイスも見つからない
    NoGameDevice,
    /// 切断時の出力が未設定
    NotConfigured,
    /// 設定された切り替え先が再生デバイスの中にない
    Missing(DeviceRef),
    /// 切断時点の既定デバイスがドングルのデバイスではない（接続中に利用者が手動で変えた）
    UserChanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connect {
    pub to: DeviceRef,
    /// 切断時の出力として保存するデバイス（切り替える直前の既定デバイス）
    pub remember: Option<DeviceRef>,
}

fn find<'a>(endpoints: &'a [Endpoint], id: &str) -> Option<&'a Endpoint> {
    endpoints.iter().find(|e| e.id == id)
}

fn device_ref(e: &Endpoint) -> DeviceRef {
    DeviceRef { id: e.id.clone(), name: e.name.clone() }
}

fn present(endpoints: &[Endpoint], dev: &DeviceRef) -> Result<DeviceRef, Skip> {
    find(endpoints, &dev.id).map(device_ref).ok_or_else(|| Skip::Missing(dev.clone()))
}

/// 切り替え前の既定デバイスのエンドポイント ID
#[derive(Debug, Clone, Copy, Default)]
pub struct Current<'a> {
    /// 既定の再生デバイス（eConsole）
    pub default: Option<&'a str>,
    /// 既定の通信デバイス（eCommunications）
    pub communications: Option<&'a str>,
}

/// ロールごとの判断。既定の再生デバイスと既定の通信デバイスは独立に決める
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan<D> {
    /// 既定の再生デバイス（eConsole と eMultimedia）。通知はこちらの結果だけで決まる
    pub default: Result<D, Skip>,
    /// 既定の通信デバイス（eCommunications）の切り替え先。`None` なら触らない
    pub communications: Option<DeviceRef>,
}

impl<D> Plan<D> {
    fn disabled() -> Self {
        Self { default: Err(Skip::Disabled), communications: None }
    }
}

pub fn on_connect(config: &Config, endpoints: &[Endpoint], current: Current) -> Plan<Connect> {
    if !config.auto_switch {
        return Plan::disabled();
    }
    let communications = config
        .switch_communications
        .then(|| endpoints.iter().find(|e| e.kind == Some(DongleKind::Chat)).map(device_ref))
        .flatten();
    Plan { default: connect_default(config, endpoints, current.default), communications }
}

fn connect_default(
    config: &Config,
    endpoints: &[Endpoint],
    current: Option<&str>,
) -> Result<Connect, Skip> {
    let to = match &config.connected_device {
        Some(dev) => present(endpoints, dev)?,
        None => endpoints
            .iter()
            .find(|e| e.kind == Some(DongleKind::Game))
            .map(device_ref)
            .ok_or(Skip::NoGameDevice)?,
    };
    let remember = match config.disconnected_device {
        Some(_) => None,
        None => current
            .and_then(|id| find(endpoints, id))
            .filter(|e| e.kind.is_none())
            .map(device_ref),
    };
    Ok(Connect { to, remember })
}

/// 既定の再生デバイスと既定の通信デバイスは別々に判断する。
/// 再生側を利用者が変えていても、通信側がドングルのままだと通話アプリが無音の Chat に残るため
pub fn on_disconnect(config: &Config, endpoints: &[Endpoint], current: Current) -> Plan<DeviceRef> {
    if !config.auto_switch {
        return Plan::disabled();
    }
    Plan {
        default: disconnect_default(config, endpoints, current.default),
        communications: disconnect_communications(config, endpoints, current.communications),
    }
}

/// 利用者が手動で変えたかどうかを先に見る。そのときは戻し先が未設定でも警告する必要がないため
fn disconnect_default(
    config: &Config,
    endpoints: &[Endpoint],
    current: Option<&str>,
) -> Result<DeviceRef, Skip> {
    if current.and_then(|id| find(endpoints, id)).is_none_or(|e| e.kind.is_none()) {
        return Err(Skip::UserChanged);
    }
    present(endpoints, config.disconnected_device.as_ref().ok_or(Skip::NotConfigured)?)
}

fn disconnect_communications(
    config: &Config,
    endpoints: &[Endpoint],
    current: Option<&str>,
) -> Option<DeviceRef> {
    if !config.switch_communications {
        return None;
    }
    let on_dongle = current.and_then(|id| find(endpoints, id)).is_some_and(|e| e.kind.is_some());
    if !on_dongle {
        return None;
    }
    present(endpoints, config.disconnected_device.as_ref()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(id: &str, kind: Option<DongleKind>) -> Endpoint {
        Endpoint { id: id.into(), name: format!("{id} name"), kind }
    }

    fn dev(id: &str) -> DeviceRef {
        DeviceRef { id: id.into(), name: format!("{id} name") }
    }

    fn endpoints() -> Vec<Endpoint> {
        vec![
            ep("speaker", None),
            ep("chat", Some(DongleKind::Chat)),
            ep("game", Some(DongleKind::Game)),
        ]
    }

    fn cur<'a>(default: Option<&'a str>, communications: Option<&'a str>) -> Current<'a> {
        Current { default, communications }
    }

    fn with_comms(id: &str) -> Config {
        Config { switch_communications: true, ..with_return(id) }
    }

    fn with_return(id: &str) -> Config {
        Config { disconnected_device: Some(dev(id)), ..Config::default() }
    }

    #[test]
    fn connect_picks_game_when_unset_and_remembers_previous_default() {
        assert_eq!(
            on_connect(&Config::default(), &endpoints(), cur(Some("speaker"), None)).default,
            Ok(Connect { to: dev("game"), remember: Some(dev("speaker")) })
        );
    }

    #[test]
    fn connect_uses_configured_device() {
        let c = Config { connected_device: Some(dev("chat")), ..with_return("speaker") };
        assert_eq!(
            on_connect(&c, &endpoints(), cur(Some("speaker"), None)).default,
            Ok(Connect { to: dev("chat"), remember: None })
        );
    }

    #[test]
    fn connect_takes_current_name_from_endpoint_list() {
        let c = Config {
            connected_device: Some(DeviceRef { id: "game".into(), name: "old name".into() }),
            ..with_return("speaker")
        };
        assert_eq!(on_connect(&c, &endpoints(), cur(None, None)).default.unwrap().to, dev("game"));
    }

    #[test]
    fn connect_skips_when_configured_device_is_missing() {
        let c = Config { connected_device: Some(dev("gone")), ..Config::default() };
        assert_eq!(on_connect(&c, &endpoints(), cur(Some("speaker"), None)).default, Err(Skip::Missing(dev("gone"))));
    }

    #[test]
    fn connect_skips_without_game_device() {
        let eps = vec![ep("speaker", None), ep("chat", Some(DongleKind::Chat))];
        assert_eq!(on_connect(&Config::default(), &eps, cur(Some("speaker"), None)).default, Err(Skip::NoGameDevice));
    }

    #[test]
    fn connect_does_not_remember_dongle_device() {
        assert_eq!(on_connect(&Config::default(), &endpoints(), cur(Some("chat"), None)).default.unwrap().remember, None);
    }

    #[test]
    fn connect_does_not_overwrite_configured_return_device() {
        let c = with_return("other");
        assert_eq!(on_connect(&c, &endpoints(), cur(Some("speaker"), None)).default.unwrap().remember, None);
    }

    #[test]
    fn connect_does_not_remember_unknown_or_absent_default() {
        let c = Config::default();
        assert_eq!(on_connect(&c, &endpoints(), cur(None, None)).default.unwrap().remember, None);
        assert_eq!(on_connect(&c, &endpoints(), cur(Some("inactive"), None)).default.unwrap().remember, None);
    }

    #[test]
    fn disabled_skips_both_directions() {
        let c = Config { auto_switch: false, ..with_return("speaker") };
        assert_eq!(on_connect(&c, &endpoints(), cur(Some("speaker"), None)).default, Err(Skip::Disabled));
        assert_eq!(on_disconnect(&c, &endpoints(), cur(Some("game"), None)).default, Err(Skip::Disabled));
    }

    #[test]
    fn disconnect_restores_return_device() {
        let c = with_return("speaker");
        assert_eq!(on_disconnect(&c, &endpoints(), cur(Some("game"), None)).default, Ok(dev("speaker")));
        assert_eq!(on_disconnect(&c, &endpoints(), cur(Some("chat"), None)).default, Ok(dev("speaker")));
    }

    #[test]
    fn disconnect_skips_when_user_changed_output() {
        let c = with_return("speaker");
        assert_eq!(on_disconnect(&c, &endpoints(), cur(Some("speaker"), None)).default, Err(Skip::UserChanged));
        assert_eq!(on_disconnect(&c, &endpoints(), cur(None, None)).default, Err(Skip::UserChanged));
        assert_eq!(on_disconnect(&c, &endpoints(), cur(Some("inactive"), None)).default, Err(Skip::UserChanged));
    }

    #[test]
    fn user_change_wins_over_missing_return_device() {
        assert_eq!(
            on_disconnect(&Config::default(), &endpoints(), cur(Some("speaker"), None)).default,
            Err(Skip::UserChanged)
        );
    }

    #[test]
    fn disconnect_skips_when_return_device_unset() {
        assert_eq!(
            on_disconnect(&Config::default(), &endpoints(), cur(Some("game"), None)).default,
            Err(Skip::NotConfigured)
        );
    }

    #[test]
    fn disconnect_skips_when_return_device_missing() {
        assert_eq!(
            on_disconnect(&with_return("usb-speaker"), &endpoints(), cur(Some("game"), None)).default,
            Err(Skip::Missing(dev("usb-speaker")))
        );
    }

    #[test]
    fn disabled_touches_no_role() {
        let c = Config { auto_switch: false, ..with_comms("speaker") };
        let plan = Plan::<Connect> { default: Err(Skip::Disabled), communications: None };
        assert_eq!(on_connect(&c, &endpoints(), cur(Some("speaker"), Some("speaker"))), plan);
        let plan = Plan::<DeviceRef> { default: Err(Skip::Disabled), communications: None };
        assert_eq!(on_disconnect(&c, &endpoints(), cur(Some("game"), Some("chat"))), plan);
    }

    #[test]
    fn communications_toggle_off_never_touches_it() {
        let c = with_return("speaker");
        let on = on_connect(&c, &endpoints(), cur(Some("speaker"), Some("speaker")));
        assert_eq!(on.communications, None);
        let off = on_disconnect(&c, &endpoints(), cur(Some("game"), Some("chat")));
        assert_eq!(off.communications, None);
    }

    #[test]
    fn connect_sends_communications_to_chat() {
        let plan = on_connect(&with_comms("speaker"), &endpoints(), cur(Some("speaker"), Some("speaker")));
        assert_eq!(plan.communications, Some(dev("chat")));
        assert_eq!(plan.default.unwrap().to, dev("game"));
    }

    #[test]
    fn connect_without_chat_leaves_communications_and_still_switches_default() {
        let eps = vec![ep("speaker", None), ep("game", Some(DongleKind::Game))];
        let plan = on_connect(&with_comms("speaker"), &eps, cur(Some("speaker"), Some("speaker")));
        assert_eq!(plan.communications, None);
        assert_eq!(plan.default.unwrap().to, dev("game"));
    }

    #[test]
    fn disconnect_restores_communications_from_dongle() {
        for comms in ["chat", "game"] {
            let plan = on_disconnect(&with_comms("speaker"), &endpoints(), cur(Some("game"), Some(comms)));
            assert_eq!(plan.communications, Some(dev("speaker")));
            assert_eq!(plan.default, Ok(dev("speaker")));
        }
    }

    #[test]
    fn disconnect_leaves_communications_that_is_not_dongle() {
        let c = with_comms("speaker");
        for comms in [Some("speaker"), Some("inactive"), None] {
            let plan = on_disconnect(&c, &endpoints(), cur(Some("game"), comms));
            assert_eq!(plan.communications, None);
        }
    }

    #[test]
    fn disconnect_leaves_communications_when_return_device_unset_or_missing() {
        let unset = Config { switch_communications: true, ..Config::default() };
        let plan = on_disconnect(&unset, &endpoints(), cur(Some("game"), Some("chat")));
        assert_eq!(plan.communications, None);
        let plan = on_disconnect(&with_comms("usb-speaker"), &endpoints(), cur(Some("game"), Some("chat")));
        assert_eq!(plan.communications, None);
    }

    #[test]
    fn disconnect_restores_communications_even_when_user_changed_default() {
        let plan = on_disconnect(&with_comms("speaker"), &endpoints(), cur(Some("speaker"), Some("chat")));
        assert_eq!(plan.default, Err(Skip::UserChanged));
        assert_eq!(plan.communications, Some(dev("speaker")));
    }
}
