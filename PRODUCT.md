# Brr product context

Tickets track local project work through Todo, In progress, Review and Complete. They are shared project artifacts available independently of the coding runtime, with ordinary agent tools for creation and status changes. Assignment does not start work. Human-reviewed run proposals connect tickets to existing background Tasks, preserving the difference between board progress and execution state. The Tickets rail section and each project's Open tickets shortcut keep work accessible without losing the conversation draft. See [local ticketing](docs/adr/007-local-ticketing.md).

Interactive responses adapt the answer to the task. The agent can use native components for structured exploration or a self-contained app for a calculator, simulation or game. Ordinary text remains useful and requires no widget. Responses appear inline with saved selections and inputs, a text alternative, Reset and optional workspace expansion. Apps obtain outside data through the agent. Requests for agent work are reviewed explicitly; task-result follow-ups open a regular owning-agent chat rather than taking over task execution. See [Intelligent UI](docs/adr/006-intelligent-ui.md).

## Register

Product: a native desktop coding workspace. Design serves an ongoing conversation and project task.

## Users and purpose

Developers select a local workspace, sign in with ChatGPT, and ask an owned agent runtime to inspect code, plan, implement, run checks and explain results. They need to review messages, shell output, changes and approval requests without leaving the conversation. The canonical domain is in CONTEXT.md and the runtime decision is in docs/adr/001-owned-runtime.md.

## Reference and personality

The user chose the ChatGPT desktop app as the functional reference and Brr's public site as the shared brand basis. Brr remains a quiet, compact coding workspace with a project/chat sidebar, conversation transcript and persistent composer. Apply the public site's typography and color roles consistently to app-owned surfaces while keeping the existing shadcn-style primitives. Planning extends the current interface instead of introducing a separate layout or brand.

## Principles and anti-references

Expose the mode the agent is using and make implementation a deliberate transition from planning. Keep planning distinct from the runtime progress checklist and from workspace access permissions. Prefer ordinary controls and inline review actions. The latest desktop reference uses a permanently visible icon rail and a separate collapsible sidebar, with chat to the right. The user specifies exactly Home, Agents and Plugins at the top of the rail, then the user profile avatar and Settings at the bottom. All five have tooltips and remain visible; only the adjacent sidebar collapses through its own control. Match graphite surfaces, compact icon spacing and project/chat hierarchy. Apply configurable colors to the permanent icon rail and the full top bar together, retaining saved preferences and Graphite as the default. The collapsible Home, Agents and Plugins menus keep their neutral dark surfaces. Keep the conversation area quiet and dark. Avoid decorative dashboards and redundant status panels. Preserve ChatGPT-only inference and the owned runtime boundary.

The profile avatar opens a full Profile page in the workspace beside the permanent rail, showing account details and the existing authentication actions. The chat list stays focused on projects and conversations; account controls live on Profile. Close or Escape returns to the previous section with the conversation draft and saved sidebar layout intact. Repeated Profile activation stays on the page; the other rail destinations remain available.

Use the Archivo variable family for Brr-owned interface text and a fixed product type scale; reserve monospace for code and technical content. Apply the public site's ink, surface, raised surface, paper, muted text and line roles through shared theme tokens. The saved theme color supplies app accents and focus rings, including body-portalled controls. Graphite uses paper highlights; Blue and Violet use brighter matching accents for contrast. Semantic status colors retain their meaning. Embedded third-party MCP app content remains provider-owned; Brr's host chrome around it follows the shared theme. Preserve the nine saved rail color choices and apply each selected color to both the permanent rail and full top toolbar. See [theme cohesion](projects/desktop-agent/theme-cohesion.md).

Projects group chats independently of folders. New project opens a compact dialog for its name and optional folders, with a primary-folder choice. The new-chat panel shows the selected project, attached folders and starting folder above the existing composer. Project settings retain model, reasoning and access defaults for new chats. Home project names select the new-chat context; a separate disclosure expands children. Chats can move between projects or No project without changing their saved workspace or learning/task scope. Delete removes the bucket and leaves its chats in Recents; Archive all chats remains separate. Project identity and membership are persisted explicitly in SQLite, never inferred from paths. Every new session uses all attached folders together, with automatic Git worktrees and direct non-Git folders. See [project chat buckets](docs/adr/008-project-chat-buckets.md).

## Accessibility

Maintain the existing visible keyboard focus, named controls, reduced-motion support and 800x600 native minimum. Mode, pending and error states must be available to assistive technology. Keyboard users can toggle planning with the documented composer shortcut, answer clarification requests and review before implementing.
