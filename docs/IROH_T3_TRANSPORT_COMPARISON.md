# irohとT3の接続方式の比較

2026-10-05に、同じPCのloopback上でiroh/QUICとTLS WebSocketを2回測定した。この条件ではWebSocketの方が速かった。irohが常に速いという根拠にはならない。指定されたirohとQRペアリングは維持する。

参照T3 Codeは`4ee6bfd50ef4a089440d5c3662db2298da9cc50e`。T3はHTTPとWebSocketで環境へ接続し、直接接続・Tailscale・SSH・T3 Connectが到達経路を変える。[T3のremote設計](https://github.com/pingdotgg/t3code/blob/4ee6bfd50ef4a089440d5c3662db2298da9cc50e/docs/internals/remote.md)、[T3 Connect](https://github.com/pingdotgg/t3code/blob/4ee6bfd50ef4a089440d5c3662db2298da9cc50e/docs/internals/t3-connect.md)を参照した。

今回のWebSocket側は同じ暗号化条件の比較用echo serverであり、T3のEffect RPC、認証、broker、Cloudflare tunnel、UIは実行していない。T3アプリ全体の速度比較とは扱わない。

## 測定条件

- Apple M4 / arm64、macOS 26.5.2 (`25F84`)、Rust release build。稼働中のHostやOSのネットワーク設定は変更しなかった。
- IPv4 `127.0.0.1`、外部relay・名前解決なし。両方ともTLS 1.3とEd25519による相互認証。WebSocketはTCP_NODELAYを有効にした。
- 各実行6ラウンド。先に測る方式をラウンドごとに入れ替えた。各方式・各ラウンドで12要求をwarmupとして除外した。
- 1 KiB制御要求を各実行3,000回、1 MiB本文を各実行96回、接続確立を各実行24回。2回の合計を以下に示す。
- 同じpayloadを送って同じpayloadを返し、応答の一致を検査した。本文は片道1 MiB、往復2 MiB。
- irohは保持した接続内の独立したbidirectional stream、WebSocketは保持した接続内のbinary frame。RPCのcodecやprovider処理は含めていない。
- 接続確立は新しい接続を測るが、Endpoint・TLS設定は保持する。Endpoint生成、QRの読み取り、初回招待の承認、ネットワーク切替後の復旧は含まない。
- 他の常駐プロセスが存在する開発機で測定した。CPU・memory・端末の消費電力は測定していない。

## 結果

単位はms。p50/p95/p99は昇順sampleのnearest rank。

| 操作 | sample数 / 方式 | 方式 | p50 | p95 | p99 |
| --- | ---: | --- | ---: | ---: | ---: |
| 接続確立 | 48 | iroh | 0.2804 | 0.4447 | 1.4714 |
| 接続確立 | 48 | WebSocket | 0.2198 | 0.3372 | 0.5161 |
| 1 KiB要求の往復 | 6,000 | iroh | 0.0400 | 0.0671 | 0.0783 |
| 1 KiB要求の往復 | 6,000 | WebSocket | 0.0221 | 0.0261 | 0.0355 |
| 1 MiB本文の往復 | 192 | iroh | 5.5532 | 5.8570 | 6.1683 |
| 1 MiB本文の往復 | 192 | WebSocket | 0.9650 | 1.0486 | 1.2086 |

この環境の小さい要求では、絶対値の差は中央値で約0.018 msだった。本文の往復には約4.588 msの差があった。これらはlocalhostの値であり、実際のLANや外出先のiPhoneへ一般化しない。

irohを維持する理由は、指定された接続方式・QR登録と、直接接続とrelayを利用できる構成である。今回の結果を速度優位の根拠にはしない。インターネットの遅延・lossや同時転送での差は、別の測定が必要である。

## 再現方法と生データ

NixのOpenSSLを含む環境で実行する。秘密鍵はmode 0700の一時ディレクトリに生成し、読み込み後に削除する。source・生データ・Nix storeには含めない。

```sh
scripts/dev-env.sh cargo run --release --locked -p host-fixture --example transport-compare -- 6
```

ツールは`crates/host-fixture/examples/transport-compare.rs`。版はiroh 1.1.0、rustls 0.23.43、async-tungstenite 0.35.0、tokio-rustls 0.26.4で、Cargo.lockに固定される。

生データは[実行1](benchmarks/iroh-websocket-loopback-1.json)と[実行2](benchmarks/iroh-websocket-loopback-2.json)。方式ごとの全sample、payload、warmup、測定順、機械と依存版を含む。

## 未測定

LANの別端末、インターネットのdirectとrelay、T3 Connectのトンネル、T3の実際のEffect RPC、制御要求と大容量転送の同時実行、loss、network切替、background復帰、CPU・memory・消費電力は未測定である。実際に到達可能な検証経路で条件を揃えてから比較する。
