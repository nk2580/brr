# WorkTrunk manages project conversation worktrees

New project conversations run in a dedicated Git worktree by default. The Tauri host invokes the real [WorkTrunk CLI](https://github.com/max-sixty/worktrunk), rather than implementing a competing worktree manager or asking the coding agent to create its own directory.

The host creates a unique `codex/brr-…` branch from the selected project's current committed HEAD using `wt switch --create --base=@ --no-cd --format=json`. WorkTrunk owns the resulting path, user configuration, project configuration and approved lifecycle hooks. The host uses the structured result to choose the runtime's working directory. Selecting a subfolder preserves that subfolder within the new worktree. Worktrees start from committed files; unsaved, staged and untracked changes in another checkout are left in place.

The original project, branch, worktree and session working directory are recorded in the application's private engine directory. A resumed conversation reuses that exact worktree. New conversations continue to use the original project. Existing conversations keep their saved working directory; migrating a live conversation could invalidate tools, approvals and uncommitted edits.

Authentication and request validation run before worktree creation. The owned ChatGPT-only runtime and its sandbox and approval rules remain authoritative for agent turns. WorkTrunk is invoked directly, without a shell, automatic command approval, force flags or alternate Codex runtime. Unapproved project commands must be approved through WorkTrunk before an unattended session can start.

A missing executable, non-Git folder, repository without commits, failed synchronous setup hook, invalid result or unavailable saved worktree blocks the affected session with recovery guidance. WorkTrunk's background post-start and post-switch hooks run independently; use a synchronous pre-start hook for setup that must finish before inference. The host does not fall back to editing the shared project checkout. Worktrees and branches remain after archive and failed setup so user work and hook output can be inspected. Merge and removal remain explicit WorkTrunk operations.

WorkTrunk is a separately installed prerequisite. The application searches PATH and conventional macOS installation directories; it does not install software or change shell configuration during chat creation. See [the setup and verification notes](../../projects/desktop-agent/worktrees.md).
