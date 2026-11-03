# Composer layout polish

Status: implemented and checked in the separate unsigned Brr Chat QA app. Installed Brr was not replaced; no commit or push was performed.

The reported composer had a second attachment toolbar below the existing controls. It now uses one footer: a compact Add context button, Project and Plan on the left, then Model, Chat options and send actions on the right. Attachment cards and import errors appear above the footer with spacing only when present. Empty composers are at most 125px tall in the checked layouts, while the editor retains its 64–200px growing area and full click-to-focus target. Archivo, theme tokens, Agents and the existing submission/settings handlers remain intact.

The running-action trigger uses the existing icon vocabulary without a native disclosure triangle. Its menu opens upward without moving Send or Stop and closes on Escape or steering selection. Long project, model and agent names shrink before primary actions at the native minimum.

Verification:

- Observed failing browser checks before the layout, running-menu and empty-editor focus fixes.
- Five focused browser scenarios passed, covering the new layout and existing long-name model controls at desktop and 800×600 sizes.
- All three existing chat browser scenarios passed against the final UI, including drafts, editor menus, attachment previews, queue editing, steering and Stop.
- All 142 targeted interaction tests passed serially: App 115, ChatContext 12, ChatEditor 14 and composer model controls 1. An earlier concurrent run had three failures; the serial run passed without product changes.
- Production frontend and the separate native QA bundle built successfully. Existing dependency annotation, bundle-size and Rust dead-code warnings remain.
- Native WebKit at 800×600: single footer, upward Add menu, Escape restoring trigger focus, multiline editing, Chat options retaining text, and disabled Send for an empty draft were inspected.
- Bounded Luna implementation and Sol review completed with no remaining actionable findings.

The checkpoint hashes, screenshots, captured test results and limitations are in [verification.json](evidence/composer-polish/verification.json). The frontend-only change did not require runtime or host behavior changes. Existing runtime/host source and packaged sidecars were checked against the preceding conversation-first checkpoint. Signing, other platforms and a native active-turn steering-menu flow were not reverified in this follow-up; running controls were exercised in the browser.

Later UI follow-up: [dismissible interrupted branch notices](branch-notices.md), with current package and affected source hashes in its verification record. This composer's original metrics remain historical evidence.
