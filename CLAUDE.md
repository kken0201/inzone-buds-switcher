# CLAUDE.md

## プロジェクト概要

Sony INZONE Buds（USB ドングル接続）のイヤホンが接続/切断されたら、Windows の既定の出力デバイスを自動で切り替え、通知を出す常駐トレイアプリ。トレイにはバッテリー残量も表示する。

- 設計：[docs/design.md](docs/design.md)
- HID プロトコル：[docs/protocol.md](docs/protocol.md)（実機で確認した内容。未確認の項目はそう明記している）

## 方針・制約

- **Windows 11 専用**。他の OS 向けの抽象化はしない
- **Rust で書く**。ランタイム不要の単一 exe として配布できるため
- **配布物は exe 1 つ**。ランタイムやインストーラーは不要にする（CRT は静的リンク）。技術に詳しくない人にも配れるようにする
- 設定画面は作らず、トレイメニューで完結させる
- 誤って切り替えないことを優先する：状態が不明なときは何もしない

## 開発の進め方

- 作業計画とタスクは GitHub の Milestone / Issue / [Project](https://github.com/users/kken0201/projects/2) で管理する。docs/ には設計と仕様だけを書き、TODO や進捗は書かない
- 実機で確認できた HID の仕様は docs/protocol.md に反映する
- `hid/protocol.rs`（レポート解析）、`state.rs`（状態機械）、`switch.rs`（切り替え先の判断）、`tray/view.rs`（ツールチップとメニューの中身）は純粋関数にして単体テストを書く
- コード中のコメントは日本語でよい

## コマンド

```
cargo build
cargo test
cargo run                                 # トレイに常駐して自動で切り替える（`run` と同じ）
cargo run -- probe                        # HID の生レポートを出力
cargo run -- probe --battery 5            # 5 秒ごとにバッテリーを問い合わせる
cargo run -- probe --query 02,04          # 指定したサブコマンドを 1 回ずつ問い合わせる
cargo run -- watch                        # HID 監視スレッドのイベントをログに出す（RUST_LOG=debug で生レポートも）
cargo run -- list                         # 再生デバイスの一覧（既定に * が付く）
cargo run -- set <id> [--communications]  # 既定の再生デバイスを設定
cargo build --release                     # 配布用 exe
```

リリースは `v<version>` のタグを push すると GitHub Actions が行う（手順は docs/design.md 6）。

## 参考実装（どちらも MIT）

- https://github.com/huequica/buds-watcher — Python。状態レポートによる接続/切断の検知
- https://github.com/takamachi66/inzone-buds-battery-tray — Rust。能動的な問い合わせによるバッテリー取得

コードを流用する場合は README にクレジットとライセンスを書く。
