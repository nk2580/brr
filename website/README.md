# Brr website

The standalone marketing site uses React, TypeScript and Vite. Install with
`pnpm install`, run `pnpm dev`, check `pnpm test`, and build with `pnpm build`.
The output is static `dist/`. No backend, account or tracking integration is
needed. The interactive tour is illustrative and does not run an agent.

`PRODUCT.md` and `DESIGN.md` record the approved Rainbow Operator direction.
`ASSETS.md` records generated artwork and the font license. Public project
documents are copies of the root project's license, notices, contribution
guide, governance and runtime provenance. Refresh them if those sources change.

The production website is hosted at https://nk2580.github.io/brr/.
Vite uses `/brr/` as its base path, including during local development and
preview. Open the `/brr/` URL printed by Vite.

GitHub Pages publishes the `gh-pages` branch from its root directory. To
publish an update from a reviewed source checkout with GitHub push access:

```sh
pnpm install --frozen-lockfile
pnpm test
pnpm pages:deploy
```

Run these commands in `website/`. The deploy command builds `dist/` and uses
the `gh-pages` package to publish only that output, including `.nojekyll`, to
`https://github.com/nk2580/brr.git`. The repository URL is explicit so this
also works in a standalone website checkout. Source changes belong on `main`;
publish the built website separately with `pnpm pages:deploy`.

The primary source link opens the current `main` checkout. Build commands use
`git clone --recurse-submodules` to include the pinned native WorkTrunk source.
The secondary v0.1.0 archive link identifies an older source snapshot; website
builds do not rebuild or modify that frozen release. GitHub's automatic source
ZIPs do not include submodule contents. The old Sites preview is not a deployment
target; no Sites hosting manifest is retained.

The tour demonstrates Plan, queued follow-ups, explicit Steer now, Stop and
review using local example state. It never starts an agent or sends inference.
Product copy follows the desktop's chat drafts/context, project buckets, named
agents, Tickets, Plugins and owned ChatGPT-only runtime. Keep those claims and
the downloadable notices aligned with the root project when publishing.
