import { forwardRef, useEffect, useId, useRef, useState } from 'react'
import { Button, type ButtonProps } from './ui/button'

export type RailButtonProps = Omit<ButtonProps, 'asChild' | 'aria-label' | 'title'> & { label: string }

export const RailButton = forwardRef<HTMLButtonElement, RailButtonProps>(({
  label,
  children,
  onKeyDown,
  onFocus,
  onBlur,
  'aria-describedby': describedBy,
  ...props
}, ref) => {
  const tooltipId = 'rail-tooltip-' + useId()
  const [open, setOpen] = useState(false)
  const hovered = useRef(false)
  const focused = useRef(false)
  const dismissed = useRef(false)
  const ariaDescribedBy = open
    ? [describedBy, tooltipId].filter(Boolean).join(' ')
    : describedBy

  useEffect(() => {
    if (!open) return
    const dismissOnEscape = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return
      dismissed.current = true
      setOpen(false)
    }
    document.addEventListener('keydown', dismissOnEscape)
    return () => document.removeEventListener('keydown', dismissOnEscape)
  }, [open])

  return <span
    className="rail-control"
    onMouseEnter={() => {
      hovered.current = true
      dismissed.current = false
      setOpen(true)
    }}
    onMouseLeave={() => {
      hovered.current = false
      setOpen(focused.current && !dismissed.current)
    }}
  >
    <Button
      ref={ref}
      type="button"
      variant="ghost"
      size="icon"
      aria-label={label}
      aria-describedby={ariaDescribedBy}
      {...props}
      onFocus={event => {
        focused.current = true
        dismissed.current = false
        setOpen(true)
        onFocus?.(event)
      }}
      onBlur={event => {
        focused.current = false
        setOpen(hovered.current && !dismissed.current)
        onBlur?.(event)
      }}
      onKeyDown={event => {
        onKeyDown?.(event)
      }}
    >{children}</Button>
    {open && <span id={tooltipId} className="rail-tooltip" role="tooltip">{label}</span>}
  </span>
})

RailButton.displayName = 'RailButton'
