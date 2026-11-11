#!/usr/bin/env python3
"""Confirm that a recent real AI request spans the client, server, and vector daemon."""
import json
import urllib.parse
import urllib.request

query = urllib.parse.urlencode({'service': 'focaldesk-cli', 'operation': 'ai.request', 'limit': 20})
with urllib.request.urlopen('http://127.0.0.1:16686/api/traces?' + query, timeout=10) as response:
    traces = json.load(response)['data']
required = {'ai.request', 'ai_ipc_request', 'ai.chat', 'ai.embedding',
            'memory.search', 'focal_vector.query', 'focal_vector.request',
            'focal_vector.search', 'ai.inference'}
for trace in sorted(traces, key=lambda t: max(s['startTime'] for s in t['spans']), reverse=True):
    spans = trace['spans']
    names = {s['operationName'] for s in spans}
    if not required <= names:
        continue
    by_id = {s['spanID']: s for s in spans}
    services = {p['serviceName'] for p in trace['processes'].values()}
    assert {'focaldesk-cli', 'focaldesk-server', 'focal-vector'} <= services
    query_span = next(s for s in spans if s['operationName'] == 'focal_vector.query')
    vector_span = next(s for s in spans if s['operationName'] == 'focal_vector.request'
                       and any(r['spanID'] == query_span['spanID'] for r in s['references']))
    assert trace['processes'][vector_span['processID']]['serviceName'] == 'focal-vector'
    root = next(s for s in spans if s['operationName'] == 'ai.request')
    server = next(s for s in spans if s['operationName'] == 'ai_ipc_request')
    assert any(r['spanID'] == root['spanID'] for r in server['references'])
    inference = next(s for s in spans if s['operationName'] == 'ai.inference')
    status = {t['key']: t['value'] for t in inference['tags']}
    assert status.get('outcome') == 'ok', status
    print('Verified 3 services and both IPC parent links.')
    print('Trace: http://localhost:16686/trace/' + trace['traceID'])
    for s in sorted(spans, key=lambda s: s['startTime']):
        service = trace['processes'][s['processID']]['serviceName']
        print(f"{service:20} {s['operationName']:30} {s['duration']/1000:10.3f} ms")
    break
else:
    raise SystemExit('No complete live documentation trace found. Seen operations: ' +
                     ', '.join(sorted({s['operationName'] for t in traces for s in t['spans']})))
