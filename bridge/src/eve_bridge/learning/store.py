"""Transactional lesson storage, including every saved revision of the whiteboard."""
from __future__ import annotations

import json
import os
import sqlite3
import time
import uuid
from contextlib import contextmanager
from pathlib import Path


def default_dir():
    return Path(os.environ.get("EVE_LEARNING_DIR", "~/.config/eve-bridge/learning")).expanduser()


class Store:
    def __init__(self, directory):
        self.directory = Path(directory)
        self.directory.mkdir(parents=True, exist_ok=True)
        self.path = self.directory / "lessons.sqlite3"
        with self.connect() as db:
            db.execute("CREATE TABLE IF NOT EXISTS lessons (id TEXT PRIMARY KEY, updated REAL, data TEXT)")
            db.execute("CREATE TABLE IF NOT EXISTS revisions (lesson_id TEXT, revision INTEGER, data TEXT, PRIMARY KEY(lesson_id,revision))")

    @contextmanager
    def connect(self):
        db = sqlite3.connect(self.path, timeout=15)
        try:
            with db:
                yield db
        finally:
            db.close()

    def get(self, key):
        with self.connect() as db:
            row = db.execute("SELECT data FROM lessons WHERE id=?", (key,)).fetchone()
        if not row:
            raise KeyError("Lesson not found")
        return json.loads(row[0])

    def list(self):
        with self.connect() as db:
            rows = db.execute("SELECT data FROM lessons ORDER BY updated DESC").fetchall()
        return [self.summary(json.loads(r[0])) for r in rows]

    @staticmethod
    def summary(s):
        return {k: s[k] for k in ("id", "topic", "level", "mode", "problem", "stage", "updated", "demo", "revision")}

    def save(self, lesson, expected=None):
        lesson = dict(lesson)
        with self.connect() as db:
            db.execute("BEGIN IMMEDIATE")
            row = db.execute("SELECT data FROM lessons WHERE id=?", (lesson["id"],)).fetchone()
            revision = json.loads(row[0])["revision"] if row else 0
            if expected is not None and revision != expected:
                raise ValueError("This lesson changed in another window. Reload it before continuing.")
            lesson.update(revision=revision + 1, updated=time.time())
            encoded = json.dumps(lesson, ensure_ascii=False)
            db.execute("INSERT OR REPLACE INTO lessons VALUES (?,?,?)", (lesson["id"], lesson["updated"], encoded))
            db.execute("INSERT INTO revisions VALUES (?,?,?)", (lesson["id"], lesson["revision"], encoded))
        self.write_widget()
        return lesson

    def create(self, mode, topic, level, demo):
        if mode not in ("learn", "help"):
            raise ValueError("Choose learn or help.")
        return self.save({"id": uuid.uuid4().hex, "mode": mode, "topic": topic or "My problem",
                          "level": level, "demo": demo, "problem": "", "ideas": "", "stuck": False,
                          "strokes": [], "board": None, "source_image": "", "board_image": "", "events": [],
                          "stage": "setup" if mode == "learn" else "input", "revision": 0})

    def history(self, key):
        self.get(key)
        with self.connect() as db:
            rows = db.execute("SELECT data FROM revisions WHERE lesson_id=? ORDER BY revision", (key,)).fetchall()
        return [json.loads(r[0]) for r in rows]

    def delete(self, key):
        with self.connect() as db:
            db.execute("DELETE FROM revisions WHERE lesson_id=?", (key,))
            db.execute("DELETE FROM lessons WHERE id=?", (key,))
        self.write_widget()

    def write_widget(self):
        rows = self.list()
        snapshot = {"total": len(rows), "completed": sum(s["stage"] == "complete" for s in rows),
                    "lessons": rows[:5], "updated": time.time()}
        temporary = self.directory / "dashboard.tmp"
        temporary.write_text(json.dumps(snapshot), encoding="utf-8")
        temporary.replace(self.directory / "dashboard.json")
