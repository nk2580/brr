import { describe, expect, it } from 'vitest'
import { initialSession, reduceSession } from './session'

const started = () => reduceSession(initialSession('chat'), {method: 'turn/started', params: {threadId: 'chat', turn: {id: 'turn', items: []}}})
const delta = (callId: string, text: string, itemId = `model-${callId}`) => ({method: 'brr/ui/argumentsDelta', params: {threadId: 'chat', turnId: 'turn', callId, itemId, delta: text}})

describe('Intelligent UI conversation streaming', () => {
  it('keeps interleaved document streams separate and reconciles the authoritative call', () => {
    let state = reduceSession(started(), delta('a', '{"title":"A",'))
    state = reduceSession(state, delta('b', '{"title":"B"}'))
    state = reduceSession(state, delta('a', '"components":[]}'))
    expect(state.items).toMatchObject([
      {id: 'a', type: 'dynamicToolCall', tool: 'brr_ui_render', argumentsText: '{"title":"A","components":[]}'},
      {id: 'b', argumentsText: '{"title":"B"}'},
    ])
    state = reduceSession(state, {method: 'item/completed', params: {threadId: 'chat', turnId: 'turn', item: {id: 'a', type: 'dynamicToolCall', tool: 'brr_ui_render', status: 'completed', success: true, arguments: {title: 'A'}}}})
    expect(reduceSession(state, delta('a', 'late')).items[0]).toMatchObject({status: 'completed', arguments: {title: 'A'}})
    expect(reduceSession(state, delta('a', 'late')).items[0].argumentsText).not.toContain('late')
  })

  it('rejects mismatched identities and removes unfinished executable previews on interruption', () => {
    let state = reduceSession(started(), delta('a', '{"title":"Partial"'))
    const valid = state
    expect(reduceSession(state, delta('a', 'wrong', 'another-model-item'))).toEqual(valid)
    expect(reduceSession(state, {...delta('a', 'wrong'), params: {...delta('a', 'wrong').params, turnId: 'older'}})).toEqual(valid)
    state = reduceSession(state, {method: 'turn/completed', params: {threadId: 'chat', turn: {id: 'turn', status: 'interrupted', items: []}}})
    expect(state.items[0]).toMatchObject({status: 'interrupted'})
    expect(reduceSession(state, delta('a', 'late'))).toEqual(state)
  })
})
