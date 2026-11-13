export type RemoteTab = 'chats' | 'agents' | 'attention' | 'fleet'

export type Channel = 'system' | 'fleet' | 'sessions' | 'chat' | 'agents' | 'questions' | 'approvals' | 'projects' | 'sync'

export type Request = {
  id: string
  channel: Channel
  method: string
  params: Record<string, unknown>
  operationId?: string
  expectedEpoch?: string
  expectedRevision?: string
  targetNodeId?: string
}

export type ApiError = { code: string; message: string }
export type Reply<T = unknown> = { id: string; result?: T; error?: ApiError }
export type Profile = { id: string; name: string; endpoint: string; fingerprint: string; nodeId: string }
export type DeviceCredential = { deviceId: string; secret: string; trustedServerCertPem: string; fingerprint: string; nodeId: string }
export type PairInvitation = { endpoint: string; code: string; fingerprint: string; certificatePem: string; name?: string }
export type EventCursor = { epoch: string; sequence: number }
export type RemoteEvent = { nodeId: string; epoch: string; ownerEpoch?: string; sequence: number; message: Record<string, unknown> }
export type EventReplay = { epoch: string; latest: number; requiresSnapshot: boolean; events: RemoteEvent[] }
export type Snapshot = {
  protocolVersion: number; nodeId: string; epoch: string; cursor: EventCursor
  nodes: (Record<string, unknown> & { nodeId?: string; epoch?: string; online?: boolean; primary?: boolean; authenticated?: boolean; runtimeConnected?: boolean })[]; account: Record<string, unknown> | null
  models: Record<string, unknown>[]; threads: Record<string, unknown>[]
  projects: Record<string, unknown>[]; agents: Record<string, unknown>[]
  tasks: Record<string, unknown>[]; questions: Record<string, unknown>[]
  approvals: Record<string, unknown>[]; sync: Record<string, unknown>
  errors: { scope: string; message: string }[]
}

export type NativeBridge = {
  invoke<T>(command: string, args?: Record<string, unknown>): Promise<T>
  listen<T>(event: string, handler: (payload: T) => void): Promise<() => void>
}

export type RequestState = 'online' | 'offline' | 'uncertain'

export type PendingMutation = {
  profileId: string
  targetNodeId?: string
  operationId: string
  request: Request
  createdAt: number
  lastAttemptAt: number
  attempts: number
  lastError?: string
}

type PendingMutationIndex = { operationIds: string[] }

export class UncertainMutationError extends Error {
  readonly name = 'UncertainMutationError'
  constructor(readonly pending: PendingMutation, cause: unknown) {
    const detail = cause instanceof Error ? ` ${cause.message}` : ''
    super(`This action may have reached the node. Retry the saved action only after checking its current state.${detail}`)
  }
}

export class PendingMutationError extends Error {
  readonly name = 'PendingMutationError'
  constructor(readonly pending: PendingMutation) {
    super('An earlier action for this node may still be unresolved. Check its state, then retry the saved action.')
  }
}

const makeId = () => globalThis.crypto?.randomUUID?.() ?? `brr-${Date.now()}-${Math.random().toString(36).slice(2)}`

export class RemoteClient {
  private readonly activeMutationScopes = new Set<string>()
  private readonly indexLocks = new Map<string, Promise<void>>()
  private readonly attachmentPreviewGenerations = new Map<string, number>()
  private readonly attachmentPreviewLocks = new Map<string, Promise<void>>()

  constructor(private readonly bridge: NativeBridge) {}

  profiles() { return this.bridge.invoke<Profile[]>('remote_profiles') }
  snapshot(profileId: string) { return this.bridge.invoke<Snapshot>('remote_snapshot', { profileId }) }
  pair(invitation: PairInvitation, name?: string) {
    return this.bridge.invoke<Profile>('remote_pair', { invitation, name: name ?? invitation.name })
  }
  async forget(profileId: string) {
    this.attachmentPreviewGenerations.set(profileId, this.attachmentPreviewGeneration(profileId) + 1)
    await this.bridge.invoke<void>('remote_forget', { id: profileId })
    await this.withAttachmentPreviewLock(profileId, async () => {
      const index = await this.cacheGet<AttachmentPreviewIndex>(attachmentPreviewIndexKey(profileId))
      for (const key of index?.keys ?? []) await this.cachePut(key, null)
      await this.cachePut(attachmentPreviewIndexKey(profileId), { keys: [] })
    })
  }
  subscribe(profileId: string, cursor?: EventCursor) {
    return this.bridge.invoke<void>('remote_subscribe', { profileId, cursor })
  }
  unsubscribe(profileId: string) { return this.bridge.invoke<void>('remote_unsubscribe', { profileId }) }
  listen<T>(event: string, handler: (payload: T) => void) {
    return this.bridge.listen<T>(event, handler)
  }
  cacheGet<T>(key: string) {
    return this.bridge.invoke<T | null>('remote_cache_get', { key })
  }
  cachePut(key: string, value: unknown) {
    return this.bridge.invoke<void>('remote_cache_put', { key, value })
  }

  attachmentPreviewGeneration(profileId: string): number {
    return this.attachmentPreviewGenerations.get(profileId) ?? 0
  }

  attachmentPreviewGet(profileId: string, ownerNodeId: string, threadId: string, sha256: string) {
    return this.cacheGet<unknown>(attachmentPreviewKey(profileId, ownerNodeId, threadId, sha256))
  }

  async attachmentPreviewPut(profileId: string, ownerNodeId: string, threadId: string, sha256: string, generation: number, value: unknown): Promise<boolean> {
    return this.withAttachmentPreviewLock(profileId, async () => {
      if (generation !== this.attachmentPreviewGeneration(profileId)) return false
      const key = attachmentPreviewKey(profileId, ownerNodeId, threadId, sha256)
      const indexKey = attachmentPreviewIndexKey(profileId)
      const index = await this.cacheGet<AttachmentPreviewIndex>(indexKey)
      if (generation !== this.attachmentPreviewGeneration(profileId)) return false
      const keys = index && Array.isArray(index.keys) ? index.keys : []
      if (!keys.includes(key)) await this.cachePut(indexKey, { keys: [...keys, key] })
      if (generation !== this.attachmentPreviewGeneration(profileId)) return false
      await this.cachePut(key, value)
      return generation === this.attachmentPreviewGeneration(profileId)
    })
  }

  private async withAttachmentPreviewLock<T>(profileId: string, action: () => Promise<T>): Promise<T> {
    const previous = this.attachmentPreviewLocks.get(profileId) ?? Promise.resolve()
    let release!: () => void
    const current = new Promise<void>(resolve => { release = resolve })
    const tail = previous.then(() => current)
    this.attachmentPreviewLocks.set(profileId, tail)
    await previous
    try {
      return await action()
    } finally {
      release()
      if (this.attachmentPreviewLocks.get(profileId) === tail) this.attachmentPreviewLocks.delete(profileId)
    }
  }

  async pendingMutations(profileId: string, targetNodeId?: string): Promise<PendingMutation[]> {
    const index = await this.readMutationIndex(profileId)
    const entries = await Promise.all(index.operationIds.map(operationId => this.readPendingMutation(profileId, operationId)))
    const present = entries.filter((entry): entry is PendingMutation => entry !== null)
    return targetNodeId === undefined ? present : present.filter(entry => entry.targetNodeId === targetNodeId)
  }

  /** Explicitly resend a previously saved envelope without changing its request or operation ID. */
  async retryMutation<T = unknown>(profileId: string, operationId: string, state: RequestState = 'online'): Promise<Reply<T>> {
    if (state !== 'online') throw new Error(state === 'offline'
      ? 'This action needs a connection. The saved action is still on this device.'
      : 'A retry is already in progress. Check the node before trying again.')
    const pending = await this.readPendingMutation(profileId, operationId)
    if (!pending) throw new Error('The saved action is no longer available.')
    return this.sendPendingMutation<T>(pending, true)
  }

  async request<T>(profileId: string, request: Omit<Request, 'id'> & { id?: string }, state: RequestState = 'online'): Promise<Reply<T>> {
    if (state !== 'online') throw new Error(state === 'offline'
      ? 'This action needs a connection. Your draft is still saved on this device.'
      : 'This action may have reached the node. Check its current state before trying again.')
    const complete: Request = { ...request, id: request.id ?? makeId(), params: request.params ?? {} }
    if (isReadMethod(complete.channel, complete.method)) {
      delete complete.operationId
      return this.send<T>(profileId, complete)
    }
    complete.operationId = complete.operationId ?? makeId()
    return this.sendPendingMutation<T>({
      profileId,
      targetNodeId: complete.targetNodeId,
      operationId: complete.operationId,
      request: complete,
      createdAt: Date.now(),
      lastAttemptAt: Date.now(),
      attempts: 1,
    }, false)
  }

  private async sendPendingMutation<T>(pending: PendingMutation, alreadyStored: boolean): Promise<Reply<T>> {
    const scope = mutationScope(pending.profileId, pending.targetNodeId)
    if (this.activeMutationScopes.has(scope)) throw new PendingMutationError(pending)
    this.activeMutationScopes.add(scope)
    try {
      if (alreadyStored) {
        const conflict = (await this.pendingMutations(pending.profileId)).find(entry =>
          entry.targetNodeId === pending.targetNodeId && entry.operationId !== pending.operationId)
        if (conflict) throw new PendingMutationError(conflict)
        pending = { ...pending, lastAttemptAt: Date.now(), attempts: pending.attempts + 1, lastError: undefined }
        await this.cachePut(pendingMutationKey(pending.profileId, pending.operationId), pending)
      } else {
        await this.persistPendingMutation(pending)
      }

      let reply: Reply<T>
      try {
        reply = await this.send<T>(pending.profileId, pending.request)
      } catch (error) {
        const uncertain = { ...pending, lastError: error instanceof Error ? error.message : String(error) }
        await this.cachePut(pendingMutationKey(pending.profileId, pending.operationId), uncertain).catch(() => undefined)
        throw new UncertainMutationError(uncertain, error)
      }
      const code = reply.error?.code
      if (code && (unconfirmedReplyCodes.has(code) || (alreadyStored && noOwnerReceiptCodes.has(code)))) {
        // A structured HTTP reply can still lack the owner's durable receipt.
        // An offline answer to a later retry cannot disprove an earlier delivery.
        await this.cachePut(pendingMutationKey(pending.profileId, pending.operationId), {
          ...pending, lastError: reply.error?.message,
        }).catch(() => undefined)
      } else {
        await this.clearPendingMutation(pending.profileId, pending.operationId)
      }
      return reply
    } finally {
      this.activeMutationScopes.delete(scope)
    }
  }

  private async send<T>(profileId: string, request: Request): Promise<Reply<T>> {
    const reply = await this.bridge.invoke<Reply<T>>('remote_request', { profileId, request })
    if (reply?.id !== request.id) throw new Error('The node acknowledgement did not match this request.')
    return reply
  }

  private async persistPendingMutation(pending: PendingMutation): Promise<void> {
    await this.withMutationIndexLock(pending.profileId, async () => {
      const index = await this.readMutationIndex(pending.profileId)
      const existing = (await Promise.all(index.operationIds.map(id => this.readPendingMutation(pending.profileId, id))))
        .filter((entry): entry is PendingMutation => Boolean(entry))
      const sameOperation = existing.find(entry => entry.operationId === pending.operationId)
      if (sameOperation) throw new PendingMutationError(sameOperation)
      const conflict = existing.find(entry => entry.targetNodeId === pending.targetNodeId)
      if (conflict) throw new PendingMutationError(conflict)

      const nextIndex = { operationIds: [...index.operationIds, pending.operationId] }
      await this.cachePut(mutationIndexKey(pending.profileId), nextIndex)
      try {
        await this.cachePut(pendingMutationKey(pending.profileId, pending.operationId), pending)
      } catch (error) {
        await this.cachePut(mutationIndexKey(pending.profileId), index).catch(() => undefined)
        throw error
      }
    })
  }

  private async clearPendingMutation(profileId: string, operationId: string): Promise<void> {
    await this.withMutationIndexLock(profileId, async () => {
      const index = await this.readMutationIndex(profileId)
      await this.cachePut(pendingMutationKey(profileId, operationId), null)
      await this.cachePut(mutationIndexKey(profileId), { operationIds: index.operationIds.filter(id => id !== operationId) })
    })
  }

  private async readMutationIndex(profileId: string): Promise<PendingMutationIndex> {
    const index = await this.cacheGet<PendingMutationIndex>(mutationIndexKey(profileId))
    return index && Array.isArray(index.operationIds) ? index : { operationIds: [] }
  }

  private readPendingMutation(profileId: string, operationId: string): Promise<PendingMutation | null> {
    return this.cacheGet<PendingMutation>(pendingMutationKey(profileId, operationId)).then(value => value ?? null)
  }

  private async withMutationIndexLock<T>(profileId: string, action: () => Promise<T>): Promise<T> {
    const previous = this.indexLocks.get(profileId) ?? Promise.resolve()
    let release!: () => void
    const current = new Promise<void>(resolve => { release = resolve })
    const tail = previous.then(() => current)
    this.indexLocks.set(profileId, tail)
    await previous
    try {
      return await action()
    } finally {
      release()
      if (this.indexLocks.get(profileId) === tail) this.indexLocks.delete(profileId)
    }
  }
}

const mutationIndexKey = (profileId: string) => `remote:pending-mutation-index:${profileId}`
const pendingMutationKey = (profileId: string, operationId: string) => `remote:pending-mutation:${profileId}:${operationId}`
type AttachmentPreviewIndex = { keys: string[] }
const attachmentPreviewIndexKey = (profileId: string) => `remote:attachment-preview-index:${profileId}`
const attachmentPreviewKey = (profileId: string, ownerNodeId: string, threadId: string, sha256: string) => `attachment-preview:${profileId}:${ownerNodeId}:${threadId}:${sha256}`
const mutationScope = (profileId: string, targetNodeId?: string) => `${profileId}\u001f${targetNodeId ?? ''}`
const unconfirmedReplyCodes = new Set(['uncertain', 'invalid_reply', 'storage_error'])
const noOwnerReceiptCodes = new Set(['owner_offline', 'owner_unavailable', 'unauthorized', 'forbidden'])

// Mirror the exact scoped read methods in brr_remote_protocol::Request::policy.
const readMethods: Partial<Record<Channel, Set<string>>> = {
  system: new Set(['status', 'account', 'models', 'login.status']),
  fleet: new Set(['status', 'nodes', 'devices']),
  sessions: new Set(['list', 'read', 'turns', 'attachment.preview', 'attachment.read']),
  chat: new Set(['drafts.list', 'drafts.get', 'queue.list', 'attachments.list', 'attachments.preview', 'recovery.list']),
  agents: new Set([
    'agents.list', 'agents.read', 'agents.drafts', 'agents.sessions', 'agents.settings', 'agents.events', 'agents.decisions',
    'memory.projects', 'memory.list', 'memory.search', 'skills.list', 'tasks.list', 'tasks.proposals', 'workflows.list',
  ]),
  questions: new Set(['list']),
  approvals: new Set(['list']),
  projects: new Set(['list']),
  sync: new Set(['status', 'records', 'conflicts', 'history', 'previewProfile']),
}

export function isReadMethod(channel: Channel, method: string): boolean {
  return readMethods[channel]?.has(method) ?? false
}

export function createOperationId() { return makeId() }

export function parseInvitation(input: string): PairInvitation {
  const value = input.trim()
  let parsed: unknown
  if (value.startsWith('brr://pair?')) {
    const data = new URL(value).searchParams.get('data')
    if (!data) throw new Error('This Brr link is missing its pairing data.')
    parsed = JSON.parse(decodeBase64Url(data))
  } else {
    parsed = JSON.parse(value)
  }
  if (!parsed || typeof parsed !== 'object') throw new Error('Paste a Brr pairing link or invitation JSON.')
  const invite = parsed as Record<string, unknown>
  const endpoint = typeof invite.endpoint === 'string' ? invite.endpoint : ''
  if (!/^https:\/\//i.test(endpoint)) throw new Error('The invitation must use an HTTPS endpoint.')
  if (typeof invite.code !== 'string' || !invite.code) throw new Error('The invitation code is missing.')
  if (typeof invite.fingerprint !== 'string' || !invite.fingerprint) throw new Error('The server fingerprint is missing.')
  const certificatePem = typeof invite.certificatePem === 'string' ? invite.certificatePem : typeof invite.certificate_pem === 'string' ? invite.certificate_pem : ''
  if (!certificatePem.includes('BEGIN CERTIFICATE')) throw new Error('The invitation is missing its trusted server certificate.')
  return { endpoint, code: invite.code, fingerprint: invite.fingerprint, certificatePem, ...(typeof invite.name === 'string' ? { name: invite.name } : {}) }
}

function decodeBase64Url(value: string) {
  const normalized = value.replace(/-/g, '+').replace(/_/g, '/')
  const binary = globalThis.atob(normalized + '='.repeat((4 - normalized.length % 4) % 4))
  return new TextDecoder().decode(Uint8Array.from(binary, char => char.charCodeAt(0)))
}
