import { MoreHorizontal } from 'lucide-react'
import { createPortal } from 'react-dom'
import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState } from 'react'
import './ProjectMenu.css'

export type ProjectMenuAction = {
  id: string
  label: string
  onSelect: () => void | Promise<void>
  destructive?: boolean
  disabled?: boolean
  restoreFocus?: boolean
}

export type ProjectMenuProps = {
  projectName: string
  projectId: string
  actions: ProjectMenuAction[]
  disabled?: boolean
}

type Position = { top: number; left: number }

export function ProjectMenu({ projectName, projectId, actions, disabled = false }: ProjectMenuProps) {
  const menuId = `project-menu-${useId()}`
  const triggerRef = useRef<HTMLButtonElement>(null)
  const menuRef = useRef<HTMLDivElement>(null)
  const actionRefs = useRef<Array<HTMLButtonElement | null>>([])
  const busyRef = useRef(false)
  const openRef = useRef(false)
  const [open, setOpen] = useState(false)
  const [position, setPosition] = useState<Position>({ top: 8, left: 8 })
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const setMenuOpen = useCallback((next: boolean) => {
    openRef.current = next
    setOpen(next)
  }, [])

  const positionMenu = useCallback(() => {
    const rect = triggerRef.current?.getBoundingClientRect()
    if (!rect) return
    const viewportWidth = window.innerWidth
    const viewportHeight = window.innerHeight
    const menu = menuRef.current
    const width = Math.min(menu?.offsetWidth || 216, Math.max(0, viewportWidth - 16))
    const height = menu?.offsetHeight || Math.min(actions.length * 36 + 20, viewportHeight - 16)
    const left = Math.min(Math.max(8, rect.right - width), Math.max(8, viewportWidth - width - 8))
    const below = rect.bottom + 4
    const top = below + height <= viewportHeight - 8
      ? below
      : Math.max(8, rect.top - height - 4)
    setPosition(current => current.top === top && current.left === left ? current : { top, left })
  }, [actions.length])

  const openMenu = (focus: 'first' | 'last' | null = null) => {
    setError(null)
    setMenuOpen(true)
    requestAnimationFrame(() => {
      positionMenu()
      if (focus) {
        const enabled = actions.map((action, index) => action.disabled ? -1 : index).filter(index => index >= 0)
        const index = focus === 'first' ? enabled[0] : enabled.at(-1)
        if (index !== undefined) actionRefs.current[index]?.focus()
      }
    })
  }

  const closeMenu = (restoreFocus = false) => {
    setMenuOpen(false)
    if (restoreFocus) requestAnimationFrame(() => triggerRef.current?.focus())
  }

  useEffect(() => {
    if (!open) return
    positionMenu()
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Node
      if (triggerRef.current?.contains(target) || menuRef.current?.contains(target)) return
      closeMenu()
    }
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        event.preventDefault()
        closeMenu(true)
      } else if (event.key === 'Tab') {
        const target = event.target as Node
        const fromMenu = menuRef.current?.contains(target) ?? false
        const fromTrigger = triggerRef.current?.contains(target) ?? false
        if (fromMenu || fromTrigger) {
          const trigger = triggerRef.current
          const focusable = Array.from(document.querySelectorAll<HTMLElement>(
            'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])'
          )).filter(element => !menuRef.current?.contains(element) && !element.closest('[hidden], [aria-hidden="true"]'))
          const index = trigger ? focusable.indexOf(trigger) : -1
          const next = focusable[index + (event.shiftKey ? -1 : 1)]
          if (next) {
            event.preventDefault()
            closeMenu()
            next.focus()
          } else {
            closeMenu()
          }
        } else {
          closeMenu()
        }
      }
    }
    const dismiss = () => closeMenu()
    const dismissOnScroll = (event: Event) => {
      if (event.target instanceof Node && menuRef.current?.contains(event.target)) return
      dismiss()
    }
    document.addEventListener('pointerdown', onPointerDown)
    document.addEventListener('keydown', onKeyDown)
    window.addEventListener('resize', dismiss)
    window.addEventListener('scroll', dismissOnScroll, true)
    return () => {
      document.removeEventListener('pointerdown', onPointerDown)
      document.removeEventListener('keydown', onKeyDown)
      window.removeEventListener('resize', dismiss)
      window.removeEventListener('scroll', dismissOnScroll, true)
    }
  }, [open, positionMenu])

  const activate = async (action: ProjectMenuAction) => {
    if (action.disabled || busyRef.current) return
    busyRef.current = true
    setBusy(true)
    setError(null)
    try {
      await action.onSelect()
      closeMenu(action.restoreFocus !== false)
    } catch (reason) {
      if (openRef.current) {
        setError(reason instanceof Error ? reason.message : String(reason))
        requestAnimationFrame(positionMenu)
      }
    } finally {
      busyRef.current = false
      setBusy(false)
    }
  }

  const handleTriggerKeyDown = (event: React.KeyboardEvent<HTMLButtonElement>) => {
    if (event.key === 'ArrowDown') {
      event.preventDefault()
      openMenu('first')
    } else if (event.key === 'ArrowUp') {
      event.preventDefault()
      openMenu('last')
    }
  }

  const handleItemKeyDown = (event: React.KeyboardEvent<HTMLButtonElement>, index: number) => {
    if (event.key === 'Home' || event.key === 'End') {
      event.preventDefault()
      const enabled = actions.map((action, itemIndex) => action.disabled ? -1 : itemIndex).filter(itemIndex => itemIndex >= 0)
      const next = event.key === 'Home' ? enabled[0] : enabled.at(-1)
      if (next !== undefined) actionRefs.current[next]?.focus()
      return
    }
    if (event.key !== 'ArrowDown' && event.key !== 'ArrowUp') return
    event.preventDefault()
    const direction = event.key === 'ArrowDown' ? 1 : -1
    for (let offset = 1; offset <= actions.length; offset += 1) {
      const next = (index + direction * offset + actions.length) % actions.length
      if (!actions[next].disabled) {
        actionRefs.current[next]?.focus()
        break
      }
    }
  }

  useLayoutEffect(() => {
    if (open) positionMenu()
  }, [open, error, positionMenu])

  useEffect(() => {
    if (!open || !error) return
    const frame = requestAnimationFrame(positionMenu)
    return () => cancelAnimationFrame(frame)
  }, [open, error, positionMenu])

  return <>
    <button
      ref={triggerRef}
      type="button"
      className="project-menu-trigger"
      aria-label={`Manage project ${projectName}`}
      title={`Manage project ${projectName}`}
      aria-haspopup="menu"
      aria-expanded={open}
      aria-controls={menuId}
      data-project-id={projectId}
      disabled={disabled}
      onKeyDown={handleTriggerKeyDown}
      onClick={event => open ? closeMenu() : openMenu(event.detail === 0 ? 'first' : null)}
    >
      <MoreHorizontal size={16} aria-hidden="true" />
    </button>
    {open && createPortal(<div
      ref={menuRef}
      id={menuId}
      className="project-menu"
      role="menu"
      aria-label={`${projectName} actions`}
      aria-busy={busy}
      data-project-id={projectId}
      style={{ top: position.top, left: position.left, maxWidth: 'calc(100vw - 16px)' }}
    >
      {actions.map((action, index) => <button
        key={action.id}
        ref={node => { actionRefs.current[index] = node }}
        type="button"
        role="menuitem"
        className={`project-menu-item${action.destructive ? ' is-destructive' : ''}`}
        disabled={disabled || busy || action.disabled}
        onClick={() => void activate(action)}
        onKeyDown={event => handleItemKeyDown(event, index)}
      >{action.label}</button>)}
      {error && <div className="project-menu-error" role="alert">{error}</div>}
    </div>, document.body)}
  </>
}
