export type RpcEvent = {
  method: string;
  params?: any;
  id?: string | number;
};

export type AgentItem = {
  id: string;
  type: string;
  text?: string;
  command?: string;
  aggregatedOutput?: string;
  status?: string;
  changes?: { path: string; diff: string; kind?: unknown }[];
  [key: string]: unknown;
};

export type CollaborationMode = 'default' | 'plan';
export type ProposedPlan = { itemId: string; turnId: string; text: string; isComplete: boolean };

export type Approval = {
  id: string | number;
  method: string;
  params: Record<string, unknown>;
};

export type SessionState = {
  threadId: string | null;
  turnId: string | null;
  status: 'idle' | 'running' | 'failed' | 'interrupted';
  items: AgentItem[];
  approvals: Approval[];
  plan: { step: string; status: string }[];
  diff: string;
  error: string | null;
  mode: CollaborationMode;
  proposedPlan: ProposedPlan | null;
};

export function initialSession(threadId?: string): SessionState {
  return {
    threadId: threadId ?? null,
    turnId: null,
    status: 'idle',
    items: [],
    approvals: [],
    plan: [],
    diff: '',
    error: null,
    mode: 'default',
    proposedPlan: null,
  };
}

function upsertItem(items: AgentItem[], item: AgentItem): AgentItem[] {
  const index = items.findIndex((existing) => existing.id === item.id);
  if (index < 0) return [...items, item];
  return items.map((existing, itemIndex) =>
    itemIndex === index ? { ...existing, ...item } : existing,
  );
}

function mergeItems(items: AgentItem[], incoming: unknown): AgentItem[] {
  if (!Array.isArray(incoming)) return items;
  return incoming.reduce<AgentItem[]>((merged, item) => {
    if (!item || typeof item.id !== 'string' || typeof item.type !== 'string') return merged;
    return upsertItem(merged, item as AgentItem);
  }, items);
}

function proposedPlanFromItems(items: unknown, turnId: unknown, isComplete = false): ProposedPlan | null {
  if (!Array.isArray(items) || typeof turnId !== 'string') return null;
  const item = [...items].reverse().find(item => item?.type === 'plan' && typeof item.id === 'string' && typeof item.text === 'string' && item.text.trim());
  return item ? { itemId: item.id, turnId, text: item.text, isComplete } : null;
}

function appendItemText(
  state: SessionState,
  params: any,
  type: string,
  field: 'text' | 'aggregatedOutput',
): SessionState {
  if (typeof params.itemId !== 'string' || typeof params.delta !== 'string') return state;
  const current = state.items.find((item) => item.id === params.itemId);
  const item: AgentItem = {
    ...(current ?? { id: params.itemId, type }),
    [field]: `${typeof current?.[field] === 'string' ? current[field] : ''}${params.delta}`,
  };
  return {
    ...state,
    turnId: state.turnId ?? params.turnId ?? null,
    items: upsertItem(state.items, item),
  };
}

const historyEventMethods = new Set([
  'item/started',
  'item/completed',
  'item/agentMessage/delta',
  'item/reasoning/summaryTextDelta',
  'item/commandExecution/outputDelta',
  'item/fileChange/patchUpdated',
  'item/plan/delta',
  'item/commandExecution/requestApproval',
  'item/fileChange/requestApproval',
  'item/permissions/requestApproval',
  'item/tool/requestUserInput',
  'serverRequest/resolved',
]);

export function reduceSession(state: SessionState, event: RpcEvent): SessionState {
  const params = event.params ?? {};
  if (params.threadId && state.threadId && params.threadId !== state.threadId) return state;
  const eventTurnId = params.turnId ?? params.turn?.id;
  const staleTurnEvent = Boolean(state.turnId && eventTurnId && eventTurnId !== state.turnId);
  if (
    staleTurnEvent &&
    event.method !== 'turn/started' &&
    event.method !== 'turn/completed' &&
    !historyEventMethods.has(event.method)
  ) return state;

  switch (event.method) {
    case 'thread/settings/updated': {
      const mode = params.threadSettings?.collaborationMode?.mode;
      return mode === 'plan' || mode === 'default' ? { ...state, mode } : state;
    }
    case 'turn/started':
      return {
        ...state,
        threadId: state.threadId ?? params.threadId ?? null,
        turnId: params.turn?.id ?? state.turnId,
        status: 'running',
        items: mergeItems(state.items, params.turn?.items),
        plan: [],
        diff: '',
        error: null,
        proposedPlan: proposedPlanFromItems(params.turn?.items, params.turn?.id),
      };
    case 'turn/completed': {
      const turn = params.turn ?? {};
      if (state.turnId && turn.id && state.turnId !== turn.id) {
        return {
          ...state,
          items: mergeItems(state.items, turn.items),
          approvals: state.approvals.filter((approval) => approval.params.turnId !== turn.id),
        };
      }
      const status = turn.status === 'failed'
        ? 'failed'
        : turn.status === 'interrupted'
          ? 'interrupted'
          : turn.status === 'completed'
            ? 'idle'
            : state.status === 'failed' || state.status === 'interrupted'
              ? state.status
              : 'idle';
      const errorMessage = turn.error?.message;
      return {
        ...state,
        turnId: turn.id ?? state.turnId,
        status,
        items: mergeItems(state.items, turn.items),
        proposedPlan: proposedPlanFromItems(turn.items, turn.id ?? state.turnId, true) ?? state.proposedPlan,
        approvals: state.approvals.filter((approval) => approval.params.turnId !== turn.id),
        error: typeof errorMessage === 'string'
          ? errorMessage
          : status === 'failed' || status === 'interrupted'
            ? state.error
            : null,
      };
    }
    case 'item/started':
    case 'item/completed': {
      const item = params.item as AgentItem | undefined;
      if (!item?.id || !item.type) return state;
      if (event.method === 'item/started' && state.proposedPlan?.isComplete && state.proposedPlan.itemId === item.id) return state;
      const eventTurnId = params.turnId;
      const isStaleTurn = Boolean(state.turnId && eventTurnId && eventTurnId !== state.turnId);
      const isTerminalCurrentTurn = Boolean(
        eventTurnId && eventTurnId === state.turnId && state.status !== 'running',
      );
      return {
        ...state,
        turnId: isStaleTurn ? state.turnId : eventTurnId ?? state.turnId,
        status: isStaleTurn || isTerminalCurrentTurn ? state.status : 'running',
        items: upsertItem(state.items, item),
        proposedPlan: isStaleTurn ? state.proposedPlan : proposedPlanFromItems([item], eventTurnId ?? state.turnId, event.method === 'item/completed') ?? state.proposedPlan,
      };
    }
    case 'item/agentMessage/delta': {
      return appendItemText(state, params, 'agentMessage', 'text');
    }
    case 'item/reasoning/summaryTextDelta':
      return appendItemText(state, params, 'reasoning', 'text');
    case 'item/commandExecution/outputDelta':
      return appendItemText(state, params, 'commandExecution', 'aggregatedOutput');
    case 'item/fileChange/patchUpdated': {
      if (typeof params.itemId !== 'string' || !Array.isArray(params.changes)) return state;
      const current = state.items.find((item) => item.id === params.itemId);
      return {
        ...state,
        turnId: state.turnId ?? params.turnId ?? null,
        items: upsertItem(state.items, {
          ...(current ?? { id: params.itemId, type: 'fileChange' }),
          changes: params.changes,
        }),
      };
    }
    case 'turn/plan/updated':
      return {
        ...state,
        turnId: state.turnId ?? params.turnId ?? null,
        plan: Array.isArray(params.plan) ? params.plan : state.plan,
      };
    case 'error': {
      if (params.willRetry !== false) return state;
      if (state.turnId && params.turnId && state.turnId !== params.turnId) return state;
      const message = params.error?.message;
      return {
        ...state,
        turnId: params.turnId ?? state.turnId,
        status: 'failed',
        error: typeof message === 'string' ? message : 'The runtime reported an error.',
      };
    }
    case 'turn/diff/updated':
      return {
        ...state,
        turnId: state.turnId ?? params.turnId ?? null,
        diff: typeof params.diff === 'string' ? params.diff : state.diff,
      };
    case 'serverRequest/resolved':
      return {
        ...state,
        approvals: state.approvals.filter((approval) => approval.id !== params.requestId),
      };
    case 'item/plan/delta': {
      if (state.proposedPlan?.isComplete && state.proposedPlan.itemId === params.itemId && !staleTurnEvent) return state;
      const next = appendItemText(state, params, 'plan', 'text');
      if (staleTurnEvent) return next;
      return { ...next, proposedPlan: proposedPlanFromItems(next.items.filter(item => item.id === params.itemId), next.turnId) ?? next.proposedPlan };
    }
    default: {
      const approvalMethods = new Set([
        'item/commandExecution/requestApproval',
        'item/fileChange/requestApproval',
        'item/permissions/requestApproval',
        'item/tool/requestUserInput',
      ]);
      if (!approvalMethods.has(event.method) || event.id === undefined) return state;
      const approval: Approval = {
        id: event.id,
        method: event.method,
        params: params as Record<string, unknown>,
      };
      return {
        ...state,
        turnId: state.turnId ?? params.turnId ?? null,
        approvals: [...state.approvals.filter(({ id }) => id !== event.id), approval],
      };
    }
  }
}

function turnStatus(status: unknown): SessionState['status'] {
  if (status === 'inProgress') return 'running';
  if (status === 'failed') return 'failed';
  if (status === 'interrupted') return 'interrupted';
  return 'idle';
}

export function hydrateThread(thread: { id: string; turns?: any[]; collaborationMode?: { mode?: unknown } | null }): SessionState {
  const turns = Array.isArray(thread.turns) ? thread.turns : [];
  const latestTurn = [...turns].reverse().find((turn) => turn.status === 'inProgress')
    ?? turns.at(-1);
  const historyItems = turns.flatMap((turn) => Array.isArray(turn.items) ? turn.items : [])
    .filter((item): item is AgentItem => Boolean(item && typeof item.id === 'string' && typeof item.type === 'string'));
  const session = initialSession(thread.id);
  const error = latestTurn?.error?.message;
  return {
    ...session,
    mode: thread.collaborationMode?.mode === 'plan' ? 'plan' : 'default',
    proposedPlan: proposedPlanFromItems(latestTurn?.items, latestTurn?.id, latestTurn?.status === 'completed'),
    turnId: typeof latestTurn?.id === 'string' ? latestTurn.id : null,
    status: turnStatus(latestTurn?.status),
    items: historyItems,
    plan: Array.isArray(latestTurn?.plan) ? latestTurn.plan : [],
    diff: typeof latestTurn?.diff === 'string' ? latestTurn.diff : '',
    error: typeof error === 'string' ? error : null,
  };
}
