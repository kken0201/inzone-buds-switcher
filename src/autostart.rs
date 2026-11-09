//! Windows 起動時の自動開始。HKCU の Run キーに登録する（管理者権限は不要。docs/design.md 4.7）。

use anyhow::{Context, Result};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, WIN32_ERROR};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegDeleteValueW, RegOpenKeyExW,
    RegQueryValueExW, RegSetValueExW,
};
use windows::core::{PCWSTR, w};

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const VALUE: PCWSTR = w!("inzone-buds-switcher");

struct Key(HKEY);

impl Key {
    fn open(access: windows::Win32::System::Registry::REG_SAM_FLAGS) -> Result<Self> {
        let mut key = HKEY::default();
        unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, None, access, &mut key) }
            .ok()
            .context("Run キーを開けません")?;
        Ok(Self(key))
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        let _ = unsafe { RegCloseKey(self.0) };
    }
}

/// 値があれば有効とみなす。exe を移動した後の古いパスでも、メニューで外せるようにするため
pub fn is_enabled() -> bool {
    let Ok(key) = Key::open(KEY_QUERY_VALUE) else { return false };
    unsafe { RegQueryValueExW(key.0, VALUE, None, None, None, None) }.is_ok()
}

/// 有効にするときは、実行中の exe のパスで上書きする
pub fn set(enabled: bool) -> Result<()> {
    let key = Key::open(KEY_SET_VALUE)?;
    if enabled {
        let exe = std::env::current_exe().context("exe のパスを取得できません")?;
        // 空白を含むパスでも起動できるよう引用符で囲む
        let data: Vec<u8> = format!("\"{}\"", exe.display())
            .encode_utf16()
            .chain(Some(0))
            .flat_map(u16::to_le_bytes)
            .collect();
        unsafe { RegSetValueExW(key.0, VALUE, None, REG_SZ, Some(&data)) }
            .ok()
            .context("Run キーに登録できません")
    } else {
        let err: WIN32_ERROR = unsafe { RegDeleteValueW(key.0, VALUE) };
        // 元からなければ目的は達成している
        if err == ERROR_FILE_NOT_FOUND { Ok(()) } else { err.ok().context("Run キーから外せません") }
    }
}
