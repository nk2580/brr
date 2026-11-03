import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import ReactMarkdown from 'react-markdown'
import remarkGfm from 'remark-gfm'
import { X, Plus, Search, MessageSquare, Clock3, Bot } from 'lucide-react'
import { client, type Model, type Thread, type ProjectRecord } from '../lib/client'
import { agents, type AgentProfile, type DelegationProposal, type LearnedSkill, type MemoryRecord, type MemorySource, type ProfileDraft, type RuntimeRequest, type Task, type Workflow, type WorkspaceSetupCommand } from '../lib/agents'
import { ticketOperationId, ticketProposalProjectSnapshot, tickets as ticketApi, type TicketRunProposal } from '../lib/tickets'
import { Button } from './ui/button'
import { getQuestionStatus, type QuestionRequest } from '../lib/questions'
import './AgentsPanel.css'

type Props = { models: Model[]; desktop: boolean; online: boolean; projectId?: string | null; cwd?: string; chooseFolder?: () => Promise<string | null>; questionError?: string; onClearQuestionError?: () => void; onOpenThread: (thread: Thread, agentId?: string, options?: { readOnly?: boolean; fresh?: boolean; questionId?: string }) => void; onClose: () => void }
type Tab = 'chat' | 'tasks' | 'memory' | 'skills' | 'profile'
type WorkspaceReview = { cwd: string; commands: WorkspaceSetupCommand[] }
const errorText = (error: unknown) => error instanceof Error ? error.message : String(error)
const Markdown = ({ children }: { children: string }) => <div className="agents-markdown"><ReactMarkdown remarkPlugins={[remarkGfm]}>{children}</ReactMarkdown></div>

export function AgentsPanel({ models, desktop, online, projectId: activeProjectId, cwd, chooseFolder, questionError, onClearQuestionError, onOpenThread, onClose }: Props) {
  const [profiles, setProfiles] = useState<AgentProfile[]>([])
  const [selectedId, setSelectedId] = useState('')
  const [tab, setTab] = useState<Tab>('chat')
  const [search, setSearch] = useState('')
  const [sessions, setSessions] = useState<Thread[]>([])
  const [drafts, setDrafts] = useState<ProfileDraft[]>([])
  const [tasks, setTasks] = useState<Task[]>([])
  const [proposals, setProposals] = useState<DelegationProposal[]>([])
  const [ticketProposals, setTicketProposals] = useState<TicketRunProposal[]>([])
  const [workflows, setWorkflows] = useState<Workflow[]>([])
  const [memories, setMemories] = useState<MemoryRecord[]>([])
  const [memorySources, setMemorySources] = useState<MemorySource[]>([])
  const [skills, setSkills] = useState<LearnedSkill[]>([])
  const [projects, setProjects] = useState<ProjectRecord[]>([])
  const [projectId, setProjectId] = useState('')
  const [runtimeRequests, setRuntimeRequests] = useState<RuntimeRequest[]>([])
  const [questions, setQuestions] = useState<QuestionRequest[]>([])
  const [taskNextCursor, setTaskNextCursor] = useState<string | null>(null)
  const [workspaceReview, setWorkspaceReview] = useState<WorkspaceReview | null>(null)
  const [settings, setSettings] = useState({ maxBackground: 4, maxPerAgent: 2 })
  const [busy, setBusy] = useState('')
  const [error, setError] = useState('')
  const [notice, setNotice] = useState('')
  const [memoryQuery, setMemoryQuery] = useState('')
  const [memoryUndo, setMemoryUndo] = useState<number | null>(null)
  const [taskProjectId, setTaskProjectId] = useState(activeProjectId ?? '')
  const [taskCwd, setTaskCwd] = useState(cwd ?? '')
  const generation = useRef(0)
  const ticketProposalOperations = useRef(new Map<string, string>())
  const profilesRef = useRef<AgentProfile[]>(profiles)
  profilesRef.current = profiles
  const selected = profiles.find(profile => profile.id === selectedId) ?? null
  const currentModel = models.find(model => model.isDefault) ?? models[0]
  const visibleProfiles = useMemo(() => profiles.filter(profile => `${profile.name} ${profile.description}`.toLocaleLowerCase().includes(search.toLocaleLowerCase())), [profiles, search])
  useEffect(() => {setTaskCwd(cwd ?? ''); setTaskProjectId(activeProjectId ?? '')}, [cwd, activeProjectId])

  const refreshProfiles = useCallback(async () => {
    const response = await agents.list()
    // Keep the newest approved profile when list and approval requests resolve out of order.
    const merged = [...response.data]
    for (const profile of profilesRef.current) {
      const index = merged.findIndex(item => item.id === profile.id)
      if (index < 0) merged.push(profile)
      else if (profile.revision > merged[index].revision) merged[index] = profile
    }
    setProfiles(merged)
    setSelectedId(current => current && merged.some(agent => agent.id === current) ? current : merged[0]?.id ?? '')
  }, [])

  const refreshDrafts = useCallback(async (threadId?: string) => {
    const response = await agents.drafts(threadId)
    setDrafts(response.data.filter(draft => ['proposed', 'pending'].includes(draft.status)))
  }, [])

  const refreshSelected = useCallback(async (agentId: string, which: Tab = tab) => {
    const ticket = ++generation.current
    const jobs: Promise<unknown>[] = [agents.sessions(agentId)]
    if (which === 'tasks') jobs.push(agents.tasks(agentId, { limit: 25 }), agents.proposals(agentId), agents.workflows(agentId), agents.settings())
    if (which === 'memory') jobs.push(Promise.all([agents.memory(agentId, projectId || undefined), agents.memory(undefined, projectId || undefined)]))
    if (which === 'skills') jobs.push(Promise.all([agents.skills(agentId, projectId || undefined), agents.skills(undefined, projectId || undefined)]))
    const settled = await Promise.allSettled(jobs)
    if (ticket !== generation.current) return
    const sessionResult = settled[0]
    if (sessionResult.status === 'fulfilled') setSessions((sessionResult.value as { data: Thread[] }).data)
    const failure = settled.find(result => result.status === 'rejected') as PromiseRejectedResult | undefined
    if (failure) setError(errorText(failure.reason))
    if (which === 'tasks') {
      if (settled[1]?.status === 'fulfilled') {
        const page = settled[1].value as { data: Task[]; nextCursor?: string | null }
        setTasks(page.data)
        setTaskNextCursor(page.nextCursor ?? null)
      }
      if (settled[2]?.status === 'fulfilled') setProposals((settled[2].value as { data: DelegationProposal[] }).data.filter(proposal => ['proposed', 'pending'].includes(proposal.status)))
      if (settled[3]?.status === 'fulfilled') setWorkflows((settled[3].value as { data: Workflow[] }).data)
      if (settled[4]?.status === 'fulfilled') setSettings(settled[4].value as typeof settings)
    }
    if (which === 'memory' && settled[1]?.status === 'fulfilled') {
      const [privateItems, sharedItems] = settled[1].value as [{ data: MemoryRecord[]; sources?: MemorySource[] }, { data: MemoryRecord[]; sources?: MemorySource[] }]
      setMemories([...new Map([...privateItems.data, ...sharedItems.data.filter(item => item.scope !== 'agent' || item.scopeId === agentId)].map(item => [item.id, item])).values()])
      setMemorySources([...new Map([...(privateItems.sources ?? []), ...(sharedItems.sources ?? [])].map(source => [source.sourceId, source])).values()])
    }
    if (which === 'skills' && settled[1]?.status === 'fulfilled') {
      const [privateItems, sharedItems] = settled[1].value as [{ data: LearnedSkill[] }, { data: LearnedSkill[] }]
      setSkills([...new Map([...privateItems.data, ...sharedItems.data.filter(item => item.scope !== 'agent' || item.scopeId === agentId)].map(item => [item.id, item])).values()])
    }
  }, [projectId, tab])

  useEffect(() => {
    let active = true
    let disposed = false
    void Promise.all([refreshProfiles(), agents.settings(), agents.decisions(), agents.drafts(), agents.projects()]).then(([, nextSettings, decisions, pendingDrafts, projectList]) => {
      if (!active) return
      setSettings(nextSettings)
      setProposals(decisions.data.filter(proposal => ['proposed', 'pending'].includes(proposal.status)))
      setRuntimeRequests(decisions.runtimeRequests ?? [])
      setQuestions(decisions.questions ?? [])
      setDrafts(pendingDrafts.data.filter(draft => ['proposed', 'pending'].includes(draft.status)))
      setProjects(projectList.data)
    }).catch(reason => { if (active) setError(errorText(reason)) })
    void ticketApi.proposals().then(result => { if (active) setTicketProposals(result.data.filter(proposal => ['proposed', 'pending'].includes(proposal.status))) }).catch(reason => { if (active) setError(errorText(reason)) })
    let unlisten: (() => void) | undefined
    void client.subscribe(event => {
      if (event.method === 'brr/agents/changed' || event.method === 'brr/tasks/changed' || event.method === 'brr/questions/changed' || event.method === 'brr/tickets/changed') {
        void agents.decisions().then(decisions => { if (active) { setProposals(decisions.data.filter(proposal => ['proposed', 'pending'].includes(proposal.status))); setRuntimeRequests(decisions.runtimeRequests ?? []); setQuestions(decisions.questions ?? []) } }).catch(reason => { if (active) setError(errorText(reason)) })
        void refreshProfiles().catch(reason => setError(errorText(reason)))
        void refreshDrafts().catch(reason => setError(errorText(reason)))
        if (event.method === 'brr/tickets/changed') void ticketApi.proposals().then(result => { if (active) setTicketProposals(result.data.filter(proposal => ['proposed', 'pending'].includes(proposal.status))) }).catch(reason => setError(errorText(reason)))
        if (selectedId) void refreshSelected(selectedId).catch(reason => setError(errorText(reason)))
      }
    }).then(dispose => { if (disposed) dispose(); else unlisten = dispose }).catch(reason => { if (!disposed) setError(errorText(reason)) })
    return () => { active = false; disposed = true; unlisten?.() }
  }, [refreshDrafts, refreshProfiles, refreshSelected, selectedId])

  useEffect(() => {
    generation.current += 1
    if (selectedId) void refreshSelected(selectedId).catch(reason => setError(errorText(reason)))
    else { setSessions([]); setTasks([]); setMemories([]); setSkills([]) }
  }, [selectedId, refreshSelected])

  useEffect(() => {
    if (selectedId) void refreshSelected(selectedId, tab).catch(reason => setError(errorText(reason)))
  }, [tab, selectedId, refreshSelected])

  const run = async (label: string, action: () => Promise<void>) => {
    setBusy(label); setError(''); setNotice('')
    try { await action() } catch (reason) { setError(errorText(reason)) } finally { setBusy('') }
  }
  const decideTicketProposal = (proposal: TicketRunProposal, approve: boolean) => run(approve ? 'Starting reviewed ticket run…' : 'Rejecting ticket run…', async () => {
    const key = `${proposal.ticketId}:${proposal.hash}:${approve ? 'approve' : 'reject'}`
    const operationId = ticketProposalOperations.current.get(key) ?? ticketOperationId()
    ticketProposalOperations.current.set(key, operationId)
    if (approve) await ticketApi.approveRun(proposal.ticketId, proposal.hash, operationId)
    else await ticketApi.rejectRun(proposal.ticketId, proposal.hash, operationId)
    ticketProposalOperations.current.delete(key)
    const result = await ticketApi.proposals()
    setTicketProposals(result.data.filter(item => ['proposed', 'pending'].includes(item.status)))
  })

  const inspectWorkspace = async (path: string) => {
    const workspace = await agents.inspectWorkspace(path)
    const commands = workspace.commands.filter(command => !command.approved)
    if (!commands.length) return false
    setWorkspaceReview({ cwd: workspace.cwd, commands })
    setNotice('Review the project setup commands before trying again.')
    return true
  }
  const approveWorkspaceHooks = () => run('Approving project setup…', async () => {
    if (!workspaceReview) return
    const commandsByRoot = new Map<string, string[]>()
    for (const command of workspaceReview.commands) {
      const root = command.cwd ?? workspaceReview.cwd
      commandsByRoot.set(root, [...(commandsByRoot.get(root) ?? []), command.template])
    }
    for (const [root, templates] of commandsByRoot) await client.approveWorktree(root, templates)
    setWorkspaceReview(null)
    setNotice('Project setup approved. Retry the action when ready.')
  })

  const beginInterview = () => run('Starting interview…', async () => {
    if (!currentModel) throw new Error('Sign in with ChatGPT to choose a model for the interview.')
    const result = await agents.interview(currentModel.model, currentModel.defaultReasoningEffort)
    setTab('chat')
    await refreshDrafts(result.thread.id)
    onOpenThread(result.thread, undefined, { fresh: true })
  })
  const openSession = (thread: Thread) => onOpenThread(thread, selected?.id)
  const newSession = () => run('Opening session…', async () => {
    if (!selected) return
    if (activeProjectId !== undefined) {
      const inspection = await client.inspectProject(activeProjectId)
      const commands = inspection.commands.filter(command => !command.approved)
      if (commands.length) {setWorkspaceReview({cwd: activeProjectId ?? '', commands}); return}
    } else if (cwd && await inspectWorkspace(cwd)) return
    const result = await agents.open(selected.id, { model: selected.model, effort: selected.effort, ...(activeProjectId !== undefined ? {projectId: activeProjectId} : cwd ? { cwd } : {}) })
    await refreshSelected(selected.id, 'chat')
    onOpenThread(result.thread, selected.id, { fresh: true })
  })
  const editProfile = () => run('Starting profile interview…', async () => {
    if (!selected) return
    const result = await agents.edit(selected.id)
    await refreshDrafts(result.thread.id)
    onOpenThread(result.thread, selected.id, { fresh: true })
  })
  const approveDraft = (draft: ProfileDraft) => run('Creating agent…', async () => {
    const result = await agents.approve(draft.id, draft.hash)
    setProfiles(current => [...current.filter(profile => profile.id !== result.agent.id), result.agent])
    setSelectedId(result.agent.id)
    setDrafts(current => current.filter(item => item.id !== draft.id))
    setNotice(`${result.agent.name} is ready.`)
  })
  const rejectDraft = (draft: ProfileDraft) => run('Discarding profile proposal…', async () => {
    await agents.reject(draft.id, draft.hash)
    setDrafts(current => current.filter(item => item.id !== draft.id))
    setNotice('Profile proposal discarded.')
  })

  const openRuntimeRequest = (request: RuntimeRequest) => {
    const threadId = typeof request.params.threadId === 'string' ? request.params.threadId : ''
    if (!threadId) return
    const thread = { id: threadId, cwd: cwd ?? '', preview: `Pending approval · ${request.method}`, updatedAt: Date.now() }
    onOpenThread(thread, typeof request.params.agentId === 'string' ? request.params.agentId : undefined)
  }

  const openQuestion = (question: QuestionRequest) => {
    onOpenThread({ id: question.threadId, cwd: cwd ?? '', preview: question.questions[0]?.question ?? 'Agent question', updatedAt: question.updatedAt }, question.agentId ?? undefined, { ...(question.taskId ? { readOnly: true } : {}), questionId: question.id })
  }

  const questionOrigin = (question: QuestionRequest) => {
    const owner = question.agentId ? profiles.find(profile => profile.id === question.agentId) : undefined
    if (question.taskId) {
      const task = tasks.find(candidate => candidate.id === question.taskId)
      const taskName = task?.title || question.taskId
      return owner ? `${taskName} · ${owner.name}` : taskName
    }
    if (question.agentId) return owner?.name || question.agentId
    return question.threadId
  }

  const questionHeading = (question: QuestionRequest) => {
    const header = question.questions[0]?.header.trim() || 'Agent questions'
    return question.questions.length > 1 ? `${header} · ${question.questions.length} questions` : header
  }

  const createTask = (event: React.FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (!selected) return
    const formElement = event.currentTarget
    const form = new FormData(formElement)
    const title = String(form.get('taskTitle') ?? '').trim()
    const instructions = String(form.get('taskInstructions') ?? '').trim()
    const criteria = String(form.get('acceptance') ?? '').split('\n').map(value => value.trim()).filter(Boolean)
    if (!title || !instructions || !criteria.length) { setError('Add task instructions and at least one acceptance criterion.'); return }
    const dependencies = Array.from(form.getAll('dependency')).map(String)
    void run('Creating task…', async () => {
      if (taskProjectId) {
        const inspection = await client.inspectProject(taskProjectId)
        const commands = inspection.commands.filter(command => !command.approved)
        if (commands.length) {setWorkspaceReview({cwd: taskProjectId, commands}); return}
      } else if (taskCwd.trim() && await inspectWorkspace(taskCwd.trim())) return
      await agents.createTask({ agentId: selected.id, title, instructions, acceptanceCriteria: criteria, dependencies, ...(taskProjectId ? {projectId: taskProjectId} : taskCwd.trim() ? { cwd: taskCwd.trim() } : {}), ...(selected.model ? { model: selected.model } : {}), ...(selected.effort ? { effort: selected.effort } : {}) })
      await refreshSelected(selected.id, 'tasks')
      formElement.reset()
    })
  }

  const loadMoreTasks = () => {
    if (!selected || !taskNextCursor) return
    void run('Loading more tasks…', async () => {
      const page = await agents.tasks(selected.id, { after: taskNextCursor, limit: 25 })
      setTasks(current => [...current, ...page.data.filter(task => !current.some(existing => existing.id === task.id))])
      setTaskNextCursor(page.nextCursor ?? null)
    })
  }

  const createSchedule = (event: React.FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (!selected) return
    const formElement = event.currentTarget
    const form = new FormData(formElement)
    const name = String(form.get('scheduleName') ?? '').trim()
    const title = String(form.get('scheduleTitle') ?? '').trim()
    const instructions = String(form.get('scheduleInstructions') ?? '').trim()
    const expression = String(form.get('cron') ?? '').trim()
    const timezone = String(form.get('timezone') ?? '').trim()
    void run('Saving schedule…', async () => {
      if (activeProjectId) {
        const inspection = await client.inspectProject(activeProjectId)
        const commands = inspection.commands.filter(command => !command.approved)
        if (commands.length) {setWorkspaceReview({cwd: activeProjectId, commands}); return}
      } else if (cwd && await inspectWorkspace(cwd)) return
      await agents.createWorkflow({ name, agentId: selected.id, title, instructions, acceptanceCriteria: [String(form.get('scheduleAcceptance') ?? '').trim()], expression, timezone, ...(activeProjectId ? {projectId: activeProjectId} : cwd ? {cwd} : {}), model: selected.model, effort: selected.effort })
      await refreshSelected(selected.id, 'tasks')
      formElement.reset()
    })
  }

  const scheduleGraph = (event: React.FormEvent<HTMLFormElement>, proposal: DelegationProposal) => {
    event.preventDefault()
    if (!selected) return
    const formElement = event.currentTarget
    const form = new FormData(formElement)
    const name = String(form.get('graphScheduleName') ?? '').trim()
    const expression = String(form.get('graphCron') ?? '').trim()
    const timezone = String(form.get('graphTimezone') ?? '').trim()
    if (!name || !expression || !timezone) return
    void run('Scheduling task graph…', async () => {
      await agents.scheduleProposal(proposal.id, proposal.hash, { name, expression, timezone })
      await refreshSelected(selected.id, 'tasks')
      setNotice(`${name} schedule is ready.`)
      formElement.reset()
    })
  }

  const searchMemories = (event: React.FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (!selected) return
    void run('Searching memory…', async () => {
      const found = await agents.searchMemory(memoryQuery, selected.id, projectId || undefined)
      setMemories(found.data.filter(item => item.scope !== 'agent' || item.scopeId === selected.id))
      setMemorySources(found.sources ?? [])
    })
  }
  const forgetMemory = (memory: MemoryRecord) => run('Forgetting memory…', async () => {
    const result = await agents.forgetMemory(memory.id, memory.scope === 'agent' ? selected?.id : undefined, memory.scope === 'project' ? memory.scopeId : undefined)
    setMemoryUndo(result.eventId)
    setMemories(current => current.filter(item => item.id !== memory.id))
    setNotice('Memory forgotten. You can undo this change.')
  })

  return <section className="agents-panel" aria-label="Agents">
    <header className="agents-header"><div><p className="agents-kicker">Optional workspace</p><h2>Agents</h2></div><button className="agents-icon-button" onClick={onClose} aria-label="Close Agents"><X size={16}/></button></header>
    {error && <div className="agents-alert" role="alert">{error}<button onClick={() => setError('')} aria-label="Dismiss error">×</button></div>}
    {questionError && <div className="agents-alert" role="alert">Question delivery needs attention: {questionError}<button onClick={() => onClearQuestionError?.()} aria-label="Dismiss question delivery error">×</button></div>}
    {notice && <div className="agents-notice" role="status">{notice}</div>}
    {workspaceReview && <section className="agents-review-card agents-workspace-review" aria-label="Review project setup hooks"><h3>Review project setup</h3><p>These commands may run in {workspaceReview.cwd}. Approve them here, then retry the action.</p><ul>{workspaceReview.commands.map((command, index) => <li key={`${command.phase}-${index}`}><span>{command.phase}{command.name ? ` · ${command.name}` : ''}</span><pre>{command.template}</pre></li>)}</ul><Button size="sm" onClick={() => void approveWorkspaceHooks()} disabled={!!busy}>Approve setup hooks</Button></section>}
    {busy && <div className="agents-busy" role="status">{busy}</div>}
    {runtimeRequests.length > 0 && <section className="agents-runtime-requests" aria-label="Pending runtime decisions"><h3>Pending approvals</h3>{runtimeRequests.map(request => <article key={String(request.id)}><div><strong>{request.method.replaceAll('/', ' · ')}</strong><small>{String(request.params.threadId ?? 'Conversation approval')}</small></div><Button size="sm" variant="outline" onClick={() => openRuntimeRequest(request)} disabled={!request.params.threadId}>Open conversation</Button></article>)}</section>}
    {questions.length > 0 && <section className="agents-runtime-requests agents-pending-questions" aria-label="Pending questions"><h3>Questions</h3>{questions.map(question => <article key={question.id}><div><strong>{questionHeading(question)}</strong><small>{getQuestionStatus(question)} · {questionOrigin(question)}</small></div><Button size="sm" variant="outline" onClick={() => openQuestion(question)}>Open question</Button></article>)}</section>}
    {ticketProposals.length > 0 && <section className="agents-runtime-requests" aria-label="Pending ticket run proposals"><h3>Ticket run proposals</h3>{ticketProposals.map(proposal => { const reviewed = ticketProposalProjectSnapshot(proposal.snapshot.context); const privateWorkspace = reviewed?.folders.length === 0; return <article className="agents-ticket-proposal" key={proposal.id}><div><strong>{proposal.snapshot.title}</strong><small>{proposal.ticketId} · {profiles.find(profile => profile.id === proposal.snapshot.agentId)?.name ?? proposal.snapshot.agentId}</small><dl><dt>Project ID</dt><dd>{proposal.snapshot.projectId ?? 'Not specified'}</dd><dt>Run folder</dt><dd>{proposal.snapshot.cwd ?? (privateWorkspace ? 'No folders attached · private workspace' : 'Not specified')}</dd><dt>Reviewed project folders</dt><dd>{reviewed?.folders.length ? <ul className="agents-ticket-reviewed-roots" aria-label={`Reviewed project folders for ${proposal.ticketId}`}>{reviewed.folders.map(folder => <li key={folder}>{folder}</li>)}</ul> : privateWorkspace ? <span>No folders attached · private workspace</span> : <span>Not recorded in this proposal</span>}</dd><dt>Workspace</dt><dd>{proposal.snapshot.workspacePermission ?? 'Not specified'}</dd><dt>Model</dt><dd>{proposal.snapshot.model ?? 'Default'}</dd><dt>Reasoning</dt><dd>{proposal.snapshot.effort ?? 'Default'}</dd></dl><p>{proposal.snapshot.instructions}</p><span>Accept when: {proposal.snapshot.acceptanceCriteria.join('; ')}</span></div><Button size="sm" disabled={!online || !!busy} onClick={() => void decideTicketProposal(proposal, true)}>Approve</Button><Button size="sm" variant="outline" disabled={!!busy} onClick={() => void decideTicketProposal(proposal, false)}>Reject</Button></article>})}</section>}
    <div className="agents-layout">
      <aside id="agents-sidebar" className="agents-list" aria-label="Agent list">
        <div className="agents-search"><Search size={14}/><input aria-label="Search agents" placeholder="Find an agent" value={search} onChange={event => setSearch(event.target.value)}/></div>
        <Button size="sm" onClick={beginInterview} disabled={!desktop || !online || !!busy}><Plus size={14}/>New agent</Button>
        <div className="agents-list-items">{visibleProfiles.map(profile => <button key={profile.id} className="agents-list-item" aria-current={selectedId === profile.id ? 'true' : undefined} onClick={() => { setSelectedId(profile.id); setTab('chat') }}><span className="agents-avatar"><Bot size={15}/></span><span><strong>{profile.name}</strong><small>{profile.description || 'General purpose agent'}</small></span></button>)}{!visibleProfiles.length && <p className="agents-empty">{profiles.length ? 'No agents match your search.' : 'Create an agent through a short interview.'}</p>}</div>
        <p className="agents-capacity">Background tasks · {settings.maxBackground} total / {settings.maxPerAgent} per agent</p>
      </aside>
      <div className="agents-detail">
        {selected ? <>
          <div className="agents-profile-heading"><div><h3>{selected.name}</h3><p>{selected.description}</p></div><Button size="sm" variant="outline" onClick={editProfile} disabled={!online || !!busy}>Edit through chat</Button></div>
          <nav className="agents-tabs" aria-label="Agent sections">{(['chat', 'tasks', 'memory', 'skills', 'profile'] as Tab[]).map(section => <button key={section} aria-selected={tab === section} onClick={() => setTab(section)}>{section[0].toUpperCase() + section.slice(1)}</button>)}</nav>
          <div className="agents-content" key={`${selected.id}:${tab}`}>
            {tab === 'chat' && <section aria-label="Agent chats"><div className="agents-section-heading"><div><h4>Chat with {selected.name}</h4><p>Sessions open in Brr’s regular conversation view.</p></div><Button size="sm" onClick={newSession} disabled={!desktop || !online || !!busy}><Plus size={14}/>New session</Button></div><div className="agents-session-list">{sessions.map(session => <button key={session.id} onClick={() => openSession(session)}><MessageSquare size={15}/><span><strong>{session.name || 'Untitled session'}</strong><small>{session.preview || 'Continue this conversation'}</small></span></button>)}{!sessions.length && <p className="agents-empty">No sessions yet. Start a chat to begin.</p>}</div></section>}
            {tab === 'tasks' && <section aria-label="Agent tasks"><div className="agents-section-heading"><div><h4>Tasks</h4><p>Assign one-off work with clear acceptance criteria and dependencies.</p></div></div>
              <form className="agents-form" onSubmit={createTask}><label>Title<input name="taskTitle" required placeholder="Review the next release"/></label><label>Instructions<textarea name="taskInstructions" required rows={3} placeholder="Describe the work for this agent"/></label><label>Acceptance criteria<textarea name="acceptance" required rows={2} placeholder="One criterion per line"/></label><label>Project<select aria-label="Task project" value={taskProjectId} onChange={event => setTaskProjectId(event.target.value)}><option value="">No project</option>{projects.filter(project => !project.hidden).map(project => <option key={project.id} value={project.id}>{project.name}</option>)}</select></label>{!taskProjectId && <label>Working folder<input aria-label="Task working folder" value={taskCwd} onChange={event => setTaskCwd(event.target.value)} placeholder="Choose a folder or enter a path"/></label>}{!taskProjectId && chooseFolder && <Button size="sm" type="button" variant="outline" onClick={() => void chooseFolder().then(path => { if (path) setTaskCwd(path) })}>Choose task folder</Button>}{tasks.length > 0 && <fieldset><legend>Depends on</legend>{tasks.map(task => <label className="agents-check" key={task.id}><input type="checkbox" name="dependency" value={task.id}/>{task.title}</label>)}</fieldset>}<Button size="sm" type="submit" disabled={!online || !!busy}>Create task</Button></form>
              <div className="agents-record-list">{tasks.map(task => <article className="agents-record agents-task-record" key={task.id}><div><strong>{task.title}</strong><span className="agents-status">{task.status === 'waiting_input' ? 'Waiting for input' : task.status}</span>{task.reason && <p>{task.reason}</p>}{task.result && <div className="agents-task-result"><strong>Result</strong><Markdown>{task.result}</Markdown></div>}{task.evidence?.length ? <div className="agents-task-result"><strong>Evidence</strong><ul>{task.evidence.map(item => <li key={item}>{item}</li>)}</ul></div> : null}{task.status === 'blocked' && task.reason?.toLowerCase().includes('unknown') && <p className="agents-muted">The outcome is unknown. Review the conversation before retrying.</p>}</div><div className="agents-record-actions">{task.threadId && <Button size="sm" variant="outline" onClick={() => onOpenThread({ id: task.threadId!, cwd: task.cwd ?? cwd ?? '', preview: task.title, updatedAt: task.updatedAt }, task.agentId, { readOnly: true })}>Open conversation</Button>}{['running', 'queued', 'waiting_input'].includes(task.status) ? <Button size="sm" variant="outline" onClick={() => void run('Pausing task…', async () => { await agents.controlTask(task.id, 'pause'); await refreshSelected(selected.id, 'tasks') })}>Pause</Button> : task.status === 'paused' ? <Button size="sm" variant="outline" onClick={() => void run('Resuming task…', async () => { await agents.controlTask(task.id, 'resume'); await refreshSelected(selected.id, 'tasks') })}>Resume</Button> : null}{(['failed', 'cancelled'].includes(task.status) || task.status === 'blocked' && !task.reason?.toLowerCase().includes('unknown')) && <Button size="sm" variant="outline" onClick={() => void run('Retrying task…', async () => { await agents.controlTask(task.id, 'retry'); await refreshSelected(selected.id, 'tasks') })}>Retry</Button>}{['running', 'queued', 'paused', 'blocked', 'waiting_approval', 'waiting_input'].includes(task.status) && <Button size="sm" variant="ghost" onClick={() => void run('Canceling task…', async () => { await agents.controlTask(task.id, 'cancel'); await refreshSelected(selected.id, 'tasks') })}>Cancel</Button>}</div></article>)}{taskNextCursor && <Button size="sm" variant="outline" disabled={!!busy} onClick={loadMoreTasks}>Load more tasks</Button>}</div>
              {proposals.map(proposal => <article className="agents-review-card" key={proposal.id}>
                <h4>Review proposed task graph</h4>
                <p>{proposal.tasks.length} linked tasks are waiting for approval. Review each agent, task, and dependency before approval.</p>
                <ol>{proposal.tasks.map(node => <li key={node.key}><strong>{node.task.title}</strong><p>{node.task.agentId} · {node.task.cwd ?? cwd ?? 'No folder selected'}</p><p>{node.task.instructions}</p><p>Accept when: {node.task.acceptanceCriteria.join('; ')}</p>{node.dependsOn.length > 0 && <small>Depends on: {node.dependsOn.join(', ')}</small>}</li>)}</ol>
                <Button size="sm" onClick={() => void run('Approving task graph…', async () => { await agents.approveProposal(proposal.id, proposal.hash); await refreshSelected(selected.id, 'tasks') })}>Approve task graph</Button>
                <details className="agents-graph-schedule">
                  <summary>Repeat this graph</summary>
                  <p>Schedule this reviewed graph to run again. Creating the schedule will not launch tasks now.</p>
                  <form className="agents-form" onSubmit={event => scheduleGraph(event, proposal)}>
                    <label>Schedule name<input name="graphScheduleName" required placeholder="Weekly release review"/></label>
                    <div className="agents-form-row">
                      <label>Cron expression<input name="graphCron" required placeholder="0 9 * * 1"/></label>
                      <label>Timezone<input name="graphTimezone" required defaultValue={Intl.DateTimeFormat().resolvedOptions().timeZone}/></label>
                    </div>
                    <Button size="sm" type="submit" disabled={!online || !!busy}><Clock3 size={13}/>Schedule graph</Button>
                  </form>
                </details>
              </article>)}
              <section className="agents-subsection"><h4>Recurring schedule</h4><form className="agents-form" onSubmit={createSchedule}><div className="agents-form-row"><label>Schedule name<input name="scheduleName" required placeholder="Weekly release review"/></label><label>Task title<input name="scheduleTitle" required placeholder="Check release readiness"/></label></div><label>Instructions<textarea name="scheduleInstructions" required rows={2}/></label><label>Acceptance criterion<input name="scheduleAcceptance" required placeholder="Report blockers with evidence"/></label><div className="agents-form-row"><label>Cron expression<input name="cron" required placeholder="0 9 * * 1"/></label><label>Timezone<input name="timezone" required defaultValue={Intl.DateTimeFormat().resolvedOptions().timeZone}/></label></div><Button size="sm" type="submit" disabled={!online || !!busy}><Clock3 size={13}/>Save schedule</Button></form>{workflows.map(workflow => <article className="agents-record" key={workflow.id}><div><strong>{workflow.name}</strong><p>{workflow.expression} · {workflow.timezone}{workflow.nextRunAt ? ` · next ${new Date(workflow.nextRunAt * 1000).toLocaleString()}` : ''}</p></div><Button size="sm" variant="outline" onClick={() => void run('Updating schedule…', async () => { await agents.controlWorkflow(workflow.id, !workflow.paused); await refreshSelected(selected.id, 'tasks') })}>{workflow.paused ? 'Resume' : 'Pause'}</Button></article>)}</section>
              <section className="agents-subsection"><h4>Background limits</h4><form className="agents-settings" onSubmit={event => { event.preventDefault(); const data = new FormData(event.currentTarget); void run('Saving limits…', async () => { const next = { maxBackground: Number(data.get('maxBackground')), maxPerAgent: Number(data.get('maxPerAgent')) }; setSettings(await agents.updateSettings(next)) }) }}><label>Total workers<input aria-label="Total background workers" type="number" name="maxBackground" min="1" max="32" value={settings.maxBackground} onChange={event => setSettings(current => ({ ...current, maxBackground: Number(event.target.value) }))}/></label><label>Per agent<input aria-label="Workers per agent" type="number" name="maxPerAgent" min="1" max="32" value={settings.maxPerAgent} onChange={event => setSettings(current => ({ ...current, maxPerAgent: Number(event.target.value) }))}/></label><Button size="sm" type="submit" disabled={!online || !!busy}>Save limits</Button></form></section>
            </section>}
            {tab === 'memory' && <section aria-label="Agent memory"><div className="agents-section-heading"><div><h4>Memory</h4><p>Review private agent memories and shared user or project facts.</p></div><label className="agents-project-choice">Project<select aria-label="Memory project" value={projectId} onChange={event => setProjectId(event.target.value)}><option value="">All projects</option>{projects.map(project => <option key={project.id} value={project.id}>{project.name}{project.hidden ? " (deleted project)" : ""}</option>)}</select></label></div><form className="agents-memory-search" onSubmit={searchMemories}><input aria-label="Search memories" value={memoryQuery} onChange={event => setMemoryQuery(event.target.value)} placeholder="Search memory"/><Button size="sm" type="submit">Search</Button><Button size="sm" variant="outline" type="button" onClick={() => void refreshSelected(selected.id, 'memory')}>All memory</Button></form>{memoryUndo !== null && <Button size="sm" variant="outline" onClick={() => void run('Undoing memory change…', async () => { await agents.undoMemory(memoryUndo); setMemoryUndo(null); await refreshSelected(selected.id, 'memory') })}>Undo forget</Button>}<div className="agents-record-list">{memories.map(memory => <article className="agents-record agents-memory-record" key={memory.id}><div><span className="agents-status">{memory.scope} · {memory.kind}</span><Markdown>{memory.content}</Markdown>{memory.sourceIds.length > 0 && <div className="agents-memory-sources"><strong>Evidence</strong>{memorySources.filter(source => memory.sourceIds.includes(source.sourceId)).map(source => <details key={`${source.sourceId}:${source.revision}`}><summary>Source · revision {source.revision}</summary><Markdown>{source.content}</Markdown><Button size="sm" variant="outline" onClick={() => onOpenThread({ id: source.threadId, cwd: cwd ?? '', preview: 'Memory source conversation', updatedAt: Date.now() }, source.agentId)}>Open source conversation</Button></details>)}</div>}</div><div className="agents-record-actions"><Button size="sm" variant="outline" onClick={() => { const content = window.prompt('Correct this memory', memory.content); if (content === null || content === memory.content) return; void run('Saving correction…', async () => { await agents.reviseMemory({ ...(memory.scope === 'agent' ? { agentId: selected.id } : {}), ...(memory.scope === 'project' ? { projectId: memory.scopeId } : {}), id: memory.id, revision: memory.revision, content }); await refreshSelected(selected.id, 'memory') }) }}>Correct</Button><Button size="sm" variant="ghost" onClick={() => forgetMemory(memory)}>Forget</Button></div></article>)}{!memories.length && <p className="agents-empty">No saved memories found.</p>}</div></section>}
            {tab === 'skills' && <section aria-label="Agent skills"><div className="agents-section-heading"><div><h4>Learned skills</h4><p>Reusable procedures saved from completed work.</p></div><label className="agents-project-choice">Project<select aria-label="Skill project" value={projectId} onChange={event => setProjectId(event.target.value)}><option value="">All projects</option>{projects.map(project => <option key={project.id} value={project.id}>{project.name}{project.hidden ? " (deleted project)" : ""}</option>)}</select></label></div><div className="agents-record-list">{skills.map(skill => <details className="agents-skill" key={skill.id}><summary><strong>{skill.name}</strong><span>{skill.description} · {skill.scope}</span></summary><Markdown>{skill.content}</Markdown><p>Sources: {skill.sourceIds.join(', ') || 'No source recorded'}</p><Button size="sm" variant="outline" onClick={() => void run('Revoking skill…', async () => { await agents.revokeSkill(skill.id, skill.scope === 'agent' ? selected.id : undefined, skill.scope === 'project' ? skill.scopeId : undefined); await refreshSelected(selected.id, 'skills') })}>Revoke skill</Button></details>)}{!skills.length && <p className="agents-empty">No learned skills yet.</p>}</div></section>}
            {tab === 'profile' && <section aria-label="Agent profile"><div className="agents-section-heading"><div><h4>Profile</h4><p>Identity, instructions, and default model used by this agent.</p></div><Button size="sm" variant="outline" onClick={editProfile} disabled={!online || !!busy}>Edit through chat</Button></div><dl className="agents-profile-fields"><dt>Name</dt><dd>{selected.name}</dd><dt>Description</dt><dd>{selected.description}</dd><dt>Model</dt><dd>{selected.model} · {selected.effort}</dd><dt>Instructions</dt><dd><Markdown>{selected.instructions}</Markdown></dd><dt>soul.md</dt><dd><Markdown>{selected.soul}</Markdown></dd></dl><p className="agents-muted">Revision {selected.revision}</p></section>}
          </div>
        </> : <div className="agents-onboarding"><span className="agents-avatar agents-avatar-large"><Bot size={22}/></span><h3>Your agents work in their own conversations</h3><p>Create one through an interview. Review the proposed instructions and soul.md before it becomes available.</p><Button onClick={beginInterview} disabled={!desktop || !online || !!busy}><Plus size={14}/>New agent</Button>{drafts.filter(draft => ['proposed', 'pending'].includes(draft.status)).map(draft => <DraftReview key={draft.id} draft={draft} busy={!!busy} onApprove={() => approveDraft(draft)} onReject={() => rejectDraft(draft)}/>)}</div>}
      </div>
    </div>
    {selected && drafts.filter(draft => ['proposed', 'pending'].includes(draft.status)).map(draft => <DraftReview key={draft.id} draft={draft} base={draft.agentId ? profiles.find(profile => profile.id === draft.agentId) : undefined} busy={!!busy} onApprove={() => approveDraft(draft)} onReject={() => rejectDraft(draft)}/>)}
  </section>
}

function DraftReview({ draft, base, busy, onApprove, onReject }: { draft: ProfileDraft; base?: AgentProfile; busy: boolean; onApprove: () => void; onReject?: () => void }) {
  const candidate = draft.candidate
  return <section className="agents-review-card" aria-label="Review agent profile"><p className="agents-kicker">Review exact profile candidate</p><h3>{candidate.name}</h3><p>{candidate.description}</p><div className="agents-candidate-file"><strong>instructions.md</strong><Markdown>{candidate.instructions}</Markdown></div><div className="agents-candidate-file"><strong>soul.md</strong><Markdown>{candidate.soul}</Markdown></div>{base && <details className="agents-profile-diff"><summary>Compare with revision {base.revision}</summary><div><strong>instructions.md · current</strong><Markdown>{base.instructions}</Markdown><strong>instructions.md · proposed</strong><Markdown>{candidate.instructions}</Markdown><strong>soul.md · current</strong><Markdown>{base.soul}</Markdown><strong>soul.md · proposed</strong><Markdown>{candidate.soul}</Markdown></div></details>}<p className="agents-muted">Model: {candidate.model} · {candidate.effort}</p><div className="agents-review-actions"><Button size="sm" onClick={onApprove} disabled={busy}>{draft.agentId ? 'Apply profile changes' : 'Create agent'}</Button>{onReject && <Button size="sm" variant="outline" onClick={onReject} disabled={busy}>Discard proposal</Button>}</div></section>
}
