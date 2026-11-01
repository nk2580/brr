# Brr runtime fork

Brr owns the Codex source under `runtime/codex`. This is a source fork, not a dependency on a user's installed CLI.

- Upstream: https://github.com/openai/codex
- Imported commit: `67f0994b532b8740f14b5b738af7b7b2a915b1d6`
- Imported: 2026-10-31
- Upstream license and notices are retained in `codex/LICENSE` and `codex/NOTICE`.

The Brr entrypoint is `codex/codex-rs/app-server/src/brr_main.rs`. It reuses the fork's agent loop, tool execution, sandboxing, conversation storage, streaming protocol and OAuth implementation. Brr's runtime policy is in `codex/brr/policy`; the fork enforces it for login and before model input. Changes to upstream files should remain narrow and be documented here to make upstream merges reviewable.

Brr restrictions are compiled through the `brr-runtime` Cargo feature. The Brr binary requires that feature, so it cannot be built without its authentication policy. Upstream binaries and provider-specific upstream tests retain their original behavior when the feature is absent. Brr's verification gate runs its own policy tests and the actual runtime smoke test with the feature enabled.

Maintained source changes:

- `app-server/src/brr_main.rs` provides Brr's stdio entrypoint and immutable startup policy, and retains upstream's compiler recursion limit of 256 for optimized builds.
- `app-server/src/request_processors/account_processor.rs` rejects unsupported login modes before starting login.
- `app-server/src/request_processors/turn_processor.rs` requires managed ChatGPT authentication before model input.
- `app-server/src/config_manager.rs` checks authentication and provider for retained input paths.
- `core/src/config/mod.rs` validates inference endpoints and selects the built-in OpenAI route and ChatGPT login policy.
- `core/src/unified_exec/process_manager.rs` terminates the exact shell process when its active exec or stdin-poll tool call is cancelled; yielded terminals remain available between calls.
- `core/tests/suite/code_mode.rs` covers interruption during foreground exec and a nested stdin poll, then checks that each shell command cannot write afterward.
- The app-server/core manifests wire the Brr feature and the standalone policy crate under `brr/policy`.

Build the runtime with `pnpm runtime:build`. The desktop host must use this binary; it must not fall back to an unrelated installed Codex CLI. Runtime compilation and native packaging are tracked in `projects/desktop-agent/state.json`.

The build also compiles the fork's `codex-code-mode-host` helper and stages it beside `brr-runtime`. `pnpm desktop:build` applies `src-tauri/tauri.bundle.conf.json` so both binaries ship with the app. The helper is part of Brr's owned runtime; it is not resolved from the user's installed Codex CLI.

`scripts/runtime-artifacts.mjs` fetches V8 artifacts from the upstream OpenAI release and verifies each file against `codex/third_party/v8/rusty_v8_152_2_0.sha256`, including cached files. These replace the default V8 download route for this source snapshot. The build selects the installed stable Rust toolchain and disables incremental compilation and development debug symbols to bound disk use.

The macOS bundle uses `src-tauri/Entitlements.plist`, matching `codex/.github/scripts/macos-signing/codex-code-mode-host.entitlements.plist`. Hardened runtime remains enabled when signing. Verify the packaged helper with `node scripts/smoke-code-host.mjs '<Brr.app>/Contents/MacOS/codex-code-mode-host'` after signing.
