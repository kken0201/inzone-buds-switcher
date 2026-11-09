mod app;
mod audio;
mod config;
mod hid;
mod probe;
mod state;
mod switch;

use std::io::IsTerminal;
use std::sync::mpsc::RecvTimeoutError;
use std::time::Instant;

use anyhow::{Result, bail};
use hid::watch::HidEvent;
use tracing_subscriber::{EnvFilter, fmt::time::ChronoLocal};

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_timer(ChronoLocal::rfc_3339())
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("probe") => probe::run(&args[1..]),
        Some("watch") => watch(),
        Some("list") => list(),
        Some("set") => set(&args[1..]),
        None | Some("run") => run(),
        _ => {
            eprintln!(
                "usage: inzone-buds-switcher [run | probe [--battery <秒>] [--query <16進,...>] [--vendor-only] | watch | list | set <id> [--communications]]"
            );
            bail!("サブコマンドを指定してください");
        }
    }
}

/// HID イベントで状態を追い、接続/切断が確定したら出力を切り替える
fn run() -> Result<()> {
    let _com = audio::ComGuard::new()?;
    let path = config::path()?;
    tracing::info!("設定: {}", path.display());
    let mut app = app::App::new(path)?;
    let (tx, rx) = std::sync::mpsc::channel();
    hid::watch::spawn(tx);
    loop {
        let ev = match app.deadline() {
            Some(d) => match rx.recv_timeout(d.saturating_duration_since(Instant::now())) {
                Ok(ev) => Some(ev),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            },
            None => match rx.recv() {
                Ok(ev) => Some(ev),
                Err(_) => break,
            },
        };
        let now = Instant::now();
        let update = match ev {
            Some(ev) => {
                log_event(&ev);
                app.handle(ev, now)
            }
            None => app.tick(now),
        };
        if let Some(u) = update {
            log_update(&u);
        }
    }
    Ok(())
}

fn log_update(u: &app::Update) {
    use state::Transition;
    use switch::{Outcome, Skip};
    match u.transition {
        Transition::Established { connected } => {
            tracing::info!("状態確定: {}（切り替えない）", if connected { "接続" } else { "切断" })
        }
        Transition::Lost => tracing::info!("状態不明（ドングルなし）"),
        Transition::Changed { connected } => {
            tracing::info!("状態変化: {}", if connected { "接続" } else { "切断" })
        }
    }
    for o in &u.outcomes {
        match o {
            Outcome::Switched(d) => tracing::info!("{} に切り替えました", d.name),
            Outcome::Restored(d) => tracing::info!("{} に戻しました", d.name),
            Outcome::Remembered(d) => tracing::info!("切断時の出力を {} に設定しました", d.name),
            Outcome::Skipped(Skip::Disabled) => tracing::info!("自動切り替えがオフ"),
            Outcome::Skipped(Skip::UserChanged) => {
                tracing::info!("既定デバイスが手動で変えられているため戻さない")
            }
            Outcome::Skipped(Skip::NoGameDevice) => {
                tracing::warn!("INZONE Buds - Game が見つからないため切り替えない")
            }
            Outcome::Skipped(Skip::NotConfigured) => tracing::warn!("切断時の出力が未設定です"),
            Outcome::Skipped(Skip::Missing(d)) => {
                tracing::warn!("{}（{}）が見つかりません", d.name, d.id)
            }
            Outcome::Failed(e) => tracing::warn!("出力を切り替えられませんでした: {e}"),
        }
    }
}

/// HID 監視スレッドのイベントをログに出し続ける
fn watch() -> Result<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    hid::watch::spawn(tx);
    for ev in rx {
        log_event(&ev);
    }
    Ok(())
}

fn log_event(ev: &HidEvent) {
    match ev {
        HidEvent::DongleFound => tracing::info!("ドングル検出"),
        HidEvent::DongleLost => tracing::info!("ドングル切断"),
        HidEvent::Earbuds(e) => {
            tracing::info!("イヤホン L={} R={}", on_off(e.left), on_off(e.right))
        }
        HidEvent::Battery(b) => tracing::info!(
            "バッテリー L={} R={} ケース={}（{}）",
            pct(b.left),
            pct(b.right),
            pct(b.case),
            if b.earbuds().any() { "イヤホン接続中" } else { "両耳とも未接続" }
        ),
    }
}

fn on_off(b: bool) -> &'static str {
    if b { "接続" } else { "切断" }
}

fn pct(v: Option<u8>) -> String {
    v.map_or("--".into(), |v| format!("{v}%"))
}

fn list() -> Result<()> {
    let _com = audio::ComGuard::new()?;
    let audio = audio::Audio::new()?;
    let default = audio.default_id()?;
    for ep in audio.list()? {
        let mark = if default.as_deref() == Some(&ep.id) { "*" } else { " " };
        let kind = ep.kind.map_or(String::new(), |k| format!("\tdongle:{k:?}"));
        println!("{mark} {}\t{}{kind}", ep.id, ep.name);
    }
    Ok(())
}

fn set(args: &[String]) -> Result<()> {
    let communications = args.iter().any(|a| a == "--communications");
    let Some(id) = args.iter().find(|a| !a.starts_with("--")) else {
        bail!("set <エンドポイント ID> [--communications]");
    };
    let _com = audio::ComGuard::new()?;
    audio::Audio::new()?.set_default(id, communications)
}
