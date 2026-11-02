import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import type { AgentItem } from '../lib/session';
import { extractMcpAppInvocation } from '../lib/mcp-app';
import { McpApp } from './McpApp';

type AgentMessageProps = { item: AgentItem; threadId?: string; onUsePrompt?: (text: string) => void };

function stringField(item: AgentItem, field: string): string | undefined {
  const value = item[field];
  return typeof value === 'string' ? value : undefined;
}

function userText(item: AgentItem): string {
  const directText = stringField(item, 'text');
  if (directText) return directText;
  const content = item.content;
  if (!Array.isArray(content)) return '';
  return content.map((part: any) => {
    if (part?.type === 'text' && typeof part.text === 'string') return part.text;
    if (part?.type === 'image' || part?.type === 'localImage') return '[Image]';
    if (part?.type === 'audio' || part?.type === 'localAudio') return '[Audio]';
    if (part?.type === 'skill' && typeof part.name === 'string') return `Skill: ${part.name}`;
    if (part?.type === 'mention' && typeof part.name === 'string') return `@${part.name}`;
    return '';
  }).filter(Boolean).join('\n');
}

function Markdown({ children }: { children: string }) {
  return <div className="agent-markdown"><ReactMarkdown remarkPlugins={[remarkGfm]}>{children}</ReactMarkdown></div>;
}

function activityLabel(item: AgentItem): string {
  if (item.type === 'mcpToolCall') {
    const server = stringField(item, 'server');
    const tool = stringField(item, 'tool');
    return `Used ${[server, tool].filter(Boolean).join(' / ') || 'a tool'}`;
  }
  if (item.type === 'collabAgentToolCall') {
    const tool = stringField(item, 'tool') ?? 'Agent activity';
    const labels: Record<string, string> = {
      spawnAgent: 'Spawned agent',
      sendInput: 'Sent input to agent',
      resumeAgent: 'Resumed agent',
      wait: 'Waited for agent',
      closeAgent: 'Closed agent',
      sendMessage: 'Messaged agent',
      followupTask: 'Assigned follow-up task',
      interruptAgent: 'Interrupted agent',
      listAgents: 'Checked agents',
    };
    return labels[tool] ?? `Agent activity: ${tool}`;
  }
  return item.type === 'subAgentActivity' ? 'Sub-agent activity' : 'Activity';
}

function readableType(type: string): string {
  return type.replace(/([a-z])([A-Z])/g, '$1 $2').replace(/^./, (letter) => letter.toUpperCase());
}

export function AgentMessage({ item, threadId, onUsePrompt }: AgentMessageProps) {
  if (item.type === 'userMessage') {
    return (
      <article className="agent-message agent-message-user" data-item-type={item.type} style={{ display: 'flex', justifyContent: 'flex-end' }}>
        <div className="agent-message-user-content"><Markdown>{userText(item)}</Markdown></div>
      </article>
    );
  }

  if (item.type === 'plan') {
    return <article className="agent-message agent-message-agent agent-message-plan" data-item-type={item.type}><h2>Proposed plan</h2><Markdown>{stringField(item, 'text') ?? ''}</Markdown></article>;
  }

  if (item.type === 'agentMessage') {
    return <article className="agent-message agent-message-agent" data-item-type={item.type}><Markdown>{stringField(item, 'text') ?? ''}</Markdown></article>;
  }

  if (item.type === 'reasoning') {
    const summary = Array.isArray(item.summary) ? item.summary.filter((part): part is string => typeof part === 'string').join('\n\n') : '';
    const text = stringField(item, 'text') ?? summary;
    return <details className="agent-message agent-message-reasoning" data-item-type={item.type}><summary>Reasoning</summary><Markdown>{text}</Markdown></details>;
  }

  if (item.type === 'commandExecution') {
    const command = stringField(item, 'command') ?? '';
    const output = stringField(item, 'aggregatedOutput') ?? '';
    const exitCode = item.exitCode;
    return (
      <details className="agent-message agent-message-command" data-item-type={item.type}>
        <summary>Command · {stringField(item, 'status') ?? 'unknown status'}</summary>
        {command && <pre><code>{command}</code></pre>}
        {output && <pre aria-label="Command output">{output}</pre>}
        <p>Exit code: {typeof exitCode === 'number' ? exitCode : 'not available'}</p>
      </details>
    );
  }

  if (item.type === 'fileChange') {
    const changes = Array.isArray(item.changes) ? item.changes : [];
    return (
      <section className="agent-message agent-message-file-change" data-item-type={item.type}>
        <strong>File changes</strong>
        {changes.map((change, index) => (
          <details key={`${change.path}-${index}`}>
            <summary>{change.path}</summary>
            <pre><code>{change.diff}</code></pre>
          </details>
        ))}
        {changes.length === 0 && <span>No file changes reported</span>}
      </section>
    );
  }

  if (item.type === 'mcpToolCall' || item.type === 'collabAgentToolCall' || item.type === 'subAgentActivity') {
    const status = stringField(item, 'status');
    const detail = stringField(item, 'prompt') ?? stringField(item, 'agentPath');
    const mcpApp = item.type === 'mcpToolCall' ? extractMcpAppInvocation(item, threadId) : null;
    return (
      <section className="agent-message agent-message-activity" data-item-type={item.type}>
        <strong>{activityLabel(item)}</strong>
        {status && <span> · {status}</span>}
        {detail && <p>{detail}</p>}
        {mcpApp && <McpApp invocation={mcpApp} onUsePrompt={onUsePrompt} />}
      </section>
    );
  }

  return <section className="agent-message agent-message-activity" data-item-type={item.type}>{readableType(item.type)}{stringField(item, 'status') ? ` · ${stringField(item, 'status')}` : ''}</section>;
}

export default AgentMessage;
