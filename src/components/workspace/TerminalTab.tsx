import { useEffect, useRef, useState } from 'react'
import { Terminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'
import { workspaceRequest } from '@/lib/workspace-client'
import '@xterm/xterm/css/xterm.css'
import './ToolTabs.css'

type PtyRead = {chunks: {seq: number; data: number[]}[]; cursor: number; exited: boolean; exitCode?: number; truncated?: boolean}

export function TerminalTab({root, visible, onTitle}: {root: string; visible: boolean; onTitle?: (title: string) => void}) {
  const [error, setError] = useState('')
  const [ptyId, setPtyId] = useState<string | null>(null)
  const [exited, setExited] = useState(false)
  const [exitCode, setExitCode] = useState<number | undefined>()
  const host = useRef<HTMLDivElement>(null)
  const terminal = useRef<Terminal | null>(null)
  const fit = useRef<FitAddon | null>(null)
  const ptyIdRef = useRef<string | null>(null)
  const create = useRef<{root: string; promise: Promise<string>} | null>(null)
  const cursor = useRef(0)
  const generation = useRef(0)
  const consumers = useRef(0)
  const lastRoot = useRef<string | null>(null)
  const activeRoot = useRef<string | null>(null)
  const closedIds = useRef(new Set<string>())
  const readInFlight = useRef(new Set<string>())
  const pendingWrite = useRef<Promise<void> | null>(null)
  const cancelOutputWait = useRef<(() => void) | null>(null)
  const exitedRef = useRef(false)
  const titleCallback = useRef(onTitle)
  titleCallback.current = onTitle
  exitedRef.current = exited

  useEffect(() => {
    const epoch = ++generation.current
    consumers.current++
    activeRoot.current = root
    let active = true
    if (lastRoot.current !== null && lastRoot.current !== root) {
      const oldId = ptyIdRef.current
      if (oldId) closePty(oldId, closedIds.current)
      ptyIdRef.current = null
      terminal.current?.dispose()
      terminal.current = null
      fit.current = null
      cursor.current = 0
      pendingWrite.current = null
      setPtyId(null); setExited(false); setExitCode(undefined); setError('')
      exitedRef.current = false
    }
    lastRoot.current = root
    let observer: ResizeObserver | undefined
    let themeObserver: MutationObserver | undefined
    const term = terminal.current ?? new Terminal({
      convertEol: true, cursorBlink: true, scrollback: 4000, allowProposedApi: false,
      fontFamily: fontFamily(), fontSize: 12, theme: terminalTheme(),
    })
    terminal.current = term
    const fitter = fit.current ?? new FitAddon()
    fit.current = fitter
    if (!term.element) term.loadAddon(fitter)
    if (host.current && !term.element) term.open(host.current)

    const applyAppearance = () => {
      term.options.fontFamily = fontFamily()
      term.options.theme = terminalTheme()
    }
    themeObserver = new MutationObserver(applyAppearance)
    themeObserver.observe(document.documentElement, {attributes: true, attributeFilter: ['class', 'style']})
    if (document.body) themeObserver.observe(document.body, {attributes: true, attributeFilter: ['class', 'style']})

    const resize = () => {
      if (!host.current || !host.current.clientWidth || !host.current.clientHeight) return
      try {
        fitter.fit()
        if (ptyIdRef.current) void workspaceRequest('terminal.resize', {id: ptyIdRef.current, cols: term.cols, rows: term.rows}).catch(reason => { if (active) setError(errorText(reason)) })
      } catch { /* The host can report zero dimensions during layout. */ }
    }
    observer = new ResizeObserver(resize)
    if (host.current) observer.observe(host.current)
    let queuedInput = ''
    let queuedInputId: string | null = null
    let inputFlushQueued = false
    let writeTail = Promise.resolve()
    const flushInput = () => {
      inputFlushQueued = false
      const id = queuedInputId
      const data = queuedInput
      queuedInput = ''; queuedInputId = null
      if (!active || !id || !data) return
      for (const chunk of splitUtf8(data, 16 * 1024)) {
        writeTail = writeTail.then(async () => {
          if (!active || ptyIdRef.current !== id || activeRoot.current !== root) return
          await workspaceRequest('terminal.write', {id, data: chunk})
        }).catch(reason => { if (active) setError(errorText(reason)) })
      }
    }
    const input = term.onData(data => {
      const id = ptyIdRef.current
      if (!active || !id) return
      if (queuedInputId && queuedInputId !== id) flushInput()
      queuedInputId = id
      queuedInput += data
      if (!inputFlushQueued) { inputFlushQueued = true; queueMicrotask(flushInput) }
    })

    const pending = create.current?.root === root
      ? create.current.promise
      : workspaceRequest<{id: string}>('terminal.create', {root, cols: term.cols, rows: term.rows}).then(result => result.id)
    create.current = {root, promise: pending}
    void pending.then(id => {
      if (generation.current !== epoch || activeRoot.current !== root) {
        if (activeRoot.current !== root) closePty(id, closedIds.current)
        return
      }
      ptyIdRef.current = id
      setPtyId(id)
      resize()
    }).catch(reason => {
      if (generation.current === epoch && activeRoot.current === root) setError(errorText(reason))
    })

    return () => {
      consumers.current = Math.max(0, consumers.current - 1)
      active = false
      input.dispose()
      observer?.disconnect()
      themeObserver?.disconnect()
      const cleanupEpoch = ++generation.current
      if (consumers.current === 0 && activeRoot.current === root) activeRoot.current = null
      queueMicrotask(() => {
        if (generation.current !== cleanupEpoch) return
        term.dispose()
        terminal.current = null
        const id = ptyIdRef.current
        ptyIdRef.current = null
        if (create.current?.root === root) create.current = null
        if (id) closePty(id, closedIds.current)
      })
    }
  }, [root])

  useEffect(() => {
    if (!visible || !ptyId) return
    let active = true
    let timer: ReturnType<typeof setTimeout> | undefined
    let failures = 0
    const term = terminal.current
    const waitForOutputWrite = (write: Promise<void>) => {
      let cancel!: () => void
      const hidden = new Promise<void>(resolve => { cancel = resolve })
      cancelOutputWait.current = cancel
      return Promise.race([write, hidden]).finally(() => {
        if (cancelOutputWait.current === cancel) cancelOutputWait.current = null
      })
    }
    const poll = async () => {
      if (!active) return
      if (activeRoot.current !== root || ptyIdRef.current !== ptyId) return
      if (readInFlight.current.has(ptyId) || pendingWrite.current) {
        timer = setTimeout(() => void poll(), 8)
        return
      }
      readInFlight.current.add(ptyId)
      let hadOutput = false
      try {
        const result = await workspaceRequest<PtyRead>('terminal.read', {id: ptyId, after: cursor.current, waitMs: 1000})
        if (!active || activeRoot.current !== root || ptyIdRef.current !== ptyId || !term) return
        failures = 0
        const chunks = result.chunks.filter(chunk => chunk.seq > cursor.current)
        const bytes = chunks.flatMap(chunk => chunk.data)
        if (result.truncated) bytes.push(...new TextEncoder().encode('\r\n[Earlier terminal output was trimmed.]\r\n'))
        const nextCursor = Math.max(cursor.current, result.cursor, ...chunks.map(chunk => chunk.seq))
        if (bytes.length) {
          hadOutput = true
          let finishWrite!: () => void
          const completion = new Promise<void>(resolve => { finishWrite = resolve })
          pendingWrite.current = completion
          term.write(Uint8Array.from(bytes), () => {
            if (terminal.current === term && ptyIdRef.current === ptyId && activeRoot.current === root) cursor.current = Math.max(cursor.current, nextCursor)
            if (pendingWrite.current === completion) pendingWrite.current = null
            finishWrite()
          })
          await waitForOutputWrite(completion)
          if (!active) return
        } else {
          cursor.current = nextCursor
        }
        if (result.exited) {
          exitedRef.current = true
          setExited(true)
          setExitCode(result.exitCode)
          titleCallback.current?.('Terminal')
          return
        }
      } catch (reason) {
        if (!active) return
        failures++
        setError(errorText(reason))
      } finally {
        readInFlight.current.delete(ptyId)
      }
      if (active && !exitedRef.current) timer = setTimeout(() => void poll(), failures ? Math.min(150 * 2 ** failures, 5000) : hadOutput ? 0 : 16)
    }
    void poll()
    return () => {
      active = false
      cancelOutputWait.current?.()
      if (timer) clearTimeout(timer)
    }
  }, [visible, ptyId, root])

  useEffect(() => {
    if (!visible) return
    let active = true
    const frame = requestAnimationFrame(() => {
      try {
        fit.current?.fit()
        const term = terminal.current
        if (ptyIdRef.current && term) void workspaceRequest('terminal.resize', {id: ptyIdRef.current, cols: term.cols, rows: term.rows}).catch(reason => { if (active) setError(errorText(reason)) })
      } catch { /* Wait for the panel to receive measurable dimensions. */ }
    })
    return () => { active = false; cancelAnimationFrame(frame) }
  }, [visible])

  return <section className="workspace-tool terminal-tool" hidden={!visible} aria-label="Terminal">
    <div className="terminal-status">{error ? <span role="alert">{error}</span> : exited ? `Process exited${exitCode === undefined ? '' : ` (${exitCode})`}` : 'Terminal'}</div>
    <div className="terminal-host" ref={host} />
  </section>
}

function closePty(id: string, closed: Set<string>) {
  if (closed.has(id)) return
  closed.add(id)
  void workspaceRequest('terminal.close', {id}).catch(() => {})
}

function splitUtf8(value: string, maxBytes: number) {
  const chunks: string[] = []
  let current = ''
  let bytes = 0
  for (const character of value) {
    const point = character.codePointAt(0) ?? 0
    const size = point <= 0x7f ? 1 : point <= 0x7ff ? 2 : point <= 0xffff ? 3 : 4
    if (bytes + size > maxBytes && current) { chunks.push(current); current = ''; bytes = 0 }
    current += character
    bytes += size
  }
  if (current) chunks.push(current)
  return chunks
}

function fontFamily() { return resolveVariable('--font-mono') || 'monospace' }

function terminalTheme() {
  return {
    background: cssColor('--background', '#000000'), foreground: cssColor('--foreground', '#ffffff'),
    cursor: cssColor('--accent', '#ffffff'), selectionBackground: cssColor('--surface-selected', '#555555'),
    black: cssColor('--surface', '#000000'), brightBlack: cssColor('--muted', '#888888'),
    white: cssColor('--foreground', '#ffffff'), brightWhite: cssColor('--foreground', '#ffffff'),
    green: cssColor('--success', '#00aa00'), red: cssColor('--danger', '#cc3333'), yellow: cssColor('--warning', '#ccaa00'),
  }
}

function cssColor(role: string, fallback: string) {
  const raw = resolveVariable(role)
  if (!raw) return fallback
  const rgb = parseRgb(raw) ?? parseOklch(raw)
  return rgb ? `#${rgb.map(channel => channel.toString(16).padStart(2, '0')).join('')}` : fallback
}

function resolveVariable(role: string): string {
  let value = getComputedStyle(document.documentElement).getPropertyValue(role).trim()
  for (let i = 0; i < 8 && value.startsWith('var('); i++) {
    const match = value.match(/^var\(\s*(--[\w-]+)\s*(?:,\s*([^)]*))?\)$/)
    if (!match) break
    value = getComputedStyle(document.documentElement).getPropertyValue(match[1]).trim() || match[2]?.trim() || ''
  }
  return value
}

function parseRgb(value: string): [number, number, number] | null {
  const hex = value.match(/^#([\da-f]{3}|[\da-f]{6})$/i)?.[1]
  if (hex) {
    const expanded = hex.length === 3 ? [...hex].map(char => char + char).join('') : hex
    return [0, 2, 4].map(index => Number.parseInt(expanded.slice(index, index + 2), 16)) as [number, number, number]
  }
  const rgb = value.match(/^rgba?\(\s*([\d.]+)%?[,\s]+([\d.]+)%?[,\s]+([\d.]+)%?/i)
  if (rgb) {
    const percentages = /^rgba?\(\s*[\d.]+%[,\s]+[\d.]+%[,\s]+[\d.]+%/i.test(value)
    const channels = rgb.slice(1, 4).map(Number)
    return channels.map(channel => clampByte(percentages ? channel * 2.55 : channel)) as [number, number, number]
  }
  return null
}

function parseOklch(value: string): [number, number, number] | null {
  const match = value.match(/^oklch\(\s*([\d.]+)(%)?\s+([\d.]+)\s+([\d.]+)(?:deg)?(?:\s*\/[^)]*)?\)$/i)
  if (!match) return null
  const lightness = Number(match[1]) / (match[2] ? 100 : 1)
  const chroma = Number(match[3])
  const angle = Number(match[4]) * Math.PI / 180
  const a = chroma * Math.cos(angle)
  const b = chroma * Math.sin(angle)
  const l1 = lightness + 0.3963377774 * a + 0.2158037573 * b
  const m1 = lightness - 0.1055613458 * a - 0.0638541728 * b
  const s1 = lightness - 0.0894841775 * a - 1.291485548 * b
  const l = l1 ** 3, m = m1 ** 3, s = s1 ** 3
  const linear = [
    4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
    -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
    -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s,
  ]
  return linear.map(channel => clampByte(255 * gamma(channel))) as [number, number, number]
}

function gamma(channel: number) { return channel <= 0.0031308 ? 12.92 * channel : 1.055 * channel ** (1 / 2.4) - 0.055 }
function clampByte(channel: number) { return Math.round(Math.max(0, Math.min(255, channel))) }
function errorText(reason: unknown) { return reason instanceof Error ? reason.message : String(reason) }
