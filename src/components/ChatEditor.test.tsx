import { createRef } from 'react'
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'
import { ChatEditor, type ChatEditorHandle } from './ChatEditor'
import { emptyDraft } from '../lib/chat-draft'

describe('ChatEditor', () => {
  it('edits semantic plain text and sends on Enter while Shift+Enter inserts a newline', async () => {
    const onChange = vi.fn()
    const onSubmit = vi.fn()
    const ref = createRef<ChatEditorHandle>()
    const { rerender } = render(<ChatEditor ref={ref} document={emptyDraft()} onChange={onChange} onSubmit={onSubmit} onTogglePlan={vi.fn()} />)
    const editor = screen.getByRole('textbox', { name: 'Message' })

    act(() => ref.current?.insertText('first line'))
    expect(onChange).toHaveBeenLastCalledWith(expect.objectContaining({ version: 1, text: 'first line' }))
    fireEvent.keyDown(editor, { key: 'Enter', shiftKey: true })
    expect(onSubmit).not.toHaveBeenCalled()
    act(() => ref.current?.insertText('second line'))
    expect(onChange).toHaveBeenLastCalledWith(expect.objectContaining({ text: 'first line\nsecond line' }))
    rerender(<ChatEditor document={onChange.mock.lastCall?.[0] ?? emptyDraft()} onChange={onChange} onSubmit={onSubmit} onTogglePlan={vi.fn()} />)
    fireEvent.keyDown(screen.getByRole('textbox', { name: 'Message' }), { key: 'Enter' })
    expect(onSubmit).toHaveBeenCalledWith(false)
  })

  it('restores a semantic selection and keeps it during an unchanged parent echo', async () => {
    const document = { ...emptyDraft('abcdef'), selection: { anchor: 2, focus: 4 } }
    const props = { document, onChange: vi.fn(), onSubmit: vi.fn(), onTogglePlan: vi.fn() }
    const { rerender } = render(<ChatEditor {...props} />)
    await waitFor(() => {
      expect(window.getSelection()?.anchorOffset).toBe(2)
      expect(window.getSelection()?.focusOffset).toBe(4)
    })
    rerender(<ChatEditor {...props} document={{ ...document }} />)
    expect(window.getSelection()?.anchorOffset).toBe(2)
    expect(window.getSelection()?.focusOffset).toBe(4)
  })

  it('publishes the post-edit selection offsets with the plain text projection', () => {
    const onChange = vi.fn()
    const ref = createRef<ChatEditorHandle>()
    render(<ChatEditor ref={ref} document={{ ...emptyDraft('abcdef'), selection: { anchor: 2, focus: 4 } }} onChange={onChange} onSubmit={vi.fn()} onTogglePlan={vi.fn()} />)
    act(() => ref.current?.insertText('X'))
    expect(onChange).toHaveBeenLastCalledWith(expect.objectContaining({
      text: 'abXef',
      selection: { anchor: 3, focus: 3 },
    }))
  })

  it('keeps undo history when the parent echoes the just-published draft', async () => {
    const onChange = vi.fn()
    const ref = createRef<ChatEditorHandle>()
    const { rerender } = render(<ChatEditor ref={ref} document={{ ...emptyDraft('start'), attachmentIds: ['att-1'] }} onChange={onChange} onSubmit={vi.fn()} onTogglePlan={vi.fn()} />)
    act(() => ref.current?.insertText(' more'))
    const published = onChange.mock.lastCall?.[0]
    expect(published?.text).toBe('start more')
    await act(async () => { await new Promise(resolve => setTimeout(resolve, 10)) })
    rerender(<ChatEditor ref={ref} document={{ ...published }} onChange={onChange} onSubmit={vi.fn()} onTogglePlan={vi.fn()} />)
    act(() => ref.current?.undo())
    await waitFor(() => expect(onChange).toHaveBeenLastCalledWith(expect.objectContaining({
      text: 'start', selection: { anchor: 5, focus: 5 }, attachmentIds: ['att-1'],
    })))
    act(() => ref.current?.redo())
    await waitFor(() => expect(onChange).toHaveBeenLastCalledWith(expect.objectContaining({
      text: 'start more', selection: { anchor: 10, focus: 10 }, attachmentIds: ['att-1'],
    })))
  })

  it('undoes and redoes a settled menu insertion while preserving semantic metadata', async () => {
    const ref = createRef<ChatEditorHandle>()
    const onChange = vi.fn()
    render(<ChatEditor ref={ref} document={emptyDraft('Hello ')} onChange={onChange} onSubmit={vi.fn()} onTogglePlan={vi.fn()} agents={[{ id: 'agent-1', name: 'Planner' }]} />)
    act(() => ref.current?.openMenu('@'))
    fireEvent.keyDown(screen.getByRole('textbox', { name: 'Message' }), { key: 'Enter' })
    await waitFor(() => expect(onChange).toHaveBeenLastCalledWith(expect.objectContaining({
      text: 'Hello @Planner ',
      references: [expect.objectContaining({ type: 'agent', id: 'agent-1', start: 6, end: 14 })],
    })))
    act(() => ref.current?.undo())
    await waitFor(() => expect(onChange).toHaveBeenLastCalledWith(expect.objectContaining({ text: 'Hello ', references: [] })))
    act(() => ref.current?.redo())
    await waitFor(() => expect(onChange).toHaveBeenLastCalledWith(expect.objectContaining({
      text: 'Hello @Planner ', references: [expect.objectContaining({ id: 'agent-1' })],
    })))
  })

  it('supports the accelerated shortcut and avoids duplicate or IME sends', () => {
    const onSubmit = vi.fn()
    render(<ChatEditor document={emptyDraft('do work')} onChange={vi.fn()} onSubmit={onSubmit} onTogglePlan={vi.fn()} />)
    const editor = screen.getByRole('textbox', { name: 'Message' })
    fireEvent.keyDown(editor, { key: 'Enter', ctrlKey: true })
    expect(onSubmit).toHaveBeenCalledWith(true)
    onSubmit.mockClear()
    fireEvent.keyDown(editor, { key: 'Enter', isComposing: true })
    fireEvent.keyDown(editor, { key: 'Enter', repeat: true })
    expect(onSubmit).not.toHaveBeenCalled()
  })

  it('settles an @agent suggestion on Enter without sending and exposes the semantic reference', async () => {
    const onChange = vi.fn()
    const onSubmit = vi.fn()
    const ref = createRef<ChatEditorHandle>()
    const { rerender } = render(<ChatEditor ref={ref} document={emptyDraft()} onChange={onChange} onSubmit={onSubmit} onTogglePlan={vi.fn()} agents={[{ id: 'agent-1', name: 'Planner' }]} />)
    const editor = screen.getByRole('textbox', { name: 'Message' })
    act(() => ref.current?.openMenu('@'))
    expect(screen.getByRole('option', { name: /Planner/ })).toBeInTheDocument()
    onChange.mockClear()
    fireEvent.keyDown(editor, { key: 'Enter' })
    expect(onSubmit).not.toHaveBeenCalled()
    const latest = onChange.mock.lastCall?.[0]
    expect(latest?.references).toEqual([expect.objectContaining({ type: 'agent', id: 'agent-1', name: 'Planner' })])
    rerender(<ChatEditor document={latest} onChange={onChange} onSubmit={onSubmit} onTogglePlan={vi.fn()} agents={[{ id: 'agent-1', name: 'Planner' }]} />)
    await waitFor(() => expect(screen.getByRole('textbox', { name: 'Message' })).toHaveTextContent('@Planner'))
  })

  it('replaces a UTF-16 query span without deleting text before an emoji query', async () => {
    const onChange = vi.fn()
    const ref = createRef<ChatEditorHandle>()
    const { rerender } = render(<ChatEditor ref={ref} document={emptyDraft('KEEP ')} onChange={onChange} onSubmit={vi.fn()} onTogglePlan={vi.fn()} agents={[{ id: 'agent-emoji', name: '😀 Nova' }]} />)
    act(() => ref.current?.openMenu('@'))
    act(() => ref.current?.insertText('😀'))
    await waitFor(() => expect(screen.getByRole('option', { name: /😀 Nova/ })).toBeInTheDocument())
    fireEvent.keyDown(screen.getByRole('textbox', { name: 'Message' }), { key: 'Enter' })
    const latest = onChange.mock.lastCall?.[0]
    expect(latest?.text).toBe('KEEP @😀 Nova ')
    expect(latest?.references).toEqual([expect.objectContaining({ id: 'agent-emoji', start: 5, end: 13 })])
    rerender(<ChatEditor document={latest} onChange={onChange} onSubmit={vi.fn()} onTogglePlan={vi.fn()} agents={[{ id: 'agent-emoji', name: '😀 Nova' }]} />)
    await waitFor(() => expect(screen.getByRole('textbox', { name: 'Message' }).textContent).toBe('KEEP @😀 Nova '))
  })

  it('offers Plan and enabled skills from slash and toggles Plan with Shift+Tab when no menu is open', async () => {
    const onTogglePlan = vi.fn()
    const ref = createRef<ChatEditorHandle>()
    render(<ChatEditor ref={ref} document={emptyDraft()} onChange={vi.fn()} onSubmit={vi.fn()} onTogglePlan={onTogglePlan} skills={[{ name: 'Review', path: '/skills/review', description: 'Review a change' }]} />)
    const editor = screen.getByRole('textbox', { name: 'Message' })
    act(() => ref.current?.openMenu('/'))
    expect(screen.getByRole('option', { name: /Review/ })).toBeInTheDocument()
    fireEvent.keyDown(editor, { key: 'Escape' })
    fireEvent.keyDown(editor, { key: 'Tab', shiftKey: true })
    expect(onTogglePlan).toHaveBeenCalledOnce()
  })

  it('dismisses an open suggestion menu with Shift+Tab without settling an option or toggling Plan', () => {
    const onChange = vi.fn()
    const onTogglePlan = vi.fn()
    const ref = createRef<ChatEditorHandle>()
    render(<ChatEditor ref={ref} document={emptyDraft()} onChange={onChange} onSubmit={vi.fn()} onTogglePlan={onTogglePlan} agents={[{ id: 'agent-1', name: 'Planner' }]} />)
    const editor = screen.getByRole('textbox', { name: 'Message' })
    act(() => ref.current?.openMenu('@'))
    expect(screen.getByRole('option', { name: /Planner/ })).toBeInTheDocument()
    fireEvent.keyDown(editor, { key: 'Tab', shiftKey: true })
    expect(screen.queryByRole('option', { name: /Planner/ })).not.toBeInTheDocument()
    expect(onChange).not.toHaveBeenCalled()
    expect(onTogglePlan).not.toHaveBeenCalled()
  })

  it('keeps a dismissed suggestion closed until the token query changes', async () => {
    const ref = createRef<ChatEditorHandle>()
    const onChange = vi.fn()
    const agents = [{ id: 'agent-1', name: 'Planner' }]
    const { rerender } = render(<ChatEditor ref={ref} document={emptyDraft()} onChange={onChange} onSubmit={vi.fn()} onTogglePlan={vi.fn()} agents={agents} />)
    const editor = screen.getByRole('textbox', { name: 'Message' })
    act(() => ref.current?.openMenu('@'))
    fireEvent.keyDown(editor, { key: 'Escape' })
    rerender(<ChatEditor ref={ref} document={{ ...emptyDraft('@'), selection: { anchor: 1, focus: 1 } }} onChange={onChange} onSubmit={vi.fn()} onTogglePlan={vi.fn()} agents={agents} />)
    expect(screen.queryByRole('option', { name: /Planner/ })).not.toBeInTheDocument()
    rerender(<ChatEditor ref={ref} document={{ ...emptyDraft('@Pla'), selection: { anchor: 4, focus: 4 } }} onChange={onChange} onSubmit={vi.fn()} onTogglePlan={vi.fn()} agents={agents} />)
    await waitFor(() => expect(screen.getByRole('option', { name: /Planner/ })).toBeInTheDocument())
  })

  it('dismisses the suggestion menu when the user clicks outside the composer', () => {
    const ref = createRef<ChatEditorHandle>()
    render(<ChatEditor ref={ref} document={emptyDraft()} onChange={vi.fn()} onSubmit={vi.fn()} onTogglePlan={vi.fn()} agents={[{ id: 'agent-1', name: 'Planner' }]} />)
    act(() => ref.current?.openMenu('@'))
    expect(screen.getByRole('option', { name: /Planner/ })).toBeInTheDocument()
    fireEvent.pointerDown(document.body)
    expect(screen.queryByRole('option', { name: /Planner/ })).not.toBeInTheDocument()
  })

  it('preserves quote and reference metadata through the imperative quote insertion API', async () => {
    const ref = createRef<ChatEditorHandle>()
    const onChange = vi.fn()
    render(<ChatEditor ref={ref} document={emptyDraft('Review')} onChange={onChange} onSubmit={vi.fn()} onTogglePlan={vi.fn()} />)
    act(() => ref.current?.insertQuote('selected text'))
    await waitFor(() => expect(onChange).toHaveBeenLastCalledWith(expect.objectContaining({
      text: 'Review “selected text”',
      quotes: [{ text: 'selected text', start: 7, end: 22 }],
    })))
  })
})

it('opens the selected file reference without submitting the draft', async () => {
  const open = vi.fn(); const submit = vi.fn();
  const document = {...emptyDraft('@main.ts'), references:[{type:'file' as const,id:'src/main.ts',name:'main.ts',start:0,end:8}]};
  render(<ChatEditor document={document} onChange={vi.fn()} onSubmit={submit} onTogglePlan={vi.fn()} onOpenReference={open} />);
  fireEvent.click(screen.getByText('@main.ts'));
  expect(open).toHaveBeenCalledWith('src/main.ts','file');
  expect(submit).not.toHaveBeenCalled();
});
