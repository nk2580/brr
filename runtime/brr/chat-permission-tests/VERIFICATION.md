# Verification record

Run on macOS arm64, offline, using the shared `runtime/target` cache:

```sh
CARGO_TARGET_DIR="$PWD/runtime/target" CARGO_INCREMENTAL=0 cargo test \
  --manifest-path runtime/brr/chat-permission-tests/Cargo.toml \
  --offline --target aarch64-apple-darwin
```

Result: **3 passed, 0 failed**. This includes both unit tests compiled from the
actual app-server `brr_chat_permissions.rs` helper and an OS-level Seatbelt test
built from the actual `codex_sandboxing::seatbelt` policy generator. The
Seatbelt test verified that the selected staged attachment is readable, a
foreign chat's staged attachment and the chat database are unreadable, and an
agent process cannot write to the selected staging directory.

The first unprivileged invocation was blocked by the surrounding execution
sandbox before the child process started (`sandbox_apply: Operation not
permitted`). Re-running the exact test through the approved elevated execution
path succeeded. No repository or user files are used as fixtures; the test
creates and removes its temporary tree.
