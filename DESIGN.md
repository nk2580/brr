# Brr existing interface

Derived from src/styles.css and the verified native desktop, without changing the design direction.

- Dark conversation surface #212121, sidebar #181818, composer #303030, foreground #ececec and muted #999997. Existing CSS variables provide background, foreground, muted, border and radius roles.
- Native system sans-serif typography, compact labels and a readable Markdown transcript. Code and shell output use monospace text.
- Sidebar navigation remains stable. Model and reasoning controls sit above the transcript. The rounded composer and project/access controls remain below it.
- Use the existing Button primitive and lucide icon vocabulary. Keyboard focus uses the existing outline; disabled and pending actions remain visibly distinct.
- Planning adds a compact composer mode control and an inline proposed-plan review/action. Reuse current border, surface and spacing conventions. No generated raster assets, display typography, decorative animation or new color palette are needed.
- The interface adapts at its existing breakpoints. New composer controls must fit the 800x600 native window without hiding Send or Stop.
