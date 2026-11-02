import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { createRef } from 'react'
import { expect, it } from 'vitest'
import { RailButton } from './RailButton'

it('shows a described tooltip while a rail button is hovered', async () => {
  const user = userEvent.setup()
  render(<RailButton label="Home"><span aria-hidden="true">H</span></RailButton>)
  const button = screen.getByRole('button', { name: 'Home' })

  await user.hover(button)

  const tooltip = screen.getByRole('tooltip', { name: 'Home' })
  expect(tooltip).toBeVisible()
  expect(button).toHaveAttribute('aria-describedby', tooltip.id)

  await user.unhover(button)
  expect(screen.queryByRole('tooltip')).not.toBeInTheDocument()
  expect(button).not.toHaveAttribute('aria-describedby')
})

it('opens the tooltip on keyboard focus and closes it on Escape or blur', async () => {
  const user = userEvent.setup()
  render(<RailButton label="Settings" aria-pressed="true" aria-current="page" aria-describedby="help-tip"><span aria-hidden="true">S</span></RailButton>)
  const button = screen.getByRole('button', { name: 'Settings' })
  await user.tab()

  const tooltip = screen.getByRole('tooltip', { name: 'Settings' })
  expect(button).toHaveFocus()
  expect(button).toHaveAttribute('aria-describedby', 'help-tip ' + tooltip.id)
  expect(button).toHaveAttribute('aria-pressed', 'true')
  expect(button).toHaveAttribute('aria-current', 'page')
  await user.keyboard('{Escape}')
  expect(screen.queryByRole('tooltip')).not.toBeInTheDocument()
  expect(button).toHaveAttribute('aria-describedby', 'help-tip')

  await user.tab()
  expect(screen.queryByRole('tooltip')).not.toBeInTheDocument()
})

it('shows a tooltip for a disabled rail button when its wrapper is hovered', async () => {
  const user = userEvent.setup()
  render(<RailButton label="Plugins and MCP" disabled><span aria-hidden="true">P</span></RailButton>)
  const button = screen.getByRole('button', { name: 'Plugins and MCP' })

  await user.hover(button)

  expect(button).toBeDisabled()
  expect(screen.getByRole('tooltip', { name: 'Plugins and MCP' })).toBeVisible()
})

it('forwards its ref to the actual button and does not set a browser title', () => {
  const ref = createRef<HTMLButtonElement>()
  render(<RailButton ref={ref} label="Home" aria-pressed="false"><span aria-hidden="true">H</span></RailButton>)

  expect(ref.current).toBe(screen.getByRole('button', { name: 'Home' }))
  expect(ref.current).not.toHaveAttribute('title')
  expect(ref.current).toHaveAttribute('type', 'button')
  expect(ref.current).toHaveClass('button-ghost', 'button-icon')
})

it('closes a hovered rail tooltip when Escape is pressed from another focused control', async () => {
  const user = userEvent.setup()
  render(<><textarea aria-label="Message" /><RailButton label="Home"><span aria-hidden="true">H</span></RailButton></>)
  const message = screen.getByRole('textbox', { name: 'Message' })
  const button = screen.getByRole('button', { name: 'Home' })
  await user.click(message)
  await user.hover(button)
  expect(screen.getByRole('tooltip', { name: 'Home' })).toBeVisible()

  await user.keyboard('{Escape}')

  expect(screen.queryByRole('tooltip')).not.toBeInTheDocument()
})

it('closes a disabled button tooltip on Escape', async () => {
  const user = userEvent.setup()
  render(<RailButton label="Plugins" disabled><span aria-hidden="true">P</span></RailButton>)
  const button = screen.getByRole('button', { name: 'Plugins' })
  await user.hover(button)
  expect(screen.getByRole('tooltip', { name: 'Plugins' })).toBeVisible()

  await user.keyboard('{Escape}')

  expect(screen.queryByRole('tooltip')).not.toBeInTheDocument()
})

it('keeps a focused tooltip open after pointer leave until the button blurs', async () => {
  const user = userEvent.setup()
  render(<><RailButton label="Home"><span aria-hidden="true">H</span></RailButton><button>Next</button></>)
  const button = screen.getByRole('button', { name: 'Home' })
  await user.tab()
  await user.hover(button)
  await user.unhover(button)

  expect(screen.getByRole('tooltip', { name: 'Home' })).toBeVisible()

  await user.tab()

  expect(screen.queryByRole('tooltip')).not.toBeInTheDocument()
})
