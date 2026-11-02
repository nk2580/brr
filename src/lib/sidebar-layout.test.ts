import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { loadSidebarCollapsed, saveSidebarCollapsed } from './sidebar-layout'

const STORAGE_KEY = 'brr.layout.sidebarCollapsed'

describe('sidebar collapse preference', () => {
  beforeEach(() => localStorage.clear())
  afterEach(() => vi.restoreAllMocks())

  it('defaults to expanded and only loads the exact true value as collapsed', () => {
    expect(loadSidebarCollapsed()).toBe(false)

    localStorage.setItem(STORAGE_KEY, 'true')
    expect(loadSidebarCollapsed()).toBe(true)

    for (const value of ['false', 'TRUE', '1', 'yes', '']) {
      localStorage.setItem(STORAGE_KEY, value)
      expect(loadSidebarCollapsed()).toBe(false)
    }
  })

  it('saves the preference and reports storage failures safely', () => {
    expect(saveSidebarCollapsed(true)).toBe(true)
    expect(localStorage.getItem(STORAGE_KEY)).toBe('true')

    expect(saveSidebarCollapsed(false)).toBe(true)
    expect(localStorage.getItem(STORAGE_KEY)).toBe('false')

    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => { throw new Error('storage disabled') })
    expect(saveSidebarCollapsed(true)).toBe(false)
  })

  it('defaults to expanded when storage reads throw', () => {
    vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => { throw new Error('storage disabled') })
    expect(loadSidebarCollapsed()).toBe(false)
  })
})
