import { useLayoutEffect, useRef } from 'react'

export interface ThemeAccentPalette {
  accent: string
  accentForeground: string
}

type InlineProperty = { value: string; priority: string }

function readInlineProperty(name: string): InlineProperty {
  const style = document.documentElement.style
  return { value: style.getPropertyValue(name), priority: style.getPropertyPriority(name) }
}

function restoreInlineProperty(name: string, previous: InlineProperty) {
  if (previous.value) {
    document.documentElement.style.setProperty(name, previous.value, previous.priority)
  } else {
    document.documentElement.style.removeProperty(name)
  }
}

export function useThemeAccent(palette: ThemeAccentPalette) {
  const previous = useRef<{ accent: InlineProperty; accentForeground: InlineProperty } | null>(null)

  useLayoutEffect(() => {
    if (!previous.current) {
      previous.current = {
        accent: readInlineProperty('--accent'),
        accentForeground: readInlineProperty('--accent-foreground'),
      }
    }

    return () => {
      if (previous.current) {
        restoreInlineProperty('--accent', previous.current.accent)
        restoreInlineProperty('--accent-foreground', previous.current.accentForeground)
      }
    }
  }, [])

  useLayoutEffect(() => {
    document.documentElement.style.setProperty('--accent', palette.accent)
    document.documentElement.style.setProperty('--accent-foreground', palette.accentForeground)
  }, [palette.accent, palette.accentForeground])
}
