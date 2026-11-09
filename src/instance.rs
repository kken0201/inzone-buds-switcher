//! 二重起動の防止（docs/design.md 4.7）。

use anyhow::Result;
use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::w;

/// 持っている間、他の起動を防ぐ
pub struct Guard(HANDLE);

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// 既に起動していれば None。`Local\` はログオンセッションごとなので、
/// 別のユーザーが同時にログオンしていても、それぞれ 1 つ起動できる
pub fn acquire() -> Result<Option<Guard>> {
    let handle = unsafe { CreateMutexW(None, false, w!(r"Local\inzone-buds-switcher")) }?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        let _ = unsafe { CloseHandle(handle) };
        return Ok(None);
    }
    Ok(Some(Guard(handle)))
}
