import { useRef, useState, type KeyboardEvent } from 'react'
import { ArrowDown, ArrowRight, Check, ChevronDown, Copy, Download, FolderOpen, Puzzle, ShieldCheck, SlidersHorizontal } from 'lucide-react'
import { WorkflowTour } from './WorkflowTour'

const commands = {
  develop: 'pnpm install\npnpm desktop:dev',
  package: 'pnpm install\nAPPLE_SIGNING_IDENTITY=- pnpm desktop:build',
}
type BuildMode = keyof typeof commands

function BuildGuide() {
  const [mode, setMode] = useState<BuildMode>('develop')
  const [feedback, setFeedback] = useState('')
  const [copying, setCopying] = useState(false)
  const copyAttempt = useRef(0)
  const tabs = useRef<Array<HTMLButtonElement | null>>([])
  const modes: BuildMode[] = ['develop', 'package']

  function chooseMode(next: BuildMode) {
    copyAttempt.current += 1
    setMode(next)
    setFeedback('')
    setCopying(false)
  }

  function onKeyDown(event: KeyboardEvent<HTMLButtonElement>, index: number) {
    const next = event.key === 'Home' ? 0 : event.key === 'End' ? 1
      : event.key === 'ArrowRight' || event.key === 'ArrowLeft' ? 1 - index : undefined
    if (next === undefined) return
    event.preventDefault()
    chooseMode(modes[next])
    tabs.current[next]?.focus()
  }

  async function copyCommands() {
    const attempt = ++copyAttempt.current
    setCopying(true)
    setFeedback('')
    try {
      await navigator.clipboard.writeText(commands[mode])
      if (attempt === copyAttempt.current) setFeedback('Commands copied.')
    } catch {
      if (attempt === copyAttempt.current) setFeedback('Could not copy. Select the commands below and copy them manually.')
    } finally {
      if (attempt === copyAttempt.current) setCopying(false)
    }
  }

  return <section className="build-section section-shell" id="get-brr" aria-labelledby="build-heading">
    <div className="build-intro">
      <span className="eyebrow">GET BRR / EARLY DESKTOP BUILD</span>
      <h2 id="build-heading">Start something.</h2>
      <p>Brr is taking shape in the open. The macOS app is ready to build from source, and there is plenty of room to make it your own.</p>
      <a className="button button-light" href="https://github.com/nk2580/brr/releases/download/v0.1.0/brr-source.tar.gz"><Download size={17} aria-hidden="true" /> Download source</a>
      <p className="build-availability">Source archive · Apache 2.0<br />A public, notarized app download is not available yet.</p>
    </div>
    <div className="build-guide">
      <p className="build-step-label">FROM AN EXTRACTED BRR SOURCE FOLDER</p>
      <p>Install Node.js 22+, pnpm 10, stable Rust and the <a href="https://v2.tauri.app/start/prerequisites/" target="_blank" rel="noreferrer">macOS Tauri prerequisites<span className="sr-only"> (opens in a new tab)</span></a>.</p>
      <p>New conversations currently need a Git project with a commit and <a href="https://worktrunk.dev/" target="_blank" rel="noreferrer">Worktrunk<span className="sr-only"> (opens in a new tab)</span></a> available as <code>wt</code>.</p>
      <div className="build-tabs" role="tablist" aria-label="Build options">
        {modes.map((value, index) => <button key={value} ref={element => { tabs.current[index] = element }} type="button" role="tab" id={`build-tab-${value}`} aria-controls="build-command-panel" aria-selected={mode === value} tabIndex={mode === value ? 0 : -1} onClick={() => chooseMode(value)} onKeyDown={event => onKeyDown(event, index)}>{value === 'develop' ? 'Run in development' : 'Package the app'}</button>)}
      </div>
      <div className="build-command-panel" role="tabpanel" id="build-command-panel" aria-labelledby={`build-tab-${mode}`} tabIndex={0}>
        <div className="code-bar"><span>Terminal</span><button className="copy-button" type="button" onClick={() => void copyCommands()} disabled={copying}>{feedback === 'Commands copied.' ? <Check size={15} aria-hidden="true" /> : <Copy size={15} aria-hidden="true" />}{copying ? 'Copying…' : 'Copy commands'}</button></div>
        <pre data-testid="build-commands"><code>{commands[mode]}</code></pre>
      </div>
      <p className="copy-feedback" role="status" aria-label="Copy feedback">{feedback}</p>
      <p className="build-note">The first build downloads and compiles the bundled runtime. Once Brr opens, choose a project folder and sign in with ChatGPT. Model access depends on your ChatGPT account.</p>
    </div>
  </section>
}

const questions = [
  ['What is an agent harness?', 'The place around an AI model that helps it do work: tools, instructions, project context, permissions and conversation history. Brr brings those pieces into a desktop workspace you can shape.'],
  ['Is Brr only for coding?', 'No. A project can hold research notes, drafts, checklists, data or code. Brr works with files and available tools, so the task and your setup determine what it can do. The tour shows a few starting points.'],
  ['How can I extend it?', 'The owned runtime includes skills and MCP tool support. Skills describe reusable workflows; MCP connects tools. Configuration is currently manual, and Brr does not yet have an in-app extension marketplace. You can also change the Apache-licensed harness source.'],
  ['What does community-owned mean here?', 'You can inspect the source, modify it, redistribute it under Apache 2.0, and propose changes. Brr is early and maintainer-led today. An open contribution process gives the community a way to shape its direction; there is no foundation or elected governance body.'],
  ['Does everything stay on my computer?', 'Your project tools run locally and Brr stores conversation history in its private application-data folder. Model inference is sent to OpenAI using your ChatGPT sign-in. Brr is not an offline model, and connected tools may send data to their configured services.'],
  ['Which accounts and platforms are supported?', 'The desktop source targets macOS and uses ChatGPT sign-in only. New conversations currently require a Git project with a commit and Worktrunk installed. Brr does not accept API keys or alternate model providers. A public, notarized app download is not available yet; start with the source build above.'],
]

export function App() {
  return <>
    <a className="skip-link" href="#main-content">Skip to content</a>
    <header className="site-header section-shell">
      <a className="wordmark" href="#" aria-label="Brr home">Brr<span className="wordmark-dot" aria-hidden="true" /></a>
      <nav className="main-nav" aria-label="Main navigation"><a href="#possibilities">Possibilities</a><a href="#extend">Extend</a><a href="#community">Community</a></nav>
      <a className="button button-small button-light" href="#get-brr">Get Brr <ArrowRight size={15} aria-hidden="true" /></a>
    </header>

    <main id="main-content">
      <section className="hero section-shell" aria-labelledby="hero-heading">
        <div className="hero-copy">
          <span className="eyebrow"><span className="status-dot" aria-hidden="true" /> OPEN SOURCE. OPEN POSSIBILITIES.</span>
          <h1 id="hero-heading"><span>Your work.</span>{' '}<span>Your way.</span></h1>
          <p className="hero-description">An open agent harness for research, writing, organising, and whatever you want to make next.</p>
          <div className="hero-actions"><a className="button button-light" href="#possibilities">Explore Brr <ArrowDown size={17} aria-hidden="true" /></a><a className="text-link" href="#extend">Make it yours <ArrowRight size={17} aria-hidden="true" /></a></div>
          <p className="hero-footnote">Your project. Your tools. Your direction.</p>
        </div>
        <div className="hero-art" aria-hidden="true"><img src="/assets/agents-go-brr.webp" srcSet="/assets/agents-go-brr-small.webp 768w, /assets/agents-go-brr.webp 1536w" sizes="(min-width: 900px) 65vw, 110vw" width="1536" height="1024" alt="" fetchPriority="high" /><span className="art-caption">agents go brr! ↗</span></div>
      </section>

      <section className="possibilities section-shell" id="possibilities" aria-labelledby="possibilities-heading">
        <div className="section-intro"><div><span className="eyebrow">A FEW DIRECTIONS TO TAKE IT</span><h2 id="possibilities-heading">Research it. Write it.<br />Make it happen.</h2></div><p>Start with a project and an idea. Make a plan together, add a new direction, and see what changes.</p></div>
        <WorkflowTour />
        <div className="control-notes"><p><SlidersHorizontal size={18} aria-hidden="true" /><span>Plan first.<br /><strong>Then decide to move.</strong></span></p><p><ShieldCheck size={18} aria-hidden="true" /><span>Stay in the loop.<br /><strong>Steer, review, or stop.</strong></span></p><p><FolderOpen size={18} aria-hidden="true" /><span>Keep your context.<br /><strong>Pick up the conversation.</strong></span></p></div>
      </section>

      <section className="extend-section" id="extend" aria-labelledby="extend-heading"><div className="section-shell">
        <div className="extend-hero"><div><span className="eyebrow">FLEXIBLE BY DESIGN</span><h2 id="extend-heading">A harness you can make your own.</h2><p>Brr is the space around the agent: your instructions, your files, the tools it can use, and the choices you make along the way.</p></div><img className="extend-art" src="/assets/extensible-harness.webp" srcSet="/assets/extensible-harness-small.webp 480w, /assets/extensible-harness.webp 960w" sizes="(min-width: 900px) 42vw, 90vw" width="960" height="640" loading="lazy" alt="Three colourful modules connected in a loop, representing reusable skills, connected tools and editable source." /></div>
        <dl className="extension-paths"><div><dt><span>01</span> Teach it a workflow.</dt><dd>Skills give the runtime reusable instructions for the way you like to work.</dd></div><div><dt><span>02</span> Bring your tools.</dt><dd>MCP support gives the runtime a way to connect to more tools and context.</dd></div><div><dt><span>03</span> Change the harness.</dt><dd>Fork the source. Experiment with an idea. Help make the next version better.</dd></div></dl>
        <p className="extension-note"><Puzzle size={17} aria-hidden="true" /> Skills and MCP are runtime capabilities. Setup is currently manual; there is no in-app marketplace yet.</p>
      </div></section>

      <section className="community-section" id="community" aria-labelledby="community-heading"><div className="section-shell community-layout"><div><span className="eyebrow">OPEN CODE. OPEN DIRECTION.</span><h2 id="community-heading">Built to be<br />built on.</h2></div><div className="community-copy"><p className="community-lead">A good harness should leave room for other people’s ideas.</p><p>Brr starts with forkable source and an invitation to contribute. Share a workflow. Improve a tool. Build something we haven’t thought of.</p><a className="button button-light" href="https://github.com/nk2580/brr" target="_blank" rel="noreferrer">Help shape Brr <ArrowRight size={17} aria-hidden="true" /><span className="sr-only"> (opens in a new tab)</span></a><div className="community-meta"><span>Apache 2.0</span><span>Early, maintainer-led project</span></div><p className="community-note">Explore the source, open an issue, or share a contribution on GitHub. <a href="/CONTRIBUTING.md" download>Read the contribution guide.</a></p></div></div></section>

      <section className="workspace-section section-shell" aria-labelledby="workspace-heading"><div><span className="eyebrow">A DESKTOP HOME FOR YOUR AGENT</span><h2 id="workspace-heading">Close to your work.<br />Clear about the boundaries.</h2></div><div className="workspace-copy"><p>Your workspace tools run on your computer. Conversations stay in Brr’s private application-data folder, ready to reopen.</p><p>The bundled harness is a source fork of Codex CLI. Model inference uses your ChatGPT sign-in and goes to OpenAI. No API-key setup or separate CLI installation is needed.</p><a className="text-link" href="/UPSTREAM.md" download>Read the runtime story <ArrowRight size={16} aria-hidden="true" /></a></div></section>

      <BuildGuide />

      <section className="faq-section section-shell" aria-labelledby="faq-heading"><div><span className="eyebrow">A FEW THINGS TO KNOW</span><h2 id="faq-heading">Good questions.</h2></div><div className="faq-list">{questions.map(([question, answer]) => <details key={question}><summary>{question}<ChevronDown size={19} aria-hidden="true" /></summary><p>{answer}</p></details>)}</div></section>
    </main>

    <footer className="site-footer section-shell"><a className="wordmark footer-wordmark" href="#" aria-label="Brr home">Brr<span className="wordmark-dot" aria-hidden="true" /></a><p>A little brr. A lot of possibility.</p><nav aria-label="Project information"><a href="/LICENSE.txt">License</a><a href="/NOTICE.txt">Notices</a><a href="/GOVERNANCE.md">Governance</a><a href="#get-brr">Get Brr <ArrowRight size={14} aria-hidden="true" /></a></nav><span className="footer-year">© {new Date().getFullYear()} Brr contributors</span></footer>
  </>
}
