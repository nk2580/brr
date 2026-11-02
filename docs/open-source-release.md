# Open source release

Brr is released under Apache License 2.0. The root `LICENSE` applies to Brr;
the owned Codex source fork retains its own `runtime/codex/LICENSE` and
`runtime/codex/NOTICE`, along with notices for third-party components. The
upstream revision and maintained fork changes are recorded in
`runtime/UPSTREAM.md`.

Create a local source archive with:

```sh
node scripts/prepare-source-release.mjs
```

The command writes `.cache/releases/brr-source.tar.gz`. Pass a path to choose a
different destination. The release selector allowlists project source,
configuration, website files, and required attribution. It omits local project
state and QA evidence, private configuration and credentials, caches, build
outputs, binaries, and nested Git metadata. It does not fetch upstream source
or use an installed Codex CLI.

The desktop's Plugins section supports marketplaces, plugin installation and
MCP configuration through the owned runtime. See the [plugin and MCP host
decision](adr/003-plugin-and-mcp-host.md) for supported capabilities and limits.
These integrations do not change Brr's ChatGPT-only inference policy.

Before publishing an archive, review the selected file list and inspect the
archive contents. The project is early and maintainer-led; see
`CONTRIBUTING.md` and `GOVERNANCE.md` for the current contribution process.
