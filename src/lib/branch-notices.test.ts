import { describe, expect, it } from 'vitest'
import { branchNoticeKey, loadBranchNoticeDismissals, saveBranchNoticeDismissals } from './branch-notices'

const notice = { id: 'op-1', sourceThreadId: 'source-1', threadId: 'target-1', phase: 'worktreeCreated', error: 'provider unavailable', cwd: '/project.feature' }

function memoryStorage(initial: Record<string, string> = {}) {
  const data = new Map(Object.entries(initial))
  return {
    getItem: (key: string) => data.get(key) ?? null,
    setItem: (key: string, value: string) => { data.set(key, value) },
  }
}

describe('interrupted branch notice dismissals', () => {
  it('keys dismissal by operation identity and stable source, target, phase, error, and path details', () => {
    const key = branchNoticeKey(notice)
    expect(branchNoticeKey({ ...notice })).toBe(key)
    for (const changed of [
      { ...notice, id: 'op-2' },
      { ...notice, sourceThreadId: 'source-2' },
      { ...notice, threadId: 'target-2' },
      { ...notice, phase: 'branchCreated' },
      { ...notice, error: 'permission denied' },
      { ...notice, cwd: '/other.feature' },
    ]) expect(branchNoticeKey(changed)).not.toBe(key)
  })

  it('round trips dismissed keys across app mounts', () => {
    const storage = memoryStorage()
    const key = branchNoticeKey(notice)
    saveBranchNoticeDismissals(new Set([key]), storage)
    expect(loadBranchNoticeDismissals(storage)).toEqual(new Set([key]))
  })

  it('treats malformed or unavailable storage as empty and makes writes best effort', () => {
    const malformed = memoryStorage({ 'brr:interrupted-branch-notice-dismissals': '{bad json' })
    expect(loadBranchNoticeDismissals(malformed)).toEqual(new Set())

    const unavailable = {
      getItem: () => { throw new Error('Storage unavailable') },
      setItem: () => { throw new Error('Storage unavailable') },
    }
    expect(loadBranchNoticeDismissals(unavailable)).toEqual(new Set())
    expect(() => saveBranchNoticeDismissals(new Set([branchNoticeKey(notice)]), unavailable)).not.toThrow()
    expect(loadBranchNoticeDismissals(null)).toEqual(new Set())
    expect(() => saveBranchNoticeDismissals(new Set(), null)).not.toThrow()
  })
})
