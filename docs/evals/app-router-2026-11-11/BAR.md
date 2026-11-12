# App router: the bar

Written 2026-11-11 before any tuning or holdout result existed. Not changed since.

## Bar (written 2026-11-11 by the orchestrator, before any tuning or holdout result exists)

Router level, scored ONCE on the blind holdout (writer B, 120 cases, never seen by the builder), gates fitted on the tuning set only:
1. On holdout cases whose truth is a device app: the router names that app (right app, whether or not it can do it) on >= 0.90 of them.
2. On holdout cases whose truth is the Mac: the router claims a device app (which would refuse the Mac agent) on <= 0.03 of them.
3. On holdout cases whose truth is neither: the router claims a device app on <= 0.05 of them.
4. Jev latency p95 <= 800 ms over the holdout run; an error or timeout yields no claim.
SHIPPED is True only if all four hold. Reported, not gated: can/can't accuracy, method accuracy, per-app confusion.

End to end (intent_eval, blind set of 40 written by writer C, app door and call door, base arm = router off, new arm = router on, each arm scored once):
5. NEW: zero device-app cases end in the Mac agent (HARD_NEGATIVE_ACTED = 0 for those cases).
6. NEW: Mac-task cases still reach the Mac agent on >= 0.90.
7. NEW overall accuracy >= BASE overall accuracy - 3 points (the documented noise floor).
The router is switched on by default only if 1-7 all hold. The bar is never changed after results exist.

