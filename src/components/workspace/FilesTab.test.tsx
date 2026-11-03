import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { FilesTab } from './FilesTab'

const api = vi.hoisted(() => ({ workspaceRequest: vi.fn() }))
vi.mock('@/lib/workspace-client', () => api)

describe('FilesTab', () => {
  beforeEach(() => api.workspaceRequest.mockReset())

  it('opens a requested worktree-relative file and selects its requested line', async () => {
    api.workspaceRequest.mockImplementation(async (method: string) => {
      if (method === 'directory.list') return {entries: []}
      if (method === 'file.read') return {path: 'src/feature.ts', content: 'first line\nsecond line\nthird line', version: 'worktree-v1'}
    })
    render(<FilesTab root="/worktree/branch" visible initialPath="src/feature.ts" line={2} />)
    const editor = await screen.findByRole('textbox', {name: 'File contents'}) as HTMLTextAreaElement
    await waitFor(() => expect(api.workspaceRequest).toHaveBeenCalledWith('file.read', {root: '/worktree/branch', path: 'src/feature.ts'}))
    await waitFor(() => expect(editor.selectionStart).toBe(11))
    expect(editor.selectionEnd).toBe(22)
    expect(editor).toHaveFocus()
  })

  it('keeps edits until explicit save and sends the file version', async () => {
    api.workspaceRequest.mockImplementation(async (method: string, params: {path?: string} = {}) => {
      if (method === 'directory.list') return {entries: params.path === 'src' ? [{name: 'main.ts', path: 'src/main.ts', kind: 'file', size: 3}] : [{name: 'src', path: 'src', kind: 'directory'}]}
      if (method === 'file.read') return {path: 'src/main.ts', content: 'old', version: 'v1'}
      if (method === 'file.write') return {path: 'src/main.ts', content: 'new', version: 'v2'}
      return {entries: [{name: 'main.ts', path: 'src/main.ts', kind: 'file', size: 3}]}
    })
    render(<FilesTab root="/project" visible />)
    fireEvent.click(await screen.findByRole('button', {name: 'src'}))
    await screen.findByRole('button', {name: 'main.ts'})
    fireEvent.click(screen.getByRole('button', {name: 'main.ts'}))
    const editor = await screen.findByRole('textbox', {name: 'File contents'}) as HTMLTextAreaElement
    fireEvent.change(editor, {target: {value: 'new'}})
    expect(screen.getByLabelText('Unsaved changes')).toBeInTheDocument()
    expect(api.workspaceRequest).not.toHaveBeenCalledWith('file.write', expect.anything())
    fireEvent.click(screen.getByRole('button', {name: 'Save'}))
    await waitFor(() => expect(api.workspaceRequest).toHaveBeenCalledWith('file.write', {
      root: '/project', path: 'src/main.ts', content: 'new', expectedVersion: 'v1',
    }))
    await waitFor(() => expect(screen.queryByLabelText('Unsaved changes')).not.toBeInTheDocument())
  })

  it('preserves a draft when the disk version is stale and offers a confirmed reload', async () => {
    api.workspaceRequest.mockImplementation(async (method: string) => {
      if (method === 'directory.list') return {entries: [{name: 'note.txt', path: 'note.txt', kind: 'file', size: 3}]}
      if (method === 'file.read') return {path: 'note.txt', content: 'disk', version: 'v1'}
      if (method === 'file.write') throw new Error('changed on disk')
    })
    render(<FilesTab root="/project" visible />)
    fireEvent.click(await screen.findByRole('button', {name: 'note.txt'}))
    const editor = await screen.findByRole('textbox', {name: 'File contents'}) as HTMLTextAreaElement
    fireEvent.change(editor, {target: {value: 'my draft'}})
    fireEvent.click(screen.getByRole('button', {name: 'Save'}))
    expect(await screen.findByText(/changed on disk/i)).toBeInTheDocument()
    expect(editor).toHaveValue('my draft')
    fireEvent.click(screen.getByRole('button', {name: 'Reload from disk'}))
    fireEvent.click(screen.getByRole('button', {name: 'Discard draft and reload'}))
    await waitFor(() => expect(screen.getByRole('textbox', {name: 'File contents'})).toHaveValue('disk'))
  })

  it('ignores an older file read that finishes after the newer selection', async () => {
    let resolveA!: (value: {path: string; content: string; version: string}) => void
    api.workspaceRequest.mockImplementation((method: string, params: {path?: string} = {}) => {
      if (method === 'directory.list') return Promise.resolve({entries: [
        {name:'a.txt',path:'a.txt',kind:'file',size:1}, {name:'b.txt',path:'b.txt',kind:'file',size:1},
      ]})
      if (method === 'file.read' && params.path === 'a.txt') return new Promise(resolve => { resolveA = resolve })
      if (method === 'file.read' && params.path === 'b.txt') return Promise.resolve({path:'b.txt',content:'B',version:'b1'})
    })
    render(<FilesTab root="/project" visible />)
    fireEvent.click(await screen.findByRole('button', {name:'a.txt'}))
    fireEvent.click(screen.getByRole('button', {name:'b.txt'}))
    await waitFor(() => expect(screen.getByRole('textbox', {name:'File contents'})).toHaveValue('B'))
    resolveA({path:'a.txt',content:'A',version:'a1'})
    await waitFor(() => expect(screen.getByRole('textbox', {name:'File contents'})).toHaveValue('B'))
  })

  it('does not reread and overwrite a dirty file when it is reselected', async () => {
    api.workspaceRequest.mockImplementation(async (method: string) => {
      if (method === 'directory.list') return {entries:[{name:'note.txt',path:'note.txt',kind:'file',size:1}]}
      if (method === 'file.read') return {path:'note.txt',content:'disk',version:'v1'}
    })
    render(<FilesTab root="/project" visible />)
    fireEvent.click(await screen.findByRole('button',{name:'note.txt'}))
    fireEvent.change(await screen.findByRole('textbox',{name:'File contents'}),{target:{value:'draft'}})
    fireEvent.click(screen.getByRole('button',{name:'note.txt'}))
    expect(screen.getByRole('textbox',{name:'File contents'})).toHaveValue('draft')
    expect(api.workspaceRequest.mock.calls.filter(([method])=>method==='file.read')).toHaveLength(1)
  })

  it('does not let a late save response replace a file opened after discarding the draft', async () => {
    let resolveSave!: (value: {path:string;content:string;version:string}) => void
    api.workspaceRequest.mockImplementation((method: string, params: {path?:string} = {}) => {
      if (method === 'directory.list') return Promise.resolve({entries:[{name:'a.txt',path:'a.txt',kind:'file',size:1},{name:'b.txt',path:'b.txt',kind:'file',size:1}]})
      if (method === 'file.read') return Promise.resolve({path:params.path!,content:params.path==='a.txt'?'A':'B',version:'v1'})
      if (method === 'file.write') return new Promise(resolve => { resolveSave=resolve })
    })
    render(<FilesTab root="/project" visible />)
    fireEvent.click(await screen.findByRole('button',{name:'a.txt'}))
    fireEvent.change(await screen.findByRole('textbox',{name:'File contents'}),{target:{value:'A draft'}})
    fireEvent.click(screen.getByRole('button',{name:'Save'}))
    fireEvent.click(screen.getByRole('button',{name:'b.txt'}))
    fireEvent.click(await screen.findByRole('button',{name:'Discard'}))
    await waitFor(()=>expect(screen.getByRole('textbox',{name:'File contents'})).toHaveValue('B'))
    resolveSave({path:'a.txt',content:'A draft',version:'v2'})
    await waitFor(()=>expect(screen.getByRole('textbox',{name:'File contents'})).toHaveValue('B'))
  })

  it('ignores a file read that resolves after switching project roots', async () => {
    let resolveOld!: (value: {path:string;content:string;version:string}) => void
    api.workspaceRequest.mockImplementation((method: string, params: {root?:string;path?:string} = {}) => {
      if (method === 'directory.list') return Promise.resolve({entries:[{name:'note.txt',path:'note.txt',kind:'file',size:1}]})
      if (method === 'file.read' && params.root === '/old') return new Promise(resolve=>{resolveOld=resolve})
      if (method === 'file.read') return Promise.resolve({path:'note.txt',content:'new root',version:'v2'})
    })
    const view=render(<FilesTab root="/old" visible />)
    fireEvent.click(await screen.findByRole('button',{name:'note.txt'}))
    view.rerender(<FilesTab root="/new" visible />)
    fireEvent.click(await screen.findByRole('button',{name:'note.txt'}))
    expect(await screen.findByRole('textbox',{name:'File contents'})).toHaveValue('new root')
    resolveOld({path:'note.txt',content:'old root',version:'v1'})
    await waitFor(()=>expect(screen.getByRole('textbox',{name:'File contents'})).toHaveValue('new root'))
  })
})
