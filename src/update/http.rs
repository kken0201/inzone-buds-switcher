//! WinHTTP による HTTPS の GET（docs/design.md 4.8）。プロキシは OS の設定に従い、リダイレクトは WinHTTP が追う。

use std::ffi::c_void;

use anyhow::{Context, Result, ensure};
use windows::Win32::Networking::WinHttp::{
    INTERNET_DEFAULT_HTTPS_PORT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen,
    WinHttpOpenRequest, WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, WinHttpSetTimeouts,
};
use windows::core::{HSTRING, PCWSTR, w};

/// exe は 1 MB 前後。壊れた応答で際限なく読まないための上限
const MAX_BYTES: usize = 32 * 1024 * 1024;
/// 名前解決・接続・送信・受信それぞれの上限（ミリ秒）
const TIMEOUT_MS: i32 = 30_000;

struct Handle(*mut c_void);

impl Handle {
    fn new(h: *mut c_void) -> windows::core::Result<Self> {
        if h.is_null() { Err(windows::core::Error::from_thread()) } else { Ok(Self(h)) }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        let _ = unsafe { WinHttpCloseHandle(self.0) };
    }
}

/// `https://host/path` を (host, path) に分ける。path がなければ `/`
fn split_https(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("https://")?;
    let (host, path) = match rest.find('/') {
        Some(i) => rest.split_at(i),
        None => (rest, "/"),
    };
    (!host.is_empty() && !host.contains([':', '@', '?', '#'])).then_some((host, path))
}

/// 200 以外はエラー
pub fn get(url: &str, accept: &str) -> Result<Vec<u8>> {
    let (host, path) = split_https(url).with_context(|| format!("https の URL として扱えません: {url}"))?;
    let agent = HSTRING::from(concat!("inzone-buds-switcher/", env!("CARGO_PKG_VERSION")));
    unsafe {
        let session = Handle::new(WinHttpOpen(
            &agent,
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ))
        .context("WinHTTP を開けません")?;
        WinHttpSetTimeouts(session.0, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS)?;
        let connect = Handle::new(WinHttpConnect(session.0, &HSTRING::from(host), INTERNET_DEFAULT_HTTPS_PORT, 0))
            .with_context(|| format!("{host} に接続できません"))?;
        let request = Handle::new(WinHttpOpenRequest(
            connect.0,
            w!("GET"),
            &HSTRING::from(path),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ))?;
        let headers: Vec<u16> = format!("Accept: {accept}\r\n").encode_utf16().collect();
        WinHttpSendRequest(request.0, Some(&headers), None, 0, 0, 0).with_context(|| format!("送信できません: {url}"))?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).with_context(|| format!("応答がありません: {url}"))?;

        let mut status = 0u32;
        let mut len = size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut c_void),
            &mut len,
            std::ptr::null_mut(),
        )?;
        ensure!(status == 200, "HTTP {status}: {url}");

        let mut body = Vec::new();
        loop {
            let mut available = 0u32;
            WinHttpQueryDataAvailable(request.0, &mut available)?;
            if available == 0 {
                return Ok(body);
            }
            let start = body.len();
            ensure!(start + available as usize <= MAX_BYTES, "応答が大きすぎます: {url}");
            body.resize(start + available as usize, 0);
            let mut read = 0u32;
            WinHttpReadData(request.0, body[start..].as_mut_ptr() as *mut c_void, available, &mut read)?;
            body.truncate(start + read as usize);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_host_and_path() {
        assert_eq!(
            split_https("https://api.github.com/repos/a/b/releases/latest"),
            Some(("api.github.com", "/repos/a/b/releases/latest"))
        );
        assert_eq!(split_https("https://example.com"), Some(("example.com", "/")));
    }

    #[test]
    fn rejects_non_https_and_odd_hosts() {
        for url in ["http://example.com/", "https:///x", "https://user@example.com/", "https://example.com:8443/"] {
            assert_eq!(split_https(url), None, "{url}");
        }
    }

    /// ネットワークに出るので既定では走らせない。`cargo test -- --ignored` で確かめる
    #[test]
    #[ignore]
    fn fetches_public_release() {
        let body = get("https://api.github.com/repos/huequica/buds-watcher", "application/vnd.github+json").unwrap();
        assert!(String::from_utf8(body).unwrap().contains("\"full_name\""));
    }
}
