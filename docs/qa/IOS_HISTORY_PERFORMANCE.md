# iPhone 会話詳細の初回転送

2026-09-20、cinema-maker の最新の完了済み会話を読み取り専用で調査した。
対象は一覧取得ではなく、会話を選択して本文を取得する `OpenSession`。

## 原因と修正

初回ページに最大 500 items を載せる際、4 KiB 以下のツール本文は
折りたたみ中でも全文を転送していた。個々のコマンド出力が小さくても、
数百件集まると初回応答の大部分を占める。また、遅延取得するコマンドにも
表示に使わない出力の先頭を含めていた。

ツール本文の遅延取得基準を 512 bytes に変更し、遅延取得するコマンドは
共通の表示規則で生成した見出しを保持する。出力は展開時に元の item から取得する。
履歴の件数・順序・追加読み込み・ユーザーとアシスタントの本文は削減しない。

同じ会話の `OpenedSession` を wire codec でエンコードしたサイズ:

| 要求 limit | 取得 items | 修正前 bytes | 修正後 bytes |
| --- | --- | --- | --- |
| 5 | 500 | 283,408 | 125,161 |
| 10 | 1,000 | 549,297 | 241,355 |
| 20 | 1,388 | 782,169 | 333,476 |

初回ページは約 56% 減少した。修正前は revision `77c0726` の稼働中 Dev Host、
修正後はこの作業ツリーの HostRpcService と実際の Codex app-server から取得した。
別プロセス・異なるキャッシュ状態なので、この比較から実機の時間短縮率は算出しない。
修正後のローカル初回要求は 95 ms、再取得は 36 ms、Core の表示データ生成は 1 ms。
SwiftUI の描画完了時間や実機回線の計測ではない。

## 回帰確認

`many_small_command_outputs_do_not_delay_opening_history` は、500 items のうち
498 件が約 2 KiB のコマンド出力であるケースを実際の Host/QUIC 経由で開く。
修正前の応答は 1,093,940 bytes で失敗し、修正後は 100 KiB 未満になる。
500 items、質問、最終回答が維持され、先頭・中央・末尾のコマンド全文が
元の item と一致することも検証する。

Simulator では同じ作業ツリーから Host と iOS をビルドして、長い履歴、
再表示、展開時の全文取得、履歴取得中の画面遷移を既存の受け入れテストで確認した。
Rust 222 件、以下の iOS 5 件が成功し、対象 crates の Clippy と rustfmt も通過した。

```sh
nix develop . --command scripts/ios-e2e.sh \
  testSimulatorOpensLongInterruptedHistoryAtLatestMessage \
  testSimulatorReopensCompletedHistoryCollapsed \
  testSimulatorRendersEveryActivityFamilyAndHidesStateOnlyItems \
  testSimulatorOpensTasksBeforeHistoryReadFinishes \
  testSimulatorReopensRunningLongHistoryWithoutBlankViewport
```

実機は調査時に接続できず、実機での体感速度は未計測。

## 追加した診断ログ

Host の `logs/host.jsonl` の `history.open` は `native_ms`、`project_ms`、
`total_ms`、`bytes`、`limit` を記録する。本文や会話タイトルは記録しない。
`native_ms` は provider からの履歴取得、`project_ms` はプロジェクト情報の付与、
`total_ms` は応答作成までであり、端末への転送時間を含まない。

稼働中 Host はビルドだけでは更新されない。反映時には実行中の処理を確認し、
Host とクライアントを同じ revision でビルドして対象プロセスを再確認する。
