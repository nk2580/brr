# Brr

**Your work. Your way.**

Brr is an open, extensible desktop agent harness for research, writing,
organising and making. Give it a local project and a direction. Plan before a
change, steer while work is in progress, review the results, and pick up the
conversation later.

The desktop app uses Rust, Tauri 2, React and TypeScript. Brr owns a source fork
of Codex CLI under [`runtime/codex`](runtime/codex), bundled with the app.
Inference uses managed ChatGPT sign-in exclusively. Workspace tools run locally;
model inference goes to OpenAI. There is no API-key or alternate-provider path,
and no dependency on an installed Codex CLI.

## Build the desktop app

The verified desktop build is macOS. Install Node.js 22+, pnpm 10, Rustup
and the [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/). From
an extracted Brr source folder:

```sh
pnpm install
pnpm desktop:dev
```

For a Git checkout, first run `git submodule update --init --recursive`.
Brr's root toolchain file pins Rust 1.98.0 for its native WorkTrunk dependency.

The first runtime build downloads and compiles a large Rust workspace and
checksum-verified V8 artifacts. Choose a local project folder, sign in with
ChatGPT, select a model available to your account and start a conversation.

New conversations use isolated Git worktrees. [WorkTrunk](https://worktrunk.dev/)
is compiled into the app from its pinned source submodule; no separate `wt`
installation is required. Choose a Git project with at least one commit.
Brr displays project setup hooks for explicit approval before they run.
Existing legacy conversations retain their saved project directory.

To create a local app bundle with ad-hoc signing:

```sh
APPLE_SIGNING_IDENTITY=- pnpm desktop:build
```

The bundle is written to `src-tauri/target/release/bundle/macos/Brr.app` and
includes both owned runtime executables and upstream notices. This local
bundle is not notarized. A public, notarized app download is not available yet.

If the system pnpm/Corepack shim is unavailable, bootstrap pnpm locally:

```sh
npm install --prefix .tools --no-save pnpm@10.18.3
export PATH="$PWD/.tools/node_modules/.bin:$PATH"
pnpm install
```

## Work with Brr

Open **Agents** to create a persistent helper through an interview. Review its
proposed `instructions.md` and `soul.md`, then create it. Each agent has separate
chat sessions, model defaults, tasks, memory, learned skills and a profile you
can revise through chat. Ordinary **New conversation** chats remain available.
General agent chats do not require a Git project.

Select an agent in the composer to start a named conversation. An explicit
`@Agent name` at the start of a message assigns a separate background task
containing that message. Agent-generated task graphs wait for your review.
The Tasks tab supports dependencies and recurring cron schedules with an
explicit timezone. Background defaults are four workers total and two task
workers per agent; these limits are editable.

New ordinary chats, agent chats and tasks learn from new evidence. The Memory
tab shows private agent facts and shared user/project knowledge, sources,
corrections, forgetting and undo. Historical chats are not backfilled. Learned
skills retain successful tool evidence. Private recall is separated by the
harness under existing local tool permissions.

Closing the window keeps background work running; **Quit Brr** stops the app.
Restart reconciles saved work before continuing. Unknown external outcomes
block for review. No background work runs while Brr is quit.

Choose **Plan**, type `/plan`, or press **Shift+Tab** in the composer to inspect
a project and refine a proposed plan. Choose **Implement plan** to start work
in the same conversation. Planning mode and workspace permissions are separate.

During active work, send a follow-up with Enter or Send to steer the current
turn. **Stop** remains available separately. Brr keeps a rejected or stale
steering instruction as a draft instead of silently starting another turn.

Messages, shell output, plans and file diffs stream into the conversation.
Requests for elevated command or file access appear as explicit approvals.
Conversations and account state are stored in Brr's private application-data
folder, separate from `~/.codex`. Connected tools can transmit data to their
configured services; Brr is not an offline model.

## Extend and contribute

Open **Plugins** from the icon rail to browse marketplaces, install Codex
plugins, review hooks and configure MCP connections. Brr uses its owned Codex
runtime for package installation, skills, connectors and MCP. The desktop also
supports MCP prompts, resources, tools, form and URL requests, and sandboxed
MCP Apps. Service credentials do not change the ChatGPT-only inference policy.
See the [plugin and MCP host decision](docs/adr/003-plugin-and-mcp-host.md)
for supported transports, authentication and remaining limits.

Brr starts as an early, maintainer-led open source project. Read the
[contribution guide](CONTRIBUTING.md), [governance notes](GOVERNANCE.md),
[runtime provenance](runtime/UPSTREAM.md) and
[source-release instructions](docs/open-source-release.md).

The public source repository is [nk2580/brr](https://github.com/nk2580/brr).

## Checks

```sh
pnpm test
pnpm build
pnpm test:e2e
cargo test --manifest-path src-tauri/Cargo.toml
pnpm runtime:test
pnpm runtime:extensions-test
pnpm runtime:extensions-smoke
pnpm runtime:extension-auth-smoke
pnpm runtime:smoke
pnpm runtime:helper-smoke
pnpm native:smoke
pnpm test:build-tools
node --test scripts/prepare-source-release.test.mjs
```

Runtime and helper smoke tests require built binaries. They use isolated
engine homes and check protocol, authentication rejection, OAuth startup and
cancellation, helper execution and clean shutdown. They do not complete
sign-in or send inference. `pnpm dev` runs a browser preview; native workspace
access and inference require the desktop app.

## Marketing website

The standalone React, TypeScript and Vite website lives in [`website`](website)
and is hosted at https://nk2580.github.io/brr/.

```sh
pnpm --dir website install
pnpm --dir website dev
pnpm --dir website test
pnpm --dir website build
```

Open the `/brr/` URL printed by Vite. Publish a tested website with
`pnpm --dir website pages:deploy`; GitHub Pages serves the resulting `gh-pages`
branch. See the [website guide](website/README.md) for publishing details.
Its interactive tour is illustrative and does not start an agent.

## License

Brr's original code is licensed under [Apache 2.0](LICENSE). Retained upstream
and third-party components keep their own licenses and notices. See
[NOTICE](NOTICE), [`runtime/codex/LICENSE`](runtime/codex/LICENSE) and
[`runtime/codex/NOTICE`](runtime/codex/NOTICE).
