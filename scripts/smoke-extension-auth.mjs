// Real owned-runtime smoke for portable plugin MCP credentials and OAuth.
// Everything runs in a fresh CODEX_HOME with fake credentials and a loopback provider.
import assert from 'node:assert/strict'
import {createHash, createPublicKey, verify as verifySignature} from 'node:crypto'
import {spawn} from 'node:child_process'
import {existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync} from 'node:fs'
import {tmpdir} from 'node:os'
import path from 'node:path'
import readline from 'node:readline'
import {createServer} from 'node:http'
import {fileURLToPath} from 'node:url'

const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const root = mkdtempSync(path.join(tmpdir(), 'brr-extension-auth-'))
const home = path.join(root, 'codex-home')
const market = path.join(root, 'marketplace')
const pluginName = 'extension-auth-fixture'
const executable = path.resolve(process.argv[2] || path.join(repo, 'src-tauri/binaries/brr-runtime'))
const apiToken = 'fixture-api-token-never-store'
const basicUser = 'fixture-basic-user'
const basicPassword = 'fixture-basic-password-never-store'
const oauthSecret = 'fixture-oauth-client-secret-never-store'
const jwtKeyId = 'brr-e2e-key'
const fixturePrivateKey = readFileSync(path.join(repo, 'runtime/codex/codex-rs/rmcp-client/tests/fixtures/brr-test-rsa.private.fixture'), 'utf8')
const fixturePublicKey = createPublicKey(readFileSync(path.join(repo, 'runtime/codex/codex-rs/rmcp-client/tests/fixtures/brr-test-rsa.public.fixture')))
const knownSecrets = [apiToken, basicUser, basicPassword, oauthSecret, fixturePrivateKey.trim(), Buffer.from(`${basicUser}:${basicPassword}`).toString('base64')]
const credentialsToRemove = new Set()
const oauthSessionTokens = new Set()
const pending = new Map()
const notifications = []
const oauth = new Map()
const installedServerNames = new Map()
const expectedApiAuth = new Map([
  ['api_bearer', {kind: 'bearer', value: `Bearer ${apiToken}`}],
  ['api_basic', {kind: 'basic', value: `Basic ${Buffer.from(`${basicUser}:${basicPassword}`).toString('base64')}`}],
  ['api_custom', {kind: 'custom', value: apiToken}],
])
const apiRequestDiagnostics = []

let runtime
let marketplacePath
let pluginId
let pluginInstalled = false
let requestId = 0
let stderr = ''
let provider
let providerPort
let oauthAccessCounter = 0
let providerFailure

const sleep = ms => new Promise(resolve => setTimeout(resolve, ms))
const json = (file, value) => {
  mkdirSync(path.dirname(file), {recursive: true})
  writeFileSync(file, JSON.stringify(value, null, 2))
}
const send = (response, status, value, headers = {}) => {
  response.writeHead(status, {'content-type': 'application/json', ...headers})
  response.end(JSON.stringify(value))
}
const safe = value => {
  let text = String(value)
  for (const secret of knownSecrets) if (secret) text = text.split(secret).join('[redacted fixture credential]')
  return text
}
const removeFixtureHome = async () => {
  for (let attempt = 0; ; attempt += 1) {
    try {
      rmSync(root, {recursive: true, force: true})
      return
    } catch (error) {
      if (error?.code !== 'ENOTEMPTY' || attempt >= 20) throw error
      await sleep(100)
    }
  }
}

const names = ['api_bearer', 'api_basic', 'api_custom', 'oauth_public', 'oauth_basic', 'oauth_post', 'oauth_jwt', 'oauth_dcr', 'oauth_dcr_empty', 'oauth_cimd']
const oauthNames = names.filter(name => name.startsWith('oauth_'))
for (const name of oauthNames) oauth.set(name, {
  method: name === 'oauth_basic' ? 'client_secret_basic' : name === 'oauth_post' ? 'client_secret_post' : name === 'oauth_jwt' ? 'private_key_jwt' : name === 'oauth_dcr' || name === 'oauth_dcr_empty' || name === 'oauth_cimd' ? 'dcr' : 'none',
  expectedClientId: name === 'oauth_dcr' || name === 'oauth_dcr_empty' || name === 'oauth_cimd' ? `registered-${name}` : `client-${name}`,
  secret: name === 'oauth_basic' || name === 'oauth_post' ? oauthSecret : undefined,
  registrations: 0,
  authorizationCodes: new Map(),
  accessTokens: new Map(),
  refreshTokens: new Map(),
  refreshCount: 0,
  grants: [],
  resource: `https://resource.example/${name}`,
  issuer: `http://127.0.0.1:__PORT__/oauth/${name}`,
})

const manifestAuth = name => {
  if (name === 'api_bearer') return {type: 'api_key', headerScheme: 'bearer'}
  if (name === 'api_basic') return {type: 'api_key', headerScheme: 'basic'}
  if (name === 'api_custom') return {type: 'api_key', headerScheme: 'custom_header', headerName: 'X-Fixture-Key'}
  const oauthEntry = oauth.get(name)
  const descriptor = {
    type: 'oauth',
    client: name === 'oauth_dcr' || name === 'oauth_dcr_empty' ? {mode: 'dcr'} : name === 'oauth_cimd' ? {mode: 'cimd'} : {
      mode: 'provided', clientId: oauthEntry.expectedClientId, tokenEndpointAuthMethod: oauthEntry.method,
    },
    authorizationServerBase: `http://127.0.0.1:${providerPort}/oauth/${name}`,
    authorizationUrl: `http://127.0.0.1:${providerPort}/oauth/${name}/authorize`,
    tokenUrl: `http://127.0.0.1:${providerPort}/oauth/${name}/token`,
    registrationUrl: `http://127.0.0.1:${providerPort}/oauth/${name}/register`,
    resource: oauthEntry.resource,
    baseScopes: name === 'oauth_dcr_empty' ? [] : [`plugin.${name}.base`],
    defaultScopes: name === 'oauth_dcr' || name === 'oauth_dcr_empty' || name === 'oauth_cimd' ? [] : [`plugin.${name}.default`],
    oidcEnabled: false,
  }
  return descriptor
}

function writeMarketplace() {
  const pluginRoot = path.join(market, 'plugins', pluginName)
  json(path.join(pluginRoot, 'plugin.json'), {
    $schema: 'https://agent-plugins.org/schemas/1.0.0/plugin.schema.json',
    name: pluginName,
    version: '1.0.0',
    mcpServers: './mcp.json',
  })
  json(path.join(pluginRoot, 'mcp.json'), {
    $schema: 'https://agent-plugins.org/schemas/1.0.0/mcp.schema.json',
    mcpServers: Object.fromEntries(names.map(name => [name, {
      type: 'streamable-http',
      url: `http://127.0.0.1:${providerPort}/mcp/${name}`,
      extensions: {'com.openai': {auth: manifestAuth(name)}},
    }])),
  })
  json(path.join(market, '.agents/plugins/marketplace.json'), {
    name: 'brr-auth-smoke',
    plugins: [{name: pluginName, source: {source: 'local', path: `./plugins/${pluginName}`}, policy: {installation: 'AVAILABLE', authentication: 'ON_USE'}}],
  })
  return pluginRoot
}

function appServerClient() {
  return new Promise((resolve, reject) => {
    runtime = spawn(executable, [], {cwd: home, env: runtimeEnv(), stdio: ['pipe', 'pipe', 'pipe']})
    runtime.stderr.on('data', data => { stderr = safe((stderr + data.toString()).slice(-4000)) })
    runtime.on('error', error => {
      for (const call of pending.values()) call.reject(error)
      reject(error)
    })
    runtime.on('exit', code => {
      const failure = Error(`Runtime exited ${code}${stderr ? `: ${safe(stderr)}` : ''}`)
      for (const call of pending.values()) call.reject(failure)
    })
    readline.createInterface({input: runtime.stdout}).on('line', line => {
      let message
      try { message = JSON.parse(line) } catch { return }
      if (message.method) {
        notifications.push(message)
        return
      }
      const call = pending.get(message.id)
      if (!call) return
      clearTimeout(call.timer)
      pending.delete(message.id)
      if (message.error) call.reject(Error(`${call.method}: ${safe(message.error.message)}`))
      else call.resolve(message.result)
    })
    resolve()
  })
}

function runtimeEnv() {
  const env = {...process.env, CODEX_HOME: home}
  for (const key of ['OPENAI_API_KEY', 'CODEX_API_KEY', 'CODEX_AUTH_JSON', 'CODEX_ACCESS_TOKEN', 'OPENAI_BASE_URL', 'CODEX_BASE_URL']) delete env[key]
  return env
}

function rpc(method, params = {}, timeoutMs = 60000) {
  return new Promise((resolve, reject) => {
    const id = ++requestId
    const timer = setTimeout(() => { pending.delete(id); reject(Error(`Timed out calling ${method}`)) }, timeoutMs)
    pending.set(id, {resolve, reject, timer, method})
    runtime.stdin.write(`${JSON.stringify({id, method, params})}\n`)
  })
}

async function waitNotification(method, predicate, timeoutMs = 30000) {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    const index = notifications.findIndex(item => item.method === method && predicate(item.params))
    if (index >= 0) return notifications.splice(index, 1)[0].params
    await sleep(25)
  }
  throw Error(`Timed out waiting for ${method}`)
}

function authenticateApiKey(name, request) {
  const expected = expectedApiAuth.get(name)
  if (!expected) return true
  if (expected.kind === 'custom') return request.headers['x-fixture-key'] === expected.value
  return request.headers.authorization === expected.value
}

function base64Url(bytes) {
  return Buffer.from(bytes).toString('base64url')
}

function verifyPrivateKeyAssertion(form, config, tokenUrl) {
  const assertion = form.get('client_assertion')
  const assertionType = form.get('client_assertion_type')
  assert.ok(assertion && assertionType === 'urn:ietf:params:oauth:client-assertion-type:jwt-bearer', 'private_key_jwt assertion is missing')
  const parts = assertion.split('.')
  assert.equal(parts.length, 3, 'private_key_jwt assertion must be a compact JWT')
  const header = JSON.parse(Buffer.from(parts[0], 'base64url').toString())
  const claims = JSON.parse(Buffer.from(parts[1], 'base64url').toString())
  assert.equal(header.alg, 'RS256', 'private_key_jwt algorithm mismatch')
  assert.equal(header.kid, jwtKeyId, 'private_key_jwt key ID mismatch')
  assert.equal(claims.iss, config.expectedClientId, 'private_key_jwt issuer mismatch')
  assert.equal(claims.sub, config.expectedClientId, 'private_key_jwt subject mismatch')
  assert.equal(claims.aud, tokenUrl, 'private_key_jwt audience mismatch')
  assert.ok(Number.isInteger(claims.exp) && claims.exp > Math.floor(Date.now() / 1000), 'private_key_jwt expiry is invalid')
  assert.ok(Number.isInteger(claims.iat) && claims.iat <= Math.floor(Date.now() / 1000), 'private_key_jwt issued-at is invalid')
  assert.ok(typeof claims.jti === 'string' && claims.jti.length > 8, 'private_key_jwt jti is missing')
  const signed = `${parts[0]}.${parts[1]}`
  assert.ok(verifySignature('RSA-SHA256', Buffer.from(signed), fixturePublicKey, Buffer.from(parts[2], 'base64url')), 'private_key_jwt signature verification failed')
  assert.equal(form.get('client_secret'), null, 'private_key_jwt must not send a client secret')
  assert.equal(form.get('client_id'), config.expectedClientId, 'private_key_jwt client_id does not match the registered client')
}

function verifyTokenClientAuth(name, form, request, config, tokenUrl) {
  if (config.method === 'none' || config.method === 'dcr') {
    assert.equal(request.headers.authorization, undefined, `${name} must not use HTTP client authentication`)
    assert.equal(form.get('client_id'), config.expectedClientId, `${name} client ID mismatch`)
    assert.equal(form.get('client_secret'), null, `${name} must not send client_secret`)
  } else if (config.method === 'client_secret_basic') {
    const expected = `Basic ${Buffer.from(`${config.expectedClientId}:${config.secret}`).toString('base64')}`
    assert.ok(request.headers.authorization === expected, `${name} client_secret_basic header mismatch`)
    assert.equal(form.get('client_secret'), null, `${name} leaked client_secret in the token form`)
    assert.equal(form.get('client_id'), config.expectedClientId, `${name} client ID mismatch`)
  } else if (config.method === 'client_secret_post') {
    assert.equal(request.headers.authorization, undefined, `${name} client_secret_post must not use Authorization`)
    assert.ok(form.get('client_secret') === config.secret, `${name} client_secret_post form value mismatch`)
    assert.equal(form.get('client_id'), config.expectedClientId, `${name} client ID mismatch`)
  } else if (config.method === 'private_key_jwt') {
    verifyPrivateKeyAssertion(form, config, tokenUrl)
  }
}

async function providerHandler(request, response) {
  const url = new URL(request.url, `http://127.0.0.1:${providerPort}`)
  const segments = url.pathname.split('/').filter(Boolean)
  const mcpName = segments[0] === 'mcp' ? segments[1] : null
  if (request.method === 'GET' && url.pathname.includes('/.well-known/oauth-authorization-server')) {
    const name = segments.at(-1)
    const config = oauth.get(name)
    if (!config) return send(response, 404, {error: 'not_found'})
    const issuer = `http://127.0.0.1:${providerPort}/oauth/${name}`
    config.issuer = issuer
    return send(response, 200, {
      issuer,
      authorization_endpoint: `http://127.0.0.1:${providerPort}/oauth/${name}/authorize`,
      token_endpoint: `http://127.0.0.1:${providerPort}/oauth/${name}/token`,
      registration_endpoint: `http://127.0.0.1:${providerPort}/oauth/${name}/register`,
      response_types_supported: ['code'],
      code_challenge_methods_supported: ['S256'],
      token_endpoint_auth_methods_supported: ['none', 'client_secret_basic', 'client_secret_post', 'private_key_jwt'],
    })
  }
  if (request.method === 'POST' && segments[0] === 'oauth' && segments[2] === 'register') {
    const name = segments[1]
    const config = oauth.get(name)
    if (!config) return send(response, 404, {error: 'not_found'})
    config.registrations += 1
    const body = await readBody(request)
    let registration
    try { registration = JSON.parse(body) } catch { return send(response, 400, {error: 'bad_registration'}) }
    assert.ok(Array.isArray(registration.redirect_uris) && registration.redirect_uris.length > 0, 'DCR redirect_uris missing')
    assert.ok(registration.redirect_uris.every(value => /^http:\/\/(127\.0\.0\.1|localhost):/.test(value)), 'DCR redirect must use loopback')
    return send(response, 201, {
      client_id: config.expectedClientId,
      token_endpoint_auth_method: 'none',
      redirect_uris: registration.redirect_uris,
      grant_types: ['authorization_code', 'refresh_token'],
      response_types: ['code'],
    })
  }
  if (request.method === 'GET' && segments[0] === 'oauth' && segments[2] === 'authorize') {
    const name = segments[1]
    const config = oauth.get(name)
    if (!config) return send(response, 404, {error: 'not_found'})
    const query = url.searchParams
    assert.equal(query.get('client_id'), config.expectedClientId, `${name} authorization client ID mismatch`)
    assert.equal(query.get('response_type'), 'code', `${name} must use authorization_code grant`)
    assert.equal(query.get('code_challenge_method'), 'S256', `${name} PKCE method mismatch`)
    const challenge = query.get('code_challenge')
    assert.ok(challenge && /^[A-Za-z0-9_-]{43}$/.test(challenge), `${name} PKCE challenge is invalid`)
    const expectedScopes = [...(name === 'oauth_dcr_empty' ? [] : [`plugin.${name}.base`]), ...(config.method === 'dcr' ? [] : [`plugin.${name}.default`])].sort()
    assert.deepEqual((query.get('scope') || '').split(/\s+/).filter(Boolean).sort(), expectedScopes, `${name} scopes differ from the declared base/default scopes`)
    const redirectUri = query.get('redirect_uri')
    const callback = new URL(redirectUri)
    assert.ok(callback.protocol === 'http:' && ['127.0.0.1', 'localhost', '::1'].includes(callback.hostname) && Number(callback.port) > 0, `${name} callback must be loopback HTTP`)
    const code = `fixture-code-${name}-${query.get('state')}`
    config.authorizationCodes.set(code, {state: query.get('state'), challenge, redirectUri, clientId: query.get('client_id')})
    const location = new URL(redirectUri)
    location.searchParams.set('code', code)
    location.searchParams.set('state', query.get('state'))
    location.searchParams.set('iss', config.issuer)
    response.writeHead(302, {location: location.toString()})
    response.end()
    return
  }
  if (request.method === 'POST' && segments[0] === 'oauth' && segments[2] === 'token') {
    const name = segments[1]
    const config = oauth.get(name)
    if (!config) return send(response, 404, {error: 'not_found'})
    const body = await readBody(request)
    const form = new URLSearchParams(body)
    const tokenUrl = `http://127.0.0.1:${providerPort}/oauth/${name}/token`
    verifyTokenClientAuth(name, form, request, config, tokenUrl)
    const resourceValues = form.getAll('resource')
    assert.equal(resourceValues.length, 1, `${name} must send exactly one resource`)
    assert.ok(resourceValues[0] === config.resource, `${name} resource indicator mismatch`)
    const grant = form.get('grant_type')
    config.grants.push(grant)
    if (grant === 'authorization_code') {
      const code = form.get('code')
      const pendingFlow = config.authorizationCodes.get(code)
      assert.ok(pendingFlow, `${name} received an unknown authorization code`)
      assert.equal(form.get('client_id'), pendingFlow.clientId, `${name} authorization code used a different client ID`)
      assert.equal(form.get('redirect_uri'), pendingFlow.redirectUri, `${name} callback URI binding mismatch`)
      const verifier = form.get('code_verifier')
      assert.ok(verifier && base64Url(createHash('sha256').update(verifier).digest()) === pendingFlow.challenge, `${name} PKCE verifier mismatch`)
      config.authorizationCodes.delete(code)
    } else if (grant === 'refresh_token') {
      const refreshClientId = config.refreshTokens.get(form.get('refresh_token'))
      if (!refreshClientId || refreshClientId !== form.get('client_id')) return send(response, 400, {error: 'invalid_grant'})
      config.refreshCount += 1
    } else {
      assert.fail(`${name} attempted unexpected OAuth grant ${grant}`)
    }
    const accessToken = `fixture-access-${name}-${++oauthAccessCounter}`
    const refreshToken = `fixture-refresh-${name}`
    oauthSessionTokens.add(accessToken)
    oauthSessionTokens.add(refreshToken)
    knownSecrets.push(accessToken, refreshToken)
    config.accessTokens.set(accessToken, form.get('client_id'))
    config.refreshTokens.set(refreshToken, form.get('client_id'))
    return send(response, 200, {access_token: accessToken, token_type: 'Bearer', expires_in: 1, refresh_token: refreshToken})
  }
  if (request.method === 'POST' && mcpName) {
    const apiAuth = expectedApiAuth.get(mcpName)
    if (apiAuth) {
      const authenticated = authenticateApiKey(mcpName, request)
      apiRequestDiagnostics.push({server: mcpName, authenticated, hasAuthorization: typeof request.headers.authorization === 'string', hasCustomHeader: typeof request.headers['x-fixture-key'] === 'string'})
      if (!authenticated) return send(response, 401, {
        error: 'missing_fixture_credential',
        server: mcpName,
        authorizationPresent: typeof request.headers.authorization === 'string',
        authorizationMatches: request.headers.authorization === apiAuth.value,
        authorizationScheme: request.headers.authorization?.split(/\s+/, 1)[0] || null,
        authorizationLength: request.headers.authorization?.length || 0,
        customHeaderPresent: typeof request.headers['x-fixture-key'] === 'string',
        customHeaderMatches: apiAuth.kind === 'custom' && request.headers['x-fixture-key'] === apiAuth.value,
      })
    }
    const oauthConfig = oauth.get(mcpName)
    const bearer = (request.headers.authorization || '').replace(/^Bearer\s+/i, '')
    if (oauthConfig && oauthConfig.accessTokens.get(bearer) !== oauthConfig.expectedClientId) {
      return send(response, 401, {error: 'invalid_token'}, {'www-authenticate': `Bearer resource_metadata="http://127.0.0.1:${providerPort}/.well-known/oauth-protected-resource/mcp/${mcpName}"`})
    }
    let message
    try { message = JSON.parse(await readBody(request)) } catch { return send(response, 400, {error: 'bad_json'}) }
    if (message.method === 'server/discover') {
      return send(response, 200, {jsonrpc: '2.0', id: message.id, result: {
        resultType: 'complete', supportedVersions: ['2026-07-28'],
        capabilities: {tools: {}, resources: {}},
        _meta: {'io.modelcontextprotocol/serverInfo': {name: `extension-auth-${mcpName}`, version: '1.0'}},
        ttlMs: 0, cacheScope: 'private',
      }})
    }
    if (message.method === 'initialize') return send(response, 200, {jsonrpc: '2.0', id: message.id, result: {protocolVersion: '2025-11-25', capabilities: {resources: {}, tools: {}}, serverInfo: {name: `extension-auth-${mcpName}`, version: '1.0'}}})
    if (message.method === 'notifications/initialized') { response.writeHead(202); response.end(); return }
    if (message.method === 'ping') return send(response, 200, {jsonrpc: '2.0', id: message.id, result: {}})
    if (message.method === 'tools/list') return send(response, 200, {jsonrpc: '2.0', id: message.id, result: {tools: [{name: 'fixture_echo', inputSchema: {type: 'object'}}]}})
    if (message.method === 'resources/list') return send(response, 200, {jsonrpc: '2.0', id: message.id, result: {resources: [{uri: `fixture://${mcpName}`, name: `fixture-${mcpName}`}]}})
    if (message.method === 'resources/read') return send(response, 200, {jsonrpc: '2.0', id: message.id, result: {contents: [{uri: message.params.uri, mimeType: 'text/plain', text: `resource verified ${mcpName}`}]}})
    if (message.method === 'tools/call') return send(response, 200, {jsonrpc: '2.0', id: message.id, result: {content: [{type: 'text', text: 'tool verified'}]}})
    return send(response, 400, {jsonrpc: '2.0', id: message.id, error: {code: -32601, message: 'Unsupported fixture method'}})
  }
  if (request.method === 'GET' && url.pathname.startsWith('/.well-known/oauth-protected-resource')) {
    const resourceName = url.pathname.split('/').at(-1)
    const config = oauth.get(resourceName)
    if (!config) return send(response, 404, {error: 'not_found'})
    return send(response, 200, {resource: config.resource, authorization_servers: [config.issuer]})
  }
  response.writeHead(404)
  response.end()
}

function readBody(request) {
  return new Promise((resolve, reject) => {
    let body = ''
    request.setEncoding('utf8')
    request.on('data', chunk => { body += chunk })
    request.on('end', () => resolve(body))
    request.on('error', reject)
  })
}

async function credentialWrite(serverName, credential) {
  credentialsToRemove.add(serverName)
  await rpc('plugin/mcpCredential/write', {pluginId, serverName, credential})
}

async function pluginDetail() {
  const result = await rpc('plugin/read', {marketplacePath: marketplacePath, pluginName})
  assert.ok(result.plugin?.summary?.installed, 'fixture plugin is not installed')
  return result.plugin
}

function authInfo(detail, name) {
  const entry = detail.mcpServerAuth?.find(item => item.serverName === name)
  assert.ok(entry, `missing auth metadata for ${name}`)
  return entry
}

function installedServerName(detail, localName) {
  const candidates = (detail.mcpServers || []).filter(name => name === localName || name.endsWith(`__${localName}`) || name.endsWith(`.${localName}`) || name.endsWith(`/${localName}`))
  if (candidates.length === 1) {
    installedServerNames.set(localName, candidates[0])
    return candidates[0]
  }
  const cached = installedServerNames.get(localName)
  assert.ok(cached, `could not uniquely resolve installed MCP server ${localName}`)
  return cached
}

async function readResource(name, threadId) {
  const response = await rpc('mcpServer/resource/read', {server: name, uri: 'fixture://message', ...(threadId ? {threadId} : {})})
  assert.ok(JSON.stringify(response).includes('resource verified'), `resource read failed for ${name}`)
}

async function resourceReadOutcome(name, threadId) {
  try {
    await readResource(name, threadId)
    return {revoked: false}
  } catch (error) {
    return {revoked: true, error: safe(error)}
  }
}

function assertNoFixtureCredentialsInHome() {
  const isTransientMarketplaceClone = candidate => {
    const relative = path.relative(home, candidate)
    const [parent, clone] = relative.split(path.sep)
    return parent === '.tmp' && clone?.startsWith('plugins-clone')
  }
  const visit = directory => {
    let entries
    try {
      entries = readdirSync(directory, {withFileTypes: true})
    } catch (error) {
      if (error?.code === 'ENOENT' && isTransientMarketplaceClone(directory)) return
      throw error
    }
    for (const entry of entries) {
      const full = path.join(directory, entry.name)
      if (entry.isSymbolicLink()) continue
      if (entry.isDirectory()) visit(full)
      else if (entry.isFile() && /\.(json|toml|yaml|yml|txt|md)$/.test(entry.name)) {
        let text
        try {
          text = readFileSync(full, 'utf8')
        } catch (error) {
          // Marketplace update clones are disposable and may be removed after
          // directory enumeration; every persistent CODEX_HOME file remains strict.
          if (error?.code === 'ENOENT' && isTransientMarketplaceClone(full)) continue
          throw error
        }
        // The SDK's dedicated OAuth token store may persist access/refresh tokens;
        // all configured plugin credentials and all ordinary config remain secret-free.
        const checkedSecrets = entry.name === '.credentials.json'
          ? knownSecrets.filter(secret => !oauthSessionTokens.has(secret))
          : knownSecrets
        for (const [index, secret] of checkedSecrets.entries()) {
          assert.ok(!text.includes(secret), `fixture credential ${index} leaked into CODEX_HOME file ${path.relative(home, full)}`)
        }
      }
    }
  }
  if (existsSync(home)) visit(home)
}

async function completeLogin(localName, detail, clientRegistration) {
  const serverName = installedServerName(detail, localName)
  const loginRequest = rpc('mcpServer/oauth/login', {name: serverName, timeoutSecs: 20, clientRegistration})
  const login = await loginRequest
  const authorizationUrl = new URL(login.authorizationUrl)
  assert.equal(authorizationUrl.hostname, '127.0.0.1', `${localName} authorization URL must be local`)
  const response = await fetch(authorizationUrl, {redirect: 'manual'})
  assert.equal(response.status, 302, `${localName} provider did not return a local callback redirect`)
  const callback = new URL(response.headers.get('location'))
  const redirectUri = new URL(authorizationUrl.searchParams.get('redirect_uri'))
  assert.ok(['127.0.0.1', 'localhost', '::1'].includes(callback.hostname), `${localName} callback redirect was not loopback`)
  assert.equal(callback.origin, redirectUri.origin, `${localName} callback origin mismatch`)
  assert.equal(callback.pathname, redirectUri.pathname, `${localName} callback path mismatch`)
  assert.ok(callback.searchParams.get('code'), `${localName} callback lacks code`)
  assert.equal(callback.searchParams.get('state'), authorizationUrl.searchParams.get('state'), `${localName} callback state mismatch`)
  assert.equal(callback.searchParams.get('iss'), oauth.get(localName).issuer, `${localName} callback issuer mismatch`)
  const callbackResponse = await fetch(callback, {redirect: 'manual'})
  assert.ok(callbackResponse.ok, `${localName} loopback callback failed`)
  const completed = await waitNotification('mcpServer/oauthLogin/completed', params => params.loginId === login.loginId)
  assert.equal(completed.success, true, `${localName} OAuth login failed${completed.error ? `: ${safe(completed.error)}` : ''}${providerFailure ? `; provider: ${safe(providerFailure)}` : ''}`)
  assert.equal(completed.loginId, login.loginId, `${localName} completion ID mismatch`)
  return serverName
}

async function main() {
  assert.ok(existsSync(executable), `owned runtime binary not found: ${executable}`)
  mkdirSync(home)
  writeFileSync(path.join(home, 'config.toml'), 'mcp_oauth_credentials_store = "file"\n[features]\nremote_plugin=false\nplugin_discovery_prefetch=false\n')

  provider = createServer((request, response) => {
    providerHandler(request, response).catch(error => {
      providerFailure ||= safe(error?.stack || error)
      if (!response.headersSent) response.writeHead(500)
      response.end()
    })
  })
  await new Promise(resolve => provider.listen(0, '127.0.0.1', resolve))
  providerPort = provider.address().port
  for (const [name, config] of oauth) config.issuer = `http://127.0.0.1:${providerPort}/oauth/${name}`
  marketplacePath = writeMarketplace()

  await appServerClient()
  await rpc('initialize', {clientInfo: {name: 'brr_extension_auth_smoke', version: '0.1.0'}, capabilities: {experimentalApi: true}})
  runtime.stdin.write('{"method":"initialized"}\n')
  await rpc('marketplace/add', {source: market})
  const catalog = await rpc('plugin/list', {marketplaceKinds: ['local']})
  const source = catalog.marketplaces.find(item => item.name === 'brr-auth-smoke')
  assert.ok(source, 'local auth smoke marketplace was not registered')
  marketplacePath = source.path
  await rpc('plugin/install', {marketplacePath: source.path, pluginName})
  pluginInstalled = true
  const detail = await pluginDetail()
  pluginId = detail.summary.id
  assert.ok(pluginId, 'installed plugin has no ID')

  const missingBearer = authInfo(detail, 'api_bearer')
  assert.equal(missingBearer.credentialConfigured, false)
  assert.ok(detail.mcpServerErrors?.some(error => error.serverName === 'api_bearer' && error.authDescriptor?.type === 'api_key' && /credential|API.key|configure/i.test(error.message)), 'missing API key lacks a structured setup diagnostic')
  for (const secret of knownSecrets) assert.ok(!JSON.stringify(detail.mcpServerErrors || []).includes(secret), 'auth setup diagnostic exposed a fixture credential')
  const publicAuth = authInfo(detail, 'oauth_public')
  assert.equal(publicAuth.credentialConfigured, false, 'public OAuth must not have a stored client credential')
  assert.equal(publicAuth.authDescriptor.client.clientId, oauth.get('oauth_public').expectedClientId, 'public OAuth diagnostic omitted its configured client ID')
  for (const name of ['oauth_dcr', 'oauth_dcr_empty', 'oauth_cimd']) assert.equal(authInfo(detail, name).credentialConfigured, false, `${name} must not have a stored client credential`)
  assert.ok(oauthNames.every(name => oauth.get(name).grants.length === 0), 'OAuth token request occurred before the fixture completed an OAuth login')
  assertNoFixtureCredentialsInHome()

  await credentialWrite('api_bearer', {kind: 'token', token: apiToken})
  await credentialWrite('api_basic', {kind: 'basic', username: basicUser, password: basicPassword})
  await credentialWrite('api_custom', {kind: 'token', token: apiToken})
  await credentialWrite('oauth_basic', {kind: 'oauth_client_secret', secret: oauthSecret})
  await credentialWrite('oauth_post', {kind: 'oauth_client_secret', secret: oauthSecret})
  await credentialWrite('oauth_jwt', {kind: 'oauth_private_key_jwt', privateKey: fixturePrivateKey, algorithm: 'RS256', keyId: jwtKeyId})
  for (const name of ['api_bearer', 'api_basic', 'api_custom']) {
    const auth = authInfo(await pluginDetail(), name)
    assert.equal(auth.credentialConfigured, true, `${name} credential presence was not reported (${JSON.stringify(auth)})`)
  }
  for (const name of ['oauth_basic', 'oauth_post', 'oauth_jwt']) {
    const auth = authInfo(await pluginDetail(), name)
    assert.equal(auth.credentialConfigured, true, `${name} credential presence was not reported (${JSON.stringify(auth)})`)
  }
  const configuredDetail = await pluginDetail()
  const configuredDetailJson = JSON.stringify(configuredDetail)
  for (const secret of knownSecrets) assert.ok(!configuredDetailJson.includes(secret), 'plugin detail exposed a fixture credential')
  const configRead = await rpc('config/read', {includeLayers: false})
  const configJson = JSON.stringify(configRead)
  for (const secret of knownSecrets) assert.ok(!configJson.includes(secret), 'config/read exposed a fixture credential')
  assertNoFixtureCredentialsInHome()

  await rpc('config/mcpServer/reload')
  const apiDetail = await pluginDetail()
  for (const localName of ['api_bearer', 'api_basic', 'api_custom']) {
    const serverName = installedServerName(apiDetail, localName)
    await readResource(serverName)
  }
  const fixtureThread = await rpc('thread/start', {cwd: home, modelProvider: 'openai', sandbox: 'read-only', approvalPolicy: 'on-request'})
  const bearerServerName = installedServerName(apiDetail, 'api_bearer')
  await readResource(bearerServerName, fixtureThread.thread.id)
  assert.ok(expectedApiAuth.size === 3, 'all API-key auth modes should be exercised')
  console.log('API-key phase passed: missing-key diagnostic, bearer/basic/custom-header access, and secret-free config/detail checks.')

  const oauthDetail = await pluginDetail()
  const publicServer = await completeLogin('oauth_public', oauthDetail)
  const basicServer = await completeLogin('oauth_basic', oauthDetail)
  const postServer = await completeLogin('oauth_post', oauthDetail)
  const jwtServer = await completeLogin('oauth_jwt', oauthDetail)
  const dcrServer = await completeLogin('oauth_dcr', oauthDetail)
  const emptyDcrServer = await completeLogin('oauth_dcr_empty', oauthDetail)
  const cimdServer = await completeLogin('oauth_cimd', oauthDetail)
  assert.ok(oauth.get('oauth_dcr').registrations >= 1, 'DCR mode did not register a client')
  assert.ok(oauth.get('oauth_dcr_empty').registrations >= 1, 'empty-scope DCR mode did not register a client')
  assert.ok(oauth.get('oauth_cimd').registrations >= 1, 'CIMD preference did not fall back to DCR')
  for (const name of ['oauth_dcr', 'oauth_cimd']) assert.ok(!oauth.get(name).expectedClientId.includes('chatgpt.com') && !oauth.get(name).expectedClientId.includes('brr'), 'fallback must not use Brr metadata identity')

  await sleep(1250)
  for (const serverName of [publicServer, basicServer, postServer, jwtServer, dcrServer, emptyDcrServer, cimdServer]) await readResource(serverName)
  for (const name of oauthNames) {
    const config = oauth.get(name)
    assert.ok(config.grants.includes('authorization_code'), `${name} never exchanged an authorization code`)
    assert.ok(config.grants.includes('refresh_token'), `${name} never exercised refresh`)
    assert.ok(config.grants.every(grant => grant === 'authorization_code' || grant === 'refresh_token'), `${name} used an alternate grant`)
  }
  assert.equal(providerFailure, undefined, `fake provider rejected a request: ${safe(providerFailure)}`)
  const postOAuthDetail = await pluginDetail()
  const postOAuthConfig = await rpc('config/read', {includeLayers: false})
  for (const secret of knownSecrets) {
    assert.ok(!JSON.stringify(postOAuthDetail).includes(secret), 'plugin detail exposed an OAuth fixture credential')
    assert.ok(!JSON.stringify(postOAuthConfig).includes(secret), 'config/read exposed an OAuth fixture credential')
  }
  console.log('OAuth phase passed: PKCE, explicit client auth modes, DCR/CIMD fallback, resource/scopes, local callbacks, issuer binding, and refresh.')
  console.log(`DCR registration counts: ${JSON.stringify(Object.fromEntries(['oauth_dcr', 'oauth_dcr_empty', 'oauth_cimd'].map(name => [name, oauth.get(name).registrations])))}`)

  assertNoFixtureCredentialsInHome()
  for (const name of credentialsToRemove) {
    await credentialWrite(name, null)
    const afterRemoval = await pluginDetail()
    assert.equal(authInfo(afterRemoval, name).credentialConfigured, false, `${name} credential remained configured after removal`)
    if (name === 'api_bearer') {
      const serverName = installedServerName(afterRemoval, name)
      const threadlessRead = await resourceReadOutcome(serverName)
      const threadScopedRead = await resourceReadOutcome(serverName, fixtureThread.thread.id)
      assert.ok(threadlessRead.revoked, 'threadless API-key authorization remained usable after credential removal')
      assert.ok(threadScopedRead.revoked, 'thread-scoped API-key authorization remained usable after credential removal')
      assert.match(threadlessRead.error, /401|credential|auth|unavailable|failed|unknown MCP server/i, `threadless read failed for an unexpected reason: ${threadlessRead.error}`)
      assert.match(threadScopedRead.error, /401|credential|auth|unavailable|failed|unknown MCP server/i, `thread-scoped read failed for an unexpected reason: ${threadScopedRead.error}`)
    }
  }
  credentialsToRemove.clear()
  await rpc('config/mcpServer/reload')
  const removed = await pluginDetail()
  assert.ok(removed.mcpServerErrors?.some(error => error.serverName === 'api_bearer' && /credential|API.key|configure/i.test(error.message)), 'removing the API key did not restore setup diagnostic')
  assertNoFixtureCredentialsInHome()

  await rpc('plugin/uninstall', {pluginId})
  pluginInstalled = false
  console.log('Credential removal phase passed: presence is false before uninstall and missing-key setup diagnostics return.')
}

try {
  await main()
} catch (error) {
  process.exitCode = 1
  console.error(`Extension auth smoke failed: ${safe(error?.stack || error)}`)
} finally {
  for (const call of pending.values()) clearTimeout(call.timer)
  if (runtime && runtime.exitCode === null) {
    // Best-effort removal occurs before uninstall so each keyring item is bound to
    // an installed temporary package root. This does not touch other bindings.
    if (pluginId && pluginInstalled) {
      for (const serverName of credentialsToRemove) {
        try { await rpc('plugin/mcpCredential/write', {pluginId, serverName, credential: null}, 5000) } catch {}
      }
      try { await rpc('plugin/uninstall', {pluginId}, 5000) } catch {}
      pluginInstalled = false
    }
    const closed = new Promise(resolve => runtime.once('close', resolve))
    runtime.stdin.end()
    const killer = setTimeout(() => runtime.kill('SIGKILL'), 5000)
    await closed
    clearTimeout(killer)
  }
  if (provider?.listening) await new Promise(resolve => provider.close(resolve))
  await removeFixtureHome()
}
