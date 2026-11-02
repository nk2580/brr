import { describe, expect, it } from 'vitest'
import { explicitAgentMention } from './agent-composer'

describe('explicit agent assignment from the composer', () => {
  it('creates a task context from only the current message and removes the mention token', () => {
    const result = explicitAgentMention('Please check this change. @Release guide list concrete blockers.', [
      { id: 'agent-1', name: 'Release guide' },
    ])

    expect(result).toEqual({ agentId: 'agent-1', instructions: 'Please check this change. list concrete blockers.' })
  })

  it('does not treat partial names or ordinary email addresses as assignment', () => {
    expect(explicitAgentMention('Ask @Release to look at dev@example.com', [{ id: 'agent-1', name: 'Release guide' }])).toBeNull()
  })

  it('preserves the current message line breaks when packaging task instructions', () => {
    expect(explicitAgentMention('Please review:\n- paths\n@Release guide\n- test gaps', [{ id: 'agent-1', name: 'Release guide' }])).toEqual({
      agentId: 'agent-1', instructions: 'Please review:\n- paths\n- test gaps',
    })
  })
})
