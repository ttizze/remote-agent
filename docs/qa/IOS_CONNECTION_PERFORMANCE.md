# iPhone 接続のパフォーマンス計測

対象は接続・復帰から初回一覧まで。Core、Host、iOS の境界を同じ試行に対応づける。
Host とクライアントを同じリビジョンでビルドする。iPhone 側にコピー・共有・計測開始の操作を求めない。

## 記録と回収

Core が単調時計による数値イベントを最大768件のメモリ内リングへ記録する。
記録は接続開始と接続成功から各30秒。失敗・キャンセル・アプリ状態変更は期間外でも残す。
上限を超えた件数は `dropped` に残り、解析で欠落を0msに置き換えない。
接続成功後31秒でスナップショットを確定し、1要求・10秒の期限で Host へ送る。
新しい接続試行は前の回収を取り消す。切断・終了も保留中の回収を破棄する。
解析結果の `attempts` は同じ回収に含まれる全試行を列挙する。`--attempt ID` で過去の試行を選べる。
同じ Store で再接続できればリング内の過去の失敗も回収できる。プロセス終了を越える永続化はしない。

診断転送を計測期間中に行わず、転送自身の要求は記録しない。
Host の詳細ファイル出力は blocking worker へ渡す。
通常の非公開・サイズ制限付き `logs/host.jsonl` を使う。
アドレス、端末鍵、認証ヘッダー、会話本文、任意の依存ライブラリのメッセージは保存しない。

```sh
python3 scripts/connection_diagnostics.py
# ローカル検証用の Host を解析する場合
python3 scripts/connection_diagnostics.py --log /path/to/logs/host.jsonl --platform Macos
```

## 計測点

| 区間 | 観測する境界・値 |
| --- | --- |
| iOS準備 | 保存状態の読込・復元、Keychain読込、Core呼出しと復帰、build番号 |
| 接続先準備 | Endpoint生成、アドレス探索、Host DNS lookup、ネットワーク調査 |
| Relay接続 | dial開始、TCP試行・成功、TLS開始・完了、Relay認証開始・完了 |
| QUIC接続 | 接続開始・完了・失敗、接続再利用の有無 |
| 各要求 | slot待ち、stream確保、エンコード、送信、readのpoll/pending/wake、最初の読取、応答完了、デコード、先行応答の採用 |
| Host | 同じstreamの受理、読取・デコード、実行待ち、処理・エンコード、書込、応答bytes |
| 経路 | 直接/Relayの選択と変更、各応答時の推定RTT、QUIC損失packets/bytes、送受信packets |
| 停止・UI | 250ms周期の処理系の実測間隔、active/inactive/background、一覧公開とSwiftUI状態更新 |

Relay はレジストリ版 `iroh-relay 1.1.0` の既存の固定メッセージだけを数値の段階へ変換する。
ライブラリのコピー、qlog、パケット全件記録、TCP socketの差替え、kernel samplerは使わない。
固定されたライブラリの更新時は公開Relayテストで境界が残っていることを確認する。
ネットワーク調査開始→完了も記録するため、Relayへdialする前の待ちを区別できる。

`RelayDialStart`→最初の`RelayTcpStart`にはDNS等の接続準備が含まれる。
TLS完了→認証開始にはWebSocket upgrade等が含まれる。どちらも特定の処理だけの時間とはしない。
TCPは成功した試行のspan同士を対応づけ、並行した試行を合算しない。
完了のない試行は失敗・キャンセル・欠落の候補であり、0msではない。
公開Relayのリージョンは既知のn0ドメインを1=aps1、2=usw1、3=use1、4=euw1へ分類するだけで、ホスト名は保存しない。

## 相関と解釈

`client.connection` は接続全体のサマリー。
`client.connection.timeline` の `trace` / `attempt` / `connection` と
`host.connection.link` の `session` を使い、同じQUIC `stream` のHost処理を対応づける。
サマリーはCoreの接続時間で、iOS準備や一覧・描画を含まない。
再利用時にはEndpoint生成や新規QUIC接続のイベントがない。

各 `at_us` はそのTrace内の単調時計。異なる端末の時刻を直接引いて片道時間を算出しない。
`host.rpc.performance` の `accepted_at_us` / `write_started_at_us` / `write_finished_at_us` は
Host endpointのTrace時計で、HostのRuntimePulseと比較できる。
stream確保から応答完了までの時間からHost内の時間を引いた残りには、クライアント処理・通信・未観測のスケジューリングが含まれる。
異なる観測区間が重なり残差が負になる場合は不整合として明示する。
全残差をRelay遅延と断定しない。

`RequestOpened.value` は要求開始からstream確保までのus、`RequestSlotWait.value`はその内のslot待ちus。
`RequestEncoded` / `ResponseDecoded`はエンコード/デコードus。
`RequestSent` / `ResponseFirstRead` / `ResponseReceived`はbytes。
RTT・準備・接続所要時間・RuntimePulseのvalueはus、通信統計は累積値。
readのWake→次のPollは通知を受けた後にアプリが再開するまでの待ちで、kernel受信時刻ではない。
先行一覧の読取タスクは採用前から応答を読み、期限・キャンセル・request slotの所有権を保つ。

RuntimePulseの遅延は処理系が予定時刻に動けなかった証拠であり、原因となったスレッドやCPU処理を特定するものではない。
アプリ状態を併せて見て、background停止とactive中の遅延を区別する。
一覧状態の更新はGPUのフレーム提示完了ではない。
QUICの損失数はRelayが使うTCPの再送回数ではない。
Relay内部の待ち、他社網のhop、TCP内の再送、kernelでのパケット受信時刻は未観測として扱う。

`BEX_CONNECTION_DIAGNOSTICS=off`を起動前に指定すれば、記録・sampler・read waker・回収を止めて同一バイナリで負荷比較できる。
通常のアプリログはこのスイッチの対象外。ローカルの比較を実機の国際回線の改善幅とは扱わない。

## 検証

TransportとCoreの実通信テストで、先行応答、期限、キャンセル、wakerの転送と破棄、
計測なし、バッファ上限、秘密情報の除外、処理系停止の検出を確認する。
診断の受信待ちが復帰完了と終了を妨げないことは、接続完了をモックで制御し、
診断キューを未読にしたCoreの単体テストで確認する。接続速度の合否とは分離する。
解析テストはプロセス・接続・streamの誤対応と、欠落を0msにしないことを確認する。

```sh
nix develop . --command cargo test -p agent-transport -p agent-core --features agent-core/bindings --lib
nix develop . --command cargo test -p agent-transport --lib \
  real_relay_records_upstream_connection_boundaries -- --ignored --nocapture
python3 -m unittest discover -s scripts/tests -p test_connection_diagnostics.py
```

## 詳細計測で分かったこと

2026-09-20 22:51 JST、Dev 9 / revision `8215f04` の iPhone 接続1回分:

| 区間 | 実測 |
| --- | ---: |
| Core 接続全体 | 8,071.9 ms |
| QUIC 確立（Relay 確立を含む） | 2,095.3 ms |
| Relay 確立 | 1,724.2 ms |
| うち TCP 接続 | 1,108.8 ms |
| scope 要求送信から応答の最初の読取まで | 5,935.1 ms |
| 対応する Host 内の読取・実行待ち・処理・書込の合計 | 6.1 ms |
| 並行した一覧の要求送信から応答完了まで | 6,498.6 ms |
| 対応する Host 内の処理 | 189.5 ms |

Relay 選択時の QUIC 推定 RTT は約958〜1,065 ms、後から選ばれた直接経路では約145〜159 ms。
直接経路が選択されたのは接続試行開始から約8.36秒後だった。
一覧応答完了から SwiftUI の状態更新コールバックまでは約21 msで、GPUの描画完了は未計測。

5.9秒の待ちは Host の処理時間だけでは説明できない。残りには通信と両端の
スケジューリングを含み、Relay 内の待ち・TCP再送・端末側の停止などの内訳は未確定。
Relay との地理的な距離だけを原因と断定しない。
診断アップロードはこの待ちの後に始まっているが、記録処理自体の影響を除外した実機比較はない。

Mac の loopback・同一 Release バイナリで計測を切り替えた比較では、接続中央値は
OFF 21.952 ms、段階のみ22.146 ms、パケット詳細24.986 msだった。
これは撤去前のローカル計測であり、タイの iPhone における負荷や改善幅ではない。

Host とクライアントは同じリビジョンで検証する。
接続元、回線、Host、保存状態、再利用の有無が異なる数値を、そのまま変更による改善幅としない。
