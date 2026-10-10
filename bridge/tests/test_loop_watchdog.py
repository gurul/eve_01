"""The loop watchdog: a stalled event loop gets its stack logged; a healthy one stays quiet."""
import asyncio
import logging
import signal
import time

from eve_bridge.loop_watchdog import PET_SECS, LoopWatchdog, register_dump_signal

LOGGER = "eve_bridge.loop_watchdog"


def _hold_the_loop(secs: float) -> None:
    """The kind of call the watchdog exists to name: synchronous, on the loop thread."""
    time.sleep(secs)


def _run(body, threshold=0.45, poll=0.05) -> LoopWatchdog:
    # Well above PET_SECS (0.25 s), so an ordinary gap between pets under load is never a stall.
    loop = asyncio.new_event_loop()
    wd = LoopWatchdog(loop, threshold=threshold, poll=poll)

    async def main():
        wd.start()
        await asyncio.sleep(0.3)          # a few pets land first
        await body()
        await asyncio.sleep(0.3)          # and the loop is seen ticking again

    try:
        loop.run_until_complete(main())
    finally:
        wd.stop()
        loop.close()
    return wd


def test_a_stall_logs_the_loop_threads_stack(caplog):
    caplog.set_level(logging.WARNING, logger=LOGGER)

    async def body():
        _hold_the_loop(0.8)

    wd = _run(body)
    msgs = [r.getMessage() for r in caplog.records if r.name == LOGGER]
    stalled = [m for m in msgs if "event loop stalled" in m]
    assert stalled, msgs
    # The stack names the call that held the loop, not just the fact of a stall.
    assert "_hold_the_loop" in stalled[0]
    assert "time.sleep" in stalled[0] or "sleep(secs)" in stalled[0]
    assert any("resumed after" in m for m in msgs), msgs
    assert wd.stalls == 1
    assert 0.7 <= wd.longest <= 1.5


def test_a_healthy_loop_logs_nothing(caplog):
    caplog.set_level(logging.WARNING, logger=LOGGER)

    async def body():
        for _ in range(10):
            await asyncio.sleep(0.05)    # busy, but never holding the loop

    wd = _run(body)
    assert wd.stalls == 0
    assert not [r for r in caplog.records if r.name == LOGGER]


def test_a_long_stall_is_reported_again_while_it_lasts(caplog):
    caplog.set_level(logging.WARNING, logger=LOGGER)
    loop = asyncio.new_event_loop()
    # The threshold must sit clearly above PET_SECS (0.25 s): at 0.2 an ordinary gap between two pets already
    # read as a stall, so a little scheduling jitter around the real one counted a second stall (1 run in 7,
    # 2026-09-23). A 1.0 s hold still crosses 0.4 and repeats every 0.3 s, so it is reported at least twice.
    assert PET_SECS < 0.4
    wd = LoopWatchdog(loop, threshold=0.4, repeat=0.3, poll=0.05)

    async def main():
        wd.start()
        await asyncio.sleep(0.2)
        _hold_the_loop(1.0)
        await asyncio.sleep(0.2)

    try:
        loop.run_until_complete(main())
    finally:
        wd.stop()
        loop.close()
    stalled = [r for r in caplog.records if r.name == LOGGER and "event loop stalled" in r.getMessage()]
    assert len(stalled) >= 2, [r.getMessage()[:60] for r in stalled]
    assert wd.stalls == 1


def test_dump_signal_registers_on_this_platform():
    assert register_dump_signal(signal.SIGUSR1) is True
