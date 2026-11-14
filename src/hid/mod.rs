//! ドングルの HID コレクションの列挙と読み書き。
//!
//! hidapi は使わず、Win32 API（cfgmgr32 / hid.dll / ReadFile・WriteFile）を直接呼ぶ。
//! このドングルは文字列ディスクリプタ（製造元・製品名・シリアル）の要求に応答せず、
//! 1 回ごとに約 5 秒でタイムアウトする（docs/protocol.md）。hidapi は列挙や初期化の際に
//! これを問い合わせるため、使い物にならないほど遅くなる。

pub mod protocol;
pub mod watch;

use anyhow::{Context, Result, bail};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_Get_Device_Interface_List_SizeW,
    CM_Get_Device_Interface_ListW, CR_SUCCESS,
};
use windows::Win32::Devices::HumanInterfaceDevice::{
    HIDP_CAPS, HidD_FreePreparsedData, HidD_GetHidGuid, HidD_GetPreparsedData, HidP_GetCaps,
    PHIDP_PREPARSED_DATA,
};
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    ReadFile, WriteFile,
};
use windows::core::{HSTRING, PCWSTR};

pub const VID: u16 = 0x054C;
pub const PID: u16 = 0x0EC2;

/// 状態レポートとバッテリーの問い合わせに使うベンダー定義コレクション（docs/protocol.md）
pub const HUB_USAGE_PAGE: u16 = 0xFF04;
pub const HUB_USAGE: u16 = 0x0001;

#[derive(Debug, Clone)]
pub struct Collection {
    pub path: String,
    pub usage_page: u16,
    pub usage: u16,
    /// レポート ID を含むバイト数（HIDP_CAPS の値そのまま）
    pub input_len: u16,
    pub output_len: u16,
    pub feature_len: u16,
}

/// 接続中のドングルの HID コレクションを列挙する
pub fn enumerate() -> Result<Vec<Collection>> {
    let needle = format!("vid_{VID:04x}&pid_{PID:04x}");
    let mut out = Vec::new();
    for path in interface_paths()? {
        if !path.to_ascii_lowercase().contains(&needle) {
            continue;
        }
        if let Some(c) = query_caps(&path) {
            out.push(c);
        }
    }
    Ok(out)
}

fn interface_paths() -> Result<Vec<String>> {
    unsafe {
        let guid = HidD_GetHidGuid();
        let mut len = 0u32;
        let cr = CM_Get_Device_Interface_List_SizeW(
            &mut len,
            &guid,
            PCWSTR::null(),
            CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
        );
        if cr != CR_SUCCESS {
            bail!("CM_Get_Device_Interface_List_SizeW 失敗: {cr:?}");
        }
        let mut buf = vec![0u16; len as usize];
        let cr = CM_Get_Device_Interface_ListW(
            &guid,
            PCWSTR::null(),
            &mut buf,
            CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
        );
        if cr != CR_SUCCESS {
            bail!("CM_Get_Device_Interface_ListW 失敗: {cr:?}");
        }
        Ok(buf
            .split(|&c| c == 0)
            .filter(|s| !s.is_empty())
            .map(String::from_utf16_lossy)
            .collect())
    }
}

/// アクセス権 0 で開いて usage を調べる（文字列ディスクリプタは問い合わせない）
fn query_caps(path: &str) -> Option<Collection> {
    let h = open_shared(path, 0, FILE_FLAGS_AND_ATTRIBUTES(0)).ok()?;
    unsafe {
        let mut pp = PHIDP_PREPARSED_DATA::default();
        let mut result = None;
        if HidD_GetPreparsedData(h, &mut pp) {
            let mut caps = HIDP_CAPS::default();
            if HidP_GetCaps(pp, &mut caps).is_ok() {
                result = Some(Collection {
                    path: path.to_string(),
                    usage_page: caps.UsagePage,
                    usage: caps.Usage,
                    input_len: caps.InputReportByteLength,
                    output_len: caps.OutputReportByteLength,
                    feature_len: caps.FeatureReportByteLength,
                });
            }
            let _ = HidD_FreePreparsedData(pp);
        }
        let _ = CloseHandle(h);
        result
    }
}

/// INZONE Hub などほかのアプリと同時に開けるよう、共有モードで開く
fn open_shared(path: &str, access: u32, flags: FILE_FLAGS_AND_ATTRIBUTES) -> Result<HANDLE> {
    unsafe {
        CreateFileW(&HSTRING::from(path), access, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, flags, None)
    }
    .with_context(|| format!("{path} を開けません"))
}

/// 読み書き用に開く
fn open_rw(path: &str, flags: FILE_FLAGS_AND_ATTRIBUTES) -> Result<HANDLE> {
    open_shared(path, (GENERIC_READ | GENERIC_WRITE).0, flags)
}

/// 開いた HID コレクション。読み書きはブロッキング（同期 I/O）
pub struct Device {
    handle: HANDLE,
}

// HANDLE はカーネルオブジェクトの番号で、どのスレッドから使ってもよい
unsafe impl Send for Device {}

impl Device {
    pub fn open(path: &str) -> Result<Self> {
        Ok(Self { handle: open_rw(path, FILE_FLAGS_AND_ATTRIBUTES(0))? })
    }

    /// 入力レポートを 1 つ読む。`buf` は入力レポート長（レポート ID 込み）以上にする
    pub fn read(&self, buf: &mut [u8]) -> Result<usize> {
        let mut n = 0u32;
        unsafe { ReadFile(self.handle, Some(buf), Some(&mut n), None) }.context("ReadFile 失敗")?;
        Ok(n as usize)
    }

    /// 出力レポートを送る。`data` の長さは出力レポート長（レポート ID 込み）に合わせる
    pub fn write(&self, data: &[u8]) -> Result<usize> {
        let mut n = 0u32;
        unsafe { WriteFile(self.handle, Some(data), Some(&mut n), None) }
            .context("WriteFile 失敗")?;
        Ok(n as usize)
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.handle) };
    }
}
