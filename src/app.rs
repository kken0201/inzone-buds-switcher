//! HID イベントを状態機械に渡し、確定した変化に応じて出力を切り替える。
//! Core Audio（`Audio`）を使うときは COM が要るので、`ComGuard` を初期化したスレッドで使う。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::audio::{Audio, Devices, DongleKind, Endpoint, find_kind};
use crate::config::{self, Config};
use crate::hid::protocol::Battery;
use crate::hid::watch::HidEvent;
use crate::notify::{self, LowAlert, LowFlags};
use crate::state::{Link, Machine, Obs, Transition};
use crate::switch::{self, Connect, Current, Outcome, Skip};

/// `D` は再生デバイスの操作。通常は Core Audio（`Audio`）で、シナリオテストではメモリ上の偽物に差し替える
pub struct App<D: Devices = Audio> {
    config: Config,
    config_path: PathBuf,
    audio: D,
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
    /// `Transition::Lost` のときは空
    pub outcomes: Vec<Outcome>,
}

impl App {
    pub fn new(config_path: PathBuf) -> Result<Self> {
        Ok(Self::with_devices(config_path, Audio::new()?))
    }
}

impl<D: Devices> App<D> {
    pub fn with_devices(config_path: PathBuf, audio: D) -> Self {
        let config = config::load(&config_path);
        let machine = Machine::new(Duration::from_millis(config.debounce_ms));
        Self {
            audio,
            config,
            config_path,
            machine,
            dongle: false,
            battery: None,
            low: LowFlags::default(),
            low_alert: None,
        }
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
            Transition::Changed { connected } => self.switch(connected, false),
            Transition::Established { connected } => self.switch(connected, true),
            Transition::Lost => Vec::new(),
        };
        Update { transition, outcomes }
    }

    /// `established` は Unknown から決まったとき（docs/design.md 4.2）
    fn switch(&mut self, connected: bool, established: bool) -> Vec<Outcome> {
        let (endpoints, default, communications) = match self.snapshot() {
            Ok(v) => v,
            Err(e) => return vec![Outcome::Failed(format!("再生デバイスを取得できません: {e:#}"))],
        };
        let current = Current { default: default.as_deref(), communications: communications.as_deref() };
        let plan = if connected {
            let c = &self.config;
            if c.auto_switch && c.switch_communications && find_kind(&endpoints, DongleKind::Chat).is_none() {
                tracing::warn!("Chat のデバイスが見つからないため、既定の通信デバイスは切り替えません");
            }
            if established {
                switch::on_established_connect(c, &endpoints, current)
            } else {
                switch::on_connect(c, &endpoints, current)
            }
        } else {
            switch::on_disconnect(&self.config, &endpoints, current).map(|to| Connect { to, remember: None })
        };
        let mut outcomes = Vec::new();
        match plan.default {
            Err(skip) => outcomes.push(Outcome::Skipped(skip)),
            Ok(Connect { to, remember }) => {
                if let Some(dev) = remember {
                    self.update_config(|c| c.disconnected_device = Some(dev.clone()));
                    outcomes.push(Outcome::Remembered(dev));
                }
                outcomes.push(match self.audio.set_default(&to.id) {
                    Ok(()) if connected => Outcome::Switched(to),
                    Ok(()) => Outcome::Restored(to),
                    Err(e) => Outcome::Failed(format!("{} に切り替えられません: {e:#}", to.name)),
                });
            }
        }
        if let Some(to) = plan.communications {
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

    /// 再生デバイスの一覧と、既定の再生デバイス・既定の通信デバイスの ID
    fn snapshot(&self) -> Result<(Vec<Endpoint>, Option<String>, Option<String>)> {
        let (endpoints, default) = self.endpoints()?;
        Ok((endpoints, default, self.audio.communications_id()?))
    }
}

impl Update {
    pub fn log(&self) {
        match self.transition {
            Transition::Established { connected } => tracing::info!("状態確定: {}", link_label(connected)),
            Transition::Lost => tracing::info!("状態不明（ドングルなし）"),
            Transition::Changed { connected } => tracing::info!("状態変化: {}", link_label(connected)),
        }
        for o in &self.outcomes {
            match o {
                Outcome::Switched(d) => tracing::info!("{} に切り替えました", d.name),
                Outcome::Restored(d) => tracing::info!("{} に戻しました", d.name),
                Outcome::Remembered(d) => tracing::info!("切断時の出力を {} に設定しました", d.name),
                Outcome::Skipped(Skip::Disabled) => tracing::info!("自動切り替えがオフ"),
                Outcome::Skipped(Skip::UserChanged) => {
                    tracing::info!("既定デバイスが手動で変えられているため戻さない")
                }
                Outcome::Skipped(Skip::OnDongle) => {
                    tracing::info!("既定デバイスがすでにドングルのデバイスのため切り替えない")
                }
                Outcome::Skipped(Skip::NoGameDevice) => {
                    tracing::warn!("INZONE Buds - Game が見つからないため切り替えない")
                }
                Outcome::Skipped(Skip::NotConfigured) => tracing::warn!("切断時の出力が未設定です"),
                Outcome::Skipped(Skip::Missing(d)) => tracing::warn!("{}（{}）が見つかりません", d.name, d.id),
                Outcome::Failed(e) => tracing::warn!("出力を切り替えられませんでした: {e}"),
            }
        }
    }
}

fn link_label(connected: bool) -> &'static str {
    if connected { "接続" } else { "切断" }
}

#[cfg(test)]
mod scenarios;
