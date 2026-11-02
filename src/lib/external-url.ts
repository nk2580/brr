import { invoke } from '@tauri-apps/api/core'

export function isExternalUrl(value: unknown): value is string {
  if (typeof value !== 'string' || /[\u0000-\u0020\u007f]/.test(value)) return false
  try {
    const url = new URL(value)
    return !url.username && !url.password && (url.protocol === 'https:' || (url.protocol === 'http:' && ['localhost','127.0.0.1','[::1]'].includes(url.hostname)))
  } catch { return false }
}

export async function openExternalUrl(url:string):Promise<void> {
  if (!isExternalUrl(url)) throw new Error('Only HTTPS or local HTTP URLs can be opened.')
  await invoke('open_external_url', {url})
}
