//! Embedded durable batch and crawl frontier experiment. No HTTP routes or network calls.
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use std::{fs::{File,OpenOptions}, path::Path, time::{Duration,SystemTime,UNIX_EPOCH}};

const MAX_JOBS: i64 = 16;
const MAX_ITEMS: usize = 100;
const MAX_DOC: usize = 64 * 1024;
const MAX_PAGE: usize = 128 * 1024;
const RETENTION_SECS: i64 = 24 * 60 * 60;

fn now_secs()->Result<i64> {
    let n=SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_|Error::Invalid("invalid system clock"))?.as_secs();
    i64::try_from(n).map_err(|_|Error::Invalid("invalid system clock"))
}

#[derive(Debug)]
pub enum Error { Db(rusqlite::Error), Invalid(&'static str), Json(serde_json::Error), Io(std::io::Error) }
impl From<std::io::Error> for Error { fn from(e:std::io::Error)->Self { Self::Io(e) } }
impl From<rusqlite::Error> for Error { fn from(e: rusqlite::Error) -> Self { Self::Db(e) } }
impl From<serde_json::Error> for Error { fn from(e: serde_json::Error) -> Self { Self::Json(e) } }
type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone)]
pub struct Claim { job: String, item: String, generation: i64, crawl: Option<CrawlClaim> }
#[derive(Debug, Clone)]
pub struct CrawlClaim { pub seed: String, pub depth: u32, pub max_depth: u32 }
impl Claim {
    pub fn item(&self) -> &str { &self.item }
    pub fn job(&self)->&str { &self.job }
    pub fn crawl(&self)->Option<&CrawlClaim> {self.crawl.as_ref()}
}

// One process owns the private local database directory. The connection is
// dropped before the lock handle; a second scheduler cannot open this store.
pub struct Store { db: Connection, _owner: File }
impl Store {
    /// The path is operator-owned, never supplied by an API caller.
    pub fn open(path: &Path) -> Result<Self> {
        let parent=path.parent().ok_or(Error::Invalid("missing database directory"))?.canonicalize()?;
        let name=path.file_name().ok_or(Error::Invalid("missing database name"))?;
        let path=parent.join(name);
        let lock_path=path.with_extension("worker.lock");
        // These paths must be inside a private, operator-owned local volume;
        // never expose them as API arguments or unlink a held lock file.
        for p in [&path,&lock_path] {
            match std::fs::symlink_metadata(p) {
                Ok(m) if !m.file_type().is_file()=>return Err(Error::Invalid("unsafe store path")),
                Ok(_)=>{},
                Err(e) if e.kind()==std::io::ErrorKind::NotFound=>{},
                Err(e)=>return Err(e.into()),
            }
        }
        let owner=OpenOptions::new().read(true).write(true).create(true).truncate(false).open(lock_path)?;
        owner.try_lock().map_err(|_|Error::Invalid("worker ownership unavailable"))?;
        let db = Connection::open(path)?;
        db.busy_timeout(Duration::from_secs(2))?;
        let version:i64=db.query_row("PRAGMA user_version",[],|r|r.get(0))?;
        let tables:i64=db.query_row("SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",[],|r|r.get(0))?;
        if !matches!(version,1|2) && !(version==0 && tables==0) {
            return Err(Error::Invalid("unsupported store schema"));
        }
        db.execute_batch("
            PRAGMA foreign_keys=ON;
            PRAGMA journal_mode=WAL;
            PRAGMA synchronous=FULL;
            BEGIN IMMEDIATE;
            CREATE TABLE IF NOT EXISTS jobs (
                id TEXT PRIMARY KEY, status TEXT NOT NULL
                CHECK(status IN ('scraping','completed','failed','cancelled')),
                created_at INTEGER NOT NULL, finished_at INTEGER
            );
            CREATE TABLE IF NOT EXISTS items (
                job TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE, key TEXT NOT NULL,
                ordinal INTEGER NOT NULL,
                state TEXT NOT NULL CHECK(state IN ('queued','running','done','failed','cancelled')),
                generation INTEGER NOT NULL DEFAULT 0,
                result_seq INTEGER, payload TEXT,
                depth INTEGER NOT NULL DEFAULT 0 CHECK(depth BETWEEN 0 AND 10),
                PRIMARY KEY(job,key), UNIQUE(job,ordinal), UNIQUE(job,result_seq)
            );
            CREATE TABLE IF NOT EXISTS crawl_jobs (
                job TEXT PRIMARY KEY REFERENCES jobs(id) ON DELETE CASCADE,
                seed TEXT NOT NULL, page_limit INTEGER NOT NULL CHECK(page_limit BETWEEN 1 AND 100),
                max_depth INTEGER NOT NULL CHECK(max_depth BETWEEN 0 AND 10)
            );")?;
        if version==1 {
            db.execute_batch("ALTER TABLE items ADD COLUMN depth INTEGER NOT NULL DEFAULT 0 CHECK(depth BETWEEN 0 AND 10);")?;
        }
        db.execute_batch("PRAGMA user_version=2; COMMIT;")?;
        let journal:String=db.query_row("PRAGMA journal_mode",[],|r|r.get(0))?;
        if !journal.eq_ignore_ascii_case("wal") {return Err(Error::Invalid("WAL unavailable"));}
        let mut store=Self {db,_owner:owner};
        store.recover_after_worker_exit()?;
        Ok(store)
    }

    /// Fixed batch; duplicate keys reject the whole creation transaction.
    pub fn create(&mut self, id: &str, keys: &[String]) -> Result<()> {
        self.create_job(id,keys,None)
    }

    pub fn create_crawl(&mut self,id:&str,seed:&str,limit:usize,max_depth:u32)->Result<()> {
        if !(1..=MAX_ITEMS).contains(&limit) || max_depth>10 {
            return Err(Error::Invalid("invalid crawl limits"));
        }
        self.create_job(id,&[seed.to_owned()],Some((seed,limit,max_depth)))
    }

    fn create_job(&mut self,id:&str,keys:&[String],crawl:Option<(&str,usize,u32)>)->Result<()> {
        if id.is_empty() || id.len() > 128 || keys.is_empty() || keys.len() > MAX_ITEMS ||
            keys.iter().any(|k| k.is_empty() || k.len() > 8192) {
            return Err(Error::Invalid("invalid batch"));
        }
        let now=now_secs()?;
        self.purge_terminal_before(now.saturating_sub(RETENTION_SECS))?;
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let count: i64 = tx.query_row("SELECT COUNT(*) FROM jobs", [], |r| r.get(0))?;
        if count >= MAX_JOBS { return Err(Error::Invalid("job store full")); }
        tx.execute("INSERT INTO jobs(id,status,created_at) VALUES(?1,'scraping',?2)",params![id,now])?;
        if let Some((seed,limit,max_depth))=crawl {
            tx.execute("INSERT INTO crawl_jobs VALUES(?1,?2,?3,?4)",params![id,seed,limit as i64,max_depth])?;
        }
        for (i, key) in keys.iter().enumerate() {
            tx.execute("INSERT INTO items(job,key,ordinal,state) VALUES(?1,?2,?3,'queued')",
                params![id, key, i as i64])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The single scheduler can find queued work after process restart without
    /// remembering any job IDs in memory. Admission is oldest-job first.
    pub fn claim_next(&mut self)->Result<Option<Claim>> {
        let id:Option<String>=self.db.query_row("SELECT jobs.id FROM jobs JOIN items ON items.job=jobs.id
            WHERE jobs.status='scraping' AND items.state='queued' ORDER BY jobs.created_at,jobs.id,items.ordinal LIMIT 1",
            [],|r|r.get(0)).optional()?;
        match id {Some(id)=>self.claim(&id),None=>Ok(None)}
    }

    pub fn claim(&mut self, id: &str) -> Result<Option<Claim>> {
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let next: Option<(String,i64,u32)> = tx.query_row(
            "SELECT items.key,generation,depth FROM items JOIN jobs ON jobs.id=items.job
             WHERE items.job=?1 AND jobs.status='scraping' AND items.state='queued'
             ORDER BY ordinal LIMIT 1", [id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        let Some((item, old,depth)) = next else { return Ok(None); };
        let crawl=tx.query_row("SELECT seed,max_depth FROM crawl_jobs WHERE job=?1",[id],
            |r|Ok(CrawlClaim{seed:r.get(0)?,depth,max_depth:r.get(1)?})).optional()?;
        let generation = old.checked_add(1).ok_or(Error::Invalid("claim generation exhausted"))?;
        tx.execute("UPDATE items SET state='running',generation=?3 WHERE job=?1 AND key=?2",
            params![id,item,generation])?;
        tx.commit()?;
        Ok(Some(Claim { job:id.into(), item, generation, crawl }))
    }

    /// Atomic result and completion commit; old claims cannot overwrite results.
    pub fn settle(&mut self, claim: &Claim, ok: bool, payload: &Value) -> Result<()> {
        self.settle_discovered(claim,ok,payload,&[])
    }

    /// The worker has already checked each discovered URL's scope. Store result
    /// and newly discovered frontier atomically, so restart cannot lose links.
    pub fn settle_discovered(&mut self,claim:&Claim,ok:bool,payload:&Value,links:&[String])->Result<()> {
        if links.len()>MAX_ITEMS || links.iter().any(|s|s.is_empty()||s.len()>8192) ||
            (!links.is_empty() && (!ok || claim.crawl().is_none())) {
            return Err(Error::Invalid("invalid discovered links"));
        }
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
        if !links.is_empty() {
            let (limit, max_depth, depth):(i64,u32,u32)=tx.query_row(
                "SELECT page_limit,max_depth,items.depth FROM crawl_jobs JOIN items ON items.job=crawl_jobs.job
                 WHERE items.job=?1 AND items.key=?2",params![claim.job,claim.item],
                |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
            if depth<max_depth {
                let mut count:i64=tx.query_row("SELECT COUNT(*) FROM items WHERE job=?1",[&claim.job],|r|r.get(0))?;
                for link in links {
                    if count>=limit {break;}
                    let added=tx.execute("INSERT INTO items(job,key,ordinal,state,depth) VALUES(?1,?2,?3,'queued',?4)
                        ON CONFLICT(job,key) DO NOTHING",params![claim.job,link,count,depth+1])?;
                    count+=added as i64;
                }
            }
        }
        let pending: i64 = tx.query_row(
            "SELECT COUNT(*) FROM items WHERE job=?1 AND state IN ('queued','running')",
            [&claim.job], |r| r.get(0))?;
        if pending == 0 {
            tx.execute("UPDATE jobs SET status='completed',finished_at=?2 WHERE id=?1",params![claim.job,now_secs()?])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Called only during open, after the exclusive file lock is held.
    /// A requeued read may be repeated, but only its new generation can settle.
    fn recover_after_worker_exit(&mut self) -> Result<usize> {
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // An interrupted request may be repeated at most three times in
        // total across restarts; ordinary fetch failures are never retried.
        let exhausted:Vec<(String,String)>={
            let mut stmt=tx.prepare("SELECT job,key FROM items WHERE state='running' AND generation>=3
                AND job IN (SELECT id FROM jobs WHERE status='scraping')")?;
            let rows=stmt.query_map([],|r|Ok((r.get(0)?,r.get(1)?)))?;
            rows.collect::<std::result::Result<Vec<_>,_>>()?
        };
        for (job,key) in exhausted {
            let seq:i64=tx.query_row("SELECT COALESCE(MAX(result_seq),0)+1 FROM items WHERE job=?1",
                [&job],|r|r.get(0))?;
            tx.execute("UPDATE items SET state='failed',result_seq=?3,payload=?4 WHERE job=?1 AND key=?2",
                params![job,key,seq,json!({"error":"interrupted attempt budget exhausted"}).to_string()])?;
        }
        let n = tx.execute("UPDATE items SET state='queued' WHERE state='running'
            AND job IN (SELECT id FROM jobs WHERE status='scraping')", [])?;
        tx.execute("UPDATE jobs SET status='completed',finished_at=?1 WHERE status='scraping'
            AND NOT EXISTS (SELECT 1 FROM items WHERE items.job=jobs.id AND state IN ('queued','running'))",
            [now_secs()?])?;
        tx.commit()?;
        Ok(n)
    }

    /// Cancels pending claims transactionally. External in-flight requests
    /// still need cooperative cancellation by the eventual worker supervisor.
    pub fn cancel(&mut self, id: &str) -> Result<()> {
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if tx.execute("UPDATE jobs SET status='cancelled',finished_at=?2 WHERE id=?1 AND status='scraping'",params![id,now_secs()?])? != 1 {
            return Err(Error::Invalid("job missing or terminal"));
        }
        tx.execute("UPDATE items SET state='cancelled' WHERE job=?1 AND state IN ('queued','running')", [id])?;
        tx.commit()?;
        Ok(())
    }

    /// Retention starts when a job becomes terminal. Never delete active jobs
    /// to make room; their eventual supervisor must enforce deadlines.
    pub fn purge_expired(&mut self)->Result<usize> {
        self.purge_terminal_before(now_secs()?.saturating_sub(RETENTION_SECS))
    }
    fn purge_terminal_before(&mut self,cutoff:i64)->Result<usize> {
        Ok(self.db.execute("DELETE FROM jobs WHERE status!='scraping' AND finished_at<=?1",[cutoff])?)
    }

    /// Stable completion-order cursor, including per-item failures. No result
    /// is silently dropped when the response byte budget is reached.
    pub fn page(&mut self, id: &str, after: i64, limit: usize) -> Result<Value> {
        if !(0..=MAX_ITEMS as i64).contains(&after) || !(1..=MAX_ITEMS).contains(&limit) {
            return Err(Error::Invalid("invalid cursor"));
        }
        self.purge_expired()?;
        let tx = self.db.transaction()?;
        let status: String = tx.query_row("SELECT status FROM jobs WHERE id=?1", [id], |r| r.get(0))
            .optional()?.ok_or(Error::Invalid("job not found"))?;
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
        let crawl_limits:Option<(i64,u32)>=tx.query_row("SELECT page_limit,max_depth FROM crawl_jobs WHERE job=?1",
            [id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let mut result=json!({"status":status,"total":total,"completed":succeeded,"failed":failed,
            "cancelled":cancelled,"results":rows,"next_after":next});
        if let Some((page_limit,max_depth))=crawl_limits {
            let attempts:i64=tx.query_row("SELECT COALESCE(SUM(generation),0) FROM items WHERE job=?1",[id],|r|r.get(0))?;
            result["crawl"]=json!({"page_limit":page_limit,"max_discovery_depth":max_depth,
                "page_attempts":attempts,"page_attempt_limit":page_limit*3,"site_coverage_verified":false});
        }
        tx.commit()?;
        Ok(result)
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
        let b=db.claim_next().unwrap().unwrap();
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
    fn exclusive_owner_claims_distinct_items_and_cancel_rejects_late_write() {
        let tmp=Temp::new();
        let mut db=Store::open(&tmp.db()).unwrap();
        assert!(matches!(Store::open(&tmp.db()),Err(Error::Invalid("worker ownership unavailable"))));
        db.create("job",&keys()).unwrap();
        let a=db.claim("job").unwrap().unwrap();
        let b=db.claim_next().unwrap().unwrap();
        assert_ne!(a.item(),b.item());
        db.settle(&b,true,&json!({"value":2})).unwrap();
        db.cancel("job").unwrap();
        assert!(db.settle(&a,true,&json!({"value":1})).is_err());
        assert!(db.claim("job").unwrap().is_none());
        let result=db.page("job",0,100).unwrap();
        assert_eq!(result["completed"],1);
        assert_eq!(result["cancelled"],2);
        assert_eq!(result["status"],"cancelled");
    }

    #[test]
    fn v1_batch_database_migrates_without_losing_results_or_claims() {
        let tmp=Temp::new();
        let db=Connection::open(tmp.db()).unwrap();
        db.execute_batch("
            CREATE TABLE jobs (
                id TEXT PRIMARY KEY, status TEXT NOT NULL,
                created_at INTEGER NOT NULL, finished_at INTEGER
            );
            CREATE TABLE items (
                job TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE, key TEXT NOT NULL,
                ordinal INTEGER NOT NULL,state TEXT NOT NULL,generation INTEGER NOT NULL DEFAULT 0,
                result_seq INTEGER,payload TEXT,
                PRIMARY KEY(job,key), UNIQUE(job,ordinal), UNIQUE(job,result_seq)
            );
            INSERT INTO jobs VALUES('old','scraping',1,NULL);
            INSERT INTO items VALUES('old','a',0,'done',1,1,'{}');
            INSERT INTO items VALUES('old','b',1,'running',1,NULL,NULL);
            PRAGMA user_version=1;").unwrap();
        drop(db);
        let mut store=Store::open(&tmp.db()).unwrap();
        assert_eq!(store.page("old",0,10).unwrap()["completed"],1);
        let b=store.claim_next().unwrap().unwrap();
        assert_eq!(b.item(),"b");
        assert!(b.crawl().is_none());
        store.settle(&b,true,&json!({"value":42})).unwrap();
        assert_eq!(store.page("old",0,10).unwrap()["completed"],2);
        store.create_crawl("new","seed",2,1).unwrap();
        assert_eq!(store.claim_next().unwrap().unwrap().crawl().unwrap().depth,0);
    }

    #[test]
    fn crawl_frontier_atomic_restart_dedup_depth_and_failure_limit() {
        let tmp=Temp::new();
        let mut db=Store::open(&tmp.db()).unwrap();
        db.create_crawl("crawl","seed",4,2).unwrap();
        let seed=db.claim_next().unwrap().unwrap();
        assert_eq!(seed.crawl().unwrap().depth,0);
        db.settle_discovered(&seed,true,&json!({"page":0}),&["seed".into(),"a".into(),"a".into(),"b".into()]).unwrap();
        let old=db.claim_next().unwrap().unwrap();
        assert_eq!(old.item(),"a");
        drop(db);
        let mut db=Store::open(&tmp.db()).unwrap();
        let a=db.claim_next().unwrap().unwrap();
        assert_eq!(a.item(),"a");
        assert!(db.settle_discovered(&old,true,&json!({}),&["wrong".into()]).is_err());
        db.settle_discovered(&a,true,&json!({"page":1}),&["b".into(),"c".into(),"over-limit".into()]).unwrap();
        let b=db.claim_next().unwrap().unwrap();
        assert_eq!(b.item(),"b");
        db.settle(&b,false,&json!({"error":"fixture"})).unwrap();
        let c=db.claim_next().unwrap().unwrap();
        assert_eq!(c.item(),"c");
        assert_eq!(c.crawl().unwrap().depth,2);
        db.settle_discovered(&c,true,&json!({"page":2}),&["too-deep".into()]).unwrap();
        assert!(db.claim_next().unwrap().is_none());
        let result=db.page("crawl",0,100).unwrap();
        assert_eq!(result["total"],4);
        assert_eq!(result["completed"],3);
        assert_eq!(result["failed"],1);
        assert_eq!(result["crawl"]["page_attempts"],5);
        assert_eq!(result["crawl"]["page_attempt_limit"],12);
        assert_eq!(result["crawl"]["site_coverage_verified"],false);
    }

    #[test]
    fn crawl_depth_zero_and_failed_discovery() {
        let tmp=Temp::new();
        let mut db=Store::open(&tmp.db()).unwrap();
        db.create_crawl("crawl","seed",100,0).unwrap();
        let seed=db.claim_next().unwrap().unwrap();
        assert!(db.settle_discovered(&seed,false,&json!({}),&["a".into()]).is_err());
        db.settle_discovered(&seed,true,&json!({}),&["a".into()]).unwrap();
        assert!(db.claim_next().unwrap().is_none());
        assert_eq!(db.page("crawl",0,10).unwrap()["total"],1);
    }

    #[test]
    fn restart_attempt_budget_is_bounded() {
        let tmp=Temp::new();
        let mut db=Store::open(&tmp.db()).unwrap();
        db.create("job",&["a".into()]).unwrap();
        for _ in 0..3 {
            assert!(db.claim_next().unwrap().is_some());
            drop(db);
            db=Store::open(&tmp.db()).unwrap();
        }
        assert!(db.claim_next().unwrap().is_none());
        let page=db.page("job",0,10).unwrap();
        assert_eq!(page["status"],"completed");
        assert_eq!(page["failed"],1);
        assert_eq!(page["completed"],0);
    }

    #[test]
    fn refuses_unknown_existing_schema() {
        let tmp=Temp::new();
        let db=Connection::open(tmp.db()).unwrap();
        db.execute("CREATE TABLE unrelated(x INTEGER)",[]).unwrap();
        drop(db);
        assert!(matches!(Store::open(&tmp.db()),Err(Error::Invalid("unsupported store schema"))));
    }

    #[test]
    fn terminal_retention_reclaims_results_without_removing_active_jobs() {
        let tmp=Temp::new();
        let mut db=Store::open(&tmp.db()).unwrap();
        db.create("old",&["a".into()]).unwrap();
        let a=db.claim_next().unwrap().unwrap();
        assert_eq!(a.job(),"old");
        db.settle(&a,true,&json!({"value":42})).unwrap();
        db.create("active",&keys()).unwrap();
        db.db.execute("UPDATE jobs SET finished_at=1 WHERE id='old'",[]).unwrap();
        assert_eq!(db.purge_terminal_before(2).unwrap(),1);
        assert!(db.page("old",0,1).is_err());
        assert_eq!(db.page("active",0,1).unwrap()["status"],"scraping");
        let remaining:i64=db.db.query_row("SELECT COUNT(*) FROM items WHERE job='old'",[],|r|r.get(0)).unwrap();
        assert_eq!(remaining,0);
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
