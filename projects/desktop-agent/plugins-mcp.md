# Codex plugins and MCP connections

Requested: install Codex plugins directly in Brr and support the full plugin and MCP host specifications.

The compatibility architecture is recorded in [ADR 003](../../docs/adr/003-plugin-and-mcp-host.md). The project must retain a distinction between implemented support, verified support, and remaining full-spec coverage.

| Surface | Implemented behavior | Verification |
| --- | --- | --- |
| Package formats | Owned Codex installer/parser for `.codex-plugin` compatibility packages and root Agent Plugins manifests; selected OpenAI extensions, skills, hooks and registered apps | Real disposable marketplace installation, read-back, skills, toggle and uninstall fixtures; native UI install/skill-read/uninstall passed |
| Marketplaces | Add local/Git sources with refs and sparse paths; upstream local/Git/npm package sources; upgrade/remove | UI/client tests and real local marketplace lifecycle |
| Remote discovery | Paged global/workspace/personal search; local, directory, shared and created marketplace selectors; availability and policy retained | UI/client fixtures; native official Codex catalog loaded; remote workspace/personal catalogs not exercised |
| Connectors and hooks | Provider auth links, auth refresh, explicit onboarding prompt insertion; show hook source/command/hash and trust the exact definition | Behavioral tests and real runtime hook-trust read-back |
| MCP configuration | Complete stdio/Streamable HTTP objects retained; masked credentials restored on unchanged edits; remove/toggle/reload | Frontend/native boundary tests |
| Plugin credentials | Bearer, Basic and custom-header API keys; provider-registered OAuth secrets and asymmetric JWT keys in system credential store | Helper tests and real isolated credential write/read/remove, authenticated resource calls and secret scans |
| OAuth | Public/Basic/Post/private-key JWT, DCR, configured host-owned CIMD, endpoint/discovery/resource/scope overrides, refresh; explicit no-auth and mixed public access | Token/signature/metadata helper tests and actual local-provider authorization-code/refresh flows across public, Basic, Post, JWT and DCR/CIMD clients |
| Tools and resources | Thread-scoped tool calls and resource reads, catalog inspection, raw results, changed tool catalogs | Owned runtime legacy stdio and modern HTTP/stdio fixtures; UI tests |
| Prompts and completion | List/get/complete through owned app-server APIs; retain original arguments, metadata and continuation state | Real modern multi-round elicitation fixture and behavioral UI tests |
| Elicitation | Global form/URL requests with original IDs, validation, accept/decline/cancel; background-thread and disconnect handling | Frontend/native tests and real runtime continuation fixture |
| Protocol negotiation | MCP 2026-07-28 for ordinary and hosted connections, automatic stdio discovery, compatible 2025 initialization fallback | Real modern HTTP metadata and modern/legacy stdio fixtures |
| MCP Apps | Official AppBridge, isolated nested frames and origin/CSP checks, app-visible same-server tools with approval, declared resources/templates, reviewed links/message insertion, downloads | Real official App SDK browser fixture and native sandbox HTTP/CSP/teardown tests; native WebKit rendering not separately exercised |

The installed profile does not include legacy HTTP+SSE transport, optional Tasks or Skills-over-MCP, deprecated sampling/roots, device permissions, or OpenAI native device-verification proofs. Those capabilities are not advertised. OpenID settings support OAuth metadata discovery and endpoint overrides, not a standalone ID-token identity flow. Credential-bearing endpoints require HTTPS or loopback HTTP. CIMD requires a legitimate configured host-owned document; providers requiring their own registered client or proprietary verification remain a separate integration boundary. Account, plan and administrator restrictions are preserved. This is not an unconditional claim that every Codex plugin can run, or an exhaustive MCP certification.

Automated commands:

```sh
pnpm test
pnpm build
cargo test --manifest-path src-tauri/Cargo.toml --offline
pnpm runtime:test
pnpm runtime:extensions-test
pnpm runtime:build
pnpm runtime:extensions-smoke
pnpm runtime:extension-auth-smoke
pnpm runtime:smoke
pnpm runtime:helper-smoke
pnpm native:smoke
pnpm test:e2e --workers=1
```

The 2026-11-01 implementation snapshot passed 135 frontend tests, 26 native tests, the separate isolated native/runtime integration check, five policy tests and 28 owned extension helper tests passed. All six browser scenarios passed serially, including plugin management, the official MCP App SDK, worktree sessions and composer controls. The production frontend build passed. The owned runtime built, and a subsequent cache check completed without recompilation. Final plugin/protocol and enhanced local-provider authentication fixtures passed on that runtime.

The auth fixture uses a disposable engine home and a loopback provider, validates PKCE/state/callback/issuer binding, public/Basic/Post/private-key JWT clients, DCR and CIMD-to-DCR fallback, explicit empty/default/base scopes, exactly one configured resource, and authorization-code plus refresh grants. Plugin credentials are removed before uninstall; OAuth session tokens stay in a disposable file store. Both threadless and already-warmed thread-scoped resource reads fail after API-key removal. Public client ID binding is exercised; rotating the configured public client ID has source-review coverage but no separate end-to-end rotation test. No real third-party provider credentials or remote workspace/personal account catalogs were exercised. The official Codex catalog loaded in the packaged native app, and a disposable static plugin was installed, inspected and uninstalled through its UI; its test marketplace was removed afterward.

Sol's final focused source review found no remaining issues in credential authority, OAuth policy, plugin-cache generation handling, or credential reconnection and revocation. Credential changes shut down current and retained plugin HTTP clients under the session refresh gate, preventing a background refresh from publishing a stale client after revocation. Operations already sent to a service cannot be recalled. The full upstream test suite was not run; the owned helper harness and actual runtime fixtures are the verification evidence.

A fresh macOS arm64 debug app bundle passed deep strict ad-hoc signature verification, packaged runtime/helper smokes, the enhanced auth fixture and attribution checks. Native plugin installation/cleanup and MCP panel inspection passed. Exact artifact hashes and final delivery state are recorded in `evidence/plugins-mcp-verification.json`. This change does not replace or publish a production release.

Separate Agents work changed the shared shell during a stalled overnight desktop inspection. On 2026-11-02, all 47 focused extension/UI integration tests passed against that newer shell. The packaged snapshot remains the verified 2026-11-01 plugin/MCP build; later unrelated source was preserved and was not bundled by this task. Actual native WebKit MCP widget rendering remains unverified; the official SDK browser fixture and native sandbox tests passed.

## Interface audit

Product register; existing dark conversation styling and system typography preserved. No image assets are needed for connection management. Desktop and minimum-window screenshots were visually inspected.

| Dimension | Score | Evidence and finding |
| --- | --- | --- |
| Accessibility | 3/4 | Named controls, semantic forms, keyboard focus and error/status text. No comprehensive WCAG conformance claim. |
| Performance | 3/4 | No extra animation or image payload; schema validation interprets schemas without dynamic code execution. Production bundle warns above 500 kB. |
| Responsive layout | 3/4 | No horizontal overflow at the 800x600 desktop minimum; prompt action remains reachable. Drawer covers part of the sidebar at minimum width. |
| Theming | 2/4 | Existing visual language retained; several extension styles use local colors rather than shared tokens. |
| Anti-patterns | 3/4 | Ordinary product controls and source review. Dense nested source/result sections could be simplified in a later polish pass. |

Audit score: 14/20, good with minor refinement work. No blocking layout defect was observed in the requested desktop sizes. The drawer's small controls and dense raw payloads prioritize desktop inspection, and are not a claim of mobile support. Suggested follow-up: `impeccable typeset` for dense detail text, `impeccable extract` for shared tokens, then `impeccable polish`. These are optional refinements and do not establish protocol coverage.
