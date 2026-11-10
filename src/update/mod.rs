//! 自動更新（docs/design.md 4.8）。専用スレッドで最新リリースを確認し、exe を置き換える。
//! 起動し直すのはメインスレッド（`tray`）が行う。

pub mod http;
pub mod release;

use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail, ensure};
use release::{Decision, Target, Version};

const LATEST_URL: &str = "https://api.github.com/repos/kken0201/inzone-buds-switcher/releases/latest";
/// ログオン直後はネットワークがまだつながっていないことがあるため
const FIRST_DELAY: Duration = Duration::from_secs(60);
const INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// 24 時間たったかを確かめる間隔。スリープ中の時間も数えるよう、時計の時刻で比べる
const POLL: Duration = Duration::from_secs(60 * 60);
const VERIFY_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// 起動時に前の版を片付けた（= 更新して起動し直した直後）
    Updated(Version),
    /// exe を置き換えた。起動し直すと新しい版になる
    Ready(Version),
    /// 置き換えられなかった。自動のときは同じ版では 1 回だけ送る
    Failed(Version),
    /// 手動で確認して、今の版が最新だった。中身はリリースの版
    UpToDate(Version),
    /// 手動で確認して、問い合わせに失敗した
    CheckFailed,
}

/// 1 回の確認の結果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    UpToDate(Version),
    CheckFailed,
    DownloadFailed(Version),
    Ready(Version),
    InstallFailed(Version),
}

/// 何を通知するか。手動のときは結果が分かるようすべて知らせ、自動のときは置き換えの成否だけを知らせる。
/// 自動で同じ版の置き換えに続けて失敗しても、通知は 1 回にする（`failed` が通知済みの版）
fn event_for(step: Step, manual: bool, failed: &mut Option<Version>) -> Option<Event> {
    match step {
        Step::Ready(v) => Some(Event::Ready(v)),
        Step::InstallFailed(v) if manual => Some(Event::Failed(v)),
        Step::InstallFailed(v) => (failed.replace(v) != Some(v)).then_some(Event::Failed(v)),
        _ if !manual => None,
        Step::UpToDate(v) => Some(Event::UpToDate(v)),
        Step::CheckFailed => Some(Event::CheckFailed),
        Step::DownloadFailed(v) => Some(Event::Failed(v)),
    }
}

/// 置き換えに使う 3 つのパス。どれも exe と同じフォルダ
#[derive(Debug, Clone)]
struct Paths {
    exe: PathBuf,
    new: PathBuf,
    old: PathBuf,
}

impl Paths {
    fn of(exe: &Path) -> Self {
        let stem = exe.file_stem().map_or("inzone-buds-switcher".into(), |s| s.to_string_lossy().into_owned());
        let sibling = |suffix: &str| exe.with_file_name(format!("{stem}.{suffix}.exe"));
        Self { exe: exe.to_owned(), new: sibling("new"), old: sibling("old") }
    }
}

/// 最新リリースを問い合わせる
pub fn check() -> Result<Decision> {
    let body = http::get(LATEST_URL, "application/vnd.github+json")?;
    let text = String::from_utf8(body).context("応答が UTF-8 ではありません")?;
    release::decide(&text, Version::current())
}

/// `enabled` がオフの間は自動では問い合わせない。返す送信側に送ると、すぐに確認する（手動）。
/// `notify` は更新スレッドから呼ばれる
pub fn spawn(exe: PathBuf, enabled: Arc<AtomicBool>, notify: impl Fn(Event) + Send + 'static) -> Sender<()> {
    let (tx, rx) = mpsc::channel::<()>();
    thread::Builder::new()
        .name("update".into())
        .spawn(move || {
            let paths = Paths::of(&exe);
            if cleanup(&paths) {
                notify(Event::Updated(Version::current()));
            }
            let mut wait = FIRST_DELAY;
            let mut last: Option<SystemTime> = None;
            let mut failed: Option<Version> = None;
            loop {
                let manual = match rx.recv_timeout(wait) {
                    Ok(()) => true,
                    Err(RecvTimeoutError::Timeout) => false,
                    Err(RecvTimeoutError::Disconnected) => return,
                };
                wait = POLL;
                let due = last.is_none_or(|t| t.elapsed().map_or(true, |e| e >= INTERVAL));
                if manual || (due && enabled.load(Ordering::Relaxed)) {
                    last = Some(SystemTime::now());
                    let ev = event_for(step(&paths), manual, &mut failed);
                    // 確認している間に重ねて選ばれた分は捨てる
                    while rx.try_recv().is_ok() {}
                    match ev {
                        Some(Event::Ready(v)) => return notify(Event::Ready(v)),
                        Some(ev) => notify(ev),
                        None => {}
                    }
                }
            }
        })
        .expect("スレッドを起動できません");
    tx
}

fn step(paths: &Paths) -> Step {
    let target = match check() {
        Ok(Decision::UpToDate(v)) => {
            tracing::info!("更新を確認: 最新は v{v}（今は v{}）", Version::current());
            return Step::UpToDate(v);
        }
        Ok(Decision::Update(t)) => t,
        Err(e) => {
            tracing::warn!("更新を確認できません: {e:#}");
            return Step::CheckFailed;
        }
    };
    let v = target.version;
    tracing::info!("新しい版 v{v} をダウンロードします: {}", target.url);
    let body = match http::get(&target.url, "application/octet-stream") {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("v{v} をダウンロードできません: {e:#}");
            return Step::DownloadFailed(v);
        }
    };
    match install(paths, &target, &body) {
        Ok(()) => {
            tracing::info!("v{v} に置き換えました。起動し直します");
            Step::Ready(v)
        }
        Err(e) => {
            tracing::warn!("v{v} に更新できません: {e:#}");
            let _ = std::fs::remove_file(&paths.new);
            Step::InstallFailed(v)
        }
    }
}

fn install(paths: &Paths, target: &Target, body: &[u8]) -> Result<()> {
    ensure!(
        body.len() as u64 == target.size,
        "大きさが合いません（{} バイト、リリースでは {} バイト）",
        body.len(),
        target.size
    );
    ensure!(sha256(body)? == target.sha256, "SHA-256 がリリースの digest と合いません");
    std::fs::write(&paths.new, body).with_context(|| format!("{} に書けません", paths.new.display()))?;
    verify(&paths.new, target.version)?;
    swap(paths)
}

fn sha256(data: &[u8]) -> Result<[u8; 32]> {
    use windows::Win32::Security::Cryptography::{BCRYPT_SHA256_ALG_HANDLE, BCryptHash};
    let mut out = [0u8; 32];
    // SAFETY: 入力と出力はどちらも有効なスライスで、出力は SHA-256 の大きさ
    unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, data, &mut out) }.ok().context("SHA-256 を計算できません")?;
    Ok(out)
}

/// `<exe> version` の出力が期待した版か。起動できない exe や別の版で置き換えないため
fn verify(exe: &Path, expected: Version) -> Result<()> {
    let mut child = Command::new(exe)
        .arg("version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("{} を起動できません", exe.display()))?;
    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if start.elapsed() >= VERIFY_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            bail!("{} が {} 秒以内に終わりません", exe.display(), VERIFY_TIMEOUT.as_secs());
        }
        thread::sleep(Duration::from_millis(100));
    };
    let mut out = String::new();
    if let Some(mut s) = child.stdout.take() {
        let _ = s.read_to_string(&mut out);
    }
    ensure!(status.success(), "{} version が失敗しました（{status}）", exe.display());
    ensure!(out.trim() == expected.to_string(), "版が違います（{} を期待、{} でした）", expected, out.trim());
    Ok(())
}

/// 実行中の exe は消せないが名前は変えられるので、退避してから新しい exe を元の名前にする
fn swap(paths: &Paths) -> Result<()> {
    match std::fs::remove_file(&paths.old) {
        Err(e) if e.kind() != ErrorKind::NotFound => {
            return Err(e).with_context(|| format!("{} を消せません", paths.old.display()));
        }
        _ => {}
    }
    std::fs::rename(&paths.exe, &paths.old)
        .with_context(|| format!("{} を退避できません", paths.exe.display()))?;
    if let Err(e) = std::fs::rename(&paths.new, &paths.exe) {
        let _ = std::fs::rename(&paths.old, &paths.exe);
        return Err(e).with_context(|| format!("{} を置けません", paths.exe.display()));
    }
    Ok(())
}

/// 前回の更新の残りを消す。退避した古い exe があれば true（更新して起動し直した直後）。
/// 古いプロセスが終わるまで消せないので、0.5 秒おきに 10 秒まで試す
fn cleanup(paths: &Paths) -> bool {
    let _ = std::fs::remove_file(&paths.new);
    if !paths.old.exists() {
        return false;
    }
    for _ in 0..20 {
        match std::fs::remove_file(&paths.old) {
            Ok(()) => return true,
            Err(e) if e.kind() == ErrorKind::NotFound => return true,
            Err(_) => thread::sleep(Duration::from_millis(500)),
        }
    }
    tracing::warn!("{} を消せません", paths.old.display());
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("inzone-buds-switcher-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn read(p: &Path) -> String {
        std::fs::read_to_string(p).unwrap()
    }

    const V: Version = Version(0, 2, 0);

    #[test]
    fn automatic_check_reports_only_install_results() {
        let mut failed = None;
        for s in [Step::UpToDate(V), Step::CheckFailed, Step::DownloadFailed(V)] {
            assert_eq!(event_for(s, false, &mut failed), None, "{s:?}");
        }
        assert_eq!(event_for(Step::Ready(V), false, &mut failed), Some(Event::Ready(V)));
    }

    #[test]
    fn automatic_install_failure_is_reported_once_per_version() {
        let mut failed = None;
        assert_eq!(event_for(Step::InstallFailed(V), false, &mut failed), Some(Event::Failed(V)));
        assert_eq!(event_for(Step::InstallFailed(V), false, &mut failed), None);
        let next = Version(0, 3, 0);
        assert_eq!(event_for(Step::InstallFailed(next), false, &mut failed), Some(Event::Failed(next)));
    }

    #[test]
    fn manual_check_reports_every_result() {
        let mut failed = Some(V);
        assert_eq!(event_for(Step::UpToDate(V), true, &mut failed), Some(Event::UpToDate(V)));
        assert_eq!(event_for(Step::CheckFailed, true, &mut failed), Some(Event::CheckFailed));
        assert_eq!(event_for(Step::DownloadFailed(V), true, &mut failed), Some(Event::Failed(V)));
        assert_eq!(event_for(Step::InstallFailed(V), true, &mut failed), Some(Event::Failed(V)));
        assert_eq!(event_for(Step::Ready(V), true, &mut failed), Some(Event::Ready(V)));
    }

    #[test]
    fn sibling_paths_keep_the_exe_name() {
        let p = Paths::of(Path::new(r"C:\tools\buds.exe"));
        assert_eq!(p.new, Path::new(r"C:\tools\buds.new.exe"));
        assert_eq!(p.old, Path::new(r"C:\tools\buds.old.exe"));
    }

    #[test]
    fn swap_moves_current_aside_and_puts_new_in_place() {
        let dir = temp_dir("swap");
        let p = Paths::of(&dir.join("app.exe"));
        std::fs::write(&p.exe, "current").unwrap();
        std::fs::write(&p.new, "new").unwrap();
        std::fs::write(&p.old, "stale").unwrap();
        swap(&p).unwrap();
        assert_eq!(read(&p.exe), "new");
        assert_eq!(read(&p.old), "current");
        assert!(!p.new.exists());
    }

    #[test]
    fn swap_restores_current_when_new_is_missing() {
        let dir = temp_dir("swap-fail");
        let p = Paths::of(&dir.join("app.exe"));
        std::fs::write(&p.exe, "current").unwrap();
        assert!(swap(&p).is_err());
        assert_eq!(read(&p.exe), "current");
        assert!(!p.old.exists());
    }

    #[test]
    fn install_rejects_size_mismatch_before_writing() {
        let dir = temp_dir("size");
        let p = Paths::of(&dir.join("app.exe"));
        std::fs::write(&p.exe, "current").unwrap();
        let t = Target { version: Version(9, 0, 0), url: String::new(), size: 10, sha256: [0; 32] };
        assert!(install(&p, &t, b"short").is_err());
        assert!(!p.new.exists());
        assert_eq!(read(&p.exe), "current");
    }

    #[test]
    fn sha256_matches_known_vector() {
        // FIPS 180-2 の "abc"
        assert_eq!(
            sha256(b"abc").unwrap(),
            [
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae, 0x22, 0x23, 0xb0, 0x03,
                0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61, 0xf2, 0x00, 0x15, 0xad,
            ]
        );
    }

    #[test]
    fn install_rejects_digest_mismatch_before_writing() {
        let dir = temp_dir("digest");
        let p = Paths::of(&dir.join("app.exe"));
        std::fs::write(&p.exe, "current").unwrap();
        let t = Target { version: Version(9, 0, 0), url: String::new(), size: 3, sha256: [0; 32] };
        assert!(install(&p, &t, b"abc").is_err());
        assert!(!p.new.exists());
        assert_eq!(read(&p.exe), "current");
    }

    #[test]
    fn verify_rejects_non_executable() {
        let dir = temp_dir("verify");
        let exe = dir.join("broken.exe");
        std::fs::write(&exe, "not a program").unwrap();
        assert!(verify(&exe, Version(9, 0, 0)).is_err());
    }

    #[test]
    fn cleanup_reports_leftover_old_exe() {
        let dir = temp_dir("cleanup");
        let p = Paths::of(&dir.join("app.exe"));
        assert!(!cleanup(&p));
        std::fs::write(&p.old, "old").unwrap();
        std::fs::write(&p.new, "partial").unwrap();
        assert!(cleanup(&p));
        assert!(!p.old.exists() && !p.new.exists());
    }
}
