# bex

[English](README.md) | [日本語](README.ja.md)

bex lets you control Codex and Claude Code on your Mac from a desktop or iPhone app. Continue conversations, review code changes, and use terminals and a browser from either device.

Use the agent accounts already set up on your Mac and pick up existing conversations. Your Mac runs the agents and keeps the project files; your iPhone connects to it.

## Get bex

Mac and iPhone are the versions currently tested and usable. The project is still in development, so expect bugs.

| App | Requirements | Download |
| --- | --- | --- |
| Mac | Apple Silicon · macOS 26 | [getbex.dev](https://getbex.dev/) |
| iPhone | iOS 26 | [TestFlight](https://testflight.apple.com/join/UJbK9Ww7) |

Windows and Android versions are in development and have not been sufficiently tested.

## Get started

1. Install Git and at least one agent, Codex or Claude Code, on your Mac and sign in to that agent.
2. Open bex on your Mac. Follow the setup to check your agents and import existing projects, or choose a project folder.
3. Choose an agent and model, then start a conversation.
4. To use your iPhone, display a pairing invitation on your Mac and scan its QR code in the iPhone app. You can also paste the invitation manually.

Keep your Mac awake and bex running while using your iPhone. Each invitation pairs one device; create a new invitation for another device. Only pair devices you trust, since they can operate agents and files on your Mac.

## Development

Install Nix and Git, then run project commands through `scripts/dev-env.sh` to use the pinned environment.

```sh
scripts/dev-env.sh just --list
scripts/dev-env.sh just check -p agent-core
scripts/dev-env.sh just unit-tests
scripts/dev-env.sh just complexity
```

On macOS, `scripts/dev-env.sh just dev` builds and opens the development app with a separate Host, the background process that runs the agents. It shares your existing agent accounts and conversation history.

To update a source checkout, pull `main`, then build and restart the app or Host as usual. On its first startup, the new Host backs up existing conversation storage in a `before-storage-unification-*` directory beside the database and upgrades it automatically. Large histories can take longer on this first startup. No separate conversion tool is needed.

Development and integration rules are in [AGENTS.md](AGENTS.md); build and test commands are in the [justfile](justfile).

## More

- [Help and connection troubleshooting](docs/SUPPORT.md)
- [Changes](CHANGELOG.md)
- [Privacy](docs/PRIVACY.md)
- [Report a security issue](SECURITY.md)
- [Windows development and installation](docs/WINDOWS.md)
- [Nightly, Stable and iPhone TestFlight/Dev distribution](docs/RELEASES.md)
