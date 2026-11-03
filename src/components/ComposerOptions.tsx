import { useCallback, useEffect, useRef, useState, type KeyboardEvent, type ReactNode } from 'react'
import { createPortal } from 'react-dom'
import { SlidersHorizontal, X } from 'lucide-react'
import { Button } from './ui/button'
import './ComposerOptions.css'

export type ComposerOptionsProps = {
  children: ReactNode
  summary?: string
  title?: string
}

const focusableSelector = 'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"]):not([disabled])'

export function ComposerOptions({ children, summary, title }: ComposerOptionsProps) {
  const triggerRef = useRef<HTMLButtonElement>(null)
  const panelRef = useRef<HTMLDivElement>(null)
  const focusOnOpen = useRef(false)
  const [open, setOpen] = useState(false)
  const [position, setPosition] = useState({ top: 0, left: 0 })

  const placePanel = useCallback(() => {
    const trigger = triggerRef.current
    const panel = panelRef.current
    if (!trigger || !panel) return
    const rect = trigger.getBoundingClientRect()
    const panelRect = panel.getBoundingClientRect()
    const gutter = 8
    const width = Math.min(panelRect.width, Math.max(0, window.innerWidth - gutter * 2))
    const left = Math.min(Math.max(gutter, rect.left), Math.max(gutter, window.innerWidth - width - gutter))
    const below = window.innerHeight - rect.bottom - gutter
    const top = below >= Math.min(panelRect.height, window.innerHeight - gutter * 2)
      ? rect.bottom + 8
      : Math.max(gutter, rect.top - panelRect.height - 8)
    setPosition({ top: Math.min(top, Math.max(gutter, window.innerHeight - gutter - panelRect.height)), left })
  }, [])

  useEffect(() => {
    if (!open) return
    if (focusOnOpen.current) {
      panelRef.current?.querySelector<HTMLElement>(`.composer-options-content ${focusableSelector}`)?.focus()
      focusOnOpen.current = false
    }
    placePanel()
    const resize = () => setOpen(false)
    const scroll = (event: Event) => {
      if (event.target instanceof Node && panelRef.current?.contains(event.target)) return
      setOpen(false)
    }
    const focusin = (event: FocusEvent) => {
      const target = event.target
      if (!(target instanceof Node) || (!panelRef.current?.contains(target) && !triggerRef.current?.contains(target))) setOpen(false)
    }
    const pointerdown = (event: PointerEvent) => {
      const target = event.target
      if (!(target instanceof Node) || (!panelRef.current?.contains(target) && !triggerRef.current?.contains(target))) setOpen(false)
    }
    const escape = (event: globalThis.KeyboardEvent) => {
      if (event.key !== 'Escape') return
      event.preventDefault()
      setOpen(false)
      triggerRef.current?.focus()
    }
    window.addEventListener('resize', resize)
    window.addEventListener('scroll', scroll, true)
    document.addEventListener('pointerdown', pointerdown)
    document.addEventListener('focusin', focusin)
    document.addEventListener('keydown', escape)
    return () => {
      window.removeEventListener('resize', resize)
      window.removeEventListener('scroll', scroll, true)
      document.removeEventListener('pointerdown', pointerdown)
      document.removeEventListener('focusin', focusin)
      document.removeEventListener('keydown', escape)
    }
  }, [open, placePanel])

  const openOptions = (keyboard: boolean) => {
    focusOnOpen.current = keyboard
    setOpen(true)
  }

  const handlePanelKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key !== 'Tab') return
    const fields = Array.from(panelRef.current?.querySelectorAll<HTMLElement>(`.composer-options-content ${focusableSelector}`) ?? [])
    const active = document.activeElement
    if (!event.shiftKey && active === panelRef.current?.querySelector('[aria-label="Close chat options"]')) {
      const all = Array.from(document.querySelectorAll<HTMLElement>(focusableSelector))
      const triggerIndex = all.indexOf(triggerRef.current!)
      const visibleOutside = all.filter(node => {
        if (panelRef.current?.contains(node)) return false
        if (node.closest('[hidden], [aria-hidden="true"]')) return false
        const style = window.getComputedStyle(node)
        return style.display !== 'none' && style.visibility !== 'hidden'
      })
      const after = all.slice(triggerIndex + 1).find(node => visibleOutside.includes(node))
      const next = after ?? visibleOutside[0]
      event.preventDefault()
      if (next) next.focus()
      setOpen(false)
      return
    }
    if (fields.length === 0) return
    if (event.shiftKey && active === panelRef.current?.querySelector('[aria-label="Close chat options"]')) {
      event.preventDefault()
      fields[fields.length - 1]?.focus()
      return
    }
    if (event.shiftKey && active === fields[0]) {
      event.preventDefault()
      triggerRef.current?.focus()
    } else if (!event.shiftKey && active === fields[fields.length - 1]) {
      const close = panelRef.current?.querySelector<HTMLElement>('[aria-label="Close chat options"]')
      if (close) {
        event.preventDefault()
        close.focus()
      }
    }
  }

  return <>
    <Button
      ref={triggerRef}
      type="button"
      variant="ghost"
      size="icon"
      className="composer-options-trigger"
      aria-label="Chat options"
      aria-expanded={open}
      aria-haspopup="dialog"
      title={title}
      onClick={event => open ? setOpen(false) : openOptions(event.detail === 0)}
      onKeyDown={event => {
        if (!open || event.key !== 'Tab' || event.shiftKey) return
        const first = panelRef.current?.querySelector<HTMLElement>(`.composer-options-content ${focusableSelector}`)
        if (first) {
          event.preventDefault()
          first.focus()
        }
      }}
    >
      <SlidersHorizontal size={16} aria-hidden="true" />
      {summary && <span className="composer-options-summary">{summary}</span>}
    </Button>
    {open && createPortal(<div
      ref={panelRef}
      className="composer-options-panel"
      role="dialog"
      aria-label="Chat options"
      style={{ top: position.top, left: position.left }}
      onKeyDown={handlePanelKeyDown}
    >
      <div className="composer-options-header">
        <strong>Chat options</strong>
        <Button type="button" variant="ghost" size="icon" aria-label="Close chat options" onClick={() => {
          setOpen(false)
          triggerRef.current?.focus()
        }}><X size={16} aria-hidden="true" /></Button>
      </div>
      <div className="composer-options-content">{children}</div>
    </div>, document.body)}
  </>
}
