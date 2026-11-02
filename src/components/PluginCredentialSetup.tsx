import { useId, useRef, useState } from 'react'
import { writePluginMcpCredential, type PluginMcpCredential } from '../lib/extensions'
import { Button } from './ui/button'

type Props = { pluginId: string; serverName: string; auth: Record<string, any>; credentialConfigured?: boolean; onSaved: () => unknown }

export function PluginCredentialSetup({ pluginId, serverName, auth, credentialConfigured = false, onSaved }: Props) {
  const id = useId()
  const privateKeyInput = useRef<HTMLInputElement>(null)
  const [secret, setSecret] = useState('')
  const [username, setUsername] = useState('')
  const [privateKey, setPrivateKey] = useState('')
  const [keyFileName, setKeyFileName] = useState('')
  const [keyId, setKeyId] = useState('')
  const [algorithm, setAlgorithm] = useState('RS256')
  const [keyLoading, setKeyLoading] = useState(false)
  const [pending, setPending] = useState(false)
  const [error, setError] = useState('')
  const [message, setMessage] = useState('')
  const basic = auth.type === 'api_key' && auth.headerScheme === 'basic'
  const oauth = ['oauth', 'mixed'].includes(auth.type) && auth.client?.mode === 'provided' && ['client_secret_basic', 'client_secret_post'].includes(auth.client?.tokenEndpointAuthMethod)
  const privateJwt = ['oauth', 'mixed'].includes(auth.type) && auth.client?.mode === 'provided' && auth.client?.tokenEndpointAuthMethod === 'private_key_jwt'
  const supported = basic || oauth || privateJwt || (auth.type === 'api_key' && ['bearer', 'custom_header'].includes(auth.headerScheme))
  if (!supported || typeof auth.serverUrl !== 'string') return null
  const secretLabel = oauth ? `OAuth client secret for ${serverName}` : basic ? `Password for ${serverName}` : `API key for ${serverName}`
  const canStore = privateJwt ? Boolean(privateKey) && !keyLoading : basic ? username.length > 0 : secret.trim().length > 0
  const selectPrivateKey = async (file?: File) => {
    setPrivateKey(''); setKeyFileName(''); setError('')
    if (!file) return
    if (file.size > 64 * 1024) { setError('Private key PEM files must be no larger than 64 KB.'); return }
    setKeyLoading(true)
    try {
      const value = await new Promise<string>((resolve, reject) => {
        const reader = new FileReader()
        reader.onload = () => typeof reader.result === 'string' ? resolve(reader.result) : reject(new Error('Unable to read the private key file.'))
        reader.onerror = () => reject(new Error('Unable to read the private key file.'))
        reader.readAsText(file)
      })
      if (!/-----BEGIN (?:[A-Z0-9 ]+ )?PRIVATE KEY-----/.test(value) || !/-----END (?:[A-Z0-9 ]+ )?PRIVATE KEY-----/.test(value)) {
        throw new Error('Choose a PEM file containing a private key.')
      }
      setPrivateKey(value); setKeyFileName(file.name)
    } catch (reason) { setError(reason instanceof Error ? reason.message : 'Unable to read the private key file.') }
    finally { setKeyLoading(false) }
  }
  const update = async (credential: PluginMcpCredential | null) => {
    setPending(true); setError(''); setMessage('')
    try {
      await writePluginMcpCredential(pluginId, serverName, credential)
      setSecret(''); setUsername(''); setPrivateKey(''); setKeyFileName('')
      if (privateKeyInput.current) privateKeyInput.current.value = ''
      await onSaved()
      setMessage(credential ? 'Credential stored in the system credential store.' : 'Stored credential removed.')
    } catch (reason) { setError(reason instanceof Error ? reason.message : String(reason)) }
    finally { setPending(false) }
  }
  return <form className="plugin-credential-setup" aria-label={`Authentication setup for ${serverName}`} onSubmit={event => {
    event.preventDefault()
    if (!pending && canStore) {
      void update(privateJwt
        ? { kind: 'oauth_private_key_jwt', privateKey, algorithm, ...(keyId.trim() ? { keyId: keyId.trim() } : {}) }
        : oauth ? { kind: 'oauth_client_secret', secret }
          : basic ? { kind: 'basic', username, password: secret }
            : { kind: 'token', token: secret })
    }
  }}>
    <p>Connect {serverName} · {pluginId}</p><p className="project-path">{auth.serverUrl}</p>
    {auth.headerName && <p>Credential header: {auth.headerName}</p>}
    {credentialConfigured && <p role="note">A credential is stored in the system credential store. You can replace or remove it here.</p>}
    {privateJwt && <>
      <p>Choose the signing algorithm configured for this OAuth client. Register the matching public key with the provider. The private key is sent to the system credential store and is never shown here.</p>
      <label htmlFor={`${id}-private-key`}>Private key PEM for {serverName}<input ref={privateKeyInput} id={`${id}-private-key`} type="file" accept=".pem,application/x-pem-file" disabled={pending || keyLoading} onChange={event => void selectPrivateKey(event.currentTarget.files?.[0])}/></label>
      {keyFileName && <p role="status">Selected file: {keyFileName}</p>}
      <label htmlFor={`${id}-algorithm`}>Signing algorithm for {serverName}<select id={`${id}-algorithm`} value={algorithm} disabled={pending} onChange={event => setAlgorithm(event.target.value)}>
        {['RS256', 'RS384', 'RS512', 'PS256', 'PS384', 'PS512', 'ES256', 'ES384', 'EdDSA'].map(value => <option key={value} value={value}>{value}</option>)}
      </select></label>
      <label htmlFor={`${id}-key-id`}>Key ID for {serverName} (optional)<input id={`${id}-key-id`} autoComplete="off" value={keyId} disabled={pending} onChange={event => setKeyId(event.target.value)}/></label>
    </>}
    {basic && <label htmlFor={`${id}-username`}>Username for {serverName}<input id={`${id}-username`} autoComplete="off" value={username} onChange={event => setUsername(event.target.value)}/></label>}
    {!privateJwt && <label htmlFor={`${id}-secret`}>{secretLabel}<input id={`${id}-secret`} type="password" autoComplete="new-password" value={secret} onChange={event => setSecret(event.target.value)}/></label>}
    <div className="extensions-row"><Button type="submit" disabled={pending || keyLoading || !canStore}>Store credential</Button><Button type="button" variant="ghost" disabled={pending} onClick={() => void update(null)}>Remove stored credential</Button></div>
    {error && <p role="alert">{error}</p>}{message && <p role="status">{message}</p>}
  </form>
}
