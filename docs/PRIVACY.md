**BEX プライバシーポリシー / Privacy Policy**

更新日 / Updated: 2026-09-21

**送信先と利用目的 / Recipients and purpose**

BEXは、あなたがペアリングしたHostに接続する開発クライアントです。メッセージ、選択した写真・動画・ファイル、音声入力、開いたプロジェクトのコードやツールの実行結果は、操作に応じてHostへ送信されます。Hostは、選択したCodexではOpenAI、ClaudeではAnthropicへ、回答・コード編集に必要な内容を送信します。音声入力の録音は文字起こしのためOpenAIへ送信します。信頼するHostだけに接続し、送信する権限のある情報だけを扱ってください。

BEX connects to a Host you explicitly pair. Your messages, selected photos, videos and files, dictation recordings, project code and tool results are sent to that Host as needed for your actions. The Host sends relevant content to OpenAI when you use Codex, or Anthropic when you use Claude, to generate responses and edit code. Dictation recordings are sent to OpenAI for transcription. Connect only to Hosts you trust and use information you are authorized to share.

**接続・端末の権限 / Connections and device permissions**

Hostとの通信は認証・暗号化されます。直接接続できない場合はirohの中継サービスを使います。中継サービスには接続元IPアドレスなどの通信情報が伝わりますが、暗号化された会話の内容を復号する鍵は渡しません。カメラはQR読み取りや選択した撮影、マイクは音声入力や動画撮影に使います。写真への保存は操作時に許可を求めます。これらの許可はiOSの設定で変更できます。

Connections to the Host are authenticated and encrypted. When a direct connection is unavailable, iroh relay services carry encrypted traffic and receive network information such as IP addresses, but not the keys needed to decrypt conversation content. Camera, microphone and photo-saving permissions are used for the corresponding features you choose and can be changed in iOS Settings.

**保存・共有 / Storage and access**

端末には接続先、認証鍵、下書き、表示用データ、一時添付ファイル、障害調査用ログを保存します。HostとAIサービスには会話や作業データが保存されます。共有Hostでは、その管理者や同じ環境へのアクセス権を持つ利用者がデータを読めます。AIサービス側の保存期間・学習利用・削除方法は、利用する契約と設定によって異なります。

The device stores paired Hosts, authentication keys, drafts, display data, temporary attachments and diagnostic logs. The Host and AI provider store conversation and work data. Administrators and other users with access to a shared Host may read that data. Provider retention, training use and deletion depend on the account's plan and settings. See the [OpenAI privacy policy](https://openai.com/policies/privacy-policy/) and [Anthropic privacy policy](https://www.anthropic.com/legal/privacy).

BEXには広告・広告トラッキング機能はありません。接続の診断情報はHostへ送信されることがあります。TestFlightで共有するクラッシュ情報やフィードバックはAppleの仕組みで開発者に届きます。サポートへ送った情報は問題の調査と返信に使用します。

BEX has no advertising or advertising tracking. Connection diagnostics may be sent to the Host. TestFlight crash reports and feedback are shared with the developer through Apple's testing service. Information you send to support is used to investigate and respond to your request.

**送信停止・削除 / Stop sharing and delete data**

新たな送信を止めるには、PC一覧からHostへの接続を解除してください。接続解除は、そのiPhoneの接続先と認証鍵を削除します。すでにHostで開始した処理は自動停止しません。アプリ内の保存データはアプリの削除で消去できます。Keychainの認証鍵は削除後も残る場合があるため、アプリを削除する前にHostへの接続を解除してください。Host上の会話・ファイルとAIサービスに送った情報は別に管理されるため、Host管理者または該当サービスに削除を依頼してください。

To stop new sharing, remove the Host connection from the PC list. This removes that iPhone's Host entry and authentication key, but does not automatically stop work already started on the Host. Deleting the app removes its app-container data. Keychain keys may survive app deletion, so remove Host connections before uninstalling. Host files, conversations and data already sent to AI services are managed separately; contact the Host administrator or the relevant provider to request deletion.

**お問い合わせ / Contact**

運営者 / Developer: Tomoki Takate

[takatetomoki@gmail.com](mailto:takatetomoki@gmail.com)
