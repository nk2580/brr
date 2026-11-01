import { useEffect, useMemo, useRef, useState } from 'react'
import { ArrowUp, Code2, FolderOpen, LoaderCircle, PanelRight, Search, Square, SquarePen } from 'lucide-react'
import { AgentMessage } from './components/AgentMessage'
import { Button } from './components/ui/button'
import { client, isDesktop, type Connection, type Model, type Thread } from './lib/client'
import { hydrateThread, initialSession, reduceSession, type Approval, type RpcEvent, type SessionState } from './lib/session'

const upsertThread = (threads: Thread[], next: Thread) => {
  const existing = threads.find(thread => thread.id === next.id)
  const merged = { ...existing, ...next }
  if (existing?.name && !next.name) merged.name = existing.name
  if (existing?.preview && !next.preview) merged.preview = existing.preview
  return [merged, ...threads.filter(thread => thread.id !== next.id)]
}
const streamingItemDeltaMethods = new Set([
  'item/agentMessage/delta', 'item/reasoning/summaryTextDelta',
  'item/commandExecution/outputDelta', 'item/plan/delta',
])
const terminalItemStatuses = new Set(['completed', 'failed', 'interrupted', 'declined'])
const mergeStreamedText = (snapshot: string, live: string) => {
  if (snapshot.includes(live)) return snapshot
  if (live.startsWith(snapshot)) return live
  for (let size = Math.min(snapshot.length, live.length); size > 0; size -= 1) {
    if (snapshot.endsWith(live.slice(0, size))) return snapshot + live.slice(size)
  }
  return live
}
const mergeHydratedItems = (hydrated: SessionState['items'], live: SessionState['items']) => {
  const liveById = new Map(live.map(item => [item.id, item]))
  const merged = hydrated.map(item => {
    const current = liveById.get(item.id)
    if (!current) return item
    liveById.delete(item.id)
    const result = { ...item, ...current }
    if (typeof item.status === 'string' && terminalItemStatuses.has(item.status)) result.status = item.status
    for (const field of ['text', 'aggregatedOutput'] as const) {
      const fromSnapshot = item[field]
      const fromLive = current[field]
      if (typeof fromSnapshot === 'string' && typeof fromLive === 'string') {
        result[field] = mergeStreamedText(fromSnapshot, fromLive)
      }
    }
    return result
  })
  return [...merged, ...live.filter(item => liveById.has(item.id))]
}

export default function App() {
  const desktop = isDesktop()
  const [account, setAccount] = useState<Connection['account']>(null)
  const [models, setModels] = useState<Model[]>([])
  const [threads, setThreads] = useState<Thread[]>([])
  const [sessions, setSessions] = useState<Record<string, SessionState>>({})
  const [resuming, setResuming] = useState<Record<string, boolean>>({})
  const hydratedThreads = useRef(new Set<string>())
  const resumingThreads = useRef(new Set<string>())
  const resumeEvents = useRef<Record<string, RpcEvent[]>>({})
  const [threadId, setThreadId] = useState<string | null>(null)
  const [folder, setFolder] = useState('')
  const [modelId, setModelId] = useState('')
  const [effort, setEffort] = useState('medium')
  const [readOnly, setReadOnly] = useState(false)
  const [activity, setActivity] = useState(true)
  const [searching, setSearching] = useState(false)
  const [query, setQuery] = useState('')
  const [text, setText] = useState('')
  const [modeChoices, setModeChoices] = useState<Record<string, 'default' | 'plan'>>({})
  const [pending, setPending] = useState(false)
  const [steeringPending, setSteeringPending] = useState(false)
  const [loginPending, setLoginPending] = useState(false)
  const [loginId, setLoginId] = useState<string | null>(null)
  const [cancelingLogin, setCancelingLogin] = useState(false)
  const loginPendingRef = useRef(false)
  const currentLoginIdRef = useRef<string | null>(null)
  const loginAttemptRef = useRef(0)
  const cancelledLoginIdsRef = useRef(new Set<string>())
  const conversationScroll = useRef<HTMLDivElement>(null)
  const followTranscript = useRef(true)
  const [engineOnline, setEngineOnline] = useState(true)
  const [error, setError] = useState('')
  const [answerDrafts, setAnswerDrafts] = useState<Record<string, Record<string, string[]>>>({})
  const viewVersion = useRef(0)
  const draftVersion = useRef(0)

  const activeModel = models.find(model => model.id === modelId) ?? models[0]
  const session = threadId ? sessions[threadId] ?? initialSession(threadId) : initialSession()
  const worktree = threads.find(thread => thread.id === threadId)?.worktree
  const modeKey = threadId ?? 'new'
  const mode = modeChoices[modeKey] ?? session.mode
  const activeTurnRef = useRef({ threadId, turnId: session.turnId })
  activeTurnRef.current = { threadId, turnId: session.turnId }
  const visibleThreads = useMemo(() => threads.filter(thread => `${thread.name ?? ''} ${thread.preview}`.toLowerCase().includes(query.toLowerCase())), [threads, query])
  useEffect(() => {
    const element = conversationScroll.current
    if (element && followTranscript.current) element.scrollTop = element.scrollHeight
  }, [session.items])
  const applyConnection = (connection: Connection) => {
    setAccount(connection.account)
    setModels(connection.models)
    setThreads(connection.threads)
    const preferred = connection.models.find(model => model.isDefault) ?? connection.models[0]
    if (preferred) { setModelId(preferred.id); setEffort(preferred.defaultReasoningEffort) }
    setEngineOnline(true)
    setError('')
  }

  useEffect(() => {
    let active = true
    let unsubscribe: (() => void) | undefined
    if (!desktop) return () => { active = false; unsubscribe?.() }
    const onEvent = (event: RpcEvent) => {
      const params = event.params ?? {}
      const collaborationMode = params.threadSettings?.collaborationMode ?? params.collaborationMode ?? params.thread?.collaborationMode
      if (event.method === 'thread/settings/updated' && typeof params.threadId === 'string'
        && (collaborationMode?.mode === 'default' || collaborationMode?.mode === 'plan')) {
        setModeChoices(previous => ({ ...previous, [params.threadId]: collaborationMode.mode }))
      }
      if (event.method === 'brr/disconnected') {
        setEngineOnline(false)
        if (currentLoginIdRef.current) cancelledLoginIdsRef.current.add(currentLoginIdRef.current)
        loginAttemptRef.current += 1
        loginPendingRef.current = false
        currentLoginIdRef.current = null
        setLoginPending(false)
        setLoginId(null)
        setCancelingLogin(false)
        setError('Coding engine disconnected. Reconnect to continue.')
      }
      if (event.method === 'account/login/completed') {
        const completedLoginId = typeof params.loginId === 'string' ? params.loginId : null
        if (completedLoginId && cancelledLoginIdsRef.current.has(completedLoginId)) return
        if (!loginPendingRef.current) return
        if (completedLoginId && currentLoginIdRef.current && completedLoginId !== currentLoginIdRef.current) return
        loginAttemptRef.current += 1
        loginPendingRef.current = false
        currentLoginIdRef.current = null
        setLoginPending(false)
        setLoginId(null)
        setCancelingLogin(false)
        if (!params.success) setError(typeof params.error === 'string' ? params.error : 'ChatGPT sign-in failed.')
        else void client.connect().then(connection => {
          if (!active) return
          applyConnection(connection)
          if (!connection.account) setError('Sign-in completed, but no ChatGPT account is available.')
        }).catch(reason => { if (active) setError(String(reason)) })
      }
      if (event.method === 'account/updated' && params.authMode === null) {
        void client.account().then(value => {
          if (active) setAccount(value)
        }).catch(reason => { if (active) setError(String(reason)) })
      }
      if (typeof params.threadId === 'string') {
        if (resumingThreads.current.has(params.threadId)) {
          resumeEvents.current[params.threadId] ??= []
          resumeEvents.current[params.threadId].push(event)
        }
        setSessions(previous => {
          const id = params.threadId as string
          const before = previous[id] ?? initialSession(id)
          const after = reduceSession(before, event)
          return after === before ? previous : { ...previous, [id]: after }
        })
      }
    }
    void client.subscribe(onEvent).then(stop => { if (active) unsubscribe = stop; else stop() }).catch(reason => { if (active) setError(String(reason)) })
    void client.connect().then(connection => {
      if (!active) return
      applyConnection(connection)
    }).catch(reason => { if (active) { setEngineOnline(false); setError(String(reason)) } })
    return () => { active = false; unsubscribe?.() }
  }, [desktop])

  const reconnect = async () => {
    const selectedThreadId = threadId
    if (currentLoginIdRef.current) cancelledLoginIdsRef.current.add(currentLoginIdRef.current)
    loginAttemptRef.current += 1
    loginPendingRef.current = false
    currentLoginIdRef.current = null
    setLoginPending(false)
    setLoginId(null)
    setCancelingLogin(false)
    setError('')
    setSessions({})
    hydratedThreads.current.clear()
    resumingThreads.current.clear()
    resumeEvents.current = {}
    try {
      const connection = await client.connect()
      applyConnection(connection)
      if (selectedThreadId) {
        const thread = connection.threads.find(candidate => candidate.id === selectedThreadId)
          ?? { id: selectedThreadId, cwd: folder, preview: '', updatedAt: Date.now() }
        await openThread(thread)
      }
    }
    catch (reason) { setEngineOnline(false); setError(String(reason)) }
  }

  const chooseFolder = async () => {
    try { const path = await client.chooseWorkspace(); if (path) { setFolder(path); if (threadId) newChat() } }
    catch (reason) { setError(String(reason)) }
  }

  const newChat = () => {
    viewVersion.current += 1
    followTranscript.current = true
    if (conversationScroll.current) conversationScroll.current.scrollTop = 0
    setThreadId(null); setText(''); setError('')
  }

  const setMode = (next: 'default' | 'plan') => {
    const id = threadId
    if (pending || steeringPending || session.status === 'running' || Boolean(id && resuming[id])) return
    setModeChoices(previous => ({ ...previous, [id ?? 'new']: next }))
  }

  const openThread = async (thread: Thread) => {
    viewVersion.current += 1
    followTranscript.current = true
    setThreadId(thread.id)
    setFolder(thread.worktree?.projectPath ?? thread.cwd)
    if (hydratedThreads.current.has(thread.id) || resumingThreads.current.has(thread.id)) return
    resumingThreads.current.add(thread.id)
    setResuming(previous => ({ ...previous, [thread.id]: true }))
    resumeEvents.current[thread.id] = []
    try {
      const result = await client.resume(thread.id)
      let hydrated = hydrateThread(result.thread)
      const bufferedEvents = resumeEvents.current[thread.id] ?? []
      const snapshotItemIds = new Set(hydrated.items.map(item => item.id))
      for (const event of bufferedEvents) {
        const params = event.params ?? {}
        if (streamingItemDeltaMethods.has(event.method) && snapshotItemIds.has(params.itemId)) continue
        const eventTurnId = params.turnId ?? params.turn?.id
        const turnAlreadyComplete = hydrated.turnId && eventTurnId === hydrated.turnId && hydrated.status !== 'running'
        if (turnAlreadyComplete && ['turn/started', 'item/started', 'item/completed'].includes(event.method)) continue
        hydrated = reduceSession(hydrated, event)
      }
      setSessions(previous => {
        const live = previous[thread.id]
        if (!live) return { ...previous, [thread.id]: hydrated }
        return { ...previous, [thread.id]: { ...hydrated, items: mergeHydratedItems(hydrated.items, live.items) } }
      })
      setModeChoices(previous => ({ ...previous, [thread.id]: previous[thread.id] ?? hydrated.mode }))
      setThreads(previous => upsertThread(previous, result.thread))
      hydratedThreads.current.add(thread.id)
    } catch (reason) { setError(String(reason)) }
    finally {
      resumingThreads.current.delete(thread.id)
      setResuming(previous => ({ ...previous, [thread.id]: false }))
      delete resumeEvents.current[thread.id]
    }
  }

  const steerMessage = async () => {
    const message = text.trim()
    const id = threadId
    const expectedTurnId = session.turnId
    if (!message || !id || !expectedTurnId || !account || !activeModel || steeringPending || pending || session.status !== 'running' || resuming[id]) return
    const requestedView = viewVersion.current
    const requestedDraft = draftVersion.current
    setSteeringPending(true)
    setError('')
    try {
      const result = await client.steer(id, message, expectedTurnId)
      if (result.turnId !== expectedTurnId) throw new Error('The runtime returned a different turn for this instruction.')
      const stillTargetingRequest = viewVersion.current === requestedView
        && activeTurnRef.current.threadId === id
        && activeTurnRef.current.turnId === expectedTurnId
      if (stillTargetingRequest && draftVersion.current === requestedDraft) setText('')
    } catch (reason) {
      if (viewVersion.current === requestedView
        && activeTurnRef.current.threadId === id
        && activeTurnRef.current.turnId === expectedTurnId
        && draftVersion.current === requestedDraft) setError(String(reason))
    } finally { setSteeringPending(false) }
  }

  const submitMessage = () => {
    if (steeringPending) return
    const trimmed = text.trim()
    if (/^\/plan(?:\s|$)/i.test(trimmed)) {
      if (pending || Boolean(threadId && resuming[threadId])) return
      if (session.status === 'running') {
        setError('Wait for the current turn to finish before changing modes.')
        return
      }
      const instruction = trimmed.replace(/^\/plan\s*/i, '').trim()
      if (!instruction) {
        setMode(mode === 'plan' ? 'default' : 'plan')
        setText('')
        return
      }
      setModeChoices(previous => ({ ...previous, [modeKey]: 'plan' }))
      void sendMessage(instruction, 'plan')
      return
    }
    if (pending || Boolean(threadId && resuming[threadId])) return
    if (session.status === 'running') void steerMessage()
    else void sendMessage()
  }

  const sendMessage = async (submittedText = text.trim(), requestedMode = mode) => {
    const message = submittedText
    if (!message || !account || !activeModel || pending || session.status === 'running' || Boolean(threadId && resuming[threadId])) return
    const requestedView = viewVersion.current
    const requestedDraft = draftVersion.current
    setPending(true); setError('')
    try {
      let id = threadId
      if (!id) {
        if (!folder) return
        const result = await client.start(folder, activeModel.model, readOnly)
        id = result.thread.id
        if (viewVersion.current === requestedView) setThreadId(id)
        setThreads(previous => upsertThread(previous, result.thread))
        setSessions(previous => ({ ...previous, [id!]: initialSession(id!) }))
        setModeChoices(previous => ({ ...previous, [id!]: previous.new ?? requestedMode }))
      }
      const result = await client.send(id, message, activeModel.model, effort, requestedMode)
      setThreads(previous => previous.map(thread => thread.id === id && !thread.name && !thread.preview
        ? { ...thread, preview: message }
        : thread))
      setSessions(previous => {
        const current = previous[id!] ?? initialSession(id!)
        if (current.turnId === result.turn.id) return previous
        const next = reduceSession(current, { method: 'turn/started', params: { threadId: id, turn: { id: result.turn.id, status: 'inProgress', items: [] } } })
        return { ...previous, [id!]: next }
      })
      setModeChoices(previous => ({ ...previous, [id!]: requestedMode }))
      if (viewVersion.current === requestedView && draftVersion.current === requestedDraft) setText('')
    } catch (reason) {
      if (viewVersion.current === requestedView) setError(String(reason))
    }
    finally { setPending(false) }
  }

  const stopTurn = async () => {
    if (!threadId || !session.turnId) return
    try { await client.interrupt(threadId, session.turnId) }
    catch (reason) { setError(String(reason)) }
  }

  const archiveThread = async () => {
    if (!threadId) return
    try {
      await client.archive(threadId)
      setThreads(previous => previous.filter(thread => thread.id !== threadId))
      setThreadId(null)
    } catch (reason) { setError(String(reason)) }
  }

  const respond = async (approval: Approval, result: Record<string, unknown>) => {
    try {
      await client.respond(approval.id, result)
      if (threadId) setSessions(previous => {
        const current = previous[threadId] ?? initialSession(threadId)
        const next = reduceSession(current, { method: 'serverRequest/resolved', params: { threadId, requestId: approval.id } })
        return { ...previous, [threadId]: next }
      })
    } catch (reason) { setError(String(reason)) }
  }

  const proposedPlan = session.proposedPlan
  const canImplementPlan = Boolean(
    proposedPlan?.text.trim()
      && proposedPlan.isComplete
      && proposedPlan.turnId === session.turnId
      && session.status === 'idle'
      && mode === 'plan'
      && !text.trim()
      && account
      && activeModel
      && engineOnline
      && !pending
      && !steeringPending
      && !(threadId && resuming[threadId]),
  )

  const login = async () => {
    const attempt = ++loginAttemptRef.current
    loginPendingRef.current = true
    currentLoginIdRef.current = null
    setLoginPending(true); setError('')
    setLoginId(null)
    try {
      const result = await client.login()
      if (loginAttemptRef.current !== attempt) return
      currentLoginIdRef.current = result.loginId
      setLoginId(result.loginId)
    }
    catch (reason) {
      if (loginAttemptRef.current === attempt) {
        loginPendingRef.current = false
        currentLoginIdRef.current = null
        setLoginPending(false); setLoginId(null); setCancelingLogin(false); setError(String(reason))
      }
    }
  }

  const cancelSignIn = async () => {
    if (!loginId || cancelingLogin) return
    setCancelingLogin(true)
    try {
      const result = await client.cancelLogin(loginId) as {status?: string}
      if (currentLoginIdRef.current === loginId && result?.status === 'canceled') {
        cancelledLoginIdsRef.current.add(loginId)
        loginAttemptRef.current += 1
        loginPendingRef.current = false
        currentLoginIdRef.current = null
        setLoginPending(false)
        setLoginId(null)
      }
    } catch (reason) {
      setError(String(reason))
    } finally { setCancelingLogin(false) }
  }

  return <div className="app-shell">
    <aside className="sidebar">
      <div className="wordmark" data-tauri-drag-region>brr<span className="wordmark-dot">.</span></div>
      <nav className="sidebar-actions" aria-label="Workspace">
        <Button className="new-chat" aria-label="New conversation" onClick={newChat}><SquarePen size={17}/>New chat</Button>
        <Button variant="ghost" onClick={() => setSearching(value => !value)}><Search size={17}/>Search chats</Button>
        <Button variant="ghost" disabled={!desktop} onClick={() => void chooseFolder()}><FolderOpen size={17}/>Open project</Button>
      </nav>
      {searching && <input className="thread-search" aria-label="Search chats" placeholder="Search chats" value={query} onChange={event => setQuery(event.target.value)} />}
      <div className="thread-list" aria-label="Chats">
        {visibleThreads.map(thread => <button key={thread.id} className={`thread-row ${threadId === thread.id ? 'selected' : ''}`} onClick={() => void openThread(thread)} title={thread.preview}>{thread.name || thread.preview || 'New chat'}</button>)}
      </div>
      <div className="account-panel">
        {desktop && !engineOnline && <div className="engine-offline" role="status"><span>Coding engine disconnected</span><Button variant="ghost" onClick={() => void reconnect()}>Reconnect</Button></div>}
        {account?.email && <span className="account-email">{account.email}</span>}
        {account ? <Button variant="ghost" onClick={() => void client.logout().then(() => setAccount(null)).catch(reason => setError(String(reason)))}>Sign out</Button> : <><Button disabled={!desktop || !engineOnline || loginPending} onClick={() => void login()}>Sign in with ChatGPT</Button>{loginPending && <Button variant="ghost" disabled={!loginId || cancelingLogin} onClick={() => void cancelSignIn()}>{cancelingLogin ? 'Cancelling…' : 'Cancel sign-in'}</Button>}</>}
      </div>
    </aside>
    <main className="main-panel">
      <header className="titlebar" data-tauri-drag-region>
        <div className="model-controls">
          <label><span className="sr-only">Model</span><select aria-label="Model" disabled={!models.length} value={activeModel?.id ?? ''} onChange={event => { const next = models.find(model => model.id === event.target.value); if (next) { setModelId(next.id); setEffort(next.defaultReasoningEffort) } }}>
            {models.length === 0 && <option value="">Brr</option>}
            {models.map(model => <option key={model.id} value={model.id}>{model.displayName}</option>)}
          </select></label>
          {activeModel && <label>Reasoning <select aria-label="Reasoning effort" value={effort} onChange={event => setEffort(event.target.value)}>
            {(activeModel?.supportedReasoningEfforts.map(option => option.reasoningEffort) ?? ['low', 'medium', 'high']).map(value => <option key={value} value={value}>{value}</option>)}
          </select></label>}
        </div>
        <div className="titlebar-actions">
          <Button variant="ghost" aria-label={`Activity ${activity ? 'on' : 'off'}`} aria-pressed={activity} onClick={() => setActivity(value => !value)}><PanelRight size={17}/>Activity</Button>
          {threadId && <Button variant="ghost" onClick={() => void archiveThread()}>Archive</Button>}
        </div>
      </header>
      <div className="workspace-body">
        <section className="conversation-panel">
          <div ref={conversationScroll} className="conversation-scroll" onScroll={event => {
            const element = event.currentTarget
            followTranscript.current = element.scrollHeight - element.scrollTop - element.clientHeight <= 120
          }}>
            {!threadId && session.items.length === 0 ? <div className="welcome"><div className="welcome-symbol"><Code2 size={30} strokeWidth={1.5}/></div><h1>What are we building?</h1>{!account && <p>Sign in with ChatGPT to start a coding session.</p>}{folder && <p className="project-path">{folder}</p>}</div> : <div className="messages">{session.items.map(item => <AgentMessage key={item.id} item={item} />)}</div>}
            {session.error && <p className="turn-error">{session.error}</p>}
            {proposedPlan && proposedPlan.isComplete && proposedPlan.turnId === session.turnId && session.status === 'idle' && mode === 'plan' && <section className="proposed-plan-review" aria-label="Proposed plan review">
              <p>Review the proposed plan above before implementing it.</p>
              <Button disabled={!canImplementPlan} onClick={() => void sendMessage('Implement the plan.', 'default')}>Implement plan</Button>
            </section>}
            {error && <p className="app-error" role="alert">{error}</p>}
          </div>
          {((activity && (session.plan.length > 0 || session.diff)) || session.approvals.length > 0) && <aside className="activity-panel" aria-label="Turn activity">
            {activity && session.plan.length > 0 && <ol className="turn-plan">{session.plan.map((step, index) => <li key={`${step.step}-${index}`} data-status={step.status}>{step.step} <span>{step.status}</span></li>)}</ol>}
            {activity && session.diff && <details className="turn-diff"><summary>Changes</summary><pre>{session.diff}</pre></details>}
            {session.approvals.map(approval => <ApprovalCard key={approval.id} approval={approval} answers={answerDrafts[String(approval.id)] ?? {}} onAnswer={(question, answer) => setAnswerDrafts(previous => ({ ...previous, [String(approval.id)]: { ...previous[String(approval.id)], [question]: answer } }))} onRespond={result => void respond(approval, result)} />)}
          </aside>}
          <div className="composer-area">
            <textarea aria-label="Message" placeholder="Ask anything" value={text} onChange={event => { draftVersion.current += 1; setText(event.target.value) }} onKeyDown={event => {
              if (event.key === 'Tab' && event.shiftKey) {
                event.preventDefault()
                setMode(mode === 'plan' ? 'default' : 'plan')
              } else if (event.key === 'Enter' && !event.shiftKey && !event.nativeEvent.isComposing) {
                event.preventDefault(); submitMessage()
              }
            }} />
            <div className="composer-toolbar">
              <div className="composer-project"><Button variant="ghost" disabled={!desktop || !engineOnline} onClick={() => void chooseFolder()}>Choose project folder</Button>{folder && <span title={folder}>{folder.split('/').filter(Boolean).at(-1) ?? folder}</span>}{threadId ? <span className="thread-access">Access: saved thread policy</span> : <label className="read-only-toggle"><input type="checkbox" checked={readOnly} onChange={event => setReadOnly(event.target.checked)} /> Read only</label>}</div>
              <Button className="mode-toggle" variant="ghost" aria-label="Plan mode" aria-pressed={mode === 'plan'} title="Plan mode (Shift+Tab)" disabled={pending || steeringPending || session.status === 'running' || Boolean(threadId && resuming[threadId])} onClick={() => setMode(mode === 'plan' ? 'default' : 'plan')}>Plan<kbd>⇧Tab</kbd></Button>
              <Button className="send-control" aria-label="Send message" disabled={!desktop || !engineOnline || !account || !activeModel || (session.status !== 'running' && !threadId && !folder) || !text.trim() || pending || steeringPending || Boolean(threadId && resuming[threadId]) || (session.status === 'running' && (!threadId || !session.turnId))} onClick={submitMessage}>{pending || steeringPending ? <LoaderCircle className="spin" size={17}/> : <ArrowUp size={19}/>}</Button>
              {session.status === 'running' && <Button className="send-control" aria-label="Stop" variant="secondary" disabled={!engineOnline} onClick={() => void stopTurn()}><Square size={14} fill="currentColor"/></Button>}
            </div>
            {worktree ? <p className="worktree-location" aria-label="Session worktree" title={worktree.path}>Worktree · {worktree.branch}</p> : !threadId && folder && <p className="worktree-location">New chats use a separate worktree.</p>}
            {loginPending && <p className="login-pending">Complete sign-in in your browser.</p>}
            {!desktop && <p className="desktop-only-note">Connect from the desktop app to use the coding engine.</p>}
          </div>
        </section>
      </div>
    </main>
  </div>
}

function ApprovalCard({ approval, answers, onAnswer, onRespond }: {
  approval: Approval
  answers: Record<string, string[]>
  onAnswer: (question: string, answer: string[]) => void
  onRespond: (result: Record<string, unknown>) => void
}) {
  const questions = Array.isArray(approval.params.questions) ? approval.params.questions as { id: string; header?: string; question: string; isOther?: boolean; isSecret?: boolean; options?: { label: string; description: string }[] | null }[] : []
  if (approval.method === 'item/tool/requestUserInput') return <section className="approval-card"><strong>Input required</strong>{questions.map(question => {
    const options = question.options ?? []
    const selected = answers[question.id] ?? []
    const otherValue = selected.filter(answer => !options.some(option => option.label === answer)).join(', ')
    const setOther = (value: string) => onAnswer(question.id, value ? [value] : [])
    return <fieldset key={question.id}><legend>{question.header || question.question}</legend><p>{question.question}</p>{options.map(option => <button className="question-option" type="button" key={option.label} aria-pressed={selected.includes(option.label)} onClick={() => onAnswer(question.id, selected.includes(option.label) ? [] : [option.label])}><span>{option.label}</span><small>{option.description}</small></button>)}<label>Other<input aria-label={`Other: ${question.question}`} type={question.isSecret ? 'password' : 'text'} value={otherValue} onChange={event => setOther(event.target.value)} /></label></fieldset>
  })}<Button disabled={questions.length === 0 || questions.some(question => !(answers[question.id] ?? []).some(answer => answer.trim()))} onClick={() => onRespond({ answers: Object.fromEntries(questions.map(question => [question.id, { answers: (answers[question.id] ?? []).filter(answer => answer.trim()) }])) })}>Submit answers</Button></section>
  if (approval.method === 'item/permissions/requestApproval') {
    const requested = approval.params.permissions && typeof approval.params.permissions === 'object' ? approval.params.permissions as Record<string, unknown> : {}
    return <section className="approval-card"><strong>Permissions requested</strong>{typeof approval.params.reason === 'string' && <p>{approval.params.reason}</p>}<pre>{JSON.stringify(requested, null, 2)}</pre><div><Button onClick={() => onRespond({ permissions: requested, scope: 'turn' })}>Allow once</Button><Button variant="ghost" onClick={() => onRespond({ permissions: {}, scope: 'turn' })}>Decline</Button></div></section>
  }
  return <section className="approval-card">
    <strong>{approval.method.includes('commandExecution') ? 'Command approval requested' : approval.method.includes('fileChange') ? 'File change approval requested' : 'Approval requested'}</strong>
    {typeof approval.params.reason === 'string' && <p>{approval.params.reason}</p>}
    {typeof approval.params.command === 'string' && <pre>{approval.params.command}</pre>}
    {typeof approval.params.cwd === 'string' && <p className="project-path">{approval.params.cwd}</p>}
    {typeof approval.params.grantRoot === 'string' && <p>File access: {approval.params.grantRoot}</p>}
    <div><Button onClick={() => onRespond({ decision: 'accept' })}>Allow</Button><Button variant="ghost" onClick={() => onRespond({ decision: 'decline' })}>Decline</Button></div>
  </section>
}
