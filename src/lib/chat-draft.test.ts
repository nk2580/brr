import { describe, expect, it } from 'vitest'
import { appendQuote, assignmentFromDraft, draftWithText, emptyDraft } from './chat-draft'

describe('semantic chat drafts', () => {
  it('keeps plain text projection and semantic chips when text changes', () => {
    const draft = emptyDraft('Check this ')
    const withReference = {
      ...draft,
      text: 'Check this @Planner and review',
      references: [{ type: 'agent' as const, id: 'agent-1', name: 'Planner', start: 12, end: 20 }],
    }

    expect(draftWithText(withReference, 'Check this @Planner and review now')).toEqual({
      ...withReference,
      text: 'Check this @Planner and review now',
      references: withReference.references,
    })
  })

  it('appends quoted text with UTF-16 spans and preserves prior references', () => {
    const start = emptyDraft('Review ')
    const quoted = appendQuote({
      ...start,
      references: [{ type: 'file', id: 'file-1', name: 'src/a.ts', path: '/repo/src/a.ts', start: 7, end: 14 }],
    }, 'line one\nline two')

    expect(quoted.text).toBe('Review “line one\nline two”')
    expect(quoted.quotes).toEqual([{ text: 'line one\nline two', start: 7, end: 26 }])
    expect(quoted.references[0]).toMatchObject({ id: 'file-1', start: 7, end: 14 })
    expect(quoted.selection).toEqual({ anchor: quoted.text.length, focus: quoted.text.length })
  })

  it('assigns only explicit top-level agent tokens and ignores quoted, referenced, and fenced text', () => {
    const agents = [{ id: 'planner-id', name: 'Planner' }]
    expect(assignmentFromDraft(emptyDraft('@Planner review this'), agents)).toEqual({ kind: 'assigned', agent: agents[0] })
    expect(assignmentFromDraft(emptyDraft('email@Planner.example'), agents)).toEqual({ kind: 'none' })
    expect(assignmentFromDraft(emptyDraft("Don't assign this to @Planner; it's already reviewed."), agents)).toEqual({ kind: 'assigned', agent: agents[0] })
    expect(assignmentFromDraft(emptyDraft("Say '@Planner' only as an example."), agents)).toEqual({ kind: 'none' })
    expect(assignmentFromDraft({
      ...emptyDraft('“@Planner”\n```\n@Planner\n```'),
    }, agents)).toEqual({ kind: 'none' })
    expect(assignmentFromDraft({
      ...emptyDraft('@Planner review'),
      references: [{ type: 'agent', id: 'planner-id', name: 'Planner', start: 0, end: 8 }],
    }, agents)).toEqual({ kind: 'assigned', agent: agents[0] })
  })

  it('reports an explicit conflict when more than one agent is targeted', () => {
    expect(assignmentFromDraft(emptyDraft('@Planner then @Builder'), [
      { id: 'a', name: 'Planner' }, { id: 'b', name: 'Builder' },
    ])).toEqual({ kind: 'error', reason: 'multiple-agents', agentIds: ['a', 'b'] })
  })
})
