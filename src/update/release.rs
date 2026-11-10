//! 最新リリースの情報から、更新するかと何を取ってくるかを決める（docs/design.md 4.8）。純粋関数。

use std::fmt;

use anyhow::{Context, Result};
use serde::Deserialize;

/// リリースに zip とは別に置く exe 単体の名前
pub const ASSET_NAME: &str = "inzone-buds-switcher.exe";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u32, pub u32, pub u32);

impl Version {
    /// `1.2.3` か `v1.2.3`。`-rc.1` などが付いたものや、数字 3 つでないものは None
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.strip_prefix('v').unwrap_or(s);
        let mut parts = s.split('.').map(|p| {
            if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) { p.parse().ok() } else { None }
        });
        let v = Self(parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(v)
    }

    pub fn current() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION")).expect("Cargo.toml の version は x.y.z")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub version: Version,
    pub url: String,
    /// ダウンロードした大きさと照らし合わせる
    pub size: u64,
    /// ダウンロードした中身の SHA-256 と照らし合わせる。GitHub が asset の `digest` に載せる値
    pub sha256: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    UpToDate(Version),
    Update(Target),
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    size: u64,
    browser_download_url: String,
    /// `sha256:<16 進 64 桁>`。2025 年 6 月より前に上げた asset には付かない
    digest: Option<String>,
}

/// `sha256:<16 進 64 桁>` を読む。それ以外の形式は None
fn parse_digest(s: &str) -> Option<[u8; 32]> {
    let hex = s.strip_prefix("sha256:")?.as_bytes();
    if hex.len() != 64 || !hex.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let mut out = [0u8; 32];
    for (o, pair) in out.iter_mut().zip(hex.chunks(2)) {
        // 16 進の文字だけなので UTF-8 として読め、2 文字なので u8 に収まる
        *o = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(out)
}

/// `releases/latest` の応答を解釈する。今の版より大きいときだけ `Update`
pub fn decide(json: &str, current: Version) -> Result<Decision> {
    let release: Release = serde_json::from_str(json).context("リリースの情報を解釈できません")?;
    let tag = &release.tag_name;
    let version = Version::parse(tag).with_context(|| format!("タグ {tag} を版として読めません"))?;
    if version <= current {
        return Ok(Decision::UpToDate(version));
    }
    let asset = release
        .assets
        .into_iter()
        .find(|a| a.name == ASSET_NAME)
        .with_context(|| format!("{tag} に {ASSET_NAME} がありません"))?;
    // 照合できないものでは置き換えない
    let sha256 = asset
        .digest
        .as_deref()
        .and_then(parse_digest)
        .with_context(|| format!("{tag} の {ASSET_NAME} に読める digest がありません"))?;
    Ok(Decision::Update(Target { version, url: asset.browser_download_url, size: asset.size, sha256 }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    const DIGEST: [u8; 32] = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29,
        30, 31,
    ];

    fn json(tag: &str, assets: &[&str]) -> String {
        let assets: Vec<String> = assets
            .iter()
            .map(|n| {
                format!(
                    r#"{{ "name": "{n}", "size": 123, "browser_download_url": "https://example.com/{n}", "digest": "sha256:{DIGEST_HEX}" }}"#
                )
            })
            .collect();
        format!(r#"{{ "tag_name": "{tag}", "draft": false, "assets": [{}] }}"#, assets.join(","))
    }

    #[test]
    fn parses_plain_and_v_prefixed_versions() {
        assert_eq!(Version::parse("1.2.3"), Some(Version(1, 2, 3)));
        assert_eq!(Version::parse("v0.10.0"), Some(Version(0, 10, 0)));
    }

    #[test]
    fn rejects_prerelease_and_malformed_versions() {
        for s in ["v1.2.3-rc.1", "v1.2", "v1.2.3.4", "v1..3", "1.2.x", "", "v", "+1.2.3", "v 1.2.3"] {
            assert_eq!(Version::parse(s), None, "{s}");
        }
    }

    #[test]
    fn compares_numerically() {
        assert!(Version(0, 10, 0) > Version(0, 9, 9));
        assert!(Version(1, 0, 0) > Version(0, 99, 99));
    }

    #[test]
    fn current_version_parses() {
        assert_eq!(Version::current().to_string(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn newer_release_picks_exe_asset() {
        let d = decide(&json("v0.2.0", &["inzone-buds-switcher-v0.2.0.zip", ASSET_NAME]), Version(0, 1, 0));
        assert_eq!(
            d.unwrap(),
            Decision::Update(Target {
                version: Version(0, 2, 0),
                url: format!("https://example.com/{ASSET_NAME}"),
                size: 123,
                sha256: DIGEST,
            })
        );
    }

    #[test]
    fn same_or_older_release_is_up_to_date() {
        let j = json("v0.2.0", &[ASSET_NAME]);
        assert_eq!(decide(&j, Version(0, 2, 0)).unwrap(), Decision::UpToDate(Version(0, 2, 0)));
        assert_eq!(decide(&j, Version(0, 3, 0)).unwrap(), Decision::UpToDate(Version(0, 2, 0)));
    }

    #[test]
    fn up_to_date_does_not_need_exe_asset() {
        let j = json("v0.1.0", &["inzone-buds-switcher-v0.1.0.zip"]);
        assert_eq!(decide(&j, Version(0, 1, 0)).unwrap(), Decision::UpToDate(Version(0, 1, 0)));
    }

    #[test]
    fn newer_release_without_exe_is_an_error() {
        assert!(decide(&json("v0.2.0", &["inzone-buds-switcher-v0.2.0.zip"]), Version(0, 1, 0)).is_err());
    }

    #[test]
    fn unreadable_tag_or_json_is_an_error() {
        assert!(decide(&json("v0.2.0-rc.1", &[ASSET_NAME]), Version(0, 1, 0)).is_err());
        assert!(decide(r#"{ "message": "Not Found" }"#, Version(0, 1, 0)).is_err());
    }

    #[test]
    fn parses_sha256_digest() {
        assert_eq!(parse_digest(&format!("sha256:{DIGEST_HEX}")), Some(DIGEST));
        assert_eq!(parse_digest(&format!("sha256:{}", DIGEST_HEX.to_uppercase())), Some(DIGEST));
    }

    #[test]
    fn rejects_malformed_digest() {
        for s in [
            DIGEST_HEX.to_string(),
            format!("sha512:{DIGEST_HEX}"),
            format!("sha256:{}", &DIGEST_HEX[2..]),
            format!("sha256:{DIGEST_HEX}00"),
            format!("sha256:{}zz", &DIGEST_HEX[2..]),
            format!("sha256:{}+f", &DIGEST_HEX[2..]),
        ] {
            assert_eq!(parse_digest(&s), None, "{s}");
        }
    }

    #[test]
    fn newer_release_without_digest_is_an_error() {
        let j = json("v0.2.0", &[ASSET_NAME]).replace(&format!(r#", "digest": "sha256:{DIGEST_HEX}""#), "");
        assert!(decide(&j, Version(0, 1, 0)).is_err());
    }
}
