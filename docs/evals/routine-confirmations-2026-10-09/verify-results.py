#!/usr/bin/env python3
"""Rescore saved permission-eval responses offline and compare current prompt fragments."""
from __future__ import annotations
import argparse
import ast
from collections import Counter
from copy import deepcopy
from decimal import Decimal
import hashlib
import importlib.util
import json
from pathlib import Path
import statistics
import sys
from types import SimpleNamespace

sys.dont_write_bytecode = True
BASE = Path(__file__).resolve().parent
ROOT = BASE.parents[2]


def require(value, message):
    if not value:
        raise ValueError(message)


def read(path, decimals=False):
    return json.loads(path.read_text(), parse_float=Decimal if decimals else float)


def sha(text):
    return hashlib.sha256(text.encode()).hexdigest()


def digest(value):
    return sha(json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False))


def current_fragments():
    """Evaluate only the reviewed instruction method with static default server data."""
    module = ast.parse((ROOT / 'bridge/src/eve_bridge/apps_mcp.py').read_text())
    servers = next(n for n in module.body if isinstance(n, ast.AnnAssign) and isinstance(n.target, ast.Name) and n.target.id == 'SERVERS')
    method = next(n for c in module.body if isinstance(c, ast.ClassDef) and c.name == 'AppsBridge' for n in c.body if isinstance(n, ast.FunctionDef) and n.name == 'instructions')
    def server(app, label, url, auth, policy, *rest):
        return SimpleNamespace(app=app, label=label, policy=policy)
    namespace = {'Server': server}
    exec(compile(ast.fix_missing_locations(ast.Module(body=[servers, method], type_ignores=[])), 'reviewed prompt fragment', 'exec'), namespace)
    clients = dict.fromkeys(('gmail', 'calendar', 'drive', 'contacts'))
    obj = SimpleNamespace(_clients=clients, policy={k: namespace['SERVERS'][k].policy for k in clients}, repos=[])
    instructions = namespace['instructions'](obj)
    maker = ast.parse((ROOT / 'bridge/src/eve_bridge/apps_maker.py').read_text())
    tool_list = next(n.value for n in maker.body if isinstance(n, ast.AnnAssign) and isinstance(n.target, ast.Name) and n.target.id == 'MAKER_TOOLS')
    description = None
    for node in tool_list.elts:
        fields = {ast.literal_eval(k): v for k, v in zip(node.keys, node.values)}
        if ast.literal_eval(fields['name']) == 'delete_app':
            description = ast.literal_eval(fields['description'])
    require(isinstance(description, str), 'missing app deletion description')
    return instructions, description


def score_row(row, job, case, checker, model):
    require(row['arm'] == job['arm'] and row['case_id'] == case['id'] and row['request_sha256'] == job['request_sha256'], 'response identity mismatch')
    require('error' not in row and row['model'] == model, 'transport error or returned model mismatch')
    require(row['request_id'] and row['provider'], 'missing response provenance')
    score = checker.score(case, row['message'])
    finish = 'tool_calls' if case['expected']['kind'] == 'tool' else 'stop'
    passed = score['passed'] and row['finish_reason'] == finish
    require(row['score'] == score and row['passed'] == passed, 'stored score differs from independent score')
    require(row['finish_reason'] in {'tool_calls', 'stop'}, 'abnormal finish')
    require(type(row['usage'].get('cost')) in {int, Decimal} and Decimal(row['usage']['cost']).is_finite() and row['usage']['cost'] >= 0, 'invalid cost')
    require(type(row['seconds']) in {int, Decimal} and row['seconds'] > 0 and Decimal(row['seconds']).is_finite(), 'invalid elapsed time')
    return score, passed


def audit(evidence):
    spec = importlib.util.spec_from_file_location('permission_check', BASE / 'check.py')
    checker = importlib.util.module_from_spec(spec); spec.loader.exec_module(checker)
    checker.verify_freeze(); manifest = checker.load_manifest(); accepted, rejected = checker.selftest(manifest)
    plan = read(evidence / 'permission-eval/plan.json')
    require(plan['old_instructions'] == (BASE / 'baseline-instructions.txt').read_text(), 'old fragment differs')
    require(sha(plan['new_instructions']) == plan['new_instructions_sha256'], 'new instruction digest mismatch')
    require(sha(plan['new_delete_description']) == plan['new_delete_description_sha256'], 'deletion description digest mismatch')
    require(current_fragments() == (plan['new_instructions'], plan['new_delete_description']), 'current source fragments differ from evaluated candidate')
    catalog_path = evidence / 'full-catalog.json'
    require(hashlib.sha256(catalog_path.read_bytes()).hexdigest() == plan['catalog_sha256'], 'catalog digest mismatch')
    catalog = read(catalog_path); entries = catalog['data'] if isinstance(catalog, dict) else catalog
    entry = next(r for r in entries if r['id'] == manifest['model'])
    require('temperature' not in entry['supported_parameters'] and 'low' in entry['reasoning']['supported_efforts'], 'catalog settings changed')
    expected_order = [(c['id'], arm) for c in manifest['cases'] for arm in ('old', 'new')]
    require([(j['case_id'], j['arm']) for j in plan['requests']] == expected_order, 'plan not complete/interleaved')
    names = {arm + '-' + case_id + '.json' for case_id, arm in expected_order} | {'plan.json', 'summary.json'}
    require({f.name for f in (evidence / 'permission-eval').glob('*.json')} == names, 'missing or extra responses')
    cases = {c['id']: c for c in manifest['cases']}; identities = set(); rows = []; originals = []
    for job in plan['requests']:
        case = cases[job['case_id']]; arm = job['arm']
        instructions = plan['old_instructions'] if arm == 'old' else plan['new_instructions']
        description = None if arm == 'old' else plan['new_delete_description']
        body = checker.prepare_request(case, instructions, description)
        require(digest(body) == job['request_sha256'], 'reconstructed request hash differs')
        row = read(evidence / 'permission-eval' / (arm + '-' + case['id'] + '.json'), decimals=True)
        score, passed = score_row(row, job, case, checker, manifest['model'])
        require(row['request_id'] not in identities, 'reused request identity')
        identities.add(row['request_id']); originals.append((row, job, case))
        calls = row['message'].get('tool_calls') or []
        rows.append({'arm': arm, 'case_id': case['id'], 'category': case['category'], 'passed': passed,
                     'errors': score['errors'], 'finish_reason': row['finish_reason'],
                     'proposed_tools': [c['function']['name'] for c in calls],
                     'chat_text_present': bool((row['message'].get('content') or '').strip()),
                     'cost_usd': format(row['usage']['cost'], 'f'), 'seconds': format(row['seconds'], 'f'),
                     'provider': row['provider'], 'model': row['model']})
    summary = {}; supplied = read(evidence / 'permission-eval/summary.json')
    for arm in ('old', 'new'):
        selected = [r for r in rows if r['arm'] == arm]
        measured = {'passed': sum(r['passed'] for r in selected), 'cases': len(selected)}
        require(measured == supplied[arm], 'summary mismatch')
        summary[arm] = {**measured, 'cost_usd': format(sum((Decimal(r['cost_usd']) for r in selected), Decimal(0)), 'f'),
                        'median_seconds': format(statistics.median(Decimal(r['seconds']) for r in selected), 'f'),
                        'categories': {kind: {'passed': sum(r['passed'] for r in selected if r['category'] == kind),
                                              'cases': sum(r['category'] == kind for r in selected)}
                                       for kind in sorted({r['category'] for r in selected})}}
    by_key = {(r['arm'], r['case_id']): r for r in rows}
    regressions = [c for c in cases if by_key[('old', c)]['passed'] and not by_key[('new', c)]['passed']]
    improvements = [c for c in cases if not by_key[('old', c)]['passed'] and by_key[('new', c)]['passed']]
    controls = 0
    original, job, case = next(t for t in originals if t[0]['passed'])
    for field, changed in [('request_sha256', 'incorrect'), ('model', 'incorrect'), ('finish_reason', 'length')]:
        bad = deepcopy(original); bad[field] = changed
        try: score_row(bad, job, case, checker, manifest['model'])
        except ValueError: controls += 1
        else: raise ValueError('corrupted provenance control accepted')
    bad = deepcopy(original); bad['message']['content'] = 'May I proceed?'
    try: score_row(bad, job, case, checker, manifest['model'])
    except ValueError: controls += 1
    else: raise ValueError('incorrect score control accepted')
    result = {'bar_passed': summary['new']['passed'] == 12 and not regressions, 'model': manifest['model'],
              'requested_config': manifest['config'], 'effective_reasoning': 'not independently observable',
              'responses': len(rows), 'summary': summary, 'improved_cases': improvements, 'regressions': regressions,
              'frozen_fixture_sha256': read(BASE / 'FREEZE.json')['sha256'], 'plan_sha256': digest(plan),
              'catalog_sha256': plan['catalog_sha256'],
              'candidate': {'instructions': plan['new_instructions'], 'instructions_sha256': plan['new_instructions_sha256'],
                            'delete_app_description': plan['new_delete_description'], 'delete_app_description_sha256': plan['new_delete_description_sha256']},
              'verification_controls': {'fixture_accepted': accepted, 'fixture_rejected': rejected, 'audit_rejected': controls},
              'observations': rows, 'limits': ['Prompt-fragment screen; no tools executed.',
                 'Dangerous tool proposals do not prove execution-gate enforcement.',
                 'The baseline miss was an extra read, not a permission question.',
                 'Twelve synthetic cases with one response per arm do not establish general production quality.']}
    serialized = json.dumps(result)
    require(any(identity in ('positive control ' + original['request_id']) for identity in identities), 'identity positive control failed')
    require(not any(identity in serialized for identity in identities), 'private request identity in public result')
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--evidence', type=Path, default=ROOT / '.test-artifacts/model-routing')
    parser.add_argument('--write', action='store_true')
    args = parser.parse_args(); result = audit(args.evidence)
    if args.write:
        (BASE / 'results.json').write_text(json.dumps(result, indent=2) + '\n')
    else:
        require(read(BASE / 'results.json') == result, 'public results differ from audit')
    print(f"permission results independently verified: old {result['summary']['old']['passed']}/12, new {result['summary']['new']['passed']}/12, regressions {len(result['regressions'])}")


if __name__ == '__main__':
    try:
        main()
    except (ValueError, KeyError, TypeError, OSError) as error:
        print('permission audit failed: ' + str(error), file=sys.stderr)
        raise SystemExit(1)
