# Projects as named groups of chats

Status: implemented and verified. Fresh development profiles only, without migrations or existing-data reset.

The accepted behavior is recorded in [ADR 008](../../docs/adr/008-project-chat-buckets.md). Projects have stable IDs and explicit membership, with a reusable creation/edit dialog and project-aware new-chat screen. Moving chats changes grouping; deletion ungroups them into Recents; archiving remains separate. Workspaces persist all resolved roots before the first turn and survive project edits and moves. Folderless ordinary chats, named agents and ticket runs use private app-owned directories.

Repository resolution groups folders by canonical Git repository, maps subfolders into one automatic worktree per repository, and uses non-Git folders directly. All hooks are reviewed before preparation begins. Partial failures identify retained worktrees. Shared background folders coordinate overlaps; runtime permissions and workspace tools receive the saved roots.

Verified so far:

- Frontend: 415/415 tests across 52 files; production build passes with existing dependency annotation and chunk-size warnings.
- Full native host suite: 173 passed, 0 failed, 7 optional/live checks ignored.
- Native workspace cases: 6/6, including multiple repositories, shared-repository subfolders, mixed folders, all-repository hook preflight and retained setup failures.
- Tickets: 8 native tests, 15 UI tests, and the approved folderless scratch/scope worker test pass.
- Dialog: 6 tests, including validation, failed saves, primary removal, keyboard focus trapping and restoration.
- Owned binary: clean signed-out profiles verify two runtime roots, primary cwd, secondary-root write coverage, read-only preservation, API-key rejection and ChatGPT OAuth start/cancel. No authenticated inference was performed.
- Full serial browser suite: 36 passed, 0 failed, 1 optional Rust sandbox fixture skipped. Project scenarios pass at desktop and 800×600, including 14 long folder paths. The welcome list scrolls without pushing the composer offscreen.
- Sol source review has no outstanding project blockers.

The combined suites include concurrent rich-chat and ticket work. Earlier shared fixture and coordinator failures are resolved. Commands, logs, limitations and hashes are recorded in [verification.json](evidence/project-buckets/verification.json); captures are alongside it. No authenticated provider inference or project-specific packaged desktop installation was performed.

Pre-existing rich-chat, ticketing, runtime and UI edits remain in the working tree. The worker file also received formatting changes while adding its folderless test; semantic changes were preserved. Shared interfaces were coordinated with the other active Brr chats with explicit user authorization. No commit, push, desktop bundle rebuild or installation is part of this task.
