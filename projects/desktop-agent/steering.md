# Steering an active turn

Requested 2026-11-01. Follow-up instructions sent with Enter or Send during an active turn use the runtime's existing `turn/steer` method. Each request supplies the selected conversation's exact `expectedTurnId`; the runtime rejects stale, ended, or non-steerable turns. Steering never falls back to starting another turn.

The desktop host allows only text input, a thread ID, and the expected turn ID. It checks ChatGPT authentication before forwarding and fills the protocol's text-element field. Model, provider, sandbox, configuration and context overrides are rejected. The source fork already checks managed ChatGPT authentication and the configured provider on this input path, so no upstream source changes were needed.

Stop remains separate and usable while a steering request is pending. Runtime events own turn state and transcript items. An acknowledged instruction clears only its unchanged composer draft. Rejections retain the draft and display an error in the original view; late responses do not overwrite a newer draft or another conversation.

## Behavioral verification

- Native request-policy RED: `steering_requires_the_expected_turn_and_accepts_only_text` failed because `turn/steer` was not allowed (session `59725`, exit 101). GREEN after adding the constrained method (session `53007`).
- Native account-gate RED: `steering_requires_chatgpt_and_forwards_the_exact_target_and_text` accepted an unsigned request (session `80652`, exit 101). GREEN after adding the account check and normalization; the full native suite passed 10 tests, with the owned-runtime integration intentionally ignored in that run (session `47827`).
- The separate real owned-runtime connection/authentication-denial/shutdown integration passed (session `3978`).
- Luna observed frontend RED for the missing client method and Enter failing to steer, then GREEN for text-only protocol routing, Enter/Send, exact turn targeting, same-turn transcript items, Stop, draft retention and view changes.
- Sol found completion-before-response races. Regressions first failed for an acknowledged instruction retained after completion and a silently rejected instruction; the fixes passed. Repeated Enter after completion also reproduced a duplicate new-turn request while the steering response was pending; a submission guard made that regression pass.
- Final frontend suite: 62 tests across 4 files passed, followed by TypeScript and Vite production build. The final asset is `index-Cl4mDJEm.js`.
- Browser preview regression passed (session `6341`); its initial sandboxed run could not bind localhost and was rerun with the required local-server access.

## Delivery verification

The final ad-hoc signed release was rebuilt with `APPLE_SIGNING_IDENTITY=- pnpm desktop:build` (session `19689`, exit 0). The owned runtime compiled from its unchanged source in 2.42 seconds using the build cache; the desktop host compiled in 33.28 seconds. The bundle is 360.62 MiB. Both actual packaged runtime and code-mode helper smoke checks passed. Deep strict signature verification passed, and packaged licenses/provenance match source byte-for-byte. The local app is not notarized.

In the actual signed-in native app, a fresh conversation ran exactly `sleep 45` in the disposable workspace. While it was in progress, the first follow-up was sent with Send and a second superseding instruction with Enter. Both appeared in the transcript and cleared their drafts; Stop remained available. The command completed normally with exit code 0 and no abort. The final reply was exactly `STEERING-ENTER-OK`, following the second instruction rather than the original requested reply.

The persisted runtime evidence contains one started/completed turn, three user messages all carrying that same turn ID, one completed command, and zero aborted turns. A full quit/relaunch restored the ChatGPT account and all four conversations. Reopening the QA conversation restored both steering instructions, the completed command and final reply. The three fixture verification files retain their prior SHA256 hashes.

[Structured native evidence](evidence/native-steering.json) binds the observations and exact runtime events to the final packaged executable hashes. Sol reviewed the final host/client/UI source and race regressions and found no remaining blocking findings.
