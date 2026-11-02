# WorkTrunk manages project conversation worktrees

New project conversations run in a dedicated Git worktree by default. The Tauri host compiles Brr’s pinned WorkTrunk source fork as a native Rust dependency. The checkout is a Git submodule at `vendor/worktrunk`; `vendor/WORKTRUNK.md` records provenance and maintained changes. Brr does not discover or launch an installed `wt` executable.

The embedded session API creates a unique `codex/brr-…` branch from the selected project’s committed HEAD. It reuses WorkTrunk’s repository discovery, configuration, path templates, hook command expansion and approval persistence. Selecting a tracked subfolder preserves that scope in the new worktree. Dirty files in another checkout remain there.

The original project, branch, worktree and working directory are recorded in the private engine directory. Resuming uses the saved location; new chats use the original project. Existing conversations retain their saved directories. Separate managed-session markers make missing metadata fail closed. A corrupt record affects that conversation without blocking the chat list.

Setup inspection and approval are native host operations protected by request validation and ChatGPT authentication. They never enter the inference runtime. Brr displays the exact unapproved project templates and requires an explicit approval click. Only those templates are saved through WorkTrunk’s approval store. The draft remains ready for the user to send afterward. Creation freezes the approved selection before any mutation.

Native operations run off the UI thread. Fresh Git identity APIs avoid stale process-wide CLI caches in the long-running host. Non-Git folders, repositories without commits, failed synchronous hooks and unavailable saved worktrees produce recovery errors. No shared-checkout fallback, automatic commit, merge, cleanup, force flags or alternate runtime is introduced. The owned ChatGPT-only runtime keeps its sandbox and approval rules.

Pre-switch and pre-start hooks complete before inference. Post-start and post-switch hooks run independently in the background; required setup belongs in pre-start. Failed setup retains any created worktree. Archiving also retains it; merge and removal remain explicit operations.

The earlier external-CLI adapter has been replaced. Rust 1.98.0 is pinned for source builds; default WorkTrunk CLI features are disabled. The upstream license and fork provenance ship in application resources. Git and programs expressly named by approved hooks remain runtime requirements. See [the dependency and integration notes](../../vendor/WORKTRUNK.md).
