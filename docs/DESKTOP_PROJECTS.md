# Desktop project organization

Open the `…` menu next to a project in the sidebar. Codex and Claude projects
use the same controls:

- Pin/unpin a project; pinned projects appear first.
- Edit the display name and section. Enter a new section name to create it,
  or clear the section to return to unclassified projects.
- Move to an existing section using the Section submenu.
- Open a local root in Explorer/Finder/the system file manager. Projects with
  multiple roots let you choose a root. This action is disabled for remote Hosts.
- Archive project chats; restore them using the same menu.
- Remove the project from the list; restore it using the sidebar's `一覧に戻す`
  action. This removes the sidebar entry, not its folder or native provider project.

Organization is saved on the device, keyed by Host storage identity and native
project ID. It does not rename filesystem folders or modify provider configuration,
conversation files, project membership, drafts, or pending submissions.

Archiving records a timestamp boundary and the loaded idle conversation IDs.
Older conversations fetched on later pages are also hidden when their update
timestamp is known. Running conversations remain visible while active. New
conversations after the boundary remain visible; an unseen conversation without
an update timestamp remains visible rather than being hidden accidentally.
Restoring clears the boundary and archived IDs. Native history remains readable.

The UI consumes `agent-core` presentation and reducer results. Persistence,
Host isolation, reversible visibility, running-task retention, later-page filtering
and new-chat visibility are covered by examples and property tests in
`crates/agent-core/src/state/projects.rs`.
