# Projects as chat buckets

Status: accepted and implemented. Verification: [project evidence](../../projects/desktop-agent/evidence/project-buckets/verification.json).

Projects have stable IDs and explicit chat membership, independent of filesystem paths. Metadata contains a name, zero or more canonical folders, a primary folder and existing new-chat defaults. The host owns project storage and membership. A folder may be used by several projects; canonical duplicates within one project are rejected.

Creation and editing use the same accessible dialog. Project selection opens a project-aware new-chat panel; the composer supports No project. Moving a chat updates membership only. Deletion atomically ungroups chats and hides the project, retaining historical task and learning references. Archive all chats is separate and uses current membership plus safe runtime descendants.

Each session persists an execution snapshot before its first turn. It records starting directory, resolved roots, source mappings, worktrees, initial scope project and project revision. Git repositories use isolated WorkTrunk worktrees; selected subfolders from a repository share one new worktree. Non-Git folders are shared directly. Folderless chats use a private app-owned session directory. The runtime sees all saved roots, subject to the selected read-only or workspace-write access policy. Existing sessions retain their snapshots after metadata edits or chat moves.

Tasks and tickets carry project IDs separately from working directories. An assignment from an existing chat captures its original execution scope and folders, even after a move or project edit; operation retries cannot replace that captured source. Folderless ticket runs use a private session directory while retaining the reviewed project identity and revision. Ticket review captures project revision; changes require a fresh proposal. Shared-folder workers coordinate overlapping roots. Workspace tools select a resolved folder per tab. Missing or changed saved roots and missing/corrupt metadata block continuation, without falling back to shared source directories.

This change targets fresh development profiles. It includes no migration, backfill, existing-data reset, inference-provider change or upstream CLI fallback.
