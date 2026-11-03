import { useCallback, useEffect, useRef, useState } from 'react'
import { ChevronDown, ChevronRight, File, RefreshCw } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { workspaceRequest } from '@/lib/workspace-client'
import './ToolTabs.css'

type Entry = {name: string; path: string; kind: 'file' | 'directory' | 'symlink'; size: number}
type FileData = {path: string; content: string; version: string}
type Listing = {entries: Entry[]}

export function FilesTab({root, visible, initialPath, line, onFilePath, onDirtyChange}: {root: string; visible: boolean; initialPath?: string; line?: number; onFilePath?: (path: string) => void; onDirtyChange?: (dirty: boolean) => void}) {
  const [listings, setListings] = useState<Record<string, Entry[]>>({})
  const [expanded, setExpanded] = useState<Set<string>>(new Set(['']))
  const [selected, setSelected] = useState<FileData | null>(null)
  const [draft, setDraft] = useState('')
  const [pendingPath, setPendingPath] = useState<string | null>(null)
  const [pendingLine, setPendingLine] = useState<number | undefined>()
  const [confirmReload, setConfirmReload] = useState(false)
  const [error, setError] = useState('')
  const [loading, setLoading] = useState(false)
  const rootState = useRef({root, generation: 0})
  if (rootState.current.root !== root) rootState.current = {root, generation: rootState.current.generation + 1}
  const operation = useRef(0)
  const editorRef = useRef<HTMLTextAreaElement>(null)
  const lineTarget = useRef<{path: string; line: number} | null>(null)
  const selectedRef = useRef(selected)
  const draftRef = useRef(draft)
  const pendingPathRef = useRef(pendingPath)
  selectedRef.current = selected
  draftRef.current = draft
  pendingPathRef.current = pendingPath
  const dirty = selected !== null && draft !== selected.content
  const dirtyRef = useRef(dirty)
  dirtyRef.current = dirty
  useEffect(() => onDirtyChange?.(dirty), [dirty, onDirtyChange])
  useEffect(() => {
    setListings({}); setExpanded(new Set([''])); setSelected(null); setDraft(''); setPendingPath(null); setPendingLine(undefined); setError('')
  }, [root])

  const loadDirectory = useCallback(async (path: string) => {
    const generation = rootState.current.generation
    try {
      const result = await workspaceRequest<Listing>('directory.list', {root, path})
      if (rootState.current.generation !== generation || rootState.current.root !== root) return
      setListings(current => ({...current, [path]: result.entries.sort((a, b) => Number(b.kind === 'directory') - Number(a.kind === 'directory') || a.name.localeCompare(b.name))}))
      setError('')
    } catch (reason) { if (rootState.current.generation === generation && rootState.current.root === root) setError(message(reason)) }
  }, [root])
  useEffect(() => { if (visible) void loadDirectory('') }, [loadDirectory, visible])

  const selectLine = (requestedLine: number, content: string) => {
    const textarea = editorRef.current
    if (!textarea) return
    const lines = content.split('\n')
    const index = Math.max(0, Math.min(lines.length - 1, Math.floor(requestedLine) - 1))
    const start = lines.slice(0, index).reduce((offset, value) => offset + value.length + 1, 0)
    textarea.focus()
    textarea.setSelectionRange(start, start + (lines[index]?.length ?? 0))
  }
  const openFile = async (path: string, requestedLine?: number) => {
    if (selectedRef.current?.path === path) {
      if (requestedLine !== undefined) selectLine(requestedLine, draftRef.current)
      return
    }
    if (dirtyRef.current && selectedRef.current?.path !== path) { setPendingPath(path); setPendingLine(requestedLine); return }
    await readFile(path, requestedLine)
  }
  const readFile = async (path: string, requestedLine?: number) => {
    lineTarget.current = requestedLine === undefined ? null : {path, line: requestedLine}
    const generation = rootState.current.generation
    const request = ++operation.current
    setLoading(true); setError(''); setConfirmReload(false)
    try {
      const file = await workspaceRequest<FileData>('file.read', {root, path})
      if (request !== operation.current || rootState.current.generation !== generation || rootState.current.root !== root) return
      setSelected(file); setDraft(file.content); setPendingPath(null); setPendingLine(undefined); onFilePath?.(file.path)
    } catch (reason) { if (request === operation.current && rootState.current.generation === generation && rootState.current.root === root) setError(message(reason)) }
    finally { if (request === operation.current && rootState.current.generation === generation) setLoading(false) }
  }
  const save = async (): Promise<boolean> => {
    if (!selected || !dirty) return true
    const target = selected
    const content = draft
    const generation = rootState.current.generation
    const request = ++operation.current
    setLoading(true); setError('')
    try {
      const saved = await workspaceRequest<FileData>('file.write', {root, path: target.path, content, expectedVersion: target.version})
      if (rootState.current.generation !== generation || rootState.current.root !== root || request !== operation.current || selectedRef.current?.path !== target.path) return true
      setSelected(saved)
      setDraft(current => current === content ? saved.content : current)
      return true
    } catch (reason) { if (request === operation.current && rootState.current.generation === generation && selectedRef.current?.path === target.path) setError(message(reason)) }
    finally { if (request === operation.current && rootState.current.generation === generation) setLoading(false) }
    return false
  }
  useEffect(() => {
    if (!visible || !initialPath) return
    if (selectedRef.current?.path === initialPath) {
      if (line !== undefined) selectLine(line, draftRef.current)
      return
    }
    void openFile(initialPath, line)
  }, [initialPath, line, visible])
  useEffect(() => {
    const target = lineTarget.current
    if (!target || selected?.path !== target.path) return
    selectLine(target.line, draft)
    lineTarget.current = null
  }, [selected, draft])

  const discardAndOpen = () => {
    if (pendingPath !== null) void readFile(pendingPath, pendingLine)
  }
  useEffect(() => {
    if (!visible) return
    const onKey = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 's') {
        event.preventDefault(); void save()
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [visible, selected, draft])

  const renderTree = (path: string, depth = 0) => (listings[path] ?? []).map(entry => <div key={entry.path}>
    <button className={`workspace-file-row ${selected?.path === entry.path ? 'is-selected' : ''}`} style={{paddingInlineStart: 10 + depth * 14}} type="button" aria-label={entry.name} onClick={() => {
      if (entry.kind === 'directory') {
        setExpanded(current => { const next = new Set(current); next.has(entry.path) ? next.delete(entry.path) : next.add(entry.path); return next })
        if (!listings[entry.path]) void loadDirectory(entry.path)
      } else void openFile(entry.path)
    }}>
      <span aria-hidden="true" className="workspace-file-icon">{entry.kind === 'directory' ? (expanded.has(entry.path) ? <ChevronDown size={14} /> : <ChevronRight size={14} />) : entry.kind === 'symlink' ? '↗' : <File size={14} />}</span>{entry.name}
    </button>
    {entry.kind === 'directory' && expanded.has(entry.path) && renderTree(entry.path, depth + 1)}
  </div>)

  return <section className="workspace-tool files-tool" hidden={!visible} aria-label="Files">
    <div className="workspace-file-tree" aria-label="Project files">
      <div className="workspace-tool-heading"><span>Files</span><Button variant="ghost" size="icon" aria-label="Refresh files" onClick={() => { setListings({}); setExpanded(new Set([''])); void loadDirectory('') }}><RefreshCw size={15} /></Button></div>
      {renderTree('')}
    </div>
    <div className="workspace-file-editor">
      {selected ? <>
      <header className="workspace-editor-header"><span className="workspace-path" title={selected.path}>{selected.path}</span>{dirty && <span className="workspace-dirty" aria-label="Unsaved changes" title="Unsaved changes">●</span>}<Button size="sm" onClick={() => void save()} disabled={!dirty || loading}>Save</Button></header>
        {error && <div className="workspace-error" role="alert"><span>{error}</span>{/changed on disk/i.test(error) && <Button variant="outline" size="sm" onClick={() => setConfirmReload(true)}>Reload from disk</Button>}</div>}
        {confirmReload && <div className="workspace-confirm" role="group" aria-label="Confirm reload"><span>Discard your draft and reload the disk version?</span><Button size="sm" onClick={() => void readFile(selected.path)}>Discard draft and reload</Button><Button variant="ghost" size="sm" onClick={() => setConfirmReload(false)}>Keep draft</Button></div>}
        <textarea ref={editorRef} className="workspace-editor" aria-label="File contents" spellCheck={false} value={draft} disabled={loading} onChange={event => setDraft(event.target.value)} />
      </> : <div className="workspace-empty">{loading ? 'Loading file…' : 'Choose a file to view and edit.'}</div>}
      {!selected && error && <div className="workspace-error" role="alert">{error}</div>}
      {pendingPath !== null && <div className="workspace-confirm workspace-switch-confirm" role="group" aria-label="Unsaved file changes"><span>Save changes before opening {pendingPath}?</span><Button size="sm" onClick={async () => { const target = pendingPath, targetLine = pendingLine; if (await save() && pendingPathRef.current === target) await readFile(target, targetLine) }}>Save</Button><Button variant="outline" size="sm" onClick={discardAndOpen}>Discard</Button><Button variant="ghost" size="sm" onClick={() => setPendingPath(null)}>Cancel</Button></div>}
    </div>
  </section>
}

function message(reason: unknown) { return reason instanceof Error ? reason.message : String(reason) }
