import { describe, expect, it } from 'vitest'
import { groupTranscriptItems, itemText, activeTranscriptTurnIndex, openingUserMessageIds, projectAcceptedSubmissionMessages, transcriptTurns } from './transcript'

describe('transcript helpers', () => {
  it('groups only contiguous completed work items and leaves interactive content visible', () => {
    const entries = groupTranscriptItems([
      { id: 'u', type: 'userMessage', text: 'Do it', turnId: 't1' },
      { id: 'r', type: 'reasoning', text: 'thinking', turnId: 't1', turnStatus: 'completed' },
      { id: 'c', type: 'commandExecution', command: 'cargo test', turnId: 't1', turnStatus: 'completed' },
      { id: 'i', type: 'dynamicToolCall', tool: 'brr_ui_render', turnId: 't1', turnStatus: 'completed' },
      { id: 'f', type: 'fileChange', turnId: 't1', turnStatus: 'completed' },
      { id: 'a', type: 'agentMessage', text: 'Done', turnId: 't1' },
    ])
    expect(entries.map(entry => entry.kind)).toEqual(['item', 'work', 'item', 'work', 'item'])
    expect(entries[1]).toMatchObject({ kind: 'work', itemIds: ['r', 'c'], completed: true })
    expect(entries[3]).toMatchObject({ kind: 'work', itemIds: ['f'], completed: true })
  })

  it('keeps actual MCP Apps visible and preserves a work group ID as later items arrive', () => {
    const first = groupTranscriptItems([
      { id: 'r1', type: 'reasoning', text: 'thinking', turnId: 't1', turnStatus: 'completed' },
    ])[0]
    const grown = groupTranscriptItems([
      { id: 'r1', type: 'reasoning', text: 'thinking', turnId: 't1', turnStatus: 'completed' },
      { id: 'r2', type: 'commandExecution', command: 'test', turnId: 't1', turnStatus: 'completed' },
      { id: 'app', type: 'mcpToolCall', server: 'calendar', tool: 'today', mcpAppUi: { resourceUri: 'ui://calendar/today' }, turnId: 't1' },
    ], 'thread-1')
    expect(first).toMatchObject({ kind: 'work', id: 'work:t1:r1' })
    expect(grown[0]).toMatchObject({ kind: 'work', id: 'work:t1:r1' })
    expect(grown[1]).toMatchObject({ kind: 'item', item: { id: 'app' } })
  })

  it.each([
    ['completed', 'completed', 'completed'],
    ['interrupted', 'stopped', 'interrupted'],
    ['failed', 'failed', 'failed'],
    ['waiting', 'waiting', 'waiting'],
  ])('reports authoritative %s work status', (_status, expected, turnStatus) => {
    const [entry] = groupTranscriptItems([
      { id: 'work', type: 'reasoning', turnId: 't1', turnStatus },
    ])
    expect(entry).toMatchObject({ kind: 'work', status: expected })
  })

  it('does not treat missing turn evidence as completed or in progress', () => {
    const [entry] = groupTranscriptItems([
      { id: 'work', type: 'reasoning', turnId: 't1' },
    ])
    expect(entry).toMatchObject({ kind: 'work', status: 'unknown', completed: false })
  })

  it('uses matching session turn state for live or terminal work whose items omit status', () => {
    const items = [{ id: 'work', type: 'reasoning', turnId: 't1' }]
    expect(groupTranscriptItems(items, undefined, { turnId: 't1', status: 'running' })[0]).toMatchObject({ status: 'inProgress' })
    expect(groupTranscriptItems([{ ...items[0], turnStatus: 'inProgress' }], undefined, { turnId: 't1', status: 'waiting' })[0]).toMatchObject({ status: 'waiting' })
    expect(groupTranscriptItems(items, undefined, { turnId: 't1', status: 'interrupted' })[0]).toMatchObject({ status: 'stopped' })
    expect(groupTranscriptItems(items, undefined, { turnId: 't1', status: 'failed' })[0]).toMatchObject({ status: 'failed' })
    expect(groupTranscriptItems(items, undefined, { turnId: 't2', status: 'running' })[0]).toMatchObject({ status: 'unknown' })
  })

  it('allows editing only a completed turn’s opening user message', () => {
    expect(openingUserMessageIds([
      { id: 'u1', type: 'userMessage', turnId: 't1', turnStatus: 'completed' },
      { id: 'u2', type: 'userMessage', turnId: 't1', turnStatus: 'completed' },
      { id: 'u3', type: 'userMessage', turnId: 't2', turnStatus: 'inProgress' },
    ], 't2')).toEqual(new Set(['u1']))
  })

  it('extracts reusable text from user content and reports turns in transcript order', () => {
    expect(itemText({ id: 'u', type: 'userMessage', content: [{ type: 'text', text: 'first' }, { type: 'mention', name: 'Mina' }] })).toBe('first\n@Mina')
    const turns = transcriptTurns([
      { id: 'a', type: 'userMessage', turnId: 'one' },
      { id: 'b', type: 'agentMessage', turnId: 'two' },
      { id: 'c', type: 'reasoning', turnId: 'one' },
    ])
    expect(turns).toEqual(['one', 'two'])
    expect(activeTranscriptTurnIndex(turns, null)).toBe(1)
    expect(activeTranscriptTurnIndex(turns, 'one')).toBe(0)
  })

  it('projects attachment footer text only from the matching accepted submission and thread', () => {
    const session = {
      threadId: 'thread-a', turnId: null, status: 'idle' as const, items: [
        {id:'matched',type:'userMessage',clientId:'submission-a',content:[{type:'text',text:'Review this change\n\nAttached local file: patch.diff\nRead-only original: /private/stage/patch.diff'}]},
        {id:'wrong-thread',type:'userMessage',clientId:'submission-b',content:[{type:'text',text:'raw from another chat'}]},
        {id:'unmatched',type:'userMessage',content:[{type:'text',text:'raw without receipt'}]},
        {id:'uncertain',type:'userMessage',clientId:'submission-c',content:[{type:'text',text:'raw from uncertain delivery'}]},
      ], approvals: [], plan: [], diff: '', error: null, mode:'default' as const, proposedPlan:null,
    }
    const projected = projectAcceptedSubmissionMessages(session, 'thread-a', [
      {id:'submission-a',threadId:'thread-a',status:'accepted',document:{text:'Review this change'},attachmentIds:['one','two']},
      {id:'submission-b',threadId:'thread-b',status:'accepted',document:{text:'another original'},attachmentIds:[]},
      {id:'submission-c',threadId:'thread-a',status:'uncertain',document:{text:'not confirmed'},attachmentIds:[]},
    ])
    expect(itemText(projected.items[0])).toBe('Review this change\n\n2 attachments')
    expect(itemText(projected.items[1])).toBe('raw from another chat')
    expect(itemText(projected.items[2])).toBe('raw without receipt')
    expect(itemText(projected.items[3])).toBe('raw from uncertain delivery')
  })
})
