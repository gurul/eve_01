from types import SimpleNamespace

import pytest
from test_fast_lane import Clock, Effectors

from eve_bridge import plan_contract as pc
from eve_bridge.plan_executor import _holds


def test_url_identity_is_exact_with_only_host_default_port_and_root_normalization():
    assert pc.url_matches('https://EXAMPLE.com:443', 'https://example.com/')
    for wrong in ('https://evil.test/article', 'http://example.com/article',
                  'https://example.com/Article', 'https://example.com/article/other',
                  'https://example.com/article?q=1', 'https://example.com/article#section',
                  'https://example.com:444/article', 'https://example.com:0/article', 'https://example.com/article-old'):
        assert not pc.url_matches(wrong, 'https://example.com/article')


@pytest.mark.parametrize('value', ['javascript:alert(1)', 'https://u:p@example.com/a', 'https://example.com:bad/a',
                                  'https://example.com/a b', '', 'https://example.com/' + 'a' * 2048,
                                  'https://example.com\\evil/a', 42])
def test_invalid_url_expectations_reject_whole_plan(value):
    with pytest.raises(pc.PlanError):
        pc.parse_plan({'steps': [{'kind': 'click', 'target': 'Article',
                                 'expect': {'kind': 'url_is', 'value': value}}]}, 'open article')


def test_urls_are_not_truncated_to_text_label_limit():
    value = 'https://example.com/' + 'a' * 150
    p = pc.parse_plan({'steps': [{'kind': 'click', 'target': 'Article',
                                 'expect': {'kind': 'url_is', 'value': value}}]}, 'open article')
    assert p.steps[0].expect.value == value


class Senses:
    def __init__(self, url, fail=False):
        self.url, self.fail, self.fresh, self.reads = url, fail, '', 0

    def snapshot(self):
        self.reads += 1
        if self.fail:
            raise RuntimeError('detached')
        self.fresh = self.url
        return SimpleNamespace(title='Generic destination')

    def current_url(self):
        return self.fresh

    def text_visible(self, text):
        return False


def holds(expect, senses):
    return _holds(expect, senses, Effectors(), lambda: 'browser', Clock(), wait=0)


def test_exact_url_requires_fresh_snapshot_and_does_not_replace_title_predicate():
    s = Senses('https://example.com/article')
    assert holds(pc.Expect('url_is', s.url), s) is True and s.reads == 1
    assert holds(pc.Expect('title_contains', 'Article'), s) is False
    assert holds(pc.Expect('url_is', 'https://example.com/other'), s) is False
    assert holds(pc.Expect('url_is', s.url), Senses(s.url, fail=True)) is False
    assert holds(pc.Expect('url_is', s.url), SimpleNamespace(snapshot=lambda: SimpleNamespace(title='Article'))) is False


def test_browser_planner_gets_observed_navigation_evidence():
    req = pc.plan_request('gpt-6-luna', 'open Article', app='browser', outline=['Article (link); destination: https://example.com/a'],
                          apps=[], effort='low', timeout=30, browser=True, current_url='https://example.com/')
    assert 'url_is' in req['instructions'] and 'Do not invent' in req['instructions']
    assert 'Observed current URL: https://example.com/' in req['input'][0]['content'][0]['text']


@pytest.mark.live
@pytest.mark.parametrize('destination', ['destination', 'wrong', 'noop'])
def test_real_browser_url_oracle_rejects_wrong_and_absent_navigation(tmp_path, destination):
    import asyncio

    from eve_bridge import browser_lane as bl

    lane = bl.BrowserLane(bl.BrowserLaneConfig(profile=tmp_path / 'profile', headless=True))
    plan = {'steps': [{'kind': 'click', 'target': 'Destination', 'label_hint': 'Destination',
                      'expect': {'kind': 'url_is', 'value': 'https://verify.test/destination'}}],
            'success': {'kind': 'url_is', 'value': 'https://verify.test/destination'}, 'final_say': 'Opened.'}
    async def go():
        clicks = []
        def prepare():
            page = lane._ensure()
            def serve(route):
                if route.request.url.endswith('/home'):
                    href = '/destination' if destination == 'noop' else '/' + destination
                    click = 'event.preventDefault()' if destination == 'noop' else ''
                    body = f'<title>Home</title><a href="{href}" onclick="{click}">Destination</a><button>Other action</button>'
                else:
                    body = '<title>Generic destination</title><p>Generic contents</p>'
                route.fulfill(status=200, content_type='text/html', body=body)
            page.page.route('https://verify.test/**', serve)
            original = page.click_candidate
            def click(c):
                clicks.append(c.label)
                return original(c)
            page.click_candidate = click
        try:
            await lane._run(prepare)
            await lane.open_url('https://verify.test/home')
            result = await lane.run_plan(plan, 'open Destination')
            url = await lane._run(lambda: lane._ensure().current_url())
            return result, url, clicks
        finally:
            await lane.close()
    result, url, clicks = asyncio.run(go())
    assert clicks == ['Destination'], str(result)
    if destination == 'destination':
        assert result['status'] == 'complete' and result['ledger'][0]['effect'] == 'confirmed'
        assert url == 'https://verify.test/destination'
    else:
        assert result['status'] == 'partial' and 'did not see it' in result['reason']
        assert url == 'https://verify.test/' + ('home' if destination == 'noop' else 'wrong')
