"""Transactional lesson storage, including every saved revision of the whiteboard."""
from __future__ import annotations

import hashlib
import json
import os
import sqlite3
import time
import uuid
from contextlib import contextmanager
from pathlib import Path


# Lesson fields that can be megabytes (screenshot, flattened board, tldraw document, strokes). A revision
# stores each one by its hash in `blobs`, so an autosave that changed only the typed ideas does not copy a
# 6 MB screenshot again: an unchanged value is the same hash, stored once per lesson.
BLOB_FIELDS = ("source_image", "board_image", "board", "strokes")
BLOB_MIN = 4096          # smaller values stay inline: a row in `blobs` would cost more than it saves
SUMMARY_FIELDS = ("id", "topic", "level", "mode", "problem", "stage", "updated", "demo", "revision")


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
            db.execute("CREATE TABLE IF NOT EXISTS blobs (lesson_id TEXT, hash TEXT, data TEXT, PRIMARY KEY(lesson_id,hash))")

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
        # SQLite picks the summary fields out of each lesson: parsing every whole lesson (images and
        # boards included) in Python on every save made the dashboard widget the slow part of a save.
        columns = ", ".join(f"json_extract(data, '$.{k}')" for k in SUMMARY_FIELDS)
        with self.connect() as db:
            rows = db.execute(f"SELECT {columns} FROM lessons ORDER BY updated DESC").fetchall()
        summaries = [dict(zip(SUMMARY_FIELDS, r)) for r in rows]
        for summary in summaries:
            summary["demo"] = bool(summary["demo"])   # JSON true comes back from SQLite as 1
        return summaries

    @staticmethod
    def summary(s):
        return {k: s[k] for k in ("id", "topic", "level", "mode", "problem", "stage", "updated", "demo", "revision")}

    def save(self, lesson, expected=None):
        lesson = dict(lesson)
        with self.connect() as db:
            db.execute("BEGIN IMMEDIATE")
            row = db.execute("SELECT json_extract(data, '$.revision') FROM lessons WHERE id=?",
                             (lesson["id"],)).fetchone()
            revision = row[0] if row else 0
            if expected is not None and revision != expected:
                raise ValueError("This lesson changed in another window. Reload it before continuing.")
            lesson.update(revision=revision + 1, updated=time.time())
            encoded = json.dumps(lesson, ensure_ascii=False)
            db.execute("INSERT OR REPLACE INTO lessons VALUES (?,?,?)", (lesson["id"], lesson["updated"], encoded))
            db.execute("INSERT INTO revisions VALUES (?,?,?)",
                       (lesson["id"], lesson["revision"], self._revision_data(db, lesson)))
        self.write_widget()
        return lesson

    def create(self, mode, topic, level, demo):
        if mode not in ("learn", "help"):
            raise ValueError("Choose learn or help.")
        return self.save({"id": uuid.uuid4().hex, "mode": mode, "topic": topic or "My problem",
                          "level": level, "demo": demo, "problem": "", "ideas": "", "stuck": False,
                          "strokes": [], "board": None, "source_image": "", "board_image": "", "events": [],
                          "stage": "setup" if mode == "learn" else "input", "revision": 0})

    @staticmethod
    def _revision_data(db, lesson):
        """The revision row: the lesson with each large field replaced by {"$blob": hash} into `blobs`."""
        revision = dict(lesson)
        for field in BLOB_FIELDS:
            value = json.dumps(lesson.get(field), ensure_ascii=False)
            if len(value) < BLOB_MIN:
                continue
            digest = hashlib.sha256(value.encode()).hexdigest()
            db.execute("INSERT OR IGNORE INTO blobs VALUES (?,?,?)", (lesson["id"], digest, value))
            revision[field] = {"$blob": digest}
        return json.dumps(revision, ensure_ascii=False)

    def history(self, key):
        self.get(key)
        with self.connect() as db:
            rows = db.execute("SELECT data FROM revisions WHERE lesson_id=? ORDER BY revision", (key,)).fetchall()
            blobs = dict(db.execute("SELECT hash, data FROM blobs WHERE lesson_id=?", (key,)).fetchall())
        history = [json.loads(r[0]) for r in rows]
        decoded = {}
        for revision in history:
            for field in BLOB_FIELDS:
                ref = revision.get(field)
                # Revisions saved before `blobs` existed hold the value inline and are left as they are.
                if isinstance(ref, dict) and set(ref) == {"$blob"} and ref["$blob"] in blobs:
                    digest = ref["$blob"]
                    if digest not in decoded:
                        decoded[digest] = json.loads(blobs[digest])
                    revision[field] = decoded[digest]
        return history

    def delete(self, key):
        with self.connect() as db:
            db.execute("DELETE FROM revisions WHERE lesson_id=?", (key,))
            db.execute("DELETE FROM blobs WHERE lesson_id=?", (key,))
            db.execute("DELETE FROM lessons WHERE id=?", (key,))
        self.write_widget()

    def write_widget(self):
        rows = self.list()
        snapshot = {"total": len(rows), "completed": sum(s["stage"] == "complete" for s in rows),
                    "lessons": rows[:5], "updated": time.time()}
        temporary = self.directory / "dashboard.tmp"
        temporary.write_text(json.dumps(snapshot), encoding="utf-8")
        temporary.replace(self.directory / "dashboard.json")
