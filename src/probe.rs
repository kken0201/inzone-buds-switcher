//! `probe` サブコマンド：ドングルの HID レポートを生のまま出力する（実機調査・不具合報告用）。
//!
//! - ドングルの全 HID コレクションを列挙して表示し、入力レポートを持つものはすべて
//!   別スレッドで読み続ける
//! - `--battery <秒>` を付けると、ベンダー定義コレクション（usage_page 0xFF04）に
//!   バッテリー問い合わせ（docs/protocol.md 2b）を指定間隔で送る。0 なら起動時に 1 回だけ
//! - `--query <16進,...>` を付けると、起動時に指定したサブコマンドの問い合わせを 1 回ずつ送る
//!   （例：`--query 01,04`）。未知のコマンドを調べるためのもの
//! - `--vendor-only` を付けると、usage_page 0xFF00 以上のコレクションだけを読む

use std::io::Write;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::hid::{self, Device, PID, VID};

const HUB_USAGE_PAGE: u16 = 0xFF04;

static OUT: Mutex<()> = Mutex::new(());

/// 1 行ずつ出力してすぐ flush する（パイプやファイルに出しても途中経過が見えるように）
macro_rules! out {
    ($($t:tt)*) => {{
        let _g = OUT.lock();
        let mut o = std::io::stdout().lock();
        let _ = writeln!(o, $($t)*);
        let _ = o.flush();
    }};
}

struct Options {
    battery_interval: Option<u64>,
    queries: Vec<u8>,
    vendor_only: bool,
}

fn parse_args(args: &[String]) -> Result<Options> {
    let mut opts = Options { battery_interval: None, queries: Vec::new(), vendor_only: false };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--battery" => {
                let v = it.next().context("--battery には秒数が必要です")?;
                opts.battery_interval = Some(v.parse().context("--battery の秒数が不正です")?);
            }
            "--query" => {
                let v = it.next().context("--query にはサブコマンドが必要です")?;
                for x in v.split(',') {
                    opts.queries.push(u8::from_str_radix(x.trim_start_matches("0x"), 16).context("--query の値が不正です")?);
                }
            }
            "--vendor-only" => opts.vendor_only = true,
            other => bail!("不明なオプション: {other}"),
        }
    }
    Ok(opts)
}

fn now() -> String {
    chrono::Local::now().format("%H:%M:%S%.3f").to_string()
}

fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ")
}

/// 末尾の 0 を省いて表示を短くする（長さは別に表示する）
fn trim_zeros(data: &[u8]) -> &[u8] {
    let end = data.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    &data[..end]
}

/// 既知のレポートに簡単な注釈を付ける（docs/protocol.md の解釈）
fn annotate(d: &[u8]) -> String {
    let at = |i: usize| d.get(i).copied();
    let pct = |v: Option<u8>| match v {
        Some(v) if v <= 100 => format!("{v}%"),
        Some(v) => format!("0x{v:02X}"),
        None => "--".into(),
    };
    match (at(8), at(9)) {
        (Some(0x12), Some(0x01)) => {
            format!("  # 状態? L={} R={}", at(13).unwrap_or(0), at(14).unwrap_or(0))
        }
        (Some(0x14), Some(0x04)) => format!(
            "  # バッテリー? seq={} L={} R={} C={}",
            at(11).unwrap_or(0),
            pct(at(14)),
            pct(at(16)),
            pct(at(18))
        ),
        _ => String::new(),
    }
}

/// 問い合わせの出力レポートを作る。`sub` = 0x04 がバッテリー（docs/protocol.md 2b）
fn build_request(len: usize, sub: u8, seq: u8) -> Vec<u8> {
    let mut r = vec![0u8; len.max(14)];
    r[..8].copy_from_slice(&[0x02, 0x0C, 0x01, 0x00, 0xFC, 0x08, 0x96, 0xC3]);
    r[8] = 0x41; // コマンド
    r[9] = sub;
    r[10] = 0x01;
    r[11] = seq;
    // チェックサム：[6] から直前までの和
    r[13] = r[6..13].iter().fold(0u8, |a, &b| a.wrapping_add(b));
    r
}

pub fn run(args: &[String]) -> Result<()> {
    let opts = parse_args(args)?;
    let cols = hid::enumerate()?;
    if cols.is_empty() {
        bail!("ドングル（VID {VID:04X} / PID {PID:04X}）が見つかりません");
    }

    out!("# {} 個の HID コレクション", cols.len());
    let mut handles = Vec::new();
    let mut hub_writer = None;
    for (idx, c) in cols.iter().enumerate() {
        let tag = format!("{:04X}:{:04X}", c.usage_page, c.usage);
        out!(
            "# [{idx}] {tag} in={} out={} feature={} {}",
            c.input_len, c.output_len, c.feature_len, c.path
        );
        if (opts.battery_interval.is_some() || !opts.queries.is_empty()) && c.usage_page == HUB_USAGE_PAGE && hub_writer.is_none() {
            // 書き込みは別のハンドルで行う。同期 I/O では、同じハンドルの read が終わるまで write が待たされるため
            match Device::open(&c.path) {
                Ok(d) => hub_writer = Some((d, c.output_len as usize)),
                Err(e) => out!("#     書き込み用に開けません: {e:#}"),
            }
        }
        if c.input_len == 0 || (opts.vendor_only && c.usage_page < 0xFF00) {
            continue;
        }
        match Device::open(&c.path) {
            Ok(dev) => {
                let len = c.input_len as usize;
                handles.push(thread::spawn(move || read_loop(&tag, &dev, len)));
            }
            Err(e) => out!("#     開けません: {e:#}"),
        }
    }
    out!("# 開始（Ctrl+C で終了）");

    let wants_tx = opts.battery_interval.is_some() || !opts.queries.is_empty();
    match hub_writer {
        None if wants_tx => out!("# usage_page 0xFF04 のコレクションを開けないので問い合わせできません"),
        None => {}
        Some((dev, len)) => {
            let mut seq: u8 = 1;
            let send = |sub: u8, seq: u8| {
                let req = build_request(len, sub, seq);
                match dev.write(&req) {
                    Ok(n) => out!(
                        "{} TX  [FF04:0001] len={n} {}  # 問い合わせ sub=0x{sub:02X} seq={seq}",
                        now(),
                        hex(trim_zeros(&req))
                    ),
                    Err(e) => out!("{} TX! [FF04:0001] {e:#}", now()),
                }
            };
            for &sub in &opts.queries {
                send(sub, seq);
                seq = seq.wrapping_add(1);
                thread::sleep(Duration::from_millis(500));
            }
            if let Some(iv) = opts.battery_interval {
                loop {
                    send(0x04, seq);
                    seq = seq.wrapping_add(1);
                    if iv == 0 {
                        break;
                    }
                    thread::sleep(Duration::from_secs(iv));
                }
            }
        }
    }
    for h in handles {
        let _ = h.join();
    }
    Ok(())
}

fn read_loop(tag: &str, dev: &Device, len: usize) {
    let mut buf = vec![0u8; len];
    let mut last = Instant::now();
    loop {
        match dev.read(&mut buf) {
            Ok(0) => continue,
            Ok(n) => {
                let gap = last.elapsed().as_secs_f64();
                last = Instant::now();
                out!(
                    "{} RX  [{tag}] len={n} +{gap:.3}s {}{}",
                    now(),
                    hex(trim_zeros(&buf[..n])),
                    annotate(&buf[..n])
                );
            }
            Err(e) => {
                out!("{} ERR [{tag}] {e:#}（このコレクションの読み取りを終了）", now());
                return;
            }
        }
    }
}
