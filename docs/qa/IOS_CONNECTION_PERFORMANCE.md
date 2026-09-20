# iPhone 起動時の接続計測

Host と iOS アプリを同じ作業ツリーからビルドして確認する。シミュレータの
loopback 接続の値を、実機の Wi-Fi / モバイル回線の改善幅として扱わない。

iOS の統合ログで subsystem `app.bex.BEX`、category
`connection-performance` を選び、Info メッセージを含めて採取する。
ログは時刻・処理段階・所要時間・真偽値のみで、接続チケット、端末鍵、会話本文を含まない。
[Apple の Logger.info ドキュメント](https://developer.apple.com/documentation/os/logger/info(_:))
に従い、Info ログは常時ディスク保存される前提にせず、再現前から収集する。

| イベント | 意味 |
| --- | --- |
| `app_model_started` | アプリの ViewModel 初期化開始 |
| `store_initialization_started` | 保存状態の読み込み前。Host 切替時は旧 Store の終了も含む |
| `persisted_snapshot_loaded` | 保存ファイル読み込み完了 |
| `store_ready` | Core の保存状態復元完了 |
| `connection_started` | 接続・復帰開始。`foreground` は背景からの復帰かどうか |
| `connection_ready elapsed_ms=…` | Core の接続・復帰処理が返った時点。単調時計による所要時間 |
| `connection_ended elapsed_ms=… cancelled=…` | 接続処理がエラーまたはキャンセルで終了 |
| `list_published connected=… has_list=…` | 一覧の変更を ViewModel が公開 |

`connection_started` から接続完了までが長ければ接続経路を調べる。
接続完了後の一覧公開までが長ければ Host の一覧取得を調べる。
`connected=false` の一覧は保存済み表示として区別する。一覧公開は
`connection_ready` より先に届くこともあるため、ログの記録順だけで処理順を仮定しない。
一覧公開は描画完了やサーバー応答そのものの計測ではなく、状態更新の観測点である。
起動直後の区間で比較し、後続の通知による一覧更新を初回取得と混同しない。

未起動からの起動と背景からの復帰を分け、同じ Host・回線・保存状態で複数回記録する。

## 実機からの計測共有

一覧画面の「その他」→「接続の計測」で直近の接続処理の結果をコピー・共有できる。
記録するのはビルド番号、接続・復帰の区別、時間、再利用の有無、経路種別だけ。
アドレス、端末鍵、会話内容は含まない。

- 通信準備: Core の Endpoint 取得・生成。
- 通信確立: 新規接続の QUIC ハンドシェイク。既存接続の再利用時は 0。
- Host 確認: イベントストリームの開始と storage scope の検証。再利用時は応答確認。
- 経路: `direct` は直接通信、`relay` は中継、`unknown` は取得不能。

経路は接続処理が完了した時点のもので、後から直接通信に切り替わる場合がある。
総時間には Keychain 読み込みなども含まれるため、各区間の合計とは一致しない。
アプリ起動・保存状態の復元、会話一覧の取得、描画の時間はこの総時間に含まれない。

復帰は既存接続の scope 確認と新規接続を同時に進め、先に検証を完了した経路を採用する。
新規接続の失敗だけで応答可能な既存接続を捨てない。未採用・キャンセルされた候補は閉じる。
Host 切替時は旧 Store の終了と新 Store の初期化を並行し、旧接続の終了確認を待たない。
会話本文を端末に永続保存する仕様は変更していない。
