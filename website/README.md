# Brr website

The standalone marketing site uses React, TypeScript and Vite. Install with
`pnpm install`, run `pnpm dev`, check `pnpm test`, and build with `pnpm build`.
The output is static `dist/`. No backend, account or tracking integration is
needed. The interactive tour is illustrative and does not run an agent.

`PRODUCT.md` and `DESIGN.md` record the approved Rainbow Operator direction.
`ASSETS.md` records generated artwork and the font license. Public project
documents are copies of the root project's license, notices, contribution
guide, governance and runtime provenance. Refresh them if those sources change.

The source download in `public/downloads/` is built from the parent Brr checkout
with `node scripts/prepare-source-release.mjs`, then copied from
`.cache/releases/brr-source.tar.gz`. It is excluded from its own source archive.
Build that archive again after source changes before deploying. Source archives
exclude their own download file. In a Brr source checkout, prepare it with:

```sh
node scripts/prepare-source-release.mjs
mkdir -p website/public/downloads
cp .cache/releases/brr-source.tar.gz website/public/downloads/
```

Run those commands from the parent Brr directory. A Sites website-source checkout
already contains the prepared download and can build independently.
