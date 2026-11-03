import { invoke } from '@tauri-apps/api/core'

export type JsonScalar = string | number | boolean | null
export type JsonValue = JsonScalar | JsonValue[] | { [key: string]: JsonValue }
export type UiState = Record<string, JsonValue>
export type UiAction = { id: string; label: string; prompt: string }
export type UiVisibleWhen = { key: string; equals: JsonScalar }
export type UiComponent =
  | ({ id: string; type: 'text'; text: string } & Visibility)
  | ({ id: string; type: 'group' | 'form'; children: UiComponent[]; label?: string; actionId?: string } & Visibility)
  | ({ id: string; type: 'comparison'; columns: { label: string; text: string }[] } & Visibility)
  | ({ id: string; type: 'table'; columns: string[]; rows: (string | number | boolean | null)[][] } & Visibility)
  | ({ id: string; type: 'timeline'; entries: { label: string; text: string }[] } & Visibility)
  | ({ id: string; type: 'chart'; chartType: 'bar' | 'line'; points: { label: string; value: number }[] } & Visibility)
  | ({ id: string; type: 'diagram'; nodes: { id: string; label: string; x: number; y: number }[]; edges: { from: string; to: string }[] } & Visibility)
  | ({ id: string; type: 'tabs'; tabs: { id: string; label: string; children: UiComponent[] }[] } & Visibility)
  | ({ id: string; type: 'select'; label: string; stateKey: string; options: { label: string; value: string }[] } & Visibility)
  | ({ id: string; type: 'slider'; label: string; stateKey: string; min: number; max: number; step?: number } & Visibility)
  | ({ id: string; type: 'input'; label: string; stateKey: string; inputType?: 'text' | 'number' } & Visibility)
  | ({ id: string; type: 'button'; label: string; actionId: string } & Visibility)
type Visibility = { visibleWhen?: UiVisibleWhen }
export type UiDocument = { version: 1; kind: 'native' | 'app'; title: string; text: string; initialState: UiState; actions: UiAction[]; components?: UiComponent[]; html?: string }
export type UiIdentity = { threadId: string; callId: string; documentHash?: string }
export type UiActionReview = { reviewId: string; text: string; threadId: string; callId: string; documentHash: string; task?: { id: string; agentId: string; projectId?: string; cwd?: string } }
export type UiLoaded = { document: UiDocument; documentHash: string; state: UiState; revision: number; task?: UiActionReview['task'] }
export type UiValidation = { ok: true; document: UiDocument } | { ok: false; error: string }

const FORBIDDEN = new Set(['__proto__', 'prototype', 'constructor'])
const LIMITS = { bytes: 5 * 1024 * 1024, components: 256, depth: 16, actions: 64, stateBytes: 64 * 1024, rows: 2000, points: 2000 }
const object = (v: unknown): v is Record<string, any> => Boolean(v && typeof v === 'object' && !Array.isArray(v))
const nonempty = (v: unknown): v is string => typeof v === 'string' && Boolean(v.trim())
const onlyKeys = (v: Record<string, any>, allowed: string[]) => Object.keys(v).every(key => allowed.includes(key))
function jsonValue(value: unknown, depth = 0): value is JsonValue {
  if (depth > LIMITS.depth) return false
  if (value === null || typeof value === 'string' || typeof value === 'boolean') return true
  if (typeof value === 'number') return Number.isFinite(value)
  if (Array.isArray(value)) return value.every(v => jsonValue(v, depth + 1))
  if (!object(value)) return false
  return Object.keys(value).every(k => !FORBIDDEN.has(k) && jsonValue(value[k], depth + 1))
}
const keyOk = (v: unknown): v is string => nonempty(v) && !FORBIDDEN.has(v)
function validateComponentList(list: unknown, depth: number, ids: Set<string>, count: { value: number }): string | null {
  if (!Array.isArray(list)) return 'components must be an array'
  if (depth > LIMITS.depth) return 'component nesting exceeds 16 levels'
  for (const c of list) {
    count.value++
    if (count.value > LIMITS.components) return 'document exceeds 256 components'
    if (!object(c) || !keyOk(c.id) || ids.has(c.id) || !nonempty(c.type)) return 'component IDs must be unique nonempty strings and types must be valid'
    ids.add(c.id)
    const allowedFields: Record<string, string[]> = { text: ['id', 'type', 'text'], group: ['id', 'type', 'children'], comparison: ['id', 'type', 'columns'], table: ['id', 'type', 'columns', 'rows'], timeline: ['id', 'type', 'entries'], chart: ['id', 'type', 'chartType', 'points'], diagram: ['id', 'type', 'nodes', 'edges'], tabs: ['id', 'type', 'tabs'], select: ['id', 'type', 'label', 'stateKey', 'options'], slider: ['id', 'type', 'label', 'stateKey', 'min', 'max', 'step'], input: ['id', 'type', 'label', 'stateKey', 'inputType'], form: ['id', 'type', 'label', 'children', 'actionId'], button: ['id', 'type', 'label', 'actionId'] }
    const allowed = allowedFields[c.type]
    if (!allowed || !onlyKeys(c, [...allowed, 'visibleWhen'])) return `unsupported fields on component ${c.id}`
    if (c.visibleWhen !== undefined && (!object(c.visibleWhen) || !onlyKeys(c.visibleWhen, ['key', 'equals']) || !keyOk(c.visibleWhen.key) || !jsonScalar(c.visibleWhen.equals))) return `invalid visibility condition on ${c.id}`
    switch (c.type) {
      case 'text': if (typeof c.text !== 'string') return `text component ${c.id} needs text`; break
      case 'group': case 'form': {
        if (c.type === 'form' && (!nonempty(c.label) || (c.actionId !== undefined && !keyOk(c.actionId)))) return `invalid form ${c.id}`
        const error = validateComponentList(c.children, depth + 1, ids, count); if (error) return error; break
      }
      case 'comparison': if (!Array.isArray(c.columns) || !c.columns.length || c.columns.some((x: any) => !object(x) || !onlyKeys(x, ['label', 'text']) || !nonempty(x.label) || typeof x.text !== 'string')) return `invalid comparison ${c.id}`; break
      case 'table': { const validCell = (cell: any) => cell === null || ['string', 'boolean'].includes(typeof cell) || (typeof cell === 'number' && Number.isFinite(cell)); if (!Array.isArray(c.columns) || !c.columns.length || c.columns.some((x: any) => typeof x !== 'string') || !Array.isArray(c.rows) || c.rows.length > LIMITS.rows || c.rows.some((row: any) => !Array.isArray(row) || row.length !== c.columns.length || row.some((cell: any) => !validCell(cell)))) return `invalid table ${c.id}`; break }
      case 'timeline': if (!Array.isArray(c.entries) || c.entries.some((x: any) => !object(x) || !onlyKeys(x, ['label', 'text']) || !nonempty(x.label) || typeof x.text !== 'string')) return `invalid timeline ${c.id}`; break
      case 'chart': if (!['bar', 'line'].includes(c.chartType) || !Array.isArray(c.points) || c.points.length > LIMITS.points || c.points.some((x: any) => !object(x) || !onlyKeys(x, ['label', 'value']) || !nonempty(x.label) || typeof x.value !== 'number' || !Number.isFinite(x.value))) return `invalid chart ${c.id}`; break
      case 'diagram': if (!Array.isArray(c.nodes) || !Array.isArray(c.edges) || c.nodes.some((n: any) => !object(n) || !onlyKeys(n, ['id', 'label', 'x', 'y']) || !keyOk(n.id) || !nonempty(n.label) || !finite(n.x) || !finite(n.y)) || new Set(c.nodes.map((n: any) => n.id)).size !== c.nodes.length || c.edges.some((e: any) => !object(e) || !onlyKeys(e, ['from', 'to']) || !c.nodes.some((n: any) => n.id === e.from) || !c.nodes.some((n: any) => n.id === e.to))) return `invalid diagram ${c.id}`; break
      case 'tabs': if (!Array.isArray(c.tabs) || !c.tabs.length || c.tabs.some((t: any) => !object(t) || !onlyKeys(t, ['id', 'label', 'children']) || !keyOk(t.id) || !nonempty(t.label)) || new Set(c.tabs.map((t: any) => t.id)).size !== c.tabs.length) return `invalid tabs ${c.id}`; else for (const t of c.tabs) { const error = validateComponentList(t.children, depth + 1, ids, count); if (error) return error }; break
      case 'select': if (!nonempty(c.label) || !keyOk(c.stateKey) || !Array.isArray(c.options) || !c.options.length || c.options.some((o: any) => !object(o) || !onlyKeys(o, ['label', 'value']) || !nonempty(o.label) || typeof o.value !== 'string')) return `invalid select ${c.id}`; break
      case 'slider': if (!nonempty(c.label) || !keyOk(c.stateKey) || !finite(c.min) || !finite(c.max) || c.max <= c.min || (c.step !== undefined && (!finite(c.step) || c.step <= 0))) return `invalid slider ${c.id}`; break
      case 'input': if (!nonempty(c.label) || !keyOk(c.stateKey) || (c.inputType !== undefined && !['text', 'number'].includes(c.inputType))) return `invalid input ${c.id}`; break
      case 'button': if (!nonempty(c.label) || !keyOk(c.actionId)) return `invalid button ${c.id}`; break
      default: return `unsupported component type on ${c.id}`
    }
  }
  return null
}
const finite = (v: unknown): v is number => typeof v === 'number' && Number.isFinite(v)
const jsonScalar = (v: unknown): v is JsonScalar => v === null || typeof v === 'string' || typeof v === 'boolean' || finite(v)
export function validateUiDocument(input: unknown): UiValidation {
  if (!object(input)) return { ok: false, error: 'document must be an object' }
  let bytes: number
  try { bytes = new TextEncoder().encode(JSON.stringify(input)).length } catch { return { ok: false, error: 'document is not valid JSON' } }
  if (bytes > LIMITS.bytes) return { ok: false, error: 'document exceeds 5 MB' }
  if (!onlyKeys(input, input.kind === 'native' ? ['version', 'kind', 'title', 'text', 'initialState', 'actions', 'components'] : ['version', 'kind', 'title', 'text', 'initialState', 'actions', 'html']) || input.version !== 1 || !['native', 'app'].includes(input.kind) || !nonempty(input.title) || !nonempty(input.text) || !object(input.initialState) || !Array.isArray(input.actions) || input.actions.length > LIMITS.actions) return { ok: false, error: 'invalid document header' }
  if (!jsonValue(input.initialState) || new TextEncoder().encode(JSON.stringify(input.initialState)).length > LIMITS.stateBytes) return { ok: false, error: 'invalid or oversized initial state' }
  if (input.actions.some((a: any) => !object(a) || !onlyKeys(a, ['id', 'label', 'prompt']) || !keyOk(a.id) || !nonempty(a.label) || typeof a.prompt !== 'string') || new Set(input.actions.map((a: any) => a.id)).size !== input.actions.length) return { ok: false, error: 'actions must have unique IDs, labels and prompt text' }
  if (input.kind === 'native') {
    if (input.html !== undefined || !Array.isArray(input.components)) return { ok: false, error: 'native documents require components' }
    const error = validateComponentList(input.components, 1, new Set(), { value: 0 }); if (error) return { ok: false, error }
  } else if (!nonempty(input.html) || input.components !== undefined) return { ok: false, error: 'app documents require self-contained html' }
  if (input.kind === 'native') {
    const actionIds = new Set(input.actions.map((action: UiAction) => action.id))
    const refsValid = (components: UiComponent[]): boolean => components.every(component => {
      if ((component.type === 'button' || component.type === 'form') && component.actionId && !actionIds.has(component.actionId)) return false
      if ((component.type === 'group' || component.type === 'form') && !refsValid(component.children)) return false
      return component.type !== 'tabs' || component.tabs.every(tab => refsValid(tab.children))
    })
    if (!refsValid(input.components)) return { ok: false, error: 'components reference an unknown action' }
  }
  const document = input as UiDocument
  if (!validateUiState(document, document.initialState)) return { ok: false, error: 'initial state does not match its controls' }
  return { ok: true, document }
}
export function validateUiState(document: UiDocument, state: unknown): state is UiState {
  if (!object(state) || !jsonValue(state) || new TextEncoder().encode(JSON.stringify(state)).length > LIMITS.stateBytes) return false
  if (document.kind !== 'native') return true
  let valid = true
  const values = state as UiState
  const check = (list: UiComponent[]) => list.forEach(c => {
    const key = c.type === 'tabs' ? c.id : ('stateKey' in c ? c.stateKey : null)
    if (key && c.type !== 'tabs' && !(key in state)) valid = false
    if (key && key in state) {
      const value = values[key]
      if (c.type === 'select' && (typeof value !== 'string' || !c.options.some(option => option.value === value))) valid = false
      else if (c.type === 'slider' && (typeof value !== 'number' || value < c.min || value > c.max)) valid = false
      else if (c.type === 'input' && typeof value !== (c.inputType === 'number' ? 'number' : 'string')) valid = false
      else if (c.type === 'tabs' && (typeof value !== 'string' || !c.tabs.some(tab => tab.id === value))) valid = false
    }
    if (c.type === 'group' || c.type === 'form') check(c.children)
    if (c.type === 'tabs') c.tabs.forEach(tab => check(tab.children))
  })
  check(document.components ?? [])
  return valid
}
export function interpolateUiText(value: string, state: UiState): string {
  return value.replace(/\{\{([^{}]+)\}\}/g, (match, key: string) => keyOk(key) && key in state && jsonScalar(state[key]) ? String(state[key]) : match)
}

const rpc = <T,>(method: string, params: Record<string, unknown>) => invoke<T>('intelligent_ui_request', { method, params })
type Listener = () => void
class UiController {
  private loaded: UiLoaded | null = null
  private published: UiLoaded | null = null
  private loading: Promise<UiLoaded> | null = null
  private listeners = new Set<Listener>()
  private writeQueue: Promise<void> = Promise.resolve()
  private state: UiState = {}
  private revision = 0
  private lastWriteError: unknown = null
  private desiredState: UiState | null = null
  private activeAppOwner: symbol | null = null
  constructor(readonly identity: UiIdentity) {}
  snapshot = () => this.published
  subscribe = (fn: Listener) => { this.listeners.add(fn); return () => this.listeners.delete(fn) }
  private emit() { this.published = this.loaded ? { ...this.loaded, state: this.desiredState ?? this.state, revision: this.revision } : null; this.listeners.forEach(fn => fn()) }
  async load(): Promise<UiLoaded> {
    if (this.loaded) return this.snapshot()!
    if (!this.loading) this.loading = (async () => {
      const result = await rpc<{ document: unknown; documentHash: string; task?: UiLoaded['task'] }>('document.read', { threadId: this.identity.threadId, callId: this.identity.callId })
      if (this.identity.documentHash && this.identity.documentHash !== result.documentHash) throw new Error('The UI document changed. Reload the conversation before using it.')
      const valid = validateUiDocument(result.document)
      if (!valid.ok) throw new Error(valid.error)
      const saved = await rpc<{ state: unknown; revision: number }>('state.read', { threadId: this.identity.threadId, callId: this.identity.callId, documentHash: result.documentHash })
      const state = validateUiState(valid.document, saved.state) ? saved.state : valid.document.initialState
      if (!Number.isInteger(saved.revision) || saved.revision < 0) throw new Error('The UI state revision is invalid.')
      this.state = state; this.revision = saved.revision
      this.loaded = { document: valid.document, documentHash: result.documentHash, state, revision: saved.revision, task: result.task }
      this.emit(); return this.snapshot()!
    })().catch(error => { this.loading = null; throw error })
    return this.loading
  }
  private writeState(resolveNext: (current: UiState) => UiState): Promise<void> {
    let requestedState: UiState | null = null
    if (this.loaded) {
      requestedState = resolveNext(this.desiredState ?? this.state)
      if (!validateUiState(this.loaded.document, requestedState)) return Promise.reject(new Error('UI state must match its controls and contain finite JSON under 64 KiB.'))
      this.desiredState = requestedState
      this.emit()
    }
    const operation = this.writeQueue.then(async () => {
      const loaded = await this.load()
      const next = requestedState ?? resolveNext(this.desiredState ?? this.state)
      if (!validateUiState(loaded.document, next)) throw new Error('UI state must match its controls and contain finite JSON under 64 KiB.')
      if (requestedState === null) { requestedState = next; this.desiredState = next; this.emit() }
      const response = await rpc<{ state: unknown; revision: number }>('state.write', { threadId: this.identity.threadId, callId: this.identity.callId, documentHash: loaded.documentHash, state: next, revision: this.revision })
      if (!validateUiState(loaded.document, response.state) || !Number.isInteger(response.revision) || response.revision <= this.revision) throw new Error('The UI state write returned an invalid revision.')
      this.state = response.state; this.revision = response.revision; this.loaded = { ...loaded, state: this.state, revision: this.revision }
      if (JSON.stringify(this.desiredState) === JSON.stringify(next)) this.desiredState = null
      this.lastWriteError = null; this.emit()
    }).catch(error => { this.lastWriteError = error; this.emit(); throw error })
    this.writeQueue = operation.catch(() => undefined)
    return operation
  }
  setState(next: UiState) { return this.writeState(() => next) }
  setValue(key: string, value: JsonValue) { if (!keyOk(key)) return Promise.reject(new Error('Invalid state key.')); return this.writeState(current => ({ ...current, [key]: value })) }
  reset() { return this.writeState(() => this.loaded!.document.initialState) }
  async prepareAction(actionId: string): Promise<UiActionReview> {
    let settledQueue: Promise<void>
    do { settledQueue = this.writeQueue; await settledQueue } while (settledQueue !== this.writeQueue)
    if (this.lastWriteError) throw new Error('Save the UI inputs before requesting this action.')
    const loaded = await this.load()
    if (!loaded.document.actions.some(action => action.id === actionId)) throw new Error('This action is not available in the UI document.')
    return rpc<UiActionReview>('action.prepare', { threadId: this.identity.threadId, callId: this.identity.callId, documentHash: loaded.documentHash, actionId })
  }
  claimApp(owner: symbol) { if (this.activeAppOwner && this.activeAppOwner !== owner) return false; this.activeAppOwner = owner; return true }
  releaseApp(owner: symbol) { if (this.activeAppOwner === owner) this.activeAppOwner = null }
}
const controllers = new Map<string, UiController>()
export function intelligentUiClient(identity: UiIdentity): UiController {
  const key = `${identity.threadId}\u0000${identity.callId}\u0000${identity.documentHash ?? ''}`
  let controller = controllers.get(key)
  if (!controller) { controller = new UiController(identity); controllers.set(key, controller) }
  const loaded = controller.snapshot()
  if (identity.documentHash && loaded && loaded.documentHash !== identity.documentHash) throw new Error('This UI identity refers to a different document.')
  return controller
}
export function clearIntelligentUiControllersForTests() { controllers.clear() }

export function parseUiDocument(input: unknown): UiDocument {
  const result = validateUiDocument(input)
  if (!result.ok) throw new Error(result.error)
  return result.document
}
