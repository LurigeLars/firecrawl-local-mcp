"""SQLite durability contract experiment; not a production queue or API.

Only synthetic data in a disposable directory. No production DB, network,
credentials or extra dependencies. A future Rust implementation must pass
the same contract against its actual storage and worker lifecycle.
"""
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import unittest

SCHEMA = """
CREATE TABLE job (
 id TEXT PRIMARY KEY, status TEXT NOT NULL
 CHECK(status IN ('scraping','completed','failed','cancelled')),
 expected INTEGER NOT NULL CHECK(expected BETWEEN 1 AND 100)
);
CREATE TABLE result (
 job_id TEXT NOT NULL REFERENCES job(id),
 seq INTEGER NOT NULL, item_key TEXT NOT NULL, document TEXT NOT NULL,
 PRIMARY KEY(job_id,seq), UNIQUE(job_id,item_key)
);
"""
MAX_DOCUMENT_BYTES = 32 * 1024
PAGE_BYTES = 64 * 1024

def connect(path):
    db = sqlite3.connect(path, timeout=2)
    db.execute("PRAGMA foreign_keys=ON")
    db.execute("PRAGMA synchronous=FULL")
    return db

def append(db, job, key, document):
    payload = json.dumps(document, ensure_ascii=False, separators=(",", ":"))
    if len(payload.encode("utf-8")) > MAX_DOCUMENT_BYTES:
        raise ValueError("result exceeds experiment budget")
    with db:
        db.execute("BEGIN IMMEDIATE")
        status, expected = db.execute("SELECT status,expected FROM job WHERE id=?", (job,)).fetchone()
        if status != "scraping":
            raise ValueError("terminal job")
        old = db.execute("SELECT document FROM result WHERE job_id=? AND item_key=?", (job,key)).fetchone()
        if old:
            if old[0] != payload:
                raise ValueError("conflicting duplicate")
            return
        count, = db.execute("SELECT COUNT(*) FROM result WHERE job_id=?", (job,)).fetchone()
        if count >= expected:
            raise ValueError("job limit reached")
        db.execute("INSERT INTO result VALUES(?,?,?,?)", (job,count+1,key,payload))

def terminal(db, job, status):
    if status not in ("completed", "failed", "cancelled"):
        raise ValueError("invalid terminal state")
    with db:
        db.execute("BEGIN IMMEDIATE")
        active = db.execute("SELECT status FROM job WHERE id=?", (job,)).fetchone()
        if active != ("scraping",):
            raise ValueError("terminal job")
        if status == "completed":
            # Synthetic batch has a known expected count. Crawl discovery and
            # error accounting require a separate real frontier contract.
            actual, expected = db.execute(
                "SELECT COUNT(result.seq), job.expected FROM job LEFT JOIN result "
                "ON result.job_id=job.id WHERE job.id=? GROUP BY job.id", (job,)).fetchone()
            if actual != expected:
                raise ValueError("incomplete batch")
        db.execute("UPDATE job SET status=? WHERE id=?", (status,job))

def page(db, job, skip=0, limit=10):
    if not 0 <= skip <= 100 or not 1 <= limit <= 100:
        raise ValueError("invalid cursor or limit")
    # Read counts and page from one consistent snapshot.
    with db:
        db.execute("BEGIN")
        status, expected = db.execute("SELECT status,expected FROM job WHERE id=?", (job,)).fetchone()
        completed, = db.execute("SELECT COUNT(*) FROM result WHERE job_id=?", (job,)).fetchone()
        rows = db.execute("SELECT document FROM result WHERE job_id=? AND seq>? ORDER BY seq LIMIT ?",
                          (job,skip,limit))
        data, used = [], 2
        for (payload,) in rows:
            size = len(payload.encode("utf-8")) + (1 if data else 0)
            if used + size > PAGE_BYTES:
                break
            data.append(json.loads(payload))
            used += size
        offset = skip + len(data)
        return dict(status=status, total=expected, completed=completed, data=data,
                    next_skip=offset if offset < completed or status == "scraping" else None)

def recover_interrupted(db):
    # Single exclusive worker process only: never run recovery while the old
    # worker is still alive. This proves explicit failure, not crawl resumption.
    with db:
        db.execute("UPDATE job SET status='failed' WHERE status='scraping'")

class Contract(unittest.TestCase):
    def setUp(self):
        self.root = tempfile.TemporaryDirectory(prefix="firecrawl-job-contract-")
        self.addCleanup(self.root.cleanup)
        self.path = Path(self.root.name) / "jobs.sqlite"
        self.db = connect(self.path)
        self.addCleanup(lambda: self.db.close())
        self.db.execute("PRAGMA journal_mode=WAL")
        self.db.executescript(SCHEMA)
        self.db.execute("INSERT INTO job VALUES('batch','scraping',3)")
        self.db.commit()

    def test_restart_and_atomic_rollback(self):
        append(self.db, "batch", "a", {"value":421.5})
        self.db.close()
        # Simulate abrupt process termination with an uncommitted write.
        child = subprocess.run([sys.executable, "-c",
            "import sqlite3,os,sys; d=sqlite3.connect(sys.argv[1]); "
            "d.execute('BEGIN IMMEDIATE'); "
            "d.execute('INSERT INTO result VALUES(?,?,?,?)',('batch',2,'b','{}')); os._exit(7)",
            str(self.path)], timeout=5, check=False)
        self.assertEqual(child.returncode, 7)
        self.db = connect(self.path)
        recover_interrupted(self.db)
        result = page(self.db, "batch")
        self.assertEqual(result["status"], "failed")
        self.assertEqual(result["data"], [{"value":421.5}])
        self.assertEqual(result["completed"], 1)
        self.assertIsNone(result["next_skip"])

    def test_paging_and_completion(self):
        for key in ("a","b","c"):
            append(self.db, "batch", key, {"key":key, "text":"å"*14000})
        terminal(self.db, "batch", "completed")
        first = page(self.db, "batch", limit=3)
        self.assertEqual(first["completed"], 3)
        self.assertEqual(first["next_skip"], 2)
        self.assertEqual([x["key"] for x in first["data"]], ["a","b"])
        last = page(self.db, "batch", skip=first["next_skip"])
        self.assertEqual([x["key"] for x in last["data"]], ["c"])
        self.assertIsNone(last["next_skip"])

    def test_duplicates_cancel_and_limits(self):
        append(self.db, "batch", "a", {"value":-1})
        append(self.db, "batch", "a", {"value":-1})
        with self.assertRaises(ValueError):
            append(self.db, "batch", "a", {"value":1})
        with self.assertRaises(ValueError):
            append(self.db, "batch", "b", {"x":"x"*MAX_DOCUMENT_BYTES})
        with self.assertRaises(ValueError):
            terminal(self.db, "batch", "completed")
        terminal(self.db, "batch", "cancelled")
        with self.assertRaises(ValueError):
            append(self.db, "batch", "b", {})
        self.assertEqual(page(self.db, "batch")["completed"], 1)
        self.assertEqual(page(self.db, "batch")["status"], "cancelled")

if __name__ == "__main__":
    unittest.main(verbosity=2)
