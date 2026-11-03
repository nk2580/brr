# Compact chat composer

The user requested a quieter, less complex composer while keeping its functions.

The message field is 64px tall with a single footer: the current project and Plan on the left; model, Chat options, Send and running Stop on the right. The project button replaces the repeated folder prompt and path. The container uses an 18px radius, a subtle neutral focus border and the existing Archivo/shared-surface theme.

Chat options holds reasoning effort, chat identity, new-chat read-only access, saved-thread access information and worktree details. Named-agent identity remains visible on the trigger; a lock marks a new read-only chat. Plan's Shift+Tab shortcut remains available. Enter sends, Shift+Enter adds a line, and the existing steering and Stop controls are preserved. Agent mentions retain their separate-task behavior.

The options panel is nonmodal, closes on Escape, outside interaction, resize or outside scroll, and restores trigger focus on explicit dismissal. Tab and Shift+Tab traverse fields and close action before returning to the surrounding controls. All selections continue to use existing app state and outgoing request handlers. No backend, runtime or inference-policy changes were needed.

Verification is recorded in [composer simplification evidence](evidence/composer-simplification-verification.json).
