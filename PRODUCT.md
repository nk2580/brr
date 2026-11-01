# Brr product context

## Register

Product: a native desktop coding workspace. Design serves an ongoing conversation and project task.

## Users and purpose

Developers select a local workspace, sign in with ChatGPT, and ask an owned agent runtime to inspect code, plan, implement, run checks and explain results. They need to review messages, shell output, changes and approval requests without leaving the conversation. The canonical domain is in CONTEXT.md and the runtime decision is in docs/adr/001-owned-runtime.md.

## Reference and personality

The user explicitly chose the ChatGPT desktop app as the functional and visual reference. Brr's existing interface is quiet, compact and familiar: a project/chat sidebar, conversation transcript and persistent composer. Keep existing shadcn-style primitives and system typography. Planning extends this interface instead of introducing a new layout or brand.

## Principles and anti-references

Expose the mode the agent is using and make implementation a deliberate transition from planning. Keep planning distinct from the runtime progress checklist and from workspace access permissions. Prefer ordinary controls and inline review actions. Avoid decorative dashboards, extra navigation, new themes and redundant status panels. Preserve ChatGPT-only inference and the owned runtime boundary.

## Accessibility

Maintain the existing visible keyboard focus, named controls, reduced-motion support and 800x600 native minimum. Mode, pending and error states must be available to assistive technology. Keyboard users can toggle planning with the documented composer shortcut, answer clarification requests and review before implementing.
