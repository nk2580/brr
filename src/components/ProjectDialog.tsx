import { useEffect, useRef, useState } from 'react'
import { X } from 'lucide-react'
import { Button } from './ui/button'
import type { Model, ProjectInput, ProjectRecord } from '../lib/client'
import './ProjectDialog.css'

export function ProjectDialog({project, models, onSave, onChooseFolders, onClose}: {
  project?: ProjectRecord
  models: Model[]
  onSave: (input: ProjectInput | ProjectRecord) => Promise<void>
  onChooseFolders: () => Promise<string[] | null>
  onClose: () => void
}) {
  const [name, setName] = useState(project?.name ?? '')
  const [folders, setFolders] = useState(project?.folders ?? [])
  const [primaryFolder, setPrimaryFolder] = useState(project?.primaryFolder ?? null)
  const [model, setModel] = useState(project?.model ?? '')
  const [effort, setEffort] = useState(project?.effort ?? '')
  const [permission, setPermission] = useState(project?.permission ?? '')
  const [pending, setPending] = useState(false)
  const [choosing, setChoosing] = useState(false)
  const [error, setError] = useState('')
  const dialog = useRef<HTMLElement>(null)
  const nameInput = useRef<HTMLInputElement>(null)
  const busy = useRef(false)
  const opener = useRef<HTMLElement | null>(null)
  const title = project ? 'Edit project' : 'Create project'
  const selectedModel = models.find(value => value.model === model)
  const reasoningModel = selectedModel ?? models.find(value => value.isDefault) ?? models[0]
  const efforts = reasoningModel?.supportedReasoningEfforts ?? []
  const unavailable = Boolean(model && !selectedModel)

  useEffect(() => {
    opener.current = document.activeElement instanceof HTMLElement ? document.activeElement : null
    nameInput.current?.focus()
    return () => opener.current?.focus()
  }, [])

  const onDialogKeyDown = (event: React.KeyboardEvent<HTMLElement>) => {
    if (event.key === 'Escape' && !pending && !choosing) { event.preventDefault(); event.stopPropagation(); onClose(); return }
    if (event.key !== 'Tab') return
    const focusable = dialog.current?.querySelectorAll<HTMLElement>('button:not(:disabled), input:not(:disabled), select:not(:disabled), summary, [tabindex="0"]')
    if (!focusable?.length) return
    const first = focusable[0], last = focusable[focusable.length - 1]
    if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus() }
    else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus() }
  }

  const addFolders = async () => {
    if (busy.current || choosing) return
    setChoosing(true); setError('')
    try {
      const selected = await onChooseFolders()
      if (selected?.length) {
        const duplicates = selected.filter((folder, index) => folders.includes(folder) || selected.indexOf(folder) !== index)
        if (duplicates.length) setError(`Folder already added: ${[...new Set(duplicates)].join(', ')}`)
        const additions = selected.filter((folder, index) => !folders.includes(folder) && selected.indexOf(folder) === index)
        if (additions.length) {
          const next = [...folders, ...additions]
          setFolders(next)
          if (!primaryFolder) setPrimaryFolder(next[0] ?? null)
        }
      }
    } catch (reason) { setError(reason instanceof Error ? reason.message : String(reason)) }
    finally { setChoosing(false) }
  }

  const removeFolder = (folder: string) => {
    const next = folders.filter(value => value !== folder)
    setFolders(next)
    if (primaryFolder === folder) setPrimaryFolder(next[0] ?? null)
  }

  const submit = (event: React.FormEvent) => {
    event.preventDefault()
    if (busy.current || !name.trim() || unavailable) return
    busy.current = true; setPending(true); setError('')
    const defaults = {model: model || null, effort: effort || null, permission: (permission || null) as ProjectRecord['permission']}
    const input: ProjectInput | ProjectRecord = project
      ? {...project, name: name.trim(), folders, primaryFolder, ...defaults}
      : {name: name.trim(), folders, primaryFolder, ...defaults}
    void Promise.resolve().then(() => onSave(input)).catch(reason => setError(reason instanceof Error ? reason.message : String(reason))).finally(() => { busy.current = false; setPending(false) })
  }

  return <div className="project-dialog-backdrop" onMouseDown={event => { if (event.target === event.currentTarget && !pending && !choosing) onClose() }}>
    <section className="project-dialog" ref={dialog} role="dialog" aria-modal="true" aria-labelledby="project-dialog-title" onKeyDown={onDialogKeyDown}>
      <header className="project-dialog-header"><h2 id="project-dialog-title">{title}</h2><Button variant="ghost" size="icon" aria-label="Close project dialog" disabled={pending || choosing} onClick={onClose}><X size={17} aria-hidden="true" /></Button></header>
      <form className="project-dialog-form" onSubmit={submit}>
        <label>Project name<input ref={nameInput} value={name} maxLength={120} required disabled={pending || choosing} onChange={event => setName(event.target.value)} /></label>
        <section className="project-dialog-folders" aria-label="Project folders">
          <div className="project-dialog-section-heading"><span>Folders</span><Button type="button" variant="secondary" disabled={pending || choosing} onClick={() => void addFolders()}>{choosing ? 'Choosing…' : 'Add folders'}</Button></div>
          {folders.length === 0 ? <p className="project-dialog-empty">Folders are optional. You can add them later.</p> : <ul>{folders.map(folder => <li key={folder}>
            {folders.length > 1 && <label className="project-dialog-primary"><input type="radio" name="primaryFolder" aria-label={`Primary folder ${folder}`} checked={primaryFolder === folder} disabled={pending || choosing} onChange={() => setPrimaryFolder(folder)} />Primary</label>}
            <span className="project-dialog-path">{folder}</span><Button type="button" variant="ghost" size="sm" aria-label={`Remove ${folder}`} disabled={pending || choosing} onClick={() => removeFolder(folder)}>Remove</Button>
          </li>)}</ul>}
        </section>
        {project && <details className="project-dialog-defaults"><summary>New chat defaults</summary><fieldset disabled={pending || choosing}><p>These defaults apply to new chats in this project. Existing chats keep their settings.</p>
          <label>Default model<select value={model} onChange={event => { const next = event.target.value; setModel(next); const nextModel = models.find(value => value.model === next) ?? models.find(value => value.isDefault) ?? models[0]; if (!nextModel?.supportedReasoningEfforts.some(value => value.reasoningEffort === effort)) setEffort('') }}>
            <option value="">Use app default</option>{unavailable && <option value={model}>Unavailable: {model}</option>}{models.map(value => <option key={value.id} value={value.model}>{value.displayName}</option>)}
          </select></label>
          {unavailable && <p role="alert">Choose an available model before saving. The saved model “{model}” is unavailable.</p>}
          <label>Default reasoning<select value={effort} onChange={event => setEffort(event.target.value)}><option value="">Use model default</option>{effort && !efforts.some(value => value.reasoningEffort === effort) && <option value={effort}>{effort}</option>}{efforts.map(value => <option key={value.reasoningEffort} value={value.reasoningEffort}>{value.reasoningEffort}</option>)}</select></label>
          <label>Default permissions<select value={permission} onChange={event => setPermission(event.target.value as typeof permission)}><option value="">Use app default</option><option value="read-only">Read only</option><option value="workspace-write">Can edit workspace</option></select></label>
        </fieldset></details>}
        {error && <p className="project-dialog-error" role="alert">{error}</p>}
        <div className="project-dialog-actions"><Button type="submit" disabled={pending || choosing || !name.trim() || unavailable}>{pending ? (project ? 'Saving…' : 'Creating…') : project ? 'Save changes' : 'Create project'}</Button><Button type="button" variant="ghost" disabled={pending || choosing} onClick={onClose}>Cancel</Button></div>
      </form>
    </section>
  </div>
}
