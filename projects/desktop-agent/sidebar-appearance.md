# Configurable sidebar colors

Implemented 2026-11-02 for the desktop application.

The later [persistent rail update](sidebar-rail.md) changes the default to Graphite and adds the always-visible icon rail. The user's latest correction moves the selected color to that fixed rail only; Home, Agents and Plugins collapsible menus stay neutral dark. Settings now calls this preference Rail color. The original eight color choices, saved preference key and explicit selections remain available. Validation below describes the original appearance delivery.

The main sidebar defaults to vibrant Lime. Settings > Appearance > Sidebar color offers Lime, Yellow, Orange, Red, Cyan, Blue, Violet and Pink. Selection applies immediately and is saved on this device. Missing, invalid or unreadable preferences fall back to Lime; a failed write keeps the session selection and displays a status message. Conversation drafts survive opening and closing settings, and Close settings restores focus to the Settings button.

The palette controls sidebar text, icons, navigation, selected chats, search, borders and focus. Blue and Violet use warm light text; the other colors use dark text. The sign-in button keeps a stable dark fill and light text across the palette. The conversation and composer retain their existing dark surfaces. Native radio controls support keyboard selection and a named checked state.

Validation:

- Observed behavioral RED for the missing Settings entry, then GREEN after integration. Observed a focus-restoration failure before adding trigger focus restoration.
- Final frontend suite: 169 tests in 22 files passed. Production TypeScript/Vite build passed.
- Production browser QA: all eight colors, arrow-key wraparound and focus, saved Violet restored after reload, and 800x600 layout without horizontal overflow. Opaque normal and selected navigation text contrast was checked from computed colors; the lowest measured ratio was 4.69:1. Inactive controls were excluded.
- Regular release Brr.app rebuilt, launched, and deep/strict ad-hoc signature verification passed. Native Settings initially selected Lime. Selected Violet, fully quit and relaunched the regular app, and confirmed Violet remained selected. ChatGPT account and saved chats restored. Restored Lime and left Settings open.
- Luna implemented the bounded palette/settings module; Sol reviewed integration and the final button contrast adjustment without blocking findings. Focusing the heading on opening settings remains an optional refinement.

Evidence: [verification.json](evidence/sidebar-colors/verification.json), [native screenshot](evidence/sidebar-colors/native-lime-final.png), [minimum window](evidence/sidebar-colors/browser-800x600.png), and [contrast measurements](evidence/sidebar-colors/contrast.json).

The existing owned runtime and helper were bundled without rebuilding the fork. No inference, authentication, runtime policy, marketing source or deployment changes were made for this feature. The app is locally ad-hoc signed, without notarization. Concurrent Agents work was preserved; this note does not certify that separate feature.
