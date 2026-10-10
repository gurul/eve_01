from types import SimpleNamespace

from eve_bridge import computer_agent as ca
from eve_bridge.daemon import Daemon


def test_browser_default_keeps_desktop_and_other_budgets():
    cfg = ca.AgentConfig(max_turns=9, max_secs=83, verify=False)
    browser = ca.browser_config(cfg, {})
    assert browser.model == 'gpt-6-luna'
    assert cfg.model == 'gpt-6-astra'
    assert browser is not cfg and browser.max_turns == 9 and browser.max_secs == 83 and not browser.verify


def test_explicit_models_take_precedence():
    cfg = ca.AgentConfig(model='custom-model')
    assert ca.browser_config(cfg, {}).model == 'custom-model'
    assert ca.browser_config(cfg, {'EVE_AGENT_MODEL': 'gpt-6-astra'}).model == 'gpt-6-astra'
    assert ca.browser_config(cfg, {'EVE_AGENT_MODEL': 'gpt-6-astra',
                                  'EVE_BROWSER_MODEL': 'gpt-6-sol'}).model == 'gpt-6-sol'
    assert ca.browser_config(ca.AgentConfig(), {'EVE_BROWSER_MODEL': '  '}).model == 'gpt-6-luna'


def test_actual_daemon_browser_factory_uses_browser_config(monkeypatch):
    monkeypatch.delenv('EVE_AGENT_MODEL', raising=False)
    monkeypatch.delenv('EVE_BROWSER_MODEL', raising=False)
    lane = SimpleNamespace(screenshot=lambda: None)
    cfg = ca.AgentConfig()
    host = SimpleNamespace(_chrome_lane=lane, _telegram=None, _planner_create=lambda req: None, _agent_cfg=cfg)
    body = Daemon._chrome_body(host, lambda: None, lambda ev: None, None)['make_auto']()
    planner = body._make_planner(lambda ev: None, None)
    assert planner.config.model == 'gpt-6-luna'
    assert planner.browser is lane
    assert cfg.model == 'gpt-6-astra'
