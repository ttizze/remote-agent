# Shared BEX browser

BEX runs a dedicated Chromium profile on the Host. A paired iPhone views and
controls the same pages that the conversation's agent uses. The profile is
separate from the user's ordinary Chrome profile; cookies and site storage stay
on the Host, so signing in once is sufficient until the site expires the login.

Each conversation owns its tabs. Cookies are shared within the Host's BEX
profile. A popup belongs to the conversation that opened it. The browser survives
closing the phone's browser screen or disconnecting the phone. Restarting the
Host preserves the profile, but does not promise to restore open pages.

The existing authenticated iroh connection carries images and typed input. No
public HTTP endpoint, router configuration, or same-Wi-Fi requirement is added.
The first version renders compressed still frames while the screen is visible;
unchanged images are omitted from subsequent replies. It is intended for web
interaction, not video or audio streaming.

The phone and agent can operate the conversation's browser concurrently, without
claiming or returning control. Opening the browser makes input available as soon
as the first frame arrives. The Host serializes individual operations and rejects
phone input if the displayed tab has changed. Agent calls remain available while
the phone is connected or interacting. Both should observe the current page
before continuing after another participant changes it.

Codex and Claude receive a BEX browser MCP tool scoped by the Host to their
conversation. It offers screenshots, navigation, tabs, click, scroll, text,
keyboard keys and dialogs. Human steps such as login use the same shared page
without a browser control mode or a handoff button.

The Host uses an installed Chrome/Chromium executable (optionally selected with
`BEX_BROWSER_EXECUTABLE`). Remote debugging binds only to loopback with a
dedicated owner-only profile. The provider bridge uses an owner-only local Unix
socket. Neither browser input nor images are written to application logs or
client persisted conversation state. Browser screenshots returned to an agent
are tool results and therefore may be retained in that provider's conversation.

Verification must cover the live browser, persistent site storage, popup
ownership, concurrent phone and agent input, stale tab input rejection, and
the iPhone UI. A local connection test does not establish external-network
latency; that requires the physical iPhone on another network.
