# BEX landing page

Source of the existing BEX landing page at https://getbex.dev/, imported from
its currently deployed static assets so download changes can be reviewed here.

Windows buttons download the published `bex-windows-x86_64-setup.exe` installer directly, including when the GitHub release API is unavailable. Run the EXE to install BEX and all companion Host/CLI/Claude SDK files into the current user's Programs folder and create a Start menu shortcut. No ZIP extraction or Rust compilation is needed. Git, Node.js, the chosen agents and WebView2 are separate runtime prerequisites.

Windows downloads stay pinned to an explicitly verified installer. A moving upstream release or a matching asset filename cannot establish that an installer uses a portable release build, so the site does not automatically replace the verified Windows URL. Native clients builds a release installer with Inno Setup after Windows tests, installs it and launches it without the build-time GPUI shader sources, retains it as an artifact, and uploads the EXE/checksum on release publication. The packager rejects development binaries and mismatched revisions. The ZIP remains an optional diagnostic artifact, not a site download.

Android phones show the Android download action prominently. Android buttons link
directly to the fork's verified Android preview APK release asset,
including when the upstream release API is unavailable. The manual
`android-download.yml` workflow builds the APK before
publishing the APK and SHA-256 checksum. Android 8.0 (API 26) or newer and an
ARM64 or x86-64 device are required.
This is a debuggable preview APK, not a store release. Both APK publication
workflows require the existing PKCS#12 signing key in the repository's
`BEX_ANDROID_PREVIEW_KEYSTORE_BASE64` secret; no signing keys are checked in.
The preview key uses alias `androiddebugkey` and password `android`; its private
key must remain secret despite that conventional debug password. Both workflows
use `scripts/configure-android-preview.sh`; missing keys stop publication.
Version codes come from the full source history and increase along descendant
main builds across both workflows. Keep the same key when moving previews between
repositories so installed previews can update in place. Keep a private backup of
this key. Ordinary CI APKs remain disposable test artifacts.
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

Android messages labeled 「送信結果不明（自動再送しません）」 offer
「表示を削除」 to remove the retained local message, or 「下書きに戻す」 to
recover its text and attachments for editing. Both use the shared core recovery
actions and work offline. Removing the message is persisted, so reopening the
new chat or restarting the app does not bring it back. Neither action resends
the instruction or cancels a task that the Host may already have accepted.

Retained queued messages are collapsed into 「送信結果不明・N件」 so they do
not fill the conversation. 「内容を確認」 reveals the original messages and
their individual recovery actions. 「表示をすべて削除」 removes the displayed
batch through the same persisted core discard action, preserving the current
draft and any inputs arriving after the batch starts. A storage failure stops
the batch and allows retrying the remaining messages.

While a conversation has an active turn, the right edge of the Android composer
shows a white circular stop button with a black square. It remains available
with an empty draft or while editing a follow-up, and interrupts that turn through
the shared core without clearing the draft. Pending requests disable repeat taps;
failed requests show a retry notice. When the turn ends, the send control returns.

The Android chat list keeps the BEX wordmark, Host selector, connection dot,
search and refresh actions in one header row, including at 320dp width.
There is no workspace heading or separate Host row. Search opens in place;
closing it clears the filter and restores the header controls. Host names
truncate to one line, and the connection dot retains a spoken status label.
The selector uses a small connection dot, PC icon and machine name without a
filled button background or chevron. Hosts default to the machine hostname
(Windows computer name); an explicit `--name` overrides it. Paired Android
clients refresh the saved name when reconnecting, without pairing again.

Recent chats includes standalone conversations and individual conversations inside projects, ordered by the latest update. Each project conversation shows its project name below the title, including while the project is collapsed. More loads older conversations across both groups; tapping a row opens its original provider and session.

The microphone button opens the phone's speech recognition activity in the device
language and appends the result to the original draft through the shared core.
Review or edit the text before sending. Cancellation preserves the draft; a phone
without a speech recognition app shows setup guidance. The speech activity owns
microphone access, so BEX does not require a separate recording permission.

The Android UI uses a dark conversation screen and a rounded bottom composer.
Tap the model/effort label to search models and change the provider-advertised
reasoning effort or speed tier. Existing conversations stay on their provider;
new chats can choose either provider. Speed descriptions are shown when available.
Use 「この設定を新規チャットの既定にする」 in that dialog to save the selected model,
reasoning effort and speed for future new chats on the Android device. These
presets survive app restarts; existing drafts keep their own settings.

The plus button opens Android's multiple-document picker. Picked files upload to
the paired Host through authenticated binary transfers; sending stays disabled
until uploads finish. Attachment cards open image, PDF, text/code and Markdown
previews. Conversation images and existing Markdown file links also open previews.
Binary or oversized text files show an explicit unavailable-preview message.
Draft attachments can be removed without deleting the original phone file.

The project chat icon and 「メイン会話を開く」 open the existing Claude project
coordinator in BEX's regular native conversation screen. History, Markdown,
selection/copy, older-message pagination and text input share the existing BEX
conversation implementation. The paired Host reads and sends channel messages
with the Claude account selected in BEX; no embedded page or phone web login is
needed. Local worker threads remain separate conversations in the project.

Conversation website and PR links open Android's browser or matching app. Links
inside tables behave the same way. If no app can handle a link, BEX displays its
URL with a copy action. Workspace file links keep the authenticated BEX preview.


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

The hero and three technical benefits highlight Rust + GPUI rendering, QUIC / iroh
streams and Host-side execution with reconnect synchronization. Sources are
README.ja.md (native clients and Host execution), ADR 0005 (reused presentation
objects, independent request/subscription/blob streams, reconnect reconciliation),
and `crates/agent-transport` (iroh endpoint and stream implementation).
QUIC stream independence is described in RFC 9000 sections 2 and 2.2:
https://www.rfc-editor.org/rfc/rfc9000.html#section-2.2
These are implementation and resilience claims, not measured memory/speed ratios
or reliability comparisons with Codex or Claude mobile apps. No reproducible
comparison supports a tenfold claim yet. Do not infer a ratio from using Rust,
promise uninterrupted networking or claim automatic replay of user input.
