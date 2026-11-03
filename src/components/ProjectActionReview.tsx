import { useEffect, useId, useRef, useState } from 'react'
import type { ProjectArchiveResult, ProjectRecord } from '../lib/client'
import './ProjectActionReview.css'

export type ProjectActionReviewProps = {
  project: ProjectRecord
  action: 'archive' | 'delete'
  chatCount: number
  pending: boolean
  result: ProjectArchiveResult | null
  error: string
  onConfirm: () => void
  onClose: () => void
}

function archivedSummary(count: number) {
  return `${count} ${count === 1 ? 'chat' : 'chats'} archived.`
}

export function ProjectActionReview({
  project,
  action,
  chatCount,
  pending,
  result,
  error,
  onConfirm,
  onClose,
}: ProjectActionReviewProps) {
  const headingId = `project-action-heading-${useId()}`
  const reviewRef = useRef<HTMLDivElement>(null)
  const submittedRef = useRef(false)
  const wasPendingRef = useRef(pending)
  const [submitted, setSubmitted] = useState(false)
  const remaining = result !== null && (result.skipped.length > 0 || result.failed.length > 0)

  useEffect(() => {
    reviewRef.current?.focus()
  }, [])

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape' || pending) return
      event.preventDefault()
      onClose()
    }
    document.addEventListener('keydown', onKeyDown)
    return () => document.removeEventListener('keydown', onKeyDown)
  }, [pending, onClose])

  useEffect(() => {
    if (pending) {
      wasPendingRef.current = true
      submittedRef.current = true
      return
    }
    if (wasPendingRef.current || result || error) {
      wasPendingRef.current = false
      submittedRef.current = false
      setSubmitted(false)
    }
  }, [pending, result, error])

  const confirm = () => {
    if (pending || submittedRef.current) return
    submittedRef.current = true
    setSubmitted(true)
    onConfirm()
  }

  const deleting = action === 'delete'
  const pendingLabel = deleting ? 'Deleting project…' : 'Archiving…'
  const title = deleting ? `Delete project ${project.name}?` : `Archive chats in ${project.name}`
  const description = deleting
    ? 'Delete this project from Brr. Its chats will become ungrouped. Project files and worktrees stay on disk.'
    : 'Archive all chats in this project.'
  const activityNote = deleting
    ? 'Deleting a project does not archive chats or delete project files.'
    : 'Active chats and background tasks stay as they are and are skipped until they are resolved.'

  return <div
    ref={reviewRef}
    className="project-action-review"
    role="region"
    aria-labelledby={headingId}
    aria-busy={pending}
    tabIndex={-1}
  >
    <div className="project-action-content">
      <p className="project-action-kicker">Project action</p>
      <h2 id={headingId}>{title}</h2>
      {!result && <>
        <p className="project-action-description">{description}</p>
        <p className="project-action-count">Brr currently lists {chatCount} {chatCount === 1 ? 'chat' : 'chats'}. The count is checked again when {deleting ? 'deletion' : 'archiving'} starts.</p>
        <p className="project-action-activity">{activityNote}</p>
      </>}

      {pending && <p className="project-action-pending" role="status">{pendingLabel}</p>}
      {error && <p className="project-action-error" role="alert">{error}</p>}

      {result && <div className="project-action-result" aria-live="polite">
        <p className="project-action-count-result">{archivedSummary(result.archivedIds.length)}</p>
        {result.skipped.length > 0 && <section className="project-action-outcome">
          <h3>Skipped chats</h3>
          <ul aria-label="Skipped chats">
            {result.skipped.map(item => <li key={item.threadId} data-thread-id={item.threadId}>
              Conversation {item.threadId}: {item.reason}
            </li>)}
          </ul>
        </section>}
        {result.failed.length > 0 && <section className="project-action-outcome">
          <h3>Chats that could not be archived</h3>
          <ul aria-label="Chats that could not be archived">
            {result.failed.map(item => <li key={item.threadId} data-thread-id={item.threadId}>
              Conversation {item.threadId}: {item.error}
            </li>)}
          </ul>
        </section>}
      </div>}

      <div className="project-action-buttons">
        {result && !remaining
          ? <button type="button" className="project-action-secondary" disabled={pending} onClick={onClose}>Done</button>
          : <>
            <button
              type="button"
              className={deleting ? 'project-action-danger' : 'project-action-primary'}
              disabled={pending || submitted}
              onClick={confirm}
            >{pending ? pendingLabel : remaining ? 'Retry remaining chats' : deleting ? 'Delete project' : 'Archive chats'}</button>
            <button type="button" className="project-action-secondary" disabled={pending} onClick={onClose}>
              {result ? 'Done' : 'Cancel'}
            </button>
          </>}
      </div>
    </div>
  </div>
}
