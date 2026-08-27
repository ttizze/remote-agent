# Phoenix/Elixir production deployment research

調査日: 2026-08-26

前提は、Phoenix Channels の WebSocket を長時間維持し、ブラウザの利用者と外部の Rust agent runner が同じバックエンドで会話すること。金額は各社の公開価格（税・為替・実際の egress は除外）であり、アプリのメモリ使用量や利用者数が未確定なので「料金表上の下限」として扱う。

## 結論

- Cloudflare Workers に通常の Phoenix release を直接載せる設計は対象外。Workers の公式言語は JavaScript/TypeScript、Python、Rust（その他は Wasm 経由）で、BEAM/Elixir の実行環境ではない。これは [Workers の言語一覧](https://developers.cloudflare.com/workers/languages/) からの判断。
- Cloudflare Containers はコンテナ経由で WebSocket を転送できる公式例があるが、デフォルトのアイドル停止、ホストイベントによる不定期停止、エフェメラルディスク、組み込み autoscaling なしという性質がある。[Containers のアーキテクチャ](https://developers.cloudflare.com/containers/platform-details/architecture/)、[FAQ](https://developers.cloudflare.com/containers/faq/)、[WebSocket 例](https://developers.cloudflare.com/containers/examples/websocket/) に基づく。長寿命 Channels の最初の本番 origin としては、接続状態と再接続・外部化を全て検証するまで「使えるか不確実」とみなす。
- 現実的な Cloudflare の使い方は、Phoenix を常時稼働する origin（Render/Gigalixir/Hetzner 等）に置き、Cloudflare を DNS・TLS・WAF・WebSocket proxy にする構成。[Cloudflare の WebSocket proxy](https://developers.cloudflare.com/network/websockets/) は全プランで利用できるが、idle timeout 対策として Phoenix の heartbeat/keepalive を必ず使う。WAF は接続確立時の HTTP 101 までで、確立後のフレームは検査しない。

## 料金と運用の比較

| 選択肢 | 公開価格上の下限 | Channels/agent との相性 | DB・バックアップ | 判定 |
| --- | --- | --- | --- | --- |
| **AWS Lightsail 1台 + SQLite** | 東京を含むリージョンで、public IPv4付き Linux は **$7/月で1 GB RAM / 40 GB SSD**。IPv6のみなら **$5/月**。[公式料金](https://aws.amazon.com/lightsail/pricing/) | 小規模な Phoenix origin を常時稼働できる。Rust runner は別ホスト/ユーザー端末とし、この VM に同居させない。 | Phoenix 1.8 の `phx.new` は SQLite3 を公式に選択可能。[`mix phx.new`](https://hexdocs.pm/phoenix/Mix.Tasks.Phx.New.html)。WAL と外部 Object Storage への継続バックアップを必須とする。単一 writer・単一 VM が前提。 | **小規模本番の最安候補**。ただし単一障害点で、DB の水平拡張はできない。 |
| **Hetzner Cloud 1台 + Postgres** | CPX12 が欧州の一部ロケーションで **€11.49/月**、Singapore で **€15.49/月**。IPv4 は **€0.50/月**、IPv6 は無料。[公式価格データ](https://www.hetzner.com/_resources/app/data/bench/cloud_data.json)、[server overview](https://docs.hetzner.com/cloud/servers/overview/) | Phoenix、Postgres、Caddyを同じ VM の systemd/Docker サービスにできる。長寿命接続に制限を設けない。 | Postgres も自己管理。サーバー Backup は日次・7スロットだが、Volume は含まず、実行中のディスク整合性は保証されない。[Backup/Snapshot docs](https://docs.hetzner.com/cloud/servers/backups-snapshots/overview/)。暗号化した `pg_dump` を別の Object Storage/S3 に定期送信し、復元テストが必須。[Object Storage docs](https://docs.hetzner.com/storage/object-storage/overview/) | **Postgres同居の安価案**。ただし日本から欧州は遅延が大きく、Singaporeは価格が上がる。 |
| **Render paid** | Web Service Starter **$7/月** + PostgreSQL Basic-1GB **$19/月** = **$26/月から**。[公式料金](https://render.com/pricing) | 有料 Web Service は受信 WebSocket の時間制限を設けていない。ただし再起動・保守・ネットワーク障害では切断されるため keepalive と再接続が必要。[WebSocket docs](https://render.com/docs/websocket) | 有料 Postgres は PITR 等のバックアップ機能を持つ。[PostgreSQL](https://render.com/docs/postgresql)、[backups](https://render.com/docs/postgresql-backups)。同一リージョンの private network を使い、Rust runner は別サービス/外部実行基盤から認証付きで接続する。egress は WebSocket 応答も課金対象。[outbound bandwidth](https://render.com/docs/outbound-bandwidth) | **運用を含めた最安候補**。Starter 512 MB は初期容量なので、Channels/BEAM の実測で増量する。HA は別料金。 |
| **Railway Hobby** | Hobby のサブスクリプション **$5/月**（同額の usage credit）。RAM $10/GB-month、CPU $20/vCPU-month、egress $0.05/GB。[料金](https://docs.railway.com/pricing/plans) | 長時間コンテナは通常動作。private network でサービス間接続可。[private networking](https://docs.railway.com/networking/private-networking) | PostgreSQL template はイメージ + Volume で、Railway 自身が「DB はデフォルト HA ではなく SLA なし」と説明している。[PostgreSQL](https://docs.railway.com/databases/postgresql)、[platform philosophy](https://docs.railway.com/platform/philosophy)。Volume backup はあるが、Volume 削除時に backup も失われる。[volume backups](https://docs.railway.com/volumes/backups) | **安いが DB を任せる本番の第一候補ではない**。外部 managed Postgres を別に用意するなら再計算が必要。 |
| **Gigalixir Standard** | 料金ページ上の Standard app は **$10/月から**（0.2 GB の最小サイズ、実用サイズは増額）+ Standard DB 0.6 GB **$25/月** = **$35/月から**。[tiers/pricing](https://docs.gigalixir.com/tiers-pricing)、[Standard DB](https://docs.gigalixir.com/database/standard-tier) | Elixir/Phoenix 向け。`dns_cluster` と分散 Phoenix Channels/PubSub の公式手順がある。[cluster docs](https://docs.gigalixir.com/cluster) | Standard DB は日次 backup を 7 日保持。無料 DB は本番不可で、自動 backup もない。[backup/recovery](https://docs.gigalixir.com/database/backup-and-recovery)、[free vs standard](https://docs.gigalixir.com/faq/free-vs-standard-tier) | **Phoenix 固有の運用を買う選択肢**。Render より高いが、クラスタリングを最初から重視するなら合理的。 |
| **Fly.io + Managed Postgres** | Managed Postgres Basic **$38/月** + provisioned storage **$0.28/GB-month**。Phoenix の Machine 料金が別途必要。[MPG pricing](https://fly.io/docs/mpg/)、[Machine pricing](https://fly.io/docs/about/pricing/) | Elixir のクラスタリング、private network、複数 Machine の公式手順がある。[Fly Elixir](https://fly.io/docs/elixir/)、[clustering](https://fly.io/docs/elixir/the-basics/clustering/) | Managed Postgres は HA、backup、connection pooling を含む。旧 Fly Postgres は自分で管理するサービスなので、安さだけで本番の安全性を比較しない。 | **技術的には非常に良いが、managed DB 前提では最安ではない**。Channels 用 Machine は autostop を切る。[autostop/autostart](https://fly.io/docs/launch/autostop-autostart/) |

## scale-to-zero の扱い

常時接続の Channels では、scale-to-zero は「アイドル HTTP アプリ」向けであり、基本的に採用しない。

- [Cloudflare Containers](https://developers.cloudflare.com/containers/platform-details/architecture/) は `sleepAfter` の既定値が 10 分で、コンテナのディスクはエフェメラル。ホストの再起動等で停止し得る。接続中の Phoenix プロセス状態を前提にできない。
- [Railway Serverless](https://docs.railway.com/deployments/serverless) は外向きパケットが 10 分以上ないと sleep し、wake 時に遅延または最初のリクエストの 502 が起こり得る。受信だけでは sleep 防止にならないため、WebSocket が存在することを稼働保証に使わない。
- [Render Free](https://render.com/docs/free) は 15 分間 inbound がないと停止し、wake に約 1 分。Free Postgres は 30 日で期限切れになり backup もないため、本番不可。
- [Fly autostop/autostart](https://fly.io/docs/launch/autostop-autostart/) は CPU/RAM 費用を下げられるが、長寿命 Channels の origin は `auto_stop_machines = "off"` とし、明示的に常時稼働させる。

## 推奨構成

### 最安（小規模・単一ノード）

```text
DNS
 -> AWS Lightsail Tokyo 1 GB ($7/month, public IPv4)
    Caddy -> Phoenix Channels
    SQLite (WAL)
 -> continuous backup -> external Object Storage/S3

Rust agent runner -> authenticated WebSocket -> Phoenix
```

これは **$7/月 + 少量の外部バックアップ料** が下限。Cloudflare proxy 等で IPv6-only origin を安定運用できるなら $5/月に下げられるが、構成の分かりやすさから public IPv4 付きを推奨する。DB の HA はなく、復元手順と復元テストが必須。agent runner は同居させず、認証付き WebSocket で Phoenix に接続する。

SQLite の単一 writer 制約や単一 VM が問題になった時点で Postgres へ移行する。最初から Postgres を使いたい場合は、2 GB 程度の VPS 1台に Phoenix + Postgres を同居させるのが次に安い。

### 運用込みの最安（今回の第一推奨）

**Render Web Service Starter + Render PostgreSQL Basic-1GB（公開価格上は $26/月から）** を Phoenix origin にする。Cloudflare は前段 proxy、Rust runner は外部の常時稼働サービスから Phoenix の認証付き WebSocket/HTTP に接続する。Render の DB backup/PITR と、WebSocket の切断・再接続仕様が、Railway の最小構成より本番の復旧責任を小さくする。

初期デプロイ後に、BEAM の resident memory、同時 Channel 数、Postgres connections、egress を測る。512 MB/1 GB の最小構成に収まらなければ Web/DB を増量する。複数 Phoenix replica に進む時点で、セッションをローカル process state に置かず、Postgres/Redis 等の共有状態と cluster/PubSub 方針を明示する。

Phoenix への密着度を優先する場合の次点は **Gigalixir Standard**。Native clustering と DB backup を得られるが、料金下限は Render より高い。Fly.io はクラスタリングの技術的適合度は高いものの、Managed Postgres を選ぶと Basic だけで $38/月なので、今回の「最安」軸では後段候補とする。

## 安全性・不確実性

- 価格は 2026-08-26 に各社の公式ページで確認した表示値。プラン・リージョン・税・為替・egress は変動するため、契約前に再確認する。
- Cloudflare Containers の WebSocket forwarding 自体は公式例で確認できるが、Phoenix をそこで長期間運用した時の接続維持・停止時の挙動・分散状態の保証までは公式資料から確認できない。従って「動く」と「本番で安全」は分けて判断する。
- どの PaaS でも deploy/restart 時の WebSocket 切断は起こり得る。クライアントの再接続、購読再同期、idempotent な agent task protocol をアプリ側で持つことが必須。
