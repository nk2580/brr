# Planning mode

Requested 2026-11-01, using the ChatGPT desktop app as the reference. Official documentation describes Plan mode as context gathering and clarification before implementation, with `/plan` and Shift+Tab controls. References: [planning guidance](https://learn.chatgpt.com/docs/learn/best-practices), [slash commands](https://learn.chatgpt.com/docs/reference/slash-commands), [desktop feature reference](https://learn.chatgpt.com/docs/features).

Direct inspection of the reference app's window was denied by the computer-use policy. Brr uses the documented interaction and the owned fork's canonical [Plan instructions](../../runtime/codex/codex-rs/collaboration-mode-templates/templates/plan.md); pixel-for-pixel parity is not claimed. No runtime source patch was needed.

## Behavior

- A composer Plan toggle, `/plan`, and Shift+Tab select the mode. `/plan` with arguments sends that instruction in Plan mode. Mode changes wait until the active turn ends; ordinary follow-ups still steer its existing mode.
- `turn/start` sends a canonical `collaborationMode` with the selected model and effort and null developer instructions, allowing the runtime to use its built-in instructions. The host rejects independent thread instructions and mode settings that override the selected configuration. ChatGPT authentication and saved workspace permissions stay in force.
- Clarification requests support one suggested choice or a custom answer per question. Empty answers cannot be submitted.
- Proposed plans stream as Markdown. “Implement plan” appears only for an authoritative completed Plan item from the current successful turn, with no unrelated composer draft. Clicking it starts “Implement the plan.” in Default mode in the same conversation. It never implements automatically.
- Modes and completed plans restore from runtime history. Older, interrupted, failed or incomplete proposals do not become an implementation action. Failed sends preserve their draft and Plan choice.

## Tests and source review

- Host mode validation RED `12142`, GREEN `27306`. A later Sol finding reproduced independent thread instruction injection in RED `85523`; the tightened boundary passed the full native suite in `28979` (12 passed, one separately exercised integration ignored).
- Session mode RED `82280`, GREEN `83386`; streaming proposal RED `63352`, GREEN `56055`; restored proposal RED `84067`, GREEN in `34273`. Final-plan authority RED `47362`, GREEN in `84651`, with all 18 session tests passing.
- Luna's UI/client TDD covered mode controls, slash arguments, saved mode, explicit handoff, incomplete proposals, failed first sends, mode locks and custom clarification answers. The full frontend suite passed 79 tests and the production build passed.
- The real isolated owned-runtime connection/authentication-denial/shutdown integration passed in `7576`. Browser regression passed in `7881`.
- Sol reviewed the final host/session/client/UI source. Findings about thread instructions, final-plan authority and failed-send mode transfer were fixed with regressions; final source review is clear.

## Native delivery

The final release build `24960` passed: cached owned runtime 2.53 seconds, desktop host 29.83 seconds, ad-hoc signed macOS bundle 360.62 MiB. The final assets are `index-BEz9e4IM.js` and `index-C2ZDj0tQ.css`. Both actual packaged runtime and code-mode helper smokes passed, deep strict signatures verified, and license/provenance resources match source. This local build is not notarized.

In the actual signed-in app, `/plan`, Shift+Tab and the Plan button each toggled mode correctly before starting a conversation. A fresh Plan turn inspected only the fixture files and emitted a real `request_user_input` question with suggested choices. Blank submission was disabled. The custom answer `beta-custom` was accepted and used in the final Markdown proposal. The Plan badge stayed on and “Implement plan” appeared only after the authoritative final Plan item.

Independent hashes confirmed both fixture files unchanged and no additional fixture files before implementation. A full quit/relaunch restored the account, five conversations, saved Plan mode, proposal and handoff action. Clicking “Implement plan” started exactly one new Default-mode turn in that same conversation. The agent observed `./check.sh beta-custom` fail with `mode=unset`, changed only `settings.txt`, and reran the check successfully. An independent terminal check passed. `settings.txt` contains exactly `mode=beta-custom` plus a newline; `check.sh` retains its baseline hash. The final native UI showed Plan off and the successful implementation result.

[Structured native evidence](evidence/native-planning.json) binds the packaged executable hashes, actual mode transitions, final Plan item, question/answer, file change and command results. [The pre-implementation record](evidence/planning-before-implementation.json) captures the unchanged baseline before the deliberate handoff. [Fixture preparation](evidence/planning-fixture.json) retains the original hashes.

Sol independently audited the final evidence: all three executable hashes and the raw transcript hash match; fixture hashes and the passing check match; raw events confirm one Plan turn followed by one Default turn, the real custom clarification answer, final plan before handoff, one settings-file change and failing/passing checks. No contradiction was found. Native UI observations were recorded through CUA and were not separately replayed by the reviewer.
