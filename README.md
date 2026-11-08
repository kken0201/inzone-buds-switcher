# inzone-buds-switcher

Sony INZONE Buds（WF-G700N）を USB ドングルで使うための Windows 11 用常駐アプリです。

- 🎧 イヤホンをケースから出すと、出力を INZONE Buds に自動で切り替えます
- 🔈 ケースに戻すと、元の出力（スピーカーなど）に自動で戻します
- 🔔 切り替えたときに通知します
- 🔋 システムトレイに左右イヤホンとケースのバッテリー残量を表示します

> 🚧 開発前です。設計は [docs/design.md](docs/design.md) を参照してください。

## 使い方（予定）

1. Releases から `inzone-buds-switcher.exe` をダウンロード
2. ダブルクリックで起動（インストール不要）
   - 「WindowsによってPCが保護されました」と出たら「詳細情報」→「実行」
3. トレイアイコンを右クリックし、「接続時の出力」と「切断時の出力」を選ぶ

## 謝辞

HID プロトコルの情報は以下のプロジェクトを参考にしています（どちらも MIT License）。

- [huequica/buds-watcher](https://github.com/huequica/buds-watcher)
- [takamachi66/inzone-buds-battery-tray](https://github.com/takamachi66/inzone-buds-battery-tray)

本ソフトウェアはソニーの公式製品ではありません。
