# bex

[English](README.md) | [日本語](README.ja.md)

bexは、Macで動くCodexやClaude Codeを、MacとiPhoneから操作できるアプリです。どちらの端末からも会話を続けたり、コードの変更を確認したり、ターミナルやブラウザを使ったりできます。

Macで設定済みのエージェントのアカウントと、既存の会話をそのまま使えます。エージェントの実行とプロジェクトのファイル管理はMacが担当し、iPhoneからそこへ接続します。

## ダウンロード

現在、動作を確認して使える状態なのはMac版とiPhone版です。まだ開発中のため、不具合が残っています。

| アプリ | 必要な環境 | 入手先 |
| --- | --- | --- |
| Mac | Apple Silicon・macOS 26 | [getbex.dev](https://getbex.dev/) |
| iPhone | iOS 26 | [TestFlight](https://testflight.apple.com/join/UJbK9Ww7) |

Windows版・Android版も開発していますが、十分な動作確認はできていません。

## 使い始める

1. MacにGitと、CodexまたはClaude Codeをインストールし、利用するエージェントへログインします。
2. Macでbexを開きます。初回設定でエージェントの確認と既存プロジェクトの取り込みを行うか、作業するフォルダーを選びます。
3. エージェントとモデルを選び、会話を始めます。
4. iPhoneから使う場合は、Macでペアリング用の招待を表示し、iPhoneアプリでQRコードを読み取ります。招待の内容を貼り付けることもできます。

iPhoneから使っている間は、Macをスリープさせず、bexを起動したままにしてください。招待は1台限りです。別の端末には新しい招待を発行します。接続した端末からMacのエージェントやファイルを操作できるため、信頼できる端末だけを登録してください。

## 開発

NixとGitを用意し、`scripts/dev-env.sh`経由でプロジェクトに固定された環境を使います。

```sh
scripts/dev-env.sh just --list
scripts/dev-env.sh just check -p agent-core
scripts/dev-env.sh just unit-tests
scripts/dev-env.sh just complexity
```

macOSでは、`scripts/dev-env.sh just dev`で開発用アプリをビルドして起動できます。エージェントを動かすバックグラウンドプロセス（Host）は開発用に分かれますが、既存のアカウントと会話履歴は共有します。

ソースから更新する場合は、`main`をpullし、通常どおりアプリまたはHostをビルドして再起動してください。新版Hostの初回起動時に、既存の会話データをDBと同じ場所の`before-storage-unification-*`ディレクトリへバックアップし、自動で更新します。履歴が大きい場合、初回の起動には時間がかかります。別の変換ツールを実行する必要はありません。

開発・統合のルールは[AGENTS.md](AGENTS.md)、ビルド・検証コマンドは[justfile](justfile)を参照してください。

## 関連情報

- [サポート・接続のトラブル](docs/SUPPORT.md)
- [変更履歴](CHANGELOG.md)
- [プライバシー](docs/PRIVACY.md)
- [脆弱性の報告](SECURITY.md)
- [Windows版の開発・インストール](docs/WINDOWS.md)
- [Nightly・本番・iPhone TestFlight／Devの配布](docs/RELEASES.md)
