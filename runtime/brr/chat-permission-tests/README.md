# Brr chat permission tests

This small standalone crate compiles the actual app-server helper at
`runtime/codex/codex-rs/app-server/src/request_processors/brr_chat_permissions.rs`
without building the app-server test graph. Its two tests inside that helper
exercise profile matching. On macOS, `seatbelt_tests` also invokes the same
public Seatbelt policy generator used by the runtime and checks a selected
attachment read, foreign-chat/database denial, and staging write denial.

Run offline on the arm64 macOS target from the repository root:

```sh
CARGO_TARGET_DIR="$PWD/runtime/target" CARGO_INCREMENTAL=0 cargo test \
  --manifest-path runtime/brr/chat-permission-tests/Cargo.toml \
  --offline --target aarch64-apple-darwin
```
