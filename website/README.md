# Brr website

The marketing site uses React, TypeScript and Vite and embeds the desktop app’s
actual frontend in a browser preview. From the repository root, install the
shared app dependencies first with `pnpm install`. Then enter `website/`, run
`pnpm install`, and use `pnpm dev`, `pnpm test`, or `pnpm build`. The output is
static `dist/`. The preview uses sample workspace data; it does not connect to
a backend, start an agent, or send inference.

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
`https://github.com/nk2580/brr.git`. The build needs the complete Brr checkout
and its installed root dependencies to bundle the actual app. Source changes belong on `main`;
publish the built website separately with `pnpm pages:deploy`.

The primary source link opens the current `main` checkout. Build commands use
`git clone --recurse-submodules` to include the pinned native WorkTrunk source.
The secondary v0.1.0 archive link identifies an older source snapshot; website
builds do not rebuild or modify that frozen release. GitHub's automatic source
ZIPs do not include submodule contents. The old Sites preview is not a deployment
target; no Sites hosting manifest is retained.

The desktop preview is a separate lazy iframe with a restricted sandbox and a
minimum width of 800 pixels. On narrow screens, scroll it horizontally; it is
not a mobile app layout. Build the website from the repository checkout after
installing the root dependencies so Vite can bundle the shared desktop
frontend. Product copy follows the desktop's chat drafts/context, project
buckets, named agents, Tickets, Plugins and owned ChatGPT-only runtime. Keep
those claims and the downloadable notices aligned with the root project when
publishing.
