import { useCallback, useEffect, useRef, useState } from 'react'
import { RefreshCw } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { workspaceRequest } from '@/lib/workspace-client'
import './ToolTabs.css'

type ChangedFile = {path: string; oldPath?: string; indexStatus: string; worktreeStatus: string; untracked: boolean}
type Status = {branch: string; files: ChangedFile[]}
type Diff = {patch: string; truncated: boolean}

export function GitDiffTab({root, visible}: {root: string; visible: boolean}) {
  const [status, setStatus] = useState<Status | null>(null)
  const [path, setPath] = useState<string | null>(null)
  const [staged, setStaged] = useState(false)
  const [diff, setDiff] = useState<Diff | null>(null)
  const [error, setError] = useState('')
  const [loading, setLoading] = useState(false)
  const rootState = useRef({root, generation: 0})
  if (rootState.current.root !== root) rootState.current = {root, generation: rootState.current.generation + 1}
  const pathRef = useRef(path)
  const stagedRef = useRef(staged)
  const statusRequest = useRef(0)
  const diffRequest = useRef(0)
  const activity = useRef(0)
  pathRef.current = path
  stagedRef.current = staged
  useEffect(() => { statusRequest.current++; diffRequest.current++; activity.current++; pathRef.current = null; stagedRef.current = false; setStatus(null); setPath(null); setStaged(false); setDiff(null); setError(''); setLoading(false) }, [root])
  const refresh = useCallback(async () => {
    if (!visible) return
    const generation = rootState.current.generation
    const request = ++statusRequest.current
    const active = ++activity.current
    setLoading(true); setError('')
    try {
      const result = await workspaceRequest<Status>('git.status', {root})
      if (rootState.current.generation !== generation || rootState.current.root !== root || request !== statusRequest.current) return
      setStatus(result)
      const selectedPath = pathRef.current
      if (selectedPath && !result.files.some(file => file.path === selectedPath)) { pathRef.current = null; diffRequest.current++; setPath(null); setDiff(null) }
    } catch (reason) { if (rootState.current.generation === generation && request === statusRequest.current) { setError(message(reason)); setStatus(null) } }
    finally { if (rootState.current.generation === generation && active === activity.current) setLoading(false) }
  }, [root, visible])
  const loadDiff = async (nextPath: string, nextStaged = staged) => {
    pathRef.current = nextPath; stagedRef.current = nextStaged
    const generation = rootState.current.generation
    const request = ++diffRequest.current
    const active = ++activity.current
    setPath(nextPath); setStaged(nextStaged); setLoading(true); setError('')
    try {
      const result = await workspaceRequest<Diff>('git.diff', {root, path: nextPath, staged: nextStaged})
      if (rootState.current.generation !== generation || rootState.current.root !== root || request !== diffRequest.current || pathRef.current !== nextPath || stagedRef.current !== nextStaged) return
      setDiff(result)
    }
    catch (reason) { if (rootState.current.generation === generation && request === diffRequest.current && pathRef.current === nextPath && stagedRef.current === nextStaged) { setDiff(null); setError(message(reason)) } }
    finally { if (rootState.current.generation === generation && active === activity.current) setLoading(false) }
  }
  useEffect(() => { void refresh() }, [refresh])
  useEffect(() => {
    if (visible && path) void loadDiff(path, staged)
    // stage selector intentionally reloads the currently selected file
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [staged, visible])

  const currentFile = status?.files.find(file => file.path === path)
  return <section className="workspace-tool git-tool" hidden={!visible} aria-label="Git diff">
    <aside className="workspace-git-files">
      <div className="workspace-tool-heading"><span>{status ? status.branch : 'Git changes'}</span><Button variant="ghost" size="icon" aria-label="Refresh git status" onClick={() => void refresh()}><RefreshCw size={15} /></Button></div>
      {status?.files.length ? status.files.map(file => <button type="button" key={file.path} className={`workspace-file-row ${path === file.path ? 'is-selected' : ''}`} aria-label={`${file.path}${file.untracked ? ', untracked' : ''}`} onClick={() => void loadDiff(file.path)}>
        <span className="workspace-git-status">{file.untracked ? '?' : file.worktreeStatus !== ' ' && file.worktreeStatus ? file.worktreeStatus : file.indexStatus || 'M'}</span><span className="workspace-path">{file.path}</span>
      </button>) : <div className="workspace-empty">{loading ? 'Checking repository…' : status ? 'Working tree clean.' : 'Git status unavailable.'}</div>}
    </aside>
    <div className="workspace-diff-pane">
      <div className="workspace-diff-toolbar"><span className="workspace-path">{currentFile?.oldPath ? `${currentFile.oldPath} → ${currentFile.path}` : path ?? 'Select a changed file'}</span>{path && <label className="workspace-stage-toggle"><input type="checkbox" checked={staged} onChange={event => setStaged(event.target.checked)} />Staged</label>}</div>
      {error && <div className="workspace-error" role="alert">{error}</div>}
      {loading && <div className="workspace-empty">Loading…</div>}
      {!loading && diff && <>
        {diff.truncated && <div className="workspace-warning" role="status">Diff is truncated.</div>}
        {diff.patch && /^(?:Binary files .* differ|GIT binary patch)/m.test(diff.patch) ? <div className="workspace-empty" role="status">Binary file changes cannot be shown as text.</div> : diff.patch ? <pre className="workspace-diff" aria-label="Unified diff">{diff.patch.split('\n').map((line, index) => <span key={index} className={line.startsWith('+') && !line.startsWith('+++') ? 'diff-add' : line.startsWith('-') && !line.startsWith('---') ? 'diff-remove' : line.startsWith('@@') ? 'diff-hunk' : ''}>{line}{'\n'}</span>)}</pre> : <div className="workspace-empty">{currentFile?.untracked ? 'No diff is available for this file.' : 'No changes in this version.'}</div>}
      </>}
      {!loading && path && !diff && !error && <div className="workspace-empty">Diff unavailable.</div>}
    </div>
  </section>
}

function message(reason: unknown) { return reason instanceof Error ? reason.message : String(reason) }
