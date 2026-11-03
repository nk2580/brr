import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import App from './App'

vi.mock('./components/ChatEditor', async () => {
  const React = await import('react')
  const ChatEditor = React.forwardRef<any, any>((props, ref) => {
    const input = React.useRef<HTMLTextAreaElement>(null)
    const publish = (text: string) => props.onChange({version: 1, text, references: [], quotes: []})
    React.useImperativeHandle(ref, () => ({
      focus: () => input.current?.focus(),
      insertText: (value: string) => publish(`${props.document.text}${props.document.text ? '\n\n' : ''}${value}`),
      insertQuote: (value: string) => publish(`${props.document.text}${props.document.text ? ' ' : ''}“${value}”`),
      openMenu: () => {},
    }), [props.document])
    return <textarea aria-label="Message" placeholder="Ask anything" ref={input} value={props.document.text} onChange={event => publish(event.target.value)} onKeyDown={event => {
      if (event.key === 'Tab' && event.shiftKey) {event.preventDefault(); props.onTogglePlan()}
      else if (event.key === 'Enter' && (event.ctrlKey || event.metaKey)) {event.preventDefault(); props.onSubmit(true)}
      else if (event.key === 'Enter' && !event.shiftKey && !event.nativeEvent.isComposing) {event.preventDefault(); props.onSubmit(false)}
    }} />
  })
  return {ChatEditor}
})

const backend = vi.hoisted(() => ({
  desktop: true,
  connect: vi.fn(async () => ({ account: { type: 'chatgpt', email: 'dev@example.com', planType: 'plus' }, models: [
    { id: 'sol', model: 'gpt-6.1-sol', displayName: 'GPT Sol', isDefault: true, defaultReasoningEffort: 'medium', supportedReasoningEfforts: [{ reasoningEffort: 'low' }, { reasoningEffort: 'medium' }, { reasoningEffort: 'high' }] },
    { id: 'astra', model: 'gpt-6-astra', displayName: 'GPT Astra', isDefault: false, defaultReasoningEffort: 'high', supportedReasoningEfforts: [{ reasoningEffort: 'medium' }, { reasoningEffort: 'high' }] },
  ], threads: [] })),
  subscribe: vi.fn(async () => () => {}),
  projects: vi.fn(async () => ({data: [{id: 'project-main', name: 'Project', folders: ['/project'], primaryFolder: '/project', revision: 1, model: null, effort: null, permission: null, hidden: false}]})),
  inspectProject: vi.fn(async () => ({commands: []})),
  send: vi.fn(async (_threadId: string, _text: string, _model: string, _effort: string, _mode: string) => ({ turn: { id: 'turn-1' } })),
  start: vi.fn(async () => ({ thread: { id: 'thread-1', cwd: '/project', preview: '', updatedAt: 1 } })),
  steer: vi.fn(async (_threadId: string, _text: string, _turnId?: string) => ({ turnId: 'turn-1' })),
  resume: vi.fn(async (id: string) => ({ thread: { id, cwd: '/project', preview: '', updatedAt: 1, turns: [] } })),
  account: vi.fn(async () => null),
  login: vi.fn(async () => ({ loginId: 'login-1' })),
  cancelLogin: vi.fn(async () => ({ status: 'canceled' })),
  respond: vi.fn(async () => ({})),
  interrupt: vi.fn(async () => ({})),
  archive: vi.fn(async () => ({})),
  logout: vi.fn(async () => ({})),
  threads: vi.fn(async () => []),
}))

vi.mock('./lib/client', () => ({ client: backend, isDesktop: () => backend.desktop }))

const chatApi = vi.hoisted(() => ({
  records: [] as Array<any>,
  listDrafts: vi.fn(async () => chatApi.records),
  getDraft: vi.fn(async (id: string) => chatApi.records.find(record => record.id === id)),
  saveDraft: vi.fn(async (draft: any) => {
    const record = { ...draft, revision: (chatApi.records.find(item => item.id === draft.id)?.revision ?? 0) + 1, updatedAt: new Date().toISOString() }
    chatApi.records = [...chatApi.records.filter(item => item.id !== draft.id), record]
    return record
  }),
  promoteDraft: vi.fn(async (id: string, threadId: string) => {
    const record = chatApi.records.find(item => item.id === id)
    const promoted = { ...record, id: threadId, threadId }
    chatApi.records = [...chatApi.records.filter(item => item.id !== id && item.id !== threadId), promoted]
    return promoted
  }),
  submit: vi.fn(async (params: any) => {
    if (params.delivery === 'send') {
      const turn = await backend.send(params.threadId, params.document.text, params.settings.model, params.settings.effort, params.settings.mode)
      return { turn, submission: {}, queued: false }
    }
    if (params.delivery === 'steer') {
      const result = await backend.steer(params.threadId, params.document.text, params.expectedTurnId)
      return { turnId: result.turnId, submission: {}, queued: false }
    }
    return { submission: {}, queued: true }
  }),
  branch: vi.fn(),
  attachments: vi.fn(async () => []),
  queue: vi.fn(async () => ({ entries: [], state: { paused: false } })),
  skills: vi.fn(async () => []),
  references: vi.fn(async () => []),
  importFiles: vi.fn(async () => []),
  importPaths: vi.fn(async () => []),
  removeAttachment: vi.fn(async () => undefined),
}))
vi.mock('./lib/chat', () => ({chat: chatApi}))

describe('composer model controls', () => {
  it('keeps secondary settings in Chat options and sends their selections without changing the draft', async () => {
    render(<App />)
    await userEvent.selectOptions(await screen.findByRole('combobox', {name: 'Project'}), 'project-main')
    const message = screen.getByRole('textbox', { name: 'Message' })
    await userEvent.type(message, 'Keep this exact draft')

    const model = screen.getByRole('combobox', { name: 'Model' })
    await userEvent.selectOptions(model, 'astra')
    expect(screen.queryByRole('combobox', { name: 'Reasoning effort' })).not.toBeInTheDocument()
    expect(screen.queryByRole('combobox', { name: 'Chat identity' })).not.toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', { name: 'Chat options' }))
    const reasoning = screen.getByRole('combobox', { name: 'Reasoning effort' })
    const send = screen.getByRole('button', { name: 'Send message' })
    const composer = message.closest('.composer-area')
    expect(composer).toBeTruthy()
    expect(model.closest('.composer-area')).toBe(composer)
    expect(reasoning.closest('[role="dialog"]')).toHaveAccessibleName('Chat options')
    expect(send.closest('.composer-area')).toBe(composer)
    expect(screen.getByRole('banner').querySelector('[aria-label="Model"]')).toBeNull()

    await userEvent.selectOptions(reasoning, 'medium')
    await userEvent.click(screen.getByRole('checkbox', { name: 'Read only' }))
    await userEvent.keyboard('{Escape}')
    expect(screen.queryByRole('dialog', { name: 'Chat options' })).not.toBeInTheDocument()
    expect(message).toHaveValue('Keep this exact draft')
    await userEvent.click(send)

    await waitFor(() => expect(backend.send).toHaveBeenCalledWith('thread-1', 'Keep this exact draft', 'gpt-6-astra', 'medium', 'default'))
    expect(backend.start).toHaveBeenCalledWith('project-main', 'gpt-6-astra', true)
    await waitFor(() => expect(screen.getByRole('textbox', {name: 'Message'})).toHaveValue(''))
  })
})
