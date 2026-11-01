#!/usr/bin/env node

import { spawn } from 'node:child_process';
import { mkdtemp, mkdir, rm } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const MAX_FRAME_BYTES = 64 * 1024 * 1024;
const PHASE_TIMEOUT_MS = 15_000;
const STARTUP_TIMEOUT_MS = 60_000;
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const defaultBinary = path.join(root, 'src-tauri', 'binaries', 'codex-code-mode-host');

function runtimePathFromArgs(args) {
  if (args.length === 0) return defaultBinary;
  if (args.length === 2 && args[0] === '--runtime') return path.resolve(args[1]);
  if (args.length === 1) return path.resolve(args[0]);
  throw new Error('Usage: node scripts/smoke-code-host.mjs [--runtime] [path-to-codex-code-mode-host]');
}

function withTimeout(promise, phase, timeoutMs = PHASE_TIMEOUT_MS) {
  let timer;
  return Promise.race([
    promise,
    new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`${phase} timed out after ${timeoutMs} ms`)), timeoutMs);
      timer.unref?.();
    }),
  ]).finally(() => clearTimeout(timer));
}

function deadline(timeoutMs = PHASE_TIMEOUT_MS) {
  return { at: Date.now() + timeoutMs, timeoutMs };
}

function remaining(deadlineAt, phase) {
  const timeoutMs = deadlineAt.at - Date.now();
  if (timeoutMs <= 0) throw new Error(`${phase} timed out after ${deadlineAt.timeoutMs} ms`);
  return timeoutMs;
}

class FrameReader {
  constructor(stream) {
    this.iterator = stream[Symbol.asyncIterator]();
    this.buffer = Buffer.alloc(0);
    this.ended = false;
  }

  async readExactly(length) {
    while (this.buffer.length < length) {
      const next = await this.iterator.next();
      if (next.done) {
        this.ended = true;
        throw new Error('host closed stdout before completing a protocol frame');
      }
      this.buffer = this.buffer.length === 0
        ? Buffer.from(next.value)
        : Buffer.concat([this.buffer, next.value]);
    }
    const result = this.buffer.subarray(0, length);
    this.buffer = this.buffer.subarray(length);
    return result;
  }

  async readFrame() {
    const header = await this.readExactly(4);
    const length = header.readUInt32LE(0);
    if (length === 0 || length > MAX_FRAME_BYTES) {
      throw new Error(`host sent invalid frame length ${length}`);
    }
    const payload = await this.readExactly(length);
    try {
      return JSON.parse(payload.toString('utf8'));
    } catch (error) {
      throw new Error(`host sent invalid JSON: ${error.message}`);
    }
  }
}

async function writeFrame(stream, message, phase, deadlineAt = deadline()) {
  const payload = Buffer.from(JSON.stringify(message), 'utf8');
  if (payload.length === 0 || payload.length > MAX_FRAME_BYTES) {
    throw new Error(`outgoing ${phase} frame exceeds the ${MAX_FRAME_BYTES}-byte limit`);
  }
  const frame = Buffer.allocUnsafe(payload.length + 4);
  frame.writeUInt32LE(payload.length, 0);
  payload.copy(frame, 4);
  await withTimeout(new Promise((resolve, reject) => {
    const onError = (error) => {
      stream.off('drain', onDrain);
      reject(error);
    };
    const onDrain = () => {
      stream.off('error', onError);
      resolve();
    };
    stream.once('error', onError);
    if (stream.write(frame)) {
      stream.off('error', onError);
      resolve();
    } else {
      stream.once('drain', onDrain);
    }
  }), `${phase} write`, remaining(deadlineAt, phase));
}

async function nextFrame(reader, phase, deadlineAt = deadline()) {
  return withTimeout(reader.readFrame(), phase, remaining(deadlineAt, phase));
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

async function operationResponse(reader, id, phase, deadlineAt = deadline()) {
  for (;;) {
    const message = await nextFrame(reader, phase, deadlineAt);
    if (message.type === 'operation/response' && message.id === id) return message;
    if (message.type === 'connection/rejected') {
      throw new Error(`host rejected connection: ${JSON.stringify(message.reason)}`);
    }
  }
}

function valueOrThrow(response, label) {
  assert(response?.result?.status === 'ok', `${label} failed: ${response?.result?.message ?? JSON.stringify(response)}`);
  return response.result.value;
}

async function main() {
  const binary = runtimePathFromArgs(process.argv.slice(2));
  const temp = await mkdtemp(path.join(os.tmpdir(), 'brr-code-host-smoke-'));
  const codexHome = path.join(temp, 'codex-home');
  await mkdir(codexHome, { recursive: true });

  let child;
  let failure;
  let stderrChunks = [];
  let exitPromise;
  try {
    child = spawn(binary, [], {
      cwd: temp,
      env: {
        PATH: process.env.PATH ?? '/usr/bin:/bin',
        ...(process.env.HOME ? { HOME: process.env.HOME } : {}),
        CODEX_HOME: codexHome,
      },
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    child.stderr.on('data', (chunk) => stderrChunks.push(Buffer.from(chunk)));
    exitPromise = new Promise((resolve, reject) => {
      child.once('exit', (code, signal) => resolve([code, signal]));
      child.once('error', reject);
    });
    void exitPromise.catch(() => {});
    const reader = new FrameReader(child.stdout);

    let phaseDeadline = deadline(STARTUP_TIMEOUT_MS);
    await writeFrame(child.stdin, {
      type: 'connection/hello',
      supportedVersions: [1],
      requiredCapabilities: [],
      optionalCapabilities: [],
    }, 'hello', phaseDeadline);
    const ready = await nextFrame(reader, 'connection handshake', phaseDeadline);
    assert(ready.type === 'connection/ready' && ready.selectedVersion === 1,
      `expected connection/ready for protocol v1, got ${JSON.stringify(ready)}`);

    phaseDeadline = deadline();
    await writeFrame(child.stdin, {
      type: 'operation/request',
      id: 1,
      request: { method: 'session/open', sessionId: 'brr-smoke' },
    }, 'session open', phaseDeadline);
    const opened = valueOrThrow(await operationResponse(reader, 1, 'session open', phaseDeadline), 'session/open');
    assert(opened.type === 'session/ready' && opened.sessionId === 'brr-smoke',
      `unexpected session/open response: ${JSON.stringify(opened)}`);

    phaseDeadline = deadline();
    await writeFrame(child.stdin, {
      type: 'operation/request',
      id: 2,
      request: {
        method: 'session/execute',
        sessionId: 'brr-smoke',
        request: {
          tool_call_id: 'smoke',
          enabled_tools: [],
          source: 'text(2+2)',
          yield_time_ms: 5_000,
          max_output_tokens: 100,
        },
      },
    }, 'session execute', phaseDeadline);

    let executeAck;
    let initialResponse;
    while (!executeAck || !initialResponse) {
      const message = await nextFrame(reader, 'session execute', phaseDeadline);
      if (message.type === 'operation/response' && message.id === 2) executeAck = message;
      if (message.type === 'execute/initialResponse' && message.id === 2) initialResponse = message;
    }
    const ack = valueOrThrow(executeAck, 'session/execute acknowledgement');
    assert(ack.type === 'execution/started', `unexpected execute acknowledgement: ${JSON.stringify(ack)}`);
    let execution = valueOrThrow(initialResponse, 'session/execute result');

    let requestId = 3;
    if (execution.Result === undefined && execution.Yielded !== undefined) {
      let cellId = execution.Yielded.cell_id;
      while (execution.Yielded !== undefined) {
        await writeFrame(child.stdin, {
          type: 'operation/request',
          id: requestId,
          request: {
            method: 'session/wait',
            sessionId: 'brr-smoke',
            request: { cell_id: cellId, yield_time_ms: 5_000 },
          },
        }, 'session wait', phaseDeadline);
        const waited = valueOrThrow(await operationResponse(reader, requestId, 'session wait', phaseDeadline), 'session/wait');
        assert(waited.type === 'wait/completed', `unexpected wait response: ${JSON.stringify(waited)}`);
        const outcome = waited.outcome?.LiveCell ?? waited.outcome?.MissingCell;
        assert(outcome, `wait response has no cell outcome: ${JSON.stringify(waited)}`);
        execution = outcome;
        cellId = execution.Yielded?.cell_id ?? cellId;
        requestId += 1;
      }
    }

    assert(execution.Result, `execution did not complete with a result: ${JSON.stringify(execution)}`);
    assert(execution.Result.error_text === null, `execution returned an error: ${execution.Result.error_text}`);
    const text = execution.Result.content_items
      ?.filter((item) => item.type === 'input_text')
      .map((item) => item.text)
      .join('');
    assert(text === '4', `expected code-mode output "4", got ${JSON.stringify(text)}`);

    const shutdownId = requestId;
    phaseDeadline = deadline();
    await writeFrame(child.stdin, {
      type: 'operation/request',
      id: shutdownId,
      request: { method: 'session/shutdown', sessionId: 'brr-smoke' },
    }, 'session shutdown', phaseDeadline);
    const closed = valueOrThrow(await operationResponse(reader, shutdownId, 'session shutdown', phaseDeadline), 'session/shutdown');
    assert(closed.type === 'session/closed' && closed.sessionId === 'brr-smoke',
      `unexpected session/shutdown response: ${JSON.stringify(closed)}`);
    child.stdin.end();

    const [code, signal] = await withTimeout(exitPromise, 'host process exit', remaining(phaseDeadline, 'host process exit'));
    assert(code === 0, `host exited with code ${code ?? 'null'}${signal ? ` (signal ${signal})` : ''}`);
    console.log(`Code-mode host smoke passed: session open, text(2+2) => ${text}, clean shutdown.`);
  } catch (error) {
    failure = error;
  } finally {
    if (child && child.exitCode === null && child.signalCode === null) {
      const cleanupExit = exitPromise ?? new Promise((resolve) => child.once('exit', resolve));
      child.stdin.destroy();
      child.kill('SIGTERM');
      try {
        await withTimeout(cleanupExit, 'host cleanup');
      } catch {
        child.kill('SIGKILL');
        await Promise.race([new Promise((resolve) => child.once('exit', resolve)), new Promise((resolve) => setTimeout(resolve, 1_000))]);
      }
    }
    await rm(temp, { recursive: true, force: true });
  }

  if (failure) {
    const stderr = child ? Buffer.concat(stderrChunks).toString('utf8').trim() : '';
    console.error(`Code-mode host smoke failed: ${failure.message}${stderr ? `\n${stderr}` : ''}`);
    process.exitCode = 1;
  }
}

main().catch((error) => {
  console.error(`Code-mode host smoke failed: ${error.message}`);
  process.exitCode = 1;
});
