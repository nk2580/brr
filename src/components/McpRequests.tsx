import { useEffect, useId, useState } from 'react'
import { Validator, type Schema as JsonSchema } from '@cfworker/json-schema'
import { client } from '../lib/client'
import { openExternalUrl, isExternalUrl } from '../lib/external-url'
import type { Approval } from '../lib/session'
import { Button } from './ui/button'

type Field = { type?: string; title?: string; description?: string; default?: unknown; enum?: string[]; enumNames?: string[]; oneOf?: {const:string; title?:string}[]; anyOf?: {const:string; title?:string}[]; items?: Field; minItems?: number; maxItems?: number; minimum?: number; maximum?: number; minLength?: number; maxLength?: number; pattern?: string; format?: string }
type Schema = {type?:string; properties?:Record<string, Field>; required?:string[]}

// This queue is independent of the selected conversation: elicitations can also
// arrive during connection setup and while a different conversation is visible.
export function McpRequests() {
  const [requests, setRequests] = useState<Approval[]>([])
  const [error, setError] = useState('')
  useEffect(() => {
    let active = true
    let stop: (() => void) | undefined
    void client.subscribe(event => {
      if (!active) return
      if (event.method === 'brr/disconnected') setRequests([])
      else if (event.method === 'serverRequest/resolved') setRequests(previous => previous.filter(request => request.id !== event.params?.requestId))
      else if (event.method === 'mcpServer/elicitation/request' && event.id !== undefined) {
        const request = {id:event.id, method:event.method, params:event.params ?? {}}
        setRequests(previous => [...previous.filter(item => item.id !== request.id), request])
      }
    }).then(unsubscribe => { if (active) stop = unsubscribe; else unsubscribe() }).catch(reason => { if (active) setError(String(reason)) })
    return () => { active = false; stop?.() }
  }, [])
  if (!requests.length && !error) return null
  return <aside className="mcp-requests" aria-label="MCP requests">
    {error && <p role="alert">{error}</p>}
    {requests.map(request => <McpRequest key={`${typeof request.id}:${request.id}`} request={request} onComplete={() => setRequests(previous => previous.filter(item => item.id !== request.id))} />)}
  </aside>
}

function McpRequest({request, onComplete}:{request:Approval; onComplete:()=>void}) {
  const params = request.params
  const schema = (params.requestedSchema ?? {}) as Schema
  const fields = Object.entries(schema.properties ?? {})
  const supportedForm = ['form','openai/form','openaiForm'].includes(String(params.mode)) && schema.type === 'object'
  const [values, setValues] = useState<Record<string, unknown>>(() => Object.fromEntries(fields.flatMap(([name,field]) => {
    if (field.default !== undefined) return [[name, field.default]]
    if (field.type === 'boolean') return [[name, false]]
    if (schema.required?.includes(name) && field.type === 'string') return [[name, '']]
    if (schema.required?.includes(name) && field.type === 'array') return [[name, []]]
    return []
  })))
  const [raw, setRaw] = useState('{}')
  const [jsonMode, setJsonMode] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const set = (name:string, value:unknown) => setValues(previous => ({...previous,[name]:value}))
  const respond = async (action:'accept'|'decline'|'cancel', content:unknown = null) => {
    setBusy(true); setError('')
    try { await client.respond(request.id, {action, content, _meta:null}); onComplete() }
    catch (reason) { setError(String(reason)); setBusy(false) }
  }
  const submit = () => {
    try {
      const content = supportedForm && !jsonMode ? Object.fromEntries(Object.entries(values).filter(([,value]) => value !== undefined)) : JSON.parse(raw)
      if (!content || typeof content !== 'object' || Array.isArray(content)) throw new Error('Enter a JSON object.')
      const validation = new Validator(schema as JsonSchema, '2020-12', false).validate(content)
      if (!validation.valid) throw new Error(validation.errors.map(item => `${item.instanceLocation || 'Response'}: ${item.error}`).join('\n'))
      void respond('accept', content)
    } catch (reason) { setError(String(reason)) }
  }
  const isUrl = params.mode === 'url'
  const deviceVerification = params.mode === 'openai/userVerification'
  return <section className="approval-card" aria-label={`MCP request from ${String(params.serverName ?? 'server')}`}>
    <strong>{String(params.serverName ?? 'MCP server')} requests input</strong>
    <p>{String(params.message ?? params.description ?? params.title ?? '')}</p>
    {error && <p role="alert">{error}</p>}
    <form noValidate onSubmit={event => { event.preventDefault(); submit() }}>
      <fieldset disabled={busy}>
        {!isUrl && !deviceVerification && supportedForm && <Button type="button" variant="ghost" onClick={() => {
          try {
            if (jsonMode) {
              const parsed = JSON.parse(raw)
              if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) throw new Error('Enter a JSON object.')
              setValues(parsed)
            } else setRaw(JSON.stringify(values,null,2))
            setJsonMode(previous => !previous); setError('')
          } catch (reason) { setError(String(reason)) }
        }}>{jsonMode ? 'Use form controls' : 'Edit response JSON'}</Button>}
        {isUrl ? <><p className="project-path">{String(params.url ?? '')}</p>{isExternalUrl(params.url) ? <Button type="button" onClick={() => void openExternalUrl(String(params.url)).catch(reason => setError(String(reason)))}>Open in browser</Button> : <p role="alert">This server supplied an unsupported URL.</p>}<p>Complete the request in your browser, then continue.</p></> : deviceVerification ? <p>Device verification is unavailable in this build. You can decline or cancel this request.</p> : supportedForm && !jsonMode ? fields.map(([name,field]) => {
          const label = field.title ?? name
          const options = field.oneOf?.map(option => ({value:option.const,label:option.title ?? option.const})) ?? field.enum?.map((value,index) => ({value,label:field.enumNames?.[index] ?? value}))
          const arrayOptions = (field.items?.anyOf ?? field.items?.oneOf)?.map(option => ({value:option.const,label:option.title ?? option.const})) ?? field.items?.enum?.map(value => ({value,label:value}))
          return <McpField key={name} name={name} label={label} field={field} value={values[name]} options={options} arrayOptions={arrayOptions} onChange={value => set(name,value)} />
        }) : <><details><summary>Requested schema</summary><pre>{JSON.stringify(schema,null,2)}</pre></details><label>Response JSON<textarea aria-label="Response JSON" value={raw} onChange={event => setRaw(event.target.value)} /></label></>}
        <div>{!deviceVerification && <Button type={isUrl ? 'button' : 'submit'} disabled={busy || (isUrl && !isExternalUrl(params.url))} onClick={isUrl ? () => void respond('accept') : undefined}>{isUrl ? 'Continue' : 'Submit'}</Button>}<Button type="button" variant="ghost" onClick={() => void respond('decline')}>Decline</Button><Button type="button" variant="ghost" onClick={() => void respond('cancel')}>Cancel</Button></div>
      </fieldset>
    </form>
  </section>
}


type Choice = {value:string; label:string}
function McpField({name,label,field,value,options,arrayOptions,onChange}:{name:string;label:string;field:Field;value:unknown;options?:Choice[];arrayOptions?:Choice[];onChange:(value:unknown)=>void}) {
  const id = useId()
  const common = {id, name, 'aria-label':label}
  let control
  if (field.type === 'boolean') {
    control = <input {...common} type="checkbox" checked={value === true} onChange={event => onChange(event.target.checked)} />
  } else if (field.type === 'array' && arrayOptions) {
    control = <select {...common} multiple value={Array.isArray(value) ? value as string[] : []} onChange={event => onChange(Array.from(event.target.selectedOptions, option => option.value))}>
      {arrayOptions.map(option => <option key={option.value} value={option.value}>{option.label || '(empty)'}</option>)}
    </select>
  } else if (options) {
    const selected = options.findIndex(option => option.value === value)
    control = <select {...common} value={selected < 0 ? '' : String(selected)} onChange={event => onChange(event.target.value === '' ? undefined : options[Number(event.target.value)].value)}>
      <option value="">Choose…</option>
      {options.map((option,index) => <option key={index} value={String(index)}>{option.label || '(empty)'}</option>)}
    </select>
  } else {
    const numeric = field.type === 'number' || field.type === 'integer'
    control = <input {...common} type={numeric ? 'number' : 'text'} step="any" value={String(value ?? '')} onChange={event => onChange(numeric ? event.target.value === '' ? undefined : Number(event.target.value) : event.target.value)} />
  }
  return <label htmlFor={common.id}>{label}{field.description && <small>{field.description}</small>}{field.format === 'date-time' && <small>Include the time zone, for example 2026-11-01T10:00:00+10:00.</small>}{control}</label>
}
