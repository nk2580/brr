# Native WorkTrunk project sessions

WorkTrunk source is compiled directly into Brr’s Rust desktop host from the pinned Git submodule at `vendor/worktrunk`. No installed `wt` executable is required or used. Reopening a conversation reuses its worktree; starting another keeps the original project selected. The composer shows the active branch and exposes its full path. Existing conversations retain their saved directory.

## Source setup

Initialize source dependencies with `git submodule update --init --recursive`. The root `rust-toolchain.toml` pins Rust 1.98.0. Use the normal pnpm build and Tauri commands. See [fork provenance](../../vendor/WORKTRUNK.md).

Projects opened in Brr must be Git repositories with at least one commit. Brr does not initialize repositories, commit another agent’s changes or copy dirty files. A new worktree contains the selected checkout’s committed HEAD; a tracked subfolder retains its working-directory scope. Git and any programs explicitly called by approved hooks must be available.

## Project setup review

WorkTrunk’s personal configuration and project `.config/wt.toml` govern configured paths and create/switch hooks. If project setup commands need approval, Brr shows their exact templates and hook phases. **Approve setup hooks** saves only those reviewed commands to WorkTrunk’s approval store; it does not start inference. Send the retained instruction afterward. **Cancel setup** retains the draft without granting approvals.

Creation freezes the approved hook selection before mutation. Synchronous pre-switch and pre-start failures stop setup; any worktree already created is retained. Post-start and post-switch hooks run in the background. Put required setup in pre-start.

## Lifecycle and recovery

The private engine directory records the original project, branch, worktree and working directory. Independent managed-session markers prevent deleted metadata from turning an isolated chat into a legacy shared-directory chat. Corrupt metadata blocks the affected conversation while healthy chats remain available.

Before each resumed turn, the host verifies the saved directory, branch and repository with fresh Git reads. There is no shared-checkout fallback. Archive retains the branch and worktree. Merge and removal remain explicit Git or WorkTrunk operations.

## Verification

The earlier external adapter’s historical evidence is in `evidence/worktrees-verification.json`. Native replacement verification passed on 1 November 2026 and is recorded separately in `evidence/worktrunk-native-verification.json`. Tests cover direct-library creation, in-app approval, stale Git identity, source isolation and submodule source packaging. Browser screenshots use protocol fixtures and do not represent authenticated native inference.

Verified native revision: `ab6256ebba9c53a8bc1c8e07d6d706f36a9072f3`, published on the owned fork’s `codex/brr-native` branch. The updated signed local app is `src-tauri/target/release/bundle/macos/Brr.app`. Its bundled WorkTrunk license/provenance and both runtime/helper smoke checks pass.
