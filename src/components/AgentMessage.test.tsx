import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import { AgentMessage } from './AgentMessage';

describe('AgentMessage', () => {
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
});
