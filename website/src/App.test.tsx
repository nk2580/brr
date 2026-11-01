import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import { App } from './App'

describe('Brr marketing page', () => {
  it('gives a broad audience a working path from possibilities to source and build instructions', () => {
    render(<App />)
    expect(screen.getByRole('heading', { level: 1, name: /Your work. Your way./ })).toBeInTheDocument()
    expect(screen.getByRole('link', { name: 'Explore Brr' })).toHaveAttribute('href', '#possibilities')
    expect(screen.getByRole('link', { name: 'Download source' })).toHaveAttribute('href', 'https://github.com/nk2580/brr/releases/download/v0.1.0/brr-source.tar.gz')
    expect(screen.getByRole('heading', { name: 'A harness you can make your own.' })).toBeInTheDocument()
  })

  it('copies the selected source-build commands and reports success', async () => {
    const user = userEvent.setup()
    const writeText = vi.spyOn(navigator.clipboard, 'writeText').mockResolvedValue()
    render(<App />)
    await user.click(screen.getByRole('tab', { name: 'Package the app' }))
    await user.click(screen.getByRole('button', { name: 'Copy commands' }))
    expect(writeText).toHaveBeenCalledWith('pnpm install\nAPPLE_SIGNING_IDENTITY=- pnpm desktop:build')
    expect(screen.getByRole('status', { name: 'Copy feedback' })).toHaveTextContent('Commands copied.')
    writeText.mockRestore()
  })

  it('keeps commands available for manual copying when clipboard access fails', async () => {
    const user = userEvent.setup()
    const writeText = vi.spyOn(navigator.clipboard, 'writeText').mockRejectedValue(new Error('denied'))
    render(<App />)
    await user.click(screen.getByRole('button', { name: 'Copy commands' }))
    expect(screen.getByRole('status', { name: 'Copy feedback' })).toHaveTextContent('Could not copy. Select the commands below and copy them manually.')
    expect(screen.getByTestId('build-commands')).toHaveTextContent('pnpm desktop:dev')
    writeText.mockRestore()
  })

  it('clears obsolete copy feedback when the build choice changes', async () => {
    const user = userEvent.setup()
    const writeText = vi.spyOn(navigator.clipboard, 'writeText').mockResolvedValue()
    render(<App />)
    await user.click(screen.getByRole('button', { name: 'Copy commands' }))
    await user.click(screen.getByRole('tab', { name: 'Run in development' }))
    expect(screen.getByRole('status', { name: 'Copy feedback' })).not.toHaveTextContent('Commands copied.')
    writeText.mockRestore()
  })
})
