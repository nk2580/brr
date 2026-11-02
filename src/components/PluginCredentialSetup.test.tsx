import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { beforeEach, expect, it, vi } from 'vitest'
import { PluginCredentialSetup } from './PluginCredentialSetup'
import { writePluginMcpCredential } from '../lib/extensions'

vi.mock('../lib/extensions', () => ({ writePluginMcpCredential: vi.fn().mockResolvedValue(undefined) }))
beforeEach(() => vi.clearAllMocks())

it('shows the destination and stores a scoped API key without transport overrides', async () => {
  const saved = vi.fn()
  render(<PluginCredentialSetup pluginId="docs@test" serverName="docs" auth={{ type: 'api_key', headerScheme: 'bearer', serverUrl: 'https://docs.test/mcp' }} onSaved={saved}/>)
  expect(screen.getByText('https://docs.test/mcp')).toBeInTheDocument()
  fireEvent.change(screen.getByLabelText('API key for docs'), { target: { value: 'private-key' } })
  fireEvent.click(screen.getByRole('button', { name: 'Store credential' }))
  await waitFor(() => expect(writePluginMcpCredential).toHaveBeenCalledWith('docs@test', 'docs', { kind: 'token', token: 'private-key' }))
  await waitFor(() => expect(saved).toHaveBeenCalledOnce())
  expect(screen.getByLabelText('API key for docs')).toHaveValue('')
  expect(screen.queryByText('private-key')).not.toBeInTheDocument()
})

it('allows an explicitly empty Basic password, registered OAuth secrets and removal', async () => {
  const view = render(<PluginCredentialSetup pluginId="docs@test" serverName="docs" auth={{ type: 'api_key', headerScheme: 'basic', serverUrl: 'https://docs.test/mcp' }} onSaved={vi.fn()}/>)
  fireEvent.change(screen.getByLabelText('Username for docs'), { target: { value: 'key-as-username' } })
  fireEvent.click(screen.getByRole('button', { name: 'Store credential' }))
  await waitFor(() => expect(writePluginMcpCredential).toHaveBeenCalledWith('docs@test', 'docs', { kind: 'basic', username: 'key-as-username', password: '' }))
  view.unmount()
  render(<PluginCredentialSetup pluginId="docs@test" serverName="docs" auth={{ type: 'oauth', client: { mode: 'provided', tokenEndpointAuthMethod: 'client_secret_post' }, serverUrl: 'https://docs.test/mcp' }} onSaved={vi.fn()}/>)
  fireEvent.change(screen.getByLabelText('OAuth client secret for docs'), { target: { value: 'registered-secret' } })
  fireEvent.click(screen.getByRole('button', { name: 'Store credential' }))
  await waitFor(() => expect(writePluginMcpCredential).toHaveBeenCalledWith('docs@test', 'docs', { kind: 'oauth_client_secret', secret: 'registered-secret' }))
  await waitFor(() => expect(screen.getByRole('button', { name: 'Remove stored credential' })).toBeEnabled())
  fireEvent.click(screen.getByRole('button', { name: 'Remove stored credential' }))
  await waitFor(() => expect(writePluginMcpCredential).toHaveBeenCalledWith('docs@test', 'docs', null))
})

it.each(['oauth', 'mixed'] as const)('stores private_key_jwt keys from a bounded PEM file for %s clients without displaying the key text', async type => {
  const privateKey = '-----BEGIN PRIVATE KEY-----\nprivate-key-material\n-----END PRIVATE KEY-----'
  render(<PluginCredentialSetup pluginId="docs@test" serverName="docs" auth={{
    type, client: { mode: 'provided', tokenEndpointAuthMethod: 'private_key_jwt' }, serverUrl: 'https://docs.test/mcp',
  }} onSaved={vi.fn()}/>)

  fireEvent.change(screen.getByLabelText('Private key PEM for docs'), {
    target: { files: [new File([privateKey], 'signing-key.pem', { type: 'application/x-pem-file' })] },
  })
  await waitFor(() => expect(screen.getByText(/Selected file: signing-key.pem/)).toBeInTheDocument())
  fireEvent.change(screen.getByLabelText('Signing algorithm for docs'), { target: { value: 'PS512' } })
  fireEvent.change(screen.getByLabelText('Key ID for docs (optional)'), { target: { value: 'registered-key-7' } })
  fireEvent.click(screen.getByRole('button', { name: 'Store credential' }))

  await waitFor(() => expect(writePluginMcpCredential).toHaveBeenCalledWith('docs@test', 'docs', {
    kind: 'oauth_private_key_jwt', privateKey, algorithm: 'PS512', keyId: 'registered-key-7',
  }))
  expect(screen.getByText(/register the matching public key with the provider/i)).toBeInTheDocument()
  expect(screen.queryByText('private-key-material')).not.toBeInTheDocument()
})

it('rejects private key PEM files larger than 64 KB without storing their contents', () => {
  render(<PluginCredentialSetup pluginId="docs@test" serverName="docs" auth={{
    type: 'oauth', client: { mode: 'provided', tokenEndpointAuthMethod: 'private_key_jwt' }, serverUrl: 'https://docs.test/mcp',
  }} onSaved={vi.fn()}/>)
  fireEvent.change(screen.getByLabelText('Private key PEM for docs'), {
    target: { files: [new File(['x'.repeat(64 * 1024 + 1)], 'oversized.pem')] },
  })
  expect(screen.getByRole('alert')).toHaveTextContent(/no larger than 64 KB/i)
  expect(screen.getByRole('button', { name: 'Store credential' })).toBeDisabled()
  expect(writePluginMcpCredential).not.toHaveBeenCalled()
})
