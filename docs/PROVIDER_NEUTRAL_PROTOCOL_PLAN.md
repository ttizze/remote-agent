# BEX：provider 中立プロトコルの実装方針

[PR #27](https://github.com/ttizze/remote-agent/pull/27) の方針を、native 契約と現行の表示・配送の要件に照らして具体化した。
実装判断には以下を使う。

- [設計と責務分担](BEX_PROTOCOL_DESIGN.md)
- [native 契約の照合結果・検証範囲](BEX_PROTOCOL_NATIVE_CONTRACTS.md)

原案の四種類の要求、八種類のエラー、単一の reasoning delta では現行の契約を保持できない。
上記の文書を正とし、旧案の重複した規則は維持しない。
五段階の実装、Host・desktop・iOS・Android のビルド、通常・結合テスト、モバイル UI の検証を完了した。
実際の認証済み native CLI による推論と稼働中 Host の切替は未実施。検証の詳細と限界は上記の照合結果に記録した。
