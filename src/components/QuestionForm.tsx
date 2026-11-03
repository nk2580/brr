import { useEffect, useRef, useState, type ReactNode } from 'react'
import { getQuestionStatus, questionAnswersEqual, type Question, type QuestionAnswers, type QuestionRequest } from '../lib/questions'
import { Button } from './ui/button'
import './QuestionForm.css'

type QuestionFormProps = {
  request: QuestionRequest
  onSaveDraft: (answers: QuestionAnswers) => Promise<unknown> | void
  onAnswer: (answers: QuestionAnswers) => Promise<unknown>
  onDismiss: () => Promise<unknown>
  origin?: string
  trailingControl?: ReactNode
}

export function QuestionForm({ request, onSaveDraft, onAnswer, onDismiss, origin, trailingControl }: QuestionFormProps) {
  const [answers, setAnswers] = useState<QuestionAnswers>(() => ({ ...(request.resolution === 'pending' ? request.draft : request.answers ?? request.draft) }))
  const [step, setStep] = useState(0)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const promptRef = useRef<HTMLHeadingElement | null>(null)
  const previousStep = useRef(step)
  const latest = useRef(answers)
  const stillPending = useRef(request.resolution === 'pending')
  stillPending.current = request.resolution === 'pending'
  const mounted = useRef(true)
  const requestId = useRef(request.id)
  const hasLocalEdits = useRef(false)
  const saveLoop = useRef<Promise<void> | null>(null)
  const queuedSave = useRef<QuestionAnswers | null>(null)
  const savedSnapshot = useRef<QuestionAnswers>(request.draft)

  useEffect(() => { latest.current = answers }, [answers])

  useEffect(() => {
    if (previousStep.current === step) return
    previousStep.current = step
    promptRef.current?.focus()
  }, [step])

  useEffect(() => {
    const authoritative = request.resolution === 'pending' ? request.draft : request.answers ?? request.draft
    if (requestId.current !== request.id) {
      requestId.current = request.id
      hasLocalEdits.current = false
      savedSnapshot.current = request.draft
      latest.current = { ...authoritative }
      setAnswers({ ...authoritative })
      setStep(0)
      setError('')
      return
    }
    if (!hasLocalEdits.current) {
      savedSnapshot.current = request.draft
      latest.current = { ...authoritative }
      setAnswers({ ...authoritative })
    }
  }, [request.id, request.draft, request.answers, request.resolution])

  const save = (value: QuestionAnswers) => {
    if (questionAnswersEqual(value, savedSnapshot.current) && !queuedSave.current) return saveLoop.current ?? Promise.resolve()
    queuedSave.current = value
    if (!saveLoop.current) {
      saveLoop.current = (async () => {
        while (queuedSave.current) {
          const next = queuedSave.current
          queuedSave.current = null
          try {
            await onSaveDraft(next)
            savedSnapshot.current = next
          } catch (reason) {
            if (mounted.current && questionAnswersEqual(next, latest.current)) setError(message(reason))
          }
        }
      })().finally(() => { saveLoop.current = null })
    }
    return saveLoop.current
  }

  useEffect(() => {
    if (request.resolution !== 'pending' || questionAnswersEqual(answers, request.draft)) return
    const timer = window.setTimeout(() => { void save(answers) }, 250)
    return () => window.clearTimeout(timer)
  }, [answers, request.draft, request.resolution])

  useEffect(() => {
    mounted.current = true
    return () => {
      mounted.current = false
      if (stillPending.current) void save(latest.current)
    }
  }, [])

  const updateAnswer = (questionId: string, value: string) => {
    hasLocalEdits.current = true
    const next = { ...latest.current, [questionId]: value ? [value] : [] }
    latest.current = next
    setAnswers(next)
    setError('')
  }

  const isComplete = request.questions.length > 0 && request.questions.every(question => {
    const values = answers[question.id] ?? []
    return values.length === 1 && Boolean(values[0].trim())
  })
  const question = request.questions[Math.min(step, Math.max(0, request.questions.length - 1))]
  const currentValue = question ? (answers[question.id] ?? [])[0] ?? '' : ''
  const isCurrentComplete = Boolean(currentValue.trim())

  const submit = async () => {
    if (!isComplete) return
    const cleanAnswers = Object.fromEntries(request.questions.map(question => [question.id, [(answers[question.id] ?? [])[0].trim()]]))
    setBusy(true)
    setError('')
    try {
      await save(latest.current)
      await onAnswer(cleanAnswers)
    } catch (reason) {
      setError(message(reason))
    } finally { setBusy(false) }
  }

  const advance = async () => {
    if (step >= request.questions.length - 1) {
      await submit()
      return
    }
    setBusy(true)
    setError('')
    try {
      await save(latest.current)
      setStep(current => Math.min(current + 1, request.questions.length - 1))
    } finally { setBusy(false) }
  }

  const goBack = () => setStep(current => Math.max(0, current - 1))

  const dismiss = async () => {
    setBusy(true)
    setError('')
    try { await save(latest.current); await onDismiss() }
    catch (reason) { setError(message(reason)) }
    finally { setBusy(false) }
  }

  if (request.resolution !== 'pending') return null
  const status = getQuestionStatus(request)

  return <section id={`question-input-${request.id}`} className="question-composer" aria-label="Answer agent questions" data-origin={origin} tabIndex={-1}>
    <header className="question-composer-header">
      <div className="question-composer-context">
        <span>{origin || 'Agent question'}</span>
        <span className="question-progress">{request.questions.length ? `${step + 1} of ${request.questions.length}` : 'No questions'}</span>
      </div>
      <span role="status" className={`question-status${request.delivery === 'uncertain' ? ' is-warning' : ''}`}>{status}</span>
    </header>
    {question ? <form className="question-composer-form" onSubmit={event => { event.preventDefault(); void advance() }}>
      <h2 ref={promptRef} tabIndex={-1} id={`question-prompt-${request.id}-${question.id}`} className="question-prompt">{question.question}</h2>
      {question.options?.length ? <fieldset disabled={busy} className="question-choice-list" aria-label={question.header || question.question}>
        <legend className="sr-only">{question.header || question.question}</legend>
        {question.options.map((option, index) => <label className="question-choice" key={`${index}-${option.label}`}>
          <input type="radio" name={`question-${request.id}-${question.id}`} checked={currentValue === option.label} onChange={() => updateAnswer(question.id, option.label)} />
          <span><strong>{option.label}</strong><small>{option.description}</small></span>
        </label>)}
      </fieldset> : null}
      <label className="question-composer-answer-label">
        {question.options?.length ? <span>Other answer</span> : null}
        <textarea disabled={busy} aria-label={question.options?.length ? `Other answer for ${question.header || question.question}` : undefined}
          aria-labelledby={question.options?.length ? undefined : `question-prompt-${request.id}-${question.id}`}
          placeholder={question.options?.length ? undefined : 'Type your answer…'} rows={2}
          value={question.options?.length ? (answers[question.id] ?? []).find(value => !question.options?.some(option => option.label === value)) ?? '' : currentValue}
          onChange={event => updateAnswer(question.id, event.target.value)} />
      </label>
      {error && <div className="question-error"><p role="alert">{error}</p><Button type="button" variant="ghost" size="sm" onClick={() => setError('')}>Clear error</Button></div>}
      <footer className="question-composer-footer">
        <Button type="button" variant="ghost" size="sm" disabled={busy} onClick={() => void dismiss()}>Dismiss</Button>
        <div className="question-composer-navigation">
          <Button type="button" variant="ghost" size="sm" disabled={busy || step === 0} onClick={goBack}>Back</Button>
          <Button type="submit" size="sm" disabled={busy || (step === request.questions.length - 1 ? !isComplete : !isCurrentComplete)}>{busy ? step === request.questions.length - 1 ? 'Submitting…' : 'Saving…' : step === request.questions.length - 1 ? error ? 'Retry answer' : 'Submit answers' : 'Next'}</Button>
          {trailingControl}
        </div>
      </footer>
    </form> : <p className="question-empty">No questions were provided.</p>}
  </section>
}

export function QuestionTranscript({ request, origin, onAnswer }: {
  request: QuestionRequest
  origin?: string
  onAnswer: () => void
}) {
  const status = getQuestionStatus(request)
  const resolved = request.resolution !== 'pending'
  const shownAnswers = request.answers ?? request.draft
  return <section id={`question-${request.id}`} className="question-transcript" aria-label="Agent question" data-origin={origin} tabIndex={-1}>
    <header className="question-transcript-header">
      <div><strong>{request.questions.length === 1 ? request.questions[0].header : 'Agent questions'}</strong>{origin && <span>{origin}</span>}</div>
      <span role="status" className={`question-status${request.delivery === 'uncertain' ? ' is-warning' : ''}`}>{status}</span>
    </header>
    <div className="question-transcript-content">
      {request.questions.map(question => <section key={question.id}>
        <h3>{question.question}</h3>
        {resolved && <p>{(shownAnswers[question.id] ?? []).join(', ') || 'No answer provided'}</p>}
      </section>)}
      {request.delivery === 'uncertain' && <p className="question-reason">{request.reason || 'We could not confirm whether the answer was delivered.'}</p>}
      {!resolved && <Button size="sm" variant="ghost" onClick={onAnswer}>Answer questions for {request.questions[0]?.header || 'this request'}</Button>}
    </div>
  </section>
}

function message(reason: unknown) {
  return reason instanceof Error ? reason.message : String(reason)
}
