mod app;
mod audio;
mod autostart;
mod config;
mod hid;
mod instance;
mod logging;
mod notify;
mod probe;
mod state;
mod switch;
mod tray;

use anyhow::{Result, bail};
use hid::watch::HidEvent;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let tray = matches!(args.first().map(String::as_str), None | Some("run"));
    logging::init(tray);
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

/// トレイに常駐し、接続/切断が確定したら出力を切り替える
fn run() -> Result<()> {
    // 二重起動は静かに終わる。スタートアップとの重複などで日常的に起こりうるため、ダイアログは出さない
    let Some(_instance) = instance::acquire()? else {
        tracing::info!("すでに起動しているため終了します");
        return Ok(());
    };
    let _com = audio::ComGuard::new()?;
    let path = config::path()?;
    tracing::info!("設定: {}", path.display());
    tray::run(app::App::new(path)?)
}

pub(crate) fn log_update(u: &app::Update) {
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

pub(crate) fn log_event(ev: &HidEvent) {
    match ev {
        HidEvent::DongleFound => tracing::info!("ドングル検出"),
        HidEvent::DongleLost => tracing::info!("ドングル切断"),
        HidEvent::Earbuds(e) => {
            tracing::info!("イヤホン L={} R={}", on_off(e.left), on_off(e.right))
        }
        HidEvent::Battery(b) => tracing::info!(
            "バッテリー L={} R={} ケース={}（{}）",
            tray::view::pct(b.left),
            tray::view::pct(b.right),
            tray::view::pct(b.case),
            if b.earbuds().any() { "イヤホン接続中" } else { "両耳とも未接続" }
        ),
    }
}

fn on_off(b: bool) -> &'static str {
    if b { "接続" } else { "切断" }
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
