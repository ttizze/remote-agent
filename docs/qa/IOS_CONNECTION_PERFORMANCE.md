# iPhone 接続の自動計測

Host とクライアントを同じリビジョンからビルドして配布する。
Dev 7 は接続後に詳細なタイムラインを Host の非公開 `logs/host.jsonl` へ送る。
iPhone で診断のコピー、共有、計測の開始操作は不要。
シミュレータや Mac の数値を実機の国際回線の性能として扱わない。

## 計測範囲

| 段階 | 記録する境界・値 |
| --- | --- |
| iOS 準備 | 保存状態の読み込み、Core 復元、Keychain 読み込み、接続呼出し・復帰 |
| Endpoint | 生成開始・完了。再利用なら生成イベントなし |
| Host の探索 | 接続先準備開始・完了、DNS lookup の開始・成功・終了 |
| Relay の確立 | DNS 開始、IPv4/IPv6 候補、TCP 試行・成功・失敗、TLS、WebSocket、Relay 認証 |
| QUIC | 接続開始・完了・失敗、接続直後の RTT と送受信・損失統計 |
| 各要求 | スロット待ち、ストリーム確保、送信、最初の読み取り、先頭応答フレーム受信完了、デコード |
| Host | 同じストリームの受理時点、要求読取・デコード、実行待ち、処理・エンコード、書込、応答バイト数 |
| 一覧 | 先行取得した応答の採用、ViewModel への公開、SwiftUI の一覧状態更新・出現 |
| 経路 | 各応答後と接続後の直接／中継経路、推定 RTT、損失パケット・バイト数、送受信パケット・暗号ハンドシェイクフレーム数 |

`vendor/iroh-relay` は固定バージョン 1.1.0 に計測点だけを追加したもの。
接続先アドレス、秘密鍵、認証ヘッダー、任意のライブラリメッセージ、会話本文は送信しない。
Relay のリージョンは公開 n0 ドメインの分類だけを数値で記録する。
`RelayRegion` は 1=aps1、2=usw1、3=use1、4=euw1、0=その他／不明。
地域名から物理的な経路や所要時間を推定しない。

## 相関と時計

- `client.connection`: 従来の接続サマリー。`total_ms` は Core の接続処理。
  UI 準備、一覧取得、描画はこの総時間に含めない。
- `client.connection.timeline`: ランダムな `trace`、`attempt`、`connection`、
  `report`、欠落イベント数 `dropped`、クライアントのリビジョン。
- `client.connection.event`: `trace` 内の `seq`、単調時計の `at_us`、固定の `phase`、
  `group`、`stream`、数値 `value`。
- `host.connection.link`: クライアントの `trace` と `connection` を Host の `session` に対応づける。
- `host.rpc.performance`: `session` と QUIC の `stream` でクライアント側の要求に対応づける。

`at_us` は Trace 作成からのマイクロ秒。同じ `trace` 内でのみ差分を計算する。
`group` は接続 ID、接続試行 ID、DNS lookup ID、または Relay の dial ID。
`phase` に応じて区別する。Relay の `stream` は TCP 試行を区別するローカル span ID で、
RPC の QUIC stream ID とは別物。両者は合成しない。
Host の `accepted_us` は当該 Host セッション開始からの相対時間。
Host と iPhone の絶対時刻を引いて片道時間を算出してはいけない。

主な `value` の単位は次のとおり。

- `AppPreparation`、`SnapshotRead`、`StoreRestored`、`IdentityRead`、`UiConnectReady`、
  `ResumeReady`、`ResumeFailed`、`RequestOpened`、`ResponseDecoded`、`RttMicros`: マイクロ秒。
  `RequestOpened` は要求開始からスロット取得・ストリーム確保までの時間。
- `RequestSent`、`ResponseFirstRead`、`ResponseReceived`: バイト数。
- `ClientBuild`: iOS build 番号。
- `UiConnectStart`: 1=foreground 復帰、0=通常の接続。
- `ListPublished`、`ListViewUpdated`: 1=接続状態、0=オフライン状態。
- DNS 候補・TCP 開始: IP family の 4 または 6。
- `PathOpened`、`PathClosed`、`PathSelected`: 1=直接、2=Relay、0=その他。
  経路イベントを継続購読し、`PathEventsDropped` で購読側の欠落数を記録する。
- 統計イベント: 接続開始からの累積値。差分を取る際は同じ接続 ID に限定する。

## 自動回収と負荷

接続完了直後、1・3・10・25 秒後に未送信の差分を送る。
応答を待って接続や画面表示を止めない。送信は同時 1 件、各 2 秒で打ち切る。
Host の ACK がなければ次の送信に同じイベントを含めるため、解析時は `trace` と `seq` で重複除去する。
新しい復帰は以前の回収処理を取り消す。切断・終了で送信 Future を破棄する。

記録期間は接続試行と成功後の各 30 秒。終端の失敗・キャンセルは期間外でも残す。
最大 768 イベントのリングバッファで、溢れた数は明示する。
失敗した試行の記録は同じ Store の次の成功時にも送る。
未送信のメモリ上の記録はプロセス終了を越えて保持しない。

パケットごとの無制限ログや TLS キーログは有効にしない。
DNS・Relay の所定の境界と必要な親 span だけを収集する。
診断要求自体を RPC タイムラインに再帰的に記録しない。
通信統計には診断送信も含むため、診断トラフィックのない測定として扱わない。

## 測定値の意味

`ResponseFirstRead` はアプリがストリームから最初のバイトを読み取った時点。
ネットワークインターフェースでの受信時刻ではない。
先行した一覧応答も専用の読み取りタスクが即座に読み、scope 確認や UI の採用を待たない。
`ReplyAdopted` と比較すれば、届いた応答をアプリが採用するまでの待ちを分離できる。
読み取りは元の要求期限、接続の request slot、所有者のキャンセルに従う。

Host の `handle_encode_us` は処理と先頭応答のエンコードを含む。
`decode_us` はストリーム受理後の要求読取とデコードで、追加バイトを待つ時間も含む。
`write_us` は QUIC 送信バッファへの書き込み時間で、相手の受信完了ではない。
各要求の往復時間から Host 処理を引いた残りは、通信と両端のスケジューリングを含む。
純粋なネットワーク RTT は別の `RttMicros`（QUIC の推定値）を見る。

`ListViewUpdated` は SwiftUI の状態変化コールバックまたは画面出現であり、
GPU が描画したフレームの提示完了を測るものではない。
欠けた境界は未観測／未実行として扱い、0 ms と補完しない。
DNS は IPv4/IPv6 が競争する。成功した TCP 試行を選び、重なった待ちを合算しない。
`RelayDialEnded` は失敗やキャンセルでも起きるため、`RelayReady` と区別する。
損失統計は QUIC が損失と判断した数で、再送回数そのものではない。

## 接続改善と比較

TLS 接続・ペアリング後、scope 確認と初回一覧要求を同時に送る。
scope 検証前に Store へ接続を採用したり、一覧を公開したりしない。
検索条件が変わった場合は先行取得を破棄する。
これは一覧取得までの直列待ちを減らす変更で、QUIC 確立時間の短縮ではない。

復帰では既存接続の確認と新規接続を競争させ、先に検証できた方を使う。
新規接続の失敗で応答可能な既存接続を捨てない。

2026-09-20 の iPhone 自動ログでは、Relay の新規接続に 1,940〜2,991 ms、
直接の新規接続に 355 ms、直接接続の再利用に 151〜190 ms という記録があった。
回線・時刻・再利用条件が揃っていないため、その差を変更による改善幅としない。
以前の東京 Fly 中継からの変更、接続元の国、ネットワークの影響を分けて検証する。

## 検証

Core のテストは、先行応答が採用前に観測されること、元の期限・キャンセル・
request slot の解放、検証前の一覧非公開、変更された検索条件の破棄を確認する。
非公開情報の除外、バッファ上限、タイムラインの順序も検証する。
公開 Relay の外部接続テストは明示的に実行する。

```sh
nix develop . --command cargo test -p agent-core --features bindings --lib \
  real_relay_records_dns_tcp_tls_websocket_and_authentication -- --ignored --nocapture
```

リリース前に、そのテスト、iOS Simulator の統合テスト、同じリビジョンの
稼働中 Dev Host に対する自動回収・要求相関を確認する。
