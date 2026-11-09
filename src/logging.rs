//! ログの出力先（docs/design.md 4.7）。

use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{Context, Result};
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::{EnvFilter, Layer, fmt::time::ChronoLocal, layer::SubscriberExt, util::SubscriberInitExt};

/// 日次ローテーションで残すファイル数。2 週間分あれば不具合の報告に足り、容量も小さいため
const KEEP_FILES: usize = 14;

pub fn dir() -> Result<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA が設定されていません")?;
    Ok(PathBuf::from(base).join("inzone-buds-switcher").join("logs"))
}

/// stderr には常に出す。`file` のときはログフォルダにも出す（トレイのときだけ。サブコマンドは出さない）。
/// 既定は info、`RUST_LOG` で変える。HID の生レポートは debug
pub fn init(file: bool) {
    let filter = || EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let stderr = tracing_subscriber::fmt::layer()
        .with_timer(ChronoLocal::rfc_3339())
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal());
    let (file_layer, error) = match file.then(file_appender) {
        Some(Ok(w)) => (
            Some(
                tracing_subscriber::fmt::layer()
                    .with_timer(ChronoLocal::rfc_3339())
                    .with_writer(w)
                    .with_ansi(false),
            ),
            None,
        ),
        Some(Err(e)) => (None, Some(e)),
        None => (None, None),
    };
    tracing_subscriber::registry().with(stderr.and_then(file_layer).with_filter(filter())).init();
    if let Some(e) = error {
        tracing::warn!("ログファイルを作れないため標準エラーだけに出力します: {e:#}");
    }
}

fn file_appender() -> Result<RollingFileAppender> {
    let dir = dir()?;
    RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("inzone-buds-switcher")
        .filename_suffix("log")
        .max_log_files(KEEP_FILES)
        .build(&dir)
        .with_context(|| format!("{} にログを作れません", dir.display()))
}
