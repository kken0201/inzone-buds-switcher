//! ドングルの監視スレッド（docs/design.md 4.1）。
//!
//! 1 スレッドで、Overlapped I/O の読み取りを待ちつつ 30 秒ごとにバッテリーを問い合わせる。
//! イベントは `mpsc::Sender` で送る。受信側が閉じたらスレッドを終える。
//! メインスレッドのメッセージループを起こす手段が要るときは、受け取り側で転送すればよいので、
//! ここは Sender のままにしておく。

use std::sync::mpsc::Sender;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_DEVICE_NOT_CONNECTED, ERROR_IO_PENDING, GENERIC_READ, GENERIC_WRITE, HANDLE,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadFile,
    WriteFile,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Threading::{CreateEventW, ResetEvent, WaitForSingleObject};
use windows::core::PCWSTR;

use super::protocol::{Battery, Earbuds, Origin, Report, build_query, hex, parse, trim_zeros};
use super::{Collection, enumerate, wide};

const HUB_USAGE_PAGE: u16 = 0xFF04;
const HUB_USAGE: u16 = 0x0001;

const SUB_LINK: u8 = 0x02;
const SUB_BATTERY: u8 = 0x04;

const BATTERY_POLL: Duration = Duration::from_secs(30);
const REENUMERATE: Duration = Duration::from_secs(3);
const IO_TIMEOUT_MS: u32 = 1000;
/// 応答を待つ期間。応答は約 0.2〜0.4 秒で届く
const RESPONSE_WINDOW: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HidEvent {
    DongleFound,
    DongleLost,
    /// `12 01` / `14 02` による左右の接続状態
    Earbuds(Earbuds),
    /// `14 04`。左右が None なら未接続（`Battery::earbuds()`）
    Battery(Battery),
}

pub fn spawn(tx: Sender<HidEvent>) -> JoinHandle<()> {
    thread::Builder::new()
        .name("hid-watch".into())
        .spawn(move || run(tx))
        .expect("スレッドを起動できません")
}

fn run(tx: Sender<HidEvent>) {
    loop {
        if let Some(col) = find_collection() {
            match session(&col, &tx) {
                Ok(()) => return,
                Err(e) => {
                    tracing::warn!("ドングルとの通信を終了: {e:#}");
                    if tx.send(HidEvent::DongleLost).is_err() {
                        return;
                    }
                }
            }
        }
        thread::sleep(REENUMERATE);
    }
}

fn find_collection() -> Option<Collection> {
    match enumerate() {
        Ok(cols) => cols
            .into_iter()
            .find(|c| c.usage_page == HUB_USAGE_PAGE && c.usage == HUB_USAGE && c.input_len > 0),
        Err(e) => {
            tracing::warn!("列挙に失敗: {e:#}");
            None
        }
    }
}

/// 送信済みの問い合わせ。応答の `seq` と突き合わせる
struct Sent {
    sub: u8,
    seq: u8,
    at: Instant,
}

/// Ok(()) は受信側が閉じたこと、Err はドングルを失ったことを表す
fn session(col: &Collection, tx: &Sender<HidEvent>) -> Result<()> {
    // 書き込みは別ハンドル。書き込みが詰まっても読み取りを止めないため
    let mut reader = Reader::open(&col.path, col.input_len as usize)?;
    let writer = Port::open(&col.path)?;
    tracing::debug!("ドングルを開きました: {}", col.path);
    if tx.send(HidEvent::DongleFound).is_err() {
        return Ok(());
    }

    let out_len = col.output_len as usize;
    let mut sent: Vec<Sent> = Vec::new();
    // Hub は小さい番号を使うので、重なりにくい範囲を使う
    let mut seq: u8 = 0x80;
    let mut query = |sub: u8, sent: &mut Vec<Sent>| -> Result<()> {
        seq = if seq == 0xFF { 0x80 } else { seq + 1 };
        let req = build_query(out_len, sub, seq);
        tracing::debug!("TX {}", hex(trim_zeros(&req)));
        match writer.write(&req) {
            Ok(()) => sent.push(Sent { sub, seq, at: Instant::now() }),
            // 応答しないのは状態の変化ではないので、警告だけ出して続ける
            Err(WriteError::Timeout) => tracing::warn!("問い合わせ 0x{sub:02X} の送信がタイムアウト"),
            Err(WriteError::Failed(e)) if e.code() == ERROR_DEVICE_NOT_CONNECTED.to_hresult() => {
                return Err(anyhow!(e).context("ドングルが取り外されました"));
            }
            Err(WriteError::Failed(e)) => tracing::warn!("問い合わせ 0x{sub:02X} の送信に失敗: {e}"),
        }
        Ok(())
    };

    query(SUB_LINK, &mut sent)?;
    query(SUB_BATTERY, &mut sent)?;
    let mut next_poll = Instant::now() + BATTERY_POLL;

    loop {
        let wait = next_poll.saturating_duration_since(Instant::now());
        if let Some(buf) = reader.wait(wait.as_millis() as u32)? {
            tracing::debug!("RX {}", hex(trim_zeros(buf)));
            sent.retain(|s| s.at.elapsed() < RESPONSE_WINDOW);
            if let Some(ev) = parse(buf).and_then(|p| to_event(p.report, p.origin, &mut sent)) {
                if tx.send(ev).is_err() {
                    return Ok(());
                }
            }
        }
        if Instant::now() >= next_poll {
            query(SUB_BATTERY, &mut sent)?;
            next_poll += BATTERY_POLL;
        }
    }
}

/// 自発送信か、自分の要求への応答だけをイベントにする（Hub など他アプリへの応答は捨てる）
fn to_event(report: Report, origin: Origin, sent: &mut Vec<Sent>) -> Option<HidEvent> {
    let (sub, ev) = match report {
        Report::Status(e) => return Some(HidEvent::Earbuds(e)),
        Report::Link(e) => (SUB_LINK, HidEvent::Earbuds(e)),
        Report::Battery(b) => (SUB_BATTERY, HidEvent::Battery(b)),
        Report::Other { .. } => return None,
    };
    if let Origin::Response { seq } = origin {
        let i = sent.iter().position(|s| s.sub == sub && s.seq == seq)?;
        sent.remove(i);
    }
    Some(ev)
}

/// FILE_FLAG_OVERLAPPED で開いたハンドルと、その I/O 完了用イベント
struct Port {
    handle: HANDLE,
    event: HANDLE,
}

enum WriteError {
    Timeout,
    Failed(windows::core::Error),
}

impl Port {
    /// INZONE Hub などと同時に開けるよう共有モードで開く
    fn open(path: &str) -> Result<Self> {
        let w = wide(path);
        let handle = unsafe {
            CreateFileW(
                PCWSTR(w.as_ptr()),
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                None,
            )
        }
        .with_context(|| format!("{path} を開けません"))?;
        match unsafe { CreateEventW(None, true, false, None) } {
            Ok(event) => Ok(Self { handle, event }),
            Err(e) => {
                let _ = unsafe { CloseHandle(handle) };
                Err(e).context("イベントを作れません")
            }
        }
    }

    fn overlapped(&self) -> Box<OVERLAPPED> {
        Box::new(OVERLAPPED { hEvent: self.event, ..Default::default() })
    }

    /// 1 秒で戻らなければ取り消す。ドングルが書き込みに応答しなくなることがある（protocol.md 7 章）
    fn write(&self, data: &[u8]) -> Result<(), WriteError> {
        let buf: Box<[u8]> = data.into();
        let mut ov = self.overlapped();
        unsafe {
            let _ = ResetEvent(self.event);
            if let Err(e) = WriteFile(self.handle, Some(&buf), None, Some(&mut *ov)) {
                if e.code() != ERROR_IO_PENDING.to_hresult() {
                    return Err(WriteError::Failed(e));
                }
            }
            if WaitForSingleObject(self.event, IO_TIMEOUT_MS) == WAIT_OBJECT_0 {
                let mut n = 0u32;
                return GetOverlappedResult(self.handle, &*ov, &mut n, false)
                    .map_err(WriteError::Failed);
            }
            let _ = CancelIoEx(self.handle, Some(&*ov));
            if WaitForSingleObject(self.event, IO_TIMEOUT_MS) != WAIT_OBJECT_0 {
                // カーネルがまだ使っているかもしれないので解放しない
                Box::leak(ov);
                Box::leak(buf);
            }
        }
        Err(WriteError::Timeout)
    }
}

impl Drop for Port {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
            let _ = CloseHandle(self.event);
        }
    }
}

/// 読み取りを 1 つ発行したままにして、完了かタイムアウトを待つ
struct Reader {
    port: Port,
    ov: Box<OVERLAPPED>,
    buf: Box<[u8]>,
    pending: bool,
}

impl Reader {
    fn open(path: &str, input_len: usize) -> Result<Self> {
        let port = Port::open(path)?;
        let ov = port.overlapped();
        Ok(Self { port, ov, buf: vec![0u8; input_len].into(), pending: false })
    }

    /// 届いたレポートを返す。`timeout_ms` 経っても届かなければ None。読み取りの失敗はすべて Err
    fn wait(&mut self, timeout_ms: u32) -> Result<Option<&[u8]>> {
        if !self.pending {
            unsafe {
                let _ = ResetEvent(self.port.event);
                if let Err(e) =
                    ReadFile(self.port.handle, Some(&mut self.buf), None, Some(&mut *self.ov))
                {
                    if e.code() != ERROR_IO_PENDING.to_hresult() {
                        return Err(e).context("ReadFile 失敗");
                    }
                }
            }
            self.pending = true;
        }
        let r = unsafe { WaitForSingleObject(self.port.event, timeout_ms) };
        if r == WAIT_TIMEOUT {
            return Ok(None);
        }
        if r != WAIT_OBJECT_0 {
            return Err(anyhow!("WaitForSingleObject 失敗: {r:?}"));
        }
        self.pending = false;
        let mut n = 0u32;
        unsafe { GetOverlappedResult(self.port.handle, &*self.ov, &mut n, false) }
            .context("読み取り失敗")?;
        Ok(Some(&self.buf[..n as usize]))
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        if !self.pending {
            return;
        }
        unsafe {
            let _ = CancelIoEx(self.port.handle, Some(&*self.ov));
            if WaitForSingleObject(self.port.event, IO_TIMEOUT_MS) != WAIT_OBJECT_0 {
                // 完了を確認できなかったバッファは解放しない
                std::mem::forget(std::mem::take(&mut self.buf));
                std::mem::forget(std::mem::replace(&mut self.ov, Box::new(OVERLAPPED::default())));
            }
        }
    }
}
