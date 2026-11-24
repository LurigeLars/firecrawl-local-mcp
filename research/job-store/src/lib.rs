//! Embedded durable batch-store experiment. No HTTP routes or network calls.
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use std::{path::Path, time::Duration};

const MAX_JOBS: i64 = 16;
const MAX_ITEMS: usize = 100;
const MAX_DOC: usize = 64 * 1024;
const MAX_PAGE: usize = 128 * 1024;

#[derive(Debug)]
pub enum Error { Db(rusqlite::Error), Invalid(&'static str), Json(serde_json::Error) }
impl From<rusqlite::Error> for Error { fn from(e: rusqlite::Error) -> Self { Self::Db(e) } }
impl From<serde_json::Error> for Error { fn from(e: serde_json::Error) -> Self { Self::Json(e) } }
type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone)]
pub struct Claim { job: String, item: String, generation: i64 }
impl Claim { pub fn item(&self) -> &str { &self.item } }

pub struct Store { db: Connection }
impl Store {
    /// The path is operator-owned, never supplied by an API caller.
    pub fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        db.busy_timeout(Duration::from_secs(2))?;
        db.execute_batch("
            PRAGMA foreign_keys=ON;
            PRAGMA journal_mode=WAL;
            PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS jobs (
                id TEXT PRIMARY KEY, status TEXT NOT NULL
                CHECK(status IN ('scraping','completed','failed','cancelled'))
            );
            CREATE TABLE IF NOT EXISTS items (
                job TEXT NOT NULL REFERENCES jobs(id), key TEXT NOT NULL,
                ordinal INTEGER NOT NULL,
                state TEXT NOT NULL CHECK(state IN ('queued','running','done','failed','cancelled')),
                generation INTEGER NOT NULL DEFAULT 0,
                result_seq INTEGER, payload TEXT,
                PRIMARY KEY(job,key), UNIQUE(job,ordinal), UNIQUE(job,result_seq)
            );")?;
        Ok(Self { db })
    }

    /// Fixed batch; duplicate keys reject the whole creation transaction.
    pub fn create(&mut self, id: &str, keys: &[String]) -> Result<()> {
        if id.is_empty() || id.len() > 128 || keys.is_empty() || keys.len() > MAX_ITEMS ||
            keys.iter().any(|k| k.is_empty() || k.len() > 8192) {
            return Err(Error::Invalid("invalid batch"));
        }
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let count: i64 = tx.query_row("SELECT COUNT(*) FROM jobs", [], |r| r.get(0))?;
        if count >= MAX_JOBS { return Err(Error::Invalid("job store full")); }
        tx.execute("INSERT INTO jobs VALUES(?1,'scraping')", [id])?;
        for (i, key) in keys.iter().enumerate() {
            tx.execute("INSERT INTO items(job,key,ordinal,state) VALUES(?1,?2,?3,'queued')",
                params![id, key, i as i64])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn claim(&mut self, id: &str) -> Result<Option<Claim>> {
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let next: Option<(String,i64)> = tx.query_row(
            "SELECT items.key,generation FROM items JOIN jobs ON jobs.id=items.job
             WHERE job=?1 AND jobs.status='scraping' AND items.state='queued'
             ORDER BY ordinal LIMIT 1", [id], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
        let Some((item, old)) = next else { return Ok(None); };
        let generation = old.checked_add(1).ok_or(Error::Invalid("claim generation exhausted"))?;
        tx.execute("UPDATE items SET state='running',generation=?3 WHERE job=?1 AND key=?2",
            params![id,item,generation])?;
        tx.commit()?;
        Ok(Some(Claim { job:id.into(), item, generation }))
    }

    /// Atomic result and completion commit; old claims cannot overwrite results.
    pub fn settle(&mut self, claim: &Claim, ok: bool, payload: &Value) -> Result<()> {
        let payload = serde_json::to_string(payload)?;
        if payload.len() > MAX_DOC { return Err(Error::Invalid("result exceeds research bound")); }
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let status: String = tx.query_row("SELECT status FROM jobs WHERE id=?1",
            [&claim.job], |r| r.get(0))?;
        if status != "scraping" { return Err(Error::Invalid("terminal job")); }
        let seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(result_seq),0)+1 FROM items WHERE job=?1",
            [&claim.job], |r| r.get(0))?;
        let changed = tx.execute(
            "UPDATE items SET state=?4,result_seq=?5,payload=?6
             WHERE job=?1 AND key=?2 AND generation=?3 AND state='running'",
            params![claim.job,claim.item,claim.generation,if ok {"done"} else {"failed"},seq,payload])?;
        if changed != 1 { return Err(Error::Invalid("stale or already settled claim")); }
        let pending: i64 = tx.query_row(
            "SELECT COUNT(*) FROM items WHERE job=?1 AND state IN ('queued','running')",
            [&claim.job], |r| r.get(0))?;
        if pending == 0 {
            tx.execute("UPDATE jobs SET status='completed' WHERE id=?1", [&claim.job])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Only call after exclusive ownership and old worker shutdown are proven.
    /// A requeued read may be repeated, but only its new generation can settle.
    pub fn recover_after_worker_exit(&mut self) -> Result<usize> {
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let n = tx.execute("UPDATE items SET state='queued' WHERE state='running'
            AND job IN (SELECT id FROM jobs WHERE status='scraping')", [])?;
        tx.commit()?;
        Ok(n)
    }

    /// Cancels pending claims transactionally. External in-flight requests
    /// still need cooperative cancellation by the eventual worker supervisor.
    pub fn cancel(&mut self, id: &str) -> Result<()> {
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if tx.execute("UPDATE jobs SET status='cancelled' WHERE id=?1 AND status='scraping'", [id])? != 1 {
            return Err(Error::Invalid("job missing or terminal"));
        }
        tx.execute("UPDATE items SET state='cancelled' WHERE job=?1 AND state IN ('queued','running')", [id])?;
        tx.commit()?;
        Ok(())
    }

    /// Stable completion-order cursor, including per-item failures. No result
    /// is silently dropped when the response byte budget is reached.
    pub fn page(&mut self, id: &str, after: i64, limit: usize) -> Result<Value> {
        if !(0..=MAX_ITEMS as i64).contains(&after) || !(1..=MAX_ITEMS).contains(&limit) {
            return Err(Error::Invalid("invalid cursor"));
        }
        let tx = self.db.transaction()?;
        let status: String = tx.query_row("SELECT status FROM jobs WHERE id=?1", [id], |r| r.get(0))?;
        let (total, succeeded, failed, cancelled, last): (i64,i64,i64,i64,i64) = tx.query_row(
            "SELECT COUNT(*),SUM(state='done'),SUM(state='failed'),SUM(state='cancelled'),
             COALESCE(MAX(result_seq),0) FROM items WHERE job=?1", [id],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
        if after > last { return Err(Error::Invalid("cursor exceeds committed results")); }
        let mut rows = Vec::new();
        let mut cursor = after;
        let mut bytes = 2;
        {
            let mut stmt = tx.prepare("SELECT result_seq,key,state,payload FROM items
                WHERE job=?1 AND result_seq>?2 ORDER BY result_seq LIMIT ?3")?;
            let mut iter = stmt.query(params![id,after,limit as i64])?;
            while let Some(r) = iter.next()? {
                let seq: i64 = r.get(0)?;
                let key: String = r.get(1)?;
                let state: String = r.get(2)?;
                let raw: String = r.get(3)?;
                let row = json!({"key":key,"state":state,"data":serde_json::from_str::<Value>(&raw)?});
                let size = serde_json::to_string(&row)?.len() + usize::from(!rows.is_empty());
                if bytes + size > MAX_PAGE {
                    if rows.is_empty() { return Err(Error::Invalid("single result exceeds page bound")); }
                    break;
                }
                bytes += size;
                rows.push(row);
                cursor = seq;
            }
        }
        let next = if cursor < last || status == "scraping" { Some(cursor) } else { None };
        tx.commit()?;
        Ok(json!({"status":status,"total":total,"completed":succeeded,"failed":failed,
            "cancelled":cancelled,"results":rows,"next_after":next}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    struct Temp(std::path::PathBuf);
    impl Temp {
        fn new() -> Self {
            let dir=std::env::temp_dir().join(format!("fc-store-{}-{}",std::process::id(),SERIAL.fetch_add(1,Ordering::Relaxed)));
            std::fs::create_dir(&dir).unwrap(); Self(dir)
        }
        fn db(&self) -> std::path::PathBuf { self.0.join("jobs.sqlite") }
    }
    impl Drop for Temp { fn drop(&mut self) { let _=std::fs::remove_dir_all(&self.0); } }
    fn keys() -> Vec<String> { vec!["a".into(),"b".into(),"c".into()] }

    #[test]
    fn restart_preserves_results_and_requeues_only_unfinished_claims() {
        let tmp=Temp::new();
        let mut db=Store::open(&tmp.db()).unwrap();
        db.create("job",&keys()).unwrap();
        let a=db.claim("job").unwrap().unwrap();
        let stale=db.claim("job").unwrap().unwrap();
        db.settle(&a,true,&json!({"value":421.5})).unwrap();
        drop(db);
        let mut db=Store::open(&tmp.db()).unwrap();
        assert_eq!(db.recover_after_worker_exit().unwrap(),1);
        let b=db.claim("job").unwrap().unwrap();
        assert_eq!(b.item(),"b");
        assert!(db.settle(&stale,true,&json!({"wrong":1})).is_err());
        db.settle(&b,false,&json!({"error":"synthetic failure"})).unwrap();
        let c=db.claim("job").unwrap().unwrap();
        db.settle(&c,true,&json!({"value":-1.2})).unwrap();
        let page=db.page("job",0,1).unwrap();
        assert_eq!(page["status"],"completed");
        assert_eq!(page["completed"],2);
        assert_eq!(page["failed"],1);
        assert_eq!(page["next_after"],1);
        let rest=db.page("job",1,100).unwrap();
        assert_eq!(rest["results"].as_array().unwrap().len(),2);
        assert!(rest["next_after"].is_null());
    }

    #[test]
    fn multiple_connections_claim_distinct_items_and_cancel_rejects_late_write() {
        let tmp=Temp::new();
        let mut db=Store::open(&tmp.db()).unwrap();
        let mut other=Store::open(&tmp.db()).unwrap();
        db.create("job",&keys()).unwrap();
        let a=db.claim("job").unwrap().unwrap();
        let b=other.claim("job").unwrap().unwrap();
        assert_ne!(a.item(),b.item());
        other.settle(&b,true,&json!({"value":2})).unwrap();
        db.cancel("job").unwrap();
        assert!(other.settle(&a,true,&json!({"value":1})).is_err());
        assert!(db.claim("job").unwrap().is_none());
        let result=db.page("job",0,100).unwrap();
        assert_eq!(result["completed"],1);
        assert_eq!(result["cancelled"],2);
        assert_eq!(result["status"],"cancelled");
    }

    #[test]
    fn creation_is_atomic_and_paging_respects_utf8_byte_budget() {
        let tmp=Temp::new();
        let mut db=Store::open(&tmp.db()).unwrap();
        assert!(db.create("job",&["a".into(),"a".into()]).is_err());
        db.create("job",&keys()).unwrap();
        for _ in 0..3 {
            let claim=db.claim("job").unwrap().unwrap();
            assert!(db.settle(&claim,true,&json!("x".repeat(MAX_DOC))).is_err());
            db.settle(&claim,true,&json!("å".repeat(30_000))).unwrap();
        }
        let first=db.page("job",0,100).unwrap();
        assert_eq!(first["results"].as_array().unwrap().len(),2);
        assert_eq!(first["next_after"],2);
        let last=db.page("job",2,100).unwrap();
        assert_eq!(last["results"].as_array().unwrap().len(),1);
        assert!(last["next_after"].is_null());
        assert!(db.cancel("job").is_err());
    }
}
