# Shared public-site theme in the desktop app

Status: implemented, reviewed, rebuilt and running locally.

## Brand basis

The public site at [nk2580.github.io/brr](https://nk2580.github.io/brr/) is the shared brand basis for Brr's typography and color roles. The live site was inspected on 2 November 2026. ChatGPT desktop remains the functional reference for the coding workspace, navigation and conversation layout.

Use the Archivo variable family at weights 400–900 across Brr-owned app UI, with a fixed product type scale. Use the shared monospace family only for code and technical content such as project paths. The Archivo font asset is bundled locally under `public/assets/fonts`, carries its existing OFL notice, and is preloaded by the desktop page; normal use does not depend on a network font request.

The shared theme roles in `src/theme.css` follow the public site's computed palette: ink/background at 16% lightness, base surface at 21%, raised surface at 27%, paper foreground at 96%, muted text at 75%, and decorative lines at 34%. Controls use a stronger border role than decorative separators. The selected theme color supplies accents and keyboard focus. Graphite uses paper highlights; Blue and Violet use brighter matching accents for contrast. Danger, warning and success each use shared foreground, background and border roles. App-owned chrome and forms consume these tokens instead of local unrelated color values.

Third-party MCP app iframe content remains provider-owned and is the explicit theming exception. Brr's panels, controls and surrounding host chrome use the shared theme.

## Shell and saved appearance

Keep the established desktop geometry: 35px top toolbar, permanent 52px rail, and adjacent collapsible sidebar at 330px desktop, 250px narrow and 230px at the 800px native minimum. The rail and toolbar remain paired: the selected one of nine saved appearance colors applies to both bars, with readable icon, hover and focus states. The Home, Agents and Plugins contextual menus use neutral theme surfaces and do not inherit the selected bar color. Existing preference identifiers and Graphite fallback remain unchanged. Settings calls this choice Theme color. Its accent and foreground tokens are applied at the document root so body-portalled menus and Chat options inherit the same palette; the welcome icon, avatar, selection borders and checkboxes update immediately. Rail and toolbar focus uses their contrasting foreground so it stays visible against the selected background.

Color roles for the rest of the app derive from the shared theme: the conversation uses the ink background, ordinary panels use the base surface, and raised controls and menus use the raised surface. Maintain clear hierarchy with the fixed type scale, consistent Archivo weights, role-based borders, theme-colored keyboard focus and semantic status colors.

## Validation

The focused browser regression first failed on the previous system font. All 217 frontend tests and 17 browser scenarios now pass. Browser checks explicitly load Archivo, compare actual fonts and surface colours across sections, preserve monospace for code, and exercise the 800x600 window in expanded and collapsed layouts. Desktop and minimum-window captures were visually reviewed. The theme capture was rerun after adding a missing read-only Plugins fixture response so the nominal preview contains no fixture error.

Sol approved the shared roles after a control-border contrast correction. The native bundle was rebuilt and deep/strict signature verification passed. The exact app was reopened; native Home and Agents display the shared theme and retain the saved Cyan rail/top-bar pairing. Further native navigation stopped after the user-change guard and an accessibility timeout; the remaining sections were verified in the browser suite. No inference or user-data changes were invoked during visual acceptance.

The bundled Archivo asset matches the public site's font byte-for-byte and includes the OFL notice. [Verification details](evidence/theme-cohesion/verification.json) record source and packaged-binary hashes. Captures: [chat](evidence/theme-cohesion/chat-desktop.png), [Agents](evidence/theme-cohesion/agents-desktop.png), [Plugins](evidence/theme-cohesion/plugins-desktop.png), and [minimum window](evidence/theme-cohesion/chat-minimum.png).

## Accent synchronization correction

The user reported Pink chrome with lime accents. The focused browser regression reproduced that mismatch before the fix. Root-level palette tokens now drive both workspace and portalled controls, and reload retains the selected palette. Across all nine palettes, accents reach at least 4.5:1 against neutral surfaces and avatar text reaches at least 4.5:1 against its fill. All 14 focused frontend checks and two browser scenarios passed. Sol approved the scoped change. See [accent verification](evidence/theme-accents-verification.json).
