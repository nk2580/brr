import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { GitDiffTab } from './GitDiffTab'

const api = vi.hoisted(() => ({ workspaceRequest: vi.fn() }))
vi.mock('@/lib/workspace-client', () => api)

describe('GitDiffTab', () => {
  beforeEach(() => api.workspaceRequest.mockReset())
  it('loads a selected file diff and switches between staged and worktree versions', async () => {
    api.workspaceRequest.mockImplementation(async (method: string, params: {staged?: boolean} = {}) => {
      if (method === 'git.status') return {branch: 'main', files: [{path: 'src/a.ts', indexStatus: 'M', worktreeStatus: 'M', untracked: false}]}
      if (method === 'git.diff') return {patch: params.staged ? '+staged line' : '+worktree line', truncated: false}
    })
    render(<GitDiffTab root="/project" visible />)
    fireEvent.click(await screen.findByRole('button', {name: 'src/a.ts'}))
    expect(await screen.findByText('+worktree line')).toBeInTheDocument()
    fireEvent.click(screen.getByRole('checkbox', {name: 'Staged'}))
    await waitFor(() => expect(screen.getByText('+staged line')).toBeInTheDocument())
    expect(api.workspaceRequest).toHaveBeenCalledWith('git.diff', {root: '/project', path: 'src/a.ts', staged: true})
  })

  it('ignores a selected file diff that resolves after a newer file selection', async () => {
    let resolveA!: (value: {patch:string;truncated:boolean}) => void
    api.workspaceRequest.mockImplementation((method: string, params: {path?:string} = {}) => {
      if (method === 'git.status') return Promise.resolve({branch:'main',files:[
        {path:'a.ts',indexStatus:'M',worktreeStatus:'M',untracked:false}, {path:'b.ts',indexStatus:'M',worktreeStatus:'M',untracked:false},
      ]})
      if (method === 'git.diff' && params.path === 'a.ts') return new Promise(resolve=>{resolveA=resolve})
      if (method === 'git.diff' && params.path === 'b.ts') return Promise.resolve({patch:'+diff B',truncated:false})
    })
    render(<GitDiffTab root="/project" visible />)
    fireEvent.click(await screen.findByRole('button',{name:'a.ts'}))
    fireEvent.click(screen.getByRole('button',{name:'b.ts'}))
    expect(await screen.findByText('+diff B')).toBeInTheDocument()
    resolveA({patch:'+diff A',truncated:false})
    await waitFor(()=>expect(screen.queryByText('+diff A')).not.toBeInTheDocument())
    expect(screen.getByText('+diff B')).toBeInTheDocument()
  })

  it('ignores a late status response from the previous project root', async () => {
    let resolveOld!: (value: {branch:string;files:{path:string;indexStatus:string;worktreeStatus:string;untracked:boolean}[]}) => void
    api.workspaceRequest.mockImplementation((_method: string, params: {root?:string} = {}) => {
      if (params.root === '/old') return new Promise(resolve=>{resolveOld=resolve})
      return Promise.resolve({branch:'new-root',files:[{path:'new.ts',indexStatus:'M',worktreeStatus:'M',untracked:false}]})
    })
    const view=render(<GitDiffTab root="/old" visible />)
    view.rerender(<GitDiffTab root="/new" visible />)
    expect(await screen.findByText('new-root')).toBeInTheDocument()
    resolveOld({branch:'old-root',files:[{path:'old.ts',indexStatus:'M',worktreeStatus:'M',untracked:false}]})
    await waitFor(()=>expect(screen.queryByText('old-root')).not.toBeInTheDocument())
    expect(screen.getByText('new-root')).toBeInTheDocument()
  })
})
