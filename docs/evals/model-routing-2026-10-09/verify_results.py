#!/usr/bin/env python3
"""Independently rescore a saved historical screen. Offline; no credentials."""
from __future__ import annotations

import argparse
from decimal import Decimal
import hashlib
import importlib.util
import json
import math
from pathlib import Path
import statistics
import sys

BASE = Path(__file__).resolve().parent
sys.dont_write_bytecode = True
LUNA = 'openai/gpt-6-luna'
DEEP = 'deepseek/deepseek-v4.1-flash'
HISTORICAL_CHOICES = {
    'summary': ['openai/gpt-5.4-nano', DEEP, 'anthropic/claude-haiku-5.5', LUNA],
    'watch': ['google/gemini-3.1-flash-lite', DEEP, 'anthropic/claude-haiku-5.5', LUNA],
    'task_assessment': ['openai/gpt-6-astra', LUNA, 'openai/gpt-6.1-sol'],
    'reflection': ['google/gemini-3.1-flash-lite', 'openai/gpt-6.1-sol', LUNA],
    'web_synthesis': ['google/gemini-3.1-flash-lite', 'google/gemini-3.8-flash', LUNA],
    'memory_extraction': ['openai/gpt-5.4-nano', LUNA],
}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def checker_module():
    for line in (BASE / 'SHA256SUMS').read_text().splitlines():
        digest, name = line.split('  ', 1)
        require(name in {'BAR.md', 'cases.json', 'check.py'}, 'unexpected frozen file')
        require(hashlib.sha256((BASE / name).read_bytes()).hexdigest() == digest, 'frozen input changed: ' + name)
    spec = importlib.util.spec_from_file_location('role_screen_checker', BASE / 'check.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def select_winners(summary):
    """Apply the frozen all-cases/cost/latency rule using decimal costs."""
    winners = {}
    for role in sorted({row['role'] for row in summary}):
        group = [row for row in summary if row['role'] == role and row['cases'] == 3
                 and row['passed'] == 3 and row['critical'] == 0 and row['errors'] == 0]
        missing = sorted(row['model'] for row in group if row['cost_usd'] is None)
        if missing:
            winners[role] = {'selected': [], 'reason': 'incomplete_cost_ranking', 'eligible_with_missing_cost': missing}
            continue
        eligible = group
        if not eligible:
            winners[role] = {'selected': [], 'reason': 'no_eligible_complete_cost_data'}
            continue
        lowest = min(Decimal(str(row['cost_usd'])) for row in eligible)
        tied = [row for row in eligible if Decimal(str(row['cost_usd'])) <= lowest * Decimal('1.10')]
        fastest = min(row['median_seconds'] for row in tied)
        fastest_group = [row for row in tied if row['median_seconds'] == fastest]
        best_cost = min(Decimal(str(row['cost_usd'])) for row in fastest_group)
        selected = sorted(row['model'] for row in fastest_group if Decimal(str(row['cost_usd'])) == best_cost)
        winners[role] = {
            'selected': selected, 'lowest_cost_usd': float(lowest),
            'cost_tie_group': sorted(row['model'] for row in tied),
            'eligible_with_missing_cost': sorted(row['model'] for row in group if row['cost_usd'] is None),
        }
    return winners


def verify(screen):
    checker = checker_module()
    manifest = checker.load_manifest()
    cases = {case['id']: case for case in manifest['cases']}
    expected_jobs = {(case['id'], model) for case in cases.values() for model in HISTORICAL_CHOICES[case['role']]}
    rows = [checker.strict_json(path.read_text()) for path in sorted(screen.glob('*.json')) if path.name != 'summary.json']
    jobs = [(row['case_id'], row['model']) for row in rows]
    require(len(jobs) == len(set(jobs)) and set(jobs) == expected_jobs, 'missing, duplicated, or unexpected response')
    ids = [row.get('request_id') for row in rows]
    require(all(ids) and len(ids) == len(set(ids)), 'missing or duplicate request evidence')
    for row in rows:
        case = cases[row['case_id']]
        require(row['role'] == case['role'], 'case role mismatch')
        require('error' not in row and isinstance(row.get('raw_text'), str), 'unsuccessful transport or missing response')
        scored = checker.score_text(case, row['raw_text'])
        require(row['score'] == scored, 'stored score does not match independent rescore')
        require(row['finish_reason'] == 'stop', 'response did not finish normally')
        require(type(row['seconds']) in (int, float) and math.isfinite(row['seconds']) and row['seconds'] >= 0, 'invalid latency')
        cost = row['usage'].get('cost')
        require(type(cost) in (int, float) and math.isfinite(cost) and cost >= 0, 'missing or invalid reported cost')
        details = row['usage'].get('cost_details', {})
        total = details.get('upstream_inference_cost')
        require(total is not None and math.isclose(total, cost, rel_tol=1e-12, abs_tol=1e-15), 'reported cost/detail mismatch')
    summary = []
    configs = {}
    for role, choices in HISTORICAL_CHOICES.items():
        for model in choices:
            group = [row for row in rows if row['role'] == role and row['model'] == model]
            require(len({json.dumps(row['config'], sort_keys=True) for row in group}) == 1, 'settings changed within a role/model')
            configs[role + ' ' + model] = group[0]['config']
            summary.append({
                'role': role, 'model': model, 'cases': len(group),
                'passed': sum(row['score']['passed'] for row in group),
                'critical': sum(row['score']['critical_error'] for row in group), 'errors': 0,
                'cost_usd': float(sum(Decimal(str(row['usage']['cost'])) for row in group)),
                'median_seconds': statistics.median(row['seconds'] for row in group),
            })
    stored = checker.strict_json((screen / 'summary.json').read_text())
    require(len(stored) == len(summary), 'summary row count mismatch')
    by_key = {(row['role'], row['model']): row for row in stored}
    require(len(by_key) == len(stored), 'duplicate summary group')
    for row in summary:
        other = by_key.get((row['role'], row['model']))
        require(other is not None and set(other) == set(row), 'summary fields differ')
        for key, value in row.items():
            equal = math.isclose(value, other[key], rel_tol=1e-12, abs_tol=1e-15) if key == 'cost_usd' else value == other[key]
            require(equal, 'summary measurement mismatch: ' + key)
    return {
        'responses': len(rows), 'passed_responses': sum(row['score']['passed'] for row in rows),
        'role_model_comparisons': len(summary),
        'total_cost_usd': float(sum(Decimal(str(row['usage']['cost'])) for row in rows)),
        'total_cached_input_tokens': sum(row['usage'].get('prompt_tokens_details', {}).get('cached_tokens', 0) for row in rows),
        'summary': summary, 'winners': select_winners(summary), 'configurations': configs,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--screen', required=True, type=Path)
    args = parser.parse_args()
    try:
        report = verify(args.screen)
        print('result verification passed: ' + json.dumps({key: report[key] for key in ['responses', 'passed_responses', 'role_model_comparisons', 'total_cost_usd', 'winners']}, sort_keys=True))
    except (ValueError, KeyError, TypeError, OSError) as error:
        print('result verification failed: ' + str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
