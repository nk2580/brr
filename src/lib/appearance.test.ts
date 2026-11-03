import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { DEFAULT_SIDEBAR_COLOR, SIDEBAR_COLORS, loadSidebarColor, saveSidebarColor } from './appearance'

describe('sidebar appearance preference', () => {
  beforeEach(() => localStorage.clear())
  afterEach(() => vi.restoreAllMocks())

  it('defaults to the reference dark sidebar while retaining the saved color choices', () => {
    expect(SIDEBAR_COLORS.map(color => color.id)).toEqual(['graphite', 'lime', 'yellow', 'orange', 'red', 'cyan', 'blue', 'violet', 'pink'])
    expect(DEFAULT_SIDEBAR_COLOR).toBe('graphite')
    expect(SIDEBAR_COLORS.every(color => color.accent && color.accentForeground)).toBe(true)
    expect(SIDEBAR_COLORS.find(color => color.id === 'graphite')).toMatchObject({ accent: 'oklch(96% .008 88)', accentForeground: 'oklch(18% .015 120)' })
    expect(SIDEBAR_COLORS.find(color => color.id === 'blue')?.accent).toBe('oklch(72% .16 264)')
  })

  it('loads a saved valid color and falls back for missing or invalid values', () => {
    expect(loadSidebarColor()).toBe('graphite')
    localStorage.setItem('brr.appearance.sidebarColor', 'violet')
    expect(loadSidebarColor()).toBe('violet')
    localStorage.setItem('brr.appearance.sidebarColor', 'not-a-color')
    expect(loadSidebarColor()).toBe('graphite')
  })

  it('saves a selected color and reports storage failures safely', () => {
    expect(saveSidebarColor('cyan')).toBe(true)
    expect(localStorage.getItem('brr.appearance.sidebarColor')).toBe('cyan')
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => { throw new Error('storage disabled') })
    expect(saveSidebarColor('red')).toBe(false)
  })

  it('falls back when storage reads throw', () => {
    vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => { throw new Error('storage disabled') })
    expect(loadSidebarColor()).toBe('graphite')
  })
})
