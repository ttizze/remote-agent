# T3 移植の実装状況

範囲は [PLAN.md](PLAN.md)、設計と実装時の判断は [ARCHITECTURE.md](ARCHITECTURE.md)、T3 のテストとの対応は [PORT_MAP.md](PORT_MAP.md) に記録する。

| 段階 | 状況 | 実装範囲 |
| --- | --- | --- |
| 1 | 完了 | `agent-domain`: ID、コマンド、事実、スレッドの状態機械、fold |
| 2 | 完了 | `agent-providers`: Codex app-server と Claude の翻訳、固定版の replay fixture |
| 3 | 完了 | `agent-runtime`: actor、SQLite の事実ログ、outbox、provider session、同期、履歴の取り込み。`host-daemon` の `conversation` が RPC へ接続する |
| 4 | 完了 | `agent-core` の同期・outbox・表示用データと UniFFI、desktop・iOS・Android。未接続の項目は PORT_MAP の「段階 4 の統合」に記録する |
| 5 | 完了 | 旧 `crates/orchestration` と `crates/provider-adapters`、旧 `orchestration/*` RPC、cwd から作る terminal handle、旧ランタイムの文書の削除 |
| M3 | 未着手 | T3 の Git/worktree 操作、GitHub PR 連携、scheduled tasks、usage の拡張、全設定、Nightly 配布 |

会話の RPC の ALPN は `remote-agent/streams/13`。旧形式の互換性・移行は設けない。iroh、QR ペアリング、provider プロセス管理、terminal・files・browser・dictation・accounts・既存 worktree 機能は維持する。

Claude 接続は固定版の Agent SDK を Node 子プロセスから直接呼ぶ。SDK 本体とライセンスは npm lockfile に従って vendoring し、Host に埋め込む。Host 上には Node.js 18 以上が必要（開発環境は Nix の Node を使う）。SDK 内部の制御・fork の履歴変換の Rust 再実装は削除した。

## 検証方針

変更 crate の単体・property test、Rust lint、Host/GPUI の build、iOS Rust/Swift の build、Android の単体テストと assembleDebug を行う。CI の結果待ち、cargo-mutants、live-provider E2E、Simulator UI テストは行わない。pixel・実機操作の受入確認はこのビルド検証に含めない。稼働中 Host を再起動せず、main と他 worktree は変更しない。

2026-10-08 の統合と SDK 接続で確認した結果:

- `scripts/dev-env.sh just unit-tests`: Rust 3,370件が通過、手動・外部の5件は skip。agent-peer の5群と、公式 SDK の Node テスト8件も通過した。
- workspace の clippy（全 target、`-D warnings`）、fmt、Host・GPUI・UniFFI の build が通過した。
- iOS の Rust・Swift バインディングと Simulator 向け app build、Swiftformat・Swiftlint が通過した。
- Android の両 ABI の Rust build、ktfmt・detekt、単体テスト7件、assembleDebug が通過した。Kotlin の最終確認では更新済みの共通バインディングと JNI を再利用した。
- SDK・worker・fixture だけを `node_modules` のない場所へ置いても、SDK テスト8件が通過した。モデルや外部サービスは呼んでいない。

macOS の SystemConfiguration は初回に実行ファイルの directory を CFBundle として走査する。Cargo の unpacked debug objects が多いと、通信テストの処理前に期限を超えた。Nextest の macOS runner は同じ inode の executable と supervisor を小さな directory に置いて実行する。期待値と timeout は変更していない。
