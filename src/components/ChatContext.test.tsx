import { render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { ChatContextBar, ChatQueue } from './ChatContext'
import type { ChatAttachment, ChatSubmission, QueueSnapshot } from '../lib/chat'

const { chat, openDialog } = vi.hoisted(() => ({
  chat: {
    importPaths: vi.fn(), preview: vi.fn(), removeAttachment: vi.fn(),
    pause: vi.fn(), resume: vi.fn(), editQueued: vi.fn(), removeQueued: vi.fn(), steerQueued: vi.fn(),
  },
  openDialog: vi.fn(),
}))
vi.mock('../lib/chat', () => ({ chat }))
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: openDialog }))

const attachment = (overrides: Partial<ChatAttachment> = {}): ChatAttachment => ({ id:'att-1', name:'notes.txt', mime:'text/plain', size:24, kind:'file', status:'ready', ...overrides })
const submission = (overrides: Partial<ChatSubmission> = {}): ChatSubmission => ({
  id:'sub-1', threadId:'thread-1', document:{version:1,text:'Review this change',references:[],quotes:[]}, attachmentIds:[],
  settings:{model:'sol',effort:'medium',mode:'default'}, delivery:'queue', status:'queued', revision:1, ...overrides,
})
const snapshot = (entries:ChatSubmission[], paused=false):QueueSnapshot => ({entries,state:{paused}})

beforeEach(() => { vi.clearAllMocks() })

describe('ChatContextBar', () => {
  it('offers native file import, renders ordered attachment cards, and removes only after revocation succeeds', async () => {
    const user = userEvent.setup()
    const onAttachmentsChange = vi.fn()
    const item = attachment()
    openDialog.mockResolvedValue(['/project/notes.txt'])
    chat.importPaths.mockResolvedValue([item])
    chat.removeAttachment.mockResolvedValue(undefined)
    const { rerender } = render(<ChatContextBar ownerId="draft-1" attachments={[attachment({id:'first',name:'first.png',kind:'image',mime:'image/png'}), item]} onAttachmentsChange={onAttachmentsChange} onError={vi.fn()} />)

    await user.click(screen.getByRole('button', {name:/Add context/i}))
    await user.click(screen.getByRole('menuitem', {name:/Attach files/i}))
    expect(openDialog).toHaveBeenCalledWith({multiple:true,directory:false})
    expect(chat.importPaths).toHaveBeenCalledWith('draft-1',['/project/notes.txt'])
    expect(await screen.findByText('first.png')).toBeInTheDocument()
    expect(screen.getByText('notes.txt')).toBeInTheDocument()
    const cards=within(screen.getByRole('list',{name:'Attached context'})).getAllByRole('listitem')
    expect(cards.map(card=>card.textContent)).toEqual(expect.arrayContaining([expect.stringContaining('first.png'),expect.stringContaining('notes.txt')]))
    expect(cards[0]).toContainElement(screen.getByText('first.png'))
    expect(cards[1]).toContainElement(screen.getByText('notes.txt'))
    const remove = screen.getByRole('button',{name:'Remove notes.txt'})
    await user.click(remove)
    expect(chat.removeAttachment).toHaveBeenCalledWith('draft-1','att-1')
    rerender(<ChatContextBar ownerId="draft-1" attachments={[attachment({id:'first',name:'first.png',kind:'image',mime:'image/png'})]} onAttachmentsChange={onAttachmentsChange} onError={vi.fn()} />)
    expect(screen.queryByText('notes.txt')).not.toBeInTheDocument()
    expect(onAttachmentsChange).toHaveBeenCalledWith(expect.arrayContaining([item]))
  })

  it('shows a retry action after native import failure without losing the chosen paths', async () => {
    const user = userEvent.setup()
    const onAttachmentsChange = vi.fn()
    openDialog.mockResolvedValue(['/project/a.txt'])
    chat.importPaths.mockRejectedValueOnce(new Error('Storage unavailable')).mockResolvedValueOnce([attachment({name:'a.txt'})])
    render(<ChatContextBar ownerId="draft-1" attachments={[]} onAttachmentsChange={onAttachmentsChange} onError={vi.fn()} />)
    await user.click(screen.getByRole('button',{name:/Add context/i}))
    await user.click(screen.getByRole('menuitem',{name:/Attach files/i}))
    expect(await screen.findByRole('alert')).toHaveTextContent('Storage unavailable')
    await user.click(screen.getByRole('button',{name:'Retry import'}))
    await waitFor(()=>expect(chat.importPaths).toHaveBeenCalledTimes(2))
    expect(chat.importPaths).toHaveBeenLastCalledWith('draft-1',['/project/a.txt'])
    expect(onAttachmentsChange).toHaveBeenCalledWith([expect.objectContaining({name:'a.txt'})])
  })

  it('keeps successful imports and retries only the failed path from a multi-file selection', async () => {
    const user=userEvent.setup();const onAttachmentsChange=vi.fn()
    openDialog.mockResolvedValue(['/project/one.txt','/project/two.txt'])
    chat.importPaths.mockResolvedValueOnce([attachment({id:'one',name:'one.txt'})]).mockRejectedValueOnce(new Error('Busy')).mockResolvedValueOnce([attachment({id:'two',name:'two.txt'})])
    const {rerender}=render(<ChatContextBar ownerId="draft-1" attachments={[]} onAttachmentsChange={onAttachmentsChange} onError={vi.fn()} />)
    await user.click(screen.getByRole('button',{name:/Add context/i}));await user.click(screen.getByRole('menuitem',{name:/Attach files/i}))
    expect(onAttachmentsChange).toHaveBeenCalledWith([expect.objectContaining({id:'one'})])
    expect(screen.getByRole('alert')).toHaveTextContent('Could not attach a file: Busy')
    expect(chat.importPaths.mock.calls.slice(0,2)).toEqual([['draft-1',['/project/one.txt']],['draft-1',['/project/two.txt']]])
    rerender(<ChatContextBar ownerId="draft-1" attachments={[attachment({id:'one',name:'one.txt'})]} onAttachmentsChange={onAttachmentsChange} onError={vi.fn()} />)
    await user.click(screen.getByRole('button',{name:'Retry import'}))
    expect(chat.importPaths).toHaveBeenLastCalledWith('draft-1',['/project/two.txt'])
    expect(onAttachmentsChange).toHaveBeenNthCalledWith(1,[expect.objectContaining({id:'one'})])
    expect(onAttachmentsChange).toHaveBeenNthCalledWith(2,[expect.objectContaining({id:'one'}),expect.objectContaining({id:'two'})])
  })

  it('previews images in a keyboard-contained dialog and labels files as metadata only', async () => {
    const user = userEvent.setup()
    chat.preview.mockResolvedValue({attachment:attachment({name:'map.png',kind:'image',mime:'image/png'}),preview:'data:image/png;base64,AA=='})
    render(<ChatContextBar ownerId="thread-1" attachments={[attachment({name:'map.png',kind:'image',mime:'image/png'}),attachment({id:'doc',name:'plan.pdf',mime:'application/pdf'})]} onAttachmentsChange={vi.fn()} onError={vi.fn()} />)
    await user.click(screen.getByRole('button',{name:'Preview map.png'}))
    const dialog=await screen.findByRole('dialog',{name:'Preview map.png'})
    expect(within(dialog).getByRole('img',{name:'map.png'})).toHaveAttribute('src','data:image/png;base64,AA==')
    expect(screen.getByText(/Original available to tool readers · no extraction/)).toBeInTheDocument()
    const close=within(dialog).getByRole('button',{name:'Close preview'})
    expect(close).toHaveFocus()
    await user.keyboard('{Tab}')
    expect(close).toHaveFocus()
    await user.keyboard('{Escape}')
    await waitFor(()=>expect(screen.queryByRole('dialog',{name:'Preview map.png'})).not.toBeInTheDocument())
    expect(screen.getByRole('button',{name:'Preview map.png'})).toHaveFocus()
  })

  it('opens optional references and command pickers without interpreting local documents', async () => {
    const user=userEvent.setup();const onOpenReferences=vi.fn();const onOpenCommands=vi.fn()
    render(<ChatContextBar ownerId="thread-1" attachments={[]} onAttachmentsChange={vi.fn()} onError={vi.fn()} onOpenReferences={onOpenReferences} onOpenCommands={onOpenCommands} />)
    await user.click(screen.getByRole('button',{name:/Add context/i}))
    await user.click(screen.getByRole('menuitem',{name:/Add reference/i}))
    expect(onOpenReferences).toHaveBeenCalledOnce()
    await user.click(screen.getByRole('button',{name:/Add context/i}))
    await user.click(screen.getByRole('menuitem',{name:/Browse commands/i}))
    expect(onOpenCommands).toHaveBeenCalledOnce()
  })

  it('supports arrow navigation, Escape, outside dismissal, and restores focus to the menu trigger', async () => {
    const user=userEvent.setup()
    render(<><ChatContextBar ownerId="thread-1" attachments={[]} onAttachmentsChange={vi.fn()} onError={vi.fn()} onOpenReferences={vi.fn()} onOpenCommands={vi.fn()} /><button type="button">Outside</button></>)
    const trigger=screen.getByRole('button',{name:/Add context/i})
    await user.click(trigger)
    const items=screen.getAllByRole('menuitem')
    expect(items[0]).toHaveFocus()
    await user.keyboard('{ArrowDown}')
    expect(items[1]).toHaveFocus()
    await user.keyboard('{Escape}')
    expect(screen.queryByRole('menu')).not.toBeInTheDocument()
    expect(trigger).toHaveFocus()
    await user.click(trigger)
    await user.click(screen.getByRole('button',{name:'Outside'}))
    expect(screen.queryByRole('menu')).not.toBeInTheDocument()
  })

  it('previews supported text as literal text and explains unsupported originals are tool-readable without extraction', async () => {
    const user=userEvent.setup()
    const text=attachment({id:'text',name:'notes.txt',mime:'text/plain'})
    const pdf=attachment({id:'pdf',name:'manual.pdf',mime:'application/pdf'})
    chat.preview.mockResolvedValueOnce({attachment:text,preview:'<script>plain text</script>\nSecond line'}).mockResolvedValueOnce({attachment:pdf,preview:null,reason:'preview-binary'})
    render(<ChatContextBar ownerId="thread-1" attachments={[text,pdf]} onAttachmentsChange={vi.fn()} onError={vi.fn()} />)
    expect(screen.getByRole('button',{name:'Preview notes.txt'})).toBeInTheDocument()
    expect(screen.getByRole('button',{name:'Preview manual.pdf'})).toBeInTheDocument()
    await user.click(screen.getByRole('button',{name:'Preview notes.txt'}))
    const dialog=await screen.findByRole('dialog',{name:'Preview notes.txt'})
    expect(dialog.querySelector('pre')?.textContent).toBe('<script>plain text</script>\nSecond line')
    expect(within(dialog).queryByRole('img')).not.toBeInTheDocument()
    await user.keyboard('{Escape}')
    expect(screen.getByRole('button',{name:'Preview notes.txt'})).toHaveFocus()
    await user.click(screen.getByRole('button',{name:'Preview manual.pdf'}))
    expect(await screen.findByRole('alert')).toHaveTextContent(/original remains available to tool readers/i)
    expect(screen.getByRole('alert')).toHaveTextContent(/does not extract/i)
  })
})

describe('ChatQueue', () => {
  it('shows only pending rows and counts queued, dispatching, uncertain, and failed submissions', () => {
    render(<ChatQueue threadId="thread-1" snapshot={snapshot([
      submission({ id:'queued', document:{version:1,text:'Queued work',references:[],quotes:[]} }),
      submission({ id:'dispatching', status:'dispatching', document:{version:1,text:'Sending work',references:[],quotes:[]} }),
      submission({ id:'uncertain', status:'uncertain', document:{version:1,text:'Unconfirmed work',references:[],quotes:[]} }),
      submission({ id:'failed', status:'failed', document:{version:1,text:'Failed work',references:[],quotes:[]} }),
      submission({ id:'accepted', status:'accepted', document:{version:1,text:'Already sent',references:[],quotes:[]} }),
      submission({ id:'cancelled', status:'cancelled', document:{version:1,text:'Cancelled work',references:[],quotes:[]} }),
    ])} running={false} onChanged={vi.fn()} onError={vi.fn()} />)
    expect(within(screen.getByRole('region', { name:'Message queue' })).getByText('4')).toBeInTheDocument()
    const pending = screen.getByRole('list', {name:'Pending messages'})
    expect(pending).toHaveClass('chat-queue-list')
    expect(pending.querySelector('details')).not.toHaveAttribute('open')
    expect(screen.getAllByText('Queued work').length).toBeGreaterThan(0)
    expect(screen.getByText('Unconfirmed work')).toBeInTheDocument()
    expect(screen.queryByText('Already sent')).not.toBeInTheDocument()
    expect(screen.queryByText('Cancelled work')).not.toBeInTheDocument()
  })

  it('hides an empty unpaused queue and keeps a paused empty queue compact and resumable', async () => {
    const user = userEvent.setup()
    const props = { threadId:'thread-1', onChanged:vi.fn(), onError:vi.fn() }
    const { rerender } = render(<ChatQueue {...props} snapshot={snapshot([submission({status:'accepted'}), submission({id:'cancelled', status:'cancelled'})])} running={false} />)
    expect(screen.queryByRole('region', {name:'Message queue'})).not.toBeInTheDocument()
    chat.resume.mockResolvedValue(undefined)
    rerender(<ChatQueue {...props} snapshot={snapshot([], true)} running={false} />)
    expect(screen.getByText('Queue paused')).toBeInTheDocument()
    expect(screen.queryByText(/Messages you queue will appear here/)).not.toBeInTheDocument()
    await user.click(screen.getByRole('button', {name:'Resume queue'}))
    expect(chat.resume).toHaveBeenCalledWith('thread-1')
  })

  it('edits, removes, and steers queued items, and pauses or resumes the thread queue', async () => {
    const user=userEvent.setup();const onChanged=vi.fn();const entry=submission({expectedTurnId:'turn-current'})
    chat.editQueued.mockResolvedValue(undefined);chat.removeQueued.mockResolvedValue(undefined);chat.steerQueued.mockResolvedValue(undefined);chat.pause.mockResolvedValue(undefined);chat.resume.mockResolvedValue(undefined)
    const {container,rerender}=render(<ChatQueue threadId="thread-1" snapshot={snapshot([entry])} running={true} onChanged={onChanged} onError={vi.fn()} />)
    const row=screen.getByRole('listitem')
    await user.click(container.querySelector('summary')!)
    await user.click(within(row).getByRole('button',{name:'Edit'}))
    const edit=within(row).getByRole('textbox',{name:'Edit queued message'})
    await user.clear(edit);await user.type(edit,'Review the latest patch')
    await user.click(within(row).getByRole('button',{name:'Save edit'}))
    expect(chat.editQueued).toHaveBeenCalledWith('sub-1','Review the latest patch')
    await user.click(within(row).getByRole('button',{name:'Steer now'}))
    expect(chat.steerQueued).toHaveBeenCalledWith('sub-1')
    await user.click(within(row).getByRole('button',{name:'Remove'}))
    expect(chat.removeQueued).toHaveBeenCalledWith('sub-1')
    await user.click(screen.getByRole('button',{name:'Pause queue'}))
    expect(chat.pause).toHaveBeenCalledWith('thread-1')
    rerender(<ChatQueue threadId="thread-1" snapshot={snapshot([entry],true)} running={false} onChanged={onChanged} onError={vi.fn()} />)
    await user.click(screen.getByRole('button',{name:'Resume queue'}))
    expect(chat.resume).toHaveBeenCalledWith('thread-1')
    expect(onChanged).toHaveBeenCalled()
  })

  it('explains uncertain sends and never offers a retry', () => {
    render(<ChatQueue threadId="thread-1" snapshot={snapshot([submission({status:'uncertain',error:'Connection lost during send'})])} running={false} onChanged={vi.fn()} onError={vi.fn()} />)
    expect(screen.getByText(/delivery could not be confirmed/i)).toBeInTheDocument()
    expect(screen.getByText(/check the conversation before sending again/i)).toBeInTheDocument()
    expect(screen.queryByRole('button',{name:/retry/i})).not.toBeInTheDocument()
    expect(screen.queryByRole('button',{name:'Steer now'})).not.toBeInTheDocument()
  })

  it('uses the active thread state to enable steer without requiring a caller-supplied turn ID', () => {
    const {rerender}=render(<ChatQueue threadId="thread-1" snapshot={snapshot([submission()])} running={false} onChanged={vi.fn()} onError={vi.fn()} />)
    expect(screen.getByRole('button',{name:'Steer now'})).toBeDisabled()
    rerender(<ChatQueue threadId="thread-1" snapshot={snapshot([submission()])} running={true} onChanged={vi.fn()} onError={vi.fn()} />)
    expect(screen.getByRole('button',{name:'Steer now'})).toBeEnabled()
  })
})
