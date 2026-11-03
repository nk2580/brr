import { invoke } from '@tauri-apps/api/core'
import type { Thread, ProjectRecord } from './client'
import type { QuestionAnswers, QuestionRequest } from './questions'

const request = <T = unknown>(method: string, params: Record<string, unknown> = {}) =>
  invoke<T>('agents_request', { method, params })

export type AgentProfile = {
  id: string; revision: number; createdAt: number; updatedAt: number
  name: string; description: string; instructions: string; soul: string; model: string; effort: string
}
export type ProfileCandidate = Pick<AgentProfile, 'name' | 'description' | 'instructions' | 'soul' | 'model' | 'effort'>
export type ProfileDraft = { id: string; threadId: string; agentId?: string; candidate: ProfileCandidate; hash: string; status: string }
export type MemoryRecord = { id: string; scope: 'agent' | 'user' | 'project'; scopeId: string; kind: string; content: string; revision: number; sourceIds: string[]; createdAt: number; updatedAt: number; eventId?: number }
export type MemorySource = { sourceId: string; revision: number; threadId: string; agentId?: string; projectId?: string; content: string; active: boolean }
export type LearnedSkill = { id: string; name: string; description: string; content: string; revision: number; scope: string; scopeId: string; sourceIds: string[]; createdAt: number; updatedAt: number }
export type TaskInput = { agentId: string; title: string; instructions: string; acceptanceCriteria: string[]; cwd?: string; projectId?: string; model?: string; effort?: string; dependencies?: string[]; context?: string; sourceThreadId?: string }
export type Task = TaskInput & { id: string; status: string; reason?: string; runId?: string; threadId?: string; result?: string; evidence?: string[]; createdAt: number; updatedAt: number }
export type DelegationNode = { key: string; task: TaskInput; dependsOn: string[] }
export type DelegationProposal = { id: string; parentTaskId: string; hash: string; status: string; tasks: DelegationNode[] }
export type Workflow = { id: string; name: string; agentId: string; expression: string; timezone: string; paused: boolean; nextRunAt?: number; lastRunAt?: number; lastStatus?: string }
export type AgentEvent = { seq: number; kind: string; entityId: string; payload: unknown; createdAt: number }
export type AgentSettings = { maxBackground: number; maxPerAgent: number }
export type Page<T> = { data: T[]; nextCursor: string | null; sources?: MemorySource[] }
export type RuntimeRequest = { id: string | number; method: string; params: Record<string, unknown> }
export type WorkspaceSetupCommand = { cwd?: string; phase: string; name?: string | null; template: string; approved: boolean }
export type { Question, QuestionAnswers, QuestionRequest } from './questions'

export const agents = {
  list: () => request<{ data: AgentProfile[] }>('agents.list'),
  read: (agentId: string) => request<AgentProfile>('agents.read', { agentId }),
  interview: (model: string, effort: string) => request<{ thread: Thread }>('agents.interview', { model, effort }),
  edit: (agentId: string) => request<{ thread: Thread }>('agents.edit', { agentId }),
  drafts: (threadId?: string) => request<{ data: ProfileDraft[] }>('agents.drafts', threadId ? { threadId } : {}),
  approve: (draftId: string, hash: string) => request<{ agent: AgentProfile }>('agents.approve', { draftId, hash }),
  reject: (draftId: string, hash: string) => request('agents.reject', { draftId, hash }),
  sessions: (agentId: string) => request<{ data: Thread[] }>('agents.sessions', { agentId }),
  open: (agentId: string, options: { model?: string; effort?: string; cwd?: string; projectId?: string | null; readOnly?: boolean } = {}) => request<{ thread: Thread }>('agents.open', { agentId, ...options }),
  inspectWorkspace: async (cwd: string) => ({ cwd, ...await request<{ git: boolean; commands: WorkspaceSetupCommand[] }>('agents.workspace.inspect', { cwd }) }),
  memory: (agentId?: string, projectId?: string) => request<Page<MemoryRecord>>('memory.list', { ...(agentId ? { agentId } : {}), ...(projectId ? { projectId } : {}) }),
  searchMemory: (query: string, agentId?: string, projectId?: string) => request<Page<MemoryRecord>>('memory.search', { query, ...(agentId ? { agentId } : {}), ...(projectId ? { projectId } : {}) }),
  reviseMemory: (input: { agentId?: string; projectId?: string; id: string; revision: number; content: string }) => request<MemoryRecord>('memory.revise', input),
  forgetMemory: (id: string, agentId?: string, projectId?: string) => request<{ eventId: number }>('memory.forget', { id, ...(agentId ? { agentId } : {}), ...(projectId ? { projectId } : {}) }),
  undoMemory: (eventId: number) => request('memory.undo', { eventId }),
  skills: (agentId?: string, projectId?: string) => request<{ data: LearnedSkill[] }>('skills.list', { ...(agentId ? { agentId } : {}), ...(projectId ? { projectId } : {}) }),
  revokeSkill: (id: string, agentId?: string, projectId?: string) => request('skills.revoke', { id, ...(agentId ? { agentId } : {}), ...(projectId ? { projectId } : {}) }),
  tasks: (agentId?: string, page: { after?: string; limit?: number } = {}) => request<Page<Task>>('tasks.list', { ...(agentId ? { agentId } : {}), ...(page.after ? { after: page.after } : {}), ...(page.limit ? { limit: page.limit } : {}) }),
  createTask: (input: TaskInput) => request<Task>('tasks.create', input),
  controlTask: (taskId: string, action: 'pause' | 'resume' | 'cancel' | 'retry') => request<Task>('tasks.control', { taskId, action }),
  proposals: (agentId?: string) => request<{ data: DelegationProposal[] }>('tasks.proposals', agentId ? { agentId } : {}),
  approveProposal: (proposalId: string, hash: string) => request<{ data: Task[] }>('tasks.approve', { proposalId, hash }),
  scheduleProposal: (proposalId: string, hash: string, schedule: { name: string; expression: string; timezone: string }) => request<Workflow>('tasks.schedule', { proposalId, hash, ...schedule }),
  workflows: (agentId?: string) => request<{ data: Workflow[] }>('workflows.list', agentId ? { agentId } : {}),
  createWorkflow: (input: { name: string; agentId: string; title: string; instructions: string; acceptanceCriteria: string[]; cwd?: string; projectId?: string; model?: string; effort?: string; expression: string; timezone: string }) => request<Workflow>('workflows.create', input),
  controlWorkflow: (workflowId: string, paused: boolean) => request<Workflow>('workflows.control', { workflowId, paused }),
  settings: () => request<AgentSettings>('agents.settings'),
  updateSettings: (settings: AgentSettings) => request<AgentSettings>('agents.settings.update', settings),
  events: (after?: number, limit?: number) => request<{ data: AgentEvent[] }>('agents.events', { ...(after === undefined ? {} : { after }), ...(limit === undefined ? {} : { limit }) }),
  decisions: () => request<{ data: DelegationProposal[]; runtimeRequests?: RuntimeRequest[]; drafts?: ProfileDraft[]; questions?: QuestionRequest[] }>('agents.decisions'),
  questions: {
    list: (options: { threadId?: string; pendingOnly?: boolean } = {}) => request<{ data: QuestionRequest[] }>('questions.list', options),
    saveDraft: (requestId: string, answers: QuestionAnswers) => request<QuestionRequest>('questions.saveDraft', { requestId, answers }),
    answer: (requestId: string, answers: QuestionAnswers) => request<QuestionRequest>('questions.answer', { requestId, answers }),
    dismiss: (requestId: string) => request<QuestionRequest>('questions.dismiss', { requestId }),
  },
  projects: () => request<{ data: ProjectRecord[] }>('memory.projects'),
}
