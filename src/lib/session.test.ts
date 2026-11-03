import { describe, expect, it } from 'vitest';
import { hydrateThread, initialSession, reduceSession } from './session';

describe('session event reduction', () => {
  it('restores saved planning mode and accepts only authoritative mode updates for its thread', () => {
    let state = hydrateThread({ id: 'thread-1', collaborationMode: { mode: 'plan' }, turns: [] });
    expect(state.mode).toBe('plan');
    expect(initialSession('thread-2').mode).toBe('default');
    state = reduceSession(state, { method: 'thread/settings/updated', params: { threadId: 'thread-2', threadSettings: { collaborationMode: { mode: 'default' } } } });
    expect(state.mode).toBe('plan');
    state = reduceSession(state, { method: 'thread/settings/updated', params: { threadId: 'thread-1', threadSettings: { collaborationMode: { mode: 'default' } } } });
    expect(state.mode).toBe('default');
  });

  it('streams a proposed plan for the active turn and clears its handoff on the next turn', () => {
    let state = reduceSession(initialSession('thread-1'), { method: 'turn/started', params: { threadId: 'thread-1', turn: { id: 'plan-turn', status: 'inProgress', items: [] } } });
    state = reduceSession(state, { method: 'item/plan/delta', params: { threadId: 'thread-1', turnId: 'plan-turn', itemId: 'proposal', delta: '# Change\n' } });
    state = reduceSession(state, { method: 'item/plan/delta', params: { threadId: 'thread-1', turnId: 'plan-turn', itemId: 'proposal', delta: 'Keep the public API.' } });
    expect(state.proposedPlan).toMatchObject({ itemId: 'proposal', turnId: 'plan-turn', text: '# Change\nKeep the public API.' });
    state = reduceSession(state, { method: 'turn/completed', params: { threadId: 'thread-1', turn: { id: 'plan-turn', status: 'completed' } } });
    expect(state.proposedPlan?.text).toBe('# Change\nKeep the public API.');
    state = reduceSession(state, { method: 'turn/started', params: { threadId: 'thread-1', turn: { id: 'implementation-turn', status: 'inProgress' } } });
    expect(state.proposedPlan).toBeNull();
    state = reduceSession(state, { method: 'item/completed', params: { threadId: 'thread-1', turnId: 'plan-turn', item: { id: 'proposal', type: 'plan', text: 'Old proposal arrives late' } } });
    expect(state.proposedPlan).toBeNull();
    expect(state.turnId).toBe('implementation-turn');
  });

  it('restores only the latest turn proposal after restart and recovers a final snapshot without deltas', () => {
    const planTurn = { id: 'plan-turn', status: 'completed', items: [{ id: 'proposal', type: 'plan', text: '# Proposed change\nKeep existing permissions.' }] };
    const state = hydrateThread({ id: 'thread-1', collaborationMode: { mode: 'plan' }, turns: [planTurn] });
    expect(state.proposedPlan).toMatchObject({ itemId: 'proposal', turnId: 'plan-turn', text: '# Proposed change\nKeep existing permissions.' });
    expect(state.mode).toBe('plan');
    const later = hydrateThread({ id: 'thread-1', collaborationMode: { mode: 'default' }, turns: [planTurn, { id: 'new-turn', status: 'completed', items: [] }] });
    expect(later.proposedPlan).toBeNull();
    const started = reduceSession(initialSession('thread-1'), { method: 'turn/started', params: { threadId: 'thread-1', turn: { id: 'plan-turn', status: 'inProgress' } } });
    const finished = reduceSession(started, { method: 'turn/completed', params: { threadId: 'thread-1', turn: planTurn } });
    expect(finished.proposedPlan).toEqual(state.proposedPlan);
  });

  it('offers only authoritative completed plan text for implementation, never streamed draft text', () => {
    let state = reduceSession(initialSession('thread-1'), { method: 'turn/started', params: { threadId: 'thread-1', turn: { id: 'plan-turn', status: 'inProgress' } } });
    state = reduceSession(state, { method: 'item/plan/delta', params: { threadId: 'thread-1', turnId: 'plan-turn', itemId: 'proposal', delta: 'An unfinished draft' } });
    expect(state.proposedPlan?.isComplete).toBe(false);
    state = reduceSession(state, { method: 'turn/completed', params: { threadId: 'thread-1', turn: { id: 'plan-turn', status: 'completed' } } });
    expect(state.proposedPlan?.isComplete).toBe(false);
    state = reduceSession(state, { method: 'item/completed', params: { threadId: 'thread-1', turnId: 'plan-turn', item: { id: 'proposal', type: 'plan', text: 'The authoritative final plan' } } });
    expect(state.proposedPlan).toEqual({ itemId: 'proposal', turnId: 'plan-turn', text: 'The authoritative final plan', isComplete: true });
    state = reduceSession(state, { method: 'item/plan/delta', params: { threadId: 'thread-1', turnId: 'plan-turn', itemId: 'proposal', delta: ' late draft fragment' } });
    expect(state.proposedPlan?.text).toBe('The authoritative final plan');
    expect(state.items.find(item => item.id === 'proposal')?.text).toBe('The authoritative final plan');
  });

  it('streams a message into its existing item and ignores events from other threads', () => {
    const initial = initialSession('thread-1');
    const started = reduceSession(initial, {
      method: 'turn/started',
      params: {
        threadId: 'thread-1',
        turn: { id: 'turn-1', status: 'inProgress', items: [] },
      },
    });
    const itemStarted = reduceSession(started, {
      method: 'item/started',
      params: {
        threadId: 'thread-1',
        turnId: 'turn-1',
        item: { id: 'message-1', type: 'agentMessage', text: '' },
      },
    });
    const message = reduceSession(itemStarted, {
      method: 'item/agentMessage/delta',
      params: {
        threadId: 'thread-1',
        turnId: 'turn-1',
        itemId: 'message-1',
        delta: 'Hello',
      },
    });
    const ignored = reduceSession(message, {
      method: 'item/agentMessage/delta',
      params: {
        threadId: 'thread-2',
        turnId: 'turn-2',
        itemId: 'message-1',
        delta: ' wrong thread',
      },
    });

    expect(ignored).toMatchObject({
      threadId: 'thread-1',
      turnId: 'turn-1',
      status: 'running',
      items: [{ id: 'message-1', type: 'agentMessage', text: 'Hello' }],
    });
    expect(ignored.items).toHaveLength(1);
  });

  it('reduces reasoning, command output, file patches, plans, and turn diffs', () => {
    let state = initialSession('thread-1');
    state = reduceSession(state, {
      method: 'item/reasoning/summaryTextDelta',
      params: { threadId: 'thread-1', turnId: 'turn-1', itemId: 'reason-1', delta: 'Thinking' },
    });
    state = reduceSession(state, {
      method: 'item/commandExecution/outputDelta',
      params: { threadId: 'thread-1', turnId: 'turn-1', itemId: 'cmd-1', delta: 'output\n' },
    });
    state = reduceSession(state, {
      method: 'item/fileChange/patchUpdated',
      params: {
        threadId: 'thread-1',
        turnId: 'turn-1',
        itemId: 'file-1',
        changes: [{ path: 'src/a.ts', kind: 'update', diff: '+line' }],
      },
    });
    state = reduceSession(state, {
      method: 'turn/plan/updated',
      params: {
        threadId: 'thread-1',
        turnId: 'turn-1',
        plan: [{ step: 'Implement', status: 'inProgress' }],
      },
    });
    state = reduceSession(state, {
      method: 'turn/diff/updated',
      params: { threadId: 'thread-1', turnId: 'turn-1', diff: 'diff body' },
    });

    expect(state).toMatchObject({
      turnId: 'turn-1',
      items: [
        { id: 'reason-1', type: 'reasoning', text: 'Thinking' },
        { id: 'cmd-1', type: 'commandExecution', aggregatedOutput: 'output\n' },
        { id: 'file-1', type: 'fileChange', changes: [{ path: 'src/a.ts', diff: '+line' }] },
      ],
      plan: [{ step: 'Implement', status: 'inProgress' }],
      diff: 'diff body',
    });
  });

  it('tracks approval requests by string or numeric id and clears them on resolution or turn completion', () => {
    let state = initialSession('thread-1');
    state = reduceSession(state, {
      method: 'item/fileChange/requestApproval',
      id: 'approval-a',
      params: { threadId: 'thread-1', turnId: 'turn-1', itemId: 'file-1', reason: 'Review patch' },
    });
    state = reduceSession(state, {
      method: 'item/commandExecution/requestApproval',
      id: 27,
      params: { threadId: 'thread-1', turnId: 'turn-2', itemId: 'cmd-2' },
    });
    expect(state.approvals).toEqual([
      { id: 'approval-a', method: 'item/fileChange/requestApproval', params: { threadId: 'thread-1', turnId: 'turn-1', itemId: 'file-1', reason: 'Review patch' } },
      { id: 27, method: 'item/commandExecution/requestApproval', params: { threadId: 'thread-1', turnId: 'turn-2', itemId: 'cmd-2' } },
    ]);

    state = reduceSession(state, {
      method: 'serverRequest/resolved',
      params: { threadId: 'thread-1', requestId: 'approval-a' },
    });
    expect(state.approvals.map(({ id }) => id)).toEqual([27]);

    state = reduceSession(state, {
      method: 'turn/completed',
      params: {
        threadId: 'thread-1',
        turn: { id: 'turn-2', status: 'completed', items: [], error: null },
      },
    });
    expect(state).toMatchObject({ status: 'idle', approvals: [] });
  });

  it('keeps tool user-input requests out of approval state for the durable question flow', () => {
    let state = reduceSession(initialSession('thread-1'), {
      method: 'item/tool/requestUserInput',
      id: 'input-approval',
      params: { threadId: 'thread-1', turnId: 'turn-1', itemId: 'question-1', questions: [] },
    });
    expect(state.approvals).toEqual([])

    state = reduceSession(state, {
      method: 'serverRequest/resolved',
      params: { threadId: 'thread-1', requestId: 'input-approval' },
    });
    expect(state.approvals).toEqual([]);
  });

  it('marks a non-retryable runtime error as failed and shows its message', () => {
    const started = reduceSession(initialSession('thread-1'), {
      method: 'turn/started',
      params: { threadId: 'thread-1', turn: { id: 'turn-1', status: 'inProgress', items: [] } },
    });
    const failed = reduceSession(started, {
      method: 'error',
      params: {
        threadId: 'thread-1',
        turnId: 'turn-1',
        error: { message: 'Runtime failed' },
        willRetry: false,
      },
    });

    expect(failed).toMatchObject({ status: 'failed', error: 'Runtime failed', turnId: 'turn-1' });
  });

  it('does not surface a retryable runtime error or fail the active turn', () => {
    const started = reduceSession(initialSession('thread-1'), {
      method: 'turn/started',
      params: { threadId: 'thread-1', turn: { id: 'turn-1', status: 'inProgress', items: [] } },
    });
    const retrying = reduceSession(started, {
      method: 'error',
      params: {
        threadId: 'thread-1',
        turnId: 'turn-1',
        error: { message: 'Temporary provider error' },
        willRetry: true,
      },
    });

    expect(retrying).toEqual(started);
  });

  it.each([
    ['failed', 'failed', { message: 'provider failed' }],
    ['interrupted', 'interrupted', null],
  ] as const)('preserves a %s terminal turn result', (turnStatus, expectedStatus, error) => {
    const state = reduceSession(initialSession('thread-1'), {
      method: 'turn/completed',
      params: {
        threadId: 'thread-1',
        turn: { id: 'turn-1', status: turnStatus, items: [], error },
      },
    });
    expect(state.status).toBe(expectedStatus);
    expect(state.error).toBe(error?.message ?? null);
  });

  it('keeps an interrupted turn terminal when its command completes late', () => {
    let state = reduceSession(initialSession('thread-1'), {
      method: 'turn/started',
      params: { threadId: 'thread-1', turn: { id: 'turn-stop', status: 'inProgress', items: [] } },
    });
    state = reduceSession(state, {
      method: 'item/started',
      params: {
        threadId: 'thread-1',
        turnId: 'turn-stop',
        item: { id: 'command-1', type: 'commandExecution', status: 'inProgress', command: 'sleep 30' },
      },
    });
    state = reduceSession(state, {
      method: 'turn/completed',
      params: { threadId: 'thread-1', turn: { id: 'turn-stop', status: 'interrupted', items: [] } },
    });

    state = reduceSession(state, {
      method: 'item/completed',
      params: {
        threadId: 'thread-1',
        turnId: 'turn-stop',
        item: { id: 'command-1', type: 'commandExecution', status: 'completed', exitCode: 130 },
      },
    });

    expect(state).toMatchObject({
      turnId: 'turn-stop',
      status: 'interrupted',
      items: [{ id: 'command-1', status: 'completed', exitCode: 130 }],
    });
    expect(state.items[0]?.turnStatus).toBe('interrupted');
  });

  it('marks an interrupted older turn when its completion arrives after the next turn starts', () => {
    let state = reduceSession(initialSession('thread-1'), {
      method: 'turn/started',
      params: { threadId: 'thread-1', turn: { id: 'turn-stop', status: 'inProgress', items: [] } },
    });
    state = reduceSession(state, {
      method: 'item/started',
      params: { threadId: 'thread-1', turnId: 'turn-stop', item: { id: 'reasoning-stop', type: 'reasoning', text: 'work' } },
    });
    state = reduceSession(state, {
      method: 'turn/started',
      params: { threadId: 'thread-1', turn: { id: 'turn-next', status: 'inProgress', items: [] } },
    });
    state = reduceSession(state, {
      method: 'turn/completed',
      params: { threadId: 'thread-1', turn: { id: 'turn-stop', status: 'interrupted', items: [] } },
    });
    expect(state.turnId).toBe('turn-next')
    expect(state.items.find(item => item.id === 'reasoning-stop')?.turnStatus).toBe('interrupted')
  });

  it('does not let a prior turn item event replace the active turn', () => {
    let state = reduceSession(initialSession('thread-1'), {
      method: 'turn/started',
      params: { threadId: 'thread-1', turn: { id: 'turn-old', status: 'inProgress', items: [] } },
    });
    state = reduceSession(state, {
      method: 'turn/completed',
      params: { threadId: 'thread-1', turn: { id: 'turn-old', status: 'interrupted', items: [] } },
    });
    state = reduceSession(state, {
      method: 'turn/started',
      params: { threadId: 'thread-1', turn: { id: 'turn-current', status: 'inProgress', items: [] } },
    });

    state = reduceSession(state, {
      method: 'item/started',
      params: {
        threadId: 'thread-1',
        turnId: 'turn-old',
        item: { id: 'old-command', type: 'commandExecution', status: 'inProgress' },
      },
    });

    expect(state).toMatchObject({ turnId: 'turn-current', status: 'running' });
    expect(state.items).toContainEqual({ id: 'old-command', type: 'commandExecution', status: 'inProgress', turnId: 'turn-old' });
  });

  it('keeps active turn metadata when scoped events from a prior turn arrive late', () => {
    let state = reduceSession(initialSession('thread-1'), {
      method: 'turn/started',
      params: { threadId: 'thread-1', turn: { id: 'turn-old', status: 'inProgress', items: [] } },
    });
    state = reduceSession(state, {
      method: 'turn/started',
      params: { threadId: 'thread-1', turn: { id: 'turn-current', status: 'inProgress', items: [] } },
    });
    state = reduceSession(state, {
      method: 'turn/plan/updated',
      params: { threadId: 'thread-1', turnId: 'turn-current', plan: [{ step: 'Current plan', status: 'inProgress' }] },
    });
    state = reduceSession(state, {
      method: 'turn/diff/updated',
      params: { threadId: 'thread-1', turnId: 'turn-current', diff: 'current diff' },
    });

    state = reduceSession(state, {
      method: 'item/agentMessage/delta',
      params: { threadId: 'thread-1', turnId: 'turn-old', itemId: 'old-message', delta: 'late text' },
    });
    state = reduceSession(state, {
      method: 'item/fileChange/patchUpdated',
      params: { threadId: 'thread-1', turnId: 'turn-old', itemId: 'old-file', changes: [{ path: 'old.ts', diff: '+old' }] },
    });
    state = reduceSession(state, {
      method: 'turn/plan/updated',
      params: { threadId: 'thread-1', turnId: 'turn-old', plan: [{ step: 'Old plan', status: 'completed' }] },
    });
    state = reduceSession(state, {
      method: 'turn/diff/updated',
      params: { threadId: 'thread-1', turnId: 'turn-old', diff: 'old diff' },
    });
    state = reduceSession(state, {
      method: 'item/commandExecution/requestApproval',
      id: 'old-approval',
      params: { threadId: 'thread-1', turnId: 'turn-old', itemId: 'old-command' },
    });

    expect(state).toMatchObject({
      turnId: 'turn-current',
      status: 'running',
      plan: [{ step: 'Current plan', status: 'inProgress' }],
      diff: 'current diff',
      items: [
        { id: 'old-message', text: 'late text' },
        { id: 'old-file', changes: [{ path: 'old.ts', diff: '+old' }] },
      ],
    });
    expect(state.approvals.map(approval => [approval.id, approval.params.turnId])).toEqual([['old-approval', 'turn-old']]);
  });

  it('merges late prior-turn final items and clears only that turn approvals', () => {
    let state = reduceSession(initialSession('thread-1'), {
      method: 'turn/started',
      params: { threadId: 'thread-1', turn: { id: 'turn-a', status: 'inProgress', items: [] } },
    });
    state = reduceSession(state, {
      method: 'item/commandExecution/requestApproval',
      id: 'approval-a',
      params: { threadId: 'thread-1', turnId: 'turn-a', itemId: 'command-a' },
    });
    state = reduceSession(state, {
      method: 'turn/started',
      params: { threadId: 'thread-1', turn: { id: 'turn-b', status: 'inProgress', items: [] } },
    });
    state = reduceSession(state, {
      method: 'turn/plan/updated',
      params: { threadId: 'thread-1', turnId: 'turn-b', plan: [{ step: 'Current', status: 'inProgress' }] },
    });
    state = reduceSession(state, {
      method: 'turn/diff/updated',
      params: { threadId: 'thread-1', turnId: 'turn-b', diff: 'current diff' },
    });
    state = reduceSession(state, {
      method: 'item/commandExecution/requestApproval',
      id: 'approval-b',
      params: { threadId: 'thread-1', turnId: 'turn-b', itemId: 'command-b' },
    });

    state = reduceSession(state, {
      method: 'turn/completed',
      params: {
        threadId: 'thread-1',
        turn: {
          id: 'turn-a',
          status: 'completed',
          items: [{ id: 'final-a', type: 'agentMessage', text: 'Final answer from A' }],
        },
      },
    });

    expect(state).toMatchObject({
      turnId: 'turn-b',
      status: 'running',
      plan: [{ step: 'Current', status: 'inProgress' }],
      diff: 'current diff',
      items: [{ id: 'final-a', text: 'Final answer from A' }],
    });
    expect(state.approvals.map(approval => approval.id)).toEqual(['approval-b']);
  });

  it('hydrates all history items and identifies the latest active turn', () => {
    const state = hydrateThread({
      id: 'thread-1',
      turns: [
        {
          id: 'turn-old',
          status: 'completed',
          items: [{ id: 'old-message', type: 'agentMessage', text: 'Earlier answer' }],
        },
        {
          id: 'turn-live',
          status: 'inProgress',
          items: [{ id: 'live-message', type: 'agentMessage', text: 'Current answer' }],
        },
      ],
    });

    expect(state).toMatchObject({
      threadId: 'thread-1',
      turnId: 'turn-live',
      status: 'running',
      items: [
        { id: 'old-message', text: 'Earlier answer' },
        { id: 'live-message', text: 'Current answer' },
      ],
    });
  });

  it('keeps hydrated history across a new turn and ignores stale turn completion', () => {
    let state = hydrateThread({
      id: 'thread-1',
      turns: [
        { id: 'turn-old-1', status: 'completed', items: [{ id: 'user-1', type: 'userMessage', text: 'First' }] },
        { id: 'turn-old-2', status: 'completed', items: [{ id: 'answer-2', type: 'agentMessage', text: 'Second' }] },
      ],
    });
    state = reduceSession(state, {
      method: 'turn/plan/updated',
      params: { threadId: 'thread-1', turnId: 'turn-old-2', plan: [{ step: 'Old plan', status: 'completed' }] },
    });
    state = reduceSession(state, {
      method: 'turn/diff/updated',
      params: { threadId: 'thread-1', turnId: 'turn-old-2', diff: 'old diff' },
    });
    state = reduceSession(state, {
      method: 'turn/started',
      params: {
        threadId: 'thread-1',
        turn: { id: 'turn-new', status: 'inProgress', items: [{ id: 'user-3', type: 'userMessage', text: 'Third' }] },
      },
    });
    const afterStart = state;
    expect(afterStart).toMatchObject({ plan: [], diff: '' });
    state = reduceSession(state, {
      method: 'turn/completed',
      params: {
        threadId: 'thread-1',
        turn: { id: 'turn-old-2', status: 'completed', items: [] },
      },
    });
    expect(state).toEqual(afterStart);

    state = reduceSession(state, {
      method: 'turn/completed',
      params: {
        threadId: 'thread-1',
        turn: { id: 'turn-new', status: 'completed', items: [{ id: 'answer-3', type: 'agentMessage', text: 'Third answer' }] },
      },
    });
    expect(state).toMatchObject({
      turnId: 'turn-new',
      status: 'idle',
      items: [
        { id: 'user-1', text: 'First' },
        { id: 'answer-2', text: 'Second' },
        { id: 'user-3', text: 'Third' },
        { id: 'answer-3', text: 'Third answer' },
      ],
    });
  });
});

it('retains the owning turn for both loaded and streamed items', () => {
  const loaded = hydrateThread({id:'chat', turns:[{id:'old',status:'completed',items:[{id:'first',type:'userMessage',text:'First'}]}]})
  expect(loaded.items[0].turnId).toBe('old')
  const live = reduceSession(loaded,{method:'item/agentMessage/delta',params:{threadId:'chat',turnId:'new',itemId:'answer',delta:'Hello'}})
  expect(live.items.find(item=>item.id==='answer')?.turnId).toBe('new')
})

it('completes streamed work even when the completion snapshot omits items', () => {
  let state = reduceSession(initialSession('chat'), {method:'turn/started',params:{threadId:'chat',turn:{id:'a',status:'inProgress',items:[]}}});
  state = reduceSession(state, {method:'item/started',params:{threadId:'chat',turnId:'a',item:{id:'work',type:'commandExecution',status:'inProgress'}}});
  state = reduceSession(state, {method:'turn/completed',params:{threadId:'chat',turn:{id:'a',status:'completed',items:[]}}});
  expect(state.items.find(item => item.id === 'work')?.turnStatus).toBe('completed');
});
