# User questions

Requested 2026-11-02. Ordinary chats, named-agent sessions, agent interviews/profile edits and background tasks share a question interface. Temporary subagents ask through their parent; learning workers have question tools disabled.

## Behavior

- `request_user_input` waits in Default and Plan mode. `request_user_input_async` returns immediately so independent work can continue. Both support one to three questions, optional suggestions and custom text, with no automatic selection or timeout.
- Questions and drafts are SQLite-backed. Each source thread/turn/item identifies one request; live JSON-RPC IDs are additionally scoped to a runtime generation. No historical-question backfill occurs.
- Pending questions replace the current chat's message composer. Questions appear one at a time, with suggested choices, a custom answer and explicit final submission. Next preserves the draft without sending it. The ordinary message draft returns after questions are answered or dismissed. Compact question and persisted answer/dismissal summaries remain in the conversation. Agents has a pending-question list even without named profiles; the rail counts requests without double-counting their waiting tasks.
- Native methods are `questions.list`, `questions.saveDraft`, `questions.answer` and `questions.dismiss`, through `agents_request`. Changes emit `brr/questions/changed`. Only the native user endpoint resolves input, and dismissal never grants permission or invents an answer.
- Live waiting replies require an exact accepted-answer tool output from the owned runtime. `serverRequest/resolved` only invalidates the live route because it also signals cancellation. Asynchronous/recovered replies steer an active turn or start a follow-up in the same thread with a durable correlation marker.
- A delivery intent is claimed once. Runtime client message IDs correlate history and do not provide runtime deduplication. After ambiguous admission, Brr reconciles authoritative history and keeps an unresolved delivery visible without blind resending.
- Background runs park in `waiting_input`, yielding worker capacity while retaining workspace ownership. Pending answers block task completion and generic continuation loops. Paused tasks retain questions/answers; explicit stop, cancellation or archive closes them. Safe answer continuation preserves the run identity and checks the current turn's external effects.

## Validation

Implementation uses Luna slices and Sol integration review. Behavioral regressions were observed before fixes for missing native APIs, the open Agents list subscription, duplicate raw tool output, false delivery acknowledgement on cancellation, and continuation past a newer unresolved tool outcome.

Final checks on 2026-11-02:

- 260 frontend tests across 32 files and the production frontend build passed after the composer redesign. Existing Zod annotation and bundle-size warnings remain.
- 41 agent-core tests and 92 native tests passed; four optional native smoke tests were excluded from the ordinary suite. Native checks used local loopback permission for the existing test server.
- All 20 browser scenarios passed after the composer redesign. At 800×600, questions replace Message/Send with one editable field, pending-list navigation focuses the composer, and Next/Back preserve answers. Keyboard choices and custom entry, draft restoration before submission, explicit keyboard submission, persisted answer restoration, visible Stop/footer controls and horizontal-overflow checks passed.
- The source-built owned runtime passed all four signed-in question-and-answer roundtrips: waiting and asynchronous questions in both Default and Plan modes. Its signed-out policy smoke also passed, including API-key login and unsigned inference rejection. Focused runtime question tests passed 24 checks and Brr policy tests passed five.
- Luna implemented bounded slices with behavioral RED/GREEN checks. Final Sol review found no remaining actionable P1/P2 issues after task cancellation, workspace recovery, delivery evidence and newer-turn safety fixes.
- The UI refinement also observed RED/GREEN for composer replacement, request switching and draft ownership, identifiable pending-list origins, Stop accidentally submitting, and focus after Next/Back. Sol's final UI review is clear.
- Rust formatting and whitespace checks passed. The runtime binary was rebuilt and staged; this change did not rebuild or install the desktop application bundle.

[Verification record](evidence/questions/verification.json)

[800×600 question form](evidence/questions/question-form-800x600.png)

[800×600 text-only question](evidence/questions/question-composer-text-800x600.png)
