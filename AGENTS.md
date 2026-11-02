# Working agreements

- Use pnpm for dependencies and scripts.
- This project explicitly uses Rust for the Tauri host and the owned Codex runtime fork.
- Use React, TypeScript, Vite and shadcn/ui component conventions.
- Use TDD for behavior changes: observe a failing behavioral test, implement, then refactor.
- Delegate bounded implementation chunks to Luna and use Sol for review.
- Preserve the ChatGPT-only inference invariant inside both the runtime fork and desktop host.
- Do not use an installed upstream Codex CLI as a fallback runtime.
- Keep source fork changes narrow and record them in `runtime/UPSTREAM.md`; preserve upstream licenses and notices.
- Project state lives in `projects/desktop-agent/state.json`; update it only with current evidence.
- Local project notes are the issue tracker for now. Domain context is in `CONTEXT.md`; decisions live under `docs/adr/`.

## Agent skills

### Issue tracker

Local project notes under projects/. See docs/agents/issue-tracker.md.

### Triage labels

Standard triage role names. See docs/agents/triage-labels.md.

### Domain docs

Single context in CONTEXT.md and docs/adr/. See docs/agents/domain.md.
