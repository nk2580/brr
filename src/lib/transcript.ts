import type { AgentItem, SessionState } from './session'
import { extractMcpAppInvocation } from './mcp-app'

export type TranscriptEntry =
  | { kind: 'item'; item: AgentItem }
  | { kind: 'work'; id: string; itemIds: string[]; items: AgentItem[]; completed: boolean; status: WorkStatus }

export type WorkStatus = 'completed' | 'failed' | 'stopped' | 'waiting' | 'inProgress' | 'unknown'

const workTypes = new Set([
  'reasoning', 'commandExecution', 'fileChange', 'mcpToolCall',
  'collabAgentToolCall', 'subAgentActivity',
])

function hasMcpApp(item: AgentItem, threadId?: string) {
  return extractMcpAppInvocation(item, threadId) !== null
}

function isFoldableWork(item: AgentItem, threadId?: string) {
  return workTypes.has(item.type) && !hasMcpApp(item, threadId)
}

function workStatus(items: AgentItem[], activeTurn?: { turnId?: string | null; status?: string }): WorkStatus {
  const statuses = items.map(item => item.turnStatus)
  if (statuses.includes('failed')) return 'failed'
  if (statuses.includes('interrupted')) return 'stopped'
  if (statuses.some(status => ['waiting', 'waitingForInput', 'waiting_for_input', 'paused'].includes(status ?? ''))) return 'waiting'
  if (statuses.length && statuses.every(status => status === 'completed')) return 'completed'

  const turnId = items[0]?.turnId
  if (turnId && turnId === activeTurn?.turnId) {
    if (activeTurn.status === 'running') return 'inProgress'
    if (activeTurn.status === 'failed') return 'failed'
    if (activeTurn.status === 'interrupted') return 'stopped'
    if (activeTurn.status === 'waiting') return 'waiting'
  }
  if (statuses.includes('inProgress')) return 'inProgress'
  return 'unknown'
}

export function groupTranscriptItems(items: AgentItem[], threadId?: string, activeTurn?: { turnId?: string | null; status?: string }): TranscriptEntry[] {
  const result: TranscriptEntry[] = []
  for (let index = 0; index < items.length;) {
    const item = items[index]
    if (!isFoldableWork(item, threadId)) {
      result.push({ kind: 'item', item })
      index += 1
      continue
    }
    const chunk: AgentItem[] = [item]
    let end = index + 1
    while (end < items.length && isFoldableWork(items[end], threadId) &&
      (items[end].turnId ?? '') === (item.turnId ?? '')) {
      chunk.push(items[end])
      end += 1
    }
    const status = workStatus(chunk, activeTurn)
    result.push({
      kind: 'work',
      id: `work:${item.turnId ?? 'unassigned'}:${item.id}`,
      itemIds: chunk.map(entry => entry.id),
      items: chunk,
      completed: status === 'completed',
      status,
    })
    index = end
  }
  return result
}

export function openingUserMessageIds(items: AgentItem[], activeTurnId: string | null): Set<string> {
  const openingByTurn = new Map<string, AgentItem>()
  for (const item of items) {
    if (item.type === 'userMessage' && typeof item.turnId === 'string' && !openingByTurn.has(item.turnId)) {
      openingByTurn.set(item.turnId, item)
    }
  }
  return new Set([...openingByTurn.values()]
    .filter(item => item.turnId !== activeTurnId && item.turnStatus === 'completed')
    .map(item => item.id))
}

export function itemText(item: AgentItem): string {
  if (typeof item.text === 'string') return item.text
  if (item.type === 'userMessage' && Array.isArray(item.content)) {
    return item.content.map((part: any) => {
      if (part?.type === 'text' && typeof part.text === 'string') return part.text
      if ((part?.type === 'image' || part?.type === 'localImage') && typeof part.imageUrl === 'string') return '[Image]'
      if (part?.type === 'image' || part?.type === 'localImage') return '[Image]'
      if (part?.type === 'audio' || part?.type === 'localAudio') return '[Audio]'
      if (part?.type === 'skill' && typeof part.name === 'string') return `Skill: ${part.name}`
      if (part?.type === 'mention' && typeof part.name === 'string') return `@${part.name}`
      return ''
    }).filter(Boolean).join('\n')
  }
  return ''
}

export function projectAcceptedSubmissionMessages<T extends {
  id: string
  threadId: string
  status: string
  document: { text: string }
  attachmentIds: string[]
}>(session: SessionState, threadId: string | null, receipts: readonly T[]): SessionState {
  if (!threadId || session.threadId !== threadId || receipts.length === 0) return session
  const accepted = new Map(receipts
    .filter(receipt => receipt.threadId === threadId && receipt.status === 'accepted' && typeof receipt.document?.text === 'string')
    .map(receipt => [receipt.id, receipt]))
  if (accepted.size === 0) return session

  let changed = false
  const items = session.items.map(item => {
    if (item.type !== 'userMessage' || typeof item.clientId !== 'string') return item
    const receipt = accepted.get(item.clientId)
    if (!receipt) return item
    const prompt = receipt.document.text
    const count = receipt.attachmentIds.length
    const summary = count === 1 ? '1 attachment' : `${count} attachments`
    const text = count ? `${prompt}${prompt.trim() ? '\n\n' : ''}${summary}` : prompt
    changed = true
    return { ...item, text }
  })
  return changed ? { ...session, items } : session
}

export function transcriptTurns(items: AgentItem[]): string[] {
  return [...new Set(items.map(item => item.turnId).filter((id): id is string => typeof id === 'string' && id.length > 0))]
}

export function activeTranscriptTurnIndex(turns: string[], activeTurnId: string | null): number {
  if (!turns.length) return -1
  const activeIndex = activeTurnId ? turns.indexOf(activeTurnId) : -1
  return activeIndex >= 0 ? activeIndex : turns.length - 1
}

export function workSummary(items: AgentItem[]): string {
  const counts = new Map<string, number>()
  for (const item of items) {
    const label = item.type === 'reasoning' ? 'Reasoning'
      : item.type === 'commandExecution' ? 'Commands'
      : item.type === 'fileChange' ? 'File changes'
      : item.type === 'mcpToolCall' ? 'MCP calls'
      : 'Agent activity'
    counts.set(label, (counts.get(label) ?? 0) + 1)
  }
  return [...counts].map(([label, count]) => `${count} ${label.toLowerCase()}`).join(' · ')
}
