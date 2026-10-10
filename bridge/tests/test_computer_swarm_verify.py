"""Evidence gates reject preserved failures and false goal claims."""
import json
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'tools'))
import computer_swarm_verify as swarm  # noqa: E402


def copy_rows(folder):
    rows = swarm.be.load_rows(swarm.BASE / 'browser')
    folder.mkdir()
    path = folder / 'runs-test.jsonl'
    path.write_text(''.join(json.dumps(r)+'\n' for r in rows))
    return path, rows


def test_no_improvement_fails_and_graded_complete_positive_control_passes(tmp_path):
    path, rows = copy_rows(tmp_path / 'browser')
    with pytest.raises(AssertionError, match='did not improve'):
        swarm.improvement(path.parent)
    for row in rows:
        row['outcome'] = 'finished'
    path.write_text(''.join(json.dumps(r)+'\n' for r in rows))
    assert sum(r['autonomous'] for r in swarm.improvement(path.parent)['browser_after'].values()) == 54
    rows[0]['success'] = False  # contradict independent fixture state
    path.write_text(''.join(json.dumps(r)+'\n' for r in rows))
    with pytest.raises(AssertionError):
        swarm.improvement(path.parent)


def test_missing_paired_run_is_not_an_improvement(tmp_path):
    path, rows = copy_rows(tmp_path / 'browser')
    path.write_text(''.join(json.dumps(r)+'\n' for r in rows[:-1]))
    with pytest.raises(AssertionError):
        swarm.improvement(path.parent)
