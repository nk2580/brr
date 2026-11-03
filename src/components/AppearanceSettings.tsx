import type { CSSProperties } from 'react'
import { Check, X } from 'lucide-react'
import { DEFAULT_SIDEBAR_COLOR, SIDEBAR_COLORS, type SidebarColorId } from '../lib/appearance'
import { Button } from './ui/button'
import './AppearanceSettings.css'

type AppearanceSettingsProps = {
  value: SidebarColorId
  onChange: (id: SidebarColorId) => void
  onClose: () => void
  saveFailed?: boolean
}

export function AppearanceSettings({ value, onChange, onClose, saveFailed = false }: AppearanceSettingsProps) {
  return (
    <section className="appearance-settings" aria-label="Application settings">
      <header className="appearance-settings-header">
        <h2>Appearance</h2>
        <Button type="button" variant="ghost" size="icon" aria-label="Close settings" onClick={onClose}><X size={16} aria-hidden="true" /></Button>
      </header>
      <p className="appearance-settings-copy">Choose a theme color for the rail, top bar and app accents. Panels keep their dark appearance. Your selection is saved on this device.</p>
      <fieldset>
        <legend>Theme color</legend>
        <div className="appearance-color-options">
          {SIDEBAR_COLORS.map(color => (
            <label
              className={`appearance-color-option${value === color.id ? ' selected' : ''}`}
              data-state={value === color.id ? 'selected' : 'unselected'}
              key={color.id}
              style={{ '--swatch-color': color.background, '--swatch-foreground': color.foreground } as CSSProperties}
            >
              <input
                type="radio"
                name="rail-color"
                value={color.id}
                aria-label={color.name}
                checked={value === color.id}
                onChange={() => onChange(color.id)}
              />
              <span className="appearance-color-swatch" aria-hidden="true">{value === color.id && <Check size={14} aria-hidden="true" />}</span>
              <span className="appearance-color-name">{color.name}</span>
              {color.id === DEFAULT_SIDEBAR_COLOR && <span className="appearance-color-default">default</span>}
            </label>
          ))}
        </div>
      </fieldset>
      {saveFailed && <p className="appearance-save-status" role="status">Color changed for this session. Could not save your preference.</p>}
    </section>
  )
}
