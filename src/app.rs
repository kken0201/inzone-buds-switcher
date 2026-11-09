//! HID イベントを状態機械に渡し、確定した変化に応じて出力を切り替える。
//! COM を使うので、`ComGuard` を初期化したスレッドで使う。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::audio::Audio;
use crate::config::{self, Config, DeviceRef};
use crate::hid::watch::HidEvent;
use crate::state::{Machine, Obs, Transition};
use crate::switch::{self, Outcome};

pub struct App {
    config: Config,
    config_path: PathBuf,
    audio: Audio,
    machine: Machine,
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
        Ok(Self { audio: Audio::new()?, config, config_path, machine })
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.machine.deadline()
    }

    pub fn handle(&mut self, ev: HidEvent, now: Instant) -> Option<Update> {
        let obs = match ev {
            // 差し直した直後は Unknown のままで、イヤホンの状態が届くまで待つ
            HidEvent::DongleFound => return None,
            HidEvent::DongleLost => Obs::DongleLost,
            HidEvent::Earbuds(e) => Obs::Earbuds(e),
            HidEvent::Battery(b) => Obs::Earbuds(b.earbuds()),
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
        let (endpoints, current) = match self.audio.list().and_then(|l| Ok((l, self.audio.default_id()?))) {
            Ok(v) => v,
            Err(e) => return vec![Outcome::Failed(format!("再生デバイスを取得できません: {e:#}"))],
        };
        let current = current.as_deref();
        let mut outcomes = Vec::new();
        let (to, done): (DeviceRef, fn(DeviceRef) -> Outcome) = if connected {
            match switch::on_connect(&self.config, &endpoints, current) {
                Ok(c) => {
                    if let Some(dev) = c.remember {
                        self.remember(dev.clone());
                        outcomes.push(Outcome::Remembered(dev));
                    }
                    (c.to, Outcome::Switched)
                }
                Err(skip) => return vec![Outcome::Skipped(skip)],
            }
        } else {
            match switch::on_disconnect(&self.config, &endpoints, current) {
                Ok(to) => (to, Outcome::Restored),
                Err(skip) => return vec![Outcome::Skipped(skip)],
            }
        };
        outcomes.push(match self.audio.set_default(&to.id, self.config.switch_communications) {
            Ok(()) => done(to),
            Err(e) => Outcome::Failed(format!("{} に切り替えられません: {e:#}", to.name)),
        });
        outcomes
    }

    /// 保存に失敗してもメモリ上の設定は使う。この起動中の切断では戻せるようにするため
    fn remember(&mut self, dev: DeviceRef) {
        self.config.disconnected_device = Some(dev);
        if let Err(e) = config::save(&self.config_path, &self.config) {
            tracing::warn!("設定を保存できません: {e:#}");
        }
    }
}
