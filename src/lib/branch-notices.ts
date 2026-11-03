export const BRANCH_NOTICE_DISMISSALS_KEY = 'brr:interrupted-branch-notice-dismissals'

export type PendingBranchNotice = {
  id: string
  sourceThreadId: string
  threadId?: string
  phase: string
  error?: string
  cwd?: string
}

type StorageAccess = Pick<Storage, 'getItem' | 'setItem'>

const browserStorage = (): StorageAccess | null => {
  try { return typeof window === 'undefined' ? null : window.localStorage }
  catch { return null }
}

export function branchNoticeKey(notice: PendingBranchNotice) {
  return JSON.stringify([
    notice.id,
    notice.sourceThreadId,
    notice.threadId ?? '',
    notice.phase,
    notice.error ?? '',
    notice.cwd ?? '',
  ])
}

export function loadBranchNoticeDismissals(storage: StorageAccess | null = browserStorage()) {
  if (!storage) return new Set<string>()
  try {
    const value: unknown = JSON.parse(storage.getItem(BRANCH_NOTICE_DISMISSALS_KEY) ?? '[]')
    return Array.isArray(value) ? new Set(value.filter((item): item is string => typeof item === 'string')) : new Set<string>()
  } catch { return new Set<string>() }
}

export function saveBranchNoticeDismissals(keys: ReadonlySet<string>, storage: StorageAccess | null = browserStorage()) {
  if (!storage) return
  try { storage.setItem(BRANCH_NOTICE_DISMISSALS_KEY, JSON.stringify([...keys])) }
  catch { /* Keep the in-memory dismissal even when browser storage is unavailable. */ }
}
