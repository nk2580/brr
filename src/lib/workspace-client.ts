import { invoke } from '@tauri-apps/api/core'

/** Send a request to the native workspace service. Paths are relative to the supplied root. */
export function workspaceRequest<T>(method: string, params: Record<string, unknown> = {}): Promise<T> {
  return invoke<T>('workspace_request', {method, params})
}
