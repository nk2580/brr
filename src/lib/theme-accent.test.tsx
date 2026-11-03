import { cleanup, render, screen } from '@testing-library/react'
import { afterEach, describe, expect, it } from 'vitest'
import { useThemeAccent } from './theme-accent'

function AccentProbe({ palette }: { palette: { accent: string; accentForeground: string } }) {
  useThemeAccent(palette)
  return <div data-testid="portalled-content" />
}

afterEach(() => {
  cleanup()
  document.documentElement.style.removeProperty('--accent')
  document.documentElement.style.removeProperty('--accent-foreground')
})

describe('theme accent', () => {
  it('sets accent tokens on the document root for portalled UI', () => {
    render(<AccentProbe palette={{ accent: 'oklch(76% .2 350)', accentForeground: 'oklch(18% .015 120)' }} />)

    expect(screen.getByTestId('portalled-content')).toBeInTheDocument()
    expect(document.documentElement.style.getPropertyValue('--accent')).toBe('oklch(76% .2 350)')
    expect(document.documentElement.style.getPropertyValue('--accent-foreground')).toBe('oklch(18% .015 120)')
  })

  it('updates palette tokens and restores prior inline values on unmount', () => {
    document.documentElement.style.setProperty('--accent', 'purple', 'important')
    document.documentElement.style.setProperty('--accent-foreground', 'white')
    const { rerender, unmount } = render(<AccentProbe palette={{ accent: 'blue', accentForeground: 'white' }} />)

    rerender(<AccentProbe palette={{ accent: 'pink', accentForeground: 'black' }} />)
    expect(document.documentElement.style.getPropertyValue('--accent')).toBe('pink')
    expect(document.documentElement.style.getPropertyValue('--accent-foreground')).toBe('black')

    unmount()
    expect(document.documentElement.style.getPropertyValue('--accent')).toBe('purple')
    expect(document.documentElement.style.getPropertyValue('--accent-foreground')).toBe('white')
  })
})
