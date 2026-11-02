# Brr's native WorkTrunk dependency

Upstream: https://github.com/max-sixty/worktrunk

Pinned baseline: v0.81.0, commit `e42f0f6fef2fdbd98fc92e5cd6c3cbea55b65b38`.

Maintained fork: https://github.com/nk2580/worktrunk

Native integration revision: `ab6256ebba9c53a8bc1c8e07d6d706f36a9072f3` (`codex/brr-native`).

Checkout: `vendor/worktrunk`, registered as a Git submodule targeting the maintained branch `codex/brr-native`. The submodule Gitlink pins the exact native integration revision. The original repository remains the checkout's `upstream` remote.

Brr depends on this checkout through Cargo with `default-features = false`. WorkTrunk's native library code is compiled into the desktop host; Brr neither searches for nor launches an installed `wt` executable. Git and any programs explicitly named by approved project hooks remain runtime requirements.

The maintained patch adds an `embedded` module for project worktree creation, setup inspection and explicit approval. It reuses WorkTrunk's repository discovery, configuration, path templates, command expansion, approval persistence and bounded command execution. Small library additions expose loading approvals from an explicit path, captured shell execution for approved hooks and fresh repository/worktree identity reads that bypass short-lived CLI caches. No CLI installation, shell integration, process-wide working-directory change or alternate inference provider is introduced.

The upstream dual MIT / Apache-2.0 license and copyright notices remain in `vendor/worktrunk/LICENSE`; this file and that license are packaged in Brr's application resources. Source archives include the expanded dependency checkout as well as `.gitmodules` so they can be built without nested Git metadata.

Builds require Rust 1.98.0, pinned by the repository's `rust-toolchain.toml`. Initialize submodules with `git submodule update --init --recursive` before building from a Git checkout. The submodule commit must contain the native API patch and be pushed before its Gitlink is shared.
