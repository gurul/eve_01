"""Putting a vault value into a page's field through the Accessibility API, for Codex tasks (vault_mcp.py).

Codex drives the browser through its own cua_repl plugin and the ChatGPT extension, so eve cannot reach its tab
over CDP the way the browser lane does. It can reach the browser's Accessibility tree. Measured 2026-10-12:

* Codex's clicks (CDP events inside the tab) never give the browser the Mac's keyboard focus, and the browser
  need not be in front at all; key events would land in the address bar or another app. The first version
  typed at the Mac's focus and failed the real Codex run.
* The second looked for the field the PAGE had focused (AXFocused). Playwright's Chromium reports it in a
  background window because Playwright emulates page focus; the owner's Chrome, in the background during a
  real Codex task, reported no focused field at all. It failed the real run too.
* What holds: the tree has every window's active tab as an AXWebArea with its URL, and in it each text field
  with its kind (AXSecureTextField for a password) and what it is called (title, placeholder, DOM id, label).
  Setting AXValue on a field sets the page's value and fires its input and change events, the way autofill
  does; nothing comes to the front and no key is pressed.

So a fill never types and never needs focus: ``page_fields`` lists the text fields of the pages a caller wants
(``want(url)``: pages on other sites are never read), and vault_mcp.Server picks the field by what it is. Focus,
when a browser does report it, only breaks a tie. Nothing here reads a field's value: the check afterwards is
AXNumberOfCharacters. Only each window's ACTIVE tab is in the tree; a page in a background tab is not seen.

A browser builds its web Accessibility tree only while an assistive client is using it. The owner's Chrome had it
on (2026-10-12); a Codex computer task reads the same tree, which keeps it on. Chrome for Testing keeps it off
unless started with --force-renderer-accessibility (the live test does). Each browser is also asked for
AXManualAccessibility, the attribute Electron and some Chromium builds turn the tree on for; Chrome for Testing
and Chrome 2026-10 answer that it is unsupported (-25205), which costs nothing.
"""

from __future__ import annotations

import sys
from dataclasses import dataclass
from typing import Any, Callable, Optional

TEXT_ROLES = frozenset({"AXTextField", "AXTextArea", "AXComboBox"})
SECURE_SUBROLE = "AXSecureTextField"
MAX_NODES = 40000                     # per browser: a walk never runs away on a huge page
BROWSERS = frozenset({
    "com.google.Chrome", "com.google.Chrome.beta", "com.google.Chrome.dev", "com.google.Chrome.canary",
    "com.google.chrome.for.testing", "org.chromium.Chromium", "com.brave.Browser", "com.microsoft.edgemac",
    "company.thebrowser.Browser", "com.vivaldi.Vivaldi", "com.operasoftware.Opera", "com.apple.Safari",
})
NAME_ATTRS = ("AXTitle", "AXDescription", "AXPlaceholderValue", "AXDOMIdentifier", "AXHelp")


@dataclass(frozen=True)
class Field:
    pid: int
    app: str
    role: str
    subrole: str
    url: str                          # the URL of the page or frame the field is in
    page: str = ""                    # the URL of the top-level page it is in (= url outside a frame)
    words: str = ""                   # what the field is called (label, placeholder, id), never what is in it
    focused: bool = False             # the page has it focused (some browsers say so only in the front window)
    order: int = 0                    # its place in its page, top to bottom
    element: Any = None

    @property
    def secure(self) -> bool:
        return self.subrole == SECURE_SUBROLE


class NativeMac:
    """The real Mac. Tests use a fake with the same four methods."""

    def __init__(self) -> None:
        if sys.platform != "darwin":
            raise RuntimeError("the vault fills fields only on macOS")
        import ApplicationServices as AS  # noqa: N814 — pyobjc's module names
        self.AS = AS

    def _attr(self, element: Any, name: str) -> Any:
        err, value = self.AS.AXUIElementCopyAttributeValue(element, name, None)
        return value if err == 0 else None

    def trusted(self) -> bool:
        return bool(self.AS.AXIsProcessTrusted())

    def _url(self, web_area: Any) -> str:
        raw = self._attr(web_area, "AXURL")
        return str(raw.absoluteString()) if hasattr(raw, "absoluteString") else str(raw or "")

    def _words(self, element: Any) -> str:
        parts = [self._attr(element, a) for a in NAME_ATTRS]
        label = self._attr(element, "AXTitleUIElement")
        if label is not None:
            parts.append(self._attr(label, "AXValue") or self._attr(label, "AXTitle"))
        return " ".join(str(p) for p in parts if isinstance(p, str) and p.strip())[:200]

    def page_fields(self, want: Callable[[str], bool], frames: bool = False) -> list[Field]:
        """Every text field in the pages whose URL ``want`` accepts, in every running browser's windows (each
        window's active tab). A page ``want`` refuses is not walked into: its fields are never read. A frame
        inside an accepted page must be accepted by ``want`` too, unless ``frames`` (a card: a payment
        provider's frame inside the checkout page)."""
        from AppKit import NSRunningApplication
        out: list[Field] = []
        # Asked per bundle id each time: NSWorkspace.runningApplications() is refreshed by the main run loop,
        # which this server does not run, so it kept listing a browser that had quit and never the one that
        # started after (live, 2026-10-12).
        running_apps = [a for bid in sorted(BROWSERS)
                        for a in NSRunningApplication.runningApplicationsWithBundleIdentifier_(bid) or []
                        if not a.isTerminated()]
        for running in running_apps:
            pid = int(running.processIdentifier())
            app = self.AS.AXUIElementCreateApplication(pid)
            self.AS.AXUIElementSetAttributeValue(app, "AXManualAccessibility", True)
            name = str(running.localizedName() or "")
            seen = 0
            stack: list[tuple[Any, str, str]] = [(w, "", "") for w in (self._attr(app, "AXWindows") or [])]
            while stack and seen < MAX_NODES:
                node, url, page = stack.pop()
                seen += 1
                role = self._attr(node, "AXRole")
                if role == "AXWebArea":
                    url = self._url(node)
                    if not (want(url) or (frames and page)):
                        continue
                    page = page or url
                elif url and role in TEXT_ROLES:
                    out.append(Field(pid=pid, app=name, role=str(role), subrole=str(self._attr(node, "AXSubrole") or ""),
                                     url=url, page=page, words=self._words(node),
                                     focused=self._attr(node, "AXFocused") is True, order=seen, element=node))
                    continue
                stack.extend((child, url, page) for child in reversed(self._attr(node, "AXChildren") or []))
        return out

    def set_value(self, field: Field, text: str) -> bool:
        return self.AS.AXUIElementSetAttributeValue(field.element, "AXValue", text) == 0

    def char_count(self, field: Field) -> Optional[int]:
        n = self._attr(field.element, "AXNumberOfCharacters")
        return int(n) if isinstance(n, int) else None
