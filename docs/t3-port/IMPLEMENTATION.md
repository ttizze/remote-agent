# T3 移植の実装状況

固定仕様と進め方は [PLAN.md](PLAN.md) に従う。

| 段階 | 状況 | 実装範囲 |
| --- | --- | --- |
| M1 | 完了 | 新しい orchestration crate、全 M1 コマンド、SQLite/outbox、Codex/Claude adapter、Host RPC、native 履歴取り込み、共通 client runtime、GPUI・SwiftUI・Compose、旧会話管理と旧テスト・fixture・runner・文書の削除 |
| M2 | 一部実装 | root checkpoint、rollback、停止・再起動時の capture、getTurnDiff、3 クライアントのターン差分選択 |
| M3 | 未着手 | T3 相当の周辺機能の拡張 |

M1 は `af584c44` で完成し、同じ commit の Host・GPUI・iOS・Android
のビルドを確認済み。新しい会話 RPC の ALPN は `remote-agent/streams/7`。
旧形式の互換性・移行は設けない。iroh、QR ペアリング、provider プロセス管理、
terminal・files・browser・dictation・accounts・既存 worktree 機能は維持する。

## M2 の実装

checkpoint は Git の private index と専用 ref に保存する。ユーザーの
index と HEAD は変更しない。scope の cwd が Git root 以下のディレクトリなら、
その cwd 以下だけを stage する。既存 ref は再送で書き換えない。
完了ターンは capture 完了まで waiting にし、保存結果と run/node/item、
次のキュー開始を一つの transaction で確定する。停止後の status は保持する。
Host 再起動後は capture を継続し、queue hold を維持する。

Diff 画面は共通 core が返す ready root checkpoint の選択肢を使い、
workspace・個別ターン・全ターンの差分を表示する。
空白差分無視は RPC/core で対応し、現在の native UI は既定の false を使う。
非 Git workspace は missing checkpoint、Git 保存失敗は error checkpoint
として会話を継続する。cone sparse checkout と unborn HEAD は検証済み。
非 cone sparse checkout の private index 再構築は未対応で error を記録する。

## 残り

- M2: fork、merge back、provider handoff、compaction、app-owned
  subagent/委任、計画の Implement/Refine、画像・ファイル添付、nested checkpoint
  scope、非 cone sparse checkpoint。provider の既存 tool/plan 通知は M1 で表示する。
- M3: T3 の Git/worktree 操作、GitHub PR 連携、scheduled tasks、usage の拡張、
  全設定、Nightly 配布。既存 Host の peripheral 機能と基本設定だけが利用可能。

## 検証方針

変更 crate の単体・property test、Rust lint、Host/GPUI の build、
iOS Rust/Swift の build、Android の formatting と両 ABI の assembleDebug を行う。
CI の結果待ち、cargo-mutants、live-provider E2E、Simulator UI テストは行わない。
pixel・実機操作の受入確認はこのビルド検証に含めない。
稼働中 Host を再起動せず、main と他 worktree は変更しない。

## PR #55 の仕上げ

追加依頼に従い origin/main の `2af069b3` を merge commit `5ad1dd70` で
取り込んだ。CI の queue・変更検出・Mac/iPhone 分離を保持し、削除済みの
会話テスト・runner に依存する箇所を現行構成へ合わせた。M2・M3 は追加していない。

`dev-env.sh` はキャッシュ破損ではなく Bash 3 と Nix の生成構文の不一致が
原因だった。キャッシュ内の固定 Bash へ切り替えてから環境を評価する。
共有キャッシュの削除・書き換えは行っていない。

- `scripts/dev-env.sh just unit-tests`: workspace 230件すべて通過、既存3件 skip。
  standalone agent-peer の5つの CLI テスト群も通過。
- workspace と agent-peer の全 target clippy `-D warnings`、両方の fmt、
  actionlint、`git diff --check` が通過。
- CI が呼ぶ依存境界・build cleanup の2テストが通過。依存境界は共有
  orchestration の Tokio 依存を許可し、その crate のクライアント非依存も検証する。
- Bash 3 からの初回 bootstrap、Nix なしのキャッシュ再利用、引数・TMPDIR・
  終了コードの保持を隔離した環境で確認。

今回の差分は開発環境・CI・テスト・文書に限定する。native app の再ビルド、
CI 待ち、mutants、E2E、Simulator UI テストは行っていない。

## M2 rollback

新しい checkpoint.rollback は provider 会話と、任意でファイルを復元する。
Codex は paginated history の thread/revert、Claude は assistant UUID の
--resume-session-at を使う。復元は admission と実行時に worktree の隔離を
確認し、通常 index と HEAD を保つ。後続 run/node と checkpoint は監査履歴に
残し、rolled_back/stale として表示から除く。失敗を thread に記録し、受付 ID
が変わった古い完了を捨てる。desktop の Edit from here で確認する。
T3 mobile にない rollback UI は追加しない。

変更 crate の171単体/property test が通過（既存2件 skip）、同じ crate の
全 target clippy と fmt が通過。Git 復元で index/HEAD、ignored file の保持と
untracked file の削除・復元を検証した。
