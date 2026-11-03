import { invoke } from '@tauri-apps/api/core'

export type TicketStatus = 'todo' | 'in_progress' | 'review' | 'complete'
export type TicketPriority = 'none' | 'low' | 'medium' | 'high' | 'urgent'
export type TicketCriterion = { id: string; text: string; checked: boolean }
export type Ticket = {
  id: string; number: string; projectId: string; title: string; description: string
  acceptanceCriteria: TicketCriterion[]; agentId?: string | null; priority: TicketPriority
  status: TicketStatus; position: number; revision: number; statusRevision: number
  archived: boolean; createdAt: number; updatedAt: number; execution?: TicketRun
}
export type TicketProject = { id: string; name: string; folders: string[]; primaryFolder: string | null; revision: number; model?: string | null; effort?: string | null; permission?: 'read-only' | 'workspace-write' | null; hidden: boolean }
export type TicketAgent = { id: string; name: string; description?: string }
export type TicketComment = { id: string; ticketId: string; body: string; actor: { kind: string; agentId?: string }; createdAt: number }
export type TicketActivity = { id: string; ticketId: string; kind: string; payload: unknown; actor: { kind: string; agentId?: string }; createdAt: number }
export type TicketRun = { taskId: string; status: string; result?: string; evidence?: string[] }
export type TicketDetail = { ticket: Ticket; comments: { data: TicketComment[]; nextCursor?: number | null }; activity: { data: TicketActivity[]; nextCursor?: number | null }; runs: TicketRun[] }
export type TicketTaskInput = { agentId: string; title: string; instructions: string; acceptanceCriteria: string[]; projectId?: string; cwd?: string; model?: string; effort?: string; workspacePermission?: 'read-only' | 'workspace-write'; context?: string; dependencies?: string[] }
export type TicketProposalProjectSnapshot = { id?: string; folders: string[]; primaryFolder?: string; selectedFolder?: string }
export type TicketRunProposal = { id: string; ticketId: string; ticketRevision: number; hash: string; status: string; snapshot: TicketTaskInput; createdAt: number }
export type TicketPage = { data: Ticket[]; nextCursor?: string | null }
export type TicketChanges = Partial<Pick<Ticket, 'title' | 'description' | 'agentId' | 'priority' | 'archived' | 'status'>> & { acceptanceCriteria?: TicketCriterion[] }
const request = <T>(method: string, params: Record<string, unknown> = {}) => invoke<T>('tickets_request', { method, params })
export const ticketOperationId = (): string => crypto.randomUUID()

export const tickets = {
  projects: () => request<{ data: TicketProject[] }>('projects'),
  agents: () => request<{ data: TicketAgent[] }>('agents'),
  list: (params: { projectId: string; cursor?: string; limit?: number; status?: TicketStatus; agentId?: string; priority?: TicketPriority; archived?: boolean }) => request<TicketPage>('list', params),
  read: (ticketId: string, cursors: { commentsCursor?: number; activityCursor?: number } = {}) => request<TicketDetail>('read', { ticketId, ...cursors }),
  create: (input: { projectId: string; title: string; description?: string; acceptanceCriteria?: TicketCriterion[]; agentId?: string; priority?: TicketPriority; operationId?: string }) => request<Ticket>('create', { ...input, operationId: input.operationId ?? ticketOperationId() }),
  update: (ticketId: string, revision: number, changes: TicketChanges, id = ticketOperationId()) => request<Ticket>('update', { ticketId, revision, changes, operationId: id }),
  move: (ticketId: string, revision: number, status: TicketStatus, beforeId?: string, id = ticketOperationId()) => request<Ticket>('move', { ticketId, revision, status, ...(beforeId ? { beforeId } : {}), operationId: id }),
  comment: (ticketId: string, body: string, id = ticketOperationId()) => request<TicketComment>('comment', { ticketId, body, operationId: id }),
  proposeRun: (ticketId: string, revision: number, agentId: string, folder?: string, id = ticketOperationId()) => request<TicketRunProposal>('proposeRun', { ticketId, revision, agentId, ...(folder ? { folder } : {}), operationId: id }),
  proposals: (ticketId?: string) => request<{ data: TicketRunProposal[] }>('proposals', ticketId ? { ticketId } : {}),
  approveRun: (ticketId: string, hash: string, id = ticketOperationId()) => request<unknown>('approveRun', { ticketId, hash, operationId: id }),
  rejectRun: (ticketId: string, hash: string, id = ticketOperationId()) => request<TicketRunProposal>('rejectRun', { ticketId, hash, operationId: id }),
}

export const ticketLabel = (ticket: Pick<Ticket, 'number'>) => /^BRR-/i.test(ticket.number) ? ticket.number : `BRR-${ticket.number}`
export const ticketProposalProjectSnapshot = (context?: string): TicketProposalProjectSnapshot | null => {
  const marker = 'Brr project snapshot: '
  const markerIndex = context?.lastIndexOf(marker) ?? -1
  if (markerIndex < 0) return null
  try {
    let serialized = context!.slice(markerIndex + marker.length).trim()
    if (serialized.startsWith('JSON ')) serialized = serialized.slice('JSON '.length).trim()
    const value: unknown = JSON.parse(serialized)
    if (!value || typeof value !== 'object' || Array.isArray(value)) return null
    const record = value as Record<string, unknown>
    const folders = Array.isArray(record.folders) ? record.folders.filter((folder): folder is string => typeof folder === 'string' && folder.length > 0) : []
    return {
      ...(typeof record.id === 'string' ? { id: record.id } : {}),
      folders,
      ...(typeof record.primaryFolder === 'string' ? { primaryFolder: record.primaryFolder } : {}),
      ...(typeof record.selectedFolder === 'string' ? { selectedFolder: record.selectedFolder } : {}),
    }
  } catch { return null }
}
export const ticketConflict = (error: unknown): Ticket | null => {
  const message = error instanceof Error ? error.message : String(error)
  try {
    const value = JSON.parse(message) as { code?: unknown; current?: Ticket }
    return value.code === 'conflict' && value.current ? value.current : null
  } catch { return null }
}
