import { act, renderHook, waitFor } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { emptyDraft } from './lib/chat-draft'
import { useChatDrafts } from './lib/use-chat-drafts'

const mockChat = vi.hoisted(() => ({
  records: [] as Array<any>,
  listDrafts: vi.fn(async () => mockChat.records),
  getDraft: vi.fn(async (id: string) => mockChat.records.find(record => record.id === id)),
  saveDraft: vi.fn(async (draft: any) => {
    const saved = { ...draft, revision: (mockChat.records.find(item => item.id === draft.id)?.revision ?? 0) + 1, updatedAt: '2026-11-02T00:00:00Z' }
    mockChat.records = [...mockChat.records.filter(item => item.id !== draft.id), saved]
    return saved
  }),
  promoteDraft: vi.fn(async (id: string, threadId: string) => {
    const record = mockChat.records.find(item => item.id === id)
    const promoted = {...record, id: threadId, threadId}
    mockChat.records = [...mockChat.records.filter(item => item.id !== id), promoted]
    return promoted
  }),
}))
vi.mock('./lib/chat', () => ({ chat: mockChat }))

describe('chat experience drafts', () => {
  beforeEach(() => { mockChat.records = []; vi.clearAllMocks() })

  it('retries the initial durable draft load after the desktop runtime is temporarily unavailable', async () => {
    mockChat.records = [{ id: 'restored-draft', threadId: null, projectId: null, agentId: null, document: emptyDraft('Persisted after restart'), settings: { model: 'gpt-5.6', effort: 'low', mode: 'plan' }, revision: 3, updatedAt: '2026-11-03T00:00:00Z' }]
    mockChat.listDrafts.mockRejectedValueOnce(new Error('Runtime is reconnecting'))

    const { result } = renderHook(() => useChatDrafts(true))

    await waitFor(() => expect(result.current.records.map(draft => draft.id)).toEqual(['restored-draft']))
    expect(mockChat.listDrafts).toHaveBeenCalledTimes(2)
    expect(result.current.records[0].document.text).toBe('Persisted after restart')
    expect(result.current.records[0].settings).toMatchObject({ model: 'gpt-5.6', effort: 'low', mode: 'plan' })
    expect(result.current.saveError).toBe('')
  })

  it('keeps separately keyed draft documents and restores the selected project and identity context', async () => {
    const { result } = renderHook(() => useChatDrafts(true))
    await waitFor(() => expect(mockChat.listDrafts).toHaveBeenCalled())
    const firstId = result.current.key
    act(() => {
      result.current.context.current = {
        threadId: null,
        projectId: '/project-a',
        agentId: 'agent-a',
        settings: { model: 'sol-a', modelId: 'model-a', effort: 'high', mode: 'plan', readOnly: true },
      }
      result.current.setDocument(emptyDraft('Draft A'))
    })
    await waitFor(() => expect(mockChat.records.find(record => record.id === firstId)?.document.text).toBe('Draft A'))

    let secondId = ''
    act(() => { secondId = result.current.newDraft() })
    expect(secondId).not.toBe(firstId)
    expect(result.current.document.text).toBe('')
    act(() => {
      result.current.context.current = {
        threadId: null,
        projectId: '/project-b',
        agentId: 'agent-b',
        settings: { model: 'sol-b', modelId: 'model-b', effort: 'medium', mode: 'default' },
      }
      result.current.setDocument(emptyDraft('Draft B'))
    })
    await waitFor(() => expect(mockChat.records.find(record => record.id === secondId)?.document.text).toBe('Draft B'))

    let opened: Awaited<ReturnType<typeof result.current.switchDraft>>
    await act(async () => { opened = await result.current.switchDraft(firstId, null) })
    expect(result.current.document.text).toBe('Draft A')
    expect(opened?.projectId).toBe('/project-a')
    expect(opened?.agentId).toBe('agent-a')
  })

  it('persists selected attachment ids and settings without changing draft text', async () => {
    const { result } = renderHook(() => useChatDrafts(true))
    await waitFor(() => expect(mockChat.listDrafts).toHaveBeenCalled())
    const id = result.current.key
    act(() => {
      result.current.setDocument({ ...emptyDraft(''), attachmentIds: ['grant-a'] })
      result.current.updateContext({ settings: { model: 'sol', modelId: 'sol-id', effort: 'high', mode: 'plan', readOnly: true } })
    })
    await waitFor(() => expect(mockChat.records.find(record => record.id === id)?.document.attachmentIds).toEqual(['grant-a']))
    expect(mockChat.records.find(record => record.id === id)?.settings).toMatchObject({ modelId: 'sol-id', effort: 'high', mode: 'plan', readOnly: true })
    act(() => { result.current.newDraft() })
    expect(result.current.document.attachmentIds).toEqual([])
  })

  it('keeps an archived edit branch hidden from failed and delayed stale draft refreshes', async () => {
    const branchDraft = { id: 'archived-branch', threadId: 'archived-branch', projectId: 'project-a', agentId: 'agent-a',
      document: emptyDraft('Archived edit request'), settings: { model: 'sol-a', effort: 'high', mode: 'plan' as const },
      revision: 2, updatedAt: '2026-11-03T00:00:00Z', branchEdit: true as const,
      branchOperationId: 'branch-operation-a', branchThread: { id: 'archived-branch', cwd: '/branch' } }
    mockChat.records = [branchDraft]
    let finishInitialLoad!: (records: any[]) => void
    mockChat.listDrafts.mockImplementationOnce(() => new Promise(resolve => { finishInitialLoad = resolve }) as never)

    const { result } = renderHook(() => useChatDrafts(true))
    await waitFor(() => expect(mockChat.listDrafts).toHaveBeenCalledTimes(1))
    act(() => result.current.hideArchivedBranch('archived-branch'))
    await act(async () => { finishInitialLoad([branchDraft]) })

    expect(result.current.records).toEqual([])
    mockChat.listDrafts.mockResolvedValueOnce([branchDraft])
    await act(async () => { await result.current.refresh() })
    expect(result.current.records).toEqual([])
  })

  it('persists a project context change immediately and promotes the submitted draft by its captured key', async () => {
    const { result } = renderHook(() => useChatDrafts(true))
    await waitFor(() => expect(mockChat.listDrafts).toHaveBeenCalled())
    const submittedId = result.current.key
    act(() => {
      result.current.setDocument(emptyDraft('Submitted text'))
      result.current.updateContext({projectId: 'project-stable-id', settings: {model: 'sol', effort: 'medium', mode: 'plan'}})
    })
    await waitFor(() => expect(mockChat.records.find(record => record.id === submittedId)?.projectId).toBe('project-stable-id'))
    const submittedContext = {...result.current.context.current}
    const submittedDocument = result.current.document
    act(() => { result.current.newDraft() })

    await act(async () => {
      await result.current.promote('created-thread', submittedId, {
        context: submittedContext,
        document: submittedDocument,
        shouldAdopt: () => false,
      })
    })

    expect(mockChat.promoteDraft).toHaveBeenCalledWith(submittedId, 'created-thread')
    expect(result.current.key).not.toBe('created-thread')
    expect(mockChat.records.find(record => record.id === 'created-thread')?.document.text).toBe('Submitted text')
  })

  it('starts a new draft without the previous chat or named agent association', async () => {
    const { result } = renderHook(() => useChatDrafts(true))
    await waitFor(() => expect(mockChat.listDrafts).toHaveBeenCalled())
    act(() => {
      result.current.context.current = { threadId: 'old-chat', agentId: 'old-agent', projectId: 'project-a',
        settings: { model: 'sol', effort: 'medium', mode: 'default' } }
      result.current.newDraft()
      result.current.updateContext({ projectId: 'project-b' })
    })
    const newId = result.current.key
    await waitFor(() => expect(mockChat.records.find(record => record.id === newId)?.projectId).toBe('project-b'))
    expect(mockChat.records.find(record => record.id === newId)?.threadId).toBeNull()
    expect(mockChat.records.find(record => record.id === newId)?.agentId).toBeNull()
  })

  it('does not adopt an asynchronously promoted draft after the user has switched drafts', async () => {
    const { result } = renderHook(() => useChatDrafts(true))
    await waitFor(() => expect(mockChat.listDrafts).toHaveBeenCalled())
    const origin = result.current.key
    const context = { threadId: null, projectId: 'project-a', agentId: '', settings: { model: 'sol', effort: 'medium', mode: 'default' as const } }
    const document = emptyDraft('Submit from this draft')
    act(() => {
      result.current.context.current = context
      result.current.setDocument(document)
    })
    await waitFor(() => expect(mockChat.records.find(record => record.id === origin)?.document.text).toBe(document.text))
    let finishPromotion!: (value: any) => void
    mockChat.promoteDraft.mockImplementationOnce(() => new Promise(resolve => { finishPromotion = resolve }) as never)
    let promoting!: Promise<void>
    act(() => { promoting = result.current.promote('origin-thread', origin, { context, document, shouldAdopt: () => result.current.key === origin }) })
    await waitFor(() => expect(mockChat.promoteDraft).toHaveBeenCalledWith(origin, 'origin-thread'))
    let nextDraft = ''
    act(() => { nextDraft = result.current.newDraft() })
    await act(async () => {
      const promoted = { ...mockChat.records.find(record => record.id === origin), id: 'origin-thread', threadId: 'origin-thread' }
      mockChat.records = [...mockChat.records.filter(record => record.id !== origin), promoted]
      finishPromotion(promoted)
      await promoting
    })
    expect(result.current.key).toBe(nextDraft)
    expect(result.current.document.text).toBe('')
    expect(mockChat.records.find(record => record.id === 'origin-thread')?.document.text).toBe(document.text)
  })
})
