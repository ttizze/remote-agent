# T3 移植の実装状況

範囲は [PLAN.md](PLAN.md)、設計と実装時の判断は [ARCHITECTURE.md](ARCHITECTURE.md)、T3 のテストとの対応は [PORT_MAP.md](PORT_MAP.md) に記録する。

| 段階 | 状況 | 実装範囲 |
| --- | --- | --- |
| 1 | 完了 | `agent-domain`: ID、コマンド、事実、スレッドの状態機械、fold |
| 2 | 完了 | `agent-providers`: Codex app-server と Claude の翻訳、固定版の replay fixture |
| 3 | 完了 | `agent-runtime`: actor、SQLite の事実ログ、outbox、provider session、同期、履歴の取り込み。`host-daemon` の `conversation` が RPC へ接続する |
| 4 | 完了 | `agent-core` の同期・outbox・表示用データと UniFFI、desktop・iOS・Android。未接続の項目は PORT_MAP の「段階 4 の統合」に記録する |
| 5 | 完了 | 旧 `crates/orchestration` と `crates/provider-adapters`、旧 `orchestration/*` RPC、cwd から作る terminal handle、旧ランタイムの文書の削除 |
| M3 | 統合中・owner follow-up と最終検証待ち | Git/VCS、GitHub PR、scheduled tasks、usage、settings/platform、worktree、Preview/device、push/activity、updater/release。Android の Git、Material You、drag arrangement、artifact/citation と desktop の minimap/assistant citations を含む。対応先と境界は `PORT_MAP.md` の段階 6 に記録する |

会話の RPC の ALPN は `remote-agent/streams/14`。旧形式の互換性・移行は設けない。iroh、QR ペアリング、provider プロセス管理、terminal・files・browser・dictation・accounts・既存 worktree 機能は維持する。

Claude 接続は公式の `@anthropic-ai/claude-agent-sdk@0.3.293` を Node 子プロセスの `crates/host-daemon/src/claude/sdk/bridge.mjs` から直接呼ぶ。bridge は SDK の公開 `query()` API と必要な query 操作だけを使い、Rust は共通イベントへの翻訳・会話状態・保存を担当する。依存とライセンスは npm lockfile の integrity・metadata に従って配布物へ含める。Host 上には Node.js 18 以上が必要（開発環境は Nix の Node を使う）。SDK 内部の制御・fork の履歴変換を Rust に移植した実装は持たない。

## 検証方針

変更 crate の単体・property test、Rust lint、Host/GPUI の build、iOS Rust/Swift の build、Android の単体テストと assembleDebug を行う。CI の結果待ち、cargo-mutants、live-provider E2E、Simulator UI テストは行わない。pixel・実機操作の受入確認はこのビルド検証に含めない。稼働中 Host を再起動せず、main と他 worktree は変更しない。

2026-10-08 の統合と SDK 接続で確認した結果（dated checkpoint の履歴）:

これらは途中のチェックポイントでの結果であり、M3 最終統合 head の証明ではない。現在の最終 head では、全 unit test、`-D warnings` clippy、fmt、Host/GPUI、iOS（共有・Activity・Share Extension）、Android、UniFFI 再生成の確認を保留している。外部 provider、実機・実端末、稼働中 Host、signing、CI 待ちの検証は行わない。

- `scripts/dev-env.sh just unit-tests`: Rust 3,370件が通過、手動・外部の5件は skip。agent-peer の5群と、公式 SDK の Node テスト8件も通過した。
- workspace の clippy（全 target、`-D warnings`）、fmt、Host・GPUI・UniFFI の build が通過した。
- iOS の Rust・Swift バインディングと Simulator 向け app build、Swiftformat・Swiftlint が通過した。
- Android の両 ABI の Rust build、ktfmt・detekt、単体テスト7件、assembleDebug が通過した。Kotlin の最終確認では更新済みの共通バインディングと JNI を再利用した。
- SDK・worker・fixture だけを `node_modules` のない場所へ置いても、SDK テスト8件が通過した。モデルや外部サービスは呼んでいない。

macOS の SystemConfiguration は初回に実行ファイルの directory を CFBundle として走査する。Cargo の unpacked debug objects が多いと、通信テストの処理前に期限を超えた。Nextest の macOS runner は同じ inode の executable と supervisor を小さな directory に置いて実行する。期待値と timeout は変更していない。
