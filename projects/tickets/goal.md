# Lightweight local ticketing

Implement the user's approved plan: durable tickets associated with registered Brr projects, a four-column Todo / In progress / Review / Complete board, typed harness tools, and explicitly reviewed linked agent execution.

Tickets are app-local shared project artifacts. They remain separate from background Tasks, private agent transcripts, runtime thread state and repository files. Humans and agents may explicitly change any ticket status. No assignment or status change automatically starts execution.

## Delivery

- Storage: revision-checked edits, actor attribution, operation deduplication, comments/activity, ordering, archive/restore, immutable run proposals and one unfinished linked execution per ticket.
- Host: independent shared AgentStore initialization, main-webview-only native ticket API, host-derived harness authority and registered project identity, reviewed execution and persistent workspace permission.
- Board: Tickets rail destination, project sidebar and Home shortcuts; quick creation, ticket details, acceptance checklist, assignment/priority, search/filtering, accessible movement, history and run results.
- Acceptance: incremental RED/GREEN evidence, concurrency and recovery regression checks, browser/native minimum-window verification, complete suites, packaged macOS acceptance and Sol review.

## Boundaries

Use stable registered project IDs across folders and worktrees, following the user's explicit decision to integrate with the new project model. Projects without folders support reviewed execution in a private app-owned scratch workspace. Missing registered folders permit ticket editing but block execution. A single folder is implicit for execution; multiple folders require an explicit choice. Hiding a project retains its tickets. Keep ordinary chats, drafts, private recall, existing Tasks, WorkTrunk and ChatGPT-only inference intact. No cloud sync, tracker integration, custom columns, labels, due dates, subtasks, dependency planning, automatic backlog pickup or historical imports.

## Execution lifecycle

Run snapshots the ticket, selected agent and workspace permission for review. Approval queues an existing Task and links it atomically. Confirmed runtime-turn attachment moves an unchanged ticket to In progress. Successful task settlement with submitted evidence moves it to Review. Explicit subsequent status writes win, including writing the current status. Failure/waiting states appear as execution badges without extra columns. Complete never terminates a running task.

Implementation and validation evidence are tracked in [state.json](state.json) and [verification.md](verification.md).
