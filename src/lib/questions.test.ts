import { describe, expect, it } from 'vitest'
import { getQuestionStatus, type QuestionRequest } from './questions'

const request = (overrides: Partial<QuestionRequest> = {}): QuestionRequest => ({
  id: 'q-1', threadId: 'thread-1', turnId: 'turn-1', itemId: 'item-1', kind: 'blocking',
  questions: [], draft: {}, resolution: 'pending', delivery: 'none', createdAt: 1, updatedAt: 1,
  ...overrides,
})

describe('question request status', () => {
  it.each([
    [request(), 'Waiting for your answer'],
    [request({ kind: 'async' }), 'Agent is continuing'],
    [request({ kind: 'async', waiting: true }), 'Waiting for your answer'],
    [request({ resolution: 'answered', delivery: 'queued' }), 'Answer queued'],
    [request({ resolution: 'answered', delivery: 'dispatching' }), 'Sending'],
    [request({ resolution: 'answered', delivery: 'delivered' }), 'Answered'],
    [request({ resolution: 'dismissed' }), 'Dismissed'],
    [request({ resolution: 'closed' }), 'Closed'],
    [request({ resolution: 'answered', delivery: 'uncertain' }), 'Delivery needs attention'],
    [request({ resolution: 'dismissed', delivery: 'uncertain' }), 'Delivery needs attention'],
  ])('maps request state to %s', (value, expected) => {
    expect(getQuestionStatus(value as QuestionRequest)).toBe(expected)
  })
})
