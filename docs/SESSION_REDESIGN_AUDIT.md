# Session再設計：増加原因とHostの簡素化

確認日：2026-09-16。指示書基準は `4118c9048997bac0409609527ea6767641cf60f8`、PRのmain基準は `e052b01`。再修正前は `759258a`。

## 結論

**会話全体を持つ同期runtimeを作り込みすぎていた。** 保存を廃止しても、新しいHostキャッシュに本文・読取範囲・容量・完了判定・hydration revisionを持たせたため、その管理とCore側の復旧処理が増えていた。「必要な新規責務だから増えた」という前回の説明では、実装方法の過剰さを説明できていなかった。

今回、会話キャッシュと15分receipt cacheを削除した。`session/open` と再接続は毎回native historyを読む。Hostは所有中の実行、未解決の要求、実行中の入力IDと接続の購読だけを保持する。

本体は再修正前から **772行削減**。PR全体は **+911行**となった。元の500〜2,500行純減の見込みも、今回提示された±0〜+800行という目安も達成していない。ファイルの移動、テストへの区分変更、整形を削減成果に含めない。

## 行数と比較基準

`crates/` と `apps/` のRust・Swift・Kotlinの物理行数（空行・コメントを含む）。独立テスト、同居テストmodule・単独テスト関数、fixture、文書、生成物、lockfile、vendor、ビルド設定を除外した概算。同じ区分で比較する。

| 区分 | 再修正前 `759258a` 対main | 今回の増減 | 修正後 対main |
|---|---:|---:|---:|
| Host/provider | +1,551 | -705 | +846 |
| core会話処理 | −27 | -64 | -91 |
| Store・その他Operation・配線・画面 | +159 | -3 | +156 |
| 合計 | **+1,683** | **−772** | **+911** |

指示書基準からは **+1,184行**。うち+273行は再設計以外のmain更新。最初のPR実装 `14c1261` からの本体削減は857行。以前混入していた53行の単独テストを除いた集計訂正は、この削減数に含めていない。

Host/providerは `host-daemon` と `codex-app-server`。core会話処理は `state.rs`、`state/notifications.rs`、`state/operations/threads.rs`、`models.rs`、`client.rs`、`session.rs`。`state/operations/submission.rs` はその他Operationの区分で、今回も区分を変更していない。

### 今回の本体差分

| ファイル | 増減 |
|---|---:|
| `crates/agent-core/src/client.rs` | -18 |
| `crates/agent-core/src/models.rs` | -24 |
| `crates/agent-core/src/presentation/conversation.rs` | -1 |
| `crates/agent-core/src/session.rs` | -2 |
| `crates/agent-core/src/state.rs` | -7 |
| `crates/agent-core/src/state/notifications.rs` | -18 |
| `crates/agent-core/src/state/operations/submission.rs` | -2 |
| `crates/agent-core/src/state/operations/threads.rs` | +5 |
| `crates/host-daemon/src/claude.rs` | -26 |
| `crates/host-daemon/src/claude/history.rs` | +0 |
| `crates/host-daemon/src/host_rpc.rs` | -2 |
| `crates/host-daemon/src/host_rpc/codex.rs` | +96 |
| `crates/host-daemon/src/host_rpc/provider_events.rs` | -138 |
| `crates/host-daemon/src/host_rpc/routing.rs` | +40 |
| `crates/host-daemon/src/host_rpc/service.rs` | -107 |
| `crates/host-daemon/src/host_rpc/session_actor.rs` | +66 |
| `crates/host-daemon/src/host_rpc/session_runtime.rs` | -541 |
| `crates/host-daemon/src/host_rpc/submissions.rs` | -93 |

`provider_events.rs` の138行全体を削減とは数えていない。Codex通知の変換はCodex adapterへ直接統合され、その追加を相殺した。録画済みテスト入力の旧形式を読む補助はテスト側にあり、製品コードの代わりに呼ばれる経路はない。

## 3領域で何を削除・統合したか

### Host全体の会話処理

- `SessionRuntime` の541行とそのキャッシュ専用テストを削除。`SessionActor` は実行中のturn・未解決要求・入力IDだけを扱い、native履歴を格納しない。
- 読取範囲の拡大保持、LRU、snapshotの4 MiB上限、1,000 turn上限、hydrated/complete/bytes/revision、更新欠落による再hydrationを削除。
- native読取中に実行が終了した場合、その読取が返るまで当該実行だけを保持し、最新のturnを返して解放する。読取のキャンセルも解放する。購読だけでは実行本文を保持しない。
- `Submissions` と15分・1,024件のreceipt、fingerprint、結果待ちchannel、RPC応答IDの再書換を削除。重複入力は実行中のID集合で拒否する。完了後の重複排除は保証せず、端末は配送不明を自動再送しない。
- Codex/Claudeが共通更新を直接発行する。`provider_events` 中間moduleを削除。Claude承認のRPC文字列生成・再解析も削除し、共通要求の検査と登録を一度に行う。
- Codex event pumpの別個の実行ID一覧を削除。提供元終了時の失敗処理は共通の実行状態を使う。承認の識別子はprovider・native session・turn・native requestで共通化し、端末別の対応表や再送を持たない。
- provider固有のプロセス、stdin回答ハンドル、Codexのネイティブページング、Claude parserは各adapterの責任として残す。これらの単なる移動を削除とは扱わない。

### Coreの履歴・更新処理

- Coreは共通Sessionの取得結果を採用し、`SessionChange`を適用する。旧older/cursor/overlap/refreshの履歴照合、Codex通知解釈は製品のCoreに残していない。
- 今回はHost revisionの保持・比較・欠番再取得を削除。購読UUIDは遅い旧購読からの通知を除くために残す。配信順序と切断検知は既存の通信層で保証する。
- ネイティブ履歴が完了済みに見えても、Host所有の実行中turnを優先する。Claudeの承認待ち再接続で、この区別が必要なことを回帰テストで確認する。
- snapshot容量制限専用だった画像本文の省略・`detailDeferred`・クライアントの再読取分岐も削除。通常のツール出力の詳細取得は残す。
- 本文の確定置換、時刻の保持、tool関係、要求の解決など、表示に必要な共通更新規則は残す。これらは履歴キャッシュの照合ではない。

### Store・Operation・画面への接続

- `SubscribeSubmission` を削除し、送信Operationに統合。未購読ならopenし、その結果をStoreに適用してから送信を続ける。先に送って初期通知を失う順序にはしない。
- 読取パラメータを得るだけのためにSnapshotを複製・変更していた箇所を削除。必要な履歴範囲の計算を共用する。
- `ReadOlder`は前回削除済みで、3クライアントは取得件数を増やすIntentから同じReadThreadへ接続している。新規の履歴Operationは追加しない。
- ナビゲーション後の遅い応答、下書き編集、作成・送信の部分成功を扱う既存のStore epochと結果適用は残す。購読の応答より通知が先に適用されないための順序制御も残す。

## 当初の説明の訂正

指示書は、旧Claudeの1,017行を全量削除に数えないこと、旧provider内の必要機能を残すこと、3画面に同量の重複があると仮定しないことを明記していた。これらを指示書の見落としとして扱った前回の説明は不正確だった。

旧Claudeは既存の表示用会話を丸ごとJSON保存する方式で、保存・復元の中心は約70行、独自一覧・本文・項目参照は約60行。一方、native transcript readerは461行ある。これは増加の一因だが、同期runtimeの過剰な実装まで正当化する理由にはならない。

## 検証

検証コマンド、最終commitの必須quality結果とCI状況はPRに記載する。既存のDesktop/iOS/Android表示の合格条件は変更しない。削除したrevisionとnativeページングの明示的なfull/完了フラグのワイヤーfixture、およびキャッシュそのものを前提とした内部テストだけを新しい契約へ移行する。

追加の確認対象：nativeファイルを外部変更して再open、読取中の実行完了、購読中でも終了本文を解放、4 MiB超・1,000 turn超の要求を切り詰めないこと、実行中入力の重複拒否、provider間の承認ID衝突。実認証を使う推論・実機・稼働中の利用者Hostの置換は行わない。
