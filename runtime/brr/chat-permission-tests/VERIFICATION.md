# Verification record

Run on macOS arm64, offline, using the shared temporary build cache:

```sh
CARGO_TARGET_DIR="$PWD/runtime/target" CARGO_INCREMENTAL=0 cargo test \
  --manifest-path runtime/brr/chat-permission-tests/Cargo.toml \
  --offline
```

Result: **5 passed, 0 failed**. This includes unit tests compiled from the
actual app-server `brr_chat_permissions.rs` helper and an OS-level Seatbelt test
built from the actual `codex_sandboxing::seatbelt` policy generator. The
Seatbelt test verified selected staged-original read, foreign-chat and
service-root store denial (auth, peers, project DB, Git repository, chat DB),
service-root write denial, Brr IPC Unix-socket connect denial, active
host-managed scratch-workspace write, selected-staging write denial, and
denial when CWD is set to `node/`, agent-store, service-root, engine-home, or
unregistered scratch paths. With the node-store CWD and outbound networking
enabled, the actual Seatbelt socket connect remains denied.

The Host environment boundary test also passed:

```sh
CARGO_TARGET_DIR="$PWD/runtime/target" CARGO_INCREMENTAL=0 cargo test \
  --manifest-path crates/Cargo.toml -p brr-host --offline \
  runtime_environment_uses_the_host_owned_data_root -- --nocapture
```

Linux Bubblewrap has a new behavioral test for the corresponding read-child
carveout and workspace write. It was not run here: this verification host is
Darwin arm64 and has neither Bubblewrap nor an installed Linux Rust target.
The packaged Linux verifier exercises the corresponding permissions through
the installed runtime. See `packaging/linux/README.md` for its scope.

The first unprivileged invocation was blocked by the surrounding execution
sandbox before the child process started (`Operation not permitted`). Running
the Seatbelt suite through the approved elevated execution path succeeded. No
repository or user files are used as fixtures; tests create and remove their
temporary trees.
