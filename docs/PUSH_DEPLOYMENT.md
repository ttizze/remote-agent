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

Native routing uses `remoteagent://threads/<environmentId>/<threadId>` for a
thread notification. The usage widget uses
`remoteagent://settings/usage?tab=limits` on iOS and the `open_usage=true`
intent extra on Android. The ActivityKit `ContentState` is the shared
`title`, `subtitle`, `activeCount`, `updatedAt`, and `activities` record; each
activity row carries `environmentId`, `threadId`, titles, canonical snake-case
phase, short `status`, `updatedAt`, and its thread deep link.

The iOS app needs the APNs capability and the `aps-environment` entitlement in
the signed target. ActivityKit push starts use the `AgentActivityAttributes`
schema and `input-push-token`; see Apple's
[ActivityKit push notification documentation](https://developer.apple.com/documentation/ActivityKit/starting-and-updating-live-activities-with-activitykit-push-notifications)
for the required app and signing configuration.

No deployment command reads a real credential in tests or source control.
