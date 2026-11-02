import { fireEvent, render, screen } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'
import { AppearanceSettings } from './AppearanceSettings'

describe('AppearanceSettings', () => {
  it('shows accessible radio choices, marks graphite as default, and changes immediately', () => {
    const onChange = vi.fn()
    render(<AppearanceSettings value="graphite" onChange={onChange} onClose={vi.fn()} />)

    expect(screen.getByRole('region', { name: 'Application settings' })).toBeInTheDocument()
    expect(screen.getByRole('heading', { name: 'Appearance' })).toBeInTheDocument()
    expect(screen.getByRole('group', { name: 'Rail color' })).toBeInTheDocument()
    const radios = screen.getAllByRole('radio')
    expect(radios).toHaveLength(9)
    expect(radios.map(radio => radio.getAttribute('aria-label'))).toEqual(['Graphite', 'Lime', 'Yellow', 'Orange', 'Red', 'Cyan', 'Blue', 'Violet', 'Pink'])
    expect(screen.getByText('Graphite')).toBeInTheDocument()
    expect(screen.getByText('default')).toBeInTheDocument()
    fireEvent.click(screen.getByRole('radio', { name: 'Violet' }))
    expect(onChange).toHaveBeenCalledWith('violet')
  })

  it('closes and announces when the preference could not be saved', () => {
    const onClose = vi.fn()
    render(<AppearanceSettings value="lime" onChange={vi.fn()} onClose={onClose} saveFailed />)
    fireEvent.click(screen.getByRole('button', { name: 'Close settings' }))
    expect(onClose).toHaveBeenCalledOnce()
    expect(screen.getByRole('status')).toHaveTextContent('Color changed for this session. Could not save your preference.')
  })
})
