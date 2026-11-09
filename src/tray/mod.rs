//! トレイアイコンとメニュー、メインスレッドのメッセージループ（docs/design.md 3, 4.4）。
//!
//! 状態はメインスレッドの `TRAY` に置き、ウィンドウプロシージャから触る。
//! HID 監視スレッドのイベントは転送スレッドがチャネルに入れ、`WM_HID` でループを起こす。

mod icon;
pub mod view;

use std::cell::{Cell, RefCell};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Instant;

use anyhow::{Context, Result};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIIF_INFO, NIIF_WARNING, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NIN_SELECT, ShellExecuteW,
    NINF_KEY, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetMessageW, HMENU, KillTimer, MF_CHECKED, MF_GRAYED, MF_POPUP, MF_SEPARATOR,
    MF_STRING, MSG, PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW,
    SW_SHOWNORMAL, SetForegroundWindow, SetTimer, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage,
    WINDOW_EX_STYLE, WM_APP, WM_CONTEXTMENU, WM_DESTROY, WM_NULL, WM_TIMER, WNDCLASSW, WS_OVERLAPPED,
};
use windows::core::{PCWSTR, w};

use crate::app::{App, Update};
use crate::notify::{self, Kind, Notice};
use crate::hid::watch::HidEvent;
use icon::Icons;
use view::{Command, Item, Setting, Status};

const WM_HID: u32 = WM_APP;
const WM_TRAY: u32 = WM_APP + 1;
const DEBOUNCE_TIMER: usize = 1;
const ICON_ID: u32 = 1;

struct Tray {
    hwnd: HWND,
    app: App,
    rx: Receiver<HidEvent>,
    icons: Icons,
    /// 最後にシェルへ渡した表示。None はアイコンがまだ（または Explorer の再起動で）ない
    shown: Option<(Status, String)>,
}

thread_local! {
    static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) };
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
}

/// `TRAY` を借りられないとき（再入中）は false
fn with_tray(f: impl FnOnce(&mut Tray)) -> bool {
    TRAY.with(|t| match t.try_borrow_mut() {
        Ok(mut t) => {
            if let Some(t) = t.as_mut() {
                f(t);
            }
            true
        }
        Err(_) => false,
    })
}

/// トレイを表示し、終了が選ばれるまでメッセージループを回す
pub fn run(app: App) -> Result<()> {
    let hwnd = create_window()?;
    let (tx, hid_rx) = mpsc::channel();
    crate::hid::watch::spawn(tx);
    let rx = forward(hid_rx, hwnd);
    let tray = Tray { hwnd, app, rx, icons: Icons::new()?, shown: None };
    TRAY.with(|t| *t.borrow_mut() = Some(tray));
    with_tray(Tray::show);

    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    // COM を使う App を、呼び出し側の ComGuard より先に捨てる
    TRAY.with(|t| t.borrow_mut().take());
    Ok(())
}

/// TaskbarCreated はブロードキャストで届くので、メッセージ専用ウィンドウ（HWND_MESSAGE）は使えない。
/// 表示しない通常のウィンドウにする
fn create_window() -> Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = w!("InzoneBudsSwitcherTray");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: class,
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return Err(windows::core::Error::from_thread()).context("ウィンドウクラスを登録できません");
        }
        TASKBAR_CREATED.set(RegisterWindowMessageW(w!("TaskbarCreated")));
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            w!("inzone-buds-switcher"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .context("ウィンドウを作れません")
    }
}

/// 監視スレッドのイベントをメインスレッド用のチャネルに移し、`WM_HID` でループを起こす。
/// 監視スレッドを Win32 のウィンドウに依存させないため、間に 1 スレッド挟む
fn forward(hid_rx: Receiver<HidEvent>, hwnd: HWND) -> Receiver<HidEvent> {
    let (tx, rx) = mpsc::channel();
    // HWND は Send ではないので整数で渡す
    let raw = hwnd.0 as isize;
    thread::Builder::new()
        .name("hid-forward".into())
        .spawn(move || {
            for ev in hid_rx {
                let hwnd = HWND(raw as _);
                if tx.send(ev).is_err()
                    || unsafe { PostMessageW(Some(hwnd), WM_HID, WPARAM(0), LPARAM(0)) }.is_err()
                {
                    break;
                }
            }
        })
        .expect("スレッドを起動できません");
    rx
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let handled = match msg {
        WM_HID => with_tray(Tray::drain),
        WM_TIMER if wparam.0 == DEBOUNCE_TIMER => with_tray(Tray::tick),
        WM_TRAY => {
            // NOTIFYICON_VERSION_4 では lParam の下位がイベント、wParam がアイコンの位置
            let event = (lparam.0 as u32) & 0xFFFF;
            // 左クリック（NIN_SELECT）とキーボードの決定（| NINF_KEY）でも開く
            if event == WM_CONTEXTMENU || event == NIN_SELECT || event == NIN_SELECT | NINF_KEY {
                let x = (wparam.0 & 0xFFFF) as i16 as i32;
                let y = ((wparam.0 >> 16) & 0xFFFF) as i16 as i32;
                open_menu(hwnd, x, y);
            }
            true
        }
        WM_DESTROY => {
            with_tray(Tray::remove);
            unsafe { PostQuitMessage(0) };
            true
        }
        m if m == TASKBAR_CREATED.get() => with_tray(|t| {
            tracing::info!("タスクバーが作り直されたのでアイコンを追加し直します");
            // 残っていると追加が失敗し続けるので、消してから追加する
            t.remove();
            t.show();
        }),
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    };
    if !handled {
        // シェルの呼び出し中などに再入して届いた。借用が解けてから処理するよう投げ直す
        let _ = unsafe { PostMessageW(Some(hwnd), msg, wparam, lparam) };
    }
    LRESULT(0)
}

/// メニューを開いている間（TrackPopupMenu のモーダルループ）も HID のメッセージを処理できるよう、
/// その間は `TRAY` を借りない
fn open_menu(hwnd: HWND, x: i32, y: i32) {
    let mut items = None;
    with_tray(|t| items = Some(t.menu()));
    let Some(items) = items else { return };
    let mut commands = Vec::new();
    let chosen = unsafe {
        let Ok(menu) = render(&items, &mut commands) else {
            tracing::warn!("メニューを作れません");
            return;
        };
        // 前面にしないと、メニューの外をクリックしても閉じない
        let _ = SetForegroundWindow(hwnd);
        let id = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, x, y, None, hwnd, None).0;
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        (id as usize).checked_sub(1).and_then(|i| commands.into_iter().nth(i))
    };
    match chosen {
        Some(Command::Quit) => {
            let _ = unsafe { DestroyWindow(hwnd) };
        }
        Some(Command::Set(s)) => {
            with_tray(|t| t.set(s));
        }
        Some(Command::Autostart(on)) => match crate::autostart::set(on) {
            Ok(()) => tracing::info!("Windows 起動時に開始: {on}"),
            Err(e) => tracing::warn!("スタートアップの設定を変えられません: {e:#}"),
        },
        Some(Command::OpenLogs) => open_logs(),
        None => {}
    }
}

fn open_logs() {
    let dir = match crate::logging::dir().and_then(|d| {
        // まだ 1 度もログを書いていなくても開けるようにする
        std::fs::create_dir_all(&d)?;
        Ok(d)
    }) {
        Ok(d) => d,
        Err(e) => return tracing::warn!("ログフォルダを開けません: {e:#}"),
    };
    let path = wide(&dir.to_string_lossy());
    let r = unsafe { ShellExecuteW(None, w!("open"), PCWSTR(path.as_ptr()), None, None, SW_SHOWNORMAL) };
    // 32 以下がエラー
    if r.0 as isize <= 32 {
        tracing::warn!("ログフォルダを開けません: {}", dir.display());
    }
}

/// メニューの項目 ID は `commands` の添字 + 1（0 は「何も選ばなかった」）
unsafe fn render(items: &[Item], commands: &mut Vec<Command>) -> Result<HMENU> {
    unsafe {
        let menu = CreatePopupMenu()?;
        for item in items {
            match item {
                Item::Separator => AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null())?,
                Item::Entry { text, checked, command } => {
                    let mut flags = MF_STRING;
                    if *checked {
                        flags |= MF_CHECKED;
                    }
                    let id = match command {
                        Some(c) => {
                            commands.push(c.clone());
                            commands.len()
                        }
                        None => {
                            flags |= MF_GRAYED;
                            0
                        }
                    };
                    AppendMenuW(menu, flags, id, PCWSTR(wide(text).as_ptr()))?
                }
                Item::Submenu { text, items } => {
                    let sub = render(items, commands)?;
                    AppendMenuW(menu, MF_STRING | MF_POPUP, sub.0 as usize, PCWSTR(wide(text).as_ptr()))?
                }
            }
        }
        Ok(menu)
    }
}

/// 末尾の NUL を残して切り詰めてコピーする
fn copy_wide(dst: &mut [u16], src: &str) {
    let max = dst.len() - 1;
    for (d, s) in dst.iter_mut().zip(src.encode_utf16().take(max)) {
        *d = s;
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

impl Tray {
    fn drain(&mut self) {
        while let Ok(ev) = self.rx.try_recv() {
            crate::log_event(&ev);
            let update = self.app.handle(ev, Instant::now());
            self.report(update);
        }
        self.rearm();
        self.show();
    }

    fn tick(&mut self) {
        let update = self.app.tick(Instant::now());
        self.report(update);
        self.rearm();
        self.show();
    }

    /// 更新をログに残し、通知する
    fn report(&mut self, update: Option<Update>) {
        if let Some(u) = update {
            crate::log_update(&u);
            let config = self.app.config().clone();
            for n in u.outcomes.iter().filter_map(|o| notify::for_outcome(o, &config)) {
                self.balloon(&n);
            }
        }
        if let Some(a) = self.app.take_low_battery() {
            tracing::info!("バッテリー低下: L={} R={}", view::pct(a.left), view::pct(a.right));
            if let Some(n) = notify::for_low_battery(a, self.app.config()) {
                self.balloon(&n);
            }
        }
    }

    /// 既存のトレイアイコンにバルーンを出す。アイコンがないときは出せない
    fn balloon(&mut self, notice: &Notice) {
        if self.shown.is_none() {
            return;
        }
        let mut nid = self.nid();
        nid.uFlags = NIF_INFO;
        nid.dwInfoFlags = if notice.kind == Kind::Warning { NIIF_WARNING } else { NIIF_INFO };
        copy_wide(&mut nid.szInfoTitle, "INZONE Buds");
        copy_wide(&mut nid.szInfo, &notice.text);
        if !unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) }.as_bool() {
            tracing::warn!("通知を表示できません: {}", notice.text);
        }
    }

    /// デバウンスの確定時刻にタイマーを合わせる。早く来ても `App::tick` が何もせず、次を合わせ直す
    fn rearm(&self) {
        unsafe {
            match self.app.deadline() {
                Some(d) => {
                    let ms = d.saturating_duration_since(Instant::now()).as_millis() as u32 + 1;
                    SetTimer(Some(self.hwnd), DEBOUNCE_TIMER, ms, None);
                }
                None => {
                    let _ = KillTimer(Some(self.hwnd), DEBOUNCE_TIMER);
                }
            }
        }
    }

    /// 開くたびに再生デバイスを列挙し直す。抜き差しされたデバイスを反映するため
    fn menu(&self) -> Vec<Item> {
        let (endpoints, current) = self.app.endpoints().unwrap_or_else(|e| {
            tracing::warn!("再生デバイスを取得できません: {e:#}");
            (Vec::new(), None)
        });
        view::menu(
            self.app.config(),
            &endpoints,
            current.as_deref(),
            Status::of(self.app.dongle(), self.app.link()),
            self.app.battery(),
            crate::autostart::is_enabled(),
        )
    }

    fn set(&mut self, setting: Setting) {
        tracing::info!("設定を変更: {setting:?}");
        self.app.update_config(|c| setting.apply(c));
    }

    fn nid(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: ICON_ID,
            ..Default::default()
        }
    }

    /// 表示が変わったときだけシェルに渡す。追加に失敗したら、次のイベントで追加し直す
    fn show(&mut self) {
        let status = Status::of(self.app.dongle(), self.app.link());
        let tip = view::tooltip(self.app.battery());
        if self.shown.as_ref().is_some_and(|(s, t)| *s == status && *t == tip) {
            return;
        }
        let mut nid = self.nid();
        nid.uFlags = NIF_ICON | NIF_TIP | NIF_MESSAGE | NIF_SHOWTIP;
        nid.uCallbackMessage = WM_TRAY;
        nid.hIcon = self.icons.get(status);
        copy_wide(&mut nid.szTip, &tip);
        let ok = unsafe {
            if self.shown.is_none() {
                nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
                Shell_NotifyIconW(NIM_ADD, &nid).as_bool()
                    && Shell_NotifyIconW(NIM_SETVERSION, &nid).as_bool()
            } else {
                Shell_NotifyIconW(NIM_MODIFY, &nid).as_bool()
            }
        };
        if ok {
            if self.shown.is_none() {
                tracing::info!("トレイアイコンを追加しました");
            }
            tracing::info!("表示: {status:?} / {tip}");
            self.shown = Some((status, tip));
        } else {
            tracing::warn!("トレイアイコンを更新できません");
            self.shown = None;
        }
    }

    fn remove(&mut self) {
        if self.shown.take().is_some() {
            let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &self.nid()) };
        }
    }
}
