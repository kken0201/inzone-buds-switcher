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

pub fn on_connect(
    config: &Config,
    endpoints: &[Endpoint],
    current: Option<&str>,
) -> Result<Connect, Skip> {
    if !config.auto_switch {
        return Err(Skip::Disabled);
    }
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

/// 利用者が手動で変えたかどうかを先に見る。そのときは戻し先が未設定でも警告する必要がないため
pub fn on_disconnect(
    config: &Config,
    endpoints: &[Endpoint],
    current: Option<&str>,
) -> Result<DeviceRef, Skip> {
    if !config.auto_switch {
        return Err(Skip::Disabled);
    }
    if current.and_then(|id| find(endpoints, id)).is_none_or(|e| e.kind.is_none()) {
        return Err(Skip::UserChanged);
    }
    present(endpoints, config.disconnected_device.as_ref().ok_or(Skip::NotConfigured)?)
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

    fn with_return(id: &str) -> Config {
        Config { disconnected_device: Some(dev(id)), ..Config::default() }
    }

    #[test]
    fn connect_picks_game_when_unset_and_remembers_previous_default() {
        assert_eq!(
            on_connect(&Config::default(), &endpoints(), Some("speaker")),
            Ok(Connect { to: dev("game"), remember: Some(dev("speaker")) })
        );
    }

    #[test]
    fn connect_uses_configured_device() {
        let c = Config { connected_device: Some(dev("chat")), ..with_return("speaker") };
        assert_eq!(
            on_connect(&c, &endpoints(), Some("speaker")),
            Ok(Connect { to: dev("chat"), remember: None })
        );
    }

    #[test]
    fn connect_takes_current_name_from_endpoint_list() {
        let c = Config {
            connected_device: Some(DeviceRef { id: "game".into(), name: "old name".into() }),
            ..with_return("speaker")
        };
        assert_eq!(on_connect(&c, &endpoints(), None).unwrap().to, dev("game"));
    }

    #[test]
    fn connect_skips_when_configured_device_is_missing() {
        let c = Config { connected_device: Some(dev("gone")), ..Config::default() };
        assert_eq!(on_connect(&c, &endpoints(), Some("speaker")), Err(Skip::Missing(dev("gone"))));
    }

    #[test]
    fn connect_skips_without_game_device() {
        let eps = vec![ep("speaker", None), ep("chat", Some(DongleKind::Chat))];
        assert_eq!(on_connect(&Config::default(), &eps, Some("speaker")), Err(Skip::NoGameDevice));
    }

    #[test]
    fn connect_does_not_remember_dongle_device() {
        assert_eq!(on_connect(&Config::default(), &endpoints(), Some("chat")).unwrap().remember, None);
    }

    #[test]
    fn connect_does_not_overwrite_configured_return_device() {
        let c = with_return("other");
        assert_eq!(on_connect(&c, &endpoints(), Some("speaker")).unwrap().remember, None);
    }

    #[test]
    fn connect_does_not_remember_unknown_or_absent_default() {
        let c = Config::default();
        assert_eq!(on_connect(&c, &endpoints(), None).unwrap().remember, None);
        assert_eq!(on_connect(&c, &endpoints(), Some("inactive")).unwrap().remember, None);
    }

    #[test]
    fn disabled_skips_both_directions() {
        let c = Config { auto_switch: false, ..with_return("speaker") };
        assert_eq!(on_connect(&c, &endpoints(), Some("speaker")), Err(Skip::Disabled));
        assert_eq!(on_disconnect(&c, &endpoints(), Some("game")), Err(Skip::Disabled));
    }

    #[test]
    fn disconnect_restores_return_device() {
        let c = with_return("speaker");
        assert_eq!(on_disconnect(&c, &endpoints(), Some("game")), Ok(dev("speaker")));
        assert_eq!(on_disconnect(&c, &endpoints(), Some("chat")), Ok(dev("speaker")));
    }

    #[test]
    fn disconnect_skips_when_user_changed_output() {
        let c = with_return("speaker");
        assert_eq!(on_disconnect(&c, &endpoints(), Some("speaker")), Err(Skip::UserChanged));
        assert_eq!(on_disconnect(&c, &endpoints(), None), Err(Skip::UserChanged));
        assert_eq!(on_disconnect(&c, &endpoints(), Some("inactive")), Err(Skip::UserChanged));
    }

    #[test]
    fn user_change_wins_over_missing_return_device() {
        assert_eq!(
            on_disconnect(&Config::default(), &endpoints(), Some("speaker")),
            Err(Skip::UserChanged)
        );
    }

    #[test]
    fn disconnect_skips_when_return_device_unset() {
        assert_eq!(
            on_disconnect(&Config::default(), &endpoints(), Some("game")),
            Err(Skip::NotConfigured)
        );
    }

    #[test]
    fn disconnect_skips_when_return_device_missing() {
        assert_eq!(
            on_disconnect(&with_return("usb-speaker"), &endpoints(), Some("game")),
            Err(Skip::Missing(dev("usb-speaker")))
        );
    }
}
