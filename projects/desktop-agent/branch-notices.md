# Dismiss interrupted branch setup notices

Status: implemented and verified in the separate unsigned Brr Chat QA app. Installed Brr was not replaced; no commit or push was performed.

The interrupted-setup banner now has an accessible × control. Dismissal persists across app restarts without deleting or recovering the branch. Saved messages offers “Review interrupted setups” to reveal hidden pending notices and return to the existing chat or draft. Recovery remains an explicit action. The banner no longer implies that the current chat is blocked.

Dismissal keys use the authoritative operation identity, source, destination, phase, error and path. A new operation or changed pending state appears again. A transient local retry error does not invalidate a stored dismissal. If browser storage is unavailable, dismissal still works for the current session. Dismissal is disabled during recovery, and focus returns to the message editor when available.

Verification:

- Observed failing behavioral checks before implementation. The final App/helper run passed 122 tests; the failed-Recover/remount regression also passed separately after its readiness assertion was strengthened.
- Two browser scenarios passed at 800×600: keyboard dismissal, draft preservation across reload, manual review without recovery/submission calls, and attention for new or changed operations. Screenshots were inspected for wrapping and overflow.
- Production frontend and the separate macOS QA package built successfully. Existing dependency annotation, bundle-size and Rust warnings remain.
- Native WebKit at 800×600: reviewed a hidden notice, restored it without retrying setup, typed an unsent draft, dismissed the banner, verified editor focus and retained text, quit the app, confirmed it was closed, reopened it, confirmed the hidden notice remained available in Saved messages, and reopened the preserved draft. Only the synthetic dismissal draft was cleared afterward; the final banner remains dismissed.
- Bounded Luna implementation and Sol review completed. Sol's fingerprint finding was fixed and covered by the failed-Recover/remount regression.

The change is a frontend preference. Host/runtime source and packaged sidecars match the preceding verified checkpoint; no branch recovery was performed during native acceptance. Current hashes, logs, screenshots and limitations are in [verification.json](evidence/branch-notices/verification.json). Earlier conversation-first and composer-polish evidence remains historical. Signing, installed-app replacement and other platforms were not verified in this follow-up.
