const STORAGE_KEY = 'brr.layout.sidebarCollapsed'

export function loadSidebarCollapsed(): boolean {
  try {
    return localStorage.getItem(STORAGE_KEY) === 'true'
  } catch {
    return false
  }
}

export function saveSidebarCollapsed(collapsed: boolean): boolean {
  try {
    localStorage.setItem(STORAGE_KEY, String(collapsed))
    return true
  } catch {
    return false
  }
}
