import { fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { expect, it, vi } from 'vitest'
import { ProjectMenu, type ProjectMenuAction } from './ProjectMenu'

const makeActions = (overrides: Partial<Record<string, () => void | Promise<void>>> = {}): ProjectMenuAction[] => [
  { id: 'settings', label: 'Project settings', onSelect: overrides.settings ?? vi.fn() },
  { id: 'reveal', label: 'Reveal in File Manager', onSelect: overrides.reveal ?? vi.fn() },
  { id: 'delete', label: 'Delete project', onSelect: overrides.delete ?? vi.fn(), destructive: true },
]

it('opens an accessible menu for the named project and runs only its selected action', async () => {
  const user = userEvent.setup()
  const selected = vi.fn()
  const other = vi.fn()
  render(<><ProjectMenu projectName="Brr" projectId="project-brr" actions={makeActions({ reveal: selected })} />
    <ProjectMenu projectName="Other" projectId="project-other" actions={makeActions({ reveal: other })} /></>)

  const trigger = screen.getByRole('button', { name: 'Manage project Brr' })
  await user.click(trigger)
  expect(trigger).toHaveAttribute('aria-haspopup', 'menu')
  expect(trigger).toHaveAttribute('aria-expanded', 'true')
  const menu = screen.getByRole('menu')
  expect(menu).toHaveAttribute('data-project-id', 'project-brr')
  await user.click(within(menu).getByRole('menuitem', { name: 'Reveal in File Manager' }))
  expect(selected).toHaveBeenCalledOnce()
  expect(other).not.toHaveBeenCalled()
  expect(screen.queryByRole('menu')).not.toBeInTheDocument()
  await waitFor(() => expect(trigger).toHaveFocus())
})

it('moves through enabled items with arrow, home and end keys, skipping disabled actions', async () => {
  const user = userEvent.setup()
  render(<ProjectMenu projectName="Brr" projectId="project-brr" actions={[
    { id: 'settings', label: 'Project settings', onSelect: vi.fn() },
    { id: 'toggle', label: 'Toggle', onSelect: vi.fn(), disabled: true },
    { id: 'reveal', label: 'Reveal in File Manager', onSelect: vi.fn() },
  ]} />)
  const trigger = screen.getByRole('button', { name: 'Manage project Brr' })
  trigger.focus()
  await user.keyboard('{ArrowDown}')
  const settings = screen.getByRole('menuitem', { name: 'Project settings' })
  const reveal = screen.getByRole('menuitem', { name: 'Reveal in File Manager' })
  await waitFor(() => expect(settings).toHaveFocus())
  await user.keyboard('{ArrowDown}')
  expect(reveal).toHaveFocus()
  await user.keyboard('{Home}')
  expect(settings).toHaveFocus()
  await user.keyboard('{End}')
  expect(reveal).toHaveFocus()
})

it('opens from Enter and focuses the first enabled item', async () => {
  const user = userEvent.setup()
  render(<ProjectMenu projectName="Brr" projectId="project-brr" actions={makeActions()} />)
  const trigger = screen.getByRole('button', { name: 'Manage project Brr' })
  trigger.focus()
  await user.keyboard('{Enter}')
  await waitFor(() => expect(screen.getByRole('menuitem', { name: 'Project settings' })).toHaveFocus())
})

it('opens from Space and focuses the first enabled item', async () => {
  const user = userEvent.setup()
  render(<ProjectMenu projectName="Brr" projectId="project-brr" actions={makeActions()} />)
  const trigger = screen.getByRole('button', { name: 'Manage project Brr' })
  trigger.focus()
  await user.keyboard(' ')
  await waitFor(() => expect(screen.getByRole('menuitem', { name: 'Project settings' })).toHaveFocus())
})

it('Escape closes and restores trigger focus; outside click closes without stealing focus', async () => {
  const user = userEvent.setup()
  render(<><ProjectMenu projectName="Brr" projectId="project-brr" actions={makeActions()} /><button>Outside</button></>)
  const trigger = screen.getByRole('button', { name: 'Manage project Brr' })
  await user.click(trigger)
  await user.keyboard('{Escape}')
  expect(screen.queryByRole('menu')).not.toBeInTheDocument()
  expect(trigger).toHaveFocus()

  await user.click(trigger)
  const outside = screen.getByRole('button', { name: 'Outside' })
  await user.click(outside)
  expect(screen.queryByRole('menu')).not.toBeInTheDocument()
  expect(outside).toHaveFocus()
})

it('dismisses on Tab and does not allow disabled project actions to activate', async () => {
  const user = userEvent.setup()
  const disabledAction = vi.fn()
  render(<><ProjectMenu projectName="Brr" projectId="project-brr" actions={[
    { id: 'settings', label: 'Project settings', onSelect: vi.fn() },
    { id: 'archive', label: 'Archive chats', onSelect: disabledAction, disabled: true },
  ]} /><button>Next</button></>)
  await user.click(screen.getByRole('button', { name: 'Manage project Brr' }))
  const disabled = screen.getByRole('menuitem', { name: 'Archive chats' })
  expect(disabled).toBeDisabled()
  await user.click(disabled)
  expect(disabledAction).not.toHaveBeenCalled()
  await user.keyboard('{Tab}')
  expect(screen.queryByRole('menu')).not.toBeInTheDocument()
})

it('moves focus before and after the trigger when tabbing from the portaled menu', async () => {
  const user = userEvent.setup()
  render(<><button>Before</button><ProjectMenu projectName="Brr" projectId="project-brr" actions={makeActions()} /><button>After</button></>)
  const trigger = screen.getByRole('button', { name: 'Manage project Brr' })
  await user.click(trigger)
  await user.keyboard('{ArrowDown}')
  await waitFor(() => expect(screen.getByRole('menuitem', { name: 'Project settings' })).toHaveFocus())
  await user.keyboard('{Shift>}{Tab}{/Shift}')
  expect(screen.queryByRole('menu')).not.toBeInTheDocument()
  expect(screen.getByRole('button', { name: 'Before' })).toHaveFocus()

  await user.click(trigger)
  await user.keyboard('{ArrowDown}')
  await waitFor(() => expect(screen.getByRole('menuitem', { name: 'Project settings' })).toHaveFocus())
  await user.keyboard('{Tab}')
  expect(screen.queryByRole('menu')).not.toBeInTheDocument()
  expect(screen.getByRole('button', { name: 'After' })).toHaveFocus()
})

it('does not restore trigger focus for actions that open another view', async () => {
  const user = userEvent.setup()
  let destination: HTMLButtonElement | null = null
  const goToSettings = vi.fn(() => destination?.focus())
  render(<><ProjectMenu projectName="Brr" projectId="project-brr" actions={[
    { id: 'settings', label: 'Project settings', onSelect: goToSettings, restoreFocus: false },
  ]} /><button ref={node => { destination = node }}>Settings view</button></>)
  await user.click(screen.getByRole('button', { name: 'Manage project Brr' }))
  await user.click(screen.getByRole('menuitem', { name: 'Project settings' }))
  expect(goToSettings).toHaveBeenCalledOnce()
  await waitFor(() => expect(screen.getByRole('button', { name: 'Settings view' })).toHaveFocus())
})

it('prevents duplicate async activation and shows a rejected action near the menu', async () => {
  const user = userEvent.setup()
  let rejectAction!: (error: Error) => void
  const failed = vi.fn(() => new Promise<void>((_resolve, reject) => { rejectAction = reject }))
  render(<ProjectMenu projectName="Brr" projectId="project-brr" actions={makeActions({ reveal: failed })} />)
  await user.click(screen.getByRole('button', { name: 'Manage project Brr' }))
  const reveal = screen.getByRole('menuitem', { name: 'Reveal in File Manager' })
  await user.click(reveal)
  await user.click(reveal)
  expect(failed).toHaveBeenCalledOnce()
  rejectAction(new Error('Folder is unavailable'))
  expect(await screen.findByRole('alert')).toHaveTextContent('Folder is unavailable')
  expect(screen.getByRole('menu')).toBeInTheDocument()
})

it('remeasures and clamps the menu after an async error expands it', async () => {
  const user = userEvent.setup()
  let rejectAction!: (error: Error) => void
  render(<ProjectMenu projectName="Brr" projectId="project-brr" actions={[
    { id: 'reveal', label: 'Reveal', onSelect: () => new Promise<void>((_resolve, reject) => { rejectAction = reject }) },
  ]} />)
  const trigger = screen.getByRole('button', { name: 'Manage project Brr' })
  Object.defineProperty(window, 'innerHeight', { configurable: true, value: 600 })
  vi.spyOn(trigger, 'getBoundingClientRect').mockReturnValue({
    x: 50, y: 76, left: 50, top: 76, right: 74, bottom: 100, width: 24, height: 24, toJSON: () => ({}),
  } as DOMRect)
  await user.click(trigger)
  const menu = screen.getByRole('menu')
  Object.defineProperty(menu, 'offsetHeight', { configurable: true, get: () => screen.queryByRole('alert') ? 500 : 56 })
  await user.click(screen.getByRole('menuitem', { name: 'Reveal' }))
  rejectAction(new Error('A detailed failure message that increases the menu height'))
  await screen.findByRole('alert')
  expect(menu).toBe(screen.getByRole('menu'))
  expect(menu.offsetHeight).toBe(500)
  await waitFor(() => expect(menu.style.top).toBe('8px'))
})

it('clamps the fixed menu to the viewport for a long project label', async () => {
  const user = userEvent.setup()
  render(<ProjectMenu projectName={'A'.repeat(300)} projectId="project-brr" actions={makeActions()} />)
  const trigger = screen.getByRole('button', { name: `Manage project ${'A'.repeat(300)}` })
  vi.spyOn(trigger, 'getBoundingClientRect').mockReturnValue({
    x: 790, y: 580, left: 790, top: 580, right: 814, bottom: 604, width: 24, height: 24,
    toJSON: () => ({}),
  } as DOMRect)
  Object.defineProperty(window, 'innerWidth', { configurable: true, value: 800 })
  Object.defineProperty(window, 'innerHeight', { configurable: true, value: 600 })
  await user.click(trigger)
  const menu = screen.getByRole('menu')
  await waitFor(() => expect(menu.style.left).toBe('576px'))
  expect(menu.style.top).toBe('448px')
  expect(menu.style.maxWidth).toBe('calc(100vw - 16px)')
})

it('closes when the viewport scrolls or resizes', async () => {
  const user = userEvent.setup()
  render(<ProjectMenu projectName="Brr" projectId="project-brr" actions={makeActions()} />)
  await user.click(screen.getByRole('button', { name: 'Manage project Brr' }))
  fireEvent.scroll(window)
  expect(screen.queryByRole('menu')).not.toBeInTheDocument()
  await user.click(screen.getByRole('button', { name: 'Manage project Brr' }))
  fireEvent.resize(window)
  expect(screen.queryByRole('menu')).not.toBeInTheDocument()
})

it('keeps the menu open when its own content scrolls', async () => {
  const user = userEvent.setup()
  render(<ProjectMenu projectName="Brr" projectId="project-brr" actions={makeActions()} />)
  await user.click(screen.getByRole('button', { name: 'Manage project Brr' }))
  fireEvent.scroll(screen.getByRole('menu'))
  expect(screen.getByRole('menu')).toBeInTheDocument()
})
