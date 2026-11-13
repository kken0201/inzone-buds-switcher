//! `App` のシナリオテスト。HID イベントの列と時刻を流し、出力の切り替え・通知・設定の保存までを通しで確かめる。
//! 再生デバイスはメモリ上の偽物（`Fake`）、設定ファイルは一時ディレクトリに置くので、実機やトレイは要らない。

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use super::{App, Update};
use crate::audio::{Devices, DongleKind, Endpoint};
use crate::config::{self, Config, DeviceRef};
use crate::hid::protocol::{Battery, Earbuds};
use crate::hid::watch::HidEvent;
use crate::notify::{self, Kind, LowAlert};
use crate::state::{Link, Transition};
use crate::switch::{Outcome, Skip};

/// 既定の設定のデバウンス（`debounce_ms`）
const DEBOUNCE: u64 = 1500;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    Default(String),
    Communications(String),
}

/// 偽物の再生デバイスの状態。テストから書き換えて、利用者の手動の操作や失敗を再現する
#[derive(Default)]
struct World {
    endpoints: Vec<Endpoint>,
    default: Option<String>,
    communications: Option<String>,
    /// `set_*` を失敗させる
    fail_set: bool,
    /// `list` を失敗させる
    fail_list: bool,
    /// 成功した `set_*` の呼び出し
    calls: Vec<Call>,
}

struct Fake(Rc<RefCell<World>>);

impl Devices for Fake {
    fn list(&self) -> Result<Vec<Endpoint>> {
        let w = self.0.borrow();
        if w.fail_list {
            bail!("列挙に失敗");
        }
        Ok(w.endpoints.clone())
    }

    fn default_id(&self) -> Result<Option<String>> {
        Ok(self.0.borrow().default.clone())
    }

    fn communications_id(&self) -> Result<Option<String>> {
        Ok(self.0.borrow().communications.clone())
    }

    fn set_default(&self, id: &str) -> Result<()> {
        let mut w = self.0.borrow_mut();
        if w.fail_set {
            bail!("設定に失敗");
        }
        w.default = Some(id.into());
        w.calls.push(Call::Default(id.into()));
        Ok(())
    }

    fn set_communications(&self, id: &str) -> Result<()> {
        let mut w = self.0.borrow_mut();
        if w.fail_set {
            bail!("設定に失敗");
        }
        w.communications = Some(id.into());
        w.calls.push(Call::Communications(id.into()));
        Ok(())
    }
}

fn ep(id: &str, kind: Option<DongleKind>) -> Endpoint {
    Endpoint { id: id.into(), name: format!("{id} name"), kind }
}

fn dev(id: &str) -> DeviceRef {
    DeviceRef { id: id.into(), name: format!("{id} name") }
}

/// スピーカーと、ドングルの Chat・Game
fn endpoints() -> Vec<Endpoint> {
    vec![ep("speaker", None), ep("chat", Some(DongleKind::Chat)), ep("game", Some(DongleKind::Game))]
}

fn buds(left: bool, right: bool) -> HidEvent {
    HidEvent::Earbuds(Earbuds { left, right })
}

fn out() -> HidEvent {
    buds(true, false)
}

fn case() -> HidEvent {
    buds(false, false)
}

struct Scenario {
    app: App<Fake>,
    world: Rc<RefCell<World>>,
    path: PathBuf,
    t0: Instant,
}

impl Scenario {
    /// `config` が `Some` なら設定ファイルに書いてから起動する。既定の出力は `default`、通信デバイスも同じにする
    fn new(name: &str, config: Option<Config>, default: &str) -> Self {
        let dir = std::env::temp_dir()
            .join(format!("inzone-buds-switcher-scenario-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("config.json");
        if let Some(c) = config {
            config::save(&path, &c).unwrap();
        }
        let world = Rc::new(RefCell::new(World {
            endpoints: endpoints(),
            default: Some(default.into()),
            communications: Some(default.into()),
            ..World::default()
        }));
        let app = App::with_devices(path.clone(), Fake(world.clone()));
        Self { app, world, path, t0: Instant::now() }
    }

    fn at(&self, ms: u64) -> Instant {
        self.t0 + Duration::from_millis(ms)
    }

    fn handle(&mut self, ev: HidEvent, ms: u64) -> Option<Update> {
        self.app.handle(ev, self.at(ms))
    }

    fn tick(&mut self, ms: u64) -> Option<Update> {
        self.app.tick(self.at(ms))
    }

    /// `ev` を `ms` に流し、デバウンスの確定時刻に `tick` して、確定した変化を返す。
    /// 確定時刻より前に `tick` しても何も起きないことも確かめる
    fn settle(&mut self, ev: HidEvent, ms: u64) -> Option<Update> {
        assert!(self.handle(ev, ms).is_none(), "デバウンスの前に確定した: {ev:?}");
        assert!(self.tick(ms + DEBOUNCE - 1).is_none(), "デバウンスの前に確定した: {ev:?}");
        self.tick(ms + DEBOUNCE)
    }

    /// ドングルが見つかり、`ev` で状態が決まった（起動直後）
    fn start(&mut self, ev: HidEvent) -> Update {
        assert!(self.handle(HidEvent::DongleFound, 0).is_none());
        self.settle(ev, 0).expect("状態が決まらない")
    }

    fn take_calls(&self) -> Vec<Call> {
        std::mem::take(&mut self.world.borrow_mut().calls)
    }

    fn default(&self) -> Option<String> {
        self.world.borrow().default.clone()
    }

    fn communications(&self) -> Option<String> {
        self.world.borrow().communications.clone()
    }

    /// 利用者が手動で既定の出力を変えた
    fn user_sets_default(&self, id: &str) {
        self.world.borrow_mut().default = Some(id.into());
    }

    /// 結果ごとの通知（出ないものは除く）
    fn notices(&self, update: &Update) -> Vec<(Kind, String)> {
        update
            .outcomes
            .iter()
            .filter_map(|o| notify::for_outcome(o, self.app.config()))
            .map(|n| (n.kind, n.text))
            .collect()
    }

    /// 設定ファイルに保存された内容
    fn saved(&self) -> Config {
        config::load(&self.path)
    }
}

impl Drop for Scenario {
    fn drop(&mut self) {
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

fn with_return(id: &str) -> Config {
    Config { disconnected_device: Some(dev(id)), ..Config::default() }
}

#[test]
fn take_out_from_case_switches_to_game_and_remembers_previous_default() {
    let mut s = Scenario::new("take-out", None, "speaker");
    let u = s.start(case());
    assert_eq!(u.transition, Transition::Established { connected: false });
    assert_eq!(s.app.link(), Link::Disconnected);
    assert!(s.take_calls().is_empty());

    let u = s.settle(out(), 5000).unwrap();
    assert_eq!(u.transition, Transition::Changed { connected: true });
    assert_eq!(u.outcomes, vec![Outcome::Remembered(dev("speaker")), Outcome::Switched(dev("game"))]);
    assert_eq!(s.take_calls(), vec![Call::Default("game".into())]);
    assert_eq!(s.default().as_deref(), Some("game"));
    assert_eq!(
        s.notices(&u),
        vec![
            (Kind::Switch, "切断時の出力を『speaker name』に設定しました（メニューで変更できます）".into()),
            (Kind::Switch, "🎧 game name に切り替えました".into()),
        ]
    );
    // 覚えた戻し先はメモリ上の設定にも、設定ファイルにも入る
    assert_eq!(s.app.config().disconnected_device, Some(dev("speaker")));
    assert_eq!(s.saved().disconnected_device, Some(dev("speaker")));
}

#[test]
fn put_back_in_case_restores_remembered_device() {
    let mut s = Scenario::new("put-back", None, "speaker");
    s.start(case());
    s.settle(out(), 5000).unwrap();
    s.take_calls();

    let u = s.settle(case(), 10_000).unwrap();
    assert_eq!(u.transition, Transition::Changed { connected: false });
    assert_eq!(u.outcomes, vec![Outcome::Restored(dev("speaker"))]);
    assert_eq!(s.take_calls(), vec![Call::Default("speaker".into())]);
    assert_eq!(s.notices(&u), vec![(Kind::Switch, "🔈 speaker nameに戻しました".into())]);

    // 2 回目の接続では、戻し先が設定済みなので覚え直さない
    let u = s.settle(out(), 20_000).unwrap();
    assert_eq!(u.outcomes, vec![Outcome::Switched(dev("game"))]);
}

#[test]
fn taking_out_both_one_by_one_switches_once() {
    let mut s = Scenario::new("one-by-one", Some(with_return("speaker")), "speaker");
    s.start(case());
    // 左を取り出し、デバウンス中に右も取り出す
    assert!(s.handle(buds(true, false), 5000).is_none());
    assert!(s.handle(buds(true, true), 5800).is_none());
    let u = s.tick(5000 + DEBOUNCE).unwrap();
    assert_eq!(u.outcomes, vec![Outcome::Switched(dev("game"))]);
    // 片耳だけ戻しても接続のまま
    assert!(s.handle(buds(false, true), 8000).is_none());
    assert!(s.tick(20_000).is_none());
    assert_eq!(s.take_calls(), vec![Call::Default("game".into())]);
}

#[test]
fn brief_put_back_within_debounce_does_not_switch() {
    let mut s = Scenario::new("flap", Some(with_return("speaker")), "speaker");
    s.start(case());
    s.settle(out(), 5000).unwrap();
    s.take_calls();
    assert!(s.handle(case(), 10_000).is_none());
    assert!(s.handle(out(), 10_500).is_none());
    assert!(s.tick(20_000).is_none());
    assert!(s.take_calls().is_empty());
}

#[test]
fn battery_report_also_drives_connection() {
    let mut s = Scenario::new("battery", Some(with_return("speaker")), "speaker");
    s.start(case());
    let b = Battery { left: Some(80), right: None, case: Some(50) };
    assert!(s.handle(HidEvent::Battery(b), 5000).is_none());
    let u = s.tick(5000 + DEBOUNCE).unwrap();
    assert_eq!(u.outcomes, vec![Outcome::Switched(dev("game"))]);
    assert_eq!(s.app.battery(), Some(b));
    // ケースに戻した側の残量は消える
    s.handle(case(), 10_000);
    assert_eq!(s.app.battery(), Some(Battery { left: None, right: None, case: Some(50) }));
}

#[test]
fn low_battery_is_alerted_once() {
    let mut s = Scenario::new("low", Some(with_return("speaker")), "speaker");
    s.start(case());
    let low = Battery { left: Some(5), right: Some(40), case: None };
    s.handle(HidEvent::Battery(low), 5000);
    assert_eq!(s.app.take_low_battery(), Some(LowAlert { left: Some(5), right: None }));
    assert_eq!(s.app.take_low_battery(), None);
    s.handle(HidEvent::Battery(Battery { left: Some(4), ..low }), 35_000);
    assert_eq!(s.app.take_low_battery(), None);
}

#[test]
fn user_changed_output_while_connected_is_not_restored() {
    let mut s = Scenario::new("user-changed", Some(with_return("speaker")), "speaker");
    s.world.borrow_mut().endpoints.push(ep("usb", None));
    s.start(case());
    s.settle(out(), 5000).unwrap();
    s.take_calls();

    s.user_sets_default("usb");
    let u = s.settle(case(), 10_000).unwrap();
    assert_eq!(u.outcomes, vec![Outcome::Skipped(Skip::UserChanged)]);
    assert!(s.take_calls().is_empty());
    assert_eq!(s.default().as_deref(), Some("usb"));
    assert!(s.notices(&u).is_empty());
}

#[test]
fn startup_in_case_does_not_switch_when_output_is_not_dongle() {
    // 戻し先が未設定でも、利用者の選んだ出力なので警告しない
    let mut s = Scenario::new("startup-case", None, "speaker");
    let u = s.start(case());
    assert_eq!(u.outcomes, vec![Outcome::Skipped(Skip::UserChanged)]);
    assert!(s.notices(&u).is_empty());
    assert!(s.take_calls().is_empty());
}

#[test]
fn startup_connected_on_dongle_does_not_switch() {
    let mut s = Scenario::new("startup-connected", Some(with_return("speaker")), "chat");
    let u = s.start(out());
    assert_eq!(u.transition, Transition::Established { connected: true });
    assert_eq!(u.outcomes, vec![Outcome::Skipped(Skip::OnDongle)]);
    assert!(s.notices(&u).is_empty());
    assert!(s.take_calls().is_empty());
    assert_eq!(s.default().as_deref(), Some("chat"));
}

#[test]
fn nothing_happens_until_earbuds_state_arrives() {
    let mut s = Scenario::new("unknown", Some(with_return("speaker")), "game");
    assert!(s.handle(HidEvent::DongleFound, 0).is_none());
    assert!(s.app.dongle());
    assert!(s.tick(60_000).is_none());
    assert_eq!(s.app.link(), Link::Unknown);
    assert!(s.take_calls().is_empty());
}

#[test]
fn dongle_unplug_and_replug_does_not_switch() {
    let mut s = Scenario::new("replug", Some(with_return("speaker")), "speaker");
    s.start(case());
    s.settle(out(), 5000).unwrap();
    s.take_calls();

    let u = s.handle(HidEvent::DongleLost, 10_000).unwrap();
    assert_eq!(u.transition, Transition::Lost);
    assert!(u.outcomes.is_empty());
    assert_eq!(s.app.link(), Link::Unknown);
    assert!(!s.app.dongle());
    assert_eq!(s.app.battery(), None);

    // 差し直すと、イヤホンの状態が届いて Connected に決まる。既定はドングルのままなので触らない
    assert!(s.handle(HidEvent::DongleFound, 15_000).is_none());
    let u = s.settle(out(), 15_200).unwrap();
    assert_eq!(u.transition, Transition::Established { connected: true });
    assert_eq!(u.outcomes, vec![Outcome::Skipped(Skip::OnDongle)]);
    assert!(s.take_calls().is_empty());
    assert_eq!(s.default().as_deref(), Some("game"));
}

#[test]
fn dongle_lost_discards_pending_change() {
    let mut s = Scenario::new("lost-pending", Some(with_return("speaker")), "speaker");
    s.start(case());
    assert!(s.handle(out(), 5000).is_none());
    assert!(s.handle(HidEvent::DongleLost, 5500).is_some());
    assert!(s.tick(5000 + DEBOUNCE).is_none());
    assert!(s.take_calls().is_empty());
}

#[test]
fn switch_communications_switches_chat_and_restores() {
    let config = Config { switch_communications: true, ..with_return("speaker") };
    let mut s = Scenario::new("comms", Some(config), "speaker");
    s.start(case());

    let u = s.settle(out(), 5000).unwrap();
    assert_eq!(u.outcomes, vec![Outcome::Switched(dev("game"))]);
    assert_eq!(s.take_calls(), vec![Call::Default("game".into()), Call::Communications("chat".into())]);
    assert_eq!(s.communications().as_deref(), Some("chat"));

    let u = s.settle(case(), 10_000).unwrap();
    assert_eq!(u.outcomes, vec![Outcome::Restored(dev("speaker"))]);
    assert_eq!(
        s.take_calls(),
        vec![Call::Default("speaker".into()), Call::Communications("speaker".into())]
    );
}

#[test]
fn communications_off_leaves_communications_device_alone() {
    let mut s = Scenario::new("comms-off", Some(with_return("speaker")), "speaker");
    s.start(case());
    s.settle(out(), 5000).unwrap();
    s.settle(case(), 10_000).unwrap();
    assert_eq!(s.take_calls(), vec![Call::Default("game".into()), Call::Default("speaker".into())]);
    assert_eq!(s.communications().as_deref(), Some("speaker"));
}

#[test]
fn not_configured_return_is_skipped_with_warning() {
    // 起動時にすでに Game を向いていると、戻し先を覚える機会がない
    let mut s = Scenario::new("not-configured", None, "game");
    s.start(out());

    let u = s.settle(case(), 5000).unwrap();
    assert_eq!(u.outcomes, vec![Outcome::Skipped(Skip::NotConfigured)]);
    assert_eq!(s.notices(&u), vec![(Kind::Warning, "⚠ 切断時の出力が未設定です".into())]);
    assert!(s.take_calls().is_empty());
    assert_eq!(s.default().as_deref(), Some("game"));
}

#[test]
fn missing_return_is_skipped_with_warning() {
    let mut s = Scenario::new("missing", Some(with_return("usb")), "speaker");
    s.start(case());
    s.settle(out(), 5000).unwrap();
    s.take_calls();

    let u = s.settle(case(), 10_000).unwrap();
    assert_eq!(u.outcomes, vec![Outcome::Skipped(Skip::Missing(dev("usb")))]);
    assert_eq!(s.notices(&u), vec![(Kind::Warning, "⚠ 戻し先のデバイスが見つかりません".into())]);
    assert!(s.take_calls().is_empty());
}

#[test]
fn missing_game_device_is_skipped_with_warning() {
    let mut s = Scenario::new("no-game", Some(with_return("speaker")), "speaker");
    s.world.borrow_mut().endpoints.retain(|e| e.kind != Some(DongleKind::Game));
    s.start(case());

    let u = s.settle(out(), 5000).unwrap();
    assert_eq!(u.outcomes, vec![Outcome::Skipped(Skip::NoGameDevice)]);
    assert_eq!(
        s.notices(&u),
        vec![(Kind::Warning, "⚠ INZONE Buds の出力デバイスが見つかりません".into())]
    );
    assert!(s.take_calls().is_empty());
}

#[test]
fn auto_switch_off_does_nothing() {
    let config = Config { auto_switch: false, ..with_return("speaker") };
    let mut s = Scenario::new("disabled", Some(config), "speaker");
    s.start(case());
    let u = s.settle(out(), 5000).unwrap();
    assert_eq!(u.outcomes, vec![Outcome::Skipped(Skip::Disabled)]);
    assert!(s.notices(&u).is_empty());
    assert!(s.take_calls().is_empty());
}

#[test]
fn failed_switch_is_reported() {
    let mut s = Scenario::new("failed", None, "speaker");
    s.start(case());
    s.world.borrow_mut().fail_set = true;

    let u = s.settle(out(), 5000).unwrap();
    let [Outcome::Remembered(remembered), Outcome::Failed(msg)] = u.outcomes.as_slice() else {
        panic!("{:?}", u.outcomes);
    };
    assert_eq!(remembered, &dev("speaker"));
    assert!(msg.contains("game name に切り替えられません"), "{msg}");
    assert_eq!(
        s.notices(&u).last(),
        Some(&(Kind::Warning, "⚠ 出力を切り替えられませんでした".into()))
    );
    assert_eq!(s.default().as_deref(), Some("speaker"));
}

#[test]
fn failed_communications_switch_is_reported_after_default() {
    let config = Config { switch_communications: true, ..with_return("speaker") };
    let mut s = Scenario::new("failed-comms", Some(config), "speaker");
    s.start(case());
    s.world.borrow_mut().fail_set = true;

    let u = s.settle(out(), 5000).unwrap();
    assert!(
        matches!(u.outcomes.as_slice(), [Outcome::Failed(a), Outcome::Failed(b)]
            if a.contains("game name") && b.contains("既定の通信デバイスを chat name")),
        "{:?}",
        u.outcomes
    );
}

#[test]
fn failed_enumeration_is_reported() {
    let mut s = Scenario::new("failed-list", Some(with_return("speaker")), "speaker");
    s.start(case());
    s.world.borrow_mut().fail_list = true;

    let u = s.settle(out(), 5000).unwrap();
    assert!(
        matches!(u.outcomes.as_slice(), [Outcome::Failed(m)] if m.contains("再生デバイスを取得できません")),
        "{:?}",
        u.outcomes
    );
    assert!(s.take_calls().is_empty());
}

#[test]
fn update_config_is_saved_to_file() {
    let mut s = Scenario::new("update-config", None, "speaker");
    s.app.update_config(|c| c.switch_communications = true);
    assert!(s.app.config().switch_communications);
    assert!(s.saved().switch_communications);
}
