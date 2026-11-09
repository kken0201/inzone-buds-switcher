mod audio;
mod config;
mod hid;
mod probe;
mod state;

use std::io::IsTerminal;

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
        _ => {
            eprintln!(
                "usage: inzone-buds-switcher <probe [--battery <秒>] [--query <16進,...>] [--vendor-only] | watch | list | set <id> [--communications]>"
            );
            bail!("サブコマンドを指定してください");
        }
    }
}

/// HID 監視スレッドのイベントをログに出し続ける
fn watch() -> Result<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    hid::watch::spawn(tx);
    for ev in rx {
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
    Ok(())
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
