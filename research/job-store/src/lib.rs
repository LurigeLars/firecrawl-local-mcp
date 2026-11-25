//! Embedded durable batch and crawl frontier experiment. No HTTP routes or network calls.
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use std::{fs::{File,OpenOptions}, path::Path, time::{Duration,SystemTime,UNIX_EPOCH}};

const MAX_JOBS: i64 = 16;
const MAX_WORKERS: i64 = 3;
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
pub struct CrawlClaim { pub seed: String, pub depth: u32, pub max_depth: u32, pub include_sitemap: bool, pub page_limit: u32, pub policy_json: String, pub max_concurrency:u32 }
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
        if !matches!(version,1|2|3|4|5) && !(version==0 && tables==0) {
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
                max_depth INTEGER NOT NULL CHECK(max_depth BETWEEN 0 AND 10),
                include_sitemap INTEGER NOT NULL DEFAULT 0 CHECK(include_sitemap IN (0,1)),
                policy_json TEXT NOT NULL DEFAULT '{}',
                max_concurrency INTEGER NOT NULL DEFAULT 1 CHECK(max_concurrency BETWEEN 1 AND 4)
            );")?;
        if version==1 {
            db.execute_batch("ALTER TABLE items ADD COLUMN depth INTEGER NOT NULL DEFAULT 0 CHECK(depth BETWEEN 0 AND 10);")?;
        }
        if version==2 {
            db.execute_batch("ALTER TABLE crawl_jobs ADD COLUMN include_sitemap INTEGER NOT NULL DEFAULT 0 CHECK(include_sitemap IN (0,1));")?;
        }
        if matches!(version,2|3) {
            db.execute_batch("ALTER TABLE crawl_jobs ADD COLUMN policy_json TEXT NOT NULL DEFAULT '{}';")?;
        }
        if matches!(version,2|3|4){
            db.execute_batch("ALTER TABLE crawl_jobs ADD COLUMN max_concurrency INTEGER NOT NULL DEFAULT 1 CHECK(max_concurrency BETWEEN 1 AND 4);")?;
        }
        db.execute_batch("PRAGMA user_version=5; COMMIT;")?;
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
        self.create_crawl_with_sitemap(id,seed,limit,max_depth,false)
    }

    pub fn create_crawl_with_sitemap(&mut self,id:&str,seed:&str,limit:usize,max_depth:u32,include_sitemap:bool)->Result<()> {
        self.create_crawl_configured(id,seed,limit,max_depth,include_sitemap,&json!({}))
    }

    pub fn create_crawl_configured(&mut self,id:&str,seed:&str,limit:usize,max_depth:u32,
        include_sitemap:bool,policy:&Value)->Result<()> {
        self.create_crawl_parallel(id,seed,limit,max_depth,include_sitemap,policy,1)
    }

    pub fn create_crawl_parallel(&mut self,id:&str,seed:&str,limit:usize,max_depth:u32,
        include_sitemap:bool,policy:&Value,max_concurrency:u32)->Result<()> {
        if !(1..=MAX_ITEMS).contains(&limit) || max_depth>10 || !(1..=4).contains(&max_concurrency) {
            return Err(Error::Invalid("invalid crawl limits"));
        }
        let raw=serde_json::to_string(policy)?;
        if !policy.is_object()||raw.len()>8192{return Err(Error::Invalid("invalid crawl policy"));}
        self.create_job(id,&[seed.to_owned()],Some((seed,limit,max_depth,include_sitemap,&raw,max_concurrency)))
    }

    fn create_job(&mut self,id:&str,keys:&[String],crawl:Option<(&str,usize,u32,bool,&str,u32)>)->Result<()> {
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
        if let Some((seed,limit,max_depth,include_sitemap,policy,max_concurrency))=crawl {
            tx.execute("INSERT INTO crawl_jobs(job,seed,page_limit,max_depth,include_sitemap,policy_json,max_concurrency) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![id,seed,limit as i64,max_depth,include_sitemap,policy,max_concurrency])?;
        }
        for (i, key) in keys.iter().enumerate() {
            tx.execute("INSERT INTO items(job,key,ordinal,state) VALUES(?1,?2,?3,'queued')",
                params![id, key, i as i64])?;
        }
        tx.commit()?;
        Ok(())
    }

    // Choose an eligible job, then reserve a distinct item transactionally.
    // The one process owns this connection behind a mutex. The claim query
    // independently rechecks the job cap and breadth-first barrier.
    pub fn claim_next(&mut self)->Result<Option<Claim>> {
        let id:Option<String>=self.db.query_row("SELECT jobs.id FROM jobs JOIN items ON items.job=jobs.id
            LEFT JOIN crawl_jobs c ON c.job=jobs.id
            WHERE jobs.status='scraping' AND items.state='queued'
            AND (SELECT COUNT(*) FROM items r WHERE r.job=jobs.id AND r.state='running')<MIN(COALESCE(c.max_concurrency,?1),?1)
            AND (c.job IS NULL OR NOT EXISTS(SELECT 1 FROM items p WHERE p.job=jobs.id
                AND p.state IN ('queued','running') AND p.depth<items.depth))
            ORDER BY jobs.created_at,jobs.id,items.depth,items.ordinal LIMIT 1",
            [MAX_WORKERS],|r|r.get(0)).optional()?;
        match id {Some(id)=>self.claim(&id),None=>Ok(None)}
    }

    pub fn claim(&mut self, id: &str) -> Result<Option<Claim>> {
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let next: Option<(String,i64,u32)> = tx.query_row(
            "SELECT items.key,generation,depth FROM items JOIN jobs ON jobs.id=items.job
             LEFT JOIN crawl_jobs c ON c.job=jobs.id
             WHERE items.job=?1 AND jobs.status='scraping' AND items.state='queued'
             AND (SELECT COUNT(*) FROM items r WHERE r.job=jobs.id AND r.state='running')<MIN(COALESCE(c.max_concurrency,?2),?2)
             AND (c.job IS NULL OR NOT EXISTS(SELECT 1 FROM items p WHERE p.job=jobs.id
                AND p.state IN ('queued','running') AND p.depth<items.depth))
             ORDER BY depth,ordinal LIMIT 1", params![id,MAX_WORKERS], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        let Some((item, old,depth)) = next else { return Ok(None); };
        let crawl=tx.query_row("SELECT seed,max_depth,include_sitemap,page_limit,policy_json,max_concurrency FROM crawl_jobs WHERE job=?1",[id],
            |r|Ok(CrawlClaim{seed:r.get(0)?,depth,max_depth:r.get(1)?,include_sitemap:r.get(2)?,page_limit:r.get(3)?,
                policy_json:r.get(4)?,max_concurrency:r.get(5)?})).optional()?;
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

    /// Firecrawl v2 status uses an offset over successful documents, not
    /// over every settled claim. Keep that view separate from the research
    /// cursor (which includes failures). All counts and documents use one
    /// read transaction so a newly committed result cannot shift the page.
    pub fn crawl_status_v2(&mut self,id:&str,skip:usize,limit:usize)->Result<Value>{
        if skip>MAX_ITEMS||!(1..=MAX_ITEMS).contains(&limit){
            return Err(Error::Invalid("invalid cursor"));
        }
        self.purge_expired()?;
        let tx=self.db.transaction()?;
        let (status,created,finished,include_sitemap,max_concurrency):(String,i64,Option<i64>,bool,i64)=tx.query_row(
            "SELECT jobs.status,created_at,finished_at,include_sitemap,max_concurrency FROM jobs JOIN crawl_jobs ON crawl_jobs.job=jobs.id WHERE jobs.id=?1",
            [id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?.ok_or(Error::Invalid("job not found"))?;
        let (total,succeeded,failed,cancelled,attempts):(i64,i64,i64,i64,i64)=tx.query_row(
            "SELECT COUNT(*),SUM(state='done'),SUM(state='failed'),SUM(state='cancelled'),COALESCE(SUM(generation),0)
             FROM items WHERE job=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
        if skip as i64>succeeded{return Err(Error::Invalid("cursor exceeds committed results"));}
        let mut docs=Vec::new();
        let mut bytes=2;
        {
            let mut stmt=tx.prepare("SELECT payload FROM items WHERE job=?1 AND state='done'
                ORDER BY result_seq LIMIT ?2 OFFSET ?3")?;
            let mut rows=stmt.query(params![id,limit as i64,skip as i64])?;
            while let Some(row)=rows.next()?{
                let raw:String=row.get(0)?;
                let size=raw.len()+usize::from(!docs.is_empty());
                if bytes+size>MAX_PAGE{
                    if docs.is_empty(){return Err(Error::Invalid("single result exceeds page bound"));}
                    break;
                }
                bytes+=size;
                docs.push(serde_json::from_str::<Value>(&raw)?);
            }
        }
        let offset=skip+docs.len();
        let next=if status=="scraping"||(offset as i64)<succeeded{Some(offset)}else{None};
        // An all-failed crawl is not reported as successfully completed.
        let api_status=if status=="completed"&&succeeded==0&&failed>0{"failed"}else{status.as_str()};
        let expiry=finished.unwrap_or(now_secs()?).saturating_add(RETENTION_SECS);
        let value=json!({"status":api_status,"completed":succeeded,"total":total-failed-cancelled,
            "data":docs,"next_skip":next,"created_at":created,"finished_at":finished,
            "expires_at":expiry,"research":{"discovered":total,"failed":failed,"cancelled":cancelled,
            "page_attempts":attempts,"sitemap_mode":if include_sitemap{"include"}else{"skip"},
                "requested_max_concurrency":max_concurrency,"worker_concurrency_cap":max_concurrency.min(MAX_WORKERS),
                "same_domain_network_concurrency":1,
                "site_coverage_verified":false,"cache_used":false}});
        tx.commit()?;
        Ok(value)
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
        let crawl_limits:Option<(i64,u32,bool)>=tx.query_row("SELECT page_limit,max_depth,include_sitemap FROM crawl_jobs WHERE job=?1",
            [id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        let mut result=json!({"status":status,"total":total,"completed":succeeded,"failed":failed,
            "cancelled":cancelled,"results":rows,"next_after":next});
        if let Some((page_limit,max_depth,include_sitemap))=crawl_limits {
            let attempts:i64=tx.query_row("SELECT COALESCE(SUM(generation),0) FROM items WHERE job=?1",[id],|r|r.get(0))?;
            result["crawl"]=json!({"page_limit":page_limit,"max_discovery_depth":max_depth,
                "page_attempts":attempts,"page_attempt_limit":page_limit*3,
                "sitemap_mode":if include_sitemap{"include"}else{"skip"},"site_coverage_verified":false});
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
    fn v2_crawl_status_offsets_only_successful_documents(){
        let tmp=Temp::new();
        let mut db=Store::open(&tmp.db()).unwrap();
        db.create_crawl("crawl","seed",4,2).unwrap();
        let seed=db.claim_next().unwrap().unwrap();
        db.settle_discovered(&seed,true,&json!({"markdown":"seed"}),&["a".into(),"b".into()]).unwrap();
        let a=db.claim_next().unwrap().unwrap();
        db.settle(&a,false,&json!({"error":"fixture"})).unwrap();
        let p=db.crawl_status_v2("crawl",0,1).unwrap();
        assert_eq!(p["status"],"scraping");
        assert_eq!(p["completed"],1);
        assert_eq!(p["next_skip"],1);
        let b=db.claim_next().unwrap().unwrap();
        db.settle(&b,true,&json!({"markdown":"second"})).unwrap();
        let first=db.crawl_status_v2("crawl",0,1).unwrap();
        assert_eq!(first["status"],"completed");
        assert_eq!(first["completed"],2);
        assert_eq!(first["research"]["discovered"],3);
        assert_eq!(first["research"]["failed"],1);
        assert_eq!(first["next_skip"],1);
        let second=db.crawl_status_v2("crawl",1,1).unwrap();
        assert_eq!(second["data"][0]["markdown"],"second");
        assert!(second["next_skip"].is_null());
        db.create("batch",&["a".into()]).unwrap();
        assert!(db.crawl_status_v2("batch",0,1).is_err());
    }

    #[test]
    fn sitemap_mode_survives_restart_and_v2_schema_migration() {
        let tmp=Temp::new();
        let mut db=Store::open(&tmp.db()).unwrap();
        db.create_crawl("legacy","old",2,1).unwrap();
        // Simulate exact previous schema, without changing existing jobs.
        db.db.execute_batch("ALTER TABLE crawl_jobs DROP COLUMN max_concurrency; ALTER TABLE crawl_jobs DROP COLUMN policy_json; ALTER TABLE crawl_jobs DROP COLUMN include_sitemap; PRAGMA user_version=2;").unwrap();
        drop(db);
        let mut db=Store::open(&tmp.db()).unwrap();
        let old=db.claim("legacy").unwrap().unwrap();
        assert!(!old.crawl().unwrap().include_sitemap);
        db.settle(&old,true,&json!({"markdown":"legacy"})).unwrap();
        db.create_crawl_with_sitemap("new","seed",3,1,true).unwrap();
        let stale=db.claim("new").unwrap().unwrap();
        assert!(stale.crawl().unwrap().include_sitemap);
        drop(db);
        let mut db=Store::open(&tmp.db()).unwrap();
        let seed=db.claim("new").unwrap().unwrap();
        assert!(seed.crawl().unwrap().include_sitemap);
        assert_eq!(seed.crawl().unwrap().page_limit,3);
        assert!(db.settle_discovered(&stale,true,&json!({}),&["stale".into()]).is_err());
        db.settle_discovered(&seed,true,&json!({"researchSitemap":{"status":"read"}}),
            &["a".into(),"a".into(),"b".into(),"over-limit".into()]).unwrap();
        drop(db);
        let mut db=Store::open(&tmp.db()).unwrap();
        let next=db.claim("new").unwrap().unwrap();
        assert_eq!(next.item(),"a");
        assert_eq!(next.crawl().unwrap().depth,1);
        let page=db.crawl_status_v2("new",0,10).unwrap();
        assert_eq!(page["research"]["sitemap_mode"],"include");
        assert_eq!(page["data"][0]["researchSitemap"]["status"],"read");
        assert_eq!(page["total"],3);
        assert_eq!(db.crawl_status_v2("legacy",0,10).unwrap()["research"]["sitemap_mode"],"skip");
    }

    #[test]
    fn configured_policy_is_durable_and_v3_migrates_without_change(){
        let tmp=Temp::new();
        let mut db=Store::open(&tmp.db()).unwrap();
        db.create_crawl_with_sitemap("old","seed",2,1,true).unwrap();
        db.db.execute_batch("ALTER TABLE crawl_jobs DROP COLUMN max_concurrency; ALTER TABLE crawl_jobs DROP COLUMN policy_json; PRAGMA user_version=3;").unwrap();
        drop(db);
        let mut db=Store::open(&tmp.db()).unwrap();
        let old=db.claim("old").unwrap().unwrap();
        assert_eq!(old.crawl().unwrap().policy_json,"{}");
        assert!(old.crawl().unwrap().include_sitemap);
        db.settle(&old,true,&json!({})).unwrap();
        let policy=json!({"includePaths":["/docs/"],"excludePaths":["private"],"delayMs":1250,"onlyMainContent":false});
        assert!(db.create_crawl_configured("bad","seed",2,1,true,&json!("x")).is_err());
        assert!(db.create_crawl_configured("huge","seed",2,1,true,&json!({"x":"x".repeat(8192)})).is_err());
        db.create_crawl_configured("new","seed",2,1,true,&policy).unwrap();
        let claim=db.claim("new").unwrap().unwrap();
        db.settle_discovered(&claim,true,&json!({}),&["next".into()]).unwrap();
        drop(db);
        let mut db=Store::open(&tmp.db()).unwrap();
        let next=db.claim("new").unwrap().unwrap();
        assert_eq!(next.item(),"next");
        assert_eq!(serde_json::from_str::<Value>(&next.crawl().unwrap().policy_json).unwrap(),policy);
    }

    #[test]
    fn crawl_parallel_caps_depth_barrier_and_v4_migration(){
        let tmp=Temp::new();
        let mut db=Store::open(&tmp.db()).unwrap();
        db.create_crawl("legacy","seed",2,1).unwrap();
        db.db.execute_batch("ALTER TABLE crawl_jobs DROP COLUMN max_concurrency; PRAGMA user_version=4;").unwrap();
        drop(db);
        let mut db=Store::open(&tmp.db()).unwrap();
        let legacy=db.claim("legacy").unwrap().unwrap();
        assert_eq!(legacy.crawl().unwrap().max_concurrency,1);
        db.settle(&legacy,true,&json!({})).unwrap();
        assert!(db.create_crawl_parallel("bad","seed",10,3,false,&json!({}),5).is_err());
        db.create_crawl_parallel("parallel","seed",10,3,false,&json!({}),2).unwrap();
        let seed=db.claim("parallel").unwrap().unwrap();
        db.settle_discovered(&seed,true,&json!({}),&["a".into(),"b".into(),"c".into()]).unwrap();
        let a=db.claim("parallel").unwrap().unwrap();
        let b=db.claim("parallel").unwrap().unwrap();
        assert_eq!(a.crawl().unwrap().max_concurrency,2);
        assert!(db.claim("parallel").unwrap().is_none());
        db.create_crawl("other","seed",1,0).unwrap();
        assert_eq!(db.claim_next().unwrap().unwrap().job(),"other");
        db.settle_discovered(&b,true,&json!({}),&["deep".into()]).unwrap();
        let c=db.claim("parallel").unwrap().unwrap();
        assert_eq!(c.item(),"c");
        db.settle(&c,true,&json!({})).unwrap();
        // Deep work must not overtake the still-running shallower page.
        assert!(db.claim("parallel").unwrap().is_none());
        drop(db);
        let mut db=Store::open(&tmp.db()).unwrap();
        let recovered=db.claim("parallel").unwrap().unwrap();
        assert_eq!(recovered.item(),"a");
        assert_eq!(recovered.crawl().unwrap().max_concurrency,2);
        assert!(db.settle(&a,true,&json!({})).is_err());
        db.settle(&recovered,true,&json!({})).unwrap();
        assert_eq!(db.claim("parallel").unwrap().unwrap().item(),"deep");
        let status=db.crawl_status_v2("parallel",0,10).unwrap();
        assert_eq!(status["research"]["requested_max_concurrency"],2);
        assert_eq!(status["research"]["worker_concurrency_cap"],2);
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
