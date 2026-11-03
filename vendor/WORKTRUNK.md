# Brr's native WorkTrunk dependency

Upstream: https://github.com/max-sixty/worktrunk

Pinned baseline: v0.81.0, commit `e42f0f6fef2fdbd98fc92e5cd6c3cbea55b65b38`.

Maintained fork: https://github.com/nk2580/worktrunk

Native integration revision: `45d83a8cc83613005932631d6c2b6e5e670883ba` (`codex/brr-native`).

Checkout: `vendor/worktrunk`, registered as a Git submodule targeting the maintained branch `codex/brr-native`. The submodule Gitlink pins the exact native integration revision. The original repository remains the checkout's `upstream` remote.

Brr depends on this checkout through Cargo with `default-features = false`. WorkTrunk's native library code is compiled into the desktop host; Brr neither searches for nor launches an installed `wt` executable. Git and any programs explicitly named by approved project hooks remain runtime requirements.

The maintained patch adds an `embedded` module for project worktree creation, setup inspection and explicit approval. It reuses WorkTrunk's repository discovery, configuration, path templates, command expansion, approval persistence and bounded command execution. Small library additions expose loading approvals from an explicit path, captured shell execution for approved hooks and fresh repository/worktree identity reads that bypass short-lived CLI caches. No CLI installation, shell integration, process-wide working-directory change or alternate inference provider is introduced.

The local conversation-branch extension adds `embedded::create_snapshot`. Brr captures the source chat's current Git files, creates a destination from the captured source HEAD, materializes that snapshot, and only then runs the approved destination setup hooks. Source switch hooks are skipped because the source worktree is not switched. The call awaits setup and reports retained paths on failure, allowing the host's branch journal to keep incomplete destinations non-runnable. The host owns file validation, consistent capture, metadata and recovery; WorkTrunk continues to own repository creation and hook execution. This snapshot extension is included in the pinned native integration revision above.

The upstream dual MIT / Apache-2.0 license and copyright notices remain in `vendor/worktrunk/LICENSE`; this file and that license are packaged in Brr's application resources. Source archives include the expanded dependency checkout as well as `.gitmodules` so they can be built without nested Git metadata.

Builds require Rust 1.98.0, pinned by the repository's `rust-toolchain.toml`. Initialize submodules with `git submodule update --init --recursive` before building from a Git checkout. The submodule commit must contain the native API patch and be pushed before its Gitlink is shared.
