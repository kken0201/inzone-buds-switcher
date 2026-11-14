//! 出力の切り替えのモデル検査（`.claude/skills/verify-output-alignment/SKILL.md`）。
//!
//! 本物の `App`（状態機械 `state::Machine` と切り替えの判断 `switch` を含む）を、
//! 周りの世界を模したモデルの上で動かし、起こりうるイベントの順序を有限の範囲ですべて試す。
//! 模すのは次のもの。それぞれの対応するコードはコメントに書く。
//!
//! - イヤホンの出し入れとドングルの抜き差し（物理）
//! - HID 監視スレッド（`hid/watch.rs` の `run` / `session`）がチャネルに送る `HidEvent`
//! - メインスレッド（`tray/mod.rs` の `Tray::drain` / `Tray::tick` / `restart_if_idle`）
//! - Windows の既定の出力（ドングルを抜くと別のデバイスに移る・挿すと Game に移ることがある）
//! - 利用者による手動の切り替え
//!
//! 時刻はデバウンスの確定時刻を過ぎたかどうかだけを区別する。`App` は複製できないので、
//! 状態（`Key`）から同じ状態の `App` を組み立て直す（`Sim::from_key`）。
//! そうして作った `App` が、初期状態から操作の列をやり直した `App` と同じ状態になることは
//! `key_captures_app_state` で確かめる。
//!
//! 確かめる性質：
//! - 安全性（すべての状態）：出力を変えるのは状態が確定したときだけで、向きが状態と合う。
//!   Connected に決まったときに、ドングルのデバイスを向いている出力を上書きしない。
//!   切断したときに、ドングル以外を向いている出力を上書きしない
//! - 収束（`aligned`）：外からの操作が止まったら、メインスレッドと監視スレッドの処理だけで、
//!   接続状態がイヤホンの実際の状態に一致し、既定の出力がそれに合ったデバイスに落ち着く

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use super::{App, Update};
use crate::audio::{Devices, DongleKind, Endpoint};
use crate::config::{self, Config};
use crate::hid::protocol::{Battery, Earbuds};
use crate::hid::watch::HidEvent;
use crate::notify::LowFlags;
use crate::state::{Link, Machine, Obs, Transition};
use crate::testutil::{dev, ep, temp_dir};

const SPEAKER: &str = "speaker";
const MONITOR: &str = "monitor";
const CHAT: &str = "chat";
const GAME: &str = "game";

fn is_dongle(id: &str) -> bool {
    id == CHAT || id == GAME
}

/// 最後に既定の出力を変えたもの
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Writer {
    /// 起動したときのまま
    Initial,
    App,
    User,
    /// ドングルの抜き差しに伴う Windows の自動の切り替え
    Windows,
}

/// 再生デバイスの側（Core Audio を模す）
struct World {
    dongle: bool,
    default: Option<&'static str>,
    writer: Writer,
    /// `App` が `set_default` に成功した回数
    app_writes: usize,
}

impl World {
    fn present(&self) -> Vec<&'static str> {
        let mut v = vec![SPEAKER, MONITOR];
        if self.dongle {
            v.extend([CHAT, GAME]);
        }
        v
    }
}

struct Fake(Rc<RefCell<World>>);

impl Devices for Fake {
    fn list(&self) -> Result<Vec<Endpoint>> {
        Ok(self
            .0
            .borrow()
            .present()
            .into_iter()
            .map(|id| {
                let kind = match id {
                    CHAT => Some(DongleKind::Chat),
                    GAME => Some(DongleKind::Game),
                    _ => None,
                };
                ep(id, kind)
            })
            .collect())
    }

    fn default_id(&self) -> Result<Option<String>> {
        Ok(self.0.borrow().default.map(String::from))
    }

    fn communications_id(&self) -> Result<Option<String>> {
        // switch_communications は既定でオフなので、通信デバイスはモデルに入れない
        Ok(None)
    }

    fn set_default(&self, id: &str) -> Result<()> {
        let mut w = self.0.borrow_mut();
        let Some(id) = w.present().into_iter().find(|p| *p == id) else {
            bail!("{id} はない");
        };
        w.default = Some(id);
        w.writer = Writer::App;
        w.app_writes += 1;
        Ok(())
    }

    fn set_communications(&self, _: &str) -> Result<()> {
        bail!("モデルでは通信デバイスを扱わない")
    }
}

/// チャネル（監視スレッド → メインスレッド）に入っている `HidEvent`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Ev {
    Found,
    Lost,
    /// `12 01` / `14 02`、または起動直後の無応答による「両耳ともケース内」
    Buds(bool),
    /// `14 04`。イヤホンが出ているときだけ届く
    Battery,
}

impl Ev {
    fn to_hid(self) -> HidEvent {
        match self {
            Ev::Found => HidEvent::DongleFound,
            Ev::Lost => HidEvent::DongleLost,
            Ev::Buds(out) => HidEvent::Earbuds(Earbuds { left: out, right: false }),
            Ev::Battery => HidEvent::Battery(Battery { left: Some(80), right: None, case: Some(50) }),
        }
    }
}

/// 監視スレッド（`hid/watch.rs`）の状態
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Hid {
    /// ドングルを開いていない（`run` の再列挙待ち）
    Closed,
    /// `session` の中。`silence` は `silence_deadline` が残っていること。
    /// `stalled` は送信が戻らず、問い合わせに応答が来ないこと
    Open { silence: bool, stalled: bool },
    /// ドングルは抜かれたが、まだ読み取りの失敗に気づいていない
    Stale,
}

/// 外からの操作の残り回数。探索を有限にするための上限
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Budget {
    buds: u8,
    unplug: u8,
    plug: u8,
    user: u8,
    restart: u8,
}

const BUDGET: Budget = Budget { buds: 3, unplug: 1, plug: 1, user: 1, restart: 1 };
/// `key_captures_app_state` の上限。初期状態からやり直すので小さくする
const SMALL: Budget = Budget { buds: 2, unplug: 1, plug: 1, user: 0, restart: 1 };
/// 外からの操作でチャネルに積める数の上限
const CHANNEL_CAP: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Env {
    /// どちらかのイヤホンがケースの外にある
    out: bool,
    dongle: bool,
    hid: Hid,
    chan: VecDeque<Ev>,
    /// 更新後の再起動のメッセージ（`WM_RESTART`）が投げられたが、まだ処理されていない
    restart_posted: bool,
    budget: Budget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Act {
    // ---- 外からの操作 ----
    /// イヤホンを出す/しまう。`drop` は自発送信のレポートが届かない（protocol.md 1 章）
    Buds { drop: bool },
    /// ドングルを抜く。既定の出力がドングルのデバイスなら Windows が `fallback` に移す
    Unplug { fallback: &'static str },
    /// ドングルを挿す。`to_game` なら Windows が既定の出力を Game に移す
    Plug { to_game: bool },
    /// 利用者が既定の出力を手で変える
    User(&'static str),
    /// 自動更新が置き換えを終え、`restart_if_idle` が再起動のメッセージを投げる
    PostRestart,
    // ---- メインスレッドと監視スレッドの処理 ----
    /// `Tray::drain` が 1 件 `App::handle` する
    Drain,
    /// デバウンスの確定時刻になる
    Deadline,
    /// `WM_TIMER` で `Tray::tick` が `App::tick` する
    Tick,
    /// `session` の起動直後、`41 02` に応答がないまま `SILENCE_TIMEOUT` が過ぎる
    Silence,
    /// 30 秒ごとの `41 04`。イヤホンが出ていれば応答が届く
    Poll,
    /// 読み取りが失敗し、`run` が `DongleLost` を送る
    NoticeLoss,
    /// `run` がドングルを見つけて `session` を始める。`link_ok` は `41 02` の送信が成功したこと。
    /// 失敗したら、その `session` の間は問い合わせに応答が来ない
    Open { link_ok: bool },
    /// `WM_RESTART` を処理して起動し直す（新しいプロセスは設定ファイルを読み直す）
    Restart,
}

impl Act {
    fn internal(self) -> bool {
        !matches!(self, Act::Buds { .. } | Act::Unplug { .. } | Act::Plug { .. } | Act::User(_) | Act::PostRestart)
    }

    fn describe(self) -> String {
        match self {
            Act::Buds { drop: false } => "イヤホンを出し入れする".into(),
            Act::Buds { drop: true } => "イヤホンを出し入れする（自発送信のレポートが届かない）".into(),
            Act::Unplug { fallback } => format!("ドングルを抜く（Windows が既定の出力を {fallback} に移す）"),
            Act::Plug { to_game: true } => "ドングルを挿す（Windows が既定の出力を game に移す）".into(),
            Act::Plug { to_game: false } => "ドングルを挿す".into(),
            Act::User(id) => format!("利用者が既定の出力を {id} にする"),
            Act::PostRestart => "更新を終え、再起動のメッセージが投げられる".into(),
            Act::Drain => "メインスレッドがイベントを 1 件 App::handle する".into(),
            Act::Deadline => "デバウンスの確定時刻になる".into(),
            Act::Tick => "WM_TIMER で App::tick する".into(),
            Act::Silence => "41 02 に応答がなく、監視スレッドが「両耳ともケース内」を送る".into(),
            Act::Poll => "41 04 の応答（バッテリー）が届く".into(),
            Act::NoticeLoss => "監視スレッドが読み取りの失敗に気づき DongleLost を送る".into(),
            Act::Open { link_ok: true } => "監視スレッドがドングルを開き、41 02 を送る".into(),
            Act::Open { link_ok: false } => "監視スレッドがドングルを開くが、41 02 の送信がタイムアウトする".into(),
            Act::Restart => "起動し直す".into(),
        }
    }
}

/// 探索に入れる通信の欠落。protocol.md で別々の時期に観測したので、別々に入れる
#[derive(Debug, Clone, Copy)]
struct Scope {
    budget: Budget,
    /// 自発送信のレポートが届かない（protocol.md 1 章）。問い合わせには応答する
    drop_reports: bool,
    /// 問い合わせの送信が戻らない（protocol.md 7 章）。自発送信は届く
    stall_writes: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Init {
    budget: Budget,
    out: bool,
    dongle: bool,
    default: &'static str,
}

static RUN: AtomicUsize = AtomicUsize::new(0);

/// 設定ファイルの置き場所。切断時の出力が設定済みの状態は、あらかじめ書いたファイルを読ませる
struct Files {
    dir: PathBuf,
}

impl Files {
    fn new(name: &str) -> Self {
        let dir = temp_dir(&format!("model-{name}"));
        for id in [SPEAKER, MONITOR] {
            let c = Config { disconnected_device: Some(dev(id)), ..Config::default() };
            config::save(&dir.join(format!("{id}.json")), &c).unwrap();
        }
        Self { dir }
    }

    fn path(&self, ret: Option<&str>) -> PathBuf {
        match ret {
            // 存在しないファイル。自動で覚えて保存するまで、切断時の出力は未設定
            None => self.dir.join(format!("run-{}.json", RUN.fetch_add(1, Ordering::Relaxed))),
            // 未設定のときしか保存しないので、共有しても書き換わらない
            Some(id) => {
                assert!(id == SPEAKER || id == MONITOR, "ドングルのデバイスを切断時の出力にした: {id}");
                self.dir.join(format!("{id}.json"))
            }
        }
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Sim {
    app: App<Fake>,
    world: Rc<RefCell<World>>,
    env: Env,
    now: Instant,
    path: PathBuf,
    /// 確定待ちの値。`Machine` は公開していないので、観測した値から追う（`observe` のあとの確定待ちは、必ず最後の観測値）
    last_obs: Option<bool>,
}

impl Sim {
    /// 起動した直後。設定ファイルはなく、切断時の出力は未設定
    fn new(init: Init, files: &Files) -> Self {
        let path = files.path(None);
        let world = Rc::new(RefCell::new(World {
            dongle: init.dongle,
            default: Some(init.default),
            writer: Writer::Initial,
            app_writes: 0,
        }));
        let app = App::with_devices(path.clone(), Fake(world.clone()));
        Self {
            app,
            world,
            env: Env {
                out: init.out,
                dongle: init.dongle,
                hid: Hid::Closed,
                chan: VecDeque::new(),
                restart_posted: false,
                budget: init.budget,
            },
            now: Instant::now(),
            path,
            last_obs: None,
        }
    }

    fn replay(init: Init, acts: &[Act], files: &Files) -> Self {
        let mut s = Self::new(init, files);
        for &a in acts {
            s.step(a).expect("やり直しの途中で安全性が破れた");
        }
        s
    }

    /// `key` の状態の `App` を組み立てる。`App` が持つ状態のうち、切り替えに効くのは
    /// 設定・接続状態・確定待ち（値と、確定時刻を過ぎたか）・ドングルの有無だけなので、それを作る。
    /// 作れなければ、`App` がモデルの想定しない状態になったので、その説明を返す
    fn try_from_key(key: &Key, files: &Files) -> Result<Self, String> {
        let fail = |what: &str| Err(format!("App の状態を組み立て直せない（{what}）: {key:?}"));
        let config = Config { disconnected_device: key.ret.as_deref().map(dev), ..Config::default() };
        let debounce = Duration::from_millis(config.debounce_ms);
        let base = Instant::now();
        let now = base + 10 * debounce;
        // 状態機械は観測を渡して作る。`App` を通さないので、組み立てる間に出力を変えない
        let mut machine = Machine::new(debounce);
        let link = link_of(key);
        if link != Link::Unknown {
            if !key.app_dongle {
                return fail("ドングルがないのに接続状態が決まっている");
            }
            let c = link == Link::Connected;
            if machine.observe(Obs::Earbuds(Earbuds { left: c, right: false }), base).is_some() {
                return fail("デバウンスの前に確定した");
            }
            if machine.poll(base + debounce) != Some(Transition::Established { connected: c }) {
                return fail("Unknown から確定しない");
            }
        }
        if let Some(c) = key.pending {
            let at = if key.expired { now - debounce } else { now };
            if machine.observe(Obs::Earbuds(Earbuds { left: c, right: false }), at).is_some() {
                return fail("確定待ちを作れない");
            }
        }
        let world = Rc::new(RefCell::new(World {
            dongle: key.env.dongle,
            default: key.default,
            writer: if key.by_user { Writer::User } else { Writer::App },
            app_writes: 0,
        }));
        // `App::with_devices` は設定ファイルを読むので使わず、フィールドを直接埋める（ファイル I/O をなくして速くするため）。
        // 再起動（`Act::Restart`）では `with_devices` で読み直すので、`path` には同じ設定が入っている
        let path = files.path(key.ret.as_deref());
        let app = App {
            config,
            config_path: path.clone(),
            audio: Fake(world.clone()),
            machine,
            dongle: key.app_dongle,
            battery: None,
            low: LowFlags::default(),
            low_alert: None,
        };
        let sim = Self { app, world, env: key.env.clone(), now, path, last_obs: key.pending };
        if &sim.key() != key {
            return fail(&format!("組み立てた状態が {:?}", sim.key()));
        }
        Ok(sim)
    }

    /// `explore` は、状態を見つけたときに組み立て直せることを確かめてから広げる
    fn from_key(key: &Key, files: &Files) -> Self {
        Self::try_from_key(key, files).expect("確かめたはずの状態を組み立て直せない")
    }

    fn enabled(&self, scope: Scope) -> Vec<Act> {
        let e = &self.env;
        let w = self.world.borrow();
        let mut v = Vec::new();
        let room = e.chan.len() < CHANNEL_CAP;
        if e.budget.buds > 0 && room {
            v.push(Act::Buds { drop: false });
            if scope.drop_reports {
                v.push(Act::Buds { drop: true });
            }
        }
        if e.dongle && e.budget.unplug > 0 && room {
            for fallback in [SPEAKER, MONITOR] {
                v.push(Act::Unplug { fallback });
                if !w.default.is_some_and(is_dongle) {
                    break; // 移さないので 1 通りでよい
                }
            }
        }
        if !e.dongle && e.budget.plug > 0 {
            v.push(Act::Plug { to_game: false });
            v.push(Act::Plug { to_game: true });
        }
        if e.budget.user > 0 {
            v.extend(w.present().into_iter().filter(|id| Some(*id) != w.default).map(Act::User));
        }
        if !e.restart_posted && e.budget.restart > 0 && self.app.deadline().is_none() {
            v.push(Act::PostRestart);
        }
        if !e.chan.is_empty() {
            v.push(Act::Drain);
        }
        match self.app.deadline() {
            Some(d) if self.now < d => v.push(Act::Deadline),
            Some(_) => v.push(Act::Tick),
            None => {}
        }
        match e.hid {
            Hid::Open { silence, stalled } => {
                if silence {
                    v.push(Act::Silence);
                }
                // 30 秒に 1 回なので、前のイベントはすでに処理されている
                if e.out && !stalled && e.chan.is_empty() {
                    v.push(Act::Poll);
                }
            }
            Hid::Stale => v.push(Act::NoticeLoss),
            Hid::Closed if e.dongle => {
                v.push(Act::Open { link_ok: true });
                if scope.stall_writes {
                    v.push(Act::Open { link_ok: false });
                }
            }
            Hid::Closed => {}
        }
        if e.restart_posted {
            v.push(Act::Restart);
        }
        v
    }

    /// 監視スレッドが `HidEvent` を送る。`session` は送るたびに `silence_deadline` を消す
    fn send(&mut self, ev: Ev) {
        if let Hid::Open { silence, .. } = &mut self.env.hid {
            *silence = false;
        }
        self.env.chan.push_back(ev);
    }

    /// 1 歩進める。安全性が破れたら、その説明を返す
    fn step(&mut self, act: Act) -> Result<(), String> {
        match act {
            Act::Buds { drop } => {
                self.env.budget.buds -= 1;
                self.env.out = !self.env.out;
                // レポートは開いているハンドルにだけ届く
                if !drop && self.env.dongle && matches!(self.env.hid, Hid::Open { .. }) {
                    self.send(Ev::Buds(self.env.out));
                }
            }
            Act::Unplug { fallback } => {
                self.env.budget.unplug -= 1;
                self.env.dongle = false;
                if matches!(self.env.hid, Hid::Open { .. }) {
                    self.env.hid = Hid::Stale;
                }
                let mut w = self.world.borrow_mut();
                w.dongle = false;
                if w.default.is_some_and(is_dongle) {
                    w.default = Some(fallback);
                    w.writer = Writer::Windows;
                }
            }
            Act::Plug { to_game } => {
                self.env.budget.plug -= 1;
                self.env.dongle = true;
                let mut w = self.world.borrow_mut();
                w.dongle = true;
                if to_game {
                    w.default = Some(GAME);
                    w.writer = Writer::Windows;
                }
            }
            Act::User(id) => {
                self.env.budget.user -= 1;
                let mut w = self.world.borrow_mut();
                w.default = Some(id);
                w.writer = Writer::User;
            }
            Act::PostRestart => {
                self.env.budget.restart -= 1;
                self.env.restart_posted = true;
            }
            Act::Drain => {
                let ev = self.env.chan.pop_front().unwrap();
                match ev {
                    Ev::Buds(c) => self.last_obs = Some(c),
                    Ev::Battery => self.last_obs = Some(true),
                    Ev::Found | Ev::Lost => {}
                }
                let now = self.now;
                return self.call(|app| app.handle(ev.to_hid(), now));
            }
            Act::Deadline => self.now = self.app.deadline().unwrap(),
            Act::Tick => {
                let now = self.now;
                return self.call(|app| app.tick(now));
            }
            Act::Silence => self.send(Ev::Buds(false)),
            Act::Poll => self.send(Ev::Battery),
            Act::NoticeLoss => {
                self.env.hid = Hid::Closed;
                self.env.chan.push_back(Ev::Lost);
            }
            Act::Open { link_ok } => {
                // 応答は 0.2〜0.4 秒で届くので、送信と応答の間のイヤホンの出し入れは入れない
                self.env.chan.push_back(Ev::Found);
                self.env.hid = Hid::Open { silence: link_ok && !self.env.out, stalled: !link_ok };
                if link_ok && self.env.out {
                    self.send(Ev::Buds(true));
                }
            }
            Act::Restart => {
                self.env.restart_posted = false;
                self.env.chan.clear();
                self.env.hid = Hid::Closed;
                self.last_obs = None;
                self.app = App::with_devices(self.path.clone(), Fake(self.world.clone()));
            }
        }
        Ok(())
    }

    /// `App` を呼び、出力の変え方が安全性を満たすか確かめる
    fn call(&mut self, f: impl FnOnce(&mut App<Fake>) -> Option<Update>) -> Result<(), String> {
        let (before, writes) = {
            let w = self.world.borrow();
            (w.default, w.app_writes)
        };
        let update = f(&mut self.app);
        let after = self.world.borrow().default;
        let wrote = self.world.borrow().app_writes != writes;
        if !wrote {
            return Ok(());
        }
        let after = after.unwrap();
        let Some(u) = update else {
            return Err(format!("状態が確定していないのに出力を {after} に変えた"));
        };
        let before_dongle = before.is_some_and(is_dongle);
        match u.transition {
            Transition::Lost => Err(format!("ドングルを失ったときに出力を {after} に変えた")),
            Transition::Changed { connected } | Transition::Established { connected }
                if connected != is_dongle(after) =>
            {
                Err(format!("{:?} なのに出力を {after} に変えた", u.transition))
            }
            Transition::Established { connected: true } if before_dongle => {
                Err(format!("Connected に決まったとき、ドングルのデバイス {before:?} を {after} で上書きした"))
            }
            Transition::Changed { connected: false } | Transition::Established { connected: false }
                if !before_dongle =>
            {
                Err(format!("切断したとき、ドングル以外の {before:?} を {after} で上書きした"))
            }
            _ => Ok(()),
        }
    }

    fn key(&self) -> Key {
        let w = self.world.borrow();
        let deadline = self.app.deadline();
        let pending = deadline.map(|_| self.last_obs.expect("確定待ちがあるのに観測がない"));
        Key {
            env: self.env.clone(),
            default: w.default,
            by_user: w.writer == Writer::User,
            ret: self.app.config().disconnected_device.as_ref().map(|d| d.id.clone()),
            link: self.app.link() as u8,
            app_dongle: self.app.dongle(),
            pending,
            expired: deadline.is_some_and(|d| self.now >= d),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    env: Env,
    default: Option<&'static str>,
    /// 最後に既定の出力を変えたのが利用者。それ以外（App・Windows・起動時のまま）は区別しなくても振る舞いが同じ
    by_user: bool,
    ret: Option<String>,
    link: u8,
    app_dongle: bool,
    pending: Option<bool>,
    expired: bool,
}

fn link_of(k: &Key) -> Link {
    [Link::Unknown, Link::Disconnected, Link::Connected][k.link as usize]
}

/// 落ち着いた状態が満たすべきこと。満たさなければ理由を返す
fn aligned(k: &Key) -> Result<(), String> {
    let e = &k.env;
    let want = if !e.dongle {
        Link::Unknown
    } else if e.out {
        Link::Connected
    } else {
        Link::Disconnected
    };
    if link_of(k) != want {
        return Err(format!(
            "接続状態が {:?} のまま（実際は {want:?}。既定の出力 {:?}・切断時の出力 {:?}）",
            link_of(k),
            k.default,
            k.ret
        ));
    }
    if !e.dongle || k.by_user {
        return Ok(()); // 利用者が最後に選んだ出力は尊重する
    }
    let on_dongle = k.default.is_some_and(is_dongle);
    if e.out && !on_dongle {
        return Err(format!("イヤホンが出ているのに既定の出力が {:?} のまま", k.default));
    }
    // 切断時の出力が未設定なら戻せない（⚠ の通知で知らせる。docs/design.md 4.3）
    if !e.out && on_dongle && k.ret.is_some() {
        return Err(format!("イヤホンをしまったのに既定の出力が {:?} のまま", k.default));
    }
    Ok(())
}

struct Node {
    key: Key,
    init: Init,
    parent: Option<(usize, Act)>,
    /// まだ試していない操作。組み立て直す回数を減らすため、見つけたときに求めておく
    todo: Vec<Act>,
    internal: Vec<usize>,
}

/// 現在のコードで見つかっている、収束しない状態の種類（`.claude/skills/verify-output-alignment/SKILL.md`）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Finding {
    /// 接続中に両耳をケースに戻したときの `12 01` を取りこぼすと、Connected のまま戻らない。
    /// 両耳がケース内だと `41 04` に応答がないので、定期的な問い合わせでは追いつけない
    MissedCaseIn,
    /// ドングルを開いたときに `41 02` の送信がタイムアウトすると、イヤホンを出し入れするまで Unknown のまま
    StalledAtOpen,
}

fn finding(k: &Key) -> Option<Finding> {
    let e = &k.env;
    match (link_of(k), e.hid) {
        (Link::Connected, Hid::Open { stalled: false, .. }) if e.dongle && !e.out => Some(Finding::MissedCaseIn),
        (Link::Unknown, Hid::Open { stalled: true, .. }) if e.dongle => Some(Finding::StalledAtOpen),
        _ => None,
    }
}

struct Report {
    states: usize,
    safety: Vec<String>,
    /// 組み立て直せない状態に着いた手順。`Key` に足りない状態があるか、`App` の状態機械が変わった
    unmodeled: Vec<String>,
    /// 収束しない状態の集まりごとに、既知の種類（どれでもなければ None）と説明
    stuck: Vec<(Option<Finding>, String)>,
}

fn trace(nodes: &[Node], mut i: usize) -> Vec<Act> {
    let mut acts = Vec::new();
    while let Some((p, a)) = nodes[i].parent {
        acts.push(a);
        i = p;
    }
    acts.reverse();
    acts
}

fn story(nodes: &[Node], i: usize, last: Option<Act>) -> String {
    let n = &nodes[i];
    let mut s = format!(
        "  初期状態: イヤホン{}・ドングル{}・既定の出力 {}・切断時の出力は未設定\n",
        if n.init.out { "が外" } else { "はケース内" },
        if n.init.dongle { "あり" } else { "なし" },
        n.init.default
    );
    for (j, a) in trace(nodes, i).into_iter().chain(last).enumerate() {
        s += &format!("  {}. {}\n", j + 1, a.describe());
    }
    s
}

/// `cross_check` なら、組み立て直した状態が、初期状態からやり直した状態と一致するかも確かめる
fn explore(scope: Scope, name: &str, cross_check: bool) -> Report {
    let files = Files::new(name);
    let mut nodes: Vec<Node> = Vec::new();
    let mut index: HashMap<Key, usize> = HashMap::new();
    let mut queue = VecDeque::new();
    let mut safety = Vec::new();
    let mut unmodeled = Vec::new();

    for dongle in [true, false] {
        for out in [true, false] {
            let defaults: &[&str] = if dongle { &[SPEAKER, GAME] } else { &[SPEAKER] };
            for &default in defaults {
                let init = Init { budget: scope.budget, out, dongle, default };
                let key = Sim::new(init, &files).key();
                if !index.contains_key(&key) {
                    let todo = Sim::from_key(&key, &files).enabled(scope);
                    index.insert(key.clone(), nodes.len());
                    queue.push_back(nodes.len());
                    nodes.push(Node { key, init, parent: None, todo, internal: Vec::new() });
                }
            }
        }
    }

    while let Some(i) = queue.pop_front() {
        let key = nodes[i].key.clone();
        if cross_check {
            let replayed = Sim::replay(nodes[i].init, &trace(&nodes, i), &files).key();
            assert_eq!(replayed, key, "組み立て直した状態がやり直した状態と違う\n{}", story(&nodes, i, None));
        }
        for act in std::mem::take(&mut nodes[i].todo) {
            let mut sim = Sim::from_key(&key, &files);
            if let Err(why) = sim.step(act) {
                safety.push(format!("{why}\n{}", story(&nodes, i, Some(act))));
                continue;
            }
            let next = sim.key();
            let j = match index.get(&next) {
                Some(&j) => j,
                None => {
                    let todo = match Sim::try_from_key(&next, &files) {
                        Ok(rebuilt) => rebuilt.enabled(scope),
                        Err(why) => {
                            unmodeled.push(format!("{why}\n{}", story(&nodes, i, Some(act))));
                            continue;
                        }
                    };
                    let init = nodes[i].init;
                    index.insert(next.clone(), nodes.len());
                    queue.push_back(nodes.len());
                    nodes.push(Node { key: next, init, parent: Some((i, act)), todo, internal: Vec::new() });
                    nodes.len() - 1
                }
            };
            if act.internal() {
                nodes[i].internal.push(j);
            }
        }
    }

    // 外からの操作がもう来ないとき、内部の処理だけで最後にたどり着く状態の集まり（出口のない強連結成分）を調べる
    let mut stuck = Vec::new();
    for comp in bottom_sccs(&nodes) {
        // 最短の手順で示すため、成分の中で最初に見つけた状態を代表にする
        let bad: Vec<usize> = comp.iter().copied().filter(|&i| aligned(&nodes[i].key).is_err()).collect();
        if let Some(&i) = bad.iter().min() {
            let kinds: Vec<_> = bad.iter().map(|&i| finding(&nodes[i].key)).collect();
            let kind = if kinds.iter().all(|k| *k == kinds[0]) { kinds[0] } else { None };
            let why = aligned(&nodes[i].key).unwrap_err();
            stuck.push((kind, format!("{why}\n{}", story(&nodes, i, None))));
        }
    }
    Report { states: nodes.len(), safety, unmodeled, stuck }
}

/// 内部の処理の辺だけで見た強連結成分のうち、外に出る辺がないもの（反復版の Tarjan）
fn bottom_sccs(nodes: &[Node]) -> Vec<Vec<usize>> {
    let n = nodes.len();
    let mut index = vec![usize::MAX; n];
    let mut low = vec![0; n];
    let mut on_stack = vec![false; n];
    let mut stack = Vec::new();
    let mut comp_of = vec![usize::MAX; n];
    let mut comps: Vec<Vec<usize>> = Vec::new();
    let mut next = 0;
    for root in 0..n {
        if index[root] != usize::MAX {
            continue;
        }
        let mut work = vec![(root, 0usize)];
        while let Some(&mut (v, ref mut ei)) = work.last_mut() {
            if *ei == 0 && index[v] == usize::MAX {
                index[v] = next;
                low[v] = next;
                next += 1;
                stack.push(v);
                on_stack[v] = true;
            }
            if let Some(&w) = nodes[v].internal.get(*ei) {
                *ei += 1;
                if index[w] == usize::MAX {
                    work.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
                continue;
            }
            work.pop();
            if let Some(&(u, _)) = work.last() {
                low[u] = low[u].min(low[v]);
            }
            if low[v] == index[v] {
                let mut comp = Vec::new();
                loop {
                    let w = stack.pop().unwrap();
                    on_stack[w] = false;
                    comp_of[w] = comps.len();
                    comp.push(w);
                    if w == v {
                        break;
                    }
                }
                comps.push(comp);
            }
        }
    }
    comps
        .into_iter()
        .enumerate()
        .filter(|(c, comp)| comp.iter().all(|&v| nodes[v].internal.iter().all(|&w| comp_of[w] == *c)))
        .map(|(_, comp)| comp)
        .collect()
}

/// 安全性の違反がなく、収束しない状態はすべて `expected` の種類であること。
/// `expected` があるなら、それが少なくとも 1 つ見つかること（見つからなくなったら直ったので、期待を更新する）
fn check(r: &Report, expected: Option<Finding>) {
    eprintln!("状態数: {}", r.states);
    let unexpected: Vec<_> = r.stuck.iter().filter(|(k, _)| k.is_none() || *k != expected).collect();
    let mut msg = String::new();
    for s in r.unmodeled.iter().take(3) {
        msg += &format!("\n[モデル外] {s}");
    }
    for s in r.safety.iter().take(3) {
        msg += &format!("\n[安全性] {s}");
    }
    for (_, s) in unexpected.iter().take(3) {
        msg += &format!("\n[収束] {s}");
    }
    assert!(
        r.unmodeled.is_empty() && r.safety.is_empty() && unexpected.is_empty(),
        "組み立て直せない状態 {} 件・安全性の違反 {} 件・収束しない状態 {} 件{msg}",
        r.unmodeled.len(),
        r.safety.len(),
        unexpected.len()
    );
    if let Some(f) = expected {
        let found = r.stuck.iter().find(|(k, _)| *k == Some(f));
        let (_, example) = found.unwrap_or_else(|| panic!("既知の {f:?} が見つからなくなった。直ったならスキルと期待を更新する"));
        eprintln!("既知の {f:?}: {} 件。例:\n{example}", r.stuck.len());
    }
}

const RELIABLE: Scope = Scope { budget: BUDGET, drop_reports: false, stall_writes: false };

/// 通信が欠けない世界
#[test]
fn converges_when_hid_is_reliable() {
    check(&explore(RELIABLE, "reliable", false), None);
}

/// 自発送信のレポートが届かないことがある世界（protocol.md 1 章）。既知の発見 `MissedCaseIn` だけが残る
#[test]
fn reports_drop_only_known_finding() {
    check(&explore(Scope { drop_reports: true, ..RELIABLE }, "drop", false), Some(Finding::MissedCaseIn));
}

/// 問い合わせの送信が戻らないことがある世界（protocol.md 7 章）。既知の発見 `StalledAtOpen` だけが残る
#[test]
fn writes_stall_only_known_finding() {
    check(&explore(Scope { stall_writes: true, ..RELIABLE }, "stall", false), Some(Finding::StalledAtOpen));
}

/// `Key` が `App` の状態を取りこぼしていないこと：組み立て直した状態と、初期状態からやり直した状態が一致する
#[test]
fn key_captures_app_state() {
    let scope = Scope { budget: SMALL, drop_reports: false, stall_writes: true };
    let r = explore(scope, "cross-check", true);
    assert!(r.unmodeled.is_empty(), "組み立て直せない状態がある:\n{}", r.unmodeled[0]);
}
