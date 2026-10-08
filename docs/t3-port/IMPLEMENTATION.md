# T3 移植の実装状況

範囲は [PLAN.md](PLAN.md)、設計と実装時の判断は [ARCHITECTURE.md](ARCHITECTURE.md)、T3 のテストとの対応は [PORT_MAP.md](PORT_MAP.md) に記録する。

| 段階 | 状況 | 実装範囲 |
| --- | --- | --- |
| 1 | 完了 | `agent-domain`: ID、コマンド、事実、スレッドの状態機械、fold |
| 2 | 完了 | `agent-providers`: Codex app-server と Claude の翻訳、固定版の replay fixture |
| 3 | 完了 | `agent-runtime`: actor、SQLite の事実ログ、outbox、provider session、同期、履歴の取り込み。`host-daemon` の `conversation` が RPC へ接続する |
| 4 | 完了 | `agent-core` の同期・outbox・表示用データと UniFFI、desktop・iOS・Android。未接続の項目は PORT_MAP の「段階 4 の統合」に記録する |
| 5 | 完了 | 旧 `crates/orchestration` と `crates/provider-adapters`、旧 `orchestration/*` RPC、cwd から作る terminal handle、旧ランタイムの文書の削除 |
| M3 | 実装済み・同一 head の最終検証待ち | Git/VCS、GitHub PR、scheduled tasks、usage、settings/platform、worktree、Preview/device、push/activity、updater/release。Android の Git、Material You、drag arrangement、artifact/citation と desktop の minimap/assistant citations を含む。対応先と境界は `PORT_MAP.md` の段階 6 に記録する |

会話の RPC の ALPN は `remote-agent/streams/14`。旧形式の互換性・移行は設けない。iroh、QR ペアリング、provider プロセス管理、terminal・files・browser・dictation・accounts・既存 worktree 機能は維持する。

Claude 接続は公式の `@anthropic-ai/claude-agent-sdk@0.3.293` を Node 子プロセスの `crates/host-daemon/src/claude/sdk/bridge.mjs` から直接呼ぶ。bridge は SDK の公開 `query()` API と必要な query 操作だけを使い、Rust は共通イベントへの翻訳・会話状態・保存を担当する。依存とライセンスは npm lockfile の integrity・metadata に従って配布物へ含める。Host 上には Node.js 18 以上が必要（開発環境は Nix の Node を使う）。SDK 内部の制御・fork の履歴変換を Rust に移植した実装は持たない。

## 検証方針

変更 crate の単体・property test、Rust lint、Host/GPUI の build、iOS Rust/Swift の build、Android の単体テストと assembleDebug を行う。CI の結果待ち、cargo-mutants、live-provider E2E、Simulator UI テストは行わない。pixel・実機操作の受入確認はこのビルド検証に含めない。稼働中 Host を再起動せず、main と他 worktree は変更しない。

2026-10-08 の統合と SDK 接続で確認した結果は dated checkpoint の履歴として残す。2026-10-09 の現行 checkpoint は次の通りである。

これらは複数の checkpoint で得た結果であり、文書更新後の一つの revision に対する M3 最終統合 head の証明ではない。Root はこの文書変更後の同一 head で、全 unit test、`-D warnings` clippy、fmt、Host/GPUI、UniFFI、iOS（共有・Activity・Share Extension）、Android、artifact stamp を再実行し、その結果を PR に記録する。外部 provider、実機・実端末、稼働中 Host、signing、CI 待ちの検証は行わない。

- `abcad5d107e215bcd08d05bc98feb9521fb83aea`: Rust unit test は 3,976/3,976 pass、3 skip。公式 SDK の Node テスト10件と agent-peer の5群もこの checkpoint で pass した。
- `319491ad312a68bf626f199cc98ae30eb6e652e2`: lint checkpoint は全項目 pass した。
- `170c255effd25da4f5c3a1e37de6a99cb816d543`: scoped lint、safe integration 17 pass/0 leak、Host/GPUI、Swift pure 6、bindings、debug FFI、arm64 Simulator Rust が pass した。Xcode は ViewBuilder の discard assignment で停止した。
- `d0713f9411c9d524dbdf39f92d237ca7f4ef16ec`: `SwiftUIRoot` の appearance revision 読み取りを修正した。この checkpoint の後続 Xcode 11 では unsigned main app、Activity/Usage widget objects、Share extension の executable/plist を確認した。iOS Rust/Swift artifact の core library stamp は `170c255effd25da4f5c3a1e37de6a99cb816d543` のままであり、同一 head の最終結果ではない。同じ `d0713f` checkpoint の Android candidate 9 は BUILD SUCCESSFUL、XML 39/39 pass、0 skip/fail/error、両 JNI release ABI、unsigned APK artifact（署名なし）、および両 ZIP の JNI `.so` に `d0713f` revision を確認した。

SDK の公開 `query()` bridge、provider fixture、通常の unit/property checks は外部 model/provider を呼ばずに検証する。旧 2026-10-08 の Rust 3,370件、SDK Node 8件、途中の native build は当時の結果として保持し、現在の 3,976件および Node 10件の checkpoint と混同しない。

macOS の SystemConfiguration は初回に実行ファイルの directory を CFBundle として走査する。Cargo の unpacked debug objects が多いと、通信テストの処理前に期限を超えた。Nextest の macOS runner は同じ inode の executable と supervisor を小さな directory に置いて実行する。期待値と timeout は変更していない。
