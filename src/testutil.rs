//! 複数のモジュールのテストで使う部品。

use std::path::PathBuf;

use crate::audio::{DongleKind, Endpoint};
use crate::config::DeviceRef;

/// 表示名は `<id> name`
pub fn ep(id: &str, kind: Option<DongleKind>) -> Endpoint {
    Endpoint { id: id.into(), name: format!("{id} name"), kind }
}

/// `ep(id, ..)` を設定に保存した形
pub fn dev(id: &str) -> DeviceRef {
    DeviceRef { id: id.into(), name: format!("{id} name") }
}

/// スピーカーと、ドングルの Chat・Game
pub fn endpoints() -> Vec<Endpoint> {
    vec![ep("speaker", None), ep("chat", Some(DongleKind::Chat)), ep("game", Some(DongleKind::Game))]
}

/// テストごとの空ディレクトリ。`name` はテストの間で重ならないようにする。%APPDATA% などには触れない
pub fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("inzone-buds-switcher-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
