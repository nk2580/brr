import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { open } from '@tauri-apps/plugin-dialog'
import type { RpcEvent } from './session'

export interface Account { type: 'chatgpt'; email: string | null; planType: string }
export interface Model { id: string; model: string; displayName: string; isDefault: boolean; supportedReasoningEfforts: {reasoningEffort: string; description: string}[]; defaultReasoningEffort: string }
export interface Turn { id: string; status?: string; items?: unknown[] }
export interface Worktree { projectPath: string; path: string; branch: string; cwd: string }
export interface Thread { id: string; name?: string | null; preview: string; cwd: string; updatedAt: number; worktree?: Worktree; historyMode?: 'legacy' | 'paginated'; collaborationMode?: { mode?: string } | null; turns?: Turn[] }
export interface Connection { account: Account | null; models: Model[]; threads: Thread[] }

export const isDesktop = () => '__TAURI_INTERNALS__' in window
const rpc = <T = unknown>(method: string, params: Record<string, unknown> = {}) => invoke<T>('engine_request', { method, params })

export const client = {
  subscribe: (handler: (event: RpcEvent) => void) => listen<RpcEvent>('engine-event', e => handler(e.payload)),
  async connect(): Promise<Connection> {
    await invoke('engine_connect')
    const auth = await rpc<{account: Account | {type: string} | null}>('account/read', { refreshToken: false })
    const [models, threads] = await Promise.all([
      rpc<{data: Model[]}>('model/list', {includeHidden: false}),
      this.threads(),
    ])
    return {account: auth.account?.type === 'chatgpt' ? auth.account as Account : null, models: models.data, threads}
  },
  async account() {
    const result = await rpc<{account: Account | null}>('account/read', {refreshToken: false})
    return result.account?.type === 'chatgpt' ? result.account : null
  },
  login: () => invoke<{loginId: string}>('chatgpt_login'),
  cancelLogin: (loginId: string) => rpc('account/login/cancel', {loginId}),
  logout: () => rpc('account/logout'),
  async threads(): Promise<Thread[]> {
    const all: Thread[] = []
    let cursor: string | undefined
    do {
      const page = await rpc<{data: Thread[]; nextCursor: string | null}>('thread/list', {limit: 100, ...(cursor ? {cursor} : {}), sortKey: 'updated_at'})
      all.push(...page.data)
      cursor = page.nextCursor ?? undefined
    } while (cursor)
    return all
  },
  chooseWorkspace: () => open({directory: true, multiple: false, title: 'Choose a project folder'}),
  start: (cwd: string, model: string, readOnly: boolean) => rpc<{thread: Thread}>('thread/start', {cwd, model, modelProvider: 'openai', approvalPolicy: 'on-request', sandbox: readOnly ? 'read-only' : 'workspace-write'}),
  async resume(threadId: string, readOnly?: boolean): Promise<{thread: Thread}> {
    const response = await rpc<{thread: Thread; collaborationMode?: Thread['collaborationMode']}>('thread/resume', {threadId, modelProvider: 'openai', approvalPolicy: 'on-request', ...(readOnly === undefined ? {} : {sandbox: readOnly ? 'read-only' : 'workspace-write'})})
    const thread = {...response.thread, collaborationMode: response.collaborationMode ?? response.thread.collaborationMode}
    if (thread.historyMode !== 'paginated') return {thread}

    const turns: Turn[] = []
    let cursor: string | undefined
    do {
      const page = await rpc<{data: Turn[]; nextCursor: string | null}>('thread/turns/list', {
        threadId: thread.id,
        limit: 100,
        sortDirection: 'asc',
        itemsView: 'full',
        ...(cursor ? {cursor} : {}),
      })
      turns.push(...page.data)
      cursor = page.nextCursor ?? undefined
    } while (cursor)

    return {thread: {...thread, turns}}
  },
  send: (threadId: string, text: string, model: string, effort: string, mode: 'default' | 'plan' = 'default') => rpc<{turn: {id: string}}>('turn/start', {
    threadId,
    input: [{type: 'text', text, text_elements: []}],
    model,
    effort,
    collaborationMode: {mode, settings: {model, reasoning_effort: effort, developer_instructions: null}},
  }),
  steer: (threadId: string, text: string, expectedTurnId: string) => rpc<{turnId: string}>('turn/steer', {
    threadId,
    input: [{type: 'text', text, text_elements: []}],
    expectedTurnId,
  }),
  interrupt: (threadId: string, turnId: string) => rpc('turn/interrupt', {threadId, turnId}),
  respond: (id: string | number, result: Record<string, unknown>) => invoke('engine_respond', {id, result}),
  archive: (threadId: string) => rpc('thread/archive', {threadId}),
  rename: (threadId: string, name: string) => rpc('thread/name/set', {threadId, name}),
}
