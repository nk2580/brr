# Project management

Status: historical implementation, superseded by [Projects as named groups of chats](project-buckets.md) and [ADR 008](../../docs/adr/008-project-chat-buckets.md). The path identity and archive-before-delete behavior below describe the earlier implementation only.

## Confirmed scope

Projects are shown in the Home chat list. Each project row has an accessible three-dot actions menu and an expand/collapse control for its chats. The menu supports renaming, project defaults, expanding/collapsing chats, revealing the folder in the platform file manager, archiving all chats, and deleting the project from Brr.

Project settings cover the project name and default model, reasoning effort, and workspace permission. Defaults apply to new chats in that project; changing them does not rewrite existing conversations. Permissions remain within Brr's existing safe choices and do not enable full access or alternate inference providers.

Reveal opens the existing project folder in the platform file manager. Delete removes the project from Brr and archives its conversational chats; it does not delete project files, repositories, or worktrees.

## Registry and identity

Brr keeps a durable app-owned SQLite project registry with hidden tombstones. The registry is separate from runtime thread rows so that a missing or deleted project remains manageable and does not reappear automatically from chat history. Paths are normalized to one identity: existing directories use their canonical filesystem path, while missing absolute paths are normalized lexically. Existing files and relative paths are not project folders.

Registering a selected folder is atomic. If its normalized path already exists in the registry, registration unhides it without replacing its name or model, reasoning, or permission defaults. A new path receives a name from its final path component and unset defaults. This also makes a folder selected through a symlink or `..` alias resolve to its existing registry record.

## Safe archive-before-delete behavior

Before hiding a project, Brr reads all runtime conversations using complete pagination and identifies the selected project's original path and descendant paths. Worktree conversations are matched against their original project identity, not only their isolated worktree directory. Unrelated projects and chats must not affect the selected project's deletion result.

Active conversations and scheduler-owned work that cannot safely be archived block removal. Already archived selected conversations are complete and do not block removal. The host evaluates archive outcomes for every selected conversation; partial failure keeps the project visible and lets the user retry after refreshing the conversation list. Only a fully successful archive pass is followed by hiding the registry entry.

The host performs a best-effort final state check before issuing archive requests. Another client may change runtime state between that check and the archive operation; the runtime does not expose an atomic archive precondition. Brr does not claim this cross-client race is eliminated.

Archive retries and refreshes preserve any unsent composer draft. Deleting a project changes Brr's registry and chat archive state only; local files and worktrees remain untouched.

## Validation status

All 217 frontend tests and 16 browser scenarios pass. The native suite passes sequentially with 80 tests and three optional live-runtime tests ignored. A parallel native run hit the existing MCP listener teardown assertion; its isolated rerun and the full sequential suite passed without changes to that code.

Sol approved the source after archive, registry, keyboard and asynchronous-navigation fixes. The exact native application bundle was rebuilt, deep/strict signature verification passed, and the app was reopened with its saved Cyan rail and toolbar. Agent acceptance did not invoke archive/delete or inference on user data. Windows Explorer launch remains untested on Windows; drive and UNC path normalization is covered by unit tests.

Evidence, source and packaged-binary hashes: [project-management-verification.json](evidence/sidebar-rail/project-management-verification.json). Browser captures: [project menu](evidence/sidebar-rail/project-menu-minimum.png), [project settings](evidence/sidebar-rail/project-settings-minimum.png), [project removal](evidence/sidebar-rail/project-removed-minimum.png).
