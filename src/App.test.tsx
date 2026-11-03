import { act, fireEvent, render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, it, expect, vi } from 'vitest'
import App from './App'

const events = vi.hoisted(() => ({ handler: null as null | ((event: any) => void), handlers: [] as Array<(event: any) => void> }))
const backend = vi.hoisted(() => ({
  desktop: true,
  connect: vi.fn(async () => ({ account: null, models: [], threads: [] })),
  login: vi.fn(async () => ({ loginId: 'login-1' })),
  cancelLogin: vi.fn(async () => ({status:'canceled'})),
  account: vi.fn(async () => null),
  subscribe: vi.fn(async (handler: (event: any) => void) => {
    events.handlers.push(handler)
    events.handler = handler
    return () => {
      events.handlers = events.handlers.filter(candidate => candidate !== handler)
      events.handler = events.handlers.at(-1) ?? null
    }
  }),
  chooseWorkspace: vi.fn(async () => '/project'),
  revealProject: vi.fn(async (_path: string) => undefined),
  projects: vi.fn(async (): Promise<{data: import('./lib/client').ProjectRecord[]}> => ({data: [{id: 'project-main', name: 'brr', folders: ['/project'], primaryFolder: '/project', revision: 1, model: null, effort: null, permission: null, hidden: false}]})),
  createProject: vi.fn(async (input: import('./lib/client').ProjectInput): Promise<import('./lib/client').ProjectRecord> => ({...input, id: 'created-project', revision: 1, hidden: false})),
  saveProject: vi.fn(async (project: import('./lib/client').ProjectRecord) => project),
  deleteProject: vi.fn(async (_projectId: string) => undefined),
  moveChat: vi.fn(async (_threadId: string, _projectId: string | null) => undefined),
  inspectProject: vi.fn(async (_projectId: string | null) => ({commands: []})),
  archiveProject: vi.fn(async (_projectId: string) => ({archivedIds: [] as string[], skipped: [] as {threadId: string; reason: string}[], failed: [] as {threadId: string; error: string}[]})),
  approveWorktree: vi.fn(async () => undefined),
  start: vi.fn(async (projectId: string | null) => ({thread:{id:'thread-1', projectId: projectId ?? undefined, cwd:'/project', preview:'', updatedAt:1}})),
  resume: vi.fn(async (id: string) => ({thread:{id, cwd:'/project', preview:'', updatedAt:1, turns:[]}})),
  send: vi.fn(async (..._args: unknown[]) => ({turn:{id:'turn-1'}})),
  steer: vi.fn(async (..._args: unknown[]) => ({turnId:'turn-1'})),
  respond: vi.fn(async () => ({})),
  interrupt: vi.fn(async () => ({})),
  archive: vi.fn(async (_threadId: string) => ({})),
  logout: vi.fn(async () => ({})),
  threads: vi.fn(async () => []),
}))
const mainProject: import('./lib/client').ProjectRecord = {id: 'project-main', name: 'brr', folders: ['/project'], primaryFolder: '/project', revision: 1, model: null, effort: null, permission: null, hidden: false}
const chatApi = vi.hoisted(() => ({
  assignTask: vi.fn(async ({task}: any) => agentApi.createTask(task)),
  records: [] as Array<any>,
  listDrafts: vi.fn(async () => chatApi.records),
  getDraft: vi.fn(async (id: string) => chatApi.records.find(record => record.id === id)),
  saveDraft: vi.fn(async (draft: any) => {
    const record = { ...draft, revision: (chatApi.records.find(item => item.id === draft.id)?.revision ?? 0) + 1, updatedAt: new Date().toISOString() }
    chatApi.records = [...chatApi.records.filter(item => item.id !== draft.id), record]
    return record
  }),
  promoteDraft: vi.fn(async (id: string, threadId: string) => {
    const record = chatApi.records.find(item => item.id === id)
    const promoted = { ...record, id: threadId, threadId }
    chatApi.records = [...chatApi.records.filter(item => item.id !== id && item.id !== threadId), promoted]
    return promoted
  }),
  submit: vi.fn(async (params: any) => {
    if (params.delivery === 'send') {
      const turn = await backend.send(params.threadId, params.document.text, params.settings.model, params.settings.effort, params.settings.mode)
      return { turn, submission: {}, queued: false }
    }
    if (params.delivery === 'steer') {
      const result = await backend.steer(params.threadId, params.document.text, params.expectedTurnId)
      return { turnId: result.turnId, submission: {}, queued: false }
    }
    return { submission: {}, queued: true }
  }),
  branch: vi.fn(),
  attachments: vi.fn(async () => []),
  queue: vi.fn(async () => ({ entries: [], state: { paused: false } })),
  skills: vi.fn(async () => []),
  references: vi.fn(async () => []),
  resolveReference: vi.fn(async (_scope: any, _id: string, _type: string) => ({root: '/project', path: 'src/main.rs', absolutePath: '/project/src/main.rs'})),
  importFiles: vi.fn(async (_ownerId: string, _files: File[]) => []),
  importPaths: vi.fn(async () => []),
  removeAttachment: vi.fn(async () => undefined),
  pendingBranches: vi.fn(async () => ({data: []})),
  recoverBranch: vi.fn(async (operationId: string) => ({thread: {id: operationId, cwd: '/project', preview: 'Recovered', updatedAt: 2}})),
  recovery: {
    list: vi.fn(async () => ({items: [] as any[]})),
    restore: vi.fn(async (_input: any) => ({target:'draft' as const, draft: null as any, attachments: [], restored: true as const})),
    cancel: vi.fn(async (_input: any) => undefined),
  },
}))
const agentApi = vi.hoisted(() => ({
  list: vi.fn(async () => ({ data: [] })), settings: vi.fn(async () => ({ maxBackground: 4, maxPerAgent: 2 })),
  decisions: vi.fn(async () => ({ data: [] })), drafts: vi.fn(async () => ({ data: [] })),
  interview: vi.fn(), approve: vi.fn(), sessions: vi.fn(async () => ({ data: [] })), open: vi.fn(),
  tasks: vi.fn(async () => ({ data: [] })), createTask: vi.fn(), proposals: vi.fn(async () => ({ data: [] })),
  workflows: vi.fn(async () => ({ data: [] })), memory: vi.fn(async () => ({ data: [] })),
  skills: vi.fn(async () => ({ data: [] })), projects: vi.fn(async () => ({ data: [] })),
  inspectWorkspace: vi.fn(async (cwd: string) => ({ cwd, git: false, commands: [] })),
  questions: {
    list: vi.fn(async (): Promise<{ data: import('./lib/questions').QuestionRequest[] }> => ({ data: [] })), saveDraft: vi.fn(async (_id: string, answers: unknown) => ({ answers })),
    answer: vi.fn(async (_id: string, answers: unknown) => ({ answers })), dismiss: vi.fn(async () => ({})),
  },
}))
vi.mock('./lib/client', () => ({ client: backend, isDesktop: () => backend.desktop, WorktreeSetupRequired: class extends Error { kind = 'worktreeSetupRequired'; constructor(public projectId: string, public commands: unknown[]) { super('Review project setup hooks') } } }))
vi.mock('./lib/chat', () => ({ chat: chatApi }))
vi.mock('./components/ChatEditor', async () => {
  const React = await import('react')
  const ChatEditor = React.forwardRef<any, any>((props, ref) => {
    const input = React.useRef<HTMLTextAreaElement>(null)
    const publish = (text: string) => props.onChange({ version: 1, text, references: [], quotes: [] })
    React.useImperativeHandle(ref, () => ({
      focus: () => input.current?.focus(),
      insertText: (value: string) => publish(`${props.document.text}${props.document.text ? '\n\n' : ''}${value}`),
      insertQuote: (value: string) => publish(`${props.document.text}${props.document.text ? ' ' : ''}“${value}”`),
      openMenu: () => {},
    }), [props.document])
    return <><textarea aria-label="Message" placeholder="Ask anything" ref={input} value={props.document.text} onChange={event => publish(event.target.value)} onKeyDown={event => {
      if (event.key === 'Tab' && event.shiftKey) { event.preventDefault(); props.onTogglePlan() }
      else if (event.key === 'Enter' && (event.ctrlKey || event.metaKey)) { event.preventDefault(); props.onSubmit(true) }
      else if (event.key === 'Enter' && !event.shiftKey && !event.nativeEvent.isComposing) { event.preventDefault(); props.onSubmit(false) }
    }} onPaste={event => { const files = Array.from(event.clipboardData.files); if (files.length) { event.preventDefault(); props.onPasteFiles?.(files) } }} />
      <button type="button" onClick={() => props.onQueryReferences?.('src')}>Find test references</button>
      <button type="button" onClick={() => props.onOpenReference?.('src/main.rs', 'file')}>Open direct test reference</button>
      {props.references.map((reference: any) => <button type="button" key={reference.id} onClick={() => props.onOpenReference?.(reference.id, reference.type)}>Open reference {reference.name}</button>)}
    </>
  })
  return { ChatEditor }
})
vi.mock('./lib/agents', () => ({ agents: agentApi }))
const ticketApi = vi.hoisted(() => ({ projects: vi.fn(async () => ({ data: [{ id: 'project-tickets', name: 'project', folders: ['/project'], primaryFolder: '/project', revision: 1, hidden: false }] })), agents: vi.fn(async () => ({ data: [] })), list: vi.fn(async () => ({ data: [], nextCursor: null })), read: vi.fn(), create: vi.fn(), update: vi.fn(), move: vi.fn(), comment: vi.fn(), proposeRun: vi.fn(), proposals: vi.fn(async () => ({ data: [] })), approveRun: vi.fn(), rejectRun: vi.fn() }))
vi.mock('./lib/tickets', async () => ({ ...(await vi.importActual('./lib/tickets')), tickets: ticketApi }))
const uiApi = vi.hoisted(() => ({task: null as null | {id: string; agentId: string; projectId: string; cwd: string}, claim: vi.fn(async () => ({reviewId: 'review-1', threadId: 'ui-chat', callId: 'ui-call', documentHash: 'hash', text: 'Compare the selected inputs'}))}))
vi.mock('./lib/intelligent-ui-actions', () => ({claimUiAction: uiApi.claim}))
vi.mock('./components/IntelligentUi', () => ({IntelligentUi: ({identity, onReview, onExpand}: any) => <div><button onClick={() => onReview?.({reviewId: 'review-1', threadId: identity.threadId, callId: identity.callId, documentHash: 'hash', text: 'Compare the selected inputs', ...(uiApi.task ? {task: uiApi.task} : {})})}>Ask about selections</button>{onExpand && <button onClick={onExpand}>Expand generated response</button>}</div>}))

async function selectProject(projectId = 'project-main') {
  await userEvent.selectOptions(await screen.findByRole('combobox', {name: 'Project'}), projectId)
}

async function openProfile() {
  await userEvent.click(await screen.findByRole('button', { name: 'Profile' }))
  return screen.findByRole('region', { name: 'User profile' })
}

async function openChatOptions() {
  await userEvent.click(await screen.findByRole('button', { name: 'Chat options' }))
  return screen.findByRole('dialog', { name: 'Chat options' })
}

describe('desktop workspace', () => {
  it('opens a requested initial chat without creating a thread or sending a message', async () => {
    const thread = {id: 'preview-writing', cwd: '/project', preview: 'Workshop outline', updatedAt: 1}
    backend.connect.mockResolvedValueOnce({account: null, models: [], threads: [thread]} as never)
    backend.resume.mockResolvedValueOnce({thread: {...thread, turns: []}})
    render(<App initialThreadId="preview-writing" />)
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('preview-writing'))
    expect(backend.start).not.toHaveBeenCalled()
    expect(backend.send).not.toHaveBeenCalled()
    expect(chatApi.submit).not.toHaveBeenCalled()
  })

  it('does not replace a newer draft with an initial chat when connection finishes late', async () => {
    let connected!: (value: any) => void
    backend.connect.mockImplementationOnce(() => new Promise(resolve => { connected = resolve }))
    render(<App initialThreadId="preview-writing" />)
    await userEvent.click(screen.getByRole('button', {name: 'New conversation'}))
    await userEvent.type(screen.getByRole('textbox', {name: 'Message'}), 'Keep my new draft')
    await act(async () => connected({account: null, models: [], threads: [{id: 'preview-writing', cwd: '/project', preview: 'Workshop outline', updatedAt: 1}]}))
    expect(backend.resume).not.toHaveBeenCalled()
    expect(screen.getByRole('textbox', {name: 'Message'})).toHaveValue('Keep my new draft')
  })

  it('keeps text typed before connecting instead of opening the initial chat', async () => {
    let connected!: (value: any) => void
    backend.connect.mockImplementationOnce(() => new Promise(resolve => { connected = resolve }))
    render(<App initialThreadId="preview-writing" />)
    await userEvent.type(screen.getByRole('textbox', {name: 'Message'}), 'Already writing here')
    await act(async () => connected({account: null, models: [], threads: [{id: 'preview-writing', cwd: '/project', preview: 'Workshop outline', updatedAt: 1}]}))
    expect(backend.resume).not.toHaveBeenCalled()
    expect(screen.getByRole('textbox', {name: 'Message'})).toHaveValue('Already writing here')
  })

  beforeEach(() => { localStorage.clear(); vi.clearAllMocks(); chatApi.recovery.list.mockResolvedValue({items: []}); chatApi.recovery.restore.mockResolvedValue({target:'draft',draft:null,attachments:[],restored:true} as never); chatApi.pendingBranches.mockResolvedValue({data: []} as never); backend.projects.mockResolvedValue({data: [mainProject]}); backend.threads.mockResolvedValue([]); chatApi.records = []; uiApi.task = null; agentApi.questions.list.mockResolvedValue({ data: [] }); ticketApi.projects.mockResolvedValue({ data: [{ id: 'project-tickets', name: 'project', folders: ['/project'], primaryFolder: '/project', revision: 1, hidden: false }] }); ticketApi.agents.mockResolvedValue({ data: [] }); ticketApi.list.mockResolvedValue({ data: [], nextCursor: null }); ticketApi.proposals.mockResolvedValue({ data: [] }) })
  it('lists saved messages after a cold reload without opening the unavailable runtime chat', async () => {
    const saved = {sourceId:'submission-cold',sourceKind:'submission',threadId:'missing-thread',kind:'baseline',projectId:'project-main',agentId:'',status:'queued',text:'Keep this unsent request',settings:{model:'gpt-5.6',effort:'low',mode:'default'},attachmentIds:[],createdAt:'2026-11-03T00:00:00Z',revision:2,restorable:true}
    chatApi.recovery.list.mockResolvedValue({items:[saved]})
    render(<App />)
    await screen.findByRole('textbox', {name:'Message'})
    const starts = backend.start.mock.calls.length
    const resumes = backend.resume.mock.calls.length

    await userEvent.click(screen.getByRole('button', {name:'Saved messages'}))

    expect(await screen.findByText('Keep this unsent request')).toBeInTheDocument()
    expect(screen.getByText(/Saved messages — runtime chat unavailable/)).toBeInTheDocument()
    expect(chatApi.recovery.list).toHaveBeenCalled()
    expect(backend.start).toHaveBeenCalledTimes(starts)
    expect(backend.resume).toHaveBeenCalledTimes(resumes)
  })

  it('restores a definite failed message into a saved draft without dispatching it', async () => {
    const saved = {sourceId:'submission-failed',sourceKind:'submission',threadId:'missing-thread',kind:'baseline',projectId:'project-main',agentId:'',status:'failed',text:'Try this again later',settings:{model:'gpt-5.6',effort:'low',mode:'default'},attachmentIds:[],createdAt:'2026-11-03T00:00:00Z',revision:4,restorable:true}
    const restoredDraft = {id:'restored-draft',threadId:null,projectId:'project-main',agentId:null,document:{version:1,text:'Try this again later',references:[],quotes:[]},settings:saved.settings,revision:1,updatedAt:'2026-11-03T00:01:00Z'}
    chatApi.recovery.list.mockResolvedValue({items:[saved]})
    chatApi.recovery.restore.mockImplementation(async ({operationId}: any) => {
      chatApi.records = [restoredDraft]
      return {target:'draft',draft:restoredDraft,attachments:[],restored:true,operationId}
    })
    render(<App />)
    await screen.findByRole('textbox', {name:'Message'})
    const starts = backend.start.mock.calls.length
    const sends = backend.send.mock.calls.length
    await userEvent.click(screen.getByRole('button', {name:'Saved messages'}))
    await userEvent.click(await screen.findByRole('button', {name:'Restore'}))

    expect(chatApi.recovery.restore).toHaveBeenCalledWith(expect.objectContaining({sourceId:'submission-failed',sourceKind:'submission',operationId:expect.stringMatching(/^[0-9a-f-]{36}$/i)}))
    expect(await screen.findByRole('textbox', {name:'Message'})).toHaveValue('Try this again later')
    expect(backend.start).toHaveBeenCalledTimes(starts)
    expect(backend.send).toHaveBeenCalledTimes(sends)
  })

  it('keeps uncertain saved messages read-only and never dispatches, restores, or cancels them', async () => {
    const saved = {sourceId:'submission-uncertain',sourceKind:'submission',threadId:'missing-thread',kind:'baseline',projectId:'project-main',agentId:'',status:'uncertain',text:'Delivery could not be confirmed',settings:{model:'gpt-5.6',effort:'low',mode:'default'},attachmentIds:[],createdAt:'2026-11-03T00:00:00Z',revision:1,restorable:false,reason:'Delivery status is unknown'}
    chatApi.recovery.list.mockResolvedValue({items:[saved]})
    render(<App />)
    await screen.findByRole('textbox', {name:'Message'})
    const starts = backend.start.mock.calls.length
    const resumes = backend.resume.mock.calls.length
    const sends = backend.send.mock.calls.length
    await userEvent.click(screen.getByRole('button', {name:'Saved messages'}))

    expect(await screen.findByText('Delivery could not be confirmed')).toBeInTheDocument()
    expect(screen.getByText(/Delivery status is unknown/)).toBeInTheDocument()
    expect(screen.queryByRole('button', {name:/Restore/})).not.toBeInTheDocument()
    expect(screen.queryByRole('button', {name:/Cancel/})).not.toBeInTheDocument()
    expect(chatApi.recovery.restore).not.toHaveBeenCalled()
    expect(chatApi.recovery.cancel).not.toHaveBeenCalled()
    expect(backend.start).toHaveBeenCalledTimes(starts)
    expect(backend.resume).toHaveBeenCalledTimes(resumes)
    expect(backend.send).toHaveBeenCalledTimes(sends)
  })

  it('preserves the active unsent draft when switching into and back out of saved messages', async () => {
    render(<App />)
    const message = await screen.findByRole('textbox', {name:'Message'})
    await userEvent.type(message, 'Keep my working draft')

    await userEvent.click(screen.getByRole('button', {name:'Saved messages'}))
    expect(await screen.findByRole('heading', {name:/Saved messages — runtime chat unavailable/})).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', {name:'Home'}))

    expect(screen.getByRole('textbox', {name:'Message'})).toHaveValue('Keep my working draft')
  })

  it('discovers and resumes a saved edit branch when its empty thread is absent from runtime listing', async () => {
    const branchThread = {id:'branch-edit-1',projectId:'project-main',cwd:'/branch/worktree',preview:'',updatedAt:5,
      workspace:{cwd:'/branch/worktree',roots:['/branch/worktree'],folders:[{source:'/project',path:'/branch/worktree'}],worktrees:[{projectPath:'/project',path:'/branch/worktree',branch:'codex/edit',cwd:'/branch/worktree'}],scopeProjectId:'project-main',projectRevision:1},
      brr:{agentId:'agent-edit'}}
    const branchDraft = {id:'branch-edit-1',threadId:'branch-edit-1',projectId:'project-main',agentId:'agent-edit',document:{version:1,text:'Continue editing this request',references:[],quotes:[],attachmentIds:[]},settings:{model:'gpt-6.1-sol',modelId:'sol',effort:'high',mode:'plan'},revision:2,updatedAt:'2026-11-03T00:00:00Z',branchEdit:true,branchOperationId:'branch-operation-1',branchThread}
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt',email:'dev@example.com',planType:'plus'},models:[{id:'sol',model:'gpt-6.1-sol',displayName:'GPT Sol',isDefault:true,defaultReasoningEffort:'medium',supportedReasoningEfforts:[{reasoningEffort:'high',description:''}]}],threads:[]} as never)
    backend.resume.mockResolvedValueOnce({thread:branchThread} as never)
    agentApi.list.mockResolvedValueOnce({data:[{id:'agent-edit',revision:1,createdAt:1,updatedAt:1,name:'Mina',description:'',instructions:'',soul:'',model:'gpt-6.1-sol',effort:'high'}]} as never)
    chatApi.records = [branchDraft]
    render(<App />)

    await userEvent.click(await screen.findByRole('button',{name:'Edit branch draft Continue editing this request'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('branch-edit-1'))
    await waitFor(() => expect(screen.queryByRole('button',{name:'Edit branch draft Continue editing this request'})).not.toBeInTheDocument())
    expect(backend.resume).toHaveBeenCalledTimes(1)
    expect(backend.start).not.toHaveBeenCalled()
    expect(backend.send).not.toHaveBeenCalled()
    expect(chatApi.submit).not.toHaveBeenCalled()
    expect(await screen.findByRole('textbox',{name:'Message'})).toHaveValue('Continue editing this request')
    await userEvent.click(await screen.findByRole('button',{name:'Chat options'}))
    expect(await screen.findByRole('combobox',{name:'Move to project'})).toHaveValue('project-main')
    expect(screen.getByRole('combobox',{name:'Chat identity'})).toHaveValue('agent-edit')
    expect(screen.getByRole('combobox',{name:'Reasoning effort'})).toHaveValue('high')

    await userEvent.type(screen.getByRole('textbox',{name:'Message'}),' more')
    await waitFor(() => expect(chatApi.records.find(record=>record.id==='branch-edit-1')).toMatchObject({threadId:'branch-edit-1',projectId:'project-main',agentId:'agent-edit',document:{text:'Continue editing this request more'}}))
  })

  it('does not resurrect an edit branch draft after archiving its empty runtime chat', async () => {
    const branchThread = {id:'branch-edit-archive',projectId:'project-main',cwd:'/branch/worktree',preview:'',updatedAt:5,
      workspace:{cwd:'/branch/worktree',roots:['/branch/worktree'],folders:[{source:'/project',path:'/branch/worktree'}],worktrees:[{projectPath:'/project',path:'/branch/worktree',branch:'codex/edit',cwd:'/branch/worktree'}],scopeProjectId:'project-main',projectRevision:1},
      brr:{agentId:'agent-edit'}}
    const branchDraft = {id:branchThread.id,threadId:branchThread.id,projectId:'project-main',agentId:'agent-edit',document:{version:1,text:'Keep this branch archived',references:[],quotes:[],attachmentIds:[]},settings:{model:'gpt-6.1-sol',modelId:'sol',effort:'high',mode:'plan'},revision:2,updatedAt:'2026-11-03T00:00:00Z',branchEdit:true,branchOperationId:'branch-operation-archive',branchThread}
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt',email:'dev@example.com',planType:'plus'},models:[{id:'sol',model:'gpt-6.1-sol',displayName:'GPT Sol',isDefault:true,defaultReasoningEffort:'medium',supportedReasoningEfforts:[{reasoningEffort:'high',description:''}]}],threads:[]} as never)
    backend.resume.mockResolvedValueOnce({thread:branchThread} as never)
    backend.archive.mockImplementationOnce(async (id: string) => {
      expect(id).toBe(branchThread.id)
      return {}
    })
    agentApi.list.mockResolvedValueOnce({data:[{id:'agent-edit',revision:1,createdAt:1,updatedAt:1,name:'Mina',description:'',instructions:'',soul:'',model:'gpt-6.1-sol',effort:'high'}]} as never)
    chatApi.records = [branchDraft]
    render(<App />)

    await userEvent.click(await screen.findByRole('button',{name:'Edit branch draft Keep this branch archived'}))
    await screen.findByRole('textbox',{name:'Message'})
    await userEvent.click(await screen.findByRole('button',{name:'Archive'}))

    await waitFor(() => expect(backend.archive).toHaveBeenCalledWith(branchThread.id))
    await waitFor(() => expect(screen.queryByRole('button',{name:'Edit branch draft Keep this branch archived'})).not.toBeInTheDocument())
    expect(chatApi.listDrafts.mock.calls.length).toBeGreaterThanOrEqual(2)
  })

  it('keeps an archived edit branch hidden when refreshing drafts fails', async () => {
    const branchThread = {id:'branch-edit-archive-offline',projectId:'project-main',cwd:'/branch/worktree',preview:'',updatedAt:5,
      workspace:{cwd:'/branch/worktree',roots:['/branch/worktree'],folders:[{source:'/project',path:'/branch/worktree'}],worktrees:[{projectPath:'/project',path:'/branch/worktree',branch:'codex/edit',cwd:'/branch/worktree'}],scopeProjectId:'project-main',projectRevision:1},
      brr:{agentId:'agent-edit'}}
    const branchDraft = {id:branchThread.id,threadId:branchThread.id,projectId:'project-main',agentId:'agent-edit',document:{version:1,text:'Archive stays hidden offline',references:[],quotes:[],attachmentIds:[]},settings:{model:'gpt-6.1-sol',modelId:'sol',effort:'high',mode:'plan'},revision:2,updatedAt:'2026-11-03T00:00:00Z',branchEdit:true,branchOperationId:'branch-operation-archive-offline',branchThread}
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt',email:'dev@example.com',planType:'plus'},models:[{id:'sol',model:'gpt-6.1-sol',displayName:'GPT Sol',isDefault:true,defaultReasoningEffort:'medium',supportedReasoningEfforts:[{reasoningEffort:'high',description:''}]}],threads:[]} as never)
    backend.resume.mockResolvedValueOnce({thread:branchThread} as never)
    backend.archive.mockImplementationOnce(async (id: string) => {
      expect(id).toBe(branchThread.id)
      chatApi.listDrafts.mockRejectedValueOnce(new Error('Draft store is reconnecting'))
      return {}
    })
    agentApi.list.mockResolvedValueOnce({data:[{id:'agent-edit',revision:1,createdAt:1,updatedAt:1,name:'Mina',description:'',instructions:'',soul:'',model:'gpt-6.1-sol',effort:'high'}]} as never)
    chatApi.records = [branchDraft]
    render(<App />)

    await userEvent.click(await screen.findByRole('button',{name:'Edit branch draft Archive stays hidden offline'}))
    await screen.findByRole('textbox',{name:'Message'})
    await userEvent.click(await screen.findByRole('button',{name:'Archive'}))

    await waitFor(() => expect(backend.archive).toHaveBeenCalledWith(branchThread.id))
    await waitFor(() => expect(screen.queryByRole('button',{name:'Edit branch draft Archive stays hidden offline'})).not.toBeInTheDocument())
    expect(chatApi.listDrafts.mock.calls.length).toBeGreaterThanOrEqual(2)
  })

  it('starts a fresh durable draft after archiving an edit branch', async () => {
    const branchThread = {id:'branch-edit-new-draft',projectId:'project-main',cwd:'/branch/worktree',preview:'',updatedAt:5,
      workspace:{cwd:'/branch/worktree',roots:['/branch/worktree'],folders:[{source:'/project',path:'/branch/worktree'}],worktrees:[{projectPath:'/project',path:'/branch/worktree',branch:'codex/edit',cwd:'/branch/worktree'}],scopeProjectId:'project-main',projectRevision:1},
      brr:{agentId:'agent-edit'}}
    const branchDraft = {id:branchThread.id,threadId:branchThread.id,projectId:'project-main',agentId:'agent-edit',document:{version:1,text:'Keep archived request unchanged',references:[],quotes:[],attachmentIds:[]},settings:{model:'gpt-6.1-sol',modelId:'sol',effort:'high',mode:'plan'},revision:2,updatedAt:'2026-11-03T00:00:00Z',branchEdit:true,branchOperationId:'branch-operation-new-draft',branchThread}
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt',email:'dev@example.com',planType:'plus'},models:[{id:'sol',model:'gpt-6.1-sol',displayName:'GPT Sol',isDefault:true,defaultReasoningEffort:'medium',supportedReasoningEfforts:[{reasoningEffort:'high',description:''}]}],threads:[]} as never)
    backend.resume.mockResolvedValueOnce({thread:branchThread} as never)
    agentApi.list.mockResolvedValueOnce({data:[{id:'agent-edit',revision:1,createdAt:1,updatedAt:1,name:'Mina',description:'',instructions:'',soul:'',model:'gpt-6.1-sol',effort:'high'}]} as never)
    chatApi.records = [branchDraft]
    render(<App />)

    await userEvent.click(await screen.findByRole('button',{name:'Edit branch draft Keep archived request unchanged'}))
    await screen.findByRole('textbox',{name:'Message'})
    await userEvent.click(await screen.findByRole('button',{name:'Archive'}))
    await waitFor(() => expect(backend.archive).toHaveBeenCalledWith(branchThread.id))
    await waitFor(() => expect(chatApi.listDrafts.mock.calls.length).toBeGreaterThanOrEqual(2))

    const archivedBeforeTyping = chatApi.records.find(record => record.id === branchThread.id)
    await userEvent.type(await screen.findByRole('textbox',{name:'Message'}),'A separate new request')

    await waitFor(() => expect(chatApi.records.some(record => record.id !== branchThread.id && record.document.text === 'A separate new request')).toBe(true))
    expect(chatApi.records.find(record => record.id === branchThread.id)).toEqual(archivedBeforeTyping)
    expect(screen.queryByRole('button',{name:'Edit branch draft Keep archived request unchanged'})).not.toBeInTheDocument()
  })

  it('does not let an archive finishing after navigation clear the new conversation', async () => {
    const branchThread = {id:'branch-edit-archive-delayed',projectId:'project-main',cwd:'/branch/worktree',preview:'',updatedAt:5,
      workspace:{cwd:'/branch/worktree',roots:['/branch/worktree'],folders:[{source:'/project',path:'/branch/worktree'}],worktrees:[{projectPath:'/project',path:'/branch/worktree',branch:'codex/edit',cwd:'/branch/worktree'}],scopeProjectId:'project-main',projectRevision:1},
      brr:{agentId:'agent-edit'}}
    const branchDraft = {id:branchThread.id,threadId:branchThread.id,projectId:'project-main',agentId:'agent-edit',document:{version:1,text:'Archive finishes later',references:[],quotes:[],attachmentIds:[]},settings:{model:'gpt-6.1-sol',modelId:'sol',effort:'high',mode:'plan'},revision:2,updatedAt:'2026-11-03T00:00:00Z',branchEdit:true,branchOperationId:'branch-operation-archive-delayed',branchThread}
    let finishArchive!: () => void
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt',email:'dev@example.com',planType:'plus'},models:[{id:'sol',model:'gpt-6.1-sol',displayName:'GPT Sol',isDefault:true,defaultReasoningEffort:'medium',supportedReasoningEfforts:[{reasoningEffort:'high',description:''}]}],threads:[]} as never)
    backend.resume.mockResolvedValueOnce({thread:branchThread} as never)
    backend.archive.mockImplementationOnce((id: string) => {
      expect(id).toBe(branchThread.id)
      return new Promise(resolve => { finishArchive = () => resolve({}) }) as never
    })
    agentApi.list.mockResolvedValueOnce({data:[{id:'agent-edit',revision:1,createdAt:1,updatedAt:1,name:'Mina',description:'',instructions:'',soul:'',model:'gpt-6.1-sol',effort:'high'}]} as never)
    chatApi.records = [branchDraft]
    render(<App />)

    await userEvent.click(await screen.findByRole('button',{name:'Edit branch draft Archive finishes later'}))
    await screen.findByRole('textbox',{name:'Message'})
    await userEvent.click(await screen.findByRole('button',{name:'Archive'}))
    await waitFor(() => expect(backend.archive).toHaveBeenCalledWith(branchThread.id))
    await userEvent.click(screen.getByRole('button',{name:'New conversation'}))
    await userEvent.type(await screen.findByRole('textbox',{name:'Message'}),'Keep this newer conversation')

    await act(async () => finishArchive())

    expect(screen.getByRole('textbox',{name:'Message'})).toHaveValue('Keep this newer conversation')
    expect(screen.queryByRole('button',{name:'Edit branch draft Archive finishes later'})).not.toBeInTheDocument()
  })

  it('does not duplicate a branch edit draft already present in the complete thread list', async () => {
    const branchThread = {id:'branch-edit-visible',projectId:null,cwd:'/branch/worktree',preview:'Edit branch',updatedAt:5}
    const branchDraft = {id:branchThread.id,threadId:branchThread.id,projectId:null,agentId:null,document:{version:1,text:'Resume this branch',references:[],quotes:[],attachmentIds:[]},settings:{model:'',effort:'medium',mode:'default'},revision:1,updatedAt:'2026-11-03T00:00:00Z',branchEdit:true,branchOperationId:'branch-operation-visible',branchThread}
    backend.connect.mockResolvedValueOnce({account:null,models:[],threads:[branchThread]} as never)
    chatApi.records = [branchDraft]
    render(<App />)
    await screen.findByRole('button',{name:'Edit branch'})
    expect(screen.queryByRole('button',{name:'Edit branch draft Resume this branch'})).not.toBeInTheDocument()
  })

  it('cancels only queued submissions using the listed revision guard', async () => {
    const queued = {sourceId:'submission-queued',sourceKind:'submission',threadId:'missing-thread',kind:'baseline',projectId:'project-main',agentId:'',status:'queued',text:'Queued message',settings:{model:'gpt-5.6',effort:'low',mode:'default'},attachmentIds:[],createdAt:'2026-11-03T00:00:00Z',revision:7,restorable:true}
    chatApi.recovery.list.mockResolvedValue({items:[queued]})
    render(<App />)
    await screen.findByRole('textbox', {name:'Message'})
    await userEvent.click(screen.getByRole('button', {name:'Saved messages'}))
    await userEvent.click(await screen.findByRole('button', {name:'Cancel'}))

    expect(chatApi.recovery.cancel).toHaveBeenCalledWith({submissionId:'submission-queued',expectedRevision:7})
    expect(backend.start).not.toHaveBeenCalled()
    expect(backend.resume).not.toHaveBeenCalled()
    expect(backend.send).not.toHaveBeenCalled()
  })

  it('closes recovery when ordinary navigation starts a new conversation', async () => {
    render(<App />)
    await screen.findByRole('textbox', {name:'Message'})
    await userEvent.click(screen.getByRole('button', {name:'Saved messages'}))
    await screen.findByRole('heading', {name:/Saved messages — runtime chat unavailable/})
    await userEvent.click(screen.getByRole('button', {name:'New conversation'}))

    expect(await screen.findByRole('textbox', {name:'Message'})).toBeInTheDocument()
    expect(screen.queryByRole('heading', {name:/Saved messages — runtime chat unavailable/})).not.toBeInTheDocument()
  })

  it('does not switch drafts when a restore completes after navigating away', async () => {
    const queued = {sourceId:'submission-delayed',sourceKind:'submission',threadId:'missing-thread',kind:'baseline',projectId:'project-main',agentId:'',status:'queued',text:'Do not take over after navigation',settings:{model:'gpt-5.6',effort:'low',mode:'default'},attachmentIds:[],createdAt:'2026-11-03T00:00:00Z',revision:1,restorable:true}
    const restoredDraft = {id:'late-restored-draft',threadId:null,projectId:'project-main',agentId:null,document:{version:1,text:queued.text,references:[],quotes:[]},settings:queued.settings,revision:1,updatedAt:'2026-11-03T00:01:00Z'}
    let finishRestore!: (value: any) => void
    chatApi.recovery.list.mockResolvedValue({items:[queued]})
    chatApi.recovery.restore.mockImplementationOnce(() => new Promise(resolve => { finishRestore = resolve }) as never)
    render(<App />)
    await screen.findByRole('textbox', {name:'Message'})
    await userEvent.click(screen.getByRole('button', {name:'Saved messages'}))
    await userEvent.click(await screen.findByRole('button', {name:'Restore'}))
    await userEvent.click(screen.getByRole('button', {name:'Home'}))
    await screen.findByRole('textbox', {name:'Message'})
    await act(async () => { finishRestore({target:'draft',draft:restoredDraft,attachments:[],restored:true}) })

    expect(screen.getByRole('textbox', {name:'Message'})).toHaveValue('')
    expect(chatApi.getDraft).not.toHaveBeenCalledWith('late-restored-draft')
    expect(backend.start).not.toHaveBeenCalled()
    expect(backend.send).not.toHaveBeenCalled()
  })

  it('allows restoring a host-marked recoverable promoted draft into a fresh draft', async () => {
    const saved = {sourceId:'promoted-source',sourceKind:'draft',threadId:'missing-thread',kind:'baseline',projectId:'project-main',agentId:'',status:'draft',text:'Preserve this promoted request',settings:{model:'gpt-5.6',effort:'low',mode:'default'},attachmentIds:[],createdAt:'2026-11-03T00:00:00Z',revision:3,restorable:true}
    const restoredDraft = {id:'restored-promoted',threadId:null,projectId:'project-main',agentId:null,document:{version:1,text:saved.text,references:[],quotes:[]},settings:saved.settings,revision:1,updatedAt:'2026-11-03T00:01:00Z'}
    chatApi.recovery.list.mockResolvedValue({items:[saved]})
    chatApi.recovery.restore.mockImplementation(async ({operationId}: any) => {
      chatApi.records = [restoredDraft]
      return {target:'draft',draft:restoredDraft,attachments:[],restored:true,operationId}
    })
    render(<App />)
    await screen.findByRole('textbox', {name:'Message'})
    await userEvent.click(screen.getByRole('button', {name:'Saved messages'}))
    await userEvent.click(await screen.findByRole('button', {name:'Restore'}))

    expect(chatApi.recovery.restore).toHaveBeenCalledWith(expect.objectContaining({sourceId:'promoted-source',sourceKind:'draft'}))
    expect(await screen.findByRole('textbox', {name:'Message'})).toHaveValue(saved.text)
    expect(backend.start).not.toHaveBeenCalled()
    expect(backend.send).not.toHaveBeenCalled()
  })

  it('reuses the same restore operation id when durable browser storage is unavailable', async () => {
    const queued = {sourceId:'submission-storage-fallback',sourceKind:'submission',threadId:'missing-thread',kind:'baseline',projectId:'project-main',agentId:'',status:'failed',text:'Retry with the same operation id',settings:{model:'gpt-5.6',effort:'low',mode:'default'},attachmentIds:[],createdAt:'2026-11-03T00:00:00Z',revision:1,restorable:true}
    const restoredDraft = {id:'storage-fallback-draft',threadId:null,projectId:'project-main',agentId:null,document:{version:1,text:queued.text,references:[],quotes:[]},settings:queued.settings,revision:1,updatedAt:'2026-11-03T00:01:00Z'}
    chatApi.recovery.list.mockResolvedValue({items:[queued]})
    chatApi.recovery.restore.mockRejectedValueOnce(new Error('Temporary storage error')).mockImplementationOnce(async () => {
      chatApi.records = [restoredDraft]
      return {target:'draft',draft:restoredDraft,attachments:[],restored:true}
    })
    const setItem = vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => { throw new Error('Storage disabled') })
    try {
      render(<App />)
      await screen.findByRole('textbox', {name:'Message'})
      await userEvent.click(screen.getByRole('button', {name:'Saved messages'}))
      await userEvent.click(await screen.findByRole('button', {name:'Restore'}))
      expect(await screen.findByText(/Could not restore saved message/)).toBeInTheDocument()
      await userEvent.click(screen.getByRole('button', {name:'Restore'}))

      const first = chatApi.recovery.restore.mock.calls[0][0]
      const second = chatApi.recovery.restore.mock.calls[1][0]
      expect(first.operationId).toBe(second.operationId)
      expect(await screen.findByRole('textbox', {name:'Message'})).toHaveValue(queued.text)
    } finally { setItem.mockRestore() }
  })
  it('does not adopt a replayed restore after its saved draft has already promoted to a chat', async () => {
    const queued = {sourceId:'submission-replayed',sourceKind:'submission',threadId:'missing-thread',kind:'baseline',projectId:'project-main',agentId:'',status:'failed',text:'Already restored into a chat',settings:{model:'gpt-5.6',effort:'low',mode:'default'},attachmentIds:[],createdAt:'2026-11-03T00:00:00Z',revision:2,restorable:true}
    chatApi.recovery.list.mockResolvedValue({items:[queued]})
    chatApi.recovery.restore.mockResolvedValue({restored:true,target:'chat',threadId:'promoted-thread'} as never)
    render(<App />)
    await screen.findByRole('textbox', {name:'Message'})
    await userEvent.click(screen.getByRole('button', {name:'Saved messages'}))
    await userEvent.click(await screen.findByRole('button', {name:'Restore'}))

    expect(await screen.findByText(/already restored to chat/)).toBeInTheDocument()
    expect(screen.getByRole('heading', {name:/Saved messages — runtime chat unavailable/})).toBeInTheDocument()
    expect(chatApi.getDraft).not.toHaveBeenCalled()
    expect(backend.start).not.toHaveBeenCalled()
    expect(backend.resume).not.toHaveBeenCalled()
    expect(backend.send).not.toHaveBeenCalled()
  })
  it('keeps independent unsent drafts when starting a new conversation and reopening the saved draft', async () => {
    render(<App />)
    const message = await screen.findByRole('textbox', { name: 'Message' })
    await userEvent.type(message, 'Draft A stays here')
    await userEvent.click(screen.getByRole('button', { name: 'New conversation' }))
    await userEvent.type(screen.getByRole('textbox', { name: 'Message' }), 'Draft B starts empty')
    await waitFor(() => expect(chatApi.records.length).toBeGreaterThanOrEqual(2))
    // The first key must keep the semantic content even after a second editor mounts.
    expect(chatApi.records.map(record => record.document.text)).toContain('Draft A stays here')
    const savedA = chatApi.records.find(record => record.document.text === 'Draft A stays here')
    expect(savedA).toBeTruthy()
    await userEvent.click(await screen.findByRole('button', { name: /Draft A stays here/ }))
    expect(screen.getByRole('textbox', { name: 'Message' })).toHaveValue('Draft A stays here')
  })

  it('does not carry an unsent chat A draft into a first visit to chat B', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt',email:'dev@example.com',planType:'plus'},models:[{id:'sol',model:'gpt-6.1-sol',displayName:'GPT Sol',isDefault:true,defaultReasoningEffort:'medium',supportedReasoningEfforts:[]}],threads:[
      {id:'thread-a',cwd:'/project',preview:'Chat A',updatedAt:2},
      {id:'thread-b',cwd:'/project',preview:'Chat B',updatedAt:1},
    ]} as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button',{name:'Chat A'}))
    const composer = await screen.findByRole('textbox',{name:'Message'})
    await userEvent.type(composer,'FILE-CONTEXT-76: quote from chat A')
    await waitFor(() => expect(chatApi.records.some(record=>record.id==='thread-a'&&record.document.text.includes('FILE-CONTEXT-76'))).toBe(true))

    await userEvent.click(screen.getByRole('button',{name:'Chat B'}))
    expect(await screen.findByRole('textbox',{name:'Message'})).toHaveValue('')

    await userEvent.click(screen.getByRole('button',{name:'Chat A'}))
    expect(await screen.findByRole('textbox',{name:'Message'})).toHaveValue('FILE-CONTEXT-76: quote from chat A')
  })

  it('shows shared-folder scope from saved workspace roots but not worktrees or scratch chats', async () => {
    const base = {scopeProjectId:'project-main',projectRevision:1}
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt',email:'dev@example.com',planType:'plus'},models:[{id:'sol',model:'gpt-6.1-sol',displayName:'GPT Sol',isDefault:true,defaultReasoningEffort:'medium',supportedReasoningEfforts:[]}],threads:[
      {id:'shared-thread',cwd:'/shared/Docs',preview:'Shared docs',updatedAt:3,workspace:{...base,cwd:'/shared/Docs',roots:['/shared/Docs'],folders:[{source:'/shared/Docs',path:'/shared/Docs'}],worktrees:[]}},
      {id:'worktree-thread',cwd:'/tmp/brr-worktree/Repo',preview:'Git chat',updatedAt:2,workspace:{...base,cwd:'/tmp/brr-worktree/Repo',roots:['/tmp/brr-worktree'],folders:[{source:'/work/Repo',path:'/tmp/brr-worktree/Repo'}],worktrees:[{projectPath:'/work/Repo',path:'/tmp/brr-worktree',branch:'codex/test',cwd:'/tmp/brr-worktree/Repo'}]}},
      {id:'scratch-thread',cwd:'/scratch/thread-c',preview:'Scratch chat',updatedAt:1,workspace:{scopeProjectId:null,projectRevision:null,cwd:'/scratch/thread-c',roots:['/scratch/thread-c'],folders:[],worktrees:[]}},
    ]} as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button',{name:'Shared docs'}))
    const sharedLabel = await screen.findByText('Shared folder · Docs')
    expect(sharedLabel).toHaveAttribute('title','/shared/Docs')

    await userEvent.click(screen.getByRole('button',{name:'Git chat'}))
    await waitFor(() => expect(screen.queryByText('Shared folder · Repo')).not.toBeInTheDocument())
    await userEvent.click(screen.getByRole('button',{name:'Scratch chat'}))
    expect(screen.queryByText(/Shared folder/)).not.toBeInTheDocument()
  })

  it('saves composer settings when they change without requiring draft text', async () => {
    backend.connect.mockResolvedValueOnce({ account: { type: 'chatgpt', email: 'dev@example.com', planType: 'plus' }, models: [{ id: 'sol', model: 'sol', displayName: 'Sol', isDefault: true, defaultReasoningEffort: 'medium', supportedReasoningEfforts: [{ reasoningEffort: 'high', description: '' }] }], threads: [] } as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', { name: 'Chat options' }))
    await userEvent.selectOptions(await screen.findByRole('combobox', { name: 'Reasoning effort' }), 'high')
    await waitFor(() => expect(chatApi.records.some(record => record.document.text === '' && record.settings.effort === 'high')).toBe(true))
  })

  it('restores the model and reasoning saved with an unsent draft', async () => {
    backend.connect.mockResolvedValueOnce({ account: { type: 'chatgpt', email: 'dev@example.com', planType: 'plus' }, models: [
      { id: 'default', model: 'sol-default', displayName: 'Default', isDefault: true, defaultReasoningEffort: 'medium', supportedReasoningEfforts: [{ reasoningEffort: 'medium', description: '' }] },
      { id: 'saved', model: 'sol-saved', displayName: 'Saved model', defaultReasoningEffort: 'low', supportedReasoningEfforts: [{ reasoningEffort: 'high', description: '' }] },
    ], threads: [] } as never)
    chatApi.records = [{ id: 'saved-context-draft', threadId: null, projectId: null, agentId: null, document: { version: 1, text: 'Use saved context', references: [], quotes: [] }, settings: { model: 'sol-saved', modelId: 'saved', effort: 'high', mode: 'plan', readOnly: true }, revision: 1, updatedAt: '2026-11-02T00:00:00Z' }]
    render(<App />)
    await userEvent.click(await screen.findByRole('button', { name: 'Draft Use saved context' }))
    expect(await screen.findByRole('combobox', { name: 'Model' })).toHaveValue('saved')
    await userEvent.click(screen.getByRole('button', { name: 'Chat options' }))
    expect(screen.getByRole('combobox', { name: 'Reasoning effort' })).toHaveValue('high')
    expect(screen.getByRole('button', { name: 'Plan mode' })).toHaveAttribute('aria-pressed', 'true')
    expect(screen.getByRole('checkbox', { name: 'Read only' })).toBeChecked()
  })

  it('recovers an interrupted branch setup without starting another branch operation', async () => {
    chatApi.pendingBranches.mockResolvedValueOnce({ data: [{ id: 'branch-op-1', sourceThreadId: 'source-chat', phase: 'worktreeCreated', cwd: '/project.feature' }] } as never)
    chatApi.recoverBranch.mockResolvedValueOnce({ thread: { id: 'recovered-chat', cwd: '/project.feature', preview: 'Recovered branch', updatedAt: 2 } } as never)
    backend.resume.mockResolvedValueOnce({ thread: { id: 'recovered-chat', cwd: '/project.feature', preview: 'Recovered branch', updatedAt: 2, turns: [] } } as never)
    render(<App />)
    expect(await screen.findByRole('region', { name: 'Interrupted branch setup' })).toHaveTextContent('worktreeCreated')
    await userEvent.click(screen.getByRole('button', { name: 'Recover setup' }))
    await waitFor(() => expect(chatApi.recoverBranch).toHaveBeenCalledWith('branch-op-1'))
    expect(chatApi.branch).not.toHaveBeenCalled()
    expect(await screen.findByRole('textbox', { name: 'Message' })).toBeVisible()
  })

  it('dismisses interrupted setup notices persistently until their operation state changes', async () => {
    const operation = { id: 'branch-notice-1', sourceThreadId: 'source-chat', threadId: 'target-chat', phase: 'worktreeCreated', cwd: '/project.feature' }
    chatApi.pendingBranches.mockResolvedValue({ data: [operation] } as never)
    const { unmount } = render(<App />)
    const banner = await screen.findByRole('region', { name: 'Interrupted branch setup' })
    expect(banner).toHaveTextContent('worktreeCreated')
    expect(banner).not.toHaveTextContent('Recover it to continue')
    const starts = backend.start.mock.calls.length
    const resumes = backend.resume.mock.calls.length
    const sends = backend.send.mock.calls.length

    await userEvent.click(screen.getByRole('button', { name: 'Dismiss interrupted setup notices' }))
    expect(screen.queryByRole('region', { name: 'Interrupted branch setup' })).not.toBeInTheDocument()
    expect(chatApi.recoverBranch).not.toHaveBeenCalled()
    expect(chatApi.branch).not.toHaveBeenCalled()
    expect(backend.start).toHaveBeenCalledTimes(starts)
    expect(backend.resume).toHaveBeenCalledTimes(resumes)
    expect(backend.send).toHaveBeenCalledTimes(sends)

    unmount()
    render(<App />)
    await userEvent.click(await screen.findByRole('button', { name: 'Saved messages' }))
    expect(await screen.findByRole('button', { name: 'Review interrupted setups (1)' })).toBeInTheDocument()
    expect(screen.queryByRole('region', { name: 'Interrupted branch setup' })).not.toBeInTheDocument()
    unmount()

    chatApi.pendingBranches.mockResolvedValue({ data: [{ ...operation, phase: 'branchCreated' }] } as never)
    render(<App />)
    expect(await screen.findByRole('region', { name: 'Interrupted branch setup' })).toHaveTextContent('branchCreated')
  })

  it('reopens dismissed setups from Saved messages without replacing the chat or recovering the branch', async () => {
    chatApi.pendingBranches.mockResolvedValue({ data: [{ id: 'branch-notice-2', sourceThreadId: 'source-chat', phase: 'worktreeCreated', cwd: '/project.feature' }] } as never)
    render(<App />)
    const message = await screen.findByRole('textbox', { name: 'Message' })
    await userEvent.type(message, 'Keep this draft')
    await userEvent.click(await screen.findByRole('button', { name: 'Dismiss interrupted setup notices' }))
    expect(screen.queryByRole('region', { name: 'Interrupted branch setup' })).not.toBeInTheDocument()

    const starts = backend.start.mock.calls.length
    const resumes = backend.resume.mock.calls.length
    const sends = backend.send.mock.calls.length
    await userEvent.click(screen.getByRole('button', { name: 'Saved messages' }))
    const review = await screen.findByRole('button', { name: 'Review interrupted setups (1)' })
    await userEvent.click(review)

    expect(await screen.findByRole('textbox', { name: 'Message' })).toHaveValue('Keep this draft')
    expect(await screen.findByRole('button', { name: 'Recover setup' })).toBeVisible()
    expect(chatApi.recoverBranch).not.toHaveBeenCalled()
    expect(chatApi.branch).not.toHaveBeenCalled()
    expect(backend.start).toHaveBeenCalledTimes(starts)
    expect(backend.resume).toHaveBeenCalledTimes(resumes)
    expect(backend.send).toHaveBeenCalledTimes(sends)
  })

  it('keeps the durable notice fingerprint stable when a recover attempt adds a transient error', async () => {
    const operation = { id: 'branch-notice-recover-error', sourceThreadId: 'source-chat', phase: 'worktreeCreated', cwd: '/project.feature' }
    chatApi.pendingBranches.mockResolvedValue({ data: [operation] } as never)
    chatApi.recoverBranch.mockRejectedValueOnce(new Error('Temporary recovery failure'))
    const { unmount } = render(<App />)
    await userEvent.click(await screen.findByRole('button', { name: 'Recover setup' }))
    expect(await screen.findByRole('status')).toHaveTextContent('Temporary recovery failure')
    await userEvent.click(screen.getByRole('button', { name: 'Dismiss interrupted setup notices' }))
    expect(screen.queryByRole('region', { name: 'Interrupted branch setup' })).not.toBeInTheDocument()

    unmount()
    render(<App />)
    await userEvent.click(await screen.findByRole('button', { name: 'Saved messages' }))
    expect(await screen.findByRole('button', { name: 'Review interrupted setups (1)' })).toBeInTheDocument()
    expect(screen.queryByRole('region', { name: 'Interrupted branch setup' })).not.toBeInTheDocument()
  })

  it('keeps a notice dismissed for the current session when local storage writes fail', async () => {
    const operation = { id: 'branch-notice-storage', sourceThreadId: 'source-chat', phase: 'worktreeCreated', cwd: '/project.feature' }
    chatApi.pendingBranches.mockResolvedValue({ data: [operation] } as never)
    const setItem = vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => { throw new Error('Storage unavailable') })
    try {
      render(<App />)
      await screen.findByRole('region', { name: 'Interrupted branch setup' })
      await userEvent.click(screen.getByRole('button', { name: 'Dismiss interrupted setup notices' }))
      expect(screen.queryByRole('region', { name: 'Interrupted branch setup' })).not.toBeInTheDocument()
    } finally { setItem.mockRestore() }
  })

  it('restores only attachment grants selected by the saved draft', async () => {
    const saved = { id: 'draft-with-attachment', threadId: null, projectId: null, agentId: null, document: { version: 1, text: '', references: [], quotes: [], attachmentIds: ['selected-grant'] }, settings: { model: '', effort: 'medium', mode: 'default' }, revision: 1, updatedAt: '2026-11-02T00:00:00Z' }
    chatApi.records = [saved]
    const selected = { id: 'selected-grant', name: 'selected.txt', mime: 'text/plain', size: 1, kind: 'file', status: 'ready' }
    const historical = { id: 'historical-grant', name: 'historical.txt', mime: 'text/plain', size: 1, kind: 'file', status: 'ready' }
    chatApi.attachments.mockResolvedValueOnce([]).mockResolvedValueOnce([selected, historical] as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', { name: 'Draft Untitled draft' }))
    expect(await screen.findByText('selected.txt')).toBeInTheDocument()
    expect(screen.queryByText('historical.txt')).not.toBeInTheDocument()
  })

  it('opens a composer reference in the selected project scope for a new draft', async () => {
    chatApi.references.mockResolvedValueOnce([{id: 'src/main.rs', name: 'main.rs', path: '/project/src/main.rs', type: 'file'}] as never)
    render(<App />)
    await selectProject()
    await userEvent.click(await screen.findByRole('button', {name: 'Find test references'}))
    await userEvent.click(await screen.findByRole('button', {name: 'Open reference main.rs'}))
    await waitFor(() => expect(chatApi.resolveReference).toHaveBeenCalledWith({projectId: 'project-main'}, 'src/main.rs', 'file'))
    expect(chatApi.references).toHaveBeenCalledWith({projectId: 'project-main', query: 'src'})
  })

  it('shows the host error when a selected project has no registered folders', async () => {
    backend.projects.mockResolvedValueOnce({data: [{...mainProject, folders: [], primaryFolder: null}]} as never)
    chatApi.resolveReference.mockRejectedValueOnce(new Error('This project has no registered folders.'))
    render(<App />)
    await selectProject()
    await userEvent.click(await screen.findByRole('button', {name: 'Open direct test reference'}))
    expect(await screen.findByRole('alert')).toHaveTextContent('This project has no registered folders.')
  })

  it('explains why a draft cannot open a workspace reference without a project', async () => {
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name: 'Open direct test reference'}))
    expect(await screen.findByRole('alert')).toHaveTextContent('Choose a project with a registered folder')
    expect(chatApi.resolveReference).not.toHaveBeenCalled()
  })

  it('keeps a late attachment import with its originating draft without showing it in the new draft', async () => {
    let finishImport!: (attachments: any[]) => void
    chatApi.importFiles.mockImplementationOnce(() => new Promise(resolve => { finishImport = resolve }) as never)
    render(<App />)
    const message = await screen.findByRole('textbox', { name: 'Message' })
    await act(async () => { fireEvent.paste(message, { clipboardData: { files: [new File(['note'], 'late.txt', { type: 'text/plain' })] } }) })
    const sourceId = chatApi.importFiles.mock.calls.at(-1)?.[0]
    await userEvent.click(screen.getByRole('button', { name: 'New conversation' }))
    await act(async () => finishImport([{ id: 'late-grant', name: 'late.txt', mime: 'text/plain', size: 4, kind: 'file', status: 'ready' }]))
    await waitFor(() => expect(chatApi.records.find(record => record.id === sourceId)?.document.attachmentIds).toEqual(['late-grant']))
    expect(screen.getByRole('textbox', { name: 'Message' })).toHaveValue('')
    expect(screen.queryByText('late.txt')).not.toBeInTheDocument()
  })

  it('sends a selected attachment without requiring prompt text and clears only its draft selection', async () => {
    backend.connect.mockResolvedValueOnce({ account: { type: 'chatgpt', email: 'dev@example.com', planType: 'plus' }, models: [{ id: 'sol', model: 'sol', displayName: 'Sol', isDefault: true, defaultReasoningEffort: 'medium', supportedReasoningEfforts: [] }], threads: [] } as never)
    const attachment = { id: 'grant-1', name: 'notes.txt', mime: 'text/plain', size: 4, kind: 'file', status: 'ready' }
    chatApi.importFiles.mockResolvedValueOnce([attachment] as never)
    render(<App />)
    const message = await screen.findByRole('textbox', { name: 'Message' })
    await act(async () => { fireEvent.paste(message, { clipboardData: { files: [new File(['note'], 'notes.txt', { type: 'text/plain' })] } }) })
    await waitFor(() => expect(screen.getByRole('button', { name: 'Send message' })).toBeEnabled())
    expect(message).toHaveValue('')
    await userEvent.click(screen.getByRole('button', { name: 'Send message' }))
    await waitFor(() => expect(chatApi.submit).toHaveBeenCalledWith(expect.objectContaining({ document: expect.objectContaining({ text: '', attachmentIds: ['grant-1'] }), attachmentIds: ['grant-1'], delivery: 'send' })))
    await waitFor(() => expect(screen.getByRole('button', { name: 'Send message' })).toBeDisabled())
    await waitFor(() => expect(chatApi.records.some(record => record.document.attachmentIds?.length === 0)).toBe(true))
    expect(chatApi.removeAttachment).not.toHaveBeenCalled()
  })

  it('queues the default message while a turn runs and reserves immediate delivery for the accelerated action', async () => {
    backend.connect.mockResolvedValueOnce({ account: { type: 'chatgpt', email: 'dev@example.com' }, models: [{ id: 'sol', model: 'sol', displayName: 'Sol', defaultReasoningEffort: 'medium', supportedReasoningEfforts: [] }], threads: [{ id: 'running-chat', preview: 'Working', cwd: '/project', updatedAt: 1 }] } as never)
    backend.resume.mockResolvedValueOnce({ thread: { id: 'running-chat', cwd: '/project', preview: 'Working', updatedAt: 1, turns: [{ id: 'turn-live', status: 'inProgress', items: [] }] } } as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', { name: 'Working' }))
    const message = await screen.findByRole('textbox', { name: 'Message' })
    await userEvent.type(message, 'Queue this while the turn runs')
    await userEvent.keyboard('{Enter}')
    await waitFor(() => expect(chatApi.submit).toHaveBeenCalledWith(expect.objectContaining({ delivery: 'queue', threadId: 'running-chat', expectedTurnId: 'turn-live' })))
  })
  it('queues an attachment-only follow-up while a turn runs', async () => {
    backend.connect.mockResolvedValueOnce({ account: { type: 'chatgpt', email: 'dev@example.com', planType: 'plus' }, models: [{ id: 'sol', model: 'sol', displayName: 'Sol', defaultReasoningEffort: 'medium', supportedReasoningEfforts: [] }], threads: [{ id: 'running-chat', preview: 'Working', cwd: '/project', updatedAt: 1 }] } as never)
    backend.resume.mockResolvedValueOnce({ thread: { id: 'running-chat', cwd: '/project', preview: 'Working', updatedAt: 1, turns: [{ id: 'turn-live', status: 'inProgress', items: [] }] } } as never)
    chatApi.importFiles.mockResolvedValueOnce([{ id: 'queue-grant', name: 'notes.txt', mime: 'text/plain', size: 4, kind: 'file', status: 'ready' }] as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', { name: 'Working' }))
    const message = await screen.findByRole('textbox', { name: 'Message' })
    await act(async () => { fireEvent.paste(message, { clipboardData: { files: [new File(['note'], 'notes.txt', { type: 'text/plain' })] } }) })
    const queue = await screen.findByRole('button', { name: 'Queue message' })
    await waitFor(() => expect(queue).toBeEnabled())
    await userEvent.click(queue)
    await waitFor(() => expect(chatApi.submit).toHaveBeenCalledWith(expect.objectContaining({ delivery: 'queue', document: expect.objectContaining({ text: '', attachmentIds: ['queue-grant'] }), attachmentIds: ['queue-grant'] })))
  })

  it('refreshes only the active thread queue after a host chat-changed event', async () => {
    backend.connect.mockResolvedValueOnce({ account: { type: 'chatgpt', email: 'dev@example.com' }, models: [{ id: 'sol', model: 'sol', displayName: 'Sol', defaultReasoningEffort: 'medium', supportedReasoningEfforts: [] }], threads: [{ id: 'running-chat', preview: 'Working', cwd: '/project', updatedAt: 1 }] } as never)
    backend.resume.mockResolvedValueOnce({ thread: { id: 'running-chat', cwd: '/project', preview: 'Working', updatedAt: 1, turns: [{ id: 'turn-live', status: 'inProgress', items: [] }] } } as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', { name: 'Working' }))
    await screen.findByRole('textbox', { name: 'Message' })
    chatApi.queue.mockClear()
    chatApi.queue.mockResolvedValueOnce({ entries: [{ id: 'remote-queue', threadId: 'running-chat', document: { version: 1, text: 'Queued by another window', references: [], quotes: [] }, attachmentIds: [], settings: { model: 'sol', effort: 'medium', mode: 'default' }, delivery: 'queue', status: 'queued', revision: 1 }], state: { paused: true, reason: 'Paused by you' } } as never)
    act(() => events.handlers.forEach(handler => handler({ method: 'brr/chat/changed', params: { threadId: 'other-chat' } })))
    expect(chatApi.queue).not.toHaveBeenCalled()
    act(() => events.handlers.forEach(handler => handler({ method: 'brr/chat/changed', params: { threadId: 'running-chat' } })))
    expect((await screen.findAllByText('Queued by another window')).length).toBeGreaterThan(0)
    expect(screen.getByText(/Queue paused/)).toBeInTheDocument()
    expect(chatApi.queue).toHaveBeenCalledWith('running-chat')
  })

  it('ignores a delayed queue snapshot after switching to another chat', async () => {
    backend.connect.mockResolvedValueOnce({ account: { type: 'chatgpt', email: 'dev@example.com' }, models: [{ id: 'sol', model: 'sol', displayName: 'Sol', defaultReasoningEffort: 'medium', supportedReasoningEfforts: [] }], threads: [{ id: 'chat-a', preview: 'Chat A', cwd: '/project', updatedAt: 2 }, { id: 'chat-b', preview: 'Chat B', cwd: '/project', updatedAt: 1 }] } as never)
    backend.resume.mockResolvedValueOnce({ thread: { id: 'chat-a', cwd: '/project', preview: 'Chat A', updatedAt: 1, turns: [] } } as never)
    backend.resume.mockResolvedValueOnce({ thread: { id: 'chat-b', cwd: '/project', preview: 'Chat B', updatedAt: 1, turns: [] } } as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', { name: 'Chat A' }))
    await screen.findByRole('textbox', { name: 'Message' })
    chatApi.queue.mockClear()
    let resolveOldSnapshot!: (snapshot: any) => void
    chatApi.queue.mockImplementationOnce(() => new Promise(resolve => { resolveOldSnapshot = resolve }))
    act(() => events.handlers.forEach(handler => handler({ method: 'brr/chat/changed', params: { threadId: 'chat-a' } })))
    await waitFor(() => expect(chatApi.queue).toHaveBeenCalledWith('chat-a'))

    await userEvent.click(screen.getByRole('button', { name: 'Chat B' }))
    await waitFor(() => expect(chatApi.queue).toHaveBeenCalledWith('chat-b'))
    await act(async () => resolveOldSnapshot({ entries: [{ id: 'old-entry', threadId: 'chat-a', document: { version: 1, text: 'Old chat queue row', references: [], quotes: [] }, attachmentIds: [], settings: { model: 'sol', effort: 'medium', mode: 'default' }, delivery: 'queue', status: 'queued', revision: 1 }], state: { paused: false } }))
    expect(screen.queryByText('Old chat queue row')).not.toBeInTheDocument()
  })

  it('never shows the previous chat queue under the newly selected chat', async () => {
    backend.connect.mockResolvedValueOnce({ account: { type: 'chatgpt', email: 'dev@example.com' }, models: [{ id: 'sol', model: 'sol', displayName: 'Sol', defaultReasoningEffort: 'medium', supportedReasoningEfforts: [] }], threads: [{ id: 'chat-a', preview: 'Chat A', cwd: '/project', updatedAt: 2 }, { id: 'chat-b', preview: 'Chat B', cwd: '/project', updatedAt: 1 }] } as never)
    backend.resume.mockResolvedValueOnce({ thread: { id: 'chat-a', cwd: '/project', preview: 'Chat A', updatedAt: 1, turns: [] } } as never)
    backend.resume.mockResolvedValueOnce({ thread: { id: 'chat-b', cwd: '/project', preview: 'Chat B', updatedAt: 1, turns: [] } } as never)
    chatApi.queue.mockResolvedValueOnce({ entries: [{ id: 'a-row', threadId: 'chat-a', document: { version: 1, text: 'A only queue row', references: [], quotes: [] }, attachmentIds: [], settings: { model: 'sol', effort: 'medium', mode: 'default' }, delivery: 'queue', status: 'queued', revision: 1 }], state: { paused: false } } as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', { name: 'Chat A' }))
    expect((await screen.findAllByText('A only queue row')).length).toBeGreaterThan(0)
    let rejectChatB!: (reason: Error) => void
    chatApi.queue.mockImplementationOnce(() => new Promise((_resolve, reject) => { rejectChatB = reject }))
    await userEvent.click(screen.getByRole('button', { name: 'Chat B' }))
    await waitFor(() => expect(chatApi.queue).toHaveBeenCalledWith('chat-b'))
    expect(screen.queryAllByText('A only queue row')).toHaveLength(0)
    await act(async () => rejectChatB(new Error('queue offline')))
    expect(screen.queryAllByText('A only queue row')).toHaveLength(0)
  })

  it('steers an attachment-only follow-up when Steer now is selected', async () => {
    backend.connect.mockResolvedValueOnce({ account: { type: 'chatgpt', email: 'dev@example.com', planType: 'plus' }, models: [{ id: 'sol', model: 'sol', displayName: 'Sol', defaultReasoningEffort: 'medium', supportedReasoningEfforts: [] }], threads: [{ id: 'running-chat', preview: 'Working', cwd: '/project', updatedAt: 1 }] } as never)
    backend.resume.mockResolvedValueOnce({ thread: { id: 'running-chat', cwd: '/project', preview: 'Working', updatedAt: 1, turns: [{ id: 'turn-live', status: 'inProgress', items: [] }] } } as never)
    chatApi.importFiles.mockResolvedValueOnce([{ id: 'steer-grant', name: 'notes.txt', mime: 'text/plain', size: 4, kind: 'file', status: 'ready' }] as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', { name: 'Working' }))
    const message = await screen.findByRole('textbox', { name: 'Message' })
    await act(async () => { fireEvent.paste(message, { clipboardData: { files: [new File(['note'], 'notes.txt', { type: 'text/plain' })] } }) })
    fireEvent.keyDown(message, { key: 'Enter', ctrlKey: true })
    await waitFor(() => expect(chatApi.submit).toHaveBeenCalledWith(expect.objectContaining({ delivery: 'steer', document: expect.objectContaining({ text: '', attachmentIds: ['steer-grant'] }), attachmentIds: ['steer-grant'] })))
  })

  it('preserves the composer draft while navigating to Tickets and back Home', async () => {
    render(<App />)
    const message = await screen.findByRole('textbox', { name: 'Message' })
    await userEvent.type(message, 'Keep this chat draft')
    await userEvent.click(screen.getByRole('button', { name: 'Tickets' }))
    expect(await screen.findByRole('heading', { name: 'Tickets' })).toBeInTheDocument()
    expect(screen.getByRole('textbox', { name: 'New ticket title' })).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', { name: 'Home' }))
    expect(screen.getByRole('textbox', { name: 'Message' })).toHaveValue('Keep this chat draft')
  })
  it('shows only Profile over Tickets and restores the ticket draft on return', async () => {
    render(<App />)
    await userEvent.click(await screen.findByRole('button', { name: 'Tickets' }))
    await userEvent.type(await screen.findByRole('textbox', { name: 'New ticket title' }), 'Unsent ticket title')
    await userEvent.click(screen.getByRole('button', { name: 'Profile' }))
    expect(await screen.findByRole('region', { name: 'User profile' })).toBeVisible()
    expect(screen.queryByRole('region', { name: 'Tickets' })).not.toBeInTheDocument()
    await userEvent.keyboard('{Escape}')
    expect(await screen.findByRole('textbox', { name: 'New ticket title' })).toHaveValue('Unsent ticket title')
  })
  it('preserves an unsaved ticket description while moving between Home and Tickets', async () => {
    const item = { id: 'ticket-nav', number: 'BRR-000021', projectId: 'project-tickets', title: 'Update onboarding', description: 'Original description', acceptanceCriteria: [], agentId: null, priority: 'none', status: 'todo', position: 0, revision: 1, statusRevision: 1, archived: false, createdAt: 1, updatedAt: 1 }
    ticketApi.list.mockResolvedValue({ data: [item], nextCursor: null } as never)
    ticketApi.read.mockResolvedValue({ ticket: item, comments: { data: [] }, activity: { data: [] }, runs: [] })
    render(<App />)
    await userEvent.click(await screen.findByRole('button', { name: 'Tickets' }))
    await userEvent.click(await screen.findByRole('button', { name: /BRR-000021 Update onboarding/ }))
    const field = await screen.findByRole('textbox', { name: 'Description' })
    await userEvent.clear(field)
    await userEvent.type(field, 'Unsaved detail draft')
    await userEvent.click(screen.getByRole('button', { name: 'Home' }))
    await userEvent.click(screen.getByRole('button', { name: 'Tickets' }))
    await userEvent.click(await screen.findByRole('button', { name: /BRR-000021 Update onboarding/ }))
    expect(await screen.findByRole('textbox', { name: 'Description' })).toHaveValue('Unsaved detail draft')
  })
  it('reviews a UI follow-up before sending, retains Plan mode and preserves the composer draft', async () => {
    backend.connect.mockResolvedValueOnce({account: {type: 'chatgpt', email: 'test@example.com'}, models: [{id: 'sol', model: 'sol', displayName: 'Sol', defaultReasoningEffort: 'medium', supportedReasoningEfforts: []}], threads: [{id: 'ui-chat', cwd: '/project', preview: 'Interactive comparison', updatedAt: 1}]} as never)
    backend.resume.mockResolvedValueOnce({thread: {id: 'ui-chat', cwd: '/project', preview: 'Interactive comparison', updatedAt: 1, collaborationMode: {mode: 'plan'}, turns: [{id: 'done', status: 'completed', items: [{id: 'ui-call', type: 'dynamicToolCall', tool: 'brr_ui_render', status: 'completed', success: true, arguments: {version: 1, kind: 'native', title: 'Comparison', text: 'Options', initialState: {}, actions: [], components: []}}]}]}} as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name: 'Interactive comparison'}))
    const message = await screen.findByRole('textbox', {name: 'Message'})
    await userEvent.type(message, 'Keep this draft')
    await userEvent.click(await screen.findByRole('button', {name: 'Ask about selections'}))
    expect(backend.send).not.toHaveBeenCalled()
    const review = await screen.findByRole('region', {name: 'Review UI follow-up'})
    expect(review).toHaveTextContent('Compare the selected inputs')
    await userEvent.click(screen.getByRole('button', {name: 'Send follow-up'}))
    await waitFor(() => expect(backend.send).toHaveBeenCalledWith('ui-chat', 'Compare the selected inputs', 'sol', 'medium', 'plan'))
    expect(message).toHaveValue('Keep this draft')
    expect(uiApi.claim).toHaveBeenCalledOnce()
  })
  it('keeps a reviewed UI follow-up pending while the conversation is working', async () => {
    backend.connect.mockResolvedValueOnce({account: {type: 'chatgpt', email: 'test@example.com'}, models: [{id: 'sol', model: 'sol', displayName: 'Sol', defaultReasoningEffort: 'medium', supportedReasoningEfforts: []}], threads: [{id: 'ui-chat', cwd: '/project', preview: 'Interactive comparison', updatedAt: 1}]} as never)
    backend.resume.mockResolvedValueOnce({thread: {id: 'ui-chat', cwd: '/project', preview: 'Interactive comparison', updatedAt: 1, turns: [{id: 'done', status: 'completed', items: [{id: 'ui-call', type: 'dynamicToolCall', tool: 'brr_ui_render', status: 'completed', success: true}]}]}} as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name: 'Interactive comparison'}))
    await userEvent.click(await screen.findByRole('button', {name: 'Ask about selections'}))
    act(() => events.handler?.({method: 'turn/started', params: {threadId: 'ui-chat', turn: {id: 'working', items: []}}}))
    expect(screen.getByRole('button', {name: 'Send follow-up'})).toBeDisabled()
    expect(uiApi.claim).not.toHaveBeenCalled()
    act(() => events.handler?.({method: 'turn/completed', params: {threadId: 'ui-chat', turn: {id: 'working', status: 'completed', items: []}}}))
    expect(screen.getByRole('button', {name: 'Send follow-up'})).toBeEnabled()
  })
  it('sends task-result follow-ups to a new owning-agent chat using the original project', async () => {
    uiApi.task = {id: 'task', agentId: 'owner', projectId: '/original', cwd: '/worktree'}
    uiApi.claim.mockResolvedValueOnce({reviewId: 'review-1', threadId: 'ui-chat', callId: 'ui-call', documentHash: 'hash', text: 'Compare the selected inputs', task: uiApi.task} as never)
    agentApi.open.mockResolvedValueOnce({thread: {id: 'followup-chat', cwd: '/new-worktree', preview: '', updatedAt: 2}})
    let finishSend!: (result: {turn: {id: string}}) => void
    backend.send.mockImplementationOnce(() => new Promise(resolve => {finishSend = resolve}))
    backend.connect.mockResolvedValueOnce({account: {type: 'chatgpt', email: 'test@example.com'}, models: [{id: 'sol', model: 'sol', displayName: 'Sol', defaultReasoningEffort: 'medium', supportedReasoningEfforts: []}], threads: [{id: 'ui-chat', cwd: '/worktree', preview: 'Task result', updatedAt: 1}]} as never)
    backend.resume.mockResolvedValueOnce({thread: {id: 'ui-chat', cwd: '/worktree', preview: 'Task result', updatedAt: 1, turns: [{id: 'done', status: 'completed', items: [{id: 'ui-call', type: 'dynamicToolCall', tool: 'brr_ui_render', status: 'completed', success: true}]}]}} as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name: 'Task result'}))
    await userEvent.click(await screen.findByRole('button', {name: 'Ask about selections'}))
    expect(await screen.findByText('Follow up in a new agent chat')).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', {name: 'Send follow-up'}))
    await waitFor(() => expect(backend.send).toHaveBeenCalledWith('followup-chat', 'Compare the selected inputs', 'sol', 'medium', 'default'))
    await userEvent.type(screen.getByRole('textbox', {name: 'Message'}), 'Preserved draft')
    expect(screen.getByRole('button', {name: 'Send message'})).toBeDisabled()
    await userEvent.keyboard('{Enter}')
    expect(backend.send).toHaveBeenCalledOnce()
    await act(async () => finishSend({turn: {id: 'task-followup'}}))
    expect(agentApi.open).toHaveBeenCalledWith('owner', {projectId: '/original', readOnly: false})
    expect(backend.send).not.toHaveBeenCalledWith('ui-chat', expect.anything(), expect.anything(), expect.anything(), expect.anything())
  })
  it('manages the exact original project without toggling its chat list', async () => {
    backend.projects.mockResolvedValueOnce({data: [
      {...mainProject, folders: ['/work/brr'], primaryFolder: '/work/brr'},
      {...mainProject, id: 'project-other', folders: ['/other/brr'], primaryFolder: '/other/brr'},
    ]} as never)
    backend.connect.mockResolvedValueOnce({ account: null, models: [], threads: [
      {id: 'worktree-chat', projectId: 'project-main', preview: 'Worktree chat', cwd: '/work/brr.feature', updatedAt: 2,
        worktree: {projectPath: '/work/brr', cwd: '/work/brr.feature', path: '/work/brr.feature', branch: 'feature'}},
      {id: 'other-chat', projectId: 'project-other', preview: 'Other chat', cwd: '/other/brr', updatedAt: 1},
    ] } as never)
    render(<App />)
    const triggers = await screen.findAllByRole('button', {name: 'Manage project brr'})
    expect(triggers).toHaveLength(2)
    await userEvent.click(triggers[0])
    expect(screen.getAllByRole('button', {name: 'Expand chats in brr'})[0]).toHaveAttribute('aria-expanded', 'false')
    await userEvent.click(screen.getByRole('menuitem', {name: /Reveal in|Open in file manager/}))
    await waitFor(() => expect(backend.revealProject).toHaveBeenCalledWith('/work/brr'))
    expect(backend.revealProject).not.toHaveBeenCalledWith('/work/brr.feature')
    expect(backend.revealProject).not.toHaveBeenCalledWith('/other/brr')
    expect(screen.getAllByRole('button', {name: 'Expand chats in brr'})[0]).toHaveAttribute('aria-expanded', 'false')
  })

  it('applies saved project defaults only to a new chat', async () => {
    const saved = {id: 'project-defaults', name: 'Desktop app', folders: ['/project'], primaryFolder: '/project', revision: 1, model: 'sol-project', effort: 'high', permission: 'read-only', hidden: false}
    backend.projects.mockResolvedValueOnce({data: [saved]} as never)
    backend.connect.mockResolvedValueOnce({account: {type: 'chatgpt', email: 'dev@example.com', planType: 'plus'}, models: [
      {id: 'default', model: 'sol-default', displayName: 'Default', isDefault: true, defaultReasoningEffort: 'medium', supportedReasoningEfforts: []},
      {id: 'project', model: 'sol-project', displayName: 'Project model', isDefault: false, defaultReasoningEffort: 'medium', supportedReasoningEfforts: [{reasoningEffort: 'high', description: ''}]},
    ], threads: []} as never)
    render(<App />)
    await screen.findByRole('button', {name: 'Manage project Desktop app'})
    await selectProject('project-defaults')
    await userEvent.type(screen.getByRole('textbox', {name: 'Message'}), 'Use these defaults')
    await userEvent.click(screen.getByRole('button', {name: 'Send message'}))
    await waitFor(() => expect(backend.start).toHaveBeenCalledWith('project-defaults', 'sol-project', true))
    expect(backend.send).toHaveBeenCalledWith('thread-1', 'Use these defaults', 'sol-project', 'high', 'default')
  })

  it('edits a project from its menu and returns Home with the draft intact', async () => {
    backend.connect.mockResolvedValueOnce({account: null, models: [], threads: [{id: 'edit-chat', preview: 'Saved chat', cwd: '/work/brr', updatedAt: 1}]} as never)
    render(<App />)
    await userEvent.type(screen.getByRole('textbox', {name: 'Message'}), 'Keep this draft')
    await userEvent.click(await screen.findByRole('button', {name: 'Manage project brr'}))
    await userEvent.click(screen.getByRole('button', {name: 'Manage project brr'}))
    await userEvent.click(screen.getByRole('button', {name: 'Expand chats in brr'}))
    expect(screen.getByRole('button', {name: 'Collapse chats in brr'})).toHaveAttribute('aria-expanded', 'true')
    await userEvent.click(screen.getByRole('button', {name: 'Manage project brr'}))
    await userEvent.click(screen.getByRole('menuitem', {name: 'Project settings'}))
    expect(screen.getByRole('dialog', {name: 'Edit project'})).toBeVisible()
    await userEvent.clear(screen.getByRole('textbox', {name: 'Project name'}))
    await userEvent.type(screen.getByRole('textbox', {name: 'Project name'}), 'Desktop')
    await userEvent.click(screen.getByRole('button', {name: 'Save changes'}))
    expect(await screen.findByRole('button', {name: 'Manage project Desktop'})).toBeVisible()
    expect(screen.getByRole('textbox', {name: 'Message'})).toHaveValue('Keep this draft')
    await userEvent.click(screen.getByRole('button', {name: 'Manage project Desktop'}))
    await userEvent.click(screen.getByRole('menuitem', {name: 'Project settings'}))
    await userEvent.click(screen.getByRole('button', {name: 'Home'}))
    expect(screen.queryByRole('dialog', {name: 'Edit project'})).not.toBeInTheDocument()
  })

  it('keeps navigation in place while project settings are saving', async () => {
    let finishSave!: (project: import('./lib/client').ProjectRecord) => void
    backend.saveProject.mockImplementationOnce(() => new Promise(resolve => { finishSave = resolve }))
    backend.connect.mockResolvedValueOnce({account: null, models: [], threads: [{id: 'save-chat', preview: 'Saved chat', cwd: '/work/brr', updatedAt: 1}]} as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name: 'Manage project brr'}))
    await userEvent.click(screen.getByRole('menuitem', {name: 'Project settings'}))
    await userEvent.click(screen.getByRole('button', {name: 'Save changes'}))
    expect(screen.getByRole('button', {name: 'Home'})).toBeDisabled()
    await userEvent.click(screen.getByRole('button', {name: 'New conversation'}))
    expect(screen.getByRole('dialog', {name: 'Edit project'})).toBeVisible()
    await act(async () => finishSave({...mainProject, name: 'Saved', revision: 2}))
    expect(await screen.findByRole('button', {name: 'Manage project Saved'})).toBeVisible()
    expect(screen.getByRole('button', {name: 'Home'})).toBeEnabled()
  })

  it('deletes a project by ID without archiving chats and preserves the draft', async () => {
    const chats = [{id: 'old-chat', projectId: 'project-main', preview: 'Older chat', cwd: '/work/brr', updatedAt: 1}]
    backend.connect.mockResolvedValueOnce({account: null, models: [], threads: chats} as never)
    backend.projects.mockResolvedValueOnce({data:[mainProject]} as never).mockResolvedValueOnce({data:[]} as never)
    backend.threads.mockResolvedValue([{...chats[0], projectId: null}] as never)
    render(<App />)
    await userEvent.type(screen.getByRole('textbox', {name: 'Message'}), 'Unsent work')
    await userEvent.click(await screen.findByRole('button', {name: 'Manage project brr'}))
    await userEvent.click(screen.getByRole('menuitem', {name: 'Delete project'}))
    await userEvent.click(screen.getByRole('button', {name: 'Delete project'}))
    await waitFor(() => expect(backend.deleteProject).toHaveBeenCalledWith('project-main'))
    expect(backend.archiveProject).not.toHaveBeenCalled()
    expect(screen.queryByRole('button', {name: 'Manage project brr'})).not.toBeInTheDocument()
    expect(screen.getByRole('textbox', {name: 'Message'})).toHaveValue('Unsent work')
  })

  it('archives project chats through the archive action while keeping the project', async () => {
    const chats = [{id: 'archive-me', projectId: 'project-main', preview: 'Old chat', cwd: '/project', updatedAt: 1}]
    backend.connect.mockResolvedValueOnce({account: null, models: [], threads: chats} as never)
    backend.archiveProject.mockResolvedValueOnce({archivedIds: ['archive-me'], skipped: [], failed: []})
    backend.threads.mockResolvedValue([])
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name: 'Manage project brr'}))
    await userEvent.click(screen.getByRole('menuitem', {name: 'Archive all chats'}))
    await userEvent.click(screen.getByRole('button', {name: 'Archive chats'}))
    await waitFor(() => expect(backend.archiveProject).toHaveBeenCalledWith('project-main'))
    expect(await screen.findByText('1 chat archived.')).toBeVisible()
    expect(backend.deleteProject).not.toHaveBeenCalled()
    await userEvent.click(screen.getByRole('button', {name: 'Done'}))
    expect(await screen.findByRole('button', {name: 'Manage project brr'})).toBeVisible()
  })

  it('loads saved agent identities after delayed connection and refreshes them on agent changes', async () => {
    let finishConnection!: (connection: unknown) => void
    const savedAgent = { id: 'saved-agent', name: 'Release guide', description: 'Keeps releases safe', instructions: '', soul: '', model: 'gpt-6.1-sol', effort: 'high', revision: 1, createdAt: 1, updatedAt: 1 }
    backend.connect.mockImplementationOnce(() => new Promise(resolve => { finishConnection = resolve }) as never)
    agentApi.list.mockResolvedValueOnce({ data: [savedAgent] } as never)
    render(<App />)

    await waitFor(() => expect(backend.connect).toHaveBeenCalled())
    expect(agentApi.list).not.toHaveBeenCalled()
    await act(async () => {
      finishConnection({ account: { type: 'chatgpt', email: 'dev@example.com', planType: 'plus' }, models: [{ id: 'sol', model: 'gpt-6.1-sol', displayName: 'GPT Sol', isDefault: true, defaultReasoningEffort: 'medium', supportedReasoningEfforts: [] }], threads: [] })
    })

    await openChatOptions()
    const identity = await screen.findByRole('combobox', { name: 'Chat identity' })
    await waitFor(() => expect(Array.from((identity as HTMLSelectElement).options).map(option => option.text)).toContain('Release guide'))

    agentApi.list.mockResolvedValueOnce({ data: [{ ...savedAgent, name: 'Release guide revised', revision: 2 }] } as never)
    act(() => events.handlers.forEach(handler => handler({ method: 'brr/agents/changed', params: {} })))
    await waitFor(() => expect(Array.from((identity as HTMLSelectElement).options).map(option => option.text)).toContain('Release guide revised'))
  })

  it('uses a contextual Agents sidebar and preserves its collapsed state and draft when returning Home', async () => {
    render(<App />)
    const message = screen.getByRole('textbox', { name: 'Message' })
    await userEvent.type(message, 'Keep this draft')

    await userEvent.click(await screen.findByRole('button', { name: 'Agents' }))

    expect(await screen.findByRole('region', { name: 'Agents' })).toBeInTheDocument()
    expect(screen.getByRole('heading', { name: 'Agents' })).toBeInTheDocument()
    expect(screen.queryByRole('complementary', { name: 'Main sidebar' })).not.toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Activity on' })).not.toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'New conversation' })).not.toBeInTheDocument()
    const collapse = screen.getByRole('button', { name: 'Collapse sidebar' })
    expect(collapse).toHaveAttribute('aria-controls', 'agents-sidebar')
    expect(collapse).toHaveAttribute('aria-expanded', 'true')
    await userEvent.click(collapse)
    expect(screen.getByRole('button', { name: 'Expand sidebar' })).toHaveAttribute('aria-controls', 'agents-sidebar')

    await userEvent.click(screen.getByRole('button', { name: 'Home' }))
    expect(screen.queryByRole('region', { name: 'Agents' })).not.toBeInTheDocument()
    expect(screen.queryByRole('complementary', { name: 'Main sidebar' })).not.toBeInTheDocument()
    expect(screen.getByRole('textbox', { name: 'Message' })).toHaveValue('Keep this draft')
    const expand = screen.getByRole('button', { name: 'Expand sidebar' })
    expect(expand).toHaveAttribute('aria-controls', 'main-sidebar')
    await userEvent.click(expand)
    expect(screen.getByRole('complementary', { name: 'Main sidebar' })).toBeVisible()
    expect(screen.getByRole('textbox', { name: 'Message' })).toHaveValue('Keep this draft')
  })

  it('returns to the selected conversation when opening a recent chat from Agents', async () => {
    backend.connect.mockResolvedValueOnce({ account: null, models: [], threads: [
      { id: 'navigation-thread', name: 'Existing navigation thread', preview: 'Existing navigation thread', cwd: '/project', updatedAt: 1 },
    ] } as never)
    render(<App />)
    await screen.findByRole('button', { name: 'Existing navigation thread' })
    await userEvent.click(screen.getByRole('button', { name: 'Agents' }))
    expect(await screen.findByRole('region', { name: 'Agents' })).toBeVisible()

    await userEvent.click(screen.getByRole('button', { name: 'Home' }))
    await userEvent.click(screen.getByRole('button', { name: 'Existing navigation thread' }))

    expect(screen.queryByRole('region', { name: 'Agents' })).not.toBeInTheDocument()
    expect(screen.getByRole('textbox', { name: 'Message' })).toBeVisible()
  })

  it('starts a selected named agent with its saved model and workspace defaults', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[]} as never)
    agentApi.list.mockResolvedValueOnce({ data: [{ id: 'agent-1', name: 'Release guide', description: '', instructions: '', soul: '', model: 'gpt-6.1-sol', effort: 'high', revision: 1, createdAt: 1, updatedAt: 1 }] } as never)
    agentApi.open.mockResolvedValueOnce({ thread: { id: 'agent-session', cwd: '/project', preview: '', updatedAt: 1 } })
    render(<App />)

    await selectProject()
    await openChatOptions()
    await userEvent.selectOptions(await screen.findByRole('combobox', { name: 'Chat identity' }), 'agent-1')
    await userEvent.type(screen.getByRole('textbox', { name: 'Message' }), 'Review this release')
    await userEvent.click(screen.getByRole('button', { name: 'Send message' }))

    await waitFor(() => expect(agentApi.open).toHaveBeenCalledWith('agent-1', { model: 'gpt-6.1-sol', effort: 'high', projectId: 'project-main', readOnly: false }))
    await waitFor(() => expect(backend.send).toHaveBeenCalledWith('agent-session', 'Review this release', 'gpt-6.1-sol', 'high', 'default'))
    expect(backend.start).not.toHaveBeenCalled()
  })

  it('can start a named agent chat without requiring a project folder', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[]} as never)
    agentApi.list.mockResolvedValueOnce({ data: [{ id: 'agent-1', name: 'Release guide', description: '', instructions: '', soul: '', model: 'gpt-6.1-sol', effort: 'medium', revision: 1, createdAt: 1, updatedAt: 1 }] } as never)
    agentApi.open.mockResolvedValueOnce({ thread: { id: 'agent-session-no-project', cwd: '', preview: '', updatedAt: 1 } })
    render(<App />)

    await openChatOptions()
    const identity = await screen.findByRole('combobox', { name: 'Chat identity' })
    await waitFor(() => expect(Array.from((identity as HTMLSelectElement).options).map(option => option.value)).toContain('agent-1'))
    await userEvent.selectOptions(identity, 'agent-1')
    await userEvent.type(screen.getByRole('textbox', { name: 'Message' }), 'Start here')
    await userEvent.click(screen.getByRole('button', { name: 'Send message' }))

    await waitFor(() => expect(agentApi.open).toHaveBeenCalledWith('agent-1', { model: 'gpt-6.1-sol', effort: 'medium', projectId: null, readOnly: false }))
    expect(backend.start).not.toHaveBeenCalled()
  })

  it('opens a fresh interview thread without trying to resume its unpersisted rollout', async () => {
    const defaultResume = backend.resume.getMockImplementation()
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[]} as never)
    agentApi.interview.mockResolvedValueOnce({ thread: { id: 'fresh-interview', cwd: '/project', preview: 'New agent interview', updatedAt: 1 } })
    backend.resume.mockImplementation(async id => {
      if (id === 'fresh-interview') throw new Error('no rollout found for thread id')
      return { thread: { id, cwd: '/project', preview: '', updatedAt: 1, turns: [] } }
    })
    render(<App />)

    await userEvent.click(await screen.findByRole('button', { name: 'Agents' }))
    await userEvent.click((await screen.findAllByRole('button', { name: 'New agent' }))[0])
    const message = await screen.findByRole('textbox', { name: 'Message' })
    expect(backend.resume).not.toHaveBeenCalledWith('fresh-interview')
    await userEvent.type(message, 'Help me define a careful release guide')
    await userEvent.click(screen.getByRole('button', { name: 'Send message' }))

    await waitFor(() => expect(backend.send).toHaveBeenCalledWith('fresh-interview', 'Help me define a careful release guide', 'gpt-6.1-sol', 'medium', 'default'))
    expect(screen.queryByText('no rollout found for thread id')).not.toBeInTheDocument()
    backend.resume.mockImplementation(defaultResume!)
  })

  it('opens a fresh named-agent session without resuming before its first turn', async () => {
    const defaultResume = backend.resume.getMockImplementation()
    const profile = { id: 'agent-1', name: 'Release guide', description: '', instructions: '', soul: '', model: 'gpt-6.1-sol', effort: 'high', revision: 1, createdAt: 1, updatedAt: 1 }
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[]} as never)
    agentApi.list.mockResolvedValueOnce({ data: [profile] } as never)
    agentApi.list.mockResolvedValueOnce({ data: [profile] } as never)
    agentApi.open.mockResolvedValueOnce({ thread: { id: 'fresh-agent-session', cwd: '', preview: 'New session', updatedAt: 1 } })
    backend.resume.mockImplementation(async id => {
      if (id === 'fresh-agent-session') throw new Error('no rollout found for thread id')
      return { thread: { id, cwd: '/project', preview: '', updatedAt: 1, turns: [] } }
    })
    render(<App />)

    await userEvent.click(await screen.findByRole('button', { name: 'Agents' }))
    await userEvent.click(await screen.findByRole('button', { name: /Release guide/ }))
    await userEvent.click(await screen.findByRole('button', { name: 'New session' }))
    const message = await screen.findByRole('textbox', { name: 'Message' })
    expect(backend.resume).not.toHaveBeenCalledWith('fresh-agent-session')
    await userEvent.type(message, 'Review this release')
    await userEvent.click(screen.getByRole('button', { name: 'Send message' }))

    await waitFor(() => expect(backend.send).toHaveBeenCalledWith('fresh-agent-session', 'Review this release', 'gpt-6.1-sol', 'high', 'default'))
    expect(screen.queryByText('no rollout found for thread id')).not.toBeInTheDocument()
    backend.resume.mockImplementation(defaultResume!)
  })

  it('shows exact project setup hooks before starting a named agent in a Git project', async () => {
    const agentOpensBefore = agentApi.open.mock.calls.length
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[]} as never)
    agentApi.list.mockResolvedValueOnce({ data: [{ id: 'agent-1', name: 'Release guide', description: '', instructions: '', soul: '', model: 'gpt-6.1-sol', effort: 'medium', revision: 1, createdAt: 1, updatedAt: 1 }] } as never)
    backend.inspectProject.mockResolvedValueOnce({ commands: [{ cwd: '/project', phase: 'post-create', name: 'install', template: 'pnpm install', approved: false }] } as never)
    render(<App />)

    await selectProject()
    await openChatOptions()
    await userEvent.selectOptions(await screen.findByRole('combobox', { name: 'Chat identity' }), 'agent-1')
    await userEvent.type(screen.getByRole('textbox', { name: 'Message' }), 'Review the release')
    await userEvent.click(screen.getByRole('button', { name: 'Send message' }))

    const review = await screen.findByRole('region', { name: 'Worktree setup approval' })
    expect(review).toHaveTextContent('pnpm install')
    expect(agentApi.open.mock.calls.length).toBe(agentOpensBefore)
  })

  it('restores a saved session identity from the thread binding after reload', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[{id:'agent-session', cwd:'/project', preview:'Release review', updatedAt:1, brr:{agentId:'agent-1', kind:'agent'}}]} as never)
    agentApi.list.mockResolvedValueOnce({ data: [{ id: 'agent-1', name: 'Release guide', description: '', instructions: '', soul: '', model: 'gpt-6.1-sol', effort: 'high', revision: 1, createdAt: 1, updatedAt: 1 }] } as never)
    render(<App />)

    await userEvent.click(await screen.findByRole('button', { name: 'Release review' }))

    await openChatOptions()
    const identity = await screen.findByRole('combobox', { name: 'Chat identity' })
    await waitFor(() => expect(identity).toHaveValue('agent-1'))
    expect(identity).toBeDisabled()
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('agent-session'))
  })

  it('turns an exact @mention into a separate task with only the current message', async () => {
    const sendsBefore = backend.send.mock.calls.length
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[]} as never)
    agentApi.list.mockResolvedValueOnce({ data: [{ id: 'agent-1', name: 'Release guide', description: '', instructions: '', soul: '', model: 'gpt-6.1-sol', effort: 'medium', revision: 1, createdAt: 1, updatedAt: 1 }] } as never)
    agentApi.createTask.mockResolvedValueOnce({ id: 'task-1', status: 'queued' })
    render(<App />)

    await selectProject()
    await userEvent.type(screen.getByRole('textbox', { name: 'Message' }), 'Check this change. @Release guide list concrete blockers.')
    await userEvent.click(screen.getByRole('button', { name: 'Send message' }))

    await waitFor(() => expect(agentApi.createTask).toHaveBeenCalledWith(expect.objectContaining({
      agentId: 'agent-1', instructions: 'Check this change. list concrete blockers.', projectId: 'project-main',
      acceptanceCriteria: [expect.any(String)],
    })))
    expect(chatApi.assignTask).toHaveBeenCalledWith(expect.objectContaining({
      ownerId: expect.any(String), document: expect.objectContaining({text: 'Check this change. @Release guide list concrete blockers.'}),
      attachmentIds: [], task: expect.objectContaining({projectId: 'project-main'}),
    }))
    expect(agentApi.createTask.mock.calls[0][0]).not.toHaveProperty('sourceThreadId')
    expect(backend.send.mock.calls.length).toBe(sendsBefore)
  })

  it('switches into planning mode with /plan and sends the command arguments as the prompt', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[]} as never)
    render(<App />)
    await selectProject()
    const message = screen.getByRole('textbox', {name:'Message'})
    await userEvent.type(message, '/plan inspect the authentication flow')
    await userEvent.keyboard('{Enter}')

    await waitFor(() => expect(backend.send).toHaveBeenLastCalledWith('thread-1', 'inspect the authentication flow', 'gpt-6.1-sol', 'medium', 'plan'))
    expect(screen.getByRole('button', {name:'Plan mode'})).toHaveAttribute('aria-pressed', 'true')
  })

  it('uses standalone /plan as a mode toggle without sending a turn', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[]} as never)
        const sendsBefore = backend.send.mock.calls.length
    render(<App />)
    await selectProject()
    const message = screen.getByRole('textbox', {name:'Message'})
    await userEvent.type(message, '/plan{enter}')

    expect(screen.getByRole('button', {name:'Plan mode'})).toHaveAttribute('aria-pressed', 'true')
    expect(message).toBeEmptyDOMElement()
    expect(backend.send.mock.calls.length).toBe(sendsBefore)
  })

  it('restores a saved conversation mode and preserves the draft while Shift+Tab toggles it', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[{id:'thread-plan', cwd:'/project', preview:'Plan thread', updatedAt:1}]} as never)
    backend.resume.mockResolvedValueOnce({thread:{id:'thread-plan', cwd:'/project', preview:'Plan thread', updatedAt:1, collaborationMode:{mode:'plan'}, turns:[]}} as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Plan thread'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-plan'))
    const modeToggle = screen.getByRole('button', {name:'Plan mode'})
    expect(modeToggle).toHaveAttribute('aria-pressed', 'true')

    const message = screen.getByRole('textbox', {name:'Message'})
    await userEvent.type(message, 'Keep this draft')
    await userEvent.keyboard('{Shift>}{Tab}{/Shift}')

    expect(modeToggle).toHaveAttribute('aria-pressed', 'false')
    expect(message).toHaveValue('Keep this draft')

    act(() => events.handler?.({method:'thread/settings/updated', params:{threadId:'thread-plan', threadSettings:{collaborationMode:{mode:'plan'}}}}))
    expect(modeToggle).toHaveAttribute('aria-pressed', 'true')
  })

  it('waits for explicit approval before implementing the current proposed plan', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[{id:'thread-plan', cwd:'/project', preview:'Plan thread', updatedAt:1}]} as never)
    backend.resume.mockResolvedValueOnce({thread:{id:'thread-plan', cwd:'/project', preview:'Plan thread', updatedAt:1, collaborationMode:{mode:'plan'}, turns:[]}} as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Plan thread'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-plan'))
    act(() => events.handler?.({method:'turn/started', params:{threadId:'thread-plan', turn:{id:'turn-plan', status:'inProgress', items:[]}}}))
    act(() => events.handler?.({method:'item/plan/delta', params:{threadId:'thread-plan', turnId:'turn-plan', itemId:'plan-item', delta:'1. Inspect the current flow'}}))
    act(() => events.handler?.({method:'item/completed', params:{threadId:'thread-plan', turnId:'turn-plan', item:{id:'plan-item', type:'plan', text:'1. Inspect the current flow'}}}))
    act(() => events.handler?.({method:'turn/completed', params:{threadId:'thread-plan', turn:{id:'turn-plan', status:'completed', items:[{id:'plan-item', type:'plan', text:'1. Inspect the current flow'}]}}}))

    const implement = await screen.findByRole('button', {name:'Implement plan'})
    expect(implement).toBeEnabled()
    expect(backend.send).not.toHaveBeenCalledWith('thread-plan', 'Implement the plan.', 'gpt-6.1-sol', 'medium', 'default')
    const message = screen.getByRole('textbox', {name:'Message'})
    await userEvent.type(message, 'Unrelated draft')
    expect(implement).toBeDisabled()
    await userEvent.clear(message)
    await userEvent.click(implement)

    await waitFor(() => expect(backend.send).toHaveBeenLastCalledWith('thread-plan', 'Implement the plan.', 'gpt-6.1-sol', 'medium', 'default'))
  })

  it('does not offer implementation for an unfinished streamed plan', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[{id:'thread-plan', cwd:'/project', preview:'Plan thread', updatedAt:1}]} as never)
    backend.resume.mockResolvedValueOnce({thread:{id:'thread-plan', cwd:'/project', preview:'Plan thread', updatedAt:1, collaborationMode:{mode:'plan'}, turns:[]}} as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Plan thread'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-plan'))
    act(() => events.handler?.({method:'turn/started', params:{threadId:'thread-plan', turn:{id:'turn-plan', status:'inProgress', items:[]}}}))
    act(() => events.handler?.({method:'item/plan/delta', params:{threadId:'thread-plan', turnId:'turn-plan', itemId:'plan-item', delta:'1. Inspect the current flow'}}))
    act(() => events.handler?.({method:'turn/completed', params:{threadId:'thread-plan', turn:{id:'turn-plan', status:'completed', items:[]}}}))

    expect(screen.queryByRole('button', {name:'Implement plan'})).not.toBeInTheDocument()
  })

  it('locks collaboration mode while a turn is running, including Shift+Tab', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    act(() => events.handler?.({method:'turn/started', params:{threadId:'thread-1', turn:{id:'turn-active', status:'inProgress', items:[]}}}))
    const modeToggle = screen.getByRole('button', {name:'Plan mode'})
    expect(modeToggle).toBeDisabled()

    fireEvent.keyDown(screen.getByRole('textbox', {name:'Message'}), {key:'Tab', shiftKey:true})

    expect(modeToggle).toHaveAttribute('aria-pressed', 'false')
  })

  it('keeps /plan commands as a draft while a turn is running', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    act(() => events.handler?.({method:'turn/started', params:{threadId:'thread-1', turn:{id:'turn-active', status:'inProgress', items:[]}}}))
    const message = screen.getByRole('textbox', {name:'Message'})
    await userEvent.type(message, '/plan continue with a different approach{enter}')

    expect(await screen.findByRole('alert')).toHaveTextContent('Wait for the current turn to finish')
    expect(message).toHaveValue('/plan continue with a different approach')
    expect(backend.steer).not.toHaveBeenCalledWith('thread-1', '/plan continue with a different approach', 'turn-active')
  })

  it('keeps plan mode and the message draft when a new conversation send fails', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[]} as never)
        backend.send.mockRejectedValueOnce(new Error('turn start failed'))
    render(<App />)
    await selectProject()
    const message = screen.getByRole('textbox', {name:'Message'})
    await userEvent.type(message, 'Plan a safe change')
    await userEvent.click(screen.getByRole('button', {name:'Plan mode'}))
    await userEvent.click(screen.getByRole('button', {name:'Send message'}))

    expect(await screen.findByRole('alert')).toHaveTextContent('turn start failed')
    expect(screen.getByRole('button', {name:'Plan mode'})).toHaveAttribute('aria-pressed', 'true')
    expect(message).toHaveValue('Plan a safe change')
  })

  it('does not show a late send failure in a different conversation', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[
      {id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1},
      {id:'thread-2', cwd:'/project', preview:'Thread two', updatedAt:2},
    ]} as never)
    let rejectSend!: (reason: Error) => void
    backend.send.mockImplementationOnce(() => new Promise((_resolve, reject) => { rejectSend = reject }) as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await userEvent.type(screen.getByRole('textbox', {name:'Message'}), 'Send in the first chat')
    await userEvent.click(screen.getByRole('button', {name:'Send message'}))
    await waitFor(() => expect(backend.send).toHaveBeenCalledWith('thread-1', 'Send in the first chat', 'gpt-6.1-sol', 'medium', 'default'))
    await userEvent.click(screen.getByRole('button', {name:'Thread two'}))

    await act(async () => rejectSend(new Error('late send failure')))

    expect(screen.getByRole('button', {name:'Thread two'})).toHaveClass('selected')
    expect(screen.queryByRole('alert')).not.toBeInTheDocument()
  })

  it('offers only ChatGPT sign-in and prevents sending until signed in', async () => {
    render(<App />)
    expect(screen.getByRole('button', { name: 'Send message' })).toBeDisabled()
    await openProfile()
    const login = await screen.findByRole('button', { name: 'Sign in with ChatGPT' })
    expect(screen.queryByText(/API key/i)).not.toBeInTheDocument()
    await userEvent.click(login)
    expect(backend.login).toHaveBeenCalledOnce()
    expect(await screen.findByText(/Complete sign-in in your browser/)).toBeInTheDocument()
  })

  it('clears pending sign-in and reports a failed login completion', async () => {
    render(<App />)
    await openProfile()
    await userEvent.click(screen.getByRole('button', {name:'Sign in with ChatGPT'}))
    expect(await screen.findByText(/Complete sign-in in your browser/)).toBeInTheDocument()
    expect(screen.getByRole('button', {name:'Sign in with ChatGPT'})).toBeDisabled()
    act(() => events.handler?.({method:'account/login/completed', params:{loginId:'login-1', success:false, error:'Browser sign-in was cancelled', onboardingEntrypoint:null}}))
    expect(await screen.findByRole('alert')).toHaveTextContent('Browser sign-in was cancelled')
    expect(screen.queryByText(/Complete sign-in in your browser/)).not.toBeInTheDocument()
    expect(screen.getByRole('button', {name:'Sign in with ChatGPT'})).toBeEnabled()
  })

  it('cancels the active ChatGPT sign-in and restores the sign-in action', async () => {
    let finishCancel!: (result: unknown) => void
    const cancellationsBefore = backend.cancelLogin.mock.calls.length
    backend.cancelLogin.mockImplementationOnce(() => new Promise(resolve => { finishCancel = resolve }) as never)
    render(<App />)
    await openProfile()
    await userEvent.click(screen.getByRole('button', {name:'Sign in with ChatGPT'}))
    expect(await screen.findByText(/Complete sign-in in your browser/)).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', {name:'Cancel sign-in'}))
    await waitFor(() => expect(backend.cancelLogin).toHaveBeenCalledWith('login-1'))
    const cancelling = screen.getByRole('button', {name:'Cancelling…'})
    expect(cancelling).toBeDisabled()
    await userEvent.click(cancelling)
    expect(backend.cancelLogin.mock.calls.length).toBe(cancellationsBefore + 1)
    await act(async () => finishCancel({status:'canceled'}))
    expect(screen.queryByText(/Complete sign-in in your browser/)).not.toBeInTheDocument()
    expect(screen.getByRole('button', {name:'Sign in with ChatGPT'})).toBeEnabled()
  })

  it('keeps the active login available to retry when cancellation fails', async () => {
    backend.cancelLogin.mockRejectedValueOnce(new Error('Could not cancel sign-in'))
    render(<App />)
    await openProfile()
    await userEvent.click(screen.getByRole('button', {name:'Sign in with ChatGPT'}))
    await screen.findByText(/Complete sign-in in your browser/)
    await userEvent.click(screen.getByRole('button', {name:'Cancel sign-in'}))
    expect(await screen.findByRole('alert')).toHaveTextContent('Could not cancel sign-in')
    expect(screen.getByText(/Complete sign-in in your browser/)).toBeInTheDocument()
    expect(screen.getByRole('button', {name:'Sign in with ChatGPT'})).toBeDisabled()
    expect(screen.getByRole('button', {name:'Cancel sign-in'})).toBeEnabled()
    await userEvent.click(screen.getByRole('button', {name:'Cancel sign-in'}))
    await waitFor(() => expect(backend.cancelLogin).toHaveBeenLastCalledWith('login-1'))
    expect(screen.queryByText(/Complete sign-in in your browser/)).not.toBeInTheDocument()
    expect(screen.getByRole('button', {name:'Sign in with ChatGPT'})).toBeEnabled()
  })

  it('ignores a late completion for a cancelled login while a retry is starting', async () => {
    let finishRetry!: (result: {loginId:string}) => void
    render(<App />)
    await openProfile()
    await userEvent.click(screen.getByRole('button', {name:'Sign in with ChatGPT'}))
    await screen.findByText(/Complete sign-in in your browser/)
    await userEvent.click(screen.getByRole('button', {name:'Cancel sign-in'}))
    await waitFor(() => expect(screen.getByRole('button', {name:'Sign in with ChatGPT'})).toBeEnabled())
    backend.login.mockImplementationOnce(() => new Promise(resolve => { finishRetry = resolve }) as never)
    await userEvent.click(screen.getByRole('button', {name:'Sign in with ChatGPT'}))
    expect(screen.getByRole('button', {name:'Sign in with ChatGPT'})).toBeDisabled()
    act(() => events.handler?.({method:'account/login/completed', params:{loginId:'login-1', success:false, error:'Cancelled', onboardingEntrypoint:null}}))
    expect(screen.getByText(/Complete sign-in in your browser/)).toBeInTheDocument()
    expect(screen.getByRole('button', {name:'Sign in with ChatGPT'})).toBeDisabled()
    await act(async () => finishRetry({loginId:'login-2'}))
    expect(screen.getByRole('button', {name:'Cancel sign-in'})).toBeEnabled()
  })

  it('accepts login completion when cancel reports the attempt already finished', async () => {
    backend.connect.mockResolvedValueOnce({account:null, models:[], threads:[]} as never)
    backend.connect.mockResolvedValueOnce({
      account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[], threads:[],
    } as never)
    backend.cancelLogin.mockResolvedValueOnce({status:'notFound'} as never)
    render(<App />)
    await openProfile()
    await userEvent.click(screen.getByRole('button', {name:'Sign in with ChatGPT'}))
    await screen.findByText(/Complete sign-in in your browser/)
    await userEvent.click(screen.getByRole('button', {name:'Cancel sign-in'}))
    await waitFor(() => expect(backend.cancelLogin).toHaveBeenCalledWith('login-1'))
    act(() => events.handler?.({method:'account/login/completed', params:{loginId:'login-1', success:true, onboardingEntrypoint:null}}))
    expect(await screen.findByText('dev@example.com')).toBeInTheDocument()
  })

  it('follows streamed transcript near the bottom but preserves a scrolled-up reading position', async () => {
    backend.connect.mockResolvedValueOnce({account:null, models:[], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    backend.resume.mockResolvedValueOnce({thread:{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1, turns:[
      {id:'turn-1', status:'inProgress', items:[{id:'message-1', type:'agentMessage', text:'Answer'}]},
    ]}} as never)
    const {container} = render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await screen.findByText('Answer')
    const scroll = container.querySelector('.chat-transcript') as HTMLDivElement
    expect(scroll).not.toBeNull()
    Object.defineProperties(scroll, {
      clientHeight: {configurable:true, value:300},
      scrollHeight: {configurable:true, value:1000},
      scrollTop: {configurable:true, writable:true, value:700},
    })
    fireEvent.scroll(scroll)
    act(() => events.handler?.({method:'item/agentMessage/delta', params:{threadId:'thread-1', turnId:'turn-1', itemId:'message-1', delta:' next'}}))
    expect(await screen.findByText('Answer next')).toBeInTheDocument()
    expect(scroll.scrollTop).toBe(1000)
    scroll.scrollTop = 100
    fireEvent.scroll(scroll)
    act(() => events.handler?.({method:'item/agentMessage/delta', params:{threadId:'thread-1', turnId:'turn-1', itemId:'message-1', delta:' last'}}))
    expect(await screen.findByText('Answer next last')).toBeInTheDocument()
    expect(scroll.scrollTop).toBe(100)
  })

  it('refreshes account and models after successful sign-in', async () => {
    backend.connect.mockResolvedValueOnce({account:null, models:[], threads:[]} as never)
    backend.connect.mockResolvedValueOnce({
      account:{type:'chatgpt', email:'dev@example.com', planType:'plus'},
      models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}],
      threads:[],
    } as never)
    render(<App />)
    await openProfile()
    await userEvent.click(screen.getByRole('button', {name:'Sign in with ChatGPT'}))
    act(() => events.handler?.({method:'account/login/completed', params:{loginId:'login-1', success:true, onboardingEntrypoint:null}}))
    expect(await screen.findByText('dev@example.com')).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', {name:'Home'}))
    expect(screen.getByRole('combobox', {name:'Model'})).toHaveValue('sol')
    await selectProject()
    await userEvent.type(screen.getByRole('textbox', {name:'Message'}), 'Start now that I am signed in')
    await userEvent.click(screen.getByRole('button', {name:'Send message'}))
    await waitFor(() => expect(backend.send).toHaveBeenCalledWith('thread-1', 'Start now that I am signed in', 'gpt-6.1-sol', 'medium', 'default'))
  })

  it('refreshes a revoked account without removing active chats', async () => {
    backend.connect.mockResolvedValueOnce({
      account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[],
      threads:[{id:'thread-1', cwd:'/project', preview:'Existing chat', updatedAt:1}],
    } as never)
    backend.resume.mockResolvedValueOnce({thread:{id:'thread-1', cwd:'/project', preview:'Existing chat', updatedAt:1, turns:[
      {id:'turn-1', status:'completed', items:[{id:'message-1', type:'agentMessage', text:'Existing answer'}]},
    ]}} as never)
    backend.account.mockResolvedValueOnce(null)
    render(<App />)
    await openProfile()
    await screen.findByText('dev@example.com')
    await userEvent.click(screen.getByRole('button', {name:'Home'}))
    await userEvent.click(await screen.findByRole('button', {name:'Existing chat'}))
    expect(await screen.findByText('Existing answer')).toBeInTheDocument()
    act(() => events.handler?.({method:'account/updated', params:{authMode:null, planType:null}}))
    expect(screen.getByRole('button', {name:'Existing chat'})).toBeInTheDocument()
    expect(screen.getByText('Existing answer')).toBeInTheDocument()
    await openProfile()
    await waitFor(() => expect(screen.queryByText('dev@example.com')).not.toBeInTheDocument())
  })

  it('starts a coding turn in the chosen folder with the selected model', async () => {
    backend.connect.mockResolvedValueOnce({account: {type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[]} as never)
    render(<App />)
    await selectProject()
    await userEvent.type(screen.getByRole('textbox', {name:'Message'}), 'Fix the failing tests')
    await userEvent.click(screen.getByRole('button', {name:'Send message'}))
    expect(backend.start).toHaveBeenCalledWith('project-main', 'gpt-6.1-sol', false)
    expect(backend.send).toHaveBeenCalledWith('thread-1', 'Fix the failing tests', 'gpt-6.1-sol', 'medium', 'default')
  })

  it('labels a new conversation with its first message after a successful send', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[]} as never)
    render(<App />)
    await selectProject()
    await userEvent.type(screen.getByRole('textbox', {name:'Message'}), 'HelloWorld')
    await userEvent.click(screen.getByRole('button', {name:'Send message'}))
    expect(await screen.findByRole('button', {name:'HelloWorld'})).toBeInTheDocument()
  })

  it('keeps the first-message label on follow-up messages', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[]} as never)
    const sendsBefore = backend.send.mock.calls.length
    render(<App />)
    await selectProject()
    await userEvent.type(screen.getByRole('textbox', {name:'Message'}), 'First message')
    await userEvent.click(screen.getByRole('button', {name:'Send message'}))
    expect(await screen.findByRole('button', {name:'First message'})).toBeInTheDocument()
    act(() => events.handler?.({method:'turn/completed', params:{threadId:'thread-1', turn:{id:'turn-1', status:'completed', items:[]}}}))
    await userEvent.type(screen.getByRole('textbox', {name:'Message'}), 'Follow-up message')
    await userEvent.click(screen.getByRole('button', {name:'Send message'}))
    await waitFor(() => expect(backend.send.mock.calls.length).toBe(sendsBefore + 2))
    expect(screen.getByRole('button', {name:'First message'})).toBeInTheDocument()
    expect(screen.queryByRole('button', {name:'Follow-up message'})).not.toBeInTheDocument()
  })

  it('does not rename a new chat when its first send fails', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[]} as never)
    backend.send.mockRejectedValueOnce(new Error('send failed'))
    render(<App />)
    await selectProject()
    await userEvent.type(screen.getByRole('textbox', {name:'Message'}), 'Failed first message')
    await userEvent.click(screen.getByRole('button', {name:'Send message'}))
    expect(await screen.findByRole('alert')).toHaveTextContent('send failed')
    expect(screen.getByRole('button', {name:'New chat'})).toBeInTheDocument()
    expect(screen.queryByRole('button', {name:'Failed first message'})).not.toBeInTheDocument()
  })

  it('preserves an existing thread preview after sending', async () => {
    backend.connect.mockResolvedValueOnce({
      account:{type:'chatgpt', email:'dev@example.com', planType:'plus'},
      models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}],
      threads:[{id:'thread-1', cwd:'/project', preview:'Existing preview', updatedAt:1}],
    } as never)
    backend.resume.mockResolvedValueOnce({thread:{id:'thread-1', cwd:'/project', preview:'Existing preview', updatedAt:1, turns:[]}} as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Existing preview'}))
    await userEvent.type(screen.getByRole('textbox', {name:'Message'}), 'Do not replace the preview')
    await userEvent.click(screen.getByRole('button', {name:'Send message'}))
    expect(await screen.findByRole('button', {name:'Existing preview'})).toBeInTheDocument()
    expect(screen.queryByRole('button', {name:'Do not replace the preview'})).not.toBeInTheDocument()
  })

  it('keeps a first-message preview when a quick resume returns empty metadata', async () => {
    backend.connect.mockResolvedValueOnce({
      account:{type:'chatgpt', email:'dev@example.com', planType:'plus'},
      models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}],
      threads:[],
    } as never)
    backend.start.mockResolvedValueOnce({thread:{id:'thread-1', cwd:'/project', preview:'', updatedAt:1}} as never)
    backend.resume.mockResolvedValueOnce({thread:{id:'thread-1', cwd:'/project', preview:'', updatedAt:2, turns:[]}} as never)
    render(<App />)
    await selectProject()
    await userEvent.type(screen.getByRole('textbox', {name:'Message'}), 'First message preview')
    await userEvent.click(screen.getByRole('button', {name:'Send message'}))
    const chat = await screen.findByRole('button', {name:'First message preview'})

    await userEvent.click(chat)
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-1'))

    expect(await screen.findByRole('button', {name:'First message preview'})).toBeInTheDocument()
  })

  it('loads the persisted question after opening a thread, saves a draft, and submits through the question API', async () => {
    backend.connect.mockResolvedValueOnce({
      account: {type:'chatgpt', email:'dev@example.com', planType:'plus'},
      models: [{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}],
      threads: [{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}],
    } as never)
    const question = {id:'q-1',threadId:'thread-1',turnId:'turn-1',itemId:'item-1',kind:'blocking' as const,
      questions:[{id:'scope',header:'Scope',question:'Where should I work?',options:[{label:'Project folder',description:'Use the selected folder'},{label:'Another folder',description:'Choose a different folder'}]}],
      draft:{},resolution:'pending' as const,delivery:'none' as const,createdAt:1,updatedAt:1}
    agentApi.questions.list.mockResolvedValue({data:[question as import('./lib/questions').QuestionRequest]})
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-1'))
    expect(await screen.findAllByText('Where should I work?')).toHaveLength(2)
    expect(await screen.findByRole('radio', {name:/Project folder/})).toBeInTheDocument()
    expect(agentApi.questions.list).toHaveBeenCalledWith({threadId:'thread-1'})
    const initialQuestionLoads = agentApi.questions.list.mock.calls.length
    agentApi.questions.list.mockResolvedValueOnce({data:[question as import('./lib/questions').QuestionRequest]})
    act(() => events.handlers.forEach(handler => handler({method:'brr/tasks/changed',params:{taskId:'task-1'}})))
    await waitFor(() => expect(agentApi.questions.list).toHaveBeenCalledTimes(initialQuestionLoads + 1))
    act(() => events.handlers.forEach(handler => handler({method:'brr/questions/error',params:{message:'Runtime disconnected while sending.'}})))
    expect(await screen.findByRole('alert')).toHaveTextContent('Question delivery needs attention: Runtime disconnected while sending.')
    await userEvent.click(screen.getAllByRole('button',{name:'Dismiss'})[0])
    await userEvent.click(screen.getByRole('radio', {name:/Project folder/}))
    await waitFor(() => expect(agentApi.questions.saveDraft).toHaveBeenCalledWith('q-1',{scope:['Project folder']}))
    await userEvent.click(screen.getByRole('button', {name:'Submit answers'}))
    await waitFor(() => expect(agentApi.questions.answer).toHaveBeenCalledWith('q-1',{scope:['Project folder']}))
    expect(backend.respond).not.toHaveBeenCalled()
  })

  it('replaces the message composer with one selected question form and restores the preserved draft', async () => {
    backend.connect.mockResolvedValueOnce({
      account: {type:'chatgpt', email:'dev@example.com', planType:'plus'},
      models: [{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}],
      threads: [{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}],
    } as never)
    const first = {id:'q-1',threadId:'thread-1',turnId:'turn-1',itemId:'item-1',kind:'blocking' as const,
      questions:[{id:'scope',header:'Scope',question:'Where should I work?',options:[{label:'Project folder',description:'Use the selected folder'}]}],
      draft:{},resolution:'pending' as const,delivery:'none' as const,createdAt:1,updatedAt:1}
    const second = {...first,id:'q-2',itemId:'item-2',questions:[{id:'tests',header:'Tests',question:'Which tests should I run?',options:[{label:'Focused tests',description:'Run only relevant tests'}]}],createdAt:2}
    agentApi.questions.list.mockResolvedValue({data:[first,second] as import('./lib/questions').QuestionRequest[]})
    agentApi.questions.saveDraft.mockImplementation(async (id, answers) => ({
      ...(id === 'q-1' ? first : second), draft:answers,
    }) as never)
    agentApi.questions.answer.mockImplementation(async (id, answers) => ({
      ...(id === 'q-1' ? first : second), answers, resolution:'answered', delivery:'delivered', updatedAt:3,
    }) as never)
    chatApi.records = [{id:'thread-1',threadId:'thread-1',projectId:null,agentId:null,document:{version:1,text:'Keep this thread-owned draft',references:[],quotes:[],attachmentIds:[]},settings:{model:'gpt-6.1-sol',modelId:'sol',effort:'medium',mode:'default'},revision:1,updatedAt:'2026-11-03T00:00:00Z'}]
    render(<App />)
    await userEvent.type(screen.getByRole('textbox', {name:'Message'}), 'Keep this separate new-chat draft')
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-1'))

    expect(await screen.findByRole('region', {name:'Answer agent questions'})).toHaveAttribute('id','question-input-q-1')
    expect(screen.getAllByRole('region', {name:'Answer agent questions'})).toHaveLength(1)
    expect(screen.getAllByRole('radio')).toHaveLength(1)
    expect(screen.queryByRole('textbox', {name:'Message'})).not.toBeInTheDocument()
    expect(screen.queryByRole('button', {name:'Send message'})).not.toBeInTheDocument()

    await userEvent.click(screen.getByRole('button', {name:'Answer questions for Tests'}))
    await waitFor(() => expect(screen.getByRole('region', {name:'Answer agent questions'})).toHaveAttribute('id','question-input-q-2'))
    await waitFor(() => expect(document.activeElement).toBe(screen.getByRole('region', {name:'Answer agent questions'})))
    act(() => events.handlers.forEach(handler => handler({method:'turn/started',params:{threadId:'thread-1',turn:{id:'turn-live',status:'inProgress',items:[]}}})))
    expect(screen.getByRole('button', {name:'Stop'})).toBeInTheDocument()
    expect(screen.getByRole('region', {name:'Answer agent questions'}).parentElement).toContainElement(screen.getByRole('button', {name:'Stop'}))
    await userEvent.click(screen.getByRole('radio', {name:/Focused tests/}))
    await userEvent.click(screen.getByRole('button', {name:'Answer questions for Scope'}))
    await waitFor(() => expect(screen.getByRole('region', {name:'Answer agent questions'})).toHaveAttribute('id','question-input-q-1'))
    await waitFor(() => expect(agentApi.questions.saveDraft).toHaveBeenCalledWith('q-2',{tests:['Focused tests']}))
    await userEvent.click(screen.getByRole('button', {name:'Answer questions for Tests'}))
    await waitFor(() => expect(screen.getByRole('region', {name:'Answer agent questions'})).toHaveAttribute('id','question-input-q-2'))
    expect(screen.getByRole('radio', {name:/Focused tests/})).toBeChecked()
    await userEvent.click(screen.getByRole('button', {name:'Submit answers'}))
    await waitFor(() => expect(agentApi.questions.answer).toHaveBeenCalledWith('q-2',{tests:['Focused tests']}))
    await waitFor(() => expect(screen.getByRole('region', {name:'Answer agent questions'})).toHaveAttribute('id','question-input-q-1'))
    await userEvent.click(screen.getByRole('radio', {name:/Project folder/}))
    await userEvent.click(screen.getByRole('button', {name:'Submit answers'}))
    await waitFor(() => expect(agentApi.questions.answer).toHaveBeenCalledWith('q-1',{scope:['Project folder']}))
    expect(await screen.findByRole('textbox', {name:'Message'})).toHaveValue('Keep this thread-owned draft')
    expect(screen.queryByRole('region', {name:'Answer agent questions'})).not.toBeInTheDocument()
    expect(screen.getByText('Focused tests')).toBeInTheDocument()
    expect(screen.getByText('Project folder')).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', {name:'Draft Keep this separate new-chat draft'}))
    expect(await screen.findByRole('textbox', {name:'Message'})).toHaveValue('Keep this separate new-chat draft')
  })

  it('focuses a resolved question transcript when opening delivery attention from Agents', async () => {
    backend.connect.mockResolvedValueOnce({
      account: {type:'chatgpt', email:'dev@example.com', planType:'plus'}, models: [],
      threads: [{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}],
    } as never)
    const attention = {id:'q-attention',threadId:'thread-1',turnId:'turn-1',itemId:'item-1',kind:'blocking' as const,
      questions:[{id:'scope',header:'Scope',question:'Where should I work?',options:null}],draft:{},
      answers:{scope:['Project folder']},resolution:'answered' as const,delivery:'uncertain' as const,
      reason:'Delivery is still being confirmed.',createdAt:1,updatedAt:2}
    agentApi.decisions.mockResolvedValue({data:[],questions:[attention] as import('./lib/questions').QuestionRequest[]} as never)
    agentApi.questions.list.mockResolvedValue({data:[attention as import('./lib/questions').QuestionRequest]})
    render(<App />)

    await userEvent.click(await screen.findByRole('button', {name:'Agents'}))
    await userEvent.click(await screen.findByRole('button', {name:'Open question'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-1'))
    const transcript = await screen.findByRole('region', {name:'Agent question'})
    await waitFor(() => expect(document.activeElement).toBe(transcript))
    expect(transcript).toHaveAttribute('id','question-q-attention')
    expect(screen.queryByRole('region', {name:'Answer agent questions'})).not.toBeInTheDocument()
    expect(screen.getByRole('textbox', {name:'Message'})).toBeInTheDocument()
    expect(screen.getByText('Delivery is still being confirmed.')).toBeInTheDocument()
  })

  it('restores the message composer and keeps its draft after dismissing the pending question', async () => {
    backend.connect.mockResolvedValueOnce({
      account: {type:'chatgpt', email:'dev@example.com', planType:'plus'}, models: [],
      threads: [{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}],
    } as never)
    const question = {id:'q-dismiss',threadId:'thread-1',turnId:'turn-1',itemId:'item-1',kind:'blocking' as const,
      questions:[{id:'scope',header:'Scope',question:'Where should I work?',options:null}],draft:{},
      resolution:'pending' as const,delivery:'none' as const,createdAt:1,updatedAt:1}
    agentApi.questions.list.mockResolvedValue({data:[question as import('./lib/questions').QuestionRequest]})
    agentApi.questions.dismiss.mockResolvedValue({...question,resolution:'dismissed',updatedAt:2} as never)
    chatApi.records = [{id:'thread-1',threadId:'thread-1',projectId:null,agentId:null,document:{version:1,text:'Thread draft after dismissal',references:[],quotes:[],attachmentIds:[]},settings:{model:'gpt-6.1-sol',modelId:'sol',effort:'medium',mode:'default'},revision:1,updatedAt:'2026-11-03T00:00:00Z'}]
    render(<App />)
    await userEvent.type(screen.getByRole('textbox', {name:'Message'}), 'Keep this separate draft after dismissal')
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-1'))
    await userEvent.click(await screen.findByRole('button', {name:'Dismiss'}))
    await waitFor(() => expect(agentApi.questions.dismiss).toHaveBeenCalledWith('q-dismiss'))
    expect(await screen.findByRole('textbox', {name:'Message'})).toHaveValue('Thread draft after dismissal')
    expect(screen.queryByRole('region', {name:'Answer agent questions'})).not.toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', {name:'Draft Keep this separate draft after dismissal'}))
    expect(await screen.findByRole('textbox', {name:'Message'})).toHaveValue('Keep this separate draft after dismissal')
  })

  it('stops a running turn without submitting the selected question answer', async () => {
    backend.connect.mockResolvedValueOnce({
      account: {type:'chatgpt', email:'dev@example.com', planType:'plus'}, models: [],
      threads: [{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}],
    } as never)
    const question = {id:'q-stop',threadId:'thread-1',turnId:'turn-1',itemId:'item-1',kind:'blocking' as const,
      questions:[{id:'scope',header:'Scope',question:'Where should I work?',options:[{label:'Project folder',description:'Use the selected folder'}]}],draft:{},
      resolution:'pending' as const,delivery:'none' as const,createdAt:1,updatedAt:1}
    agentApi.questions.list.mockResolvedValue({data:[question as import('./lib/questions').QuestionRequest]})
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-1'))
    await screen.findByRole('region', {name:'Answer agent questions'})
    act(() => events.handlers.forEach(handler => handler({method:'turn/started',params:{threadId:'thread-1',turn:{id:'turn-live',status:'inProgress',items:[]}}})))
    await userEvent.click(screen.getByRole('radio', {name:/Project folder/}))
    await userEvent.click(screen.getByRole('button', {name:'Stop'}))

    await waitFor(() => expect(backend.interrupt).toHaveBeenCalledWith('thread-1','turn-live'))
    expect(agentApi.questions.answer).not.toHaveBeenCalled()
    expect(screen.getByRole('region', {name:'Answer agent questions'})).toHaveAttribute('id','question-input-q-stop')
  })

  it('shows the persisted answer summary instead of the request_user_input function output item', async () => {
    backend.connect.mockResolvedValueOnce({
      account: {type:'chatgpt', email:'dev@example.com', planType:'plus'}, models: [],
      threads: [{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}],
    } as never)
    backend.resume.mockResolvedValueOnce({thread:{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1, turns:[{
      id:'turn-1', status:'completed', items:[{id:'question-item', type:'functionCallOutput', name:'request_user_input', output:'{"scope":["Project folder"]}'}],
    }]}} as never)
    agentApi.questions.list.mockResolvedValue({data:[{
      id:'question-answer', threadId:'thread-1', turnId:'turn-1', itemId:'question-item', kind:'blocking',
      questions:[{id:'scope', header:'Scope', question:'Where should I work?', options:null}], draft:{},
      answers:{scope:['Project folder']}, resolution:'answered', delivery:'delivered', createdAt:1, updatedAt:2,
    } as import('./lib/questions').QuestionRequest]})
    render(<App />)

    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-1'))

    expect(await screen.findByText('Where should I work?')).toBeInTheDocument()
    expect(screen.getByText('Project folder')).toBeInTheDocument()
    expect(screen.queryByText('Function Call Output')).not.toBeInTheDocument()
    expect(screen.queryByText('{"scope":["Project folder"]}')).not.toBeInTheDocument()
  })

  it('keeps the runtime user-input request separate from the persisted inline question', async () => {
    backend.connect.mockResolvedValueOnce({
      account: {type:'chatgpt', email:'dev@example.com', planType:'plus'}, models: [],
      threads: [{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}],
    } as never)
    const question = {
      id:'question-persisted', threadId:'thread-1', turnId:'turn-1', itemId:'question-item', kind:'blocking' as const,
      questions:[{id:'scope', header:'Scope', question:'Where should I work?', options:null}], draft:{},
      resolution:'pending' as const, delivery:'none' as const, createdAt:1, updatedAt:1,
    }
    agentApi.questions.list.mockResolvedValue({data:[]})
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await waitFor(() => expect(agentApi.questions.list).toHaveBeenCalledWith({threadId:'thread-1'}))

    agentApi.questions.list.mockResolvedValueOnce({data:[question as import('./lib/questions').QuestionRequest]})
    act(() => events.handlers.forEach(handler => handler({method:'item/tool/requestUserInput', id:'raw-request', params:{
      threadId:'thread-1', turnId:'turn-1', itemId:'question-item', questions:question.questions,
    }})))
    act(() => events.handlers.forEach(handler => handler({method:'brr/questions/changed', params:{threadId:'thread-1', requestId:question.id}})))

    expect(await screen.findAllByText('Where should I work?')).toHaveLength(2)
    expect(screen.queryByText('Input required')).not.toBeInTheDocument()
    expect(screen.queryByRole('complementary', {name:'Turn activity'})).not.toBeInTheDocument()
  })

  it('counts a waiting-input task and its stored question once in the accessible Agents badge', async () => {
    const question = {id:'q-2',threadId:'thread-2',turnId:'turn-1',itemId:'item-2',taskId:'task-2',kind:'blocking' as const,
      questions:[{id:'scope',header:'Scope',question:'Which scope?',options:null}],draft:{},resolution:'pending' as const,delivery:'none' as const,createdAt:1,updatedAt:1}
    agentApi.decisions.mockResolvedValue({data:[],questions:[question as import('./lib/questions').QuestionRequest]} as never)
    agentApi.tasks.mockResolvedValue({data:[{id:'task-2',status:'waiting_input'}]} as never)
    render(<App />)

    expect(await screen.findByLabelText('1 agent alerts')).toBeInTheDocument()
  })

  it('shows requested permissions and can allow once or decline for this turn', async () => {
    backend.connect.mockResolvedValueOnce({
      account: {type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[],
      threads: [{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}],
    } as never)
    backend.resume.mockResolvedValueOnce({thread:{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1, turns:[]}})
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-1'))
    const requested = {network:{enabled:true}, fileSystem:{read:['/project']}}
    act(() => events.handler?.({method:'item/permissions/requestApproval', id:91, params:{threadId:'thread-1', turnId:'turn-1', itemId:'item-1', permissions:requested, cwd:'/project'}}))

    expect(await screen.findByText('Permissions requested')).toBeInTheDocument()
    expect(screen.getByText(/\/project/)).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', {name:'Allow once'}))
    await waitFor(() => expect(backend.respond).toHaveBeenCalledWith(91, {permissions:requested, scope:'turn'}))

    act(() => events.handler?.({method:'item/permissions/requestApproval', id:92, params:{threadId:'thread-1', turnId:'turn-1', itemId:'item-2', permissions:requested, cwd:'/project'}}))
    await userEvent.click(await screen.findByRole('button', {name:'Decline'}))
    await waitFor(() => expect(backend.respond).toHaveBeenCalledWith(92, {permissions:{}, scope:'turn'}))
  })

  it('responds to command and file approvals with their decision values', async () => {
    backend.connect.mockResolvedValueOnce({account:null, models:[], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-1'))
    act(() => events.handler?.({method:'item/commandExecution/requestApproval', id:31, params:{threadId:'thread-1', turnId:'turn-1', itemId:'cmd-1', command:'pnpm test'}}))
    expect(await screen.findByText('Command approval requested')).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', {name:'Allow'}))
    await waitFor(() => expect(backend.respond).toHaveBeenCalledWith(31, {decision:'accept'}))
    act(() => events.handler?.({method:'item/fileChange/requestApproval', id:'file-request', params:{threadId:'thread-1', turnId:'turn-1', itemId:'file-1', reason:'Write a source file'}}))
    expect(await screen.findByText('File change approval requested')).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', {name:'Decline'}))
    await waitFor(() => expect(backend.respond).toHaveBeenCalledWith('file-request', {decision:'decline'}))
  })

  it('loads thread history when background streaming created a partial session and keeps newer deltas', async () => {
    backend.connect.mockResolvedValueOnce({account:null, models:[], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    let finishResume!: (response: unknown) => void
    backend.resume.mockImplementationOnce(() => new Promise(resolve => { finishResume = resolve }) as never)
    render(<App />)
    await screen.findByRole('button', {name:'Thread one'})
    await waitFor(() => expect(events.handler).toBeTypeOf('function'))
    act(() => events.handler?.({method:'item/agentMessage/delta', params:{threadId:'thread-1', turnId:'turn-2', itemId:'live-message', delta:'live one'}}))
    await userEvent.click(screen.getByRole('button', {name:'Thread one'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-1'))
    act(() => events.handler?.({method:'item/agentMessage/delta', params:{threadId:'thread-1', turnId:'turn-2', itemId:'live-message', delta:' live two'}}))
    await act(async () => finishResume({thread:{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1, turns:[
      {id:'turn-1', status:'completed', items:[{id:'history-message', type:'agentMessage', text:'Past answer'}]},
      {id:'turn-2', status:'inProgress', items:[{id:'live-message', type:'agentMessage', text:'snapshot value'}]},
    ]}}))

    expect(await screen.findByText('Past answer')).toBeInTheDocument()
    expect(screen.getByText('live one live two')).toBeInTheDocument()
  })

  it('does not truncate text already included in a resume snapshot', async () => {
    backend.connect.mockResolvedValueOnce({account:null, models:[], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    let finishResume!: (response: unknown) => void
    backend.resume.mockImplementationOnce(() => new Promise(resolve => { finishResume = resolve }) as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-1'))
    act(() => events.handler?.({method:'item/agentMessage/delta', params:{threadId:'thread-1', turnId:'turn-1', itemId:'message-1', delta:'Hello'}}))
    await act(async () => finishResume({thread:{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1, turns:[
      {id:'turn-1', status:'inProgress', items:[{id:'message-1', type:'agentMessage', text:'Hello world'}]},
    ]}}))
    expect(await screen.findByText('Hello world')).toBeInTheDocument()
  })

  it('does not replace snapshot text with a streamed suffix', async () => {
    backend.connect.mockResolvedValueOnce({account:null, models:[], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    let finishResume!: (response: unknown) => void
    backend.resume.mockImplementationOnce(() => new Promise(resolve => { finishResume = resolve }) as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-1'))
    act(() => events.handler?.({method:'item/agentMessage/delta', params:{threadId:'thread-1', turnId:'turn-1', itemId:'message-1', delta:' world'}}))
    await act(async () => finishResume({thread:{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1, turns:[
      {id:'turn-1', status:'inProgress', items:[{id:'message-1', type:'agentMessage', text:'Hello world'}]},
    ]}}))
    expect(await screen.findByText('Hello world')).toBeInTheDocument()
  })

  it('joins overlapping live text onto the hydrated snapshot', async () => {
    backend.connect.mockResolvedValueOnce({account:null, models:[], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    let finishResume!: (response: unknown) => void
    backend.resume.mockImplementationOnce(() => new Promise(resolve => { finishResume = resolve }) as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-1'))
    act(() => events.handler?.({method:'item/agentMessage/delta', params:{threadId:'thread-1', turnId:'turn-1', itemId:'message-1', delta:'world!'}}))
    await act(async () => finishResume({thread:{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1, turns:[
      {id:'turn-1', status:'inProgress', items:[{id:'message-1', type:'agentMessage', text:'Hello world'}]},
    ]}}))
    expect(await screen.findByText('Hello world!')).toBeInTheDocument()
  })

  it('does not revive a completed turn from a buffered start event', async () => {
    backend.connect.mockResolvedValueOnce({account:null, models:[], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    let finishResume!: (response: unknown) => void
    backend.resume.mockImplementationOnce(() => new Promise(resolve => { finishResume = resolve }) as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-1'))
    act(() => events.handler?.({method:'turn/started', params:{threadId:'thread-1', turn:{id:'turn-1', status:'inProgress', items:[]}}}))
    await act(async () => finishResume({thread:{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1, turns:[
      {id:'turn-1', status:'completed', items:[]},
    ]}}))
    expect(screen.queryByRole('button', {name:'Stop'})).not.toBeInTheDocument()
  })

  it('keeps completed item status when stale in-progress events arrive during resume', async () => {
    backend.connect.mockResolvedValueOnce({account:null, models:[], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    let finishResume!: (response: unknown) => void
    backend.resume.mockImplementationOnce(() => new Promise(resolve => { finishResume = resolve }) as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-1'))
    act(() => events.handler?.({method:'item/started', params:{threadId:'thread-1', turnId:'turn-1', item:{id:'command-1', type:'commandExecution', status:'inProgress', command:'pnpm test'}}}))
    await act(async () => finishResume({thread:{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1, turns:[
      {id:'turn-1', status:'completed', items:[{id:'command-1', type:'commandExecution', status:'completed', command:'pnpm test', exitCode:0}]},
    ]}}))
    expect(await screen.findByText('Command · completed')).toBeInTheDocument()
  })

  it('starts read-only work and resumes without overriding the thread sandbox', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[]} as never)
    render(<App />)
    await selectProject()
    await openChatOptions()
    await userEvent.click(screen.getByRole('checkbox', {name:'Read only'}))
    await userEvent.type(screen.getByRole('textbox', {name:'Message'}), 'Review this project')
    await userEvent.click(screen.getByRole('button', {name:'Send message'}))
    await waitFor(() => expect(backend.start).toHaveBeenCalledWith('project-main', 'gpt-6.1-sol', true))
    await userEvent.click(await screen.findByRole('button', {name:'Review this project'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-1'))
    expect(backend.resume).toHaveBeenLastCalledWith('thread-1')
  })

  it('keeps a completed turn complete when its send response arrives later', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[]} as never)
    let finishSend!: (response: unknown) => void
    backend.send.mockImplementationOnce(() => new Promise(resolve => { finishSend = resolve }) as never)
    render(<App />)
    await selectProject()
    const sendsBefore = backend.send.mock.calls.length
    await selectProject()
    await userEvent.type(screen.getByRole('textbox', {name:'Message'}), 'Make a change')
    await userEvent.click(screen.getByRole('button', {name:'Send message'}))
    await waitFor(() => expect(backend.send).toHaveBeenCalled())
    act(() => events.handler?.({method:'turn/completed', params:{threadId:'thread-1', turn:{id:'turn-fast', status:'completed', error:null, items:[{id:'answer-fast', type:'agentMessage', text:'Finished'}]}}}))
    await screen.findByText('Finished')
    await act(async () => finishSend({turn:{id:'turn-fast'}}))
    expect(screen.queryByRole('button', {name:'Stop'})).not.toBeInTheDocument()
    expect(backend.send.mock.calls.length).toBe(sendsBefore + 1)
  })

  it('shows engine disconnect state and offers a reconnect action', async () => {
    render(<App />)
    await openProfile()
    await waitFor(() => expect(events.handler).toBeTypeOf('function'))
    act(() => events.handler?.({method:'brr/disconnected', params:{reason:'engine stopped'}}))
    expect(await screen.findByRole('button', {name:'Reconnect'})).toBeInTheDocument()
    expect(screen.getByRole('status')).toHaveTextContent(/coding engine disconnected/i)
    const callsBeforeRetry = backend.connect.mock.calls.length
    await userEvent.click(screen.getByRole('button', {name:'Reconnect'}))
    await waitFor(() => expect(backend.connect.mock.calls.length).toBe(callsBeforeRetry + 1))
  })

  it('allows sign-in to retry after disconnecting during login', async () => {
    render(<App />)
    await openProfile()
    await userEvent.click(screen.getByRole('button', {name:'Sign in with ChatGPT'}))
    expect(await screen.findByText(/Complete sign-in in your browser/)).toBeInTheDocument()
    act(() => events.handler?.({method:'brr/disconnected', params:{reason:'engine stopped'}}))
    await userEvent.click(await screen.findByRole('button', {name:'Reconnect'}))
    const login = await screen.findByRole('button', {name:'Sign in with ChatGPT'})
    await waitFor(() => expect(login).toBeEnabled())
  })

  it('reloads the active thread after reconnecting during a running turn', async () => {
    backend.connect.mockResolvedValueOnce({account:null, models:[], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    backend.resume.mockResolvedValueOnce({thread:{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1, turns:[]}})
    backend.resume.mockResolvedValueOnce({thread:{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1, turns:[{id:'turn-live', status:'completed', items:[]}]}} as never)
    render(<App />)
    const resumesBefore = backend.resume.mock.calls.length
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await waitFor(() => expect(backend.resume.mock.calls.length).toBe(resumesBefore + 1))
    act(() => events.handler?.({method:'turn/started', params:{threadId:'thread-1', turn:{id:'turn-live', status:'inProgress', items:[]}}}))
    expect(screen.getByRole('button', {name:'Stop'})).toBeInTheDocument()
    act(() => events.handler?.({method:'brr/disconnected', params:{reason:'engine stopped'}}))
    await openProfile()
    await userEvent.click(await screen.findByRole('button', {name:'Reconnect'}))
    await waitFor(() => expect(backend.resume.mock.calls.length).toBe(resumesBefore + 2))
    expect(backend.resume).toHaveBeenLastCalledWith('thread-1')
    await userEvent.click(screen.getByRole('button', {name:'Home'}))
    expect(screen.queryByRole('button', {name:'Stop'})).not.toBeInTheDocument()
  })

  it('does not connect or subscribe in browser preview', async () => {
    backend.desktop = false
    const connectsBefore = backend.connect.mock.calls.length
    const subscribesBefore = backend.subscribe.mock.calls.length
    render(<App />)
    expect(await screen.findByText(/Connect from the desktop app/)).toBeInTheDocument()
    expect(backend.connect).toHaveBeenCalledTimes(connectsBefore)
    expect(backend.subscribe).toHaveBeenCalledTimes(subscribesBefore)
    expect(screen.queryByRole('alert')).not.toBeInTheDocument()
    backend.desktop = true
  })

  it('does not submit an IME composition when Enter is pressed', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[]} as never)
    render(<App />)
    await selectProject()
    const message = screen.getByRole('textbox', {name:'Message'})
    await userEvent.type(message, '日本語')
    const sendsBefore = backend.send.mock.calls.length
    fireEvent.keyDown(message, {key:'Enter', code:'Enter', isComposing:true})
    expect(backend.send.mock.calls.length).toBe(sendsBefore)
  })

  it('keeps a blocking approval in chat while workspace tools are open', async () => {
    backend.connect.mockResolvedValueOnce({account:null, models:[], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-1'))
    const newTab = screen.getByRole('button', {name:'New tab'})
    expect(newTab).toHaveAttribute('title', 'New tab')
    await userEvent.click(newTab)
    act(() => events.handler?.({method:'item/commandExecution/requestApproval', id:'blocking-approval', params:{threadId:'thread-1', turnId:'turn-1', itemId:'command-1', command:'pnpm test', cwd:'/project', reason:'Run the project tests with network access'}}))
    expect(await screen.findByText('Command approval requested')).toBeInTheDocument()
    expect(screen.getByText('pnpm test')).toBeInTheDocument()
    expect(screen.getAllByText('/project').length).toBeGreaterThan(0)
    expect(screen.getByText('Run the project tests with network access')).toBeInTheDocument()
  })

  it('keeps Send disabled while thread history is loading', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    let finishResume!: (response: unknown) => void
    backend.resume.mockImplementationOnce(() => new Promise(resolve => { finishResume = resolve }) as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await userEvent.type(screen.getByRole('textbox', {name:'Message'}), 'Wait for history')
    expect(screen.getByRole('button', {name:'Send message'})).toBeDisabled()
    await act(async () => finishResume({thread:{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1, turns:[]}}))
  })

  it('starts a new chat under a different project after opening an existing thread', async () => {
    const other = {...mainProject, id: 'project-other', name: 'other', folders: ['/other'], primaryFolder: '/other'}
    backend.projects.mockResolvedValueOnce({data: [mainProject, other]} as never)
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[{id:'old-thread', projectId: 'project-main', cwd:'/existing', preview:'Existing', updatedAt:1}]} as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Existing'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('old-thread'))
    await userEvent.click(screen.getByRole('button', {name:'New conversation'}))
    await selectProject('project-other')
    await userEvent.type(screen.getByRole('textbox', {name:'Message'}), 'Use the other project')
    await userEvent.click(screen.getByRole('button', {name:'Send message'}))
    await waitFor(() => expect(backend.start).toHaveBeenCalledWith('project-other', 'gpt-6.1-sol', false))
  })

  it('steers the active turn when Enter is pressed and keeps Stop available', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    backend.steer.mockResolvedValueOnce({turnId:'turn-live'}).mockResolvedValueOnce({turnId:'turn-live'})
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-1'))
    await waitFor(() => expect(events.handler).toBeTypeOf('function'))
    act(() => events.handler?.({method:'turn/started', params:{threadId:'thread-1', turn:{id:'turn-live', status:'inProgress', items:[]}}}))
    const sendsBefore = backend.send.mock.calls.length
    const startsBefore = backend.start.mock.calls.length
    const message = screen.getByRole('textbox', {name:'Message'})
    await userEvent.type(message, 'Please adjust the current work')
    await userEvent.keyboard('{Control>}{Enter}{/Control}')
    expect(screen.getByRole('button', {name:'Stop'})).toBeInTheDocument()
    expect(backend.send.mock.calls.length).toBe(sendsBefore)
    await waitFor(() => expect(backend.steer).toHaveBeenLastCalledWith('thread-1', 'Please adjust the current work', 'turn-live'))
    expect(backend.start.mock.calls.length).toBe(startsBefore)
    expect(screen.getByRole('button', {name:'Stop'})).toBeInTheDocument()
    act(() => events.handler?.({method:'item/started', params:{
      threadId:'thread-1', turnId:'turn-live',
      item:{id:'steered-user-message', type:'userMessage', content:[{type:'text', text:'Please adjust the current work'}]},
    }}))
    expect(await screen.findByText('Please adjust the current work')).toBeInTheDocument()

    await userEvent.type(message, 'Use the click control')
    await userEvent.keyboard('{Control>}{Enter}{/Control}')
    await waitFor(() => expect(backend.steer).toHaveBeenLastCalledWith('thread-1', 'Use the click control', 'turn-live'))
    expect(screen.getByRole('button', {name:'Stop'})).toBeInTheDocument()
  })

  it('keeps Stop usable during steering and preserves a newer draft when steering resolves', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    let finishSteer!: (response: {turnId:string}) => void
    backend.steer.mockImplementationOnce(() => new Promise(resolve => { finishSteer = resolve }) as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    act(() => events.handler?.({method:'turn/started', params:{threadId:'thread-1', turn:{id:'turn-active', status:'inProgress', items:[]}}}))
    const message = screen.getByRole('textbox', {name:'Message'})
    await userEvent.type(message, 'Steer this turn')
    await userEvent.keyboard('{Control>}{Enter}{/Control}')
    await waitFor(() => expect(backend.steer).toHaveBeenLastCalledWith('thread-1', 'Steer this turn', 'turn-active'))

    const stop = screen.getByRole('button', {name:'Stop'})
    expect(stop).toBeEnabled()
    await userEvent.click(stop)
    await waitFor(() => expect(backend.interrupt).toHaveBeenLastCalledWith('thread-1', 'turn-active'))
    await userEvent.clear(message)
    await userEvent.type(message, 'A newer draft')
    await act(async () => finishSteer({turnId:'turn-active'}))

    expect(message).toHaveValue('A newer draft')
    expect(screen.getByRole('button', {name:'Stop'})).toBeInTheDocument()
  })

  it('clears an acknowledged steer when its turn completes before the reply', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    let finishSteer!: (response: {turnId:string}) => void
    backend.steer.mockImplementationOnce(() => new Promise(resolve => { finishSteer = resolve }) as never)
    const sendsBefore = backend.send.mock.calls.length
    const startsBefore = backend.start.mock.calls.length
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    act(() => events.handler?.({method:'turn/started', params:{threadId:'thread-1', turn:{id:'turn-active', status:'inProgress', items:[]}}}))
    const message = screen.getByRole('textbox', {name:'Message'})
    await userEvent.type(message, 'Keep going with the current approach')
    await userEvent.keyboard('{Control>}{Enter}{/Control}')
    await waitFor(() => expect(backend.steer).toHaveBeenLastCalledWith('thread-1', 'Keep going with the current approach', 'turn-active'))
    act(() => events.handler?.({method:'turn/completed', params:{threadId:'thread-1', turn:{id:'turn-active', status:'completed', items:[]}}}))

    await act(async () => finishSteer({turnId:'turn-active'}))

    expect(message).toBeEmptyDOMElement()
    expect(backend.send.mock.calls.length).toBe(sendsBefore)
    expect(backend.start.mock.calls.length).toBe(startsBefore)
  })

  it('does not start another turn on Enter while a completed turn steer is pending', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    let finishSteer!: (response: {turnId:string}) => void
    backend.steer.mockImplementationOnce(() => new Promise(resolve => { finishSteer = resolve }) as never)
    const sendsBefore = backend.send.mock.calls.length
    const startsBefore = backend.start.mock.calls.length
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    act(() => events.handler?.({method:'turn/started', params:{threadId:'thread-1', turn:{id:'turn-active', status:'inProgress', items:[]}}}))
    const message = screen.getByRole('textbox', {name:'Message'})
    await userEvent.type(message, 'Continue after completion')
    await userEvent.keyboard('{Control>}{Enter}{/Control}')
    await waitFor(() => expect(backend.steer).toHaveBeenLastCalledWith('thread-1', 'Continue after completion', 'turn-active'))
    act(() => events.handler?.({method:'turn/completed', params:{threadId:'thread-1', turn:{id:'turn-active', status:'completed', items:[]}}}))

    await userEvent.type(message, '{enter}')

    expect(backend.send.mock.calls.length).toBe(sendsBefore)
    expect(backend.start.mock.calls.length).toBe(startsBefore)
    expect(message).toHaveValue('Continue after completion')
    await act(async () => finishSteer({turnId:'turn-active'}))
  })

  it('shows a late no-active-turn rejection after completion and keeps its draft', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    let rejectSteer!: (reason: Error) => void
    backend.steer.mockImplementationOnce(() => new Promise((_resolve, reject) => { rejectSteer = reject }) as never)
    const sendsBefore = backend.send.mock.calls.length
    const startsBefore = backend.start.mock.calls.length
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    act(() => events.handler?.({method:'turn/started', params:{threadId:'thread-1', turn:{id:'turn-active', status:'inProgress', items:[]}}}))
    const message = screen.getByRole('textbox', {name:'Message'})
    await userEvent.type(message, 'Keep this for a new turn')
    await userEvent.keyboard('{Control>}{Enter}{/Control}')
    await waitFor(() => expect(backend.steer).toHaveBeenLastCalledWith('thread-1', 'Keep this for a new turn', 'turn-active'))
    act(() => events.handler?.({method:'turn/completed', params:{threadId:'thread-1', turn:{id:'turn-active', status:'completed', items:[]}}}))

    await act(async () => rejectSteer(new Error('no active turn')))

    expect(await screen.findByRole('alert')).toHaveTextContent('no active turn')
    expect(message).toHaveValue('Keep this for a new turn')
    expect(backend.send.mock.calls.length).toBe(sendsBefore)
    expect(backend.start.mock.calls.length).toBe(startsBefore)
  })

  it('does not surface a late steer error over a newer draft', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    let rejectSteer!: (reason: Error) => void
    backend.steer.mockImplementationOnce(() => new Promise((_resolve, reject) => { rejectSteer = reject }) as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    act(() => events.handler?.({method:'turn/started', params:{threadId:'thread-1', turn:{id:'turn-active', status:'inProgress', items:[]}}}))
    const message = screen.getByRole('textbox', {name:'Message'})
    await userEvent.type(message, 'Submitted instruction')
    await userEvent.keyboard('{Control>}{Enter}{/Control}')
    await waitFor(() => expect(backend.steer).toHaveBeenLastCalledWith('thread-1', 'Submitted instruction', 'turn-active'))
    await userEvent.clear(message)
    await userEvent.type(message, 'A newer instruction')

    await act(async () => rejectSteer(new Error('no active turn')))

    expect(message).toHaveValue('A newer instruction')
    expect(screen.queryByRole('alert')).not.toBeInTheDocument()
  })

  it('does not clear another thread draft when a pending steer resolves', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[
      {id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1},
      {id:'thread-2', cwd:'/project', preview:'Thread two', updatedAt:2},
    ]} as never)
    let finishSteer!: (response: {turnId:string}) => void
    backend.steer.mockImplementationOnce(() => new Promise(resolve => { finishSteer = resolve }) as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    act(() => events.handler?.({method:'turn/started', params:{threadId:'thread-1', turn:{id:'turn-one', status:'inProgress', items:[]}}}))
    const message = screen.getByRole('textbox', {name:'Message'})
    await userEvent.type(message, 'Steer thread one')
    await userEvent.keyboard('{Control>}{Enter}{/Control}')
    await waitFor(() => expect(backend.steer).toHaveBeenLastCalledWith('thread-1', 'Steer thread one', 'turn-one'))

    await userEvent.click(screen.getByRole('button', {name:'Thread two'}))
    const threadTwoMessage = screen.getByRole('textbox', {name:'Message'})
    await userEvent.clear(threadTwoMessage)
    await userEvent.type(threadTwoMessage, 'Draft for thread two')
    await act(async () => finishSteer({turnId:'turn-one'}))

    expect(screen.getByRole('textbox', {name:'Message'})).toHaveValue('Draft for thread two')
    expect(screen.getByRole('button', {name:'Thread two'})).toHaveClass('selected')
  })

  it('keeps a failed steering draft and never falls back to a new turn', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    backend.steer.mockRejectedValueOnce(new Error('active turn changed'))
    const sendsBefore = backend.send.mock.calls.length
    const startsBefore = backend.start.mock.calls.length
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    act(() => events.handler?.({method:'turn/started', params:{threadId:'thread-1', turn:{id:'turn-active', status:'inProgress', items:[]}}}))
    const message = screen.getByRole('textbox', {name:'Message'})
    await userEvent.type(message, 'Keep this instruction')
    await userEvent.keyboard('{Control>}{Enter}{/Control}')

    expect(await screen.findByRole('alert')).toHaveTextContent('active turn changed')
    expect(message).toHaveValue('Keep this instruction')
    expect(backend.send.mock.calls.length).toBe(sendsBefore)
    expect(backend.start.mock.calls.length).toBe(startsBefore)
  })

  it('keeps Stop targeted at the active turn after a prior turn delta arrives late', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-1'))
    act(() => events.handler?.({method:'turn/started', params:{threadId:'thread-1', turn:{id:'turn-old', status:'inProgress', items:[]}}}))
    act(() => events.handler?.({method:'turn/completed', params:{threadId:'thread-1', turn:{id:'turn-old', status:'interrupted', items:[]}}}))
    act(() => events.handler?.({method:'turn/started', params:{threadId:'thread-1', turn:{id:'turn-current', status:'inProgress', items:[]}}}))

    act(() => events.handler?.({method:'item/agentMessage/delta', params:{threadId:'thread-1', turnId:'turn-old', itemId:'old-message', delta:'late old text'}}))
    await userEvent.click(screen.getByRole('button', {name:'Stop'}))

    await waitFor(() => expect(backend.interrupt).toHaveBeenLastCalledWith('thread-1', 'turn-current'))
  })
})
