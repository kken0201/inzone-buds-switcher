//! HID イベントを状態機械に渡し、確定した変化に応じて出力を切り替える。
//! COM を使うので、`ComGuard` を初期化したスレッドで使う。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::audio::{Audio, Endpoint};
use crate::config::{self, Config, DeviceRef};
use crate::hid::protocol::Battery;
use crate::hid::watch::HidEvent;
use crate::notify::{self, LowAlert, LowFlags};
use crate::state::{Link, Machine, Obs, Transition};
use crate::switch::{self, Current, Outcome, Skip};

pub struct App {
    config: Config,
    config_path: PathBuf,
    audio: Audio,
    machine: Machine,
    dongle: bool,
    /// 最後に届いたバッテリー。切断された側は None にする
    battery: Option<Battery>,
    /// バッテリー低下を通知済みの側。`battery` と違い、切断されても消さない（docs/design.md 4.5）
    low: LowFlags,
    low_alert: Option<LowAlert>,
}

#[derive(Debug)]
pub struct Update {
    pub transition: Transition,
    /// `Transition::Changed` のときだけ空でない
    pub outcomes: Vec<Outcome>,
}

impl App {
    pub fn new(config_path: PathBuf) -> Result<Self> {
        let config = config::load(&config_path);
        let machine = Machine::new(Duration::from_millis(config.debounce_ms));
        Ok(Self {
            audio: Audio::new()?,
            config,
            config_path,
            machine,
            dongle: false,
            battery: None,
            low: LowFlags::default(),
            low_alert: None,
        })
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn link(&self) -> Link {
        self.machine.link()
    }

    pub fn dongle(&self) -> bool {
        self.dongle
    }

    pub fn battery(&self) -> Option<Battery> {
        self.battery
    }

    /// 直近のイベントで新たにしきい値以下になった側。取り出すと空になる
    pub fn take_low_battery(&mut self) -> Option<LowAlert> {
        self.low_alert.take()
    }

    /// 再生デバイスの一覧と、既定デバイスの ID
    pub fn endpoints(&self) -> Result<(Vec<Endpoint>, Option<String>)> {
        Ok((self.audio.list()?, self.audio.default_id()?))
    }

    /// 変更して保存する。保存に失敗してもメモリ上の設定は使う。この起動中の切断では戻せるようにするため
    pub fn update_config(&mut self, change: impl FnOnce(&mut Config)) {
        change(&mut self.config);
        if let Err(e) = config::save(&self.config_path, &self.config) {
            tracing::warn!("設定を保存できません: {e:#}");
        }
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.machine.deadline()
    }

    pub fn handle(&mut self, ev: HidEvent, now: Instant) -> Option<Update> {
        let obs = match ev {
            // 差し直した直後は Unknown のままで、イヤホンの状態が届くまで待つ
            HidEvent::DongleFound => {
                self.dongle = true;
                return None;
            }
            HidEvent::DongleLost => {
                self.dongle = false;
                self.battery = None;
                Obs::DongleLost
            }
            HidEvent::Earbuds(e) => {
                // ケースに戻した側の残量は、次の問い合わせ（最大 30 秒後）まで古いまま残るため消す
                if let Some(b) = &mut self.battery {
                    b.left = b.left.filter(|_| e.left);
                    b.right = b.right.filter(|_| e.right);
                }
                Obs::Earbuds(e)
            }
            HidEvent::Battery(b) => {
                self.battery = Some(b);
                let (low, alert) =
                    notify::battery_alert(self.low, b.left, b.right, self.config.low_battery_threshold);
                self.low = low;
                self.low_alert = alert.or(self.low_alert);
                Obs::Earbuds(b.earbuds())
            }
        };
        let transition = self.machine.observe(obs, now)?;
        Some(self.apply(transition))
    }

    pub fn tick(&mut self, now: Instant) -> Option<Update> {
        let transition = self.machine.poll(now)?;
        Some(self.apply(transition))
    }

    fn apply(&mut self, transition: Transition) -> Update {
        let outcomes = match transition {
            Transition::Changed { connected } => self.switch(connected),
            Transition::Established { .. } | Transition::Lost => Vec::new(),
        };
        Update { transition, outcomes }
    }

    fn switch(&mut self, connected: bool) -> Vec<Outcome> {
        let (endpoints, default) = match self.endpoints() {
            Ok(v) => v,
            Err(e) => return vec![Outcome::Failed(format!("再生デバイスを取得できません: {e:#}"))],
        };
        let communications = match self.audio.communications_id() {
            Ok(v) => v,
            Err(e) => return vec![Outcome::Failed(format!("再生デバイスを取得できません: {e:#}"))],
        };
        let current = Current { default: default.as_deref(), communications: communications.as_deref() };
        let (default, communications, done): (Result<(DeviceRef, Option<DeviceRef>), Skip>, _, fn(DeviceRef) -> Outcome) =
            if connected {
                let plan = switch::on_connect(&self.config, &endpoints, current);
                (plan.default.map(|c| (c.to, c.remember)), plan.communications, Outcome::Switched)
            } else {
                let plan = switch::on_disconnect(&self.config, &endpoints, current);
                (plan.default.map(|to| (to, None)), plan.communications, Outcome::Restored)
            };
        if connected && self.config.auto_switch && self.config.switch_communications && communications.is_none() {
            tracing::warn!("Chat のデバイスが見つからないため、既定の通信デバイスは切り替えません");
        }
        let mut outcomes = Vec::new();
        match default {
            Err(skip) => outcomes.push(Outcome::Skipped(skip)),
            Ok((to, remember)) => {
                if let Some(dev) = remember {
                    self.update_config(|c| c.disconnected_device = Some(dev.clone()));
                    outcomes.push(Outcome::Remembered(dev));
                }
                outcomes.push(match self.audio.set_default(&to.id) {
                    Ok(()) => done(to),
                    Err(e) => Outcome::Failed(format!("{} に切り替えられません: {e:#}", to.name)),
                });
            }
        }
        if let Some(to) = communications {
            match self.audio.set_communications(&to.id) {
                Ok(()) => tracing::info!("既定の通信デバイスを {} に切り替えました", to.name),
                Err(e) => outcomes.push(Outcome::Failed(format!(
                    "既定の通信デバイスを {} に切り替えられません: {e:#}",
                    to.name
                ))),
            }
        }
        outcomes
    }
}
