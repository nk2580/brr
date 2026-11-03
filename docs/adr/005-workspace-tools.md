# 005: Native workspace tools beside isolated agent chats

Status: Accepted by the user, 2026-11-02.

The user requested a resizable tabbed workspace beside chat with an inline annotatable agent-operated browser, terminal, Git diffs and manual file editing. They selected original project folders for manual tools, and collecting notes before sending them to chat.

Use a persistent React panel with one native child webview per browser tab and a separate Rust tool service for PTYs, file operations and Git. Capture original project roots per tab. Preserve the existing WorkTrunk execution boundary for model shell/file tools. Use one foreground chat context for browser authorization plus immutable per-tab ownership. Only an unowned tab may be claimed by its explicitly supplied originating chat; creating a first saved chat promotes tabs from that exact draft identity. Popups carry source tab identity and are rejected across chat boundaries.

Expose constrained typed browser actions as an owned dynamic tool. Refresh catalogs for existing foreground sessions via a Brr-feature-only app-server endpoint calling the existing persistent replacement API. Do not expose arbitrary eval or manual-terminal execution to model tool calls. Keep ChatGPT-only runtime/host authentication unchanged.

Scope native IPC to the local main webview. Remote webviews have no app capability grant. Treat snapshots as untrusted, bounded data. Keep annotation notes in frontend state until the user explicitly appends them to the originating message draft. Use explicit conflict-aware file saves and keep unsaved tab contents while hidden.

This requires native acceptance beyond browser fixtures: DOM fixtures verify tool UI and layout; actual child webview rendering, annotation polling and PTY input must also be exercised in the packaged macOS application. Windows implementation remains unverified until tested on Windows.
