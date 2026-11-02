export type MentionableAgent = { id: string; name: string }

/** Resolve an explicit @Name token without attaching any prior transcript context. */
export function explicitAgentMention(message: string, profiles: MentionableAgent[]) {
  const candidates = [...profiles].sort((left, right) => right.name.length - left.name.length)
  for (const agent of candidates) {
    const escaped = agent.name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
    const mention = new RegExp(`(^|\\s)@${escaped}(?=\\s|$)`, 'i')
    const match = mention.exec(message)
    if (!match || match.index === undefined) continue
    const before = message.slice(0, match.index + match[1].length)
    const after = message.slice(match.index + match[0].length)
    const separator = before && after && !/[\t \n]$/.test(before) && !/^[\t \n]/.test(after) ? ' ' : ''
    const trailingNewlineOverlap = before.endsWith('\n') && after.startsWith('\n')
    const instructions = `${before}${separator}${trailingNewlineOverlap ? after.slice(1) : after}`
      .replace(/[\t ]{2,}/g, ' ')
      .replace(/[\t ]+\n/g, '\n')
      .replace(/\n[\t ]+/g, '\n')
      .trim()
    if (instructions) return { agentId: agent.id, instructions }
    return null
  }
  return null
}
