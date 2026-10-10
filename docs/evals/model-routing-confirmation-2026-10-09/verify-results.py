#!/usr/bin/env python3
"""Offline independent audit; --write publishes only allowlisted aggregate data."""
from __future__ import annotations

import argparse
from collections import defaultdict
from copy import deepcopy
from decimal import Decimal
import hashlib
import importlib.util
import json
from pathlib import Path
import statistics
import sys

sys.dont_write_bytecode = True
BASE = Path(__file__).resolve().parent
DEFAULT_PRIVATE = Path('.test-artifacts/model-routing')


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False).encode()).hexdigest()


def read(path, decimal=False):
    return json.loads(path.read_text(), parse_float=Decimal if decimal else float)


def fixed(value):
    return format(Decimal(value), 'f')


def check_config(request, catalog):
    model, role, config = request['model'], request['role'], request['config']
    entry = catalog[model]
    supported = set(entry['supported_parameters'])
    require(config['max_tokens'] == 2048, 'output cap mismatch')
    require(not set(config) - supported and 'response_format' in supported, 'unsupported settings')
    require(('temperature' in config) == ('temperature' in supported), 'temperature capability mismatch')
    require(config.get('temperature', 0) == 0, 'nonzero temperature')
    if model == 'google/gemini-3.1-flash-lite':
        expected = None
    elif role == 'watch' and model in {'deepseek/deepseek-v4.1-flash', 'anthropic/claude-haiku-5.5'}:
        expected = {'enabled': False}
    elif role == 'watch':
        expected = {'effort': 'none'}
    else:
        expected = {'effort': 'medium' if role == 'reflection' else 'low'}
    require(config.get('reasoning') == expected, 'role reasoning mismatch')
    if expected and 'effort' in expected:
        require(expected['effort'] in entry.get('reasoning', {}).get('supported_efforts', []), 'unsupported effort')
    if expected == {'enabled': False}:
        require(not entry.get('reasoning', {}).get('mandatory'), 'cannot disable reasoning')


def verify_row(row, request, case, checker):
    for key in ['case_id', 'role', 'model', 'config', 'request_sha256', 'config_sha256']:
        require(row[key] == request[key], 'response and plan differ')
    provider = {'require_parameters': True, 'allow_fallbacks': False}
    body = {'model': request['model'], **request['config'], 'messages': case['messages'],
            'response_format': case['response_format'], 'usage': {'include': True}, 'provider': provider}
    require(digest(body) == request['request_sha256'], 'request hash mismatch')
    require(digest({'model': request['model'], **request['config'], 'provider': provider}) == request['config_sha256'], 'config hash mismatch')
    require('error' not in row and row['finish_reason'] == 'stop', 'response failed or truncated')
    require(row['response_model'] == request['model'], 'returned model mismatch')
    require(isinstance(row['response_provider'], str) and row['response_provider'], 'missing provider')
    require(isinstance(row['request_id'], str) and row['request_id'], 'missing request identity')
    scored = checker.score_text(case, row['raw_text'])
    require(scored == row['score'] and scored['passed'] and not scored['critical_error'], 'independent rescore failed')
    cost = row['usage'].get('cost')
    require(type(cost) in {int, Decimal} and Decimal(cost).is_finite() and cost >= 0, 'missing or invalid cost')
    seconds = row['seconds']
    require(type(seconds) in {int, Decimal} and Decimal(seconds).is_finite() and seconds > 0, 'invalid elapsed time')
    for key in ['prompt_tokens', 'completion_tokens', 'total_tokens']:
        require(type(row['usage'][key]) is int and row['usage'][key] >= 0, 'invalid token count')
    require(row['usage']['total_tokens'] == row['usage']['prompt_tokens'] + row['usage']['completion_tokens'], 'token arithmetic mismatch')
    return scored


def winners(rows):
    selected = {}
    for role in sorted({r['role'] for r in rows}):
        eligible = [r for r in rows if r['role'] == role and r['passed'] == r['cases'] == 3]
        require(eligible and all(r['cost_usd'] is not None for r in eligible), 'incomplete cost ranking')
        minimum = min(Decimal(r['cost_usd']) for r in eligible)
        tied = [r for r in eligible if Decimal(r['cost_usd']) <= minimum * Decimal('1.10')]
        best = min((Decimal(r['median_seconds']), Decimal(r['cost_usd'])) for r in tied)
        chosen = [r['model'] for r in tied if (Decimal(r['median_seconds']), Decimal(r['cost_usd'])) == best]
        selected[role] = {'selected': sorted(chosen), 'cost_tie_group': sorted(r['model'] for r in tied)}
    return selected


def positive_controls(row, request, case, checker, catalog):
    mutations = []
    wrong = deepcopy(row); wrong['raw_text'] = '{}'; mutations.append(wrong)
    wrong = deepcopy(row); wrong['config']['max_tokens'] = 1; mutations.append(wrong)
    wrong = deepcopy(row); wrong['request_sha256'] = 'invalid'; mutations.append(wrong)
    wrong = deepcopy(row); wrong['usage'].pop('cost'); mutations.append(wrong)
    wrong = deepcopy(row); wrong['response_model'] = 'synthetic-wrong-model'; mutations.append(wrong)
    wrong = deepcopy(row); wrong['finish_reason'] = 'length'; mutations.append(wrong)
    for wrong in mutations:
        try:
            verify_row(wrong, request, case, checker)
        except ValueError:
            pass
        else:
            raise ValueError('corrupted response control accepted')
    changed = deepcopy(request)
    if 'temperature' in changed['config']:
        changed['config'].pop('temperature')
    else:
        changed['config']['temperature'] = 0
    try:
        check_config(changed, catalog)
    except ValueError:
        pass
    else:
        raise ValueError('temperature control accepted')
    rows = [{'role': 'watch', 'model': m, 'cases': 3, 'passed': 3, 'cost_usd': c, 'median_seconds': t}
            for m, c, t in [('A', '1', '3'), ('B', '1.10', '2'), ('C', '1.10001', '1')]]
    require(winners(rows)['watch']['selected'] == ['B'], 'cost boundary control failed')
    rows[1]['passed'] = 2
    require(winners(rows)['watch']['selected'] == ['A'], 'failed arm control selected')
    return 9


def check_benchmark_types(kinds):
    require(kinds and not any("swe" in k.lower() for k in kinds), "unexpected SWE metric; revisit report")


def audit(private):
    frozen = read(BASE / 'FREEZE.json')
    require(set(frozen['sha256']) == {'cases.json', 'BAR.md', 'check.py', 'REVIEW.md', 'verify-freeze.py'}, 'frozen file set mismatch')
    for name, sha in frozen['sha256'].items():
        require(hashlib.sha256((BASE / name).read_bytes()).hexdigest() == sha, 'frozen input changed')
    spec = importlib.util.spec_from_file_location('confirmation_checker', BASE / 'check.py')
    checker = importlib.util.module_from_spec(spec); spec.loader.exec_module(checker)
    manifest = checker.load_manifest(); checker.selftest(manifest)
    cases = {c['id']: c for c in manifest['cases']}
    plan = read(private / 'confirmation/plan.json')
    require(plan['plan_sha256'] == digest({k: v for k, v in plan.items() if k != 'plan_sha256'}), 'plan digest mismatch')
    for field, filename in [('cases_sha256', 'cases.json'), ('checker_sha256', 'check.py'), ('bar_sha256', 'BAR.md')]:
        require(plan[field] == frozen['sha256'][filename], 'plan frozen input mismatch')
    require(plan['temperature_policy'] == 'matched', 'sampling policy mismatch')
    raw_catalog = read(private / 'full-catalog.json')
    catalog = {r['id']: r for r in (raw_catalog['data'] if isinstance(raw_catalog, dict) else raw_catalog)}
    require(digest(catalog) == plan['catalog_sha256'], 'catalog hash mismatch')
    requests = plan['requests']
    require(len(requests) == 57 and len({r['request_sha256'] for r in requests}) == 57, 'request completeness mismatch')
    expected_files = {r['request_sha256'] + '.json' for r in requests} | {'plan.json', 'summary.json'}
    require({f.name for f in (private / 'confirmation').glob('*.json')} == expected_files, 'missing or extra records')
    groups = defaultdict(list); identities = set()
    for request in requests:
        require(request['case_id'] in cases and request['role'] == cases[request['case_id']]['role'], 'case identity mismatch')
        check_config(request, catalog)
        row = read(private / 'confirmation' / (request['request_sha256'] + '.json'), decimal=True)
        verify_row(row, request, cases[request['case_id']], checker)
        require(row['request_id'] not in identities, 'duplicate request identity')
        identities.add(row['request_id']); groups[(row['role'], row['model'])].append(row)
    require(len(groups) == 19, 'role/configuration count mismatch')
    summary = []; observations = []
    supplied = {(r['role'], r['model']): r for r in read(private / 'confirmation/summary.json', decimal=True)}
    require(set(supplied) == set(groups), 'summary coverage mismatch')
    for (role, model), rows in sorted(groups.items()):
        require(len(rows) == 3 and {r['case_id'] for r in rows} == {c['id'] for c in cases.values() if c['role'] == role}, 'missing or repeated role case')
        require(all(r['config'] == rows[0]['config'] for r in rows), 'mixed arm settings')
        total = sum((r['usage']['cost'] for r in rows), Decimal(0))
        median = statistics.median(r['seconds'] for r in rows)
        old = supplied[(role, model)]
        require(old['cases'] == old['passed'] == 3 and old['critical'] == old['errors'] == 0, 'saved summary scoring mismatch')
        require(abs(old['cost_usd'] - total) < Decimal('0.000000000000000001') and old['median_seconds'] == median, 'summary arithmetic mismatch')
        summary.append({'role': role, 'model': model, 'config': rows[0]['config'], 'cases': 3, 'passed': 3,
                        'critical_errors': 0, 'cost_usd': fixed(total), 'median_seconds': fixed(median),
                        'effective_reasoning': 'not independently observable',
                        'providers': sorted({r['response_provider'] for r in rows})})
        for row in sorted(rows, key=lambda r: r['case_id']):
            observations.append({'case_id': row['case_id'], 'role': role, 'model': model, 'passed': True,
                                 'cost_usd': fixed(row['usage']['cost']), 'seconds': fixed(row['seconds']),
                                 'prompt_tokens': row['usage']['prompt_tokens'], 'completion_tokens': row['usage']['completion_tokens'],
                                 'reasoning_tokens': row['usage'].get('completion_tokens_details', {}).get('reasoning_tokens')})
    benchmark = read(private / 'benchmarks.json')
    kinds = sorted({r['benchmark_type'] for r in benchmark['data'] if 'benchmark_type' in r})
    check_benchmark_types(kinds)
    try:
        check_benchmark_types(kinds + ["deep_swe"])
    except ValueError:
        pass
    else:
        raise ValueError("SWE presence control accepted")
    request = requests[0]; row = read(private / 'confirmation' / (request['request_sha256'] + '.json'), decimal=True)
    controls = 1 + positive_controls(row, request, cases[request['case_id']], checker, catalog)
    return {'status': 'fresh_confirmation', 'cases_per_role': 3, 'responses': len(identities), 'passed': len(identities),
            'role_model_configurations': len(groups), 'critical_errors': 0, 'saved_transport_errors': 0,
            'total_cost_usd': fixed(sum((Decimal(r['cost_usd']) for r in summary), Decimal(0))),
            'cached_prompt_tokens': sum(r['usage'].get('prompt_tokens_details', {}).get('cached_tokens', 0) for rows in groups.values() for r in rows),
            'decimal_encoding': 'Costs and durations are exact decimal strings from saved JSON numbers.',
            'verification_positive_controls': controls,
            'frozen_sha256': frozen['sha256'], 'plan_sha256': plan['plan_sha256'], 'catalog_sha256': plan['catalog_sha256'],
            'benchmark_context': {'as_of': benchmark['meta']['as_of'], 'benchmark_types': kinds, 'deep_swe_returned': False},
            'selection': winners(summary), 'summary': summary, 'observations': observations,
            'limits': ['Three short synthetic exact-JSON cases per role.', 'Production prompts and long contexts are not evaluated.',
                       'Requested reasoning settings are recorded; effective internal effort is not independently observable.',
                       'No vision or actual-path smoke results are included in this confirmation score.']}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--private', type=Path, default=DEFAULT_PRIVATE)
    parser.add_argument('--write', action='store_true')
    args = parser.parse_args()
    results = audit(args.private)
    if args.write:
        (BASE / 'results.json').write_text(json.dumps(results, indent=2) + '\n')
    else:
        require(read(BASE / 'results.json') == results, 'published results differ from independent audit')
    print(f"confirmation results independently verified: {results['passed']}/{results['responses']} passed, {results['role_model_configurations']} configurations, {results['verification_positive_controls']} controls")


if __name__ == '__main__':
    try:
        main()
    except (ValueError, KeyError, TypeError, OSError) as error:
        print('confirmation audit failed: ' + str(error), file=sys.stderr)
        raise SystemExit(1)
