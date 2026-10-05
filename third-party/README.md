# Third-party notices

T3 Code's default palette is reproduced from commit
`4ee6bfd50ef4a089440d5c3662db2298da9cc50e`. Its MIT notice is in
`T3-Code-LICENSE`.

The native mobile clients bundle the same DM Sans regular, medium and bold
fonts used by T3 Code. The files are vendored from
[expo/google-fonts](https://github.com/expo/google-fonts/tree/a289ae7354d2b4cdbb6eafbf9ad4be42518785ae/font-packages/dm-sans)
at commit `a289ae7354d2b4cdbb6eafbf9ad4be42518785ae`. Their SIL Open Font
License is in `DMSans-OFL.txt`.

| File | SHA-256 |
| --- | --- |
| `dm_sans_regular.ttf` | `20ccb90498d8ca511bb0be31a74eccd5f29fbe1161852ef72781b703929e98ec` |
| `dm_sans_medium.ttf` | `568dafd2db3728534b42c064e63ed1ff45ec97739bc21e407123dfddcb2ad255` |
| `dm_sans_bold.ttf` | `3764a2ce62fa95596c3315c1a0ca379e7cf827ed397c97fc036925b9b20b74dc` |

The Apple bundles and Android assets include this directory.

The read-only Codex history projection is adapted from
[OpenAI Codex](https://github.com/openai/codex/tree/7f892275e31002f0422477c6219189284560e689)
at commit `7f892275e31002f0422477c6219189284560e689`, specifically the persisted
protocol, thread history builder and paginated history projection. Copyright
OpenAI. Its Apache 2.0 license is in `Codex-LICENSE`. Bex owns the bounded source
reads; it does not use Codex's rollout writer or database repair runtime.
