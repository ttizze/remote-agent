# Direct push deployment

The Host sends awareness events directly to APNs and FCM. Provider credentials
stay on the Host and are read only from the deployment environment:

- APNs: `APNS_TEAM_ID`, `APNS_KEY_ID`, and `APNS_PRIVATE_KEY` (the complete
  ES256 private-key PEM), plus the registered bundle ID and APNs environment
  supplied by the iOS client.
- FCM: `FCM_PROJECT_ID`, `FCM_CLIENT_EMAIL`, and `FCM_PRIVATE_KEY` (the
  service-account RSA private-key PEM).

The Android app uses the pinned `com.google.firebase:firebase-messaging:24.1.2`
client. Its Firebase app identifiers are public build configuration, supplied
without committing `google-services.json` or credentials:

```sh
./gradlew :apps:mobile:assembleDebug \
  -PfirebaseApiKey='...' \
  -PfirebaseApplicationId='...' \
  -PfirebaseProjectId='...' \
  -PfirebaseSenderId='...' \
  -PfirebaseStorageBucket='...'
```

The same values may be provided through the `REMOTE_AGENT_FIREBASE_*`
environment variables. If they are absent, the Android model reports the push
capability as unsupported and does not ask for notification permission or call
the Firebase SDK. The values are emitted into the Android public `BuildConfig`
only; they are not Host provider credentials.

Android data pushes include the bounded activity aggregate. The native message
service renders it as one stable, ongoing notification while `activeCount` is
positive and turns that notification into a dismissible completion/failure
entry when the aggregate becomes idle. Ordinary per-thread notices use their
own persisted notification id; ids are allocated rather than derived from a
deep-link hash.

The FCM envelope carries `updated_at` as the Host delivery clock, while each
row's `updatedAt` remains its source event time. Android uses the envelope only
for bounded replay/future rejection and keeps accepted rows until the Host's
`activity_expires_at`; source timestamps are used for row ordering and display
expiry. Alert payloads are emitted only for newly entered attention or terminal
rows using the previous Host aggregate, with grouped rows getting a stable
transition identity built from sorted `[environmentId, threadId, phase,
updatedAt]` tuples. When the last terminal row expires, the Host sends a
typed empty aggregate with the latest retained source timestamp so native stores
clear that Host without inventing a replacement event.

Native routing uses `remoteagent://threads/<environmentId>/<threadId>` for a
thread notification. The usage widget uses
`remoteagent://settings/usage?tab=limits` on iOS and the `open_usage=true`
intent extra on Android. Grouped activity alerts use the shared
`remoteagent://overview` route, which opens the host-aware activity overview.
The ActivityKit `ContentState` is the shared
`title`, `subtitle`, `activeCount`, `updatedAt`, and `activities` record; each
activity row carries `environmentId`, `threadId`, titles, canonical snake-case
phase, short `status`, `updatedAt`, and its thread deep link.

The iOS app needs the APNs capability and the `aps-environment` entitlement in
the signed target. ActivityKit push starts use the `AgentActivityAttributes`
schema and `input-push-token`. The app-scoped push-to-start token is copied to
each retained Host's separate registration, so a Host can start its own card
while the app is closed. Each returned activity token is stored only on the
Host whose `environmentId` is in that activity's content state; see Apple's
[ActivityKit push notification documentation](https://developer.apple.com/documentation/ActivityKit/starting-and-updating-live-activities-with-activitykit-push-notifications)
for the required app and signing configuration.

FCM token replacement and provider error handling follow Google's
[token-management guidance](https://firebase.google.com/docs/cloud-messaging/manage-tokens):
an `INVALID_ARGUMENT` response is not treated as token expiry unless the
response proves that the exact registered token is unregistered.

No deployment command reads a real credential in tests or source control.
