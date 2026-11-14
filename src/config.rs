//! 設定ファイル `%APPDATA%\inzone-buds-switcher\config.json`（docs/design.md 4.6）。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRef {
    pub id: String,
    /// 表示用。照合には `id` だけを使う
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub auto_switch: bool,
    /// None ならドングルの Game デバイスを使う
    pub connected_device: Option<DeviceRef>,
    pub disconnected_device: Option<DeviceRef>,
    pub switch_communications: bool,
    pub notify_switch: bool,
    pub notify_battery: bool,
    pub debounce_ms: u64,
    pub low_battery_threshold: u8,
    /// 新しい版を GitHub Releases から取ってきて置き換える（docs/design.md 4.8）
    pub auto_update: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            auto_switch: true,
            connected_device: None,
            disconnected_device: None,
            switch_communications: false,
            notify_switch: true,
            notify_battery: true,
            debounce_ms: 1500,
            low_battery_threshold: 5,
            auto_update: true,
        }
    }
}

/// 欠けている項目は既定値、知らない項目は無視する
pub fn parse(text: &str) -> serde_json::Result<Config> {
    // メモ帳は BOM 付きで保存することがある
    serde_json::from_str(text.strip_prefix('\u{feff}').unwrap_or(text))
}

pub fn to_json(config: &Config) -> String {
    serde_json::to_string_pretty(config).expect("Config は必ず JSON にできる")
}

pub fn path() -> Result<PathBuf> {
    let appdata = std::env::var_os("APPDATA").context("APPDATA が設定されていません")?;
    Ok(PathBuf::from(appdata).join("inzone-buds-switcher").join("config.json"))
}

/// ファイルがなければ既定値。壊れていれば `config.json.bad` に退避して既定値を使う。
/// 退避するのは、あとで保存したときに利用者が書いた内容を黙って上書きしないため
pub fn load(path: &Path) -> Config {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Config::default(),
        Err(e) => {
            tracing::warn!("{} を読めないため既定の設定を使います: {e}", path.display());
            return Config::default();
        }
    };
    match parse(&text) {
        Ok(c) => c,
        Err(e) => {
            let bad = path.with_extension("json.bad");
            tracing::warn!(
                "{} を解釈できないため {} に退避して既定の設定を使います: {e}",
                path.display(),
                bad.display()
            );
            if let Err(e) = std::fs::rename(path, &bad) {
                tracing::warn!("退避に失敗: {e}");
            }
            Config::default()
        }
    }
}

/// 一時ファイルに書いてから置き換える。途中で落ちても壊れた設定を残さないため
pub fn save(path: &Path, config: &Config) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("{} を作れません", dir.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, to_json(config)).with_context(|| format!("{} に書けません", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("{} を置き換えられません", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::temp_dir;

    fn dev(id: &str, name: &str) -> DeviceRef {
        DeviceRef { id: id.into(), name: name.into() }
    }

    #[test]
    fn parses_design_example() {
        let c = parse(
            r#"{
              "auto_switch": false,
              "connected_device": { "id": "{0.0.0.00000000}.{a}", "name": "INZONE Buds - Game" },
              "disconnected_device": { "id": "{0.0.0.00000000}.{b}", "name": "スピーカー" },
              "switch_communications": true,
              "notify_switch": false,
              "notify_battery": false,
              "debounce_ms": 2000,
              "low_battery_threshold": 10,
              "auto_update": false
            }"#,
        )
        .unwrap();
        assert_eq!(
            c,
            Config {
                auto_switch: false,
                connected_device: Some(dev("{0.0.0.00000000}.{a}", "INZONE Buds - Game")),
                disconnected_device: Some(dev("{0.0.0.00000000}.{b}", "スピーカー")),
                switch_communications: true,
                notify_switch: false,
                notify_battery: false,
                debounce_ms: 2000,
                low_battery_threshold: 10,
                auto_update: false,
            }
        );
    }

    #[test]
    fn missing_fields_take_defaults() {
        assert_eq!(parse("{}").unwrap(), Config::default());
        let c = parse(r#"{ "debounce_ms": 800 }"#).unwrap();
        assert_eq!(c, Config { debounce_ms: 800, ..Config::default() });
    }

    #[test]
    fn unknown_fields_are_ignored() {
        assert_eq!(parse(r#"{ "future_option": 1 }"#).unwrap(), Config::default());
    }

    #[test]
    fn null_devices_are_unset() {
        let c = parse(r#"{ "connected_device": null, "disconnected_device": null }"#).unwrap();
        assert_eq!(c.connected_device, None);
        assert_eq!(c.disconnected_device, None);
    }

    #[test]
    fn accepts_utf8_bom() {
        assert_eq!(parse("\u{feff}{}").unwrap(), Config::default());
    }

    #[test]
    fn rejects_broken_json() {
        assert!(parse("{ \"auto_switch\": ").is_err());
        assert!(parse(r#"{ "debounce_ms": "fast" }"#).is_err());
    }

    #[test]
    fn serialized_form_round_trips() {
        let c = Config { disconnected_device: Some(dev("x", "スピーカー")), ..Config::default() };
        assert_eq!(parse(&to_json(&c)).unwrap(), c);
    }

    #[test]
    fn load_missing_file_gives_defaults() {
        let dir = temp_dir("missing");
        assert_eq!(load(&dir.join("config.json")), Config::default());
    }

    #[test]
    fn load_broken_file_moves_it_aside() {
        let dir = temp_dir("broken");
        let path = dir.join("config.json");
        std::fs::write(&path, "{ broken").unwrap();
        assert_eq!(load(&path), Config::default());
        assert!(!path.exists());
        assert_eq!(std::fs::read_to_string(dir.join("config.json.bad")).unwrap(), "{ broken");
    }

    #[test]
    fn save_then_load_round_trips_and_leaves_no_temp_file() {
        let dir = temp_dir("save");
        let path = dir.join("sub").join("config.json");
        let first = Config { debounce_ms: 1, ..Config::default() };
        save(&path, &first).unwrap();
        let second = Config { disconnected_device: Some(dev("x", "y")), ..Config::default() };
        save(&path, &second).unwrap();
        assert_eq!(load(&path), second);
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["config.json"]);
    }
}
