# Collapsible left sidebar

Implemented 2026-11-02. The sidebar toggle hides and restores the wide left pane, preserves the conversation draft and selected sidebar color, and remembers the layout on this device. A missing, malformed or unreadable preference defaults to expanded. Storage errors do not break the toggle. Hidden navigation is removed from keyboard and accessibility exposure, while a visible control remains available to reopen it.

Verification for the recorded build snapshot:

- Observed behavioral RED for the missing collapse control, then GREEN for collapse, keyboard reopening and draft preservation. Observed a second RED for missing remount persistence before connecting the preference helper.
- Six focused layout/preference tests passed. Full frontend suite: 177 tests in 24 files passed. TypeScript/Vite production build passed. Luna implemented the bounded preference helper; Sol review found no blocking issues.
- Production browser QA at 800x600: the hidden pane occupied no space, the main canvas filled the available width, and no horizontal overflow occurred. Keyboard reopening, visible focus after closing Settings, retained draft/color and both saved layout states passed.
- The regular Brr.app was rebuilt, deep/strict ad-hoc signature verification passed, and native collapse preserved the draft. The toggle cleared the macOS window controls. The test draft was removed without sending it.
- Two regular Brr builds were running with the same preferences, making the first regular restart result ambiguous. Repeated the restart test in the separately identified Brr Sidebar QA app: the saved value was true, a full quit/relaunch restored the collapsed pane, and reopening worked. Restored the QA app to expanded and closed it.

Evidence: [verification.json](evidence/sidebar-collapse/verification.json), [native restart screenshot](evidence/sidebar-collapse/native-restart-collapsed.png), and [800x600 screenshot](evidence/sidebar-collapse/browser-collapsed-800x600.png).

Scope: this is desktop layout work; runtime, inference and authentication policy were not changed. The existing owned runtime/helper were packaged without rebuilding the fork. A concurrent navigation-shell refactor changed App.tsx, styles and layout tests after this verified snapshot; those edits were preserved. This record certifies the exact packaged artifacts and frontend snapshot in the evidence, rather than later unrelated shell work. The broader desktop project remains in progress.
