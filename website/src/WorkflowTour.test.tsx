import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it } from 'vitest'
import { WorkflowTour } from './WorkflowTour'

describe('WorkflowTour', () => {
  it('embeds the actual app preview with a constrained sandbox and lazy loading', () => {
    render(<WorkflowTour />)

    const preview = screen.getByRole('region', { name: 'Desktop app preview' })
    const frame = screen.getByTitle('Brr app preview')
    expect(preview.contains(frame)).toBe(true)
    expect(preview.getAttribute('tabindex')).toBe('0')
    expect(frame.getAttribute('src')).toBe(`${import.meta.env.BASE_URL}app-preview.html`)
    expect(frame.getAttribute('loading')).toBe('lazy')
    expect(frame.getAttribute('sandbox')).toBe('allow-scripts allow-same-origin')
    expect(frame.hasAttribute('allow')).toBe(false)
    expect(frame.hasAttribute('allowfullscreen')).toBe(false)
  })

  it('labels the preview honestly and opens it in a separate tab', () => {
    render(<WorkflowTour />)

    expect(screen.getByText('Actual Brr interface · Sample workspace. No agent is running.')).toBeTruthy()
    const openPreview = screen.getByRole('link', { name: 'Open full preview' })
    expect(openPreview.getAttribute('href')).toBe(`${import.meta.env.BASE_URL}app-preview.html`)
    expect(openPreview.getAttribute('target')).toBe('_blank')
    expect(openPreview.getAttribute('rel')).toBe('noreferrer')
  })

  it('resets by remounting the same preview and has no synthetic tour controls', async () => {
    const user = userEvent.setup()
    render(<WorkflowTour />)
    const initialFrame = screen.getByTitle('Brr app preview')

    await user.click(screen.getByRole('button', { name: 'Reset preview' }))

    const resetFrame = screen.getByTitle('Brr app preview')
    expect(resetFrame).not.toBe(initialFrame)
    expect(resetFrame.getAttribute('src')).toBe(`${import.meta.env.BASE_URL}app-preview.html`)
    expect(screen.queryByRole('tablist')).toBeNull()
    expect(screen.queryByRole('button', { name: /Writing|Research|Making|Plan|Steer|Review|Resume/ })).toBeNull()
    expect(screen.queryByText(/Group the notes into a clear workshop flow/)).toBeNull()
  })
})
