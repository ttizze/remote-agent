# Remote Coding

Remote Agent connects a mobile control surface to coding work that remains owned by a trusted computer and its local Codex installation.

## Language

**PC Host**:
The trusted computer-side Remote Agent installation, comprising the Host Daemon and Host Manager.
_Avoid_: Host, server, backend, relay

**Host Daemon**:
The background component that connects Mobile Clients to the local Codex installation.
_Avoid_: Host, server, backend

**Host Manager**:
The local interface for managing the Host Daemon, Paired Devices, and connection status.
_Avoid_: Host window, desktop client

**Mobile Client**:
An Android or iOS application that displays and controls work performed through the PC Host.
_Avoid_: Frontend, remote terminal

**Paired Device**:
A Mobile Client whose device identity the PC Host has accepted for future connections.
_Avoid_: User, account, session

**Working Directory**:
The directory supplied to Codex for a Thread. A Paired Device may select any directory available to the PC Host account.

**Manual Edit**:
A deterministic file change authored in the Mobile Client's built-in editor and written directly by the Host Daemon, including while Codex Turns are active.
_Avoid_: Prompt, Turn, AI edit

**Editor Draft**:
Unsaved Manual Edit content kept on a Paired Device and checked against the current file Revision before it can be saved after reconnection.
_Avoid_: Workspace file, queued write

**AI Edit**:
A requested file change performed by Codex as part of a Turn rather than saved directly by the built-in editor.
_Avoid_: Manual edit, direct save

**PC Host Identity**:
The long-lived cryptographic identity pinned during pairing and used to authenticate replaceable QUIC transport certificates.
_Avoid_: Address, transport certificate, pairing ticket

**PC Host Profile**:
A Mobile Client's saved connection information, Paired Device identity, and isolated Mobile Cache for one PC Host Identity.
_Avoid_: Account, server profile, session

**Direct Connection Setup**:
A user-initiated attempt to create a router port mapping and verify Mobile Client reachability without changing the PC firewall or guaranteeing internet reachability.
_Avoid_: Open port, automatic public access

**Codex Thread**:
A Codex-owned conversation containing Turns and their Items.
_Avoid_: Remote Agent session, host session, chat record

**Turn**:
One user request and the Codex work that follows within a Codex Thread.
_Avoid_: Run, job, prompt

**Steering Input**:
Additional user input applied to the active Turn in a Codex Thread rather than starting an independent concurrent Turn.
_Avoid_: Second Turn, concurrent Turn

**Item**:
A Codex-owned unit within a Turn, such as a message, command execution, tool call, or file change.
_Avoid_: Event, block

**Approval Request**:
A pending Codex decision that requires an explicit response from a Paired Device before work can continue.
_Avoid_: Confirmation, prompt

**Blob**:
Binary content transferred separately from structured control messages.
_Avoid_: Attachment, RPC payload

**Snapshot**:
A complete current view of a Codex Thread obtained through the PC Host and used to reconcile Mobile Client state.
_Avoid_: Cache, stored history

**Mobile Cache**:
A durable device-local copy of Thread lists, messages, tool output, and diffs, excluding Blobs and Workspace files; it is reconciled with a Snapshot when the PC Host becomes reachable.
_Avoid_: Source of truth, Remote Agent history

**Live Event**:
A transient Codex update delivered after a Snapshot.
_Avoid_: History record, persisted event
