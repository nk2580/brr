import { render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { expect, it, vi } from 'vitest'
import { QuestionForm, QuestionTranscript } from './QuestionForm'
import type { QuestionRequest } from '../lib/questions'

const makeRequest = (overrides: Partial<QuestionRequest> = {}): QuestionRequest => ({
  id: 'request-7', threadId: 'thread-1', turnId: 'turn-2', itemId: 'item-3', agentId: 'agent-4',
  kind: 'blocking', questions: [{id:'language', header:'Language', question:'Which language should I use?', options:[{label:'TypeScript',description:'Typed JavaScript'},{label:'Rust',description:'Systems language'}]}, {id:'notes',header:'Notes',question:'Anything else to know?'}],
  draft: {}, resolution: 'pending', delivery: 'none', createdAt: 1, updatedAt: 1,
  ...overrides,
})

it('presents one compact composer question at a time and keeps Next separate from Submit', async () => {
  const user = userEvent.setup()
  const onSaveDraft = vi.fn(async () => {})
  const onAnswer = vi.fn(async () => {})
  render(<QuestionForm request={makeRequest()} onSaveDraft={onSaveDraft} onAnswer={onAnswer} onDismiss={vi.fn(async () => {})} />)

  const composer = screen.getByRole('region', {name:'Answer agent questions'})
  expect(composer).toHaveAttribute('id', 'question-input-request-7')
  expect(composer).toHaveClass('question-composer')
  expect(composer).toHaveAttribute('tabindex', '-1')
  expect(screen.getByText('1 of 2')).toBeInTheDocument()
  expect(screen.getByText('Which language should I use?')).toBeInTheDocument()
  expect(screen.queryByText('Anything else to know?')).not.toBeInTheDocument()
  await user.click(screen.getByRole('radio', {name:/TypeScript Typed JavaScript/}))
  await user.click(screen.getByRole('button', {name:'Next'}))

  expect(await screen.findByText('Anything else to know?')).toBeInTheDocument()
  expect(screen.getByText('2 of 2')).toBeInTheDocument()
  expect(onSaveDraft).toHaveBeenCalledWith({language:['TypeScript']})
  expect(onAnswer).not.toHaveBeenCalled()
  await user.click(screen.getByRole('button', {name:'Back'}))
  expect(screen.getByRole('radio', {name:/TypeScript Typed JavaScript/})).toBeChecked()
  await user.click(screen.getByRole('button', {name:'Next'}))
  await user.type(screen.getByRole('textbox', {name:'Anything else to know?'}), 'Review the tests')
  await user.click(screen.getByRole('button', {name:'Submit answers'}))
  await waitFor(() => expect(onAnswer).toHaveBeenCalledWith({language:['TypeScript'],notes:['Review the tests']}))
})

it('moves keyboard focus to each new prompt after Next and Back', async () => {
  const user = userEvent.setup()
  render(<QuestionForm request={makeRequest()} onSaveDraft={vi.fn()} onAnswer={vi.fn(async () => {})} onDismiss={vi.fn(async () => {})} />)
  await user.click(screen.getByRole('radio', {name:/TypeScript Typed JavaScript/}))
  screen.getByRole('button', {name:'Next'}).focus()
  await user.keyboard('{Enter}')

  const secondPrompt = screen.getByRole('heading', {name:'Anything else to know?'})
  expect(secondPrompt).toHaveFocus()
  await user.tab()
  expect(screen.getByRole('textbox', {name:'Anything else to know?'})).toHaveFocus()
  screen.getByRole('button', {name:'Back'}).focus()
  await user.keyboard('{Enter}')

  expect(screen.getByRole('heading', {name:'Which language should I use?'})).toHaveFocus()
  await user.tab()
  expect(screen.getByRole('radio', {name:/TypeScript Typed JavaScript/})).toHaveFocus()
})

it('places a trailing control beside question navigation in the composer footer', () => {
  render(<QuestionForm request={makeRequest()} onSaveDraft={vi.fn()} onAnswer={vi.fn(async () => {})} onDismiss={vi.fn(async () => {})} trailingControl={<button type="button" aria-label="Stop">Stop</button>} />)
  const stop = screen.getByRole('button', {name:'Stop'})
  const navigation = stop.parentElement
  expect(navigation).toHaveClass('question-composer-navigation')
  expect(within(navigation!).getByRole('button', {name:'Next'})).toBeInTheDocument()
})

it('clears a suggestion when custom text is entered and preserves both answers after a failed submit', async () => {
  const user = userEvent.setup()
  const onAnswer = vi.fn().mockRejectedValue(new Error('Connection lost'))
  render(<QuestionForm request={makeRequest()} onSaveDraft={vi.fn()} onAnswer={onAnswer} onDismiss={vi.fn(async () => {})} />)
  await user.click(screen.getByRole('radio', {name:/TypeScript Typed JavaScript/}))
  await user.type(screen.getByLabelText('Other answer for Language'), 'JavaScript')
  expect(screen.getByRole('radio', {name:/TypeScript Typed JavaScript/})).not.toBeChecked()
  await user.click(screen.getByRole('button', {name:'Next'}))
  await user.type(screen.getByRole('textbox', {name:'Anything else to know?'}), 'Use existing tests')
  await user.click(screen.getByRole('button', {name:'Submit answers'}))
  expect(await screen.findByRole('alert')).toHaveTextContent('Connection lost')
  expect(screen.getByRole('textbox', {name:'Anything else to know?'})).toHaveValue('Use existing tests')
  await user.click(screen.getByRole('button', {name:'Back'}))
  expect(screen.getByLabelText('Other answer for Language')).toHaveValue('JavaScript')
  expect(screen.getByRole('radio', {name:/TypeScript Typed JavaScript/})).not.toBeChecked()
  await user.click(screen.getByRole('button', {name:'Clear error'}))
  expect(screen.queryByRole('alert')).not.toBeInTheDocument()
})

it('locks answer inputs while the final answer is being submitted', async () => {
  const user = userEvent.setup()
  let finishAnswer!: () => void
  const onAnswer = vi.fn(() => new Promise<void>(resolve => { finishAnswer = resolve }))
  render(<QuestionForm request={makeRequest({questions:[{id:'notes',header:'Notes',question:'Anything else to know?',options:null}]})} onSaveDraft={vi.fn()} onAnswer={onAnswer} onDismiss={vi.fn(async () => {})} />)
  const notes = screen.getByRole('textbox', {name:'Anything else to know?'})
  await user.type(notes, 'Use the current tests')
  await user.click(screen.getByRole('button', {name:'Submit answers'}))
  expect(await screen.findByRole('button', {name:'Submitting…'})).toBeDisabled()
  expect(notes).toBeDisabled()
  finishAnswer()
  await waitFor(() => expect(screen.getByRole('button', {name:'Submit answers'})).toBeEnabled())
})

it('keeps Submit disabled until every question has exactly one trimmed, nonempty answer', async () => {
  const user = userEvent.setup()
  const onAnswer = vi.fn(async () => {})
  render(<QuestionForm request={makeRequest()} onSaveDraft={vi.fn()} onAnswer={onAnswer} onDismiss={vi.fn(async () => {})} />)
  expect(screen.getByRole('button', {name:'Next'})).toBeDisabled()
  await user.click(screen.getByRole('radio', {name:/Rust Systems language/}))
  expect(screen.getByRole('button', {name:'Next'})).toBeEnabled()
  await user.click(screen.getByRole('button', {name:'Next'}))
  const submit = screen.getByRole('button', {name:'Submit answers'})
  expect(submit).toBeDisabled()
  const notes = screen.getByRole('textbox', {name:'Anything else to know?'})
  expect(notes).toHaveAttribute('placeholder', 'Type your answer…')
  await user.type(notes, '   ')
  expect(submit).toBeDisabled()
  await user.clear(notes)
  await user.type(notes, ' Review the tests ')
  expect(submit).toBeEnabled()
  await user.click(submit)
  await waitFor(() => expect(onAnswer).toHaveBeenCalledWith({language:['Rust'],notes:['Review the tests']}))
})

it('accepts authoritative draft updates while untouched and keeps newer local edits', async () => {
  const user = userEvent.setup()
  const props = { onSaveDraft: vi.fn(), onAnswer: vi.fn(async () => {}), onDismiss: vi.fn(async () => {}) }
  const { rerender } = render(<QuestionForm request={makeRequest()} {...props} />)
  const updated = makeRequest({draft:{language:['Rust']}})
  rerender(<QuestionForm request={updated} {...props} />)
  expect(screen.getByRole('radio', {name:/Rust Systems language/})).toBeChecked()
  await user.click(screen.getByRole('radio', {name:/TypeScript Typed JavaScript/}))
  rerender(<QuestionForm request={makeRequest({draft:{language:['Rust']}})} {...props} />)
  expect(screen.getByRole('radio', {name:/TypeScript Typed JavaScript/})).toBeChecked()
})

it('does not flush a pending draft after the request has already resolved elsewhere', async () => {
  const user = userEvent.setup()
  const onSaveDraft = vi.fn()
  const { rerender, unmount } = render(<QuestionForm request={makeRequest()} onSaveDraft={onSaveDraft} onAnswer={vi.fn(async () => {})} onDismiss={vi.fn(async () => {})} />)
  await user.type(screen.getByLabelText('Other answer for Language'), 'Local unsent note')
  rerender(<QuestionForm request={makeRequest({resolution:'answered',delivery:'delivered',answers:{}})} onSaveDraft={onSaveDraft} onAnswer={vi.fn(async () => {})} onDismiss={vi.fn(async () => {})} />)
  unmount()
  expect(onSaveDraft).not.toHaveBeenCalled()
})

it('flushes the latest draft when leaving a still-pending composer', async () => {
  const user = userEvent.setup()
  const onSaveDraft = vi.fn(async () => {})
  const { unmount } = render(<QuestionForm request={makeRequest()} onSaveDraft={onSaveDraft} onAnswer={vi.fn(async () => {})} onDismiss={vi.fn(async () => {})} />)
  await user.type(screen.getByLabelText('Other answer for Language'), 'Preserve this note')
  unmount()
  await waitFor(() => expect(onSaveDraft).toHaveBeenCalledWith({language:['Preserve this note']}))
})

it('dismisses explicitly and renders answered requests as a read-only transcript summary', async () => {
  const user = userEvent.setup()
  const onDismiss = vi.fn(async () => {})
  render(<QuestionForm request={makeRequest()} onSaveDraft={vi.fn()} onAnswer={vi.fn()} onDismiss={onDismiss} />)
  await user.click(screen.getByRole('button', {name:'Dismiss'}))
  expect(onDismiss).toHaveBeenCalledOnce()
  render(<QuestionTranscript request={makeRequest({resolution:'answered',delivery:'delivered',answers:{language:['Rust'],notes:['Keep it concise']}})} onAnswer={vi.fn()} />)
  const transcript = screen.getByRole('region', {name:'Agent question'})
  expect(transcript).toHaveAttribute('id', 'question-request-7')
  expect(transcript).toHaveAttribute('tabindex', '-1')
  expect(within(transcript).getByRole('status')).toHaveTextContent('Answered')
  expect(within(transcript).getByText('Which language should I use?')).toBeInTheDocument()
  expect(within(transcript).getByText('Rust')).toBeInTheDocument()
  expect(within(transcript).queryByRole('button', {name:'Submit answers'})).not.toBeInTheDocument()
})

it('offers a compact answer-navigation action for pending transcript questions', async () => {
  const user = userEvent.setup()
  const onAnswer = vi.fn()
  render(<QuestionTranscript request={makeRequest()} origin="Agent task" onAnswer={onAnswer} />)
  const transcript = screen.getByRole('region', {name:'Agent question'})
  expect(transcript).toHaveAttribute('id', 'question-request-7')
  expect(transcript).toHaveTextContent('Which language should I use?')
  expect(transcript).toHaveTextContent('Waiting for your answer')
  await user.click(screen.getByRole('button', {name:'Answer questions for Language'}))
  expect(onAnswer).toHaveBeenCalledOnce()
  expect(screen.queryByRole('textbox')).not.toBeInTheDocument()
})

it('shows dismissed transcript questions as read-only with no answer provided', () => {
  render(<QuestionTranscript request={makeRequest({questions:[{id:'language',header:'Language',question:'Which language should I use?',options:null}],resolution:'dismissed'})} onAnswer={vi.fn()} />)
  const transcript = screen.getByRole('region', {name:'Agent question'})
  expect(within(transcript).getByRole('status')).toHaveTextContent('Dismissed')
  expect(within(transcript).getByText('No answer provided')).toBeInTheDocument()
  expect(within(transcript).queryByRole('button')).not.toBeInTheDocument()
})

it('saves the latest partial draft before an explicit dismissal', async () => {
  const user = userEvent.setup()
  const order: string[] = []
  const onSaveDraft = vi.fn(async () => { order.push('draft') })
  const onDismiss = vi.fn(async () => { order.push('dismiss') })
  const request = makeRequest({questions:[{id:'notes',header:'Notes',question:'Anything else to know?',options:null}]})
  render(<QuestionForm request={request} onSaveDraft={onSaveDraft} onAnswer={vi.fn(async () => {})} onDismiss={onDismiss} />)
  await user.type(screen.getByRole('textbox', {name:'Anything else to know?'}), 'Keep my draft')
  await user.click(screen.getByRole('button', {name:'Dismiss'}))
  await waitFor(() => expect(order).toEqual(['draft', 'dismiss']))
  expect(onSaveDraft).toHaveBeenCalledWith({notes:['Keep my draft']})
})

it('does not offer blind retry when transcript delivery is uncertain', () => {
  render(<QuestionTranscript request={makeRequest({resolution:'answered',delivery:'uncertain',reason:'Runtime disconnected while sending.'})} onAnswer={vi.fn()} />)
  expect(screen.getByRole('status')).toHaveTextContent('Delivery needs attention')
  expect(screen.getByText('Runtime disconnected while sending.')).toBeInTheDocument()
  expect(screen.queryByRole('button', {name:/Answer questions/})).not.toBeInTheDocument()
})
