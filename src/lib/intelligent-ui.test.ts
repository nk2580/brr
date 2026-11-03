import { beforeEach, describe, expect, it, vi } from 'vitest'
import { invoke } from '@tauri-apps/api/core'
import { validateUiDocument, interpolateUiText, intelligentUiClient, validateUiState } from './intelligent-ui'
import type { UiDocument } from './intelligent-ui'
import cases from '../../fixtures/intelligent-ui-documents.json'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))
const invokeMock = vi.mocked(invoke)
const doc: UiDocument = { version: 1, kind: 'native', title: 'Budget', text: 'Total: {{total}}', initialState: { total: 5 }, actions: [], components: [{ id: 'total', type: 'input', label: 'Total', stateKey: 'total', inputType: 'number' }] }

describe('intelligent UI contract', () => {
  it('matches the shared native and app contract cases', () => {
    for (const item of cases) expect(validateUiDocument(item.document).ok, item.name).toBe(item.valid)
  })
  it('accepts a valid native document and interpolates scalar values only', () => {
    expect(validateUiDocument(doc)).toEqual({ ok: true, document: doc })
    expect(interpolateUiText('Total {{total}}; {{missing}}', { total: 12, missing: { x: 1 } })).toBe('Total 12; {{missing}}')
  })
  it('rejects prototype keys, invalid control defaults, and excessive depth', () => {
    expect(validateUiDocument({ ...doc, initialState: JSON.parse('{"__proto__":{"polluted":true}}') }).ok).toBe(false)
    expect(validateUiDocument({ ...doc, initialState: { total: 'five' } }).ok).toBe(false)
    expect(validateUiDocument({ ...doc, initialState: {} }).ok).toBe(false)
    expect(validateUiState(doc, {})).toBe(false)
    const tabs: UiDocument = { ...doc, initialState: { total: 3 }, components: [{ id: 'total', type: 'input', label: 'Total', stateKey: 'total', inputType: 'number' }, { id: 'tabs', type: 'tabs', tabs: [{ id: 'overview', label: 'Overview', children: [] }] }] }
    expect(validateUiDocument(tabs).ok).toBe(true)
    expect(validateUiDocument({ ...doc, threadId: 'model-controlled' }).ok).toBe(false)
    let children: any[] = [{ id: 'leaf', type: 'text', text: 'x' }]
    for (let i = 0; i < 17; i++) children = [{ id: `g${i}`, type: 'group', children }]
    expect(validateUiDocument({ ...doc, components: children }).ok).toBe(false)
  })
})

describe('intelligent UI client', () => {
  beforeEach(() => { invokeMock.mockReset() })
  it('reads authoritative document and state, then serializes writes with revisions', async () => {
    invokeMock.mockImplementation(async (_cmd, args: any) => {
      const { method, params } = args
      if (method === 'document.read') return { document: doc, documentHash: 'hash-1' } as never
      if (method === 'state.read') return { state: { total: 5 }, revision: 3 } as never
      if (method === 'state.write') return { state: params.state, revision: params.revision + 1 } as never
      throw new Error(method)
    })
    const controller = intelligentUiClient({ threadId: 'thread', callId: 'call' })
    const loaded = await controller.load()
    expect(loaded.state).toEqual({ total: 5 })
    await Promise.all([controller.setValue('total', 6), controller.setValue('tab', 'details')])
    expect(invokeMock).toHaveBeenCalledWith('intelligent_ui_request', expect.objectContaining({ method: 'state.write', params: expect.objectContaining({ revision: 3, state: { total: 6 } }) }))
  })
  it('orders reset immediately between same-turn state edits and scopes cache by document hash', async () => {
    const writes: { revision: number; state: any }[] = []
    invokeMock.mockImplementation(async (_cmd, args: any) => {
      const { method, params } = args
      if (method === 'document.read') return { document: doc, documentHash: 'hash-1' } as never
      if (method === 'state.read') return { state: { total: 5 }, revision: 0 } as never
      if (method === 'state.write') { writes.push({ revision: params.revision, state: params.state }); return { state: params.state, revision: params.revision + 1 } as never }
      throw new Error(method)
    })
    const controller = intelligentUiClient({ threadId: 'ordered', callId: 'call', documentHash: 'hash-1' })
    await controller.load()
    await Promise.all([controller.setValue('total', 8), controller.reset(), controller.setValue('total', 9)])
    expect(writes.map(write => write.state)).toEqual([{ total: 8 }, { total: 5 }, { total: 9 }])
    const beforeLoad = intelligentUiClient({ threadId: 'reset-before-load', callId: 'call', documentHash: 'hash-1' })
    const reset = beforeLoad.reset()
    const edit = beforeLoad.setValue('total', 11)
    await Promise.all([reset, edit])
    expect(writes.slice(-2).map(write => write.state)).toEqual([{ total: 5 }, { total: 11 }])
    expect(intelligentUiClient({ threadId: 'ordered', callId: 'call', documentHash: 'hash-2' })).not.toBe(controller)
    await expect(intelligentUiClient({ threadId: 'ordered', callId: 'call', documentHash: 'hash-2' }).load()).rejects.toThrow(/changed/)
  })
})
