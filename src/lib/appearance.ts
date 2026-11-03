export const SIDEBAR_COLORS = [
  { id: 'graphite', name: 'Graphite', background: 'oklch(16% .006 305)', foreground: 'oklch(96% .008 88)', accent: 'oklch(96% .008 88)', accentForeground: 'oklch(18% .015 120)' },
  { id: 'lime', name: 'Lime', background: 'oklch(92% .22 116)', foreground: 'oklch(18% .015 120)', accent: 'oklch(92% .22 116)', accentForeground: 'oklch(18% .015 120)' },
  { id: 'yellow', name: 'Yellow', background: 'oklch(90% .18 95)', foreground: 'oklch(18% .015 120)', accent: 'oklch(90% .18 95)', accentForeground: 'oklch(18% .015 120)' },
  { id: 'orange', name: 'Orange', background: 'oklch(78% .18 55)', foreground: 'oklch(18% .015 120)', accent: 'oklch(78% .18 55)', accentForeground: 'oklch(18% .015 120)' },
  { id: 'red', name: 'Red', background: 'oklch(70% .2 28)', foreground: 'oklch(18% .015 120)', accent: 'oklch(70% .2 28)', accentForeground: 'oklch(18% .015 120)' },
  { id: 'cyan', name: 'Cyan', background: 'oklch(83% .15 195)', foreground: 'oklch(18% .015 120)', accent: 'oklch(83% .15 195)', accentForeground: 'oklch(18% .015 120)' },
  { id: 'blue', name: 'Blue', background: 'oklch(49% .22 264)', foreground: 'oklch(97% .008 88)', accent: 'oklch(72% .16 264)', accentForeground: 'oklch(18% .015 120)' },
  { id: 'violet', name: 'Violet', background: 'oklch(49% .24 300)', foreground: 'oklch(97% .008 88)', accent: 'oklch(74% .18 300)', accentForeground: 'oklch(18% .015 120)' },
  { id: 'pink', name: 'Pink', background: 'oklch(76% .2 350)', foreground: 'oklch(18% .015 120)', accent: 'oklch(76% .2 350)', accentForeground: 'oklch(18% .015 120)' },
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
