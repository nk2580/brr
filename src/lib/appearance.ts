export const SIDEBAR_COLORS = [
  { id: 'graphite', name: 'Graphite', background: 'oklch(19.53% .013 264.21)', foreground: 'oklch(82.22% .0137 262.37)' },
  { id: 'lime', name: 'Lime', background: 'oklch(92% .22 116)', foreground: 'oklch(18% .015 120)' },
  { id: 'yellow', name: 'Yellow', background: 'oklch(90% .18 95)', foreground: 'oklch(18% .015 120)' },
  { id: 'orange', name: 'Orange', background: 'oklch(78% .18 55)', foreground: 'oklch(18% .015 120)' },
  { id: 'red', name: 'Red', background: 'oklch(70% .2 28)', foreground: 'oklch(18% .015 120)' },
  { id: 'cyan', name: 'Cyan', background: 'oklch(83% .15 195)', foreground: 'oklch(18% .015 120)' },
  { id: 'blue', name: 'Blue', background: 'oklch(49% .22 264)', foreground: 'oklch(97% .008 88)' },
  { id: 'violet', name: 'Violet', background: 'oklch(49% .24 300)', foreground: 'oklch(97% .008 88)' },
  { id: 'pink', name: 'Pink', background: 'oklch(76% .2 350)', foreground: 'oklch(18% .015 120)' },
] as const

export type SidebarColorId = typeof SIDEBAR_COLORS[number]['id']
export const DEFAULT_SIDEBAR_COLOR: SidebarColorId = 'graphite'

const STORAGE_KEY = 'brr.appearance.sidebarColor'

function isSidebarColorId(value: string | null): value is SidebarColorId {
  return SIDEBAR_COLORS.some(color => color.id === value)
}

export function loadSidebarColor(): SidebarColorId {
  try {
    const value = localStorage.getItem(STORAGE_KEY)
    return isSidebarColorId(value) ? value : DEFAULT_SIDEBAR_COLOR
  } catch {
    return DEFAULT_SIDEBAR_COLOR
  }
}

export function saveSidebarColor(id: SidebarColorId): boolean {
  try {
    localStorage.setItem(STORAGE_KEY, id)
    return true
  } catch {
    return false
  }
}
