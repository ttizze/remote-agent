# BEX landing page

Source of the existing BEX landing page at https://getbex.dev/, imported from
its currently deployed static assets so download changes can be reviewed here.

Windows buttons link directly to the fork's published Windows x86-64 preview ZIP,
including when the release API is unavailable. Extract the ZIP and run
`bex-desktop.exe`; Rust and source compilation are unnecessary. This is an unsigned
development preview, with Git, Node.js and the chosen agents installed separately.

Windows buttons also check the public GitHub latest-release API. When it contains the
uploaded `bex-windows-x86_64-release.zip` asset, the hero buttons, platform menu,
and download card link directly to that asset without visiting a release page.
If the upstream ZIP is not available, the pinned preview download stays usable.

Native clients publishes the ZIP after Windows tests on a release publication.
The checksum is attached alongside it. Publishing a release from a tag predating
this workflow change will not produce the new asset; use a tag containing it.

Android phones show the Android download action prominently. Android buttons link
directly to the fork's verified Android preview APK release asset,
including when the upstream release API is unavailable. The manual
`android-download.yml` workflow builds the APK before
publishing the APK and SHA-256 checksum. Android 8.0 (API 26) or newer and an
ARM64 or x86-64 device are required.
This is a debuggable preview APK, not a store release. The download workflow now
requires a persistent PKCS#12 signing key in the repository's
`BEX_ANDROID_PREVIEW_KEYSTORE_BASE64` secret; no signing keys are checked in.
The preview key uses alias `androiddebugkey` and password `android`; its private
key must remain secret despite that conventional debug password. The workflow
increments the Android version code using its run number, so later previews from
the same repository/key can update in place. Keep a private backup of this key.
Older previews used disposable keys: replacing one removes its local pairing keys
and drafts, so save unsent drafts first and pair again afterward. PC conversation
history stays on the Host. A production/store release needs its own signing policy.

Check download behavior with `node --test lp/download.test.cjs`.
Preview from this directory with `npx wrangler dev` and deploy with
`npx wrangler deploy`. Use the Cloudflare account that owns `bex-lp` and
`getbex.dev`. Publish and verify the APK asset before deploying a changed Android URL.


## Usage demos

The LP includes three silent ten-second recreated BEX usage demos, with fictional
project data and an English interface. The dark desktop shell follows the supplied
BEX screenshot, including its sidebar, composer and workspace card. These are labeled recreations, not recordings of a real session.
Tabs support arrow keys, Home/End and accessible selected states. Inactive and
out-of-view clips pause. Reduced-motion preferences disable automatic playback;
native video controls remain available. Only the selected clip is loaded.

Reproducible HyperFrames source lives in `demo-source/` (Node 22 and FFmpeg).
Run `npm run check` there, then
`npm run render -- --output ../bex-demo.mp4 --fps 30 --quality standard`.
Split the 30-second render at 0/10/20 seconds into `public/demos/agents.mp4`,
`changes.mp4`, `mobile.mp4` using FFmpeg with libx264, yuv420p, CRF 23 and
`-movflags +faststart`. Posters use full-render timestamps 3/17/27 seconds.
The shipped clips are about 120–170 KB each, 1280×720, H.264, without audio.


The published APK was found to require API 37 after an installation failure was reported.
The replacement has minimum API 26 (Android 8.0), while compile/target SDK remain
37. It supports ARM64 and x86-64. Android 8–16 bypass the Android 17-only local
network permission; Android 17+ keeps the permission check. The download workflow
runs unit tests and Android lint, then checks the packaged minimum SDK and
verifies the APK signature before publishing. The reporting phone can now install
and connect; its follow-up runtime failures were traced to an older running PC Host
and an existing conversation owned by the Codex desktop app.

## Android connection recovery

Build and restart the PC Host and desktop client together when updating the APK.
Replacing source or binaries alone does not update the already running Host.
A mismatched Host can load projects successfully while model-list decoding fails.
Keep pairing credentials when restarting; reinstalling Android is unnecessary for
a PC-only Host update.

Codex permits one app-server writer per conversation. BEX never steals an active
writer or silently forks. If the PC Codex app owns a conversation, choose the
message's **分岐** action to continue in a separate conversation; the unsent draft
is copied while the original draft and conversation remain intact. New mobile
builds present a readable recovery message instead of a raw RPC envelope.

A model usable in the Codex desktop app may still be rejected by the separately
connected app-server account. Choose an available catalog model when that happens;
BEX does not silently substitute another model. The live smoke test completed a
reply on an explicit fork using GPT-6-Astra, while this account rejected
gpt-6.1-sol through the BEX connection.

## Mobile composer

The Android UI uses a dark conversation screen and a rounded bottom composer.
Tap the model/effort label to search models and change the provider-advertised
reasoning effort or speed tier. Existing conversations stay on their provider;
new chats can choose either provider. Speed descriptions are shown when available.

The plus button opens Android's multiple-document picker. Picked files upload to
the paired Host through authenticated binary transfers; sending stays disabled
until uploads finish. Attachment cards open image, PDF, text/code and Markdown
previews. Conversation images and existing Markdown file links also open previews.
Binary or oversized text files show an explicit unavailable-preview message.
Draft attachments can be removed without deleting the original phone file.

The landing page uses a short hero followed immediately by the tabbed usage demo.
Repeated mock screens, feature catalogues, platform lists, decorative illustrations
and release announcements are omitted. Three short benefits, pairing steps and
downloads provide the remaining hierarchy; detailed specifications live in Docs.
The demo retains a short recreation label. Supporting text is at least 14px,
with body copy at 16px or larger; the video contains its own recorded UI text.

The three benefits and three pairing steps each have a distinct generated UI
illustration in `public/assets/ui/`. These illustrate agent selection, mobile
approval, local execution, opening the app, QR pairing and resuming a conversation.
They use fictional data and are labeled as UI illustrations, not live screenshots.
Images reserve a 3:2 layout box, load lazily and stack with the cards on phones.

Native responsiveness and disconnect recovery claims are grounded in README.ja.md
(directory structure and Claude execution) and ADR 0005 (shared immutable rendering,
reuse of unchanged presentation objects, reconnect reconciliation). They describe
implementation choices and resumability, not comparative memory/speed benchmarks
or guaranteed uninterrupted networking. No automatic retry of user input is claimed.
