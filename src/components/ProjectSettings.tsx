import type { Model, ProjectRecord } from '../lib/client'
import { ProjectDialog } from './ProjectDialog'

/** Compatibility wrapper for callers still using the project-settings entry point. */
export function ProjectSettings({project, models, onSave, onClose, onChooseFolders}: {
  project: ProjectRecord
  models: Model[]
  onSave: (project: ProjectRecord) => Promise<void>
  onClose: () => void
  onChooseFolders?: () => Promise<string[] | null>
}) {
  return <ProjectDialog project={project} models={models} onSave={value => onSave(value as ProjectRecord)} onChooseFolders={onChooseFolders ?? (async () => null)} onClose={onClose} />
}
