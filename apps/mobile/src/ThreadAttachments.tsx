import { useEffect, useRef, useState } from 'react'
import { FileText, Image as ImageIcon } from 'lucide-react'
import { RemoteClient } from '@brr/remote-client'

type Attachment = {
  id: string
  name: string
  mime: string
  kind: string
  sizeBytes: number
  sha256: string
  submissionId: string
}
type Preview = { preview: string | null; reason?: string }
type Props = {
  attachments: unknown
  client: RemoteClient
  profileId: string
  ownerNodeId: string
  threadId: string
  online: boolean
}

const object = (value: unknown): value is Record<string, unknown> => Boolean(value && typeof value === 'object' && !Array.isArray(value))
const validAttachment = (value: unknown): value is Attachment => object(value)
  && typeof value.id === 'string' && typeof value.name === 'string' && typeof value.mime === 'string'
  && typeof value.kind === 'string' && typeof value.submissionId === 'string'
  && Number.isSafeInteger(value.sizeBytes) && Number(value.sizeBytes) >= 0
  && typeof value.sha256 === 'string' && /^[a-f0-9]{64}$/.test(value.sha256)
const attachmentKey = (file: Attachment) => `${file.submissionId}:${file.id}`

function checkedPreview(value: unknown, attachment: Attachment): Preview {
  if (!object(value) || !object(value.attachment) || value.attachment.sha256 !== attachment.sha256
    || value.attachment.sizeBytes !== attachment.sizeBytes) throw new Error('The attachment response did not match this conversation.')
  if (value.preview === null) return { preview: null, reason: typeof value.reason === 'string' ? value.reason : undefined }
  if (typeof value.preview !== 'string') throw new Error('The attachment preview was incomplete.')
  if (attachment.kind === 'image') {
    if (value.preview.length > 2 * 1024 * 1024 * 4 / 3 + 200
      || !/^data:image\/(?:png|jpeg|gif|webp|avif|bmp|tiff|heic|heif);base64,[A-Za-z0-9+/]*={0,2}$/.test(value.preview)) {
      throw new Error('This image preview could not be displayed safely.')
    }
  } else if (new TextEncoder().encode(value.preview).length > 2 * 1024 * 1024) {
    throw new Error('This attachment is too large to preview.')
  }
  return { preview: value.preview }
}

export function ThreadAttachments({ attachments, client, profileId, ownerNodeId, threadId, online }: Props) {
  const files = Array.isArray(attachments) ? attachments.filter(validAttachment) : []
  const [previews, setPreviews] = useState<Record<string, Preview>>({})
  const [open, setOpen] = useState<Record<string, boolean>>({})
  const [busy, setBusy] = useState('')
  const [error, setError] = useState<{ id: string; message: string } | null>(null)
  const generation = useRef(0)
  useEffect(() => { const current = ++generation.current; return () => { if (generation.current === current) generation.current++ } }, [profileId, ownerNodeId, threadId])
  if (!files.length) return null

  const preview = async (file: Attachment, retry = false) => {
    const fileId = attachmentKey(file)
    if (!retry && open[fileId]) { setOpen(previous => ({ ...previous, [fileId]: false })); return }
    setOpen(previous => ({ ...previous, [fileId]: true }))
    setError(null)
    if (previews[file.sha256]) return
    const current = generation.current
    const cacheGeneration = client.attachmentPreviewGeneration(profileId)
    setBusy(fileId)
    try {
      const cached = await client.attachmentPreviewGet(profileId, ownerNodeId, threadId, file.sha256).catch(() => null)
      if (generation.current !== current || client.attachmentPreviewGeneration(profileId) !== cacheGeneration) return
      if (cached) {
        try {
          const result = checkedPreview(cached, file)
          setPreviews(previous => ({ ...previous, [file.sha256]: result }))
          return
        } catch { /* Reload a malformed cache entry from the verified service. */ }
      }
      if (!online) throw new Error('Connect to the primary to load this attachment.')
      const reply = await client.request(profileId, { channel: 'sessions', method: 'attachment.preview', params: { threadId, sha256: file.sha256 }, targetNodeId: ownerNodeId }, 'online')
      if (reply.error) throw new Error(reply.error.message)
      const result = checkedPreview(reply.result, file)
      if (generation.current !== current || client.attachmentPreviewGeneration(profileId) !== cacheGeneration) return
      await client.attachmentPreviewPut(profileId, ownerNodeId, threadId, file.sha256, cacheGeneration, { attachment: { sha256: file.sha256, sizeBytes: file.sizeBytes }, ...result }).catch(() => false)
      if (generation.current === current && client.attachmentPreviewGeneration(profileId) === cacheGeneration) setPreviews(previous => ({ ...previous, [file.sha256]: result }))
    } catch (cause) {
      if (generation.current === current && client.attachmentPreviewGeneration(profileId) === cacheGeneration) setError({ id: fileId, message: cause instanceof Error ? cause.message : 'The attachment could not be loaded. Try again.' })
    } finally { if (generation.current === current) setBusy('') }
  }

  return <section className="thread-attachments" aria-label="Conversation attachments">
    <h2>Attachments</h2>
    {files.map(file => {
      const result = previews[file.sha256]
      const fileId = attachmentKey(file)
      const expanded = Boolean(open[fileId])
      return <div className="thread-attachment" key={fileId}>
        <button className="thread-attachment-trigger" aria-label={`Preview ${file.name}`} aria-expanded={expanded} disabled={busy === fileId} onClick={() => void preview(file)}>
          {file.kind === 'image' ? <ImageIcon size={17} aria-hidden /> : <FileText size={17} aria-hidden />}
          <span><strong>{file.name}</strong><small>{new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 }).format(file.sizeBytes / 1024)} KB</small></span>
          <small>{busy === fileId ? 'Loading…' : expanded ? 'Hide' : 'Preview'}</small>
        </button>
        {expanded && error?.id === fileId && <div className="attachment-feedback" role="alert"><p>{error.message}</p><button className="text-button" disabled={!online} onClick={() => void preview(file, true)}>Try again</button></div>}
        {expanded && result && (result.preview === null
          ? <p className="helper-copy">{result.reason === 'preview-too-large' ? 'Preview is available for files up to 2 MB.' : 'Preview is unavailable for this file type.'}</p>
          : file.kind === 'image' ? <img className="thread-attachment-image" src={result.preview} alt={file.name} />
            : <pre className="thread-attachment-text" tabIndex={0} aria-label={`Attachment preview ${file.name}`}>{result.preview}</pre>)}
      </div>
    })}
  </section>
}
