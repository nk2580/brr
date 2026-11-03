import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import { AgentMessage } from './AgentMessage';

vi.mock('./McpApp', () => ({ McpApp: ({ invocation }: any) => <div data-testid="mcp-app">{invocation.resourceUri}</div> }));
vi.mock('./IntelligentUi', () => ({ IntelligentUi: ({identity, preview, document}: any) => <div data-testid="intelligent-ui" data-preview={Boolean(preview)}>{identity.callId}{document?.title}</div> }));

describe('AgentMessage', () => {
  it('renders successful generated UI calls and keeps failed app source as text', () => {
    const document = {version: 1, kind: 'app', title: 'Calculator', text: 'Calculator alternative', initialState: {}, actions: [], html: '<script>bad()</script>'};
    const {rerender} = render(<AgentMessage item={{id: 'ui-1', type: 'dynamicToolCall', tool: 'brr_ui_render', status: 'completed', success: true, arguments: document}} threadId="chat" />);
    expect(screen.getByTestId('intelligent-ui')).toHaveTextContent('ui-1');
    rerender(<AgentMessage item={{id: 'ui-1', type: 'dynamicToolCall', tool: 'brr_ui_render', status: 'failed', success: false, arguments: document}} threadId="chat" />);
    expect(screen.queryByTestId('intelligent-ui')).not.toBeInTheDocument();
    expect(screen.getByText('Calculator alternative')).toBeInTheDocument();
    expect(window.document.querySelector('script')).toBeNull();
  });

  it('labels a runtime plan item as a proposed plan', () => {
    render(<AgentMessage item={{ id: 'plan-1', type: 'plan', text: '1. Inspect the current flow' }} />);

    expect(screen.getByRole('heading', { name: 'Proposed plan' })).toBeInTheDocument();
    expect(screen.getByText('Inspect the current flow')).toBeInTheDocument();
  });

  it('renders agent text as GitHub-flavored markdown while escaping raw HTML', () => {
    render(
      <AgentMessage
        item={{
          id: 'message-1',
          type: 'agentMessage',
          text: 'A **bold** answer\n\n| Name | State |\n| --- | --- |\n| job | done |\n\n<script>bad()</script>',
        }}
      />,
    );

    expect(screen.getByText('bold').tagName).toBe('STRONG');
    expect(screen.getByRole('table')).toBeInTheDocument();
    expect(screen.queryByRole('generic', { name: 'bad()' })).not.toBeInTheDocument();
    expect(screen.getByText('<script>bad()</script>')).toBeInTheDocument();
  });

  it('reveals command details and exit status when expanded', async () => {
    render(
      <AgentMessage
        item={{
          id: 'command-1',
          type: 'commandExecution',
          command: 'pnpm build',
          aggregatedOutput: 'Build complete',
          status: 'completed',
          exitCode: 0,
        }}
      />,
    );

    const details = screen.getByText('Command · completed').closest('details');
    expect(details).not.toHaveAttribute('open');
    await userEvent.click(screen.getByText('Command · completed'));
    expect(details).toHaveAttribute('open');
    expect(screen.getByText('pnpm build')).toBeInTheDocument();
    expect(screen.getByText('Build complete')).toBeInTheDocument();
    expect(screen.getByText('Exit code: 0')).toBeInTheDocument();
  });

  it('renders an MCP App only for tool items with a UI resource and active thread', () => {
    const onUsePrompt = vi.fn();
    const { rerender } = render(<AgentMessage item={{ id: 'tool-1', type: 'mcpToolCall', server: 'calendar', tool: 'today', mcpAppUi: { resourceUri: 'ui://calendar/today' } }} threadId="thread-1" onUsePrompt={onUsePrompt} />);
    expect(screen.getByTestId('mcp-app')).toHaveTextContent('ui://calendar/today');
    rerender(<AgentMessage item={{ id: 'tool-1', type: 'mcpToolCall', server: 'calendar', tool: 'today', mcpAppUi: { resourceUri: 'ui://calendar/today' } }} />);
    expect(screen.queryByTestId('mcp-app')).not.toBeInTheDocument();
  });
});


it('copies a code block as plain text without its fence or surrounding answer', async () => {
  const writeText = vi.fn().mockResolvedValue(undefined);
  Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } });
  render(<AgentMessage item={{ id: 'code-answer', type: 'agentMessage', text: 'Example:\n\n```ts\nconst value = 42;\n```' }} />);
  await userEvent.click(screen.getByRole('button', { name: 'Copy code' }));
  expect(writeText).toHaveBeenCalledWith('const value = 42;\n');
});
