export type DraftReferenceType = 'agent' | 'file' | 'folder' | 'skill'

export type DraftReference = {
  type: DraftReferenceType
  id: string
  name: string
  path?: string
  start: number
  end: number
}

export type DraftQuote = { text: string; start: number; end: number }
export type DraftSelection = { anchor: number; focus: number }
export type DraftDocument = {
  version: 1
  text: string
  references: DraftReference[]
  quotes: DraftQuote[]
  selection?: DraftSelection
  attachmentIds?: string[]
}
export type DraftAgent = { id: string; name: string }
export type DraftAssignment =
  | { kind: 'none' }
  | { kind: 'assigned'; agent: DraftAgent }
  | { kind: 'error'; reason: 'multiple-agents'; agentIds: string[] }

export function emptyDraft(text = ''): DraftDocument {
  return { version: 1, text, references: [], quotes: [], attachmentIds: [] }
}

function remapSpan(start: number, end: number, oldText: string, newText: string) {
  if (oldText === newText) return { start, end }
  let prefix = 0
  while (prefix < oldText.length && prefix < newText.length && oldText[prefix] === newText[prefix]) prefix += 1
  let suffix = 0
  while (suffix < oldText.length - prefix && suffix < newText.length - prefix &&
    oldText[oldText.length - 1 - suffix] === newText[newText.length - 1 - suffix]) suffix += 1
  const oldEnd = oldText.length - suffix
  const newEnd = newText.length - suffix
  const delta = newEnd - oldEnd
  if (end <= prefix) return { start, end }
  if (start >= oldEnd) return { start: start + delta, end: end + delta }
  if (start < prefix && end > oldEnd) return { start, end: end + delta }
  return null
}

export function draftWithText(document: DraftDocument, text: string): DraftDocument {
  if (document.text === text) return document
  const references = document.references.flatMap(reference => {
    const span = remapSpan(reference.start, reference.end, document.text, text)
    return span && span.end <= text.length ? [{ ...reference, ...span }] : []
  })
  const quotes = document.quotes.flatMap(quote => {
    const span = remapSpan(quote.start, quote.end, document.text, text)
    return span && span.end <= text.length ? [{ ...quote, ...span }] : []
  })
  const selection = document.selection
    ? { anchor: Math.min(text.length, document.selection.anchor), focus: Math.min(text.length, document.selection.focus) }
    : undefined
  return { ...document, text, references, quotes, ...(selection ? { selection } : {}) }
}

export function appendQuote(document: DraftDocument, quoteText: string): DraftDocument {
  const prefix = document.text && !/[\s]$/.test(document.text) ? ' ' : ''
  const start = document.text.length + prefix.length
  const rendered = `“${quoteText}”`
  const nextText = `${document.text}${prefix}${rendered}`
  return {
    ...document,
    text: nextText,
    references: document.references.map(reference => ({ ...reference })),
    quotes: [...document.quotes.map(quote => ({ ...quote })), { text: quoteText, start, end: start + rendered.length }],
    selection: { anchor: nextText.length, focus: nextText.length },
  }
}

function maskedRanges(text: string): Array<{ start: number; end: number }> {
  const ranges: Array<{ start: number; end: number }> = []
  const fenced = /```[\s\S]*?(?:```|$)/g
  for (const match of text.matchAll(fenced)) ranges.push({ start: match.index!, end: match.index! + match[0].length })
  const doubleQuoted = /“[^”]*”|"(?:\\.|[^"\\])*"/g
  for (const match of text.matchAll(doubleQuoted)) ranges.push({ start: match.index!, end: match.index! + match[0].length })
  const singleQuoted = /(^|[\s([{])'(?:\\.|[^'\\])*'/g
  for (const match of text.matchAll(singleQuoted)) {
    const start = match.index! + match[1].length
    ranges.push({ start, end: match.index! + match[0].length })
  }
  return ranges.sort((a, b) => a.start - b.start)
}

function overlaps(start: number, end: number, ranges: Array<{ start: number; end: number }>) {
  return ranges.some(range => start < range.end && end > range.start)
}

export function assignmentFromDraft(document: DraftDocument, agents: DraftAgent[]): DraftAssignment {
  const targets = new Map<string, DraftAgent>()
  for (const ref of document.references) {
    if (ref.type === 'agent') {
      const agent = agents.find(candidate => candidate.id === ref.id)
      if (agent) targets.set(agent.id, agent)
    }
  }
  const masked = [
    ...maskedRanges(document.text),
    ...document.references.filter(ref => ref.type !== 'agent').map(({ start, end }) => ({ start, end })),
  ]
  const candidates = [...agents].sort((a, b) => b.name.length - a.name.length)
  for (const agent of candidates) {
    const escaped = agent.name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
    const pattern = new RegExp(`(^|[\\s(])@${escaped}(?=$|[\\s),.!?:;])`, 'gi')
    for (const match of document.text.matchAll(pattern)) {
      const start = match.index! + match[1].length
      const end = start + match[0].length - match[1].length
      if (overlaps(start, end, masked)) continue
      const coveredByAgentRef = document.references.some(ref => ref.type === 'agent' && ref.id === agent.id && ref.start === start && ref.end === end)
      if (!coveredByAgentRef && !overlaps(start, end, document.references.map(ref => ({ start: ref.start, end: ref.end })))) targets.set(agent.id, agent)
    }
  }
  const found = [...targets.values()]
  if (!found.length) return { kind: 'none' }
  if (found.length > 1) return { kind: 'error', reason: 'multiple-agents', agentIds: found.map(agent => agent.id) }
  return { kind: 'assigned', agent: found[0] }
}
