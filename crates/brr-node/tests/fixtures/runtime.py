#!/usr/bin/env python3
"""Test-only app-server boundary fixture. It performs no inference or tools."""
import json
import os
import sys

state_path = os.path.join(os.environ['BRR_SERVICE_DATA_DIR'], 'node', 'fixture-runtime.json')
try:
    with open(state_path) as state_file:
        state = json.load(state_file)
except FileNotFoundError:
    state = {'threads': {}, 'calls': [], 'approvals': [], 'seedApprovals': []}

def save():
    with open(state_path, 'w') as state_file:
        json.dump(state, state_file)

def emit(message):
    print(json.dumps(message), flush=True)

for line in sys.stdin:
    message = json.loads(line)
    if 'method' not in message:
        state['approvals'].append(message)
        state['seedApprovals'] = [
            approval for approval in state.get('seedApprovals', [])
            if approval.get('id') != message.get('id')
        ]
        save()
        continue
    if 'id' not in message:
        continue
    method = message['method']
    params = message.get('params', {})
    state['calls'].append({'method': method, 'params': params})
    result = {}
    event = None
    if method == 'initialize':
        result = {'capabilities': {}}
    elif method == 'account/read':
        result = {'account': {'type': 'chatgpt', 'email': 'fixture@example.invalid', 'planType': 'fixture'}}
    elif method == 'model/list':
        result = {'data': [{'id': 'fixture-model', 'model': 'fixture-model', 'displayName': 'Fixture', 'isDefault': True, 'defaultReasoningEffort': 'medium', 'supportedReasoningEfforts': [{'reasoningEffort': 'medium', 'description': 'Fixture'}]}]}
    elif method == 'thread/start':
        thread_id = 'thread-' + str(len(state['threads']) + 1)
        thread = {'id': thread_id, 'preview': '', 'cwd': params['cwd'], 'createdAt': 1, 'updatedAt': 1, 'turns': [], 'status': {'type': 'idle'}}
        state['threads'][thread_id] = thread
        rollouts = os.path.join(os.environ['CODEX_HOME'], 'sessions', 'fixture')
        os.makedirs(rollouts, exist_ok=True)
        with open(os.path.join(rollouts, thread_id + '.jsonl'), 'w') as rollout:
            rollout.write(json.dumps({'fixtureThreadId': thread_id}) + '\n')
        result = {'thread': thread}
    elif method in ('thread/read', 'thread/resume'):
        result = {'thread': state['threads'][params['threadId']]}
    elif method == 'thread/list':
        result = {'data': [thread for thread in state['threads'].values() if thread.get('archived', False) == params.get('archived', False)], 'nextCursor': None}
    elif method == 'thread/turns/list':
        result = {'data': state['threads'][params['threadId']]['turns'], 'nextCursor': None}
    elif method == 'turn/start':
        thread = state['threads'][params['threadId']]
        turn_id = 'turn-' + str(len(thread['turns']) + 1)
        item = {'id': 'user-' + turn_id, 'type': 'userMessage', 'clientId': params.get('clientUserMessageId'), 'content': params.get('input', [])}
        turn = {'id': turn_id, 'status': 'inProgress', 'items': [item]}
        thread['turns'].append(turn)
        thread['status'] = {'type': 'active'}
        thread['preview'] = '\n'.join(part.get('text', '') for part in item['content'])
        result = {'turn': turn}
        event = {'method': 'turn/started', 'params': {'threadId': thread['id'], 'turn': turn}}
    elif method == 'turn/steer':
        thread = state['threads'][params['threadId']]
        turn = thread['turns'][-1]
        if turn['id'] != params['expectedTurnId']:
            emit({'id': message['id'], 'error': {'code': -32602, 'message': 'Turn changed'}})
            continue
        turn['items'].append({'id': 'steer-' + str(len(turn['items'])), 'type': 'userMessage', 'clientId': params.get('clientUserMessageId'), 'content': params.get('input', [])})
        result = {'turnId': turn['id']}
    elif method == 'turn/interrupt':
        thread = state['threads'][params['threadId']]
        turn = thread['turns'][-1]
        turn['status'] = 'interrupted'
        thread['status'] = {'type': 'idle'}
        event = {'method': 'turn/completed', 'params': {'threadId': thread['id'], 'turn': turn}}
    elif method == 'account/login/start':
        result = {'type': 'chatgptDeviceCode', 'loginId': 'fixture-login', 'verificationUrl': 'https://auth.openai.com/device', 'userCode': 'FIXTURE'}
    save()
    emit({'id': message['id'], 'result': result})
    if method == 'initialize':
        for approval in state.get('seedApprovals', []):
            emit(approval)
    if event:
        emit(event)
