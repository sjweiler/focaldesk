#!/usr/bin/env python3
"""Check live stages, freshness, streaming data, and dashboard query semantics."""
import argparse
import json
import math
import urllib.parse
import urllib.request

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--smoke', action='store_true', help='also require a recent metrics_smoke run')
args = parser.parse_args()


def read(url):
    with urllib.request.urlopen(url, timeout=10) as response:
        return json.load(response)


def query(expression):
    result = read('http://127.0.0.1:9090/api/v1/query?' + urllib.parse.urlencode({'query': expression}))
    assert result['status'] == 'success', result
    return result['data']['result']


def values_by_stage(rows):
    return {(r['metric'].get('service_name'), r['metric'].get('stage')): float(r['value'][1]) for r in rows}


rows = query('focaldesk_operation_requests_total')
seen = {(r['metric'].get('service_name'), r['metric']['stage'], r['metric']['outcome']): float(r['value'][1]) for r in rows}
expected = {('focaldesk-server', stage, 'ok') for stage in ['request', 'embedding', 'inference', 'vector_search']}
expected.add(('focal-vector', 'vector_search', 'ok'))
assert all(seen.get(key, 0) > 0 for key in expected), ('Missing completed live operations', seen)
if args.smoke:
    for outcome in ['ok', 'error', 'timeout', 'cancelled']:
        assert seen.get(('focaldesk-metrics-smoke', 'request', outcome)) == 1
assert query('up{job="focaldesk"}')[0]['value'][1] == '1'
health = {r['metric']['service_name']: float(r['value'][1]) for r in query('focaldesk_service_telemetry_fresh')}
for service in ['focaldesk-server', 'focal-vector', 'focaldesk-ai-console']:
    assert health.get(service) == 1, ('Stale or missing exporter', service, health)
for row in query('focaldesk_operation_duration_seconds_count'):
    metric = row['metric']
    key = (metric.get('service_name'), metric['stage'], metric['outcome'])
    assert float(row['value'][1]) == seen[key], ('Histogram/counter mismatch', key)

# Streaming measurements must exist independently of chunk sizes or text content.
for metric in ['focaldesk_inference_first_content_seconds_count', 'focaldesk_inference_stream_duration_seconds_count']:
    streams = query(f'{metric}{{service_name="focaldesk-server"}}')
    assert streams and float(streams[0]['value'][1]) >= 2, ('Run two successful streams first', metric)
tokens = query('focaldesk_inference_output_tokens_total{service_name="focaldesk-server"}')
assert tokens and float(tokens[0]['value'][1]) > 0

dashboard = read('http://127.0.0.1:3000/api/dashboards/uid/focaldesk-ai')['dashboard']
assert dashboard['title'] == 'FocalDesk AI Operations'
assert len(dashboard['panels']) == 18
panels = {}
for panel in dashboard['panels']:
    for target in panel['targets']:
        expression = (target['expr'].replace('$service', 'focaldesk-server|focal-vector')
                      .replace('$stage', '.*').replace('$__rate_interval', '15m').replace('$__range', '15m'))
        results = query(expression)
        assert results, ('No dashboard data', panel['title'])
        if not panel['title'].startswith('p95'):
            assert any(math.isfinite(float(r['value'][1])) for r in results), ('No finite dashboard data', panel['title'])
        panels[panel['title']] = results
    print('Verified panel:', panel['title'])
samples = values_by_stage(panels['Completed samples in selected range'])
p95 = values_by_stage(panels['p95 latency — selected range'])
for key, count in samples.items():
    if 0 < count < 20:
        assert key in p95 and math.isnan(p95[key]), ('Sparse p95 must be hidden', key, count, p95)
    elif count >= 20:
        assert key in p95 and math.isfinite(p95[key]), ('Sufficient p95 must be shown', key, count, p95)
phases = query('focaldesk_ollama_phase_duration_seconds_count{service_name="focaldesk-server"}')
assert {r['metric']['phase'] for r in phases} == {'model_load','prompt_eval','generation'}
assert all(float(r['value'][1]) >= 2 for r in phases)
print('Verified fresh exporters, active/age gauges, streaming and Ollama phase measurements, selected-range counts, and sparse-p95 gating.')
print('Dashboard: http://localhost:3000/d/focaldesk-ai/focaldesk-ai-operations')
