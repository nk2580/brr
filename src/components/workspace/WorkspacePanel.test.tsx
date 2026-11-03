import {useEffect, useState} from 'react'
import {fireEvent, render, screen, waitFor} from '@testing-library/react'
import {beforeEach, expect, it, vi} from 'vitest'
import {WorkspacePanel} from './WorkspacePanel'

const native = vi.hoisted(() => ({browserRequest: vi.fn(), listen: vi.fn().mockResolvedValue(() => {})}))
vi.mock('@tauri-apps/api/event', () => ({listen: native.listen}))
vi.mock('./BrowserTab', () => ({
  browserRequest: native.browserRequest,
  BrowserTab: ({canSendNotes, onNativeId}: {canSendNotes: boolean; onNativeId: (id: string) => void}) => {
    useEffect(() => { onNativeId('native-tab') }, [])
    return <button disabled={!canSendNotes}>Share collected notes</button>
  },
}))
vi.mock('./FilesTab', () => ({FilesTab: ({root, initialPath, line, onDirtyChange}: {root: string; initialPath?: string; line?: number; onDirtyChange?: (dirty: boolean) => void}) => <div>Files root: {root}<span>{initialPath ?? 'no initial file'}:{line ?? 'no line'}</span><button onClick={() => onDirtyChange?.(true)}>Make files dirty</button></div>}))
beforeEach(() => {native.browserRequest.mockReset().mockResolvedValue({}); localStorage.clear()})

it('expands generated content in one workspace tab and unmounts it when another tool is selected', async () => {
  const props = {draftKey: 1, promotion: null, open: true, visible: true, request: 0, root: '', threadId: 'chat', desktop: false, onOpen: vi.fn(), onClose: vi.fn(), onWidth: vi.fn(), onSendNotes: vi.fn()}
  const closed = vi.fn()
  render(<WorkspacePanel {...props} uiView={{id: 'chat/call', title: 'Compare options', content: <button>Interactive control</button>}} onCloseUi={closed} />)
  expect(await screen.findByRole('tab', {name: 'Compare options'})).toHaveAttribute('aria-selected', 'true')
  expect(screen.getByRole('button', {name: 'Interactive control'})).toBeInTheDocument()
  fireEvent.click(screen.getByRole('button', {name: 'Add workspace tab'}))
  expect(screen.queryByRole('button', {name: 'Interactive control'})).not.toBeInTheDocument()
  fireEvent.click(screen.getByRole('button', {name: 'Close Compare options tab'}))
  expect(closed).toHaveBeenCalledOnce()
})

it('promotes a new-chat browser with its first chat, but never shares its notes with another chat or draft', async () => {
  const props = {draftKey: 1, promotion: null, open: true, visible: true, request: 1, root: '/original', threadId: null, desktop: true, onOpen: vi.fn(), onClose: vi.fn(), onWidth: vi.fn(), onSendNotes: vi.fn()}
  const view = render(<WorkspacePanel {...props} />)
  fireEvent.click(await screen.findByRole('button', {name: 'Browser'}))
  expect(screen.getByRole('button', {name: 'Share collected notes'})).toBeEnabled()
  view.rerender(<WorkspacePanel {...props} threadId="chat-a" promotion={{draftKey: 1, threadId: 'chat-a'}} />)
  await waitFor(() => expect(native.browserRequest).toHaveBeenLastCalledWith('browser.context', {threadId: 'chat-a', tabId: 'native-tab', ownerThreadId: 'chat-a'}))
  expect(screen.getByRole('button', {name: 'Share collected notes'})).toBeEnabled()
  view.rerender(<WorkspacePanel {...props} threadId="chat-b" />)
  expect(screen.getByRole('button', {name: 'Share collected notes'})).toBeDisabled()
  await waitFor(() => expect(native.browserRequest).toHaveBeenLastCalledWith('browser.context', {threadId: 'chat-b', tabId: 'native-tab', ownerThreadId: 'chat-a'}))
  view.rerender(<WorkspacePanel {...props} draftKey={2} />)
  expect(screen.getByRole('button', {name: 'Share collected notes'})).toBeDisabled()
})

it('lets each new tab choose a workspace root and keeps that root pinned', async () => {
  const props = {draftKey: 1, promotion: null, open: true, visible: true, request: 1, root: '/repo/a', roots: ['/repo/a', '/repo/b'], threadId: 'chat', desktop: true, onOpen: vi.fn(), onClose: vi.fn(), onWidth: vi.fn(), onSendNotes: vi.fn()}
  render(<WorkspacePanel {...props} />)
  fireEvent.change(await screen.findByRole('combobox', {name: 'Folder'}), {target: {value: '/repo/b'}})
  fireEvent.click(screen.getByRole('button', {name: 'Files'}))
  expect(screen.getByText('Files root: /repo/b')).toBeInTheDocument()
  fireEvent.click(screen.getByRole('button', {name: 'Add workspace tab'}))
  expect(screen.getByRole('combobox', {name: 'Folder'})).toHaveValue('/repo/a')
  fireEvent.click(screen.getByRole('button', {name: 'Files'}))
  expect(screen.getByText('Files root: /repo/a')).toBeInTheDocument()
  fireEvent.click(screen.getAllByRole('tab').find(tab => tab.getAttribute('title') === 'Files · /repo/b')!)
  expect(screen.getByText('Files root: /repo/b')).toBeInTheDocument()
})

it('keeps a scratch session available for browsing without exposing its internal folder to file tools', async () => {
  const props = {draftKey: 1, promotion: null, open: true, visible: true, request: 1, root: '/private/scratch', roots: [], threadId: 'chat', desktop: true, onOpen: vi.fn(), onClose: vi.fn(), onWidth: vi.fn(), onSendNotes: vi.fn()}
  render(<WorkspacePanel {...props} />)
  expect(await screen.findByRole('button', {name: 'Browser'})).toBeEnabled()
  for (const tool of ['Terminal', 'Git diff', 'Files']) expect(screen.getByRole('button', {name: tool})).toBeDisabled()
  expect(screen.queryByText('/private/scratch')).not.toBeInTheDocument()
})

it('keeps tab state while hidden and supports keyboard tab selection', async () => {
  const props = {draftKey: 1, promotion: null, open: true, visible: true, request: 1, root: '', threadId: null, desktop: false, onOpen: vi.fn(), onClose: vi.fn(), onWidth: vi.fn(), onSendNotes: vi.fn()}
  const view = render(<WorkspacePanel {...props} />)
  fireEvent.click(await screen.findByRole('button', {name: 'Browser'}))
  fireEvent.click(screen.getByRole('button', {name: 'Add workspace tab'}))
  const newTab = screen.getByRole('tab', {name: 'New tab'})
  fireEvent.keyDown(newTab, {key: 'ArrowLeft'})
  expect(screen.getByRole('tab', {name: 'Browser'})).toHaveFocus()
  expect(screen.getByRole('tab', {name: 'Browser'})).toHaveAttribute('aria-selected', 'true')
  view.rerender(<WorkspacePanel {...props} open={false} />)
  view.rerender(<WorkspacePanel {...props} />)
  expect(screen.getAllByRole('tab')).toHaveLength(2)
  expect(screen.getByRole('tab', {name: 'Browser'})).toHaveAttribute('aria-selected', 'true')
})

it('expands and restores the workspace without unmounting its tab or changing the saved width', async () => {
  localStorage.setItem('brr.workspace.width', '620')
  const props = {draftKey: 1, promotion: null, open: true, visible: true, request: 0, root: '', threadId: 'chat', desktop: false, onOpen: vi.fn(), onClose: vi.fn(), onWidth: vi.fn(), onSendNotes: vi.fn()}
  function ControlledPanel() {
    const [expanded, setExpanded] = useState(false)
    return <WorkspacePanel {...props} expanded={expanded} onExpandedChange={setExpanded} uiView={{id: 'chat/call', title: 'Interactive response', content: <input aria-label="Response value" defaultValue="kept state" />}} />
  }
  const {container} = render(<ControlledPanel />)
  const panel = container.querySelector('.workspace-panel')!
  expect(await screen.findByRole('tab', {name: 'Interactive response'})).toBeInTheDocument()
  const input = screen.getByRole('textbox', {name: 'Response value'})
  input.focus()
  fireEvent.change(input, {target: {value: 'edited state'}})
  fireEvent.click(screen.getByRole('button', {name: 'Expand workspace panel'}))
  expect(panel).toHaveAttribute('data-expanded', 'true')
  expect(screen.getByRole('button', {name: 'Restore split view'})).toHaveAttribute('aria-pressed', 'true')
  expect(screen.queryByRole('separator', {name: 'Resize workspace panel'})).not.toBeInTheDocument()
  expect(screen.getByRole('textbox', {name: 'Response value'})).toBe(input)
  fireEvent.click(screen.getByRole('button', {name: 'Restore split view'}))
  expect(panel).toHaveAttribute('data-expanded', 'false')
  expect(screen.getByRole('separator', {name: 'Resize workspace panel'})).toBeInTheDocument()
  expect(screen.getByRole('textbox', {name: 'Response value'})).toBe(input)
  expect(input).toHaveValue('edited state')
  expect(localStorage.getItem('brr.workspace.width')).toBe('620')
})


it('opens a referenced file in its exact worktree root and selects the requested line', async () => {
  const props = {draftKey: 1, promotion: null, open: true, visible: true, request: 0, root: '/canonical', roots: ['/canonical'], threadId: 'chat-a', desktop: false, onOpen: vi.fn(), onClose: vi.fn(), onWidth: vi.fn(), onSendNotes: vi.fn(), fileOpen: {id: 'request-1', root: '/worktree/branch', path: 'src/feature.ts', line: 17, threadId: 'chat-a'}}
  render(<WorkspacePanel {...props} />)
  expect(await screen.findByRole('tab', {name: 'Files'})).toHaveAttribute('aria-selected', 'true')
  expect(screen.getByText('Files root: /worktree/branch')).toBeInTheDocument()
  expect(screen.getByText('src/feature.ts:17')).toBeInTheDocument()
  expect(props.onOpen).toHaveBeenCalled()
})

it('opens a referenced file in a fresh tab when the same-chat Files tab has unsaved edits', async () => {
  const props = {draftKey: 1, promotion: null, open: true, visible: true, request: 1, root: '/repo', roots: ['/repo'], threadId: 'chat-a', desktop: true, onOpen: vi.fn(), onClose: vi.fn(), onWidth: vi.fn(), onSendNotes: vi.fn()}
  const view = render(<WorkspacePanel {...props} />)
  fireEvent.click(await screen.findByRole('button', {name: 'Files'}))
  fireEvent.click(screen.getByRole('button', {name: 'Make files dirty'}))
  expect(screen.getByLabelText('Unsaved changes')).toBeInTheDocument()
  view.rerender(<WorkspacePanel {...props} fileOpen={{id: 'request-2', root: '/repo', path: 'src/next.ts', line: 9, threadId: 'chat-a'}} />)
  expect(await screen.findByText('src/next.ts:9')).toBeInTheDocument()
  expect(screen.getAllByRole('tab')).toHaveLength(2)
  expect(screen.getAllByText('Files root: /repo')).toHaveLength(2)
})


it('reuses a clean Files tab for another file request from the same chat and root', async () => {
  const props = {draftKey: 1, promotion: null, open: true, visible: true, request: 0, root: '/repo', roots: ['/repo'], threadId: 'chat-a', desktop: true, onOpen: vi.fn(), onClose: vi.fn(), onWidth: vi.fn(), onSendNotes: vi.fn()}
  const view = render(<WorkspacePanel {...props} fileOpen={{id: 'request-a', root: '/repo', path: 'src/first.ts', line: 3, threadId: 'chat-a'}} />)
  expect(await screen.findByText('src/first.ts:3')).toBeInTheDocument()
  view.rerender(<WorkspacePanel {...props} fileOpen={{id: 'request-b', root: '/repo', path: 'src/second.ts', line: 8, threadId: 'chat-a'}} />)
  expect(await screen.findByText('src/second.ts:8')).toBeInTheDocument()
  expect(screen.getAllByRole('tab')).toHaveLength(1)
})
