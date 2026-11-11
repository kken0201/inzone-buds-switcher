# セキュリティポリシー

## サポートしている版

セキュリティの修正は最新のリリースにだけ行います。古い版を使っている場合は、[最新のリリース](https://github.com/kken0201/inzone-buds-switcher/releases/latest)に更新してください。

## 脆弱性の報告

脆弱性を見つけたら、公開の Issue には書かず、GitHub の非公開の報告機能で知らせてください。

1. [Security タブ](https://github.com/kken0201/inzone-buds-switcher/security)を開きます
2. 「Report a vulnerability」をクリックし、内容を書いて送ります

再現手順、影響を受ける版、想定される影響を書いてもらえると助かります。個人で開発しているため、返信までに時間がかかることがあります。

## 配布物の確認

リリースの exe には、GitHub Actions でビルドしたことを示す来歴（attestation）を付けています。ダウンロードしたファイルは次のコマンドで確かめられます。

```
gh attestation verify inzone-buds-switcher.exe --repo kken0201/inzone-buds-switcher
```
