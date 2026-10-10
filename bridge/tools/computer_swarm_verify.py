"""Independently grade retained before/after computer-use evidence and render measured changes."""
from __future__ import annotations

import argparse
import json
import statistics
from collections import Counter
from pathlib import Path

import browser_model_eval as be
import computer_protocol_eval as ax
import computer_protocol_plot as plots
import computer_visual_eval as visual
import decisions_live_demo as desktop

BASE = Path('docs/computer-protocol-eval')


def browser_summary(folder):
    ax.check_browser(folder)
    rows = be.load_rows(folder)
    result = {}
    for model in sorted({r['model'] for r in rows}):
        subset = [r for r in rows if r['model'] == model]
        result[model] = {'n': len(subset), 'goals': sum(r['success'] for r in subset),
                         'autonomous': sum(r['success'] and r['outcome'] == 'finished' for r in subset),
                         'p50_secs': statistics.median(r['secs'] for r in subset),
                         'errors': sum(r['outcome'] == 'error' for r in subset)}
    return result


def improvement(folder):
    before, after = browser_summary(BASE / 'browser'), browser_summary(folder)
    assert before.keys() == after.keys()
    for model in before:
        assert after[model]['n'] == before[model]['n']
        assert after[model]['goals'] >= before[model]['goals']
        assert after[model]['errors'] <= before[model]['errors']
    assert sum(r['autonomous'] for r in after.values()) > sum(r['autonomous'] for r in before.values()), 'autonomous completion did not improve'
    return {'browser_before': before, 'browser_after': after}


def desktop_summary(path):
    desktop.check_report(path)
    cases = json.loads(path.read_text())['cases']
    return dict(Counter(r['classification'] for r in cases))


def summaries(folder):
    result = improvement(folder / 'browser')
    ax.check(folder / 'ax.json')
    visual.check(folder / 'visual.json')
    result['admission'] = plots.summaries(folder)['admission']
    result['desktop_before'] = desktop_summary(Path('docs/decisions-live-results.json'))
    result['desktop_after'] = desktop_summary(folder / 'desktop.json')
    return result


def render(folder, data):
    import matplotlib
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt

    names = list(data['browser_before'])
    x = list(range(len(names)))
    fig, axes = plt.subplots(2, 2, figsize=(13, 9), layout='constrained')
    fig.suptitle('Eve: measured computer-use improvements', fontsize=19, weight='bold')
    for key, shift, color, label in (('browser_before', -.18, '#929eaa', 'Before'),
                                   ('browser_after', .18, '#009e73', 'After')):
        axes[0, 0].bar([i+shift for i in x], [100*data[key][n]['autonomous']/data[key][n]['n'] for n in names],
                       width=.35, color=color, label=label)
        axes[0, 1].bar([i+shift for i in x], [data[key][n]['p50_secs'] for n in names], width=.35, color=color, label=label)
    axes[0, 0].set(title='Whole browser tasks finished autonomously', ylabel='Percent of 18 tasks / model', ylim=(0, 110))
    axes[0, 1].set(title='Whole browser task median wall time', ylabel='Seconds')
    for a in axes[0]:
        a.set(xticks=x, xticklabels=names)
        a.legend()
    admission = data['admission']
    labels = ['Guarded keyword', 'Guarded keyword\n+ Decisions (AX only)']
    bars = axes[1, 0].bar(labels, [r['coverage']*100 for r in admission.values()], color=['#929eaa', '#0072b2'])
    for bar, row in zip(bars, admission.values(), strict=True):
        axes[1, 0].text(bar.get_x()+bar.get_width()/2, bar.get_height()+2,
                        f"{row['coverage']:.1%}; {row['wrong_clicks']} wrong", ha='center')
    axes[1, 0].set(title='Guarded admission: frozen AX development fixtures', ylabel='Correct admitted inputs / click requests (%)', ylim=(0, 110))
    axes[1, 1].bar(['Before', 'After'], [data[k].get('verified_success', 0) for k in ('desktop_before', 'desktop_after')], color=['#929eaa', '#009e73'])
    axes[1, 1].set(title='Calculator: independently observed verified steps', ylabel='Verified steps / 5 requested inputs', ylim=(0, 5))
    fig.supxlabel('Browser: identical nine isolated tasks × two repeats per model; independent goal oracles.\n'
                  'AX admission uses recording effectors and simulated stable observations; it is not task success.\n'
                  'Calculator: CUA transport + real AX, eight requests; pixel staleness persists. Development evidence, not a deployment holdout.', fontsize=10)
    fig.savefig(folder / 'improvement.png', dpi=160)
    svg = folder / 'improvement.svg'
    fig.savefig(svg)
    svg.write_text('\n'.join(line.rstrip() for line in svg.read_text().splitlines())+'\n')
    plt.close(fig)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--browser', type=Path)
    p.add_argument('--evidence', type=Path)
    p.add_argument('--render', type=Path)
    args = p.parse_args()
    if args.browser:
        data = improvement(args.browser)
        print(json.dumps(data, indent=2))
        print('BROWSER_IMPROVED')
    elif args.evidence or args.render:
        folder = args.evidence or args.render
        data = summaries(folder)
        if args.render:
            (folder / 'improvement-summary.json').write_text(json.dumps(data, indent=2)+'\n')
            render(folder, data)
        else:
            assert json.loads((folder / 'improvement-summary.json').read_text()) == data
            import tempfile

            from PIL import Image, ImageChops
            with tempfile.TemporaryDirectory() as temporary:
                fresh = Path(temporary)
                render(fresh, data)
                with Image.open(folder / 'improvement.png') as saved, Image.open(fresh / 'improvement.png') as regenerated:
                    assert saved.size == regenerated.size
                    assert ImageChops.difference(saved.convert('RGB'), regenerated.convert('RGB')).getbbox() is None, 'stale improvement plot'
            with Image.open(folder / 'improvement.png') as im:
                assert im.width >= 1500 and im.height >= 1000
            assert 'Calculator' in (folder / 'improvement.svg').read_text()
            print('SWARM_EVIDENCE_CHECKED')
    else:
        p.error('choose --browser, --evidence or --render')


if __name__ == '__main__':
    main()
