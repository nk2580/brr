# Intelligent UI v1 implementation contract

`brr_ui_render` takes one JSON document. Required fields: `version: 1`, `kind: "native" | "app"`, nonempty `title` and `text` (the accessible text alternative), `initialState` (JSON object), and `actions` (array of `{id,label,prompt}`). Native documents require `components`; app documents require a nonempty self-contained `html` string. No model-supplied thread, call, revision, URL, permission or RPC fields are accepted. Document identity comes from the runtime thread/call; its hash comes from canonical host serialization.

Native components have unique nonempty `id` and `type`. The types and fields are:

- `text`: `text`.
- `group`: `children`.
- `comparison`: `columns: [{label,text}]`.
- `table`: `columns: string[]`, `rows: (string|number|boolean|null)[][]`.
- `timeline`: `entries: [{label,text}]`.
- `chart`: `chartType: "bar" | "line"`, `points: [{label,value:number}]`.
- `diagram`: `nodes: [{id,label,x:number,y:number}]`, `edges: [{from,to}]`.
- `tabs`: `tabs: [{id,label,children}]`; selection saved under the component ID.
- `select`: `label`, `stateKey`, `options: [{label,value:string}]`.
- `slider`: `label`, `stateKey`, `min`, `max`, optional positive `step`.
- `input`: `label`, `stateKey`, optional `inputType: "text" | "number"`.
- `form`: `label`, `children`, optional `actionId`.
- `button`: `label`, `actionId`.

Any component may have `visibleWhen: {key,equals: JSON scalar}`. Text interpolation supports only `{{key}}` scalar lookup, never expressions/eval. State keys and IDs cannot be prototype keys. Computation beyond selection/visibility belongs in a sandboxed app. Limits: 5 MB UTF-8 document, 256 components, nesting depth 16, 64 actions, 64 KiB JSON state, 2,000 chart points/table rows. State values must be finite JSON; initial values for bound controls must match their control types and ranges.

The runtime emits `brr/ui/argumentsDelta` with `threadId,turnId,itemId,callId,delta` for this reserved tool only, and terminal item events remain authoritative. Preview components are disabled until a completed dynamic tool call with `success:true` and a host result containing `documentHash`. No app code runs during previews. Failed/interrupted items display text, not executable UI.

The native command is `intelligent_ui_request({method,params})`. Methods:

- `document.read`: `{threadId,callId}` -> `{document,documentHash,task?:{id,agentId,projectId?,cwd?}}`; resolve successful runtime history, including paginated history.
- `state.read`: `{threadId,callId,documentHash}` -> `{state,revision}`; defaults when absent.
- `state.write`: same identity plus `{state,revision}` -> `{state,revision}` using compare-and-swap. Reset is an ordered state write of initial defaults. Never merge different document hashes.
- `action.prepare`: identity plus `{actionId}` -> `{reviewId,text,threadId,callId,documentHash,task?}` from the authoritative action and saved inputs.
- `action.claim`: `{reviewId}` -> the prepared review once; stale document/state revisions and repeat claims fail. Claim precedes one normal foreground user-turn submission; uncertain sends are not automatically replayed.
- `sandbox.create`: identity plus `{hostOrigin}` -> `{url,token}`. Only finalized authoritative app documents, fixed deny-network policy.
- `sandbox.destroy`: `{token}` -> null.

Custom apps receive `window.brrUI.getState()`, `setState(jsonObject)`, `requestAction(actionId)` and `resize(height)`. The isolated outer proxy authenticates its inner frame; Brr authenticates the outer frame source/origin and per-mount capability. Only these four operations exist. The initial saved state is installed before user code. State changes are serialized; action preparation waits for saves. Apps have no network, external resources, nested frames, navigation, forms, downloads, device or Tauri access. Inline and expanded presentations share state and run at most one app instance. Offscreen/inactive app instances are torn down.

Follow-ups use a Brr-owned review with Cancel and Send. Foreground Send uses the current chat's normal user-turn path and current collaboration mode without clearing the composer draft. It is disabled while running, offline, resuming, or awaiting questions/approvals. Task-result Send creates a regular foreground chat for the owning agent using the canonical project and normal workspace rules; it never sends into the task execution thread.
