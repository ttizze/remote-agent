# Native mobile fonts

The native clients use DM Sans Regular, Medium and Bold, matching the pinned T3 mobile configuration. The bundled static TTF files come from [Expo Google Fonts](https://github.com/expo/google-fonts/tree/a289ae7354d2b4cdbb6eafbf9ad4be42518785ae/font-packages/dm-sans); their SIL Open Font License is in `native-font-license.txt`.

| File | Upstream file | SHA-256 |
| --- | --- | --- |
| `native-res/font/dmsans_regular.ttf` | `400Regular/DMSans_400Regular.ttf` | `20ccb90498d8ca511bb0be31a74eccd5f29fbe1161852ef72781b703929e98ec` |
| `native-res/font/dmsans_medium.ttf` | `500Medium/DMSans_500Medium.ttf` | `568dafd2db3728534b42c064e63ed1ff45ec97739bc21e407123dfddcb2ad255` |
| `native-res/font/dmsans_bold.ttf` | `700Bold/DMSans_700Bold.ttf` | `3764a2ce62fa95596c3315c1a0ca379e7cf827ed397c97fc036925b9b20b74dc` |

SwiftUI registers these same files through `UIAppFonts`. Compose reads them from the shared `native-res` resource directory. No font fetch is required at build or runtime.
