import { validateUiDocument, type UiDocument } from './intelligent-ui'

// An incomplete value is never promoted to a complete component. Only the root
// object and its components array may contribute their already-closed prefix.
export function previewUiDocument(source: string): UiDocument | null {
  if (source.length > 5_000_000) return null
  let at = 0, remaining = 100_000
  const white = () => { while (/\s/.test(source[at] ?? '') && at < source.length) at++ }
  type Parsed = {value: any; complete: boolean}
  function read(depth = 0): Parsed {
    if (depth > 40 || --remaining < 0) throw new Error('UI preview limit')
    white()
    const start = at, first = source[at]
    if (first === '{' || first === '[') {
      const array = first === '[', close = array ? ']' : '}'
      const result: any = array ? [] : Object.create(null)
      at++; white()
      if (source[at] === close) { at++; return {value: result, complete: true} }
      while (at < source.length) {
        let key: string | null = null
        if (!array) {
          if (source[at] !== '"') throw new Error('Expected key')
          const parsed = read(depth + 1)
          if (!parsed.complete) return {value: result, complete: false}
          key = parsed.value
          if (key === '__proto__' || key === 'constructor' || key === 'prototype') throw new Error('Unsafe key')
          white()
          if (at === source.length) return {value: result, complete: false}
          if (source[at++] !== ':') throw new Error('Expected colon')
        }
        const parsed = read(depth + 1)
        if (array) { if (parsed.complete) result.push(parsed.value) }
        else if (parsed.complete || (depth === 0 && key === 'components' && Array.isArray(parsed.value))) result[key!] = parsed.value
        if (!parsed.complete) return {value: result, complete: false}
        white()
        if (source[at] === close) { at++; return {value: result, complete: true} }
        if (at === source.length) return {value: result, complete: false}
        if (source[at++] !== ',') throw new Error('Expected comma')
        white()
      }
      return {value: result, complete: false}
    }
    if (first === '"') {
      at++
      while (at < source.length) {
        if (source[at] === '\\') { at += 2; continue }
        if (source[at++] === '"') return {value: JSON.parse(source.slice(start, at)), complete: true}
      }
      return {value: undefined, complete: false}
    }
    while (at < source.length && !/[\s,\]}]/.test(source[at])) at++
    if (at === source.length) return {value: undefined, complete: false}
    return {value: JSON.parse(source.slice(start, at)), complete: true}
  }
  try {
    const result = read().value
    if (!result || result.version !== 1 || !['native', 'app'].includes(result.kind)
      || typeof result.title !== 'string' || typeof result.text !== 'string') return null
    const candidate = {...result, initialState: result.initialState ?? {}, actions: result.actions ?? [],
      ...(result.kind === 'native' ? {components: result.components ?? []} : {html: result.html ?? '<!doctype html>'})}
    const validated = validateUiDocument(candidate)
    return validated.ok ? validated.document : null
  } catch { return null }
}
