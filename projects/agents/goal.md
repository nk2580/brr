# Persistent Agents

Implement the approved persistent Agents layer over baseline Brr. Preserve ordinary chats, ChatGPT-only owned runtime, Plan mode, plugins and WorkTrunk.

## Required behavior

- Chat-first Agents with conversational interview, reviewed instructions.md/soul.md, typed harness creation, user-directed profile changes and multiple sessions.
- All new ordinary and named-agent chats learn. No historical backfill. Private agent recall plus automatically shared user/project facts, source evidence, corrections, forgetting, undo and learned SKILL.md procedures.
- Separate task threads, four background workers/two task workers per agent, recurring schedules and dependency graphs. Agent-generated delegation requires reviewed graph approval. Mentions are explicit separate task assignments.
- Host owns task continuation; managed threads cannot bypass it with native spawning or goal continuation. Existing baseline temporary agents remain available.
- Window close keeps execution alive; Quit checkpoints/stops; restart reconciles and automatically resumes only provably recoverable work. Unknown external outcomes block rather than replay.
- Existing visual language, keyboard support and 800x600 minimum.

## Delivery

Use TDD, bounded Luna implementation, Sol review, frontend/native/runtime/browser tests and packaged smoke evidence. Capture current uncommitted source baseline. Do not claim integration or native acceptance from unit tests alone.
