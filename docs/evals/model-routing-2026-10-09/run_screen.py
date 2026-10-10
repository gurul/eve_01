#!/usr/bin/env python3
"""Plan a role screen offline; --run explicitly authorizes API calls and charges."""
from __future__ import annotations

import argparse
import concurrent.futures as futures
from copy import deepcopy
import hashlib
import importlib.util
import json
import math
import os
from pathlib import Path
import statistics
import sys
import time
import urllib.error
import urllib.request

BASE = Path(__file__).resolve().parent
ENDPOINT = 'https://openrouter.ai/api/v1/chat/completions'
sys.dont_write_bytecode = True
LUNA = 'openai/gpt-6-luna'
DEEP = 'deepseek/deepseek-v4.1-flash'
LITE = 'google/gemini-3.1-flash-lite'
HAIKU = 'anthropic/claude-haiku-5.5'
DEFAULT_MODELS = {
    'summary': ['openai/gpt-5.4-nano', DEEP, HAIKU, LUNA],
    'watch': [LITE, DEEP, HAIKU, LUNA],
    'task_assessment': ['openai/gpt-6-astra', LUNA, 'openai/gpt-6.1-sol'],
    'reflection': [LITE, 'openai/gpt-6.1-sol', LUNA],
    'web_synthesis': [LITE, 'google/gemini-3.8-flash', LUNA],
    'memory_extraction': ['openai/gpt-5.4-nano', LUNA],
}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False).encode()).hexdigest()


def historical_settings(role, model):
    if model == LITE:
        return {'max_tokens': 2048, 'temperature': 0}
    if role == 'watch' and model in (DEEP, HAIKU):
        return {'max_tokens': 2048, 'reasoning': {'enabled': False}, **({'temperature': 0} if model == DEEP else {})}
    if role == 'watch' and model == LUNA:
        return {'max_tokens': 2048, 'reasoning': {'effort': 'none'}}
    return {'max_tokens': 2048, 'reasoning': {'effort': 'medium' if role == 'reflection' else 'low'}}


def settings(role, model, catalog, policy):
    config = historical_settings(role, model)
    require(model in catalog, 'catalog entry missing for ' + model)
    entry = catalog[model]
    supported = set(entry.get('supported_parameters', []))
    if policy == 'matched':
        config.pop('temperature', None)
        if 'temperature' in supported:
            config['temperature'] = 0
    require(not set(config) - supported, 'unsupported request settings for ' + model)
    require('response_format' in supported, 'structured response format unavailable for ' + model)
    reasoning = config.get('reasoning', {})
    if 'effort' in reasoning:
        require(reasoning['effort'] in entry.get('reasoning', {}).get('supported_efforts', []), 'unsupported reasoning effort for ' + model)
    if reasoning.get('enabled') is False:
        require(not entry.get('reasoning', {}).get('mandatory', False), 'reasoning cannot be disabled for ' + model)
    return config


def load_catalog(paths):
    catalog = {}
    for path in paths:
        data = json.loads(path.read_text())
        rows = data.get('data', []) if isinstance(data, dict) else data
        require(isinstance(rows, list), 'catalog must be a list or contain data list')
        for row in rows:
            catalog[row['id']] = row
    return catalog


def load_checker(path):
    spec = importlib.util.spec_from_file_location('role_screen_checker', path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def prepare(cases_path, checker_path, bar_path, catalogs, models, policy):
    checker = load_checker(checker_path)
    # Test the checker on its own bundled controls; custom cases can have new IDs.
    checker.selftest(checker.load_manifest())
    manifest = checker.load_manifest(cases_path)
    require(set(models) == set(manifest['roles']), 'model map must cover every role exactly')
    require(all(isinstance(choices, list) and choices and len(choices) == len(set(choices)) and all(isinstance(model, str) and model for model in choices) for choices in models.values()), 'empty or repeated model choices')
    catalog = load_catalog(catalogs)
    jobs = []
    for case in manifest['cases']:
        for model in models[case['role']]:
            config = settings(case['role'], model, catalog, policy)
            body = {'model': model, **config, 'messages': case['messages'], 'response_format': case['response_format'],
                    'usage': {'include': True}, 'provider': {'require_parameters': True, 'allow_fallbacks': False}}
            jobs.append({'case': case, 'model': model, 'config': config, 'body': body,
                         'request_sha256': digest(body), 'config_sha256': digest({'model': model, **config, 'provider': body['provider']})})
    plan = {
        'cases_sha256': hashlib.sha256(cases_path.read_bytes()).hexdigest(),
        'checker_sha256': hashlib.sha256(checker_path.read_bytes()).hexdigest(),
        'bar_sha256': hashlib.sha256(bar_path.read_bytes()).hexdigest(),
        'catalog_sha256': digest(catalog), 'temperature_policy': policy,
        'requests': [{'case_id': job['case']['id'], 'role': job['case']['role'], 'model': job['model'], 'config': job['config'],
                      'request_sha256': job['request_sha256'], 'config_sha256': job['config_sha256']} for job in jobs],
    }
    plan['plan_sha256'] = digest(plan)
    return checker, jobs, plan


def run_job(job, key, output, checker, timeout):
    path = output / (job['request_sha256'] + '.json')
    if path.exists():
        raise FileExistsError('response already exists; refusing another request')
    started = time.perf_counter()
    row = {'case_id': job['case']['id'], 'role': job['case']['role'], 'model': job['model'], 'config': job['config'],
           'request_sha256': job['request_sha256'], 'config_sha256': job['config_sha256']}
    try:
        request = urllib.request.Request(ENDPOINT, data=json.dumps(job['body']).encode(),
                                         headers={'Authorization': 'Bearer ' + key, 'Content-Type': 'application/json', 'X-Title': 'eve model screen'})
        with urllib.request.urlopen(request, timeout=timeout) as response:
            payload = json.load(response)
        # Preserve response evidence even if its shape, finish, or content is invalid.
        row.update({'usage': payload.get('usage', {}), 'request_id': payload.get('id'),
                    'response_model': payload.get('model'), 'response_provider': payload.get('provider')})
        choice = payload['choices'][0]
        content = choice['message'].get('content') or ''
        row.update({'raw_text': content, 'finish_reason': choice.get('finish_reason'),
                    'score': checker.score_text(job['case'], content)})
    except Exception as error:
        row['error'] = type(error).__name__
        if isinstance(error, urllib.error.HTTPError):
            row['http_status'] = error.code
    row['seconds'] = round(time.perf_counter() - started, 3)
    # A new output directory and request hashes prevent stale reuse; nothing retries.
    with path.open('x') as target:
        json.dump(row, target, indent=2)
    return row


def summarize(rows):
    summary = []
    for role, model in sorted({(row['role'], row['model']) for row in rows}):
        group = [row for row in rows if row['role'] == role and row['model'] == model]
        costs = [row.get('usage', {}).get('cost') for row in group]
        valid_costs = all(type(cost) in (int, float) and math.isfinite(cost) and cost >= 0 for cost in costs)
        summary.append({'role': role, 'model': model, 'cases': len(group),
                        'passed': sum(row.get('score', {}).get('passed', False) and row.get('finish_reason') == 'stop' for row in group),
                        'critical': sum(row.get('score', {}).get('critical_error', False) for row in group),
                        'errors': sum('error' in row for row in group),
                        'cost_usd': math.fsum(costs) if valid_costs else None,
                        'median_seconds': statistics.median(row['seconds'] for row in group)})
    return summary


def selftest():
    from unittest.mock import patch
    from io import BytesIO
    import tempfile
    from verify_results import select_winners
    supported = ['max_tokens', 'reasoning', 'response_format', 'temperature']
    entry = {'supported_parameters': supported, 'reasoning': {'mandatory': False, 'supported_efforts': ['low', 'medium', 'none']}}
    catalog = {model: deepcopy(entry) for choices in DEFAULT_MODELS.values() for model in choices}
    for role, choices in DEFAULT_MODELS.items():
        for model in choices:
            require(settings(role, model, catalog, 'matched')['temperature'] == 0, 'matched temperature absent')
    no_temperature = deepcopy(catalog)
    no_temperature[LUNA]['supported_parameters'].remove('temperature')
    require('temperature' not in settings('summary', LUNA, no_temperature, 'matched'), 'unsupported temperature sent')
    try:
        settings('summary', LUNA, {}, 'matched')
    except ValueError:
        pass
    else:
        raise ValueError('missing catalog control did not fail')
    require(digest({'model': LUNA, 'effort': 'low'}) != digest({'model': LUNA, 'effort': 'medium'}), 'config change did not change hash')
    base = {'role': 'watch', 'cases': 3, 'passed': 3, 'critical': 0, 'errors': 0}
    rows = [{**base, 'model': 'A', 'cost_usd': 1, 'median_seconds': 2},
            {**base, 'model': 'B', 'cost_usd': 1.1, 'median_seconds': 1},
            {**base, 'model': 'C', 'cost_usd': 1.10001, 'median_seconds': 0.1}]
    require(select_winners(rows)['watch']['selected'] == ['B'], '10 percent boundary failed')
    rows[1]['passed'] = 2
    require(select_winners(rows)['watch']['selected'] == ['A'], 'failed model selected')
    rows[0]['cost_usd'] = None
    require(select_winners(rows)['watch']['selected'] == [] and select_winners(rows)['watch']['reason'] == 'incomplete_cost_ranking', 'missing cost must make ranking inconclusive')
    sample = {'role': 'summary', 'model': LUNA, 'score': {'passed': True, 'critical_error': False}, 'finish_reason': 'stop', 'seconds': 1, 'usage': {}}
    require(summarize([sample])[0]['cost_usd'] is None, 'missing usage cost treated as zero')
    with patch('urllib.request.urlopen', side_effect=AssertionError('network without opt-in')):
        require(main([]) == 0, 'offline default failed')
    checker = load_checker(BASE / 'check.py')
    case = checker.load_manifest()['cases'][0]
    body = {'model': LUNA, 'messages': case['messages'], 'response_format': case['response_format'], 'max_tokens': 2048}
    job = {'case': case, 'model': LUNA, 'config': {'max_tokens': 2048}, 'body': body,
           'request_sha256': digest(body), 'config_sha256': digest({'model': LUNA, 'max_tokens': 2048})}
    payload = {'id': 'synthetic-request', 'model': LUNA, 'provider': 'synthetic-provider', 'usage': {'cost': 0.1},
               'choices': [{'message': {'content': json.dumps(case['expected'])}, 'finish_reason': 'stop'}]}
    def fake_open(request, timeout):
        require(json.loads(request.data) == body and timeout == 1, 'transmitted request differs')
        return BytesIO(json.dumps(payload).encode())
    with tempfile.TemporaryDirectory() as directory, patch('urllib.request.urlopen', side_effect=fake_open):
        output = Path(directory)
        scored = run_job(job, 'synthetic-test-credential', output, checker, 1)
        require(scored['score']['passed'] and scored['response_model'] == LUNA and scored['response_provider'] == 'synthetic-provider', 'response provenance was not saved')
        require(json.loads((output / (digest(body) + '.json')).read_text()) == scored, 'response file differs')
        try:
            run_job(job, 'synthetic-test-credential', output, checker, 1)
        except FileExistsError:
            pass
        else:
            raise ValueError('response overwrite control did not fail')
    print('runner selftest passed: matched settings, missing catalog, config hashes, tie boundaries, missing costs, offline default, mock request/response, overwrite refusal')


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--run', action='store_true', help='explicitly authorize network requests and provider charges')
    parser.add_argument('--selftest', action='store_true')
    parser.add_argument('--cases', type=Path, default=BASE / 'cases.json')
    parser.add_argument('--checker', type=Path, default=BASE / 'check.py')
    parser.add_argument('--bar', type=Path, help='defaults to BAR.md beside the cases')
    parser.add_argument('--catalog', type=Path, action='append', default=[], help='saved model catalog; repeat to merge, with later entries winning')
    parser.add_argument('--models', type=Path, help='optional JSON role-to-model-list map')
    parser.add_argument('--temperature-policy', choices=['matched', 'historical'], default='matched')
    parser.add_argument('--out', type=Path, help='new output directory; an existing directory is refused')
    parser.add_argument('--workers', type=int, default=6)
    parser.add_argument('--timeout', type=float, default=60)
    args = parser.parse_args(argv)
    if args.selftest:
        require(not args.run, '--selftest and --run cannot be combined')
        selftest()
        return 0
    if not args.catalog:
        require(not args.run, '--run requires saved catalogs covering every candidate')
        print('Offline: no requests made. Supply --catalog to inspect a request plan; add --run and --out to authorize charges.')
        return 0
    require(args.workers > 0 and math.isfinite(args.timeout) and args.timeout > 0, 'workers and timeout must be positive')
    models = json.loads(args.models.read_text()) if args.models else DEFAULT_MODELS
    checker, jobs, plan = prepare(args.cases, args.checker, args.bar or args.cases.with_name('BAR.md'), args.catalog, models, args.temperature_policy)
    if not args.run:
        print(json.dumps(plan, indent=2))
        return 0
    require(args.out is not None, '--run requires --out')
    require(not args.out.exists(), 'output directory already exists; refusing replay or overwrite')
    key = os.environ.get('OPENROUTER_API_KEY', '').strip()
    require(bool(key), 'OPENROUTER_API_KEY must be set in the process environment')
    args.out.mkdir(parents=True)
    (args.out / 'plan.json').write_text(json.dumps(plan, indent=2))
    with futures.ThreadPoolExecutor(max_workers=args.workers) as pool:
        rows = list(pool.map(lambda job: run_job(job, key, args.out, checker, args.timeout), jobs))
    (args.out / 'summary.json').write_text(json.dumps(summarize(rows), indent=2))
    print(json.dumps({'responses': len(rows), 'errors': sum('error' in row for row in rows), 'plan_sha256': plan['plan_sha256']}))
    return 1 if any('error' in row or not row.get('score', {}).get('passed') or row.get('finish_reason') != 'stop' for row in rows) else 0


if __name__ == '__main__':
    try:
        raise SystemExit(main())
    except (ValueError, KeyError, TypeError, OSError) as error:
        print('runner refused: ' + str(error), file=sys.stderr)
        raise SystemExit(2)
