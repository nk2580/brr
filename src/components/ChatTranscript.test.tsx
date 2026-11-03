import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'
import { startTransition } from 'react'
import { ChatTranscript } from './ChatTranscript'
import { initialSession, type AgentItem } from '../lib/session'

function renderTranscript(items: AgentItem[], overrides: Partial<React.ComponentProps<typeof ChatTranscript>> = {}) {
  const session = { ...initialSession('thread-1'), turnId: 'turn-2', items }
  return render(<ChatTranscript
    session={session}
    threadId="thread-1"
    renderItem={item => <div data-testid={`rendered-${item.id}`}>{String(item.text ?? item.type)}</div>}
    onQuote={vi.fn()}
    onReuse={vi.fn()}
    onEdit={vi.fn()}
    onBranch={vi.fn()}
    {...overrides}
  />)
}

describe('ChatTranscript', () => {
  it('keeps completed work disclosure state when React defers the toggle update', async () => {
    renderTranscript([{ id:'r-toggle', type:'reasoning', text:'Finished reasoning', turnId:'turn-1', turnStatus:'completed' }])
    const summary = screen.getByText(/Completed work/)
    const details = summary.closest('details')!
    expect(details).not.toHaveAttribute('open')
    startTransition(() => { fireEvent.click(summary) })
    await waitFor(() => expect(details).toHaveAttribute('open'))
    expect(screen.getByTestId('rendered-r-toggle')).toBeVisible()
  })

  it('keeps completed work collapsed while keeping interactive responses mounted and visible', () => {
    renderTranscript([
      { id: 'r1', type: 'reasoning', text: 'thought', turnId: 'turn-1', turnStatus: 'completed' },
      { id: 'ui1', type: 'dynamicToolCall', tool: 'brr_ui_render', text: 'interactive', turnId: 'turn-1', turnStatus: 'completed' },
      { id: 'mcp1', type: 'mcpToolCall', server: 'calendar', tool: 'today', mcpAppUi: { resourceUri: 'ui://tool' }, turnId: 'turn-1', turnStatus: 'completed' },
    ])
    expect(screen.getByText(/Completed work/)).toBeInTheDocument()
    expect(screen.getByTestId('rendered-r1')).not.toBeVisible()
    expect(screen.getByTestId('rendered-ui1')).toBeVisible()
    expect(screen.getByTestId('rendered-mcp1')).toBeVisible()
  })

  it('offers message actions and branches from the selected assistant turn', () => {
    const onQuote = vi.fn()
    const onReuse = vi.fn()
    const onBranch = vi.fn()
    renderTranscript([
      { id: 'u1', type: 'userMessage', text: 'Make a small change', turnId: 'turn-1' },
      { id: 'a1', type: 'agentMessage', text: 'Done', turnId: 'turn-1', turnStatus: 'completed' },
      { id: 'u2', type: 'userMessage', text: 'Follow up', turnId: 'turn-2' },
    ], { onQuote, onReuse, onBranch })
    fireEvent.click(screen.getAllByRole('button', { name: 'Quote' })[0])
    fireEvent.click(screen.getAllByRole('button', { name: 'Reuse' })[0])
    fireEvent.click(screen.getByRole('button', { name: 'Branch from this turn' }))
    expect(onQuote).toHaveBeenCalledWith('Make a small change')
    expect(onReuse).toHaveBeenCalledWith('Make a small change')
    expect(onBranch).toHaveBeenCalledWith('turn-1')
    expect(screen.getByRole('navigation', { name: 'Conversation turns' })).toBeInTheDocument()
    expect(screen.getByText('Turn 2 of 2')).toBeInTheDocument()
  })

  it('keeps the turn navigator on the latest turn when the session is idle', () => {
    renderTranscript([
      { id: 'a1', type: 'agentMessage', text: 'First', turnId: 'turn-1' },
      { id: 'a2', type: 'agentMessage', text: 'Second', turnId: 'turn-2' },
    ], { session: { ...initialSession('thread-1'), items: [
      { id: 'a1', type: 'agentMessage', text: 'First', turnId: 'turn-1' },
      { id: 'a2', type: 'agentMessage', text: 'Second', turnId: 'turn-2' },
    ] } })
    expect(screen.getByText('Turn 2 of 2')).toBeInTheDocument()
  })

  it('offers Edit on completed turn openings, including the latest turn when idle', () => {
    renderTranscript([
      { id: 'u1', type: 'userMessage', text: 'Opening', turnId: 'turn-1', turnStatus: 'completed' },
      { id: 'u2', type: 'userMessage', text: 'Follow up', turnId: 'turn-1', turnStatus: 'completed' },
      { id: 'u3', type: 'userMessage', text: 'Latest opening', turnId: 'turn-2', turnStatus: 'completed' },
    ])
    const editButtons = screen.getAllByRole('button', { name: 'Edit' })
    expect(editButtons).toHaveLength(2)
    expect(editButtons.map(button => button.closest('[data-transcript-item]')?.getAttribute('data-transcript-item'))).toEqual(['u1', 'u3'])
  })

  it('does not offer Edit for the opening message of the currently running turn', () => {
    const items: AgentItem[] = [
      { id:'u1', type:'userMessage', text:'Completed opening', turnId:'turn-1', turnStatus:'completed' },
      { id:'u2', type:'userMessage', text:'Running opening', turnId:'turn-2', turnStatus:'inProgress' },
    ]
    renderTranscript(items, { session:{ ...initialSession('thread-1'), turnId:'turn-2', status:'running', items } })
    const edits = screen.getAllByRole('button', {name:'Edit'})
    expect(edits).toHaveLength(1)
    expect(edits[0].closest('[data-transcript-item]')).toHaveAttribute('data-transcript-item', 'u1')
  })

  it.each([
    ['interrupted', 'Stopped work'],
    ['failed', 'Failed work'],
    ['waiting', 'Waiting for input'],
  ])('shows %s work status and hides branch actions for a non-completed turn', (turnStatus, label) => {
    renderTranscript([
      { id: 'r1', type: 'reasoning', text: 'Work', turnId: 'turn-1', turnStatus },
      { id: 'a1', type: 'agentMessage', text: 'Stopped response', turnId: 'turn-1', turnStatus },
    ])
    expect(screen.getByText(new RegExp(label))).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Branch from this turn' })).not.toBeInTheDocument()
  })

  it('lets stopped work collapse after it becomes terminal', async () => {
    renderTranscript([{ id: 'r1', type: 'reasoning', text: 'Stopped reasoning', turnId: 'turn-1', turnStatus: 'interrupted' }])
    const summary = screen.getByText(/Stopped work/)
    const details = summary.closest('details')!
    expect(details).not.toHaveAttribute('open')
    fireEvent.click(summary)
    await waitFor(() => expect(details).toHaveAttribute('open'))
    expect(screen.getByTestId('rendered-r1')).toBeVisible()
  })

  it('shows branch action only for a completed turn boundary', () => {
    renderTranscript([{ id: 'a1', type: 'agentMessage', text: 'Done', turnId: 'turn-1', turnStatus: 'completed' }])
    expect(screen.getByRole('button', { name: 'Branch from this turn' })).toBeInTheDocument()
  })

  it('explains that edit and branch actions use the current files', () => {
    renderTranscript([
      { id: 'u1', type: 'userMessage', text: 'Opening request', turnId: 'turn-1', turnStatus: 'completed' },
      { id: 'a1', type: 'agentMessage', text: 'Done', turnId: 'turn-1', turnStatus: 'completed' },
    ])
    expect(screen.getByRole('button', {name:'Branch from this turn'})).toBeInTheDocument()
    expect(screen.getByText('Uses current files')).toBeInTheDocument()
    expect(screen.getByRole('button', {name:'Edit'})).toHaveAttribute('title', expect.stringMatching(/branch before.*turn.*current files/i))
  })

  it('does not call work in progress when turn status is unknown', () => {
    renderTranscript([{ id: 'r1', type: 'reasoning', text: 'Work', turnId: 'turn-1' }])
    expect(screen.getByText(/Work status unavailable/)).toBeInTheDocument()
    expect(screen.queryByText(/Work in progress/)).not.toBeInTheDocument()
  })

  it('shows waiting when the active turn has an unresolved approval', () => {
    const items: AgentItem[] = [{ id: 'r1', type: 'reasoning', text: 'Work', turnId: 'turn-1', turnStatus: 'inProgress' }]
    renderTranscript(items, { session: {
      ...initialSession('thread-1'), turnId: 'turn-1', status: 'running', items,
      approvals: [{ id: 'approval-1', method: 'item/commandExecution/requestApproval', params: { turnId: 'turn-1' } }],
    } })
    expect(screen.getByText(/Waiting for input/)).toBeInTheDocument()
    expect(screen.queryByText(/Work in progress/)).not.toBeInTheDocument()
  })


  it('keeps the semantic reading anchor in place when transcript content resizes', () => {
    let onResize: ResizeObserverCallback | undefined
    class FakeResizeObserver {
      constructor(callback: ResizeObserverCallback) { onResize = callback }
      observe() {}
      unobserve() {}
      disconnect() {}
    }
    vi.stubGlobal('ResizeObserver', FakeResizeObserver)
    try {
      const { container } = renderTranscript([
        { id: 'anchor', type: 'agentMessage', text: 'Visible answer', turnId: 'turn-1' },
      ], { threadId: 'resize-anchor-thread' })
      const root = screen.getByLabelText('Conversation transcript')
      const row = container.querySelector<HTMLElement>('[data-transcript-item="anchor"]')!
      Object.defineProperties(root, {
        scrollHeight: { value: 1000, configurable: true },
        clientHeight: { value: 200, configurable: true },
        getBoundingClientRect: { value: () => ({ top: 0, bottom: 200, left: 0, right: 300, width: 300, height: 200, x: 0, y: 0, toJSON() {} }), configurable: true },
      })
      let rowTop = 100
      Object.defineProperty(row, 'getBoundingClientRect', {
        value: () => ({ top: rowTop, bottom: rowTop + 40, left: 0, right: 300, width: 300, height: 40, x: 0, y: rowTop, toJSON() {} }),
        configurable: true,
      })
      root.scrollTop = 300
      fireEvent.scroll(root)
      rowTop = 80
      onResize?.([], {} as ResizeObserver)
      expect(root.scrollTop).toBe(280)
    } finally {
      vi.unstubAllGlobals()
    }
  })

  it('shows jump-to-latest after the reader leaves the bottom', () => {
    renderTranscript([{ id: 'u', type: 'userMessage', text: 'Hello', turnId: 'turn-1' }])
    const root = screen.getByLabelText('Conversation transcript')
    Object.defineProperties(root, { scrollHeight: { value: 500, configurable: true }, clientHeight: { value: 200, configurable: true } })
    fireEvent.scroll(root, { target: { scrollTop: 0 } })
    expect(screen.getByRole('button', { name: 'Jump to latest' })).toBeInTheDocument()
  })
})
