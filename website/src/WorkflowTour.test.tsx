import { fireEvent, render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it } from 'vitest'
import { WorkflowTour } from './WorkflowTour'

describe('WorkflowTour', () => {
  it('switches between local example stories and resets to the plan', async () => {
    const user = userEvent.setup()
    render(<WorkflowTour />)

    expect(screen.getByText('Turn my notes into a friendly workshop outline. Show me a plan first.')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Research' }))
    expect(screen.getByRole('button', { name: 'Research' })).toHaveAttribute('aria-pressed', 'true')
    expect(screen.getByText(/Compare two local project notes/)).toBeInTheDocument()

    await user.click(screen.getByRole('tab', { name: 'Review' }))
    expect(screen.getByRole('tab', { name: 'Review' })).toHaveAttribute('aria-selected', 'true')
    await user.click(screen.getByRole('button', { name: 'Making' }))
    expect(screen.getByRole('tab', { name: 'Plan' })).toHaveAttribute('aria-selected', 'true')
    expect(screen.getByRole('button', { name: 'Making' })).toHaveAttribute('aria-pressed', 'true')
  })

  it('moves tab selection and focus with the tab-list keyboard controls', () => {
    render(<WorkflowTour />)
    const planTab = screen.getByRole('tab', { name: 'Plan' })
    planTab.focus()
    fireEvent.keyDown(planTab, { key: 'ArrowRight' })
    expect(screen.getByRole('tab', { name: 'Steer' })).toHaveFocus()
    expect(screen.getByRole('tab', { name: 'Steer' })).toHaveAttribute('aria-selected', 'true')

    fireEvent.keyDown(screen.getByRole('tab', { name: 'Steer' }), { key: 'End' })
    expect(screen.getByRole('tab', { name: 'Resume' })).toHaveFocus()
    fireEvent.keyDown(screen.getByRole('tab', { name: 'Resume' }), { key: 'Home' })
    expect(planTab).toHaveFocus()
    fireEvent.keyDown(planTab, { key: 'ArrowLeft' })
    expect(screen.getByRole('tab', { name: 'Resume' })).toHaveFocus()
  })

  it('returns the transcript panel to the top when the stage or story changes', async () => {
    const user = userEvent.setup()
    render(<WorkflowTour />)
    await user.click(screen.getByRole('button', { name: 'Implement plan' }))

    const panel = screen.getByRole('tabpanel')
    const instruction = screen.getByLabelText('Steering instruction')
    await user.type(instruction, 'Keep the draft while I review the plan.')
    panel.scrollTop = 420
    await user.click(screen.getByRole('tab', { name: 'Review' }))
    expect(panel.scrollTop).toBe(0)

    await user.click(screen.getByRole('tab', { name: 'Steer' }))
    expect(screen.getByLabelText('Steering instruction')).toHaveValue('Keep the draft while I review the plan.')
    await user.click(screen.getByRole('button', { name: 'Stop demo' }))
    expect(screen.getByRole('status')).toHaveTextContent('Demo stopped. Your instruction is still here.')
    await user.click(screen.getByRole('tab', { name: 'Review' }))
    await user.click(screen.getByRole('tab', { name: 'Steer' }))
    expect(screen.getByRole('button', { name: 'Continue demo' })).toBeInTheDocument()
    expect(screen.getByLabelText('Steering instruction')).toHaveValue('Keep the draft while I review the plan.')

    panel.scrollTop = 280
    await user.click(screen.getByRole('button', { name: 'Making' }))
    expect(panel.scrollTop).toBe(0)
  })

  it('hands the proposed plan to the next stage only when requested', async () => {
    const user = userEvent.setup()
    render(<WorkflowTour />)
    expect(screen.getAllByRole('listitem')[0]).toHaveTextContent('Group the notes into a clear workshop flow')
    expect(screen.getByRole('tab', { name: 'Steer' })).toHaveAttribute('aria-selected', 'false')

    await user.click(screen.getByRole('button', { name: 'Implement plan' }))
    expect(screen.getByRole('tab', { name: 'Steer' })).toHaveAttribute('aria-selected', 'true')
    expect(screen.getByLabelText('Steering instruction')).toBeInTheDocument()
  })

  it('sends exact steering text and keeps stopping separate from instruction sending', async () => {
    const user = userEvent.setup()
    render(<WorkflowTour />)
    await user.click(screen.getByRole('button', { name: 'Implement plan' }))
    const input = screen.getByLabelText('Steering instruction')
    await user.type(input, 'Keep the opening warm and concise.')
    await user.click(screen.getByRole('button', { name: 'Send instruction' }))
    expect(screen.getByText('Keep the opening warm and concise.')).toBeInTheDocument()
    expect(screen.getByRole('tab', { name: 'Steer' })).toHaveAttribute('aria-selected', 'true')
    expect(screen.getByRole('button', { name: 'Stop demo' })).toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: 'Stop demo' }))
    expect(screen.getByRole('status')).toHaveTextContent('Demo stopped. Your instruction is still here.')
    expect(screen.getByText('Keep the opening warm and concise.')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Continue demo' }))
    expect(screen.getByRole('status')).toHaveTextContent('Demo continued.')
    expect(screen.getByText('Keep the opening warm and concise.')).toBeInTheDocument()
  })

  it('shows review changes and returns to a reset plan', async () => {
    const user = userEvent.setup()
    render(<WorkflowTour />)
    await user.click(screen.getByRole('tab', { name: 'Review' }))
    const diff = screen.getByLabelText('Illustrative changes')
    expect(diff.tagName).toBe('PRE')
    expect(diff).toHaveTextContent(/^-\s+Workshop notes/m)
    expect(diff.querySelector('.tour-diff-added')).toHaveTextContent('+ Friendly workshop outline')
    expect(screen.getByText(/Read through the changes and keep or change whatever you like/)).toBeInTheDocument()

    await user.click(screen.getByRole('tab', { name: 'Resume' }))
    expect(screen.getByText(/Example transcript from an earlier session/i)).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Return to plan' }))
    expect(screen.getByRole('tab', { name: 'Plan' })).toHaveAttribute('aria-selected', 'true')
    expect(screen.getAllByRole('listitem')[0]).toHaveTextContent('Group the notes into a clear workshop flow')
    expect(screen.queryByLabelText('Steering instruction')).not.toBeInTheDocument()
  })

  it('shows a command summary for a making review only', async () => {
    const user = userEvent.setup()
    render(<WorkflowTour />)
    await user.click(screen.getByRole('button', { name: 'Making' }))
    await user.click(screen.getByRole('tab', { name: 'Review' }))
    expect(screen.getByText(/Example command: pnpm test/)).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Writing' }))
    expect(screen.queryByText(/Example command:/)).not.toBeInTheDocument()
  })
})
