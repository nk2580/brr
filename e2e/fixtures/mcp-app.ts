import { App } from '@modelcontextprotocol/ext-apps'

const app = new App({ name: 'Brr Playwright MCP App', version: '1.0.0' }, {})
const output = (id: string, value: unknown) => {
  const target = document.getElementById(id)
  if (target) target.textContent = JSON.stringify(value)
}
app.ontoolinput = ({ arguments: args }) => output('tool-input', args ?? {})
app.ontoolresult = result => output('tool-result', result)

document.getElementById('call-local')?.addEventListener('click', async () => {
  try { output('local-call', await app.callServerTool({ name: 'lookup', arguments: { query: 'same server' } })) }
  catch (error) { output('local-call', String(error)) }
})
document.getElementById('call-other')?.addEventListener('click', async () => {
  try { output('other-call', await app.callServerTool({ name: 'tool-from-other-server', arguments: { query: 'must not cross server' } })) }
  catch (error) { output('other-call', String(error)) }
})
document.getElementById('download-fixture')?.addEventListener('click', () => {
  const link = document.createElement('a')
  link.href = URL.createObjectURL(new Blob(['fixture download'], { type: 'text/plain' }))
  link.download = 'fixture.txt'
  link.click()
  URL.revokeObjectURL(link.href)
})

void app.connect().then(() => output('connect-status', 'connected')).catch(error => {
  const target = document.getElementById('app-error')
  if (target) target.textContent = String(error)
})
