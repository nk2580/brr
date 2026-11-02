import { expect, it, vi } from 'vitest'
import { isExternalUrl, openExternalUrl } from './external-url'
const invoke = vi.hoisted(() => vi.fn(async () => {}))
vi.mock('@tauri-apps/api/core', () => ({invoke}))
it('opens HTTPS OAuth in the native browser and rejects executable or credential-bearing URLs', async () => {
  await openExternalUrl('https://accounts.example.com/oauth?state=xyz')
  expect(invoke).toHaveBeenCalledWith('open_external_url', {url:'https://accounts.example.com/oauth?state=xyz'})
  for (const url of ['javascript:alert(1)','file:///etc/passwd','http://external.example.com','https://user:password@example.com','https://example.com/\n']) {
    expect(isExternalUrl(url)).toBe(false)
    await expect(openExternalUrl(url)).rejects.toThrow('Only HTTPS')
  }
  expect(isExternalUrl('http://[::1]:8080/authorize')).toBe(true)
})
