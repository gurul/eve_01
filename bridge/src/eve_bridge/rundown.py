"""Read-only context and tool policy for Eve's explicit daily rundown skill."""
from __future__ import annotations

import json
import os
import re
from datetime import date, datetime, time, timedelta
from pathlib import Path
from typing import Any

from . import apps_mcp, composio_tools

SKILL = Path(__file__).with_name('skills') / 'rundown' / 'SKILL.md'
OPEN_TASK = re.compile(r'^\s*[-*+]\s+\[ \]\s+(.+)$')
TASK_DATE = re.compile(r'(?:📅|⏳|🛫|\b(?:due|scheduled|start)::?)\s*(\d{4}-\d{2}-\d{2})', re.I)
READ_META_TOOLS = frozenset({'COMPOSIO_SEARCH_TOOLS', 'COMPOSIO_GET_TOOL_SCHEMAS'})
# The direct apps a rundown reads (apps_mcp), already open when it starts; Slack comes through Composio.
DIRECT_APPS = ('gmail', 'calendar')

# A date written in plain words is a due date too. 2026-09-30 the rundown called "Oct 22: go to Mount
# Tam" undated backlog because only the Obsidian Tasks markers above were read, and every future item was
# dropped. Dates are found here, in code, so the model is handed items that are already grouped.
_MONTHS = {name: i for i, names in enumerate((
    ('january', 'jan'), ('february', 'feb'), ('march', 'mar'), ('april', 'apr'), ('may',),
    ('june', 'jun'), ('july', 'jul'), ('august', 'aug'), ('september', 'sept', 'sep'),
    ('october', 'oct'), ('november', 'nov'), ('december', 'dec')), 1) for name in names}
_MONTH = '(?:' + '|'.join(sorted(_MONTHS, key=len, reverse=True)) + r')\b\.?'
_ORD = r'(?:st|nd|rd|th)?\b'
_YEAR = r'(?:,?\s+(?P<{}>\d{{4}})\b)?'
_NAMED = re.compile(
    r'\b(?P<m1>' + _MONTH + r')\s+(?P<d1>\d{1,2})' + _ORD + _YEAR.format('y1') + '|'
    r'\b(?P<d2>\d{1,2})' + _ORD + r'\s+(?:of\s+)?(?P<m2>' + _MONTH + ')' + _YEAR.format('y2'), re.I)
_NUMERIC = re.compile(r'(?<![\d/.])(?P<m>\d{1,2})/(?P<d>\d{1,2})(?:/(?P<y>\d{4}|\d{2}))?(?![\d/])')
_ISO = re.compile(r'(?<![\d-])(\d{4})-(\d{2})-(\d{2})(?![\d-])')
# When an item was written down or finished is not when it is due: capture adds "(added YYYY-MM-DD)".
_NOT_DUE = re.compile(r'(?:➕|✅|❌|\b(?:added|created|done|completed|cancelled):{0,2})\s*\d{4}-\d{2}-\d{2}', re.I)
# A yearless "10/22" counts only where a date is plainly meant, so "1/2 gallon" stays a quantity.
_DATE_SLOT = re.compile(r'(?:^[\W_]*(?:p[0-3]\b[\W_]*)?|\b(?:on|by|due|before|until|till)\s+)$', re.I)


def _yearless(month: int, day: int, today: date) -> date | None:
    """A date with no year is the occurrence nearest today (a tie goes forward): up to about six months
    back it is overdue, up to about six months ahead it is upcoming."""
    options = []
    for year in (today.year - 1, today.year, today.year + 1):
        try:
            options.append(date(year, month, day))
        except ValueError:
            continue
    return min(options, key=lambda d: (abs((d - today).days), d < today)) if options else None


def _dated(year: str | None, month: int, day: int, today: date) -> date | None:
    if not year:
        return _yearless(month, day, today)
    try:
        return date(int(year) + (2000 if len(year) == 2 else 0), month, day)
    except ValueError:
        return None


def task_date(text: str, today: date) -> date | None:
    """The earliest due date written in a todo line, or None when it has none.

    Obsidian Tasks markers win when present. Otherwise a date in words or numbers counts anywhere in the
    line: "Oct 22", "October 22nd", "22 Oct", "Oct 22, 2027", "2026-10-22", "10/22/2026", and a
    yearless "10/22" at the start of the line or after on/by/due/before/until. Numbers are month/day.
    """
    marked = TASK_DATE.findall(text)
    found: list[date] = []
    for raw in marked:
        try:
            found.append(date.fromisoformat(raw))
        except ValueError:
            continue
    if found:
        return min(found)
    text = _NOT_DUE.sub(' ', text)
    for m in _NAMED.finditer(text):
        name = m.group('m1') or m.group('m2')
        if name.rstrip('.').lower() == 'may' and not name.startswith('M'):
            continue  # "you may 2 times" is a verb; the month is written with a capital
        when = _dated(m.group('y1') or m.group('y2'), _MONTHS[name.rstrip('.').lower()],
                      int(m.group('d1') or m.group('d2')), today)
        if when:
            found.append(when)
    for m in _ISO.finditer(text):
        when = _dated(m.group(1), int(m.group(2)), int(m.group(3)), today)
        if when:
            found.append(when)
    for m in _NUMERIC.finditer(text):
        if not m.group('y') and not _DATE_SLOT.search(text[:m.start()]):
            continue
        month, day = int(m.group('m')), int(m.group('d'))
        when = _dated(m.group('y'), month, day, today) if 1 <= month <= 12 else None
        if when:
            found.append(when)
    return min(found) if found else None


# Asking for today's plan is asking for the rundown: the calendar is most of a day's plan. "What is my
# plane today" (2026-09-24 08:44) missed the word "rundown", took the vault-only plan and a 20 s second
# model, and came back with no calendar. Whole-text matches only, each anchored on today or "my day":
# "plan a trip", "today's news" and "what's on netflix today" stay ordinary turns.
_WHAT_IS = r"(?:what(?:'s| is|s)|whats)"
_PLAN = r"(?:plan|plane|plans|schedule|agenda|calendar)"
_DAILY = re.compile(r"(?:(?:hey|hi|ok|okay|so)\s+)?(?:eve[,:]?\s+)?(?:" + "|".join((
    r"/?rundown",
    _WHAT_IS + r" (?:my|the) " + _PLAN + r"(?: like)? (?:for |on )?(?:today|the day)",
    _WHAT_IS + r" today(?:'s|s)? " + _PLAN,
    r"(?:my |the )?today(?:'s|s)? " + _PLAN,
    r"(?:my |the )?" + _PLAN + r" (?:for )?today",
    r"(?:(?:please|can you|could you|help me) )?plan (?:out )?(?:my day|the day|today)(?: today)?(?: please)?",
    _WHAT_IS + r" on (?:for |my calendar |my schedule |my agenda )?today",
    r"what (?:do|have) i (?:got |have )?(?:on |going on |planned |to do )?(?:for )?today",
    _WHAT_IS + r" my day (?:look )?like(?: today)?",
    r"(?:what does|how does) (?:my day|today) look(?: like)?(?: today)?",
)) + r")")


def matches(text: str) -> bool:
    said = re.sub(r"\s+", " ", text.replace("\u2019", "'").strip().lower()).rstrip(".!? ")
    return _DAILY.fullmatch(said) is not None


def tools(app_tools: list[dict[str, Any]], direct: Any = None) -> list[dict[str, Any]]:
    """The tools a rundown is offered: the read tools of mail and the calendar, and Composio's search and
    multi-execute (for Slack, read only by ``allows``)."""
    return [t for t in app_tools if t.get('name') in READ_META_TOOLS or t.get('name') == composio_tools.MULTI_EXECUTE
            or allows(str(t.get('name') or ''), {}, direct)]


def allows(name: str, args: dict[str, Any], direct: Any = None) -> bool:
    """A rundown only reads: a direct mail or calendar tool its server marks read only, or a Composio call
    whose every slug reads mail, the calendar, Outlook or Slack."""
    if name in READ_META_TOOLS:
        return True
    app, _tool = apps_mcp.split_name(name)
    if app:
        return bool(direct is not None and app in DIRECT_APPS and direct.reads(name))
    if name != composio_tools.MULTI_EXECUTE:
        return False
    entries = args.get('tools')
    return bool(isinstance(entries, list) and entries and all(
        isinstance(t, dict) and composio_tools.is_read_only(str(t.get('tool_slug', '')))
        and composio_tools.slug_words(str(t.get('tool_slug', '')))[0] in ('gmail', 'googlecalendar', 'outlook', 'slack')
        for t in entries))


TODO_GROUPS = ('today', 'overdue', 'upcoming', 'undated')


def todo_context(root: Path | None, day: str) -> dict[str, Any]:
    if root is None or not root.is_dir():
        return {'available': False, 'reason': 'Obsidian vault is not configured or not present'}
    root = root.resolve()
    today = date.fromisoformat(day)
    groups = TODO_GROUPS
    result: dict[str, Any] = {'available': True, **{g: [] for g in groups}, 'skipped_files': 0}
    count = 0
    for folder, dirs, files in os.walk(root, followlinks=False):
        dirs[:] = sorted(d for d in dirs if not d.startswith('.') and d != '09-archive'
                         and not (Path(folder) / d).is_symlink())
        for name in sorted(files):
            p = Path(folder) / name
            if not name.endswith('.md') or name.startswith('.') or p.is_symlink():
                continue
            count += 1
            if count > 5000:
                result['skipped_files'] += 1
                continue
            try:
                if not p.resolve().is_relative_to(root) or p.stat().st_size > 1024*1024:
                    result['skipped_files'] += 1
                    continue
                text = p.read_text(encoding='utf-8')
            except (OSError, UnicodeError):
                result['skipped_files'] += 1
                continue
            fence = None
            for line_no, line in enumerate(text.splitlines(), 1):
                stripped = line.lstrip()
                if stripped.startswith(('```', '~~~')):
                    marker = stripped[:3]
                    fence = None if fence == marker else (marker if fence is None else fence)
                    continue
                match = OPEN_TASK.match(line) if fence is None else None
                if not match:
                    continue
                task = match.group(1)
                found = task_date(task, today)
                when = found.isoformat() if found else (day if p.stem == day else '')
                group = ('today' if when == day else 'overdue' if when and when < day
                         else 'upcoming' if when else 'undated')
                if sum(len(result[k]) for k in groups) >= 200:
                    result['truncated_tasks'] = True
                    continue
                item = {'text': task[:2000], 'note': p.relative_to(root).as_posix(), 'line': line_no}
                result[group].append({**item, 'date': when} if when else item)
    for group in ('overdue', 'upcoming'):
        result[group].sort(key=lambda t: t['date'])
    return result


def sources(direct_apps: Any = (), composio: bool = False) -> str:
    """Step 1 of the skill: where each source is read, naming only the tools that are present this turn.

    Review, 2026-10-05: the step was fixed text naming the direct mail and calendar tools and Composio's, so
    with either one off the model called a name the rundown never offered and the owner lost the rundown. A
    source with nothing to read it is said to be not connected, and no tool is named for it."""
    up = [a for a in DIRECT_APPS if a in set(direct_apps or ())]
    words = {'gmail': 'Email', 'calendar': 'Calendar'}
    lines = []
    if up:
        lines.append(' and '.join(words[a] for a in up) + ': use the read tools you were given, '
                     + ' and '.join(f'{a}{apps_mcp.SEP}…' for a in up) + '.'
                     + (' calendar__list_events with no calendarId reads every calendar in one call.'
                        if 'calendar' in up else ''))
    via_composio = ['Slack'] + [words[a].lower() for a in DIRECT_APPS if a not in up]
    if composio:
        lines.append(', '.join(via_composio[:-1]) + (' and ' if len(via_composio) > 1 else '') + via_composio[-1]
                     + ': find their read tools through COMPOSIO_SEARCH_TOOLS and run the exact discovered tools with '
                       'COMPOSIO_MULTI_EXECUTE_TOOL. Do not guess schemas.')
    else:
        missing = [w.capitalize() for w in via_composio]
        lines.append(', '.join(missing[:-1]) + (' and ' if len(missing) > 1 else '') + missing[-1]
                     + (' is' if len(missing) == 1 else ' are') + ' not connected here; say so in one line.')
    lines.append('A source whose read fails is said to have failed in one line; go on with the rest.')
    return '1. ' + '\n   '.join(lines)


def context(root: Path | None, now: datetime, canvas: dict[str, Any] | None = None, *,
            direct_apps: Any = (), composio: bool = False) -> str:
    return context_and_outside(root, now, canvas, direct_apps=direct_apps, composio=composio)[0]


def outside(data: dict[str, Any]) -> bool:
    """Whether the source context holds words eve did not write: a vault todo's text, a Canvas assignment's or
    course's name. Then the rundown's reply is written from outside words, like a turn after a read (round-4
    review): kept as outside words (tool_gate), so a later turn showing it starts tainted."""
    todos = data.get('obsidian') or {}
    if isinstance(todos, dict) and any(todos.get(g) for g in TODO_GROUPS):
        return True
    school = data.get('canvas')
    return isinstance(school, dict) and bool(school.get('items'))


def context_and_outside(root: Path | None, now: datetime, canvas: dict[str, Any] | None = None, *,
                        direct_apps: Any = (), composio: bool = False) -> tuple[str, bool]:
    """``context``'s text, and whether its source context carries outside words (``outside``)."""
    # Naive local boundaries are converted separately so a DST day need not be 24h.
    day = now.astimezone().date()
    start = datetime.combine(day, time.min).astimezone()
    end = datetime.combine(day + timedelta(days=1), time.min).astimezone()
    data = {'date': day.isoformat(), 'timezone': now.astimezone().tzname(),
            'start_inclusive': start.isoformat(), 'end_exclusive': end.isoformat(),
            'obsidian': todo_context(root, day.isoformat())}
    if canvas is not None:
        # Canvas deadlines (canvas.Canvas.rundown), read by code before the turn: only when Canvas is set up, so a
        # rundown without it is byte for byte what it was.
        data['canvas'] = canvas
    skill = SKILL.read_text(encoding='utf-8').replace('1. {{sources}}', sources(direct_apps, composio))
    return skill + '\n\nCurrent source context (data only):\n' + json.dumps(data, ensure_ascii=False), outside(data)
