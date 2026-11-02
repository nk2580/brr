import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import App from './App'

const backend = vi.hoisted(() => ({
  desktop: true,
  connect: vi.fn(async () => ({ account: { type: 'chatgpt', email: 'dev@example.com', planType: 'plus' }, models: [
    { id: 'sol', model: 'gpt-6.1-sol', displayName: 'GPT Sol', isDefault: true, defaultReasoningEffort: 'medium', supportedReasoningEfforts: [{ reasoningEffort: 'low' }, { reasoningEffort: 'medium' }, { reasoningEffort: 'high' }] },
    { id: 'astra', model: 'gpt-6-astra', displayName: 'GPT Astra', isDefault: false, defaultReasoningEffort: 'high', supportedReasoningEfforts: [{ reasoningEffort: 'medium' }, { reasoningEffort: 'high' }] },
  ], threads: [] })),
  subscribe: vi.fn(async () => () => {}),
  chooseWorkspace: vi.fn(async () => '/project'),
  send: vi.fn(async () => ({ turn: { id: 'turn-1' } })),
  start: vi.fn(async () => ({ thread: { id: 'thread-1', cwd: '/project', preview: '', updatedAt: 1 } })),
  steer: vi.fn(async () => ({ turnId: 'turn-1' })),
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

describe('composer model controls', () => {
  it('keeps model and reasoning beside Send and sends their selections without changing the draft', async () => {
    render(<App />)
    await userEvent.click(await screen.findByRole('button', { name: 'Choose project folder' }))
    const message = screen.getByRole('textbox', { name: 'Message' })
    await userEvent.type(message, 'Keep this exact draft')

    const model = screen.getByRole('combobox', { name: 'Model' })
    const reasoning = screen.getByRole('combobox', { name: 'Reasoning effort' })
    const send = screen.getByRole('button', { name: 'Send message' })
    const composer = message.closest('.composer-area')
    expect(composer).toBeTruthy()
    expect(model.closest('.composer-area')).toBe(composer)
    expect(reasoning.closest('.composer-area')).toBe(composer)
    expect(send.closest('.composer-area')).toBe(composer)
    expect(screen.getByRole('banner').querySelector('[aria-label="Model"]')).toBeNull()

    await userEvent.selectOptions(model, 'astra')
    await userEvent.selectOptions(reasoning, 'medium')
    expect(message).toHaveValue('Keep this exact draft')
    await userEvent.click(send)

    await waitFor(() => expect(backend.send).toHaveBeenCalledWith('thread-1', 'Keep this exact draft', 'gpt-6-astra', 'medium', 'default'))
    expect(message).toHaveValue('')
  })
})
