# Brr chat permission tests

This small standalone crate compiles the actual app-server helper at
`runtime/codex/codex-rs/app-server/src/request_processors/brr_chat_permissions.rs`
without building the app-server test graph. Its unit tests inside that helper
exercise profile matching and hostile-CWD cases. On macOS, `seatbelt_tests` also invokes the same
public Seatbelt policy generator used by the runtime and checks a selected
original read, service-store/database denial, denied Unix-socket connection,
managed scratch-workspace write, foreign-chat denial, and staging write denial.
In-root CWD access requires the exact thread marker and saved workspace
snapshot created by Host; arbitrary `node/`, agent-store, service-root,
engine-home, or unregistered scratch CWDs do not reopen private files. The
service root is supplied by the host-owned
`BRR_SERVICE_DATA_DIR`; the runtime fails closed if it is missing.

Run offline on the arm64 macOS target from the repository root:

```sh
CARGO_TARGET_DIR="$PWD/runtime/target" CARGO_INCREMENTAL=0 cargo test \
  --manifest-path runtime/brr/chat-permission-tests/Cargo.toml \
  --offline --target aarch64-apple-darwin
```

On Linux, `codex-linux-sandbox` has a behavioral test named
`readable_carveout_below_denied_service_root_is_reopened_without_exposing_siblings`.
It verifies a staged child remains readable below a denied service root, the
active workspace remains writable, and a sibling auth store remains unreadable.
Run it on Linux amd64 before claiming Linux Bubblewrap enforcement support.
