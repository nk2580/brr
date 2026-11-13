import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import { MobileApp } from './App'
import { RemoteClient, type NativeBridge, type Profile, type Snapshot } from '@brr/remote-client'

describe('mobile shell', () => {
  it('shows the four task tabs and starts at the pairing path', async () => {
    render(<MobileApp />)
    expect(screen.getByRole('heading', { name: 'Pair your Brr node' })).toBeInTheDocument()
    expect(screen.getByRole('tab', { name: /Chats/ })).toBeInTheDocument()
    expect(screen.getByRole('tab', { name: /Agents/ })).toBeInTheDocument()
    expect(screen.getByRole('tab', { name: /Attention/ })).toBeInTheDocument()
    expect(screen.getByRole('tab', { name: /Fleet/ })).toBeInTheDocument()
    expect(screen.getByLabelText('Pairing link or invitation')).toBeInTheDocument()
  })

  it('pairs through the native bridge and enters the connected chat surface', async () => {
    const profile: Profile = { id: 'phone-1', name: 'Primary', endpoint: 'https://node.local', fingerprint: 'a'.repeat(64), nodeId: 'node-1' }
    const snapshot: Snapshot = { protocolVersion: 1, nodeId: 'node-1', epoch: 'runtime-1', cursor: { epoch: 'runtime-1', sequence: 0 }, nodes: [{ nodeId: 'node-1', name: 'Primary', primary: true, online: true }], account: null, models: [], threads: [], projects: [], agents: [], tasks: [], questions: [], approvals: [], sync: {}, errors: [] }
    const invoke = vi.fn(async <T,>(command: string) => {
      const result = command === 'remote_profiles' ? []
        : command === 'remote_pair' ? profile
          : command === 'remote_snapshot' ? snapshot
            : command === 'remote_cache_get' ? null : undefined
      return result as T
    })
    const listen = vi.fn(async () => () => undefined)
    const client = new RemoteClient({ invoke, listen } as NativeBridge)
    const user = userEvent.setup()
    render(<MobileApp client={client} native />)
    fireEvent.change(screen.getByRole('textbox', { name: 'Pairing link or invitation' }), { target: { value: JSON.stringify({ endpoint: 'https://node.local', code: 'once', fingerprint: 'a'.repeat(64), certificatePem: '-----BEGIN CERTIFICATE-----\nZmFrZQ==\n-----END CERTIFICATE-----' }) } })
    await user.click(screen.getByRole('button', { name: 'Pair this phone' }))
    expect(await screen.findByRole('heading', { name: 'Chats' })).toBeInTheDocument()
    expect(invoke).toHaveBeenCalledWith('remote_pair', expect.objectContaining({ name: undefined, invitation: expect.objectContaining({ code: 'once', endpoint: 'https://node.local' }) }))
    expect(screen.getByText('Connected')).toBeInTheDocument()
  })

  it('shows the native string error when pairing fails', async () => {
    const invoke = vi.fn(async <T,>(command: string) => {
      if (command === 'remote_profiles') return [] as T
      if (command === 'remote_pair') throw 'The paired server certificate did not match the invitation.'
      return null as T
    })
    const client = new RemoteClient({ invoke, listen: vi.fn(async () => () => undefined) } as NativeBridge)
    const user = userEvent.setup()
    render(<MobileApp client={client} native />)
    fireEvent.change(screen.getByRole('textbox', { name: 'Pairing link or invitation' }), { target: { value: JSON.stringify({
      endpoint: 'https://node.local',
      code: 'once',
      fingerprint: 'a'.repeat(64),
      certificatePem: '-----BEGIN CERTIFICATE-----\nZmFrZQ==\n-----END CERTIFICATE-----',
    }) } })
    await user.click(screen.getByRole('button', { name: 'Pair this phone' }))
    expect(await screen.findByText(/server certificate did not match/)).toBeInTheDocument()
  })

  it('scans a QR invitation, validates it, and waits for the user to confirm pairing', async () => {
    const profile: Profile = { id: 'phone-qr', name: 'Primary', endpoint: 'https://node.local', fingerprint: 'a'.repeat(64), nodeId: 'node-1' }
    const snapshot: Snapshot = { protocolVersion: 1, nodeId: 'node-1', epoch: 'runtime-1', cursor: { epoch: 'runtime-1', sequence: 0 }, nodes: [], account: null, models: [], threads: [], projects: [], agents: [], tasks: [], questions: [], approvals: [], sync: {}, errors: [] }
    const invitation = JSON.stringify({ endpoint: 'https://node.local', code: 'qr-once', fingerprint: 'a'.repeat(64), certificatePem: '-----BEGIN CERTIFICATE-----\nZmFrZQ==\n-----END CERTIFICATE-----' })
    const invoke = vi.fn(async <T,>(command: string) => (command === 'remote_profiles' ? [] : command === 'remote_pair' ? profile : command === 'remote_snapshot' ? snapshot : command === 'remote_cache_get' ? null : undefined) as T)
    const client = new RemoteClient({ invoke, listen: vi.fn(async () => () => undefined) } as NativeBridge)
    const scanner = { scan: vi.fn(async () => ({ content: invitation })), cancel: vi.fn(async () => undefined) }
    const user = userEvent.setup()
    render(<MobileApp client={client} native scanner={scanner} />)

    await user.click(screen.getByRole('button', { name: 'Scan QR code' }))
    expect(await screen.findByText(/QR invitation scanned/)).toBeInTheDocument()
    expect(screen.getByRole('textbox', { name: 'Pairing link or invitation' })).toHaveValue(invitation)
    expect(invoke).not.toHaveBeenCalledWith('remote_pair', expect.anything())
    await user.click(screen.getByRole('button', { name: 'Pair this phone' }))
    expect(await screen.findByRole('heading', { name: 'Chats' })).toBeInTheDocument()
    expect(invoke).toHaveBeenCalledWith('remote_pair', expect.objectContaining({ invitation: expect.objectContaining({ code: 'qr-once' }) }))
  })

  it('cancels QR scanning without pairing or leaving the pairing screen stuck', async () => {
    let rejectScan: (reason: Error) => void = () => undefined
    const scanner = {
      scan: vi.fn(() => new Promise<{ content: string }>((_resolve, reject) => { rejectScan = reject })),
      cancel: vi.fn(async () => { rejectScan(new Error('scan cancelled')) }),
    }
    const invoke = vi.fn(async <T,>(command: string) => (command === 'remote_profiles' ? [] : undefined) as T)
    const client = new RemoteClient({ invoke, listen: vi.fn(async () => () => undefined) } as NativeBridge)
    const user = userEvent.setup()
    render(<MobileApp client={client} native scanner={scanner} />)

    await user.click(screen.getByRole('button', { name: 'Scan QR code' }))
    await user.click(await screen.findByRole('button', { name: 'Cancel scan' }))
    await waitFor(() => expect(screen.getByRole('button', { name: 'Scan QR code' })).toBeEnabled())
    expect(scanner.cancel).toHaveBeenCalledOnce()
    expect(invoke).not.toHaveBeenCalledWith('remote_pair', expect.anything())
  })

  it('rejects malformed scanned data and keeps paste pairing available', async () => {
    const scanner = { scan: vi.fn(async () => ({ content: '{"endpoint":"http://untrusted.local"}' })), cancel: vi.fn(async () => undefined) }
    const client = new RemoteClient({ invoke: vi.fn(async <T,>(command: string) => (command === 'remote_profiles' ? [] : undefined) as T), listen: vi.fn(async () => () => undefined) } as NativeBridge)
    const user = userEvent.setup()
    render(<MobileApp client={client} native scanner={scanner} />)
    await user.click(screen.getByRole('button', { name: 'Scan QR code' }))
    expect(await screen.findByText(/Could not scan that QR code/)).toBeInTheDocument()
    expect(screen.getByRole('textbox', { name: 'Pairing link or invitation' })).toHaveValue('')
    expect(screen.getByRole('button', { name: 'Pair this phone' })).toBeDisabled()
    expect(screen.getByRole('button', { name: 'Scan QR code' })).toBeEnabled()
  })
})
