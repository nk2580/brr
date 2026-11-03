import { forwardRef, useCallback, useEffect, useId, useImperativeHandle, useMemo, useRef, useState } from 'react'
import type { ReactNode, Ref } from 'react'
import { $getRoot, $getSelection, $addUpdateTag, HISTORY_PUSH_TAG, $setSelection, $createRangeSelection, $isElementNode, $isRangeSelection, $isTextNode, $createParagraphNode, $createTextNode, $applyNodeReplacement, TextNode, KEY_ENTER_COMMAND, UNDO_COMMAND, REDO_COMMAND, COMMAND_PRIORITY_HIGH, type EditorConfig, type NodeKey, type SerializedTextNode, type Spread } from 'lexical'
import { LexicalComposer } from '@lexical/react/LexicalComposer'
import { ContentEditable } from '@lexical/react/LexicalContentEditable'
import { PlainTextPlugin } from '@lexical/react/LexicalPlainTextPlugin'
import { HistoryPlugin, type HistoryState } from '@lexical/react/LexicalHistoryPlugin'
import { OnChangePlugin } from '@lexical/react/LexicalOnChangePlugin'
import { useLexicalComposerContext } from '@lexical/react/LexicalComposerContext'
import { File, Folder, Bot, Sparkles, WandSparkles } from 'lucide-react'
import type { DraftDocument, DraftReference, DraftReferenceType } from '../lib/chat-draft'
import { appendQuote } from '../lib/chat-draft'
import './ChatEditor.css'

export type ChatEditorAgent = { id: string; name: string }
export type ChatEditorReference = { id: string; name: string; path: string; type: 'file' | 'folder' }
export type ChatEditorSkill = { name: string; path: string; description?: string }
export type ChatEditorHandle = {
  focus: () => void
  insertText: (text: string) => void
  insertQuote: (text: string) => void
  openMenu: (trigger: '@' | '/') => void
  undo: () => void
  redo: () => void
}
export type ChatEditorProps = {
  document: DraftDocument
  onChange: (document: DraftDocument) => void
  onSubmit: (accelerated: boolean) => void
  onTogglePlan: () => void
  disabled?: boolean
  agents?: ChatEditorAgent[]
  references?: ChatEditorReference[]
  skills?: ChatEditorSkill[]
  onQueryReferences?: (query: string) => void
  onPasteFiles?: (files: File[]) => void
  onOpenReference?: (id: string, type: 'file' | 'folder') => void
}

type EntityMeta = { kind: 'reference'; reference: DraftReference } | { kind: 'quote'; quote: { text: string; start: number; end: number } }
type SerializedEntityTextNode = Spread<{ entity?: EntityMeta }, SerializedTextNode>

class EntityTextNode extends TextNode {
  __entity?: EntityMeta
  constructor(text: string, entity?: EntityMeta, key?: NodeKey) { super(text, key); this.__entity = entity }
  static getType() { return 'chat-entity-text' }
  static clone(node: EntityTextNode) { return new EntityTextNode(node.__text, node.__entity, node.__key) }
  static importJSON(serialized: SerializedEntityTextNode) { return new EntityTextNode(serialized.text, serialized.entity).updateFromJSON(serialized) }
  updateFromJSON(serialized: SerializedEntityTextNode) { const node = super.updateFromJSON(serialized); node.__entity = serialized.entity; return node }
  exportJSON(): SerializedEntityTextNode { return { ...super.exportJSON(), entity: this.__entity } }
  createDOM(config: EditorConfig) {
    const element = super.createDOM(config)
    if (this.__entity?.kind === 'reference') {
      const reference = this.__entity.reference
      element.classList.add('chat-editor-reference-chip', `chat-editor-reference-${reference.type}`)
      if (reference.type === 'file' || reference.type === 'folder') {
        element.dataset.chatReferenceId = reference.id
        element.dataset.chatReferenceType = reference.type
        element.setAttribute('role', 'link')
        element.setAttribute('aria-label', `Open reference ${reference.name}`)
        element.tabIndex = 0
      }
    }
    if (this.__entity?.kind === 'quote') element.classList.add('chat-editor-quote-chip')
    return element
  }
  updateDOM(previous: this, element: HTMLElement, config: EditorConfig) {
    if (previous.__entity?.kind !== this.__entity?.kind || JSON.stringify(previous.__entity) !== JSON.stringify(this.__entity)) return true
    return super.updateDOM(previous, element, config)
  }
  getEntity() { return this.__entity }
}

const $createEntityTextNode = (text: string, entity?: EntityMeta) => $applyNodeReplacement(new EntityTextNode(text, entity)).setMode('token')

type MenuItem = { key: string; label: string; detail?: string; icon: ReactNode; entity: EntityMeta; text: string }

function $setSelectionFromOffsets(anchorOffset: number, focusOffset: number, textLength: number) {
  const nodes = $getRoot().getAllTextNodes()
  const pointAt = (requested: number, preferNext: boolean) => {
    const target = Math.max(0, Math.min(textLength, requested))
    let cursor = 0
    for (let index = 0; index < nodes.length; index += 1) {
      const node = nodes[index]
      const length = node.getTextContentSize()
      if (target < cursor + length || target === cursor + length && (!preferNext || index === nodes.length - 1)) return { node, offset: target - cursor }
      cursor += length
      if (target === cursor && preferNext && nodes[index + 1]) return { node: nodes[index + 1], offset: 0 }
    }
    const last = nodes.at(-1)
    return last ? { node: last, offset: last.getTextContentSize() } : null
  }
  const anchor = pointAt(anchorOffset, true)
  const focus = pointAt(focusOffset, false)
  if (!anchor || !focus) { $getRoot().selectEnd(); return null }
  const selection = $createRangeSelection()
  selection.anchor.set(anchor.node.getKey(), anchor.offset, 'text')
  selection.focus.set(focus.node.getKey(), focus.offset, 'text')
  $setSelection(selection)
  return selection
}

function makeEditorState(document: DraftDocument) {
  const ranges = [
    ...document.references.map(reference => ({ start: reference.start, end: reference.end, entity: { kind: 'reference' as const, reference } })),
    ...document.quotes.map(quote => ({ start: quote.start, end: quote.end, entity: { kind: 'quote' as const, quote } })),
  ].sort((a, b) => a.start - b.start)
  return () => {
    const root = $getRoot()
    root.clear()
    let paragraph = $createParagraphNode()
    root.append(paragraph)
    let cursor = 0
    for (const range of ranges) {
      if (range.start < cursor || range.end > document.text.length) continue
      if (range.start > cursor) paragraph.append($createTextNode(document.text.slice(cursor, range.start)))
      paragraph.append($createEntityTextNode(document.text.slice(range.start, range.end), range.entity))
      cursor = range.end
    }
    if (cursor < document.text.length) paragraph.append($createTextNode(document.text.slice(cursor)))
    if (document.text.includes('\n') && ranges.length === 0) (paragraph.getFirstChild() as TextNode | null)?.setTextContent(document.text)
    if (!document.selection) { root.selectEnd(); return }
    $setSelectionFromOffsets(document.selection.anchor, document.selection.focus, document.text.length)
  }
}

function plainTextAndEntities() {
  const root = $getRoot()
  let text = ''
  const references: DraftReference[] = []
  const quotes: DraftDocument['quotes'] = []
  const nodeStarts = new Map<string, number>()
  const visit = (node: import('lexical').LexicalNode) => {
    nodeStarts.set(node.getKey(), text.length)
    if ($isTextNode(node)) {
      const value = node.getTextContent()
      const start = text.length
      nodeStarts.set(node.getKey(), start)
      text += value
      const entity = node instanceof EntityTextNode ? node.getEntity() : undefined
      if (entity?.kind === 'reference') references.push({ ...entity.reference, start, end: start + value.length })
      if (entity?.kind === 'quote') quotes.push({ ...entity.quote, start, end: start + value.length })
      return
    }
    if ($isElementNode(node)) node.getChildren().forEach((child, index, children) => {
      visit(child)
      if (child.getType() === 'linebreak' || index < children.length - 1 && child.getType() === 'linebreak') text += '\n'
    })
  }
  root.getChildren().forEach((child, index) => { if (index) text += '\n'; visit(child) })
  const selection = $getSelection()
  const pointOffset = (point: import('lexical').PointType) => {
    const node = point.getNode()
    const start = nodeStarts.get(node.getKey())
    if (start === undefined) return undefined
    if ($isTextNode(node)) return start + Math.min(point.offset, node.getTextContentSize())
    if ($isElementNode(node)) {
      const preceding = node.getChildren().slice(0, point.offset)
      return start + preceding.reduce((length, child) => length + child.getTextContent().length + (child.getType() === 'linebreak' ? 1 : 0), 0)
    }
    return start
  }
  const anchor = $isRangeSelection(selection) ? pointOffset(selection.anchor) : undefined
  const focus = $isRangeSelection(selection) ? pointOffset(selection.focus) : undefined
  return { text, references, quotes, nodeStarts, ...(anchor !== undefined && focus !== undefined ? { selection: { anchor, focus } } : {}) }
}

function QueryPlugin({ onQuery, onMenuState }: { onQuery: (text: string) => void; onMenuState: (state: { trigger: '@' | '/'; query: string; start: number; end: number } | null) => void }) {
  const [editor] = useLexicalComposerContext()
  useEffect(() => editor.registerUpdateListener(({ editorState }) => editorState.read(() => {
    const { text, nodeStarts } = plainTextAndEntities()
    const selection = $getSelection()
    let caret = text.length
    if ($isRangeSelection(selection)) {
      const anchor = selection.anchor
      const anchorNode = anchor.getNode()
      if (anchorNode instanceof TextNode) {
        const start = nodeStarts.get(anchorNode.getKey())
        if (start !== undefined) caret = start + Math.min(anchor.offset, anchorNode.getTextContentSize())
      }
    }
    const before = text.slice(0, Math.min(text.length, caret))
    const match = /(^|\s)([@/])([^\s@/]*)$/.exec(before)
    if (!match) { onMenuState(null); return }
    const start = before.length - match[0].length + match[1].length
    const query = match[3]
    onMenuState({ trigger: match[2] as '@' | '/', query, start, end: before.length })
    if (match[2] === '@') onQuery(query)
  })), [editor, onMenuState, onQuery])
  return null
}

function ChangePlugin({ onChange, document }: { onChange: (document: DraftDocument) => void; document: DraftDocument }) {
  return <OnChangePlugin onChange={editorState => editorState.read(() => {
    const snapshot = plainTextAndEntities()
    onChange({ ...document, version: 1, text: snapshot.text, references: snapshot.references, quotes: snapshot.quotes, ...(snapshot.selection ? { selection: snapshot.selection } : {}) })
  })} />
}

function EditorBody(props: ChatEditorProps & { forwardedRef: Ref<ChatEditorHandle> }) {
  const [editor] = useLexicalComposerContext()
  // Seed legacy history after the composer has committed its initial state;
  // otherwise its first edit has no current entry to put on the undo stack.
  const historyState = useMemo<HistoryState>(() => ({ current: null, undoStack: [], redoStack: [] }), [editor])
  useEffect(() => {
    historyState.current = { editor, editorState: editor.getEditorState() }
  }, [editor, historyState])
  const [menu, setMenu] = useState<{ trigger: '@' | '/'; query: string; start: number; end: number } | null>(null)
  const [highlighted, setHighlighted] = useState(0)
  const menuId = useId()
  const shellRef = useRef<HTMLDivElement>(null)
  const dismissedMenu = useRef<string | null>(null)
  const composingEndedAt = useRef(0)
  const source = useRef(JSON.stringify(props.document))
  const latestPublished = useRef('')
  const onQueryReferences = useRef(props.onQueryReferences)
  onQueryReferences.current = props.onQueryReferences
  const query = useCallback((text: string) => onQueryReferences.current?.(text), [])
  const menuKey = (value: NonNullable<typeof menu>) => `${value.trigger}:${value.start}:${value.query}`
  const menuState = useCallback((next: typeof menu) => {
    if (!next) { dismissedMenu.current = null; setMenu(null); return }
    const key = menuKey(next)
    if (dismissedMenu.current === key) { setMenu(null); return }
    if (dismissedMenu.current && dismissedMenu.current !== key) dismissedMenu.current = null
    setMenu(next)
    setHighlighted(0)
  }, [])

  useEffect(() => editor.setEditable(!props.disabled), [editor, props.disabled])

  useEffect(() => {
    if (!menu) return
    const dismissOutside = (event: PointerEvent) => {
      if (event.target instanceof Node && shellRef.current?.contains(event.target)) return
      dismissedMenu.current = menuKey(menu)
      setMenu(null)
    }
    document.addEventListener('pointerdown', dismissOutside)
    return () => document.removeEventListener('pointerdown', dismissOutside)
  }, [menu])

  useEffect(() => {
    // Some DOM test environments do not implement Range geometry; browsers do.
    const serialized = JSON.stringify(props.document)
    if (serialized === latestPublished.current) return
    if (serialized === source.current) return
    source.current = serialized
    editor.update(makeEditorState(props.document))
  }, [editor, props.document])

  const items = useMemo<MenuItem[]>(() => {
    if (!menu) return []
    if (menu.trigger === '@') {
      const agents: MenuItem[] = (props.agents ?? []).filter(agent => agent.name.toLowerCase().includes(menu.query.toLowerCase())).map(agent => ({
        key: `agent:${agent.id}`, label: agent.name, detail: 'Agent', icon: <Bot size={14} aria-hidden="true"/>,
        text: `@${agent.name}`, entity: { kind: 'reference', reference: { type: 'agent', id: agent.id, name: agent.name, start: menu.start, end: menu.start + agent.name.length + 1 } },
      }))
      const refs: MenuItem[] = (props.references ?? []).filter(reference => reference.name.toLowerCase().includes(menu.query.toLowerCase())).map(reference => ({
        key: `${reference.type}:${reference.id}`, label: reference.name, detail: reference.path, icon: reference.type === 'folder' ? <Folder size={14} aria-hidden="true"/> : <File size={14} aria-hidden="true"/>,
        text: `@${reference.name}`, entity: { kind: 'reference', reference: { ...reference, start: menu.start, end: menu.start + reference.name.length + 1 } },
      }))
      return [...agents, ...refs]
    }
    const builtins: MenuItem[] = [{ key: 'plan', label: 'Plan mode', detail: 'Toggle Plan mode', icon: <WandSparkles size={14} aria-hidden="true"/>, text: '/plan', entity: { kind: 'reference', reference: { type: 'skill', id: 'plan', name: 'Plan mode', path: 'builtin:plan', start: menu.start, end: menu.start + 5 } } }]
    const skills: MenuItem[] = (props.skills ?? []).filter(skill => skill.name.toLowerCase().includes(menu.query.toLowerCase())).map(skill => ({
      key: `skill:${skill.path}`, label: skill.name, detail: skill.description || skill.path, icon: <Sparkles size={14} aria-hidden="true"/>, text: `/${skill.name}`,
      entity: { kind: 'reference', reference: { type: 'skill', id: skill.path, name: skill.name, path: skill.path, start: menu.start, end: menu.start + skill.name.length + 1 } },
    }))
    return [...builtins.filter(item => item.label.toLowerCase().includes(menu.query.toLowerCase())), ...skills]
  }, [menu, props.agents, props.references, props.skills])

  useImperativeHandle(props.forwardedRef, () => ({
    focus: () => editor.focus(),
    undo: () => { editor.dispatchCommand(UNDO_COMMAND, undefined) },
    redo: () => { editor.dispatchCommand(REDO_COMMAND, undefined) },
    insertText: text => editor.update(() => {
      $addUpdateTag(HISTORY_PUSH_TAG)
      let selection = $getSelection()
      if (!$isRangeSelection(selection)) { $getRoot().selectEnd(); selection = $getSelection() }
      if ($isRangeSelection(selection)) selection.insertText(text)
      const current = plainTextAndEntities()
      const next: DraftDocument = { ...props.document, version: 1, text: current.text, references: current.references, quotes: current.quotes, ...(current.selection ? { selection: current.selection } : {}) }
      latestPublished.current = JSON.stringify(next)
      source.current = latestPublished.current
      props.onChange(next)
    }),
    insertQuote: text => editor.update(() => {
      $addUpdateTag(HISTORY_PUSH_TAG)
      const current = plainTextAndEntities()
      const next = appendQuote({ ...props.document, ...current }, text)
      makeEditorState(next)()
      latestPublished.current = JSON.stringify(next)
      source.current = latestPublished.current
      props.onChange(next)
    }),
    openMenu: trigger => {
      dismissedMenu.current = null
      let before = ''
      editor.update(() => {
      before = plainTextAndEntities().text
      let selection = $getSelection()
      if (!$isRangeSelection(selection)) { $getRoot().selectEnd(); selection = $getSelection() }
      if ($isRangeSelection(selection)) selection.insertText(trigger)
      })
      setMenu({ trigger, query: '', start: before.length, end: before.length + 1 })
    },
  }), [editor, props.document, props.forwardedRef])

  const choose = useCallback((item: MenuItem) => {
    if (!menu) return
    if (item.key === 'plan') props.onTogglePlan()
    editor.update(() => {
      $addUpdateTag(HISTORY_PUSH_TAG)
      let selection = $getSelection()
      if (!$isRangeSelection(selection)) { $getRoot().selectEnd(); selection = $getSelection() }
      if (!$isRangeSelection(selection)) return
      // Derive the completion span from the live caret so a quick key press
      // cannot settle a stale menu range while the query update is still rendering.
      const currentSnapshot = plainTextAndEntities()
      const caret = currentSnapshot.selection?.focus ?? menu.end
      const beforeCaret = currentSnapshot.text.slice(0, caret)
      const token = /(^|\s)([@/])([^\s@/]*)$/.exec(beforeCaret)
      const liveStart = token?.[2] === menu.trigger ? beforeCaret.length - token[0].length + token[1].length : menu.start
      const liveEnd = token?.[2] === menu.trigger ? caret : menu.end
      // Select the exact UTF-16 span, then replace it in one edit. Repeated
      // character deletion treats grapheme clusters as units and can eat text before emoji.
      const replacementSelection = $setSelectionFromOffsets(liveStart, liveEnd, currentSnapshot.text.length)
      if (!replacementSelection) return
      if (item.key !== 'plan') {
        const node = $createEntityTextNode(item.text, item.entity)
        replacementSelection.insertNodes([node, $createTextNode(' ')])
      }
      const current = plainTextAndEntities()
      const next: DraftDocument = { ...props.document, version: 1, text: current.text, references: current.references, quotes: current.quotes, ...(current.selection ? { selection: current.selection } : {}) }
      latestPublished.current = JSON.stringify(next)
      source.current = latestPublished.current
      props.onChange(next)
    })
    setMenu(null)
  }, [editor, menu, props])

  const onEnter = (event: KeyboardEvent) => {
    if (event.isComposing || event.repeat || Date.now() - composingEndedAt.current < 80) { event.preventDefault(); return true }
    if (menu && items.length) { event.preventDefault(); choose(items[highlighted] ?? items[0]); return true }
    if (event.ctrlKey || event.metaKey) { event.preventDefault(); props.onSubmit(true); return true }
    if (!event.shiftKey) { event.preventDefault(); props.onSubmit(false); return true }
    return false
  }
  useEffect(() => editor.registerCommand(KEY_ENTER_COMMAND, event => event ? onEnter(event) : false, COMMAND_PRIORITY_HIGH), [editor, onEnter])

  const onKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    if (event.key === 'Enter') return
    if (menu && items.length && (event.key === 'ArrowDown' || event.key === 'ArrowUp')) {
      event.preventDefault()
      setHighlighted(index => (index + (event.key === 'ArrowDown' ? 1 : items.length - 1)) % items.length)
      return
    }
    if (menu && items.length && event.key === 'Tab' && !event.shiftKey) { event.preventDefault(); choose(items[highlighted] ?? items[0]); return }
    if (event.key === 'Escape' && menu) { event.preventDefault(); dismissedMenu.current = menuKey(menu); setMenu(null); return }
    if (event.key === 'Tab' && event.shiftKey) { event.preventDefault(); if (menu) { dismissedMenu.current = menuKey(menu); setMenu(null) } else props.onTogglePlan(); return }
  }

  const onEditorChange = (document: DraftDocument) => {
    latestPublished.current = JSON.stringify(document)
    source.current = latestPublished.current
    props.onChange(document)
  }
  const handlePaste = (event: React.ClipboardEvent<HTMLDivElement>) => {
    const files = Array.from(event.clipboardData.files)
    if (files.length && props.onPasteFiles) { event.preventDefault(); props.onPasteFiles(files); return }
    event.preventDefault()
    const text = event.clipboardData.getData('text/plain')
    editor.update(() => { const selection = $getSelection(); if ($isRangeSelection(selection)) selection.insertText(text) })
  }
  const renderItem = (item: MenuItem) => {
    const index = items.indexOf(item)
    return <button id={`${menuId}-option-${index}`} type="button" role="option" aria-selected={highlighted === index} className="chat-editor-menu-item" key={item.key} onMouseDown={event => event.preventDefault()} onClick={() => choose(item)}>
      {item.icon}<span>{item.label}</span>{item.detail && <small>{item.detail}</small>}
    </button>
  }

  const openReference = (target: EventTarget | null) => {
    const chip = target instanceof Element ? target.closest<HTMLElement>('[data-chat-reference-id]') : null
    if (!chip || !props.onOpenReference) return false
    props.onOpenReference(chip.dataset.chatReferenceId!, chip.dataset.chatReferenceType as 'file' | 'folder')
    return true
  }
  return <div className="chat-editor-shell" ref={shellRef} onClick={event => { if (openReference(event.target)) event.stopPropagation() }} onKeyDownCapture={event => {
    if ((event.key === 'Enter' || event.key === ' ') && event.target instanceof HTMLElement && event.target.matches('[data-chat-reference-id]') && openReference(event.target)) { event.preventDefault(); event.stopPropagation() }
  }}>
    <div className="chat-editor-input" onKeyDown={onKeyDown} onCompositionEnd={() => { composingEndedAt.current = Date.now() }} onPaste={handlePaste}>
      <PlainTextPlugin contentEditable={<ContentEditable aria-label="Message" className="chat-editor-content" aria-disabled={props.disabled} aria-controls={menu ? menuId : undefined} aria-activedescendant={menu && items.length ? `${menuId}-option-${highlighted}` : undefined} onFocus={() => editor.update(() => { if (!$isRangeSelection($getSelection())) $getRoot().selectEnd() })} />} placeholder={<div className="chat-editor-placeholder">Message</div>} ErrorBoundary={({ children }: { children: ReactNode }) => <>{children}</>} />
      <HistoryPlugin externalHistoryState={historyState} />
      <ChangePlugin onChange={onEditorChange} document={props.document} />
      <QueryPlugin onQuery={query} onMenuState={menuState} />
    </div>
    {menu && items.length > 0 && <div className="chat-editor-menu" id={menuId} role="listbox" aria-label={menu.trigger === '@' ? 'Agents and references' : 'Plan and skills'}>
      {menu.trigger === '@' ? <>
        {items.filter(item => item.key.startsWith('agent:')).length > 0 && <><div className="chat-editor-menu-heading">Agents</div>{items.filter(item => item.key.startsWith('agent:')).map(renderItem)}</>}
        {items.filter(item => !item.key.startsWith('agent:')).length > 0 && <><div className="chat-editor-menu-heading">Files and folders</div>{items.filter(item => !item.key.startsWith('agent:')).map(renderItem)}</>}
      </> : <><div className="chat-editor-menu-heading">Plan and skills</div>{items.map(renderItem)}</>}
    </div>}
  </div>
}

export const ChatEditor = forwardRef<ChatEditorHandle, ChatEditorProps>(function ChatEditor(props, ref) {
  const initialConfig = useMemo(() => ({
    namespace: 'BrrChatEditor', nodes: [EntityTextNode], editable: !props.disabled,
    onError(error: Error) { throw error },
    editorState: makeEditorState(props.document),
  }), [])
  return <LexicalComposer initialConfig={initialConfig}>
    <EditorBody {...props} forwardedRef={ref}/>
  </LexicalComposer>
})

// Retain an exported symbol for consumers that need to create a semantic node in an editor extension.
export { EntityTextNode }
