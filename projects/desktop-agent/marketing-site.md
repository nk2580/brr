# Rainbow marketing website and source release

The user confirmed the Rainbow Operator brief, a broader audience beyond
developers, Apache 2.0, and open-source publication. They further steered the
artwork to feel like 'agents go brr!'. The implementation lives independently
under `website/`; the native app theme was preserved.

## Delivery

- Production website: https://nk2580.github.io/brr/
- Public source: https://github.com/nk2580/brr
- Source prerelease: https://github.com/nk2580/brr/releases/tag/v0.1.0
- Website identity and direction: `website/PRODUCT.md`, `website/DESIGN.md`.
- Built-in image-generation asset paths and prompts: `website/ASSETS.md`.
- Machine-readable delivery: `evidence/marketing/delivery.json`.
- Final source archive evidence: `evidence/marketing/source-archive.json`.
- Browser screenshots: `evidence/marketing/desktop.jpg`, `mobile-tour.jpg`,
  `tablet.jpg` (all under the same evidence/marketing directory).

## Verification

The website has 12 passing behavior tests and a passing TypeScript/Vite build.
Manual browser inspection covered 320, 393, 768 and 1440 pixel widths, project
choices, explicit plan handoff, steering, separate Stop/recovery, copy feedback
and FAQ disclosure. Browser errors were empty. Font-weight selection, hero
overflow/clipping and retained tour scroll position were corrected after
inspection. Tour scroll reset and the public release download link have
observed failing/passing regressions. Sol's code review is clear.

The isolated source snapshot passed 81 desktop frontend tests and a production
build, plus 23 native tests (2 manual/runtime tests remained ignored). Three
source selector tests pass, including metadata stripping. Root/upstream
licenses match. The archive contains exactly the committed 9,917 source files,
with no AppleDouble entries, xattr metadata, symlinks, account files, private QA
records, project state, caches, generated binaries or website hosting identity.
GitHub reports the same uploaded source SHA-256 as the local artifact.

GitHub Pages is now the production host. The public page renders without a
sign-in gate, and 14 live HTML, asset and document responses match the verified
production build byte for byte. The source download points directly to the
verified GitHub release asset. The earlier private Sites preview is historical
and is no longer a deployment target.

## Source boundary and future updates

The shared root checkout received concurrent worktree-session implementation
while this task ran. It was preserved. Publication used an isolated, tested
snapshot in `.cache/releases/public-source-v0.1.0-final`; the root Git index and
branch were not changed. The tagged source release is frozen at
`6b9f9676a2dc345d8ca80ee382e19746b5268489`. Public main additionally contains the
tested website download-link correction at
`7b0c5a057eec8923d07b53566b7046ae9498c057`. Later live desktop edits are not
implicitly part of that source prerelease.

New source conversations currently require Git and Worktrunk; setup guidance
states that prerequisite. No notarized app binary was published. The source
release remains an early prerelease. Community participation is described
through forkable code and open contribution/governance documents, without
inventing an established foundation or contributor community.

The source archive and checksum are in `.cache/releases/` and mirrored in the
website's prepared download directory. Build source releases with
`node scripts/prepare-source-release.mjs`; review the selected snapshot before
publishing. Do not publish the whole dirty root checkout or private evidence.

The requested hero caption removal is published. The original rainbow artwork
remains, with its text overlay and unused caption styles removed. The website
suite (11 tests), production build, local visual check and Sol review pass.
Current screenshot: `evidence/marketing/hero-without-caption.png`.

The hero was subsequently regenerated with a matte painted-wood style, a simpler
chevron composition, and soft lighting. Built-in image generation and the exact
two prompts are documented in `website/artwork/hero-rainbow-wood.prompt.md`.
The responsive alpha WebPs, 11 tests/build, Sol review, and desktop/mobile checks
pass. Authenticated live rendering is now verified in
`evidence/marketing/hero-wood-live.png`.

Production hosting was migrated to GitHub Pages at the user's explicit request.
Vite and runtime asset/document URLs use `/brr/`; canonical and social image
metadata point to the public Pages URL. `pnpm --dir website pages:deploy` builds
and publishes to the dedicated `gh-pages` branch. The GitHub credentials lack
workflow-file write permission, so publication uses Pages' supported branch
source with existing repository access. Sites metadata was removed. The source
release remains frozen. Current proof: `evidence/marketing/github-pages-live.png`.

The footer copy now reads exactly `agnets go Brr!`, preserving the user's
spelling. All 12 tests, the production build, Sol review, GitHub Pages deployment
and live footer verification pass. Proof: `evidence/marketing/github-pages-footer.png`.
