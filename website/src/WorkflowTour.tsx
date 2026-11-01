import { useLayoutEffect, useRef, useState, type KeyboardEvent } from 'react'
import { ArrowRight, FileText, Hammer, Search, Send, Square, Sparkles, Play } from 'lucide-react'

type StoryKey = 'writing' | 'research' | 'making'
type Stage = 'plan' | 'steer' | 'review' | 'resume'

const stories: Record<StoryKey, {
  label: string
  icon: typeof FileText
  project: string
  task: string
  plan: string[]
  before: string
  after: string
  command?: string
}> = {
  writing: {
    label: 'Writing',
    icon: FileText,
    project: 'Workshop outline',
    task: 'Turn my notes into a friendly workshop outline. Show me a plan first.',
    plan: [
      'Group the notes into a clear workshop flow',
      'Draft friendly section headings and short prompts',
      'Save an outline beside the original notes for review',
    ],
    before: 'Workshop notes\n- welcome\n- activity ideas\n- wrap up',
    after: 'Friendly workshop outline\n1. Welcome and settle in\n2. Explore an idea together\n3. Wrap up and reflect',
  },
  research: {
    label: 'Research',
    icon: Search,
    project: 'Project notes',
    task: 'Compare two local project notes and make a concise evidence list.',
    plan: [
      'Read the two notes in this example project',
      'List the points they share and where they differ',
      'Prepare a short evidence list with source names',
    ],
    before: 'project-alpha.md\n- Keep the first release small\n\nproject-beta.md\n- Start with a narrow first release',
    after: 'Shared direction\n+ Keep the first release narrow\n\nOpen question\n+ Which workflow should be included first?\n\nSources: project-alpha.md, project-beta.md',
  },
  making: {
    label: 'Making',
    icon: Hammer,
    project: 'Local README',
    task: 'Tidy the setup section in this local project README and show me the change.',
    plan: [
      'Read the existing setup notes in the example README',
      'Rewrite them as short, ordered steps',
      'Show the proposed change and a relevant local check',
    ],
    before: '## Setup\nInstall stuff and then run it.',
    after: '## Setup\n1. Install dependencies with pnpm install.\n2. Start the local app with pnpm dev.',
    command: 'Example command: pnpm test',
  },
}

const stages: { id: Stage; label: string }[] = [
  { id: 'plan', label: 'Plan' },
  { id: 'steer', label: 'Steer' },
  { id: 'review', label: 'Review' },
  { id: 'resume', label: 'Resume' },
]

export function WorkflowTour() {
  const [storyKey, setStoryKey] = useState<StoryKey>('writing')
  const [stage, setStage] = useState<Stage>('plan')
  const [instruction, setInstruction] = useState('')
  const [sentInstructions, setSentInstructions] = useState<string[]>([])
  const [stopped, setStopped] = useState(false)
  const [feedback, setFeedback] = useState('')
  const tabRefs = useRef<Array<HTMLButtonElement | null>>([])
  const transcriptRef = useRef<HTMLDivElement>(null)
  const story = stories[storyKey]

  useLayoutEffect(() => {
    if (transcriptRef.current) transcriptRef.current.scrollTop = 0
  }, [stage, storyKey])

  function selectStory(key: StoryKey) {
    setStoryKey(key)
    setStage('plan')
    setInstruction('')
    setSentInstructions([])
    setStopped(false)
    setFeedback('')
  }

  function selectStage(next: Stage) {
    setStage(next)
    setFeedback('')
  }

  function onTabKeyDown(event: KeyboardEvent<HTMLButtonElement>, index: number) {
    let nextIndex: number | undefined
    if (event.key === 'ArrowRight') nextIndex = (index + 1) % stages.length
    if (event.key === 'ArrowLeft') nextIndex = (index + stages.length - 1) % stages.length
    if (event.key === 'Home') nextIndex = 0
    if (event.key === 'End') nextIndex = stages.length - 1
    if (nextIndex === undefined) return
    event.preventDefault()
    const next = stages[nextIndex]
    selectStage(next.id)
    tabRefs.current[nextIndex]?.focus()
  }

  function sendInstruction() {
    if (!instruction.trim() || stopped) return
    setSentInstructions((current) => [...current, instruction])
    setInstruction('')
    setFeedback('Instruction added to this example conversation.')
  }

  function stopDemo() {
    setStopped(true)
    setFeedback('Demo stopped. Your instruction is still here.')
  }

  function continueDemo() {
    setStopped(false)
    setFeedback('Demo continued.')
  }

  function returnToPlan() {
    setStage('plan')
    setInstruction('')
    setSentInstructions([])
    setStopped(false)
    setFeedback('Returned to the plan.')
  }

  return (
    <section className="workflow-tour" aria-label="Interactive Brr workflow tour">
      <div className="tour-window">
        <header className="tour-topbar">
          <span className="window-dots" aria-hidden="true"><i /><i /><i /></span>
          <span className="tour-window-title"><Sparkles size={15} aria-hidden="true" /> Brr <span>/</span> {story.project}</span>
          <span className="tour-example-badge">LOCAL EXAMPLE</span>
        </header>

        <div className="tour-workspace">
          <aside className="tour-sidebar" aria-label="Example projects">
            <p className="tour-sidebar-label">TRY A WORKFLOW</p>
            <div className="tour-projects">
              {(Object.entries(stories) as [StoryKey, typeof stories[StoryKey]][]).map(([key, item]) => {
                const Icon = item.icon
                return (
                  <button
                    className={`tour-project-button${storyKey === key ? ' is-active' : ''}`}
                    type="button"
                    key={key}
                    aria-pressed={storyKey === key}
                    onClick={() => selectStory(key)}
                  >
                    <Icon size={16} aria-hidden="true" />
                    <span>{item.label}</span>
                    {storyKey === key && <span className="tour-project-dot" aria-hidden="true" />}
                  </button>
                )
              })}
            </div>
            <p className="tour-sidebar-note">A few ways to work with a project on your computer.</p>
          </aside>

          <div className="tour-main">
            <div className="tour-stages" role="tablist" aria-label="Workflow stages">
              {stages.map((item, index) => (
                <button
                  key={item.id}
                  ref={(element) => { tabRefs.current[index] = element }}
                  className={`tour-stage-tab${stage === item.id ? ' is-active' : ''}`}
                  id={`tour-tab-${item.id}`}
                  type="button"
                  role="tab"
                  aria-selected={stage === item.id}
                  aria-controls="tour-stage-panel"
                  tabIndex={stage === item.id ? 0 : -1}
                  onClick={() => selectStage(item.id)}
                  onKeyDown={(event) => onTabKeyDown(event, index)}
                >
                  <span className="tour-stage-number" aria-hidden="true">0{index + 1}</span>{item.label}
                </button>
              ))}
            </div>

            <div
              ref={transcriptRef}
              className="tour-transcript"
              id="tour-stage-panel"
              role="tabpanel"
              aria-labelledby={`tour-tab-${stage}`}
              tabIndex={0}
            >
              {stage === 'plan' && (
                <>
                  <div className="tour-message-user"><span className="tour-message-label">YOU ASKED</span><p>{story.task}</p></div>
                  <div className="tour-message-agent">
                    <span className="tour-message-label"><Sparkles size={13} aria-hidden="true" /> Brr made a plan</span>
                    <div className="tour-plan">
                      <p>Here is a small plan for this local example. You can review it before anything moves ahead.</p>
                      <ol className="tour-plan-steps">
                        {story.plan.map((step) => <li key={step}>{step}</li>)}
                      </ol>
                      <button className="tour-primary-button" type="button" onClick={() => selectStage('steer')}>
                        Implement plan <ArrowRight size={15} aria-hidden="true" />
                      </button>
                    </div>
                  </div>
                </>
              )}

              {stage === 'steer' && (
                <>
                  <div className="tour-message-user"><span className="tour-message-label">YOU ASKED</span><p>{story.task}</p></div>
                  <div className="tour-message-agent">
                    <span className="tour-message-label"><Sparkles size={13} aria-hidden="true" /> Working through your plan</span>
                    <p>This tour shows how you can add a direction while work is in progress.</p>
                  </div>
                  {sentInstructions.map((text, index) => (
                    <div className="tour-message-user tour-sent-instruction" key={`${index}-${text}`}><span className="tour-message-label">YOUR INSTRUCTION</span><p>{text}</p></div>
                  ))}
                  {stopped && <p className="tour-stopped-note">The example is paused. Your conversation stays visible.</p>}
                  <div className="tour-composer">
                    <label htmlFor="tour-instruction">Steering instruction</label>
                    <textarea
                      id="tour-instruction"
                      maxLength={240}
                      rows={2}
                      value={instruction}
                      onChange={(event) => setInstruction(event.target.value)}
                      placeholder="For example: Keep the opening warm and concise."
                    />
                    <div className="tour-composer-actions">
                      <span>{instruction.length}/240</span>
                      <button className="tour-primary-button" type="button" onClick={sendInstruction} disabled={!instruction.trim() || stopped}>
                        <Send size={14} aria-hidden="true" /> Send instruction
                      </button>
                    </div>
                  </div>
                  <div className="tour-demo-controls">
                    {stopped
                      ? <button className="tour-text-button" type="button" onClick={continueDemo}><Play size={14} aria-hidden="true" /> Continue demo</button>
                      : <button className="tour-text-button" type="button" onClick={stopDemo}><Square size={13} aria-hidden="true" /> Stop demo</button>}
                  </div>
                </>
              )}

              {stage === 'review' && (
                <div className="tour-review">
                  <span className="tour-message-label"><FileText size={13} aria-hidden="true" /> PROPOSED CHANGE</span>
                  <p className="tour-review-intro">Here is an example of a change you can read through before deciding what to keep.</p>
                  <pre className="tour-diff" aria-label="Illustrative changes"><span className="tour-diff-removed">- {story.before.replaceAll('\n', '\n- ')}</span>{'\n'}<span className="tour-diff-added">+ {story.after.replaceAll('\n', '\n+ ')}</span></pre>
                  {story.command && <p className="tour-command-summary">{story.command}. This example does not run it.</p>}
                  <p>Read through the changes and keep or change whatever you like.</p>
                </div>
              )}

              {stage === 'resume' && (
                <div className="tour-resume">
                  <span className="tour-message-label"><Play size={13} aria-hidden="true" /> EXAMPLE TRANSCRIPT FROM AN EARLIER SESSION</span>
                  <div className="tour-message-user"><span className="tour-message-label">YOU ASKED</span><p>Organise my local project notes into a short outline.</p></div>
                  <div className="tour-message-agent"><span className="tour-message-label">BRR</span><p>I grouped the notes, drafted an outline, and left it ready for your review.</p></div>
                  <p className="tour-resume-explainer">A real session can be picked up again with its conversation and project context.</p>
                  <button className="tour-text-button" type="button" onClick={returnToPlan}>Return to plan <ArrowRight size={14} aria-hidden="true" /></button>
                </div>
              )}
            </div>

            <div className="tour-feedback" aria-live="polite" role="status">{feedback || 'Choose a stage to explore how Brr works.'}</div>
          </div>
        </div>
      </div>
      <p className="tour-caption">Interactive tour · illustrative example. No agent is running.</p>
    </section>
  )
}
