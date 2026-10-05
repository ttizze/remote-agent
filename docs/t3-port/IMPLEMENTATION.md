# T3 移植の実装状況

固定仕様と進め方は [PLAN.md](PLAN.md) に従う。

| 段階 | 状況 | 実装範囲 |
| --- | --- | --- |
| M1 | 完了 | 新しい orchestration crate、全 M1 コマンド、SQLite/outbox、Codex/Claude adapter、Host RPC、native 履歴取り込み、共通 client runtime、GPUI・SwiftUI・Compose、旧会話管理と旧テスト・fixture・runner・文書の削除 |
| M2 | 実装完了 | root/nested checkpoint、rollback、fork/merge back、provider handoff、compaction、native subagent と app-owned 委任、計画フォローアップ、画像・ファイル添付、getTurnDiff、3 クライアントの表示・操作 |
| M3 | 未着手 | T3 相当の周辺機能の拡張 |

M1 は `af584c44` で完成し、同じ commit の Host・GPUI・iOS・Android
のビルドを確認済み。新しい会話 RPC の ALPN は `remote-agent/streams/9`。
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

## M2 fork / context transfer

fork は監査履歴を source に残し、source point までを inherited timeline として表示する。
merge back は fork の親への転送だけを許す。native fork、portable full/delta context、
再開失敗時の fresh session、実際の開始時の転送消費を run/attempt guard で保存する。
3クライアントの fork/merge back は共通 core の判断を使う。

変更 crate の176単体/property test が通過（既存2件 skip）。開始判定、fork の
冪等な再送、未完成 run の拒否、merge lineage、履歴の byte 枠、inherited/local
履歴の position による pagination と単項目取得を検証した。clippy/fmt も通過。

## M2 maintenance / plan follow-up

compact は native maintenance turn として queue でき、steer は拒否する。
workspace checkpoint は作らず、空の会話は失敗として表示する。desktop は
Implement/Refine と新しい thread での実装を提供し、source plan を原子的に完了する。
更新された案は以前の未完了案を superseded にする。mobile は T3 の /plan と
/default を使い、desktop 専用の実装ボタンは追加しない。

変更 crate の180単体/property test、全 target clippy と fmt が通過（既存2件 skip）。

## M1 レビュー修正

b52a2be1 に対する87件を aacd3737 以降の現行コードと照合し、
関連するまとまりごとに修正と回帰テストを commit した。
[PR55_REVIEW.md](PR55_REVIEW.md) に各指摘の対応・不採用の理由・検証を記録する。
SQLite/Tokio runtime は Host feature に限定し、前回の共有 runtime 許可を置き換えた。
残存 integration targets、store_session、Swiftlint/detekt/native unit tests を戻した。

実装・テスト08b882ecで全 unit-tests 306件と agent-peer の5群、
workspace/agent-peer clippy・fmt、actionlint が通過（外部・手動5件 skip）。
Swift unit tests 2件、Android JVM unit tests 3件と各 native lint、
iOS/Android build も通過。初回全 target 実行に手動 WebKit probe が一度混入した
点と、その後の ignored 修正は対応表に記録した。Simulator UI/実 provider E2E、
CI 待ちは実施していない。この時点では M2/M3 を中断していたが、以下の追加依頼で M2 のみ再開した。

## 2 回目のレビューと M2 完了

2 回目のレビューでは effect lease/再取得、provider 停止と rewind、scope/queue/下書き/同期、
import の checkout 所有権、native 表示・保存状態を修正した。各指摘の対応と理由は
[PR55_REVIEW.md](PR55_REVIEW.md) に記録した。

追加の M2 は app-owned の自己完結した委任、停止・結果通知・acknowledgement・復旧、
Host の scoped MCP bridge、Codex/Claude の native subagent と runless 子履歴、Agents roster、
3 クライアントの写真/ファイル添付、検証付き iroh 転送と provider 画像入力を実装した。
汎用 nested scope の baseline/capture/restore/diff、独立 ordinal、専用 outbox、所有権と
rollback 境界も実装した。固定 T3 と同じく自動 scope は root のみで、モバイルに
Implement/Refine や nested scope の新しい画面を足していない。

M3 は未着手。main の取り込み、稼働 Host の更新、他 worktree の変更は行っていない。
M2 完了後は同じ PR #55 に push し、レビュー後の再開指示を待つ。

### 最終検証

- `CARGO_INCREMENTAL=0 NEXTEST_TEST_THREADS=4 scripts/dev-env.sh just unit-tests`: **391件通過、既存5件 skip**。standalone agent-peer の5テスト群も通過。transport/pairing/management などの現行 integration targets と、新しい回帰/property/Git/provider fixture tests を含む。
- workspace の全 target/bindings と agent-peer の Clippy `-D warnings`、両 fmt、actionlint、`git diff --check` 通過。
- Swift: swiftformat/Swiftlint strict、native unit **2件通過**、Rust/Swift bindings と generic iOS Simulator destination のアプリ **build 成功**。Simulator の起動・UI操作なし。
- Android: ktfmt/detekt、JVM unit **3件通過**、arm64-v8a/x86_64 の **assembleDebug 成功**。
- macOS: Host/provider supervisor/GPUI の **release build・署名・署名検証成功**。稼働 Host を起動・再起動していない。
- 最終の実装・全体テストと Host/3クライアントのビルド対象は `665deaa1`。以後の変更は検証記録のみ。CI待ち、cargo-mutants、E2E、実 provider/実機の受入確認は実施していない。
