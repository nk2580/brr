import { act, render, screen, waitFor } from '@testing-library/react'
import { fireEvent } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { expect, it, vi } from 'vitest'
import { McpRequests } from './McpRequests'

const backend = vi.hoisted(() => ({
  handler: null as null | ((event: any) => void),
  respond: vi.fn(async () => {}),
}))
vi.mock('../lib/client', () => ({ client: {
  subscribe: async (handler: any) => { backend.handler = handler; return () => { backend.handler = null } },
  respond: backend.respond,
} }))
vi.mock('../lib/external-url', async importOriginal => ({...await importOriginal<typeof import('../lib/external-url')>(), openExternalUrl: vi.fn(async () => {}) }))

it('answers a background MCP form with typed values and the original request ID', async () => {
  render(<McpRequests />)
  await waitFor(() => expect(backend.handler).not.toBeNull())
  act(() => backend.handler?.({id:'form-42', method:'mcpServer/elicitation/request', params:{
    threadId:'background', turnId:null, serverName:'travel', mode:'form', message:'Choose your trip',
    requestedSchema:{type:'object', properties:{city:{type:'string', title:'City'}, count:{type:'integer', minimum:1, maximum:5}, flexible:{type:'boolean', default:false}, seats:{type:'array', items:{type:'string', enum:['aisle','window']}, minItems:1}}, required:['city','count','seats']},
  }}))
  await userEvent.type(screen.getByLabelText('City'), 'Brisbane')
  await userEvent.type(screen.getByLabelText('count'), '2')
  await userEvent.click(screen.getByLabelText('flexible'))
  await userEvent.selectOptions(screen.getByLabelText('seats'), ['window'])
  await userEvent.click(screen.getByRole('button', {name:'Submit'}))
  await waitFor(() => expect(backend.respond).toHaveBeenCalledWith('form-42', {action:'accept', content:{city:'Brisbane', count:2, flexible:true, seats:['window']}, _meta:null}))
  expect(screen.queryByText('Choose your trip')).not.toBeInTheDocument()
})

it('lets the user decline an unsafe URL without opening it, and removes resolved requests', async () => {
  render(<McpRequests />)
  await waitFor(() => expect(backend.handler).not.toBeNull())
  act(() => backend.handler?.({id:91, method:'mcpServer/elicitation/request', params:{threadId:'background', serverName:'untrusted', mode:'url', message:'Connect account', url:'javascript:alert(1)', elicitationId:'e1'}}))
  expect(screen.queryByRole('button', {name:'Open in browser'})).not.toBeInTheDocument()
  await userEvent.click(screen.getByRole('button', {name:'Decline'}))
  await waitFor(() => expect(backend.respond).toHaveBeenCalledWith(91, {action:'decline', content:null, _meta:null}))
  act(() => backend.handler?.({id:'gone', method:'mcpServer/elicitation/request', params:{serverName:'test', mode:'form', message:'Resolved elsewhere', requestedSchema:{type:'object', properties:{}}}}))
  act(() => backend.handler?.({method:'serverRequest/resolved', params:{requestId:'gone'}}))
  expect(screen.queryByText('Resolved elsewhere')).not.toBeInTheDocument()
})

it('accepts required empty strings allowed by the server schema and validates typed defaults', async () => {
  render(<McpRequests />)
  await waitFor(() => expect(backend.handler).not.toBeNull())
  act(() => backend.handler?.({id:'empty', method:'mcpServer/elicitation/request', params:{serverName:'schema', mode:'form', message:'Confirm values', requestedSchema:{type:'object', properties:{notes:{type:'string'}, code:{type:'string', enum:['','yes']}, picks:{type:'array', items:{anyOf:[{const:'one',title:'First'}]}, default:['one']}, count:{type:'integer', minimum:1, default:0}}, required:['notes','code']}}}))
  await userEvent.click(screen.getByRole('button', {name:'Submit'}))
  expect(backend.respond).not.toHaveBeenCalledWith('empty', expect.anything())
  await userEvent.clear(screen.getByLabelText('count'))
  await userEvent.type(screen.getByLabelText('count'), '1')
  await userEvent.click(screen.getByRole('button', {name:'Submit'}))
  await waitFor(() => expect(backend.respond).toHaveBeenCalledWith('empty', {action:'accept',content:{notes:'',code:'',picks:['one'],count:1},_meta:null}))
})

it('supports structured OpenAI form responses through a validated JSON editor', async () => {
  render(<McpRequests />)
  await waitFor(() => expect(backend.handler).not.toBeNull())
  act(() => backend.handler?.({id:'structured',method:'mcpServer/elicitation/request',params:{serverName:'plugin',mode:'openaiForm',message:'Choose settings',requestedSchema:{type:'object',properties:{settings:{type:'object',properties:{enabled:{type:'boolean'}},required:['enabled']}},required:['settings']}}}))
  await userEvent.click(screen.getByRole('button',{name:'Edit response JSON'}))
  fireEvent.change(screen.getByRole('textbox',{name:'Response JSON'}),{target:{value:'{"settings":{"enabled":true}}'}})
  await userEvent.click(screen.getByRole('button',{name:'Submit'}))
  await waitFor(() => expect(backend.respond).toHaveBeenCalledWith('structured',{action:'accept',content:{settings:{enabled:true}},_meta:null}))
})
