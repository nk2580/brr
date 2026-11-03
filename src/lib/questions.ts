export type Question = {
  id: string
  header: string
  question: string
  options?: { label: string; description: string }[] | null
}

export type QuestionAnswers = Record<string, string[]>

export type QuestionRequest = {
  id: string
  threadId: string
  turnId: string
  itemId: string
  agentId?: string | null
  taskId?: string | null
  runId?: string | null
  kind: 'blocking' | 'async'
  questions: Question[]
  draft: QuestionAnswers
  answers?: QuestionAnswers | null
  resolution: 'pending' | 'answered' | 'dismissed' | 'closed'
  delivery: 'none' | 'queued' | 'dispatching' | 'delivered' | 'uncertain'
  reason?: string | null
  clientMessageId?: string | null
  createdAt: number
  updatedAt: number
  runtimeGeneration?: string | null
  runtimeRequestId?: string | number | null
  waiting?: boolean
}

export function getQuestionStatus(request: QuestionRequest) {
  if (request.delivery === 'uncertain') return 'Delivery needs attention'
  if (request.resolution === 'dismissed') return 'Dismissed'
  if (request.resolution === 'closed') return 'Closed'
  if (request.resolution === 'answered') {
    if (request.delivery === 'queued') return 'Answer queued'
    if (request.delivery === 'dispatching') return 'Sending'
    return 'Answered'
  }
  return request.kind === 'blocking' || request.waiting ? 'Waiting for your answer' : 'Agent is continuing'
}

export function questionAnswersEqual(left: QuestionAnswers, right: QuestionAnswers) {
  const leftIds = Object.keys(left).sort()
  const rightIds = Object.keys(right).sort()
  return leftIds.length === rightIds.length && leftIds.every((id, index) => {
    const values = left[id] ?? []
    const other = right[rightIds[index]] ?? []
    return id === rightIds[index] && values.length === other.length && values.every((value, valueIndex) => value === other[valueIndex])
  })
}
