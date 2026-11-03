import { useState } from 'react'

const previewPath = `${import.meta.env.BASE_URL}app-preview.html`

export function WorkflowTour() {
  const [previewVersion, setPreviewVersion] = useState(0)

  return (
    <section className="workflow-tour" aria-label="Brr desktop app preview">
      <div className="app-preview-frame" role="region" aria-label="Desktop app preview" tabIndex={0}>
        <iframe
          key={previewVersion}
          src={previewPath}
          title="Brr app preview"
          loading="lazy"
          sandbox="allow-scripts allow-same-origin"
        />
      </div>
      <div className="app-preview-actions">
        <p className="app-preview-caption">Actual Brr interface · Sample workspace. No agent is running.</p>
        <div className="app-preview-links">
          <button type="button" onClick={() => setPreviewVersion((current) => current + 1)}>
            Reset preview
          </button>
          <a href={previewPath} target="_blank" rel="noreferrer">Open full preview</a>
        </div>
      </div>
      <p className="app-preview-mobile-note">
        This is Brr’s desktop interface. On a narrow screen, scroll the preview horizontally to inspect it.
      </p>
    </section>
  )
}
