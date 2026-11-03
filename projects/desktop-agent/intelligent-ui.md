# Intelligent UI

The approved feature combines native interactive responses and sandboxed custom apps in ordinary chats, named-agent chats and task-result conversations. The agent selects the useful response format; text remains the default for simple answers. See [the decision](../../docs/adr/006-intelligent-ui.md) and [the v1 contract](../../docs/intelligent-ui-contract.md).

Native components cover comparisons, tables, timelines, bar/line charts, diagrams, tabs, selectors, sliders, inputs and forms. Complete components preview progressively with inactive controls. Final successful runtime history authorizes interaction; custom code runs only after complete host validation. Responses provide text alternatives, Reset and workspace expansion.

Outside data comes through existing agent tools. Custom apps have no direct networking or native access. Follow-ups require a host-owned review of the prompt and saved inputs. They preserve the composer draft, current mode and existing approval/question handling. Task-result follow-ups create a normal owning-agent chat under existing workspace rules.

Implementation used Luna chunks and Sol integration review. Behavioral failures observed before fixes include absent UI streaming, interleaved stream handling, missing transcript rendering/expansion/review, callback-driven app remounts, Reset ordering, optimistic typing, stopped-app copy and concurrent task-result submission. Shared fixtures check Rust and TypeScript document validation. Sol's final reviews found no remaining concrete defects in the reviewed changes.

Verified on 2026-11-02:

- All 308 frontend tests passed with two workers. The final renderer copy/layout adjustment also passed its nine focused tests and the production frontend build.
- Native Rust suite: 122 passed, five optional tests ignored. Owned runtime build, feature-on/off argument-parser tests, five policy tests and protocol smoke checks passed. The optional core registration test was stopped during test-profile compilation before its assertion; actual native generation subsequently exercised registration successfully.
- Combined browser run: 24 of 25 passed; the composer timeout passed on an isolated rerun (both composer scenarios passed). The separate MCP App scenario passed on its required normal development origin. Intelligent UI scenarios included persistence, Reset, expansion, reviewed follow-ups and the actual Rust custom-app sandbox with blocked networking and absent native APIs.
- Final local debug macOS bundle built, was ad-hoc signed, passed deep/strict signature verification, and passed packaged runtime and code-helper smoke tests. It is at `src-tauri/target/debug/bundle/macos/Brr.app`; this is local delivery, not a published release.
- In that exact app, the existing managed ChatGPT account generated a native comparison and custom saved counter through two successful render calls in a disposable read-only project. Native selector/slider keyboard use, saved state across expansion and full quit/reopen, Reset, Stop/Run, text fallback and a reviewed follow-up preserving Plan mode and the unsent draft were verified. A resize request of 100000 was contained; a 2.5-second busy app recovered. The final 800×600 window retained readable controls.

Evidence lives under `evidence/intelligent-ui/`; `verification.json` identifies the package, tests and native screenshots. `native-desktop.png` and `native-800x600.png` are browser screenshots of native components; screenshots prefixed `packaged-native-` are actual macOS app captures. Tests and acceptance prompts use synthetic data. Windows native acceptance remains unverified. The custom-app sandbox provides browser isolation and Stop/Run recovery, not hard CPU isolation for arbitrary JavaScript.
