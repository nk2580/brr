# Brr desktop coding agent

Build a Rust Tauri React TypeScript desktop coding agent and harness with a ChatGPT-like interface. Use an owned source fork of Codex CLI as the runtime, support only ChatGPT login for inference, implement small chunks with Luna and review with Sol, and use test-driven development.

Completion requires live native and authenticated workflow evidence, not only mocked tests or a frontend preview.

Real ChatGPT coding, approvals, Stop, recovery and restart/resume pass in the native app. Stop's original failure was fixed with a real failing/passing regression and Sol review. The updated release bundle, both packaged runtime smokes and Sol's artifact checks pass. After explicit user approval, the rebuilt app passed the delayed-write Stop probe, post-Stop shell test, full quit/relaunch, restored account/history and resumed inference. Sol's final linked-evidence audit is clear; all required tasks are complete.

## Current evidence

- Frontend tests: latest 79 passed, including planning controls, authoritative proposals, explicit handoff, clarification answers, saved mode, steering and draft/view protection. pnpm build passed. Sol's final UI review found no remaining concrete issue. Browser preview Playwright check passed after the earlier UX changes.
- Native host tests: 12 passed, including canonical collaboration modes, instruction-override rejection, constrained steering and ChatGPT authentication enforcement. Isolated source runtime policy: 4 passed.
- Native dev executable and release bundle compiled. Actual native window inspected at 1280x860 and 800x600.
- Luna implemented session, request policy, message renderer and app integration using red/green tests. Sol reviews have produced regression fixes.
- Rust JSON-RPC fixture test passed. Native request policy tests passed.
- Owned brr-runtime built from pinned source in 18m07s; full debug runtime/helper build then completed in session 95746. Actual staged binary smoke passed: initialization, isolated signed-out model/history loading, API-key login rejection, ChatGPT OAuth start/trusted URL/cancel, and unauthenticated inference rejection. No installed Codex fallback exists.
- Staged code-mode host smoke passed: handshake, session open, JavaScript `text(2+2)` returned `4`, session shutdown and clean process exit.
- Native host connected to the actual owned runtime using an isolated account, loaded models, rejected inference, and shut down successfully.
- Actual native ChatGPT login and coding inference succeeded after the user unlocked the Mac and completed sign-in.
- Optimized release app built successfully with `APPLE_SIGNING_IDENTITY=- pnpm desktop:build`. Actual packaged runtime and helper smokes, deep signature verification, license/provenance comparisons, JIT/hardened-runtime checks and system-only dynamic dependency inspection all passed.
- Actual release native startup verified in session `80504`: `brr` process `34757` spawned packaged sibling `brr-runtime` process `34766`; both stayed alive over one minute. Both exited after the owned test session received Ctrl-C.
- In fixture `.cache/native-qa/20261101T004516130Z-ecc43d/workspace`, the signed-in agent observed three expected failures, fixed only `adder.go`, passed `./test.sh`, and displayed the diff. Independent tests/diff confirmed the outcome. Actual approval Decline blocked a sibling marker write; separate Allow wrote/read the exact marker and left the README hash unchanged.
- Quit/relaunch retained the signed-in account, both conversations and the full QA transcript. A real resumed follow-up passed `./test.sh`, recalled the Add fix and made no edits. Stop failed: `stop-probe.txt` was written after interruption; preserve it as evidence and use a fresh marker for the fixed-runtime retest.

## Completion

The final audit matched the explicit requirements to source, tests, native observations and packaged artifacts. Sol independently verified the native evidence record against the current binaries and fixture files and found no remaining acceptance gate. The linked matrix is in `verification.md`; structured native results are in `evidence/native-stop-recovery.json`.

## Build and process results

- Owned runtime main build session `89959` completed successfully; main binary staged in src-tauri/binaries.
- Full debug runtime/helper build `95746` completed and staged both binaries. Earlier build `16018` failed on the default V8 artifact route; checksum-verified upstream artifacts resolved it.
- Optimized build `72391` finished dependencies but failed at Brr's entrypoint compiler recursion limit. Added the same `recursion_limit = 256` attribute as upstream main/lib. Cached optimized runtime build and final packaging `43202` then completed successfully; its runtime step took 21m33s and reused compiled dependencies.
- Optimized desktop host compilation `96850` completed successfully in 5m07s; subsequent release runtime staging and final packaging also completed.
- Native debug app bundle build `18397` completed with sidecars and ad-hoc signing; packaged runtime and helper smokes and signature validation passed.
- Original Tauri dev process `60672` and Vite server `28524` were stopped with Ctrl-C after terminal verification. They must not be polled or treated as live sessions.
- Verified development native bundle: src-tauri/target/debug/bundle/macos/Brr.app.
- Verified optimized release bundle: src-tauri/target/release/bundle/macos/Brr.app (360.54 MiB). Packaged runtime smoke `41999` and helper smoke `21236` completed successfully. Packaging uses src-tauri/tauri.bundle.conf.json. No build or preview process remains active.
- Updated release build `42346` completed successfully after the cancellation/UI fixes: runtime 43m20s, native host 28.69s, signed bundle 360.62 MiB. Actual packaged runtime smoke `56248` and helper smoke `38462` both exited 0. Sol independently verified deep strict signatures, executable sidecars, hardened runtime/JIT entitlements, and exact current license/provenance resources.
- The user cleared the locked-desktop block and explicitly approved launch/QA. The rebuilt app's actual Stop killed the foreground command (exit 137), prevented the delayed marker after 35 seconds and stayed idle. A recovery shell test exited 0. Quit/relaunch preserved the ChatGPT account and all conversations, and real resumed inference succeeded. `evidence/native-stop-recovery.json` records executable hashes and persisted runtime events; fixture verification files remain identical to baseline.

## Active-turn steering update

The requested steering feature is complete. Enter and Send pass follow-up text to the exact active turn while Stop remains available. All 62 frontend tests, 10 native tests, the real isolated runtime integration and browser regression passed. Sol reviewed the final source after fixing response-ordering and duplicate-send races. The rebuilt signed release and both packaged smoke checks passed. Actual native QA accepted two steering instructions within one turn, completed the original shell command normally, followed the second instruction, and restored the full transcript through quit/relaunch. See [steering verification](steering.md) and [structured native evidence](evidence/native-steering.json).

## Planning-mode update

The requested ChatGPT-style planning flow is implemented and verified in the rebuilt native app: Plan control, /plan, Shift+Tab, real clarification with custom answers, final proposal review, unchanged fixture files during planning, saved Plan/handoff recovery through restart, and deliberate same-conversation Default-mode implementation with failing/passing checks. The full frontend/native checks, packaged smokes and Sol source review pass. See [planning-mode verification](planning-mode.md) and [native evidence](evidence/native-planning.json).
