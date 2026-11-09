//! イヤホンの接続状態の状態機械（docs/design.md 4.2）。時刻を引数で受け取る純粋なロジック。

use std::time::{Duration, Instant};

use crate::hid::protocol::Earbuds;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    Unknown,
    Disconnected,
    Connected,
}

impl Link {
    fn known(connected: bool) -> Self {
        if connected { Self::Connected } else { Self::Disconnected }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Obs {
    /// 状態レポート・接続問い合わせの応答・バッテリーレポートから得た左右の接続
    Earbuds(Earbuds),
    DongleLost,
}

/// 確定した状態の変化。出力を切り替えてよいのは `Changed` だけ
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    /// Unknown から状態が決まった（起動直後・ドングルの差し直し）。出力は変えない
    Established { connected: bool },
    /// ドングルを失って Unknown になった
    Lost,
    /// Disconnected と Connected の間の変化
    Changed { connected: bool },
}

pub struct Machine {
    debounce: Duration,
    link: Link,
    /// 確定待ちの接続状態と、それが観測され始めた時刻
    pending: Option<(bool, Instant)>,
}

impl Machine {
    pub fn new(debounce: Duration) -> Self {
        Self { debounce, link: Link::Unknown, pending: None }
    }

    pub fn link(&self) -> Link {
        self.link
    }

    /// 次に `poll` を呼ぶべき時刻。確定待ちがなければ None
    pub fn deadline(&self) -> Option<Instant> {
        self.pending.map(|(_, since)| since + self.debounce)
    }

    pub fn observe(&mut self, obs: Obs, now: Instant) -> Option<Transition> {
        match obs {
            Obs::DongleLost => {
                self.pending = None;
                let was = std::mem::replace(&mut self.link, Link::Unknown);
                (was != Link::Unknown).then_some(Transition::Lost)
            }
            Obs::Earbuds(e) => {
                let connected = e.any();
                if self.link == Link::known(connected) {
                    self.pending = None;
                } else if !matches!(self.pending, Some((c, _)) if c == connected) {
                    self.pending = Some((connected, now));
                }
                self.poll(now)
            }
        }
    }

    pub fn poll(&mut self, now: Instant) -> Option<Transition> {
        let (connected, _) = self.pending?;
        if now < self.deadline()? {
            return None;
        }
        self.pending = None;
        let was = std::mem::replace(&mut self.link, Link::known(connected));
        Some(if was == Link::Unknown {
            Transition::Established { connected }
        } else {
            Transition::Changed { connected }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEBOUNCE: Duration = Duration::from_millis(1500);

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn buds(left: bool, right: bool) -> Obs {
        Obs::Earbuds(Earbuds { left, right })
    }

    /// `connected` が確定した状態の機械と、確定した時刻
    fn settled(connected: bool) -> (Machine, Instant) {
        let t0 = Instant::now();
        let mut m = Machine::new(DEBOUNCE);
        m.observe(buds(connected, false), t0);
        let t = t0 + DEBOUNCE;
        assert_eq!(m.poll(t), Some(Transition::Established { connected }));
        (m, t)
    }

    #[test]
    fn starts_unknown_without_deadline() {
        let m = Machine::new(DEBOUNCE);
        assert_eq!(m.link, Link::Unknown);
        assert_eq!(m.deadline(), None);
    }

    #[test]
    fn leaving_unknown_is_established_not_changed() {
        let t0 = Instant::now();
        let mut m = Machine::new(DEBOUNCE);
        assert_eq!(m.observe(buds(true, true), t0), None);
        assert_eq!(m.link, Link::Unknown);
        assert_eq!(m.deadline(), Some(t0 + DEBOUNCE));
        assert_eq!(m.poll(t0 + ms(1499)), None);
        assert_eq!(m.poll(t0 + DEBOUNCE), Some(Transition::Established { connected: true }));
        assert_eq!(m.link, Link::Connected);
        assert_eq!(m.deadline(), None);
    }

    #[test]
    fn unknown_to_disconnected_is_established() {
        let (m, _) = settled(false);
        assert_eq!(m.link, Link::Disconnected);
    }

    #[test]
    fn connect_after_debounce_is_changed() {
        let (mut m, t) = settled(false);
        assert_eq!(m.observe(buds(true, false), t), None);
        assert_eq!(m.poll(t + DEBOUNCE), Some(Transition::Changed { connected: true }));
        assert_eq!(m.link, Link::Connected);
    }

    #[test]
    fn disconnect_after_debounce_is_changed() {
        let (mut m, t) = settled(true);
        assert_eq!(m.observe(buds(false, false), t), None);
        assert_eq!(m.poll(t + DEBOUNCE), Some(Transition::Changed { connected: false }));
        assert_eq!(m.link, Link::Disconnected);
    }

    #[test]
    fn one_earbud_counts_as_connected() {
        let (mut m, t) = settled(false);
        m.observe(buds(false, true), t);
        assert_eq!(m.poll(t + DEBOUNCE), Some(Transition::Changed { connected: true }));
    }

    #[test]
    fn flicker_shorter_than_debounce_is_ignored() {
        let (mut m, t) = settled(true);
        assert_eq!(m.observe(buds(false, false), t), None);
        assert_eq!(m.observe(buds(true, false), t + ms(1000)), None);
        assert_eq!(m.deadline(), None);
        assert_eq!(m.poll(t + ms(5000)), None);
        assert_eq!(m.link, Link::Connected);
    }

    #[test]
    fn taking_out_second_earbud_does_not_restart_debounce() {
        let (mut m, t) = settled(false);
        m.observe(buds(true, false), t);
        assert_eq!(m.observe(buds(true, true), t + ms(1000)), None);
        assert_eq!(m.deadline(), Some(t + DEBOUNCE));
        assert_eq!(m.poll(t + DEBOUNCE), Some(Transition::Changed { connected: true }));
    }

    #[test]
    fn observation_past_deadline_confirms_without_poll() {
        let (mut m, t) = settled(false);
        m.observe(buds(true, false), t);
        assert_eq!(
            m.observe(buds(true, false), t + ms(2000)),
            Some(Transition::Changed { connected: true })
        );
    }

    #[test]
    fn dongle_lost_is_immediate_and_cancels_pending() {
        let (mut m, t) = settled(true);
        m.observe(buds(false, false), t);
        assert_eq!(m.observe(Obs::DongleLost, t + ms(10)), Some(Transition::Lost));
        assert_eq!(m.link, Link::Unknown);
        assert_eq!(m.deadline(), None);
        assert_eq!(m.poll(t + ms(5000)), None);
    }

    #[test]
    fn dongle_lost_while_unknown_reports_nothing() {
        let t0 = Instant::now();
        let mut m = Machine::new(DEBOUNCE);
        m.observe(buds(true, false), t0);
        assert_eq!(m.observe(Obs::DongleLost, t0), None);
        assert_eq!(m.deadline(), None);
    }

    #[test]
    fn state_after_dongle_replug_is_established_not_changed() {
        let (mut m, t) = settled(true);
        m.observe(Obs::DongleLost, t);
        m.observe(buds(false, false), t + ms(100));
        assert_eq!(
            m.poll(t + ms(100) + DEBOUNCE),
            Some(Transition::Established { connected: false })
        );
    }
}
