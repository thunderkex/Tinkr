use crate::{io_err, provider::Provider, Result};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::{collections::BTreeMap, path::Path, time::Duration};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    ReadLess,
    SendLess,
    SayLess,
}

impl Layer {
    pub fn as_str(self) -> &'static str {
        match self {
            Layer::ReadLess => "read_less",
            Layer::SendLess => "send_less",
            Layer::SayLess => "say_less",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CountKind {
    Measured,
    Estimated,
}

impl CountKind {
    pub fn as_str(self) -> &'static str {
        match self {
            CountKind::Measured => "measured",
            CountKind::Estimated => "estimated",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Event {
    pub ts: i64,
    pub layer: Layer,
    pub provider: Provider,
    pub tool: String,
    pub session: Option<String>,
    pub before: u64,
    pub after: u64,
    pub kind: CountKind,
    pub recall_id: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, PartialEq)]
pub struct LayerSummary {
    pub events: u64,
    pub before: u64,
    pub after: u64,
    pub saved: u64,
    pub measured_events: u64,
    pub estimated_events: u64,
}

#[derive(Debug, Default, Clone, Serialize, PartialEq)]
pub struct Summary {
    pub events: u64,
    pub before: u64,
    pub after: u64,
    pub saved: u64,
    pub percent: f64,
    pub measured_events: u64,
    pub estimated_events: u64,
    pub by_layer: BTreeMap<String, LayerSummary>,
    pub by_provider: BTreeMap<String, LayerSummary>,
    pub by_tool: BTreeMap<String, LayerSummary>,
}

pub struct Ledger {
    conn: Connection,
}

fn is_corrupt_err(e: &rusqlite::Error) -> bool {
    matches!(
        e.sqlite_error_code(),
        Some(
            rusqlite::ErrorCode::DatabaseCorrupt
                | rusqlite::ErrorCode::NotADatabase
                | rusqlite::ErrorCode::DiskFull
        )
    )
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS events(
  id INTEGER PRIMARY KEY,
  ts INTEGER NOT NULL,
  layer TEXT NOT NULL,
  provider TEXT NOT NULL DEFAULT 'none',
  tool TEXT NOT NULL,
  session TEXT,
  tokens_before INTEGER NOT NULL,
  tokens_after INTEGER NOT NULL,
  kind TEXT NOT NULL,
  recall_id TEXT
);
CREATE INDEX IF NOT EXISTS events_ts ON events(ts);
";

impl Ledger {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(io_err(dir))?;
        }
        match Connection::open(path).and_then(Self::init_conn) {
            Ok(l) => Ok(l),
            Err(e) if is_corrupt_err(&e) => {
                let mut bad = path.as_os_str().to_os_string();
                bad.push(".corrupt");
                let _ = std::fs::rename(path, &bad);
                let _ = std::fs::remove_file(path);
                Self::init(Connection::open(path)?)
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init_conn(conn: Connection) -> rusqlite::Result<Self> {
        conn.busy_timeout(Duration::from_secs(5))?;
        let _mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
        conn.execute_batch("PRAGMA synchronous=NORMAL;")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    fn init(conn: Connection) -> Result<Self> {
        Ok(Self::init_conn(conn)?)
    }

    pub fn record(&self, e: &Event) -> Result<i64> {
        let tool = crate::recall::redact_str(&e.tool);
        let session = e.session.as_deref().map(crate::recall::redact_str);
        self.conn.execute(
            "INSERT INTO events(ts,layer,provider,tool,session,tokens_before,tokens_after,kind,recall_id)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                e.ts,
                e.layer.as_str(),
                e.provider.as_str(),
                tool.as_ref(),
                session.as_deref(),
                e.before as i64,
                e.after as i64,
                e.kind.as_str(),
                e.recall_id
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn recall_count(&self, tool: &str, since: i64) -> Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM events WHERE tool=?1 AND ts>=?2 AND recall_id IS NOT NULL
             AND layer='read_less'",
            params![tool, since],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    pub fn summary(&self, since: Option<i64>) -> Result<Summary> {
        let mut stmt = self.conn.prepare(
            "SELECT layer, provider, tool, kind, COUNT(*), COALESCE(SUM(tokens_before),0), COALESCE(SUM(tokens_after),0)
             FROM events WHERE ts>=?1 GROUP BY layer, provider, tool, kind",
        )?;
        let rows = stmt.query_map(params![since.unwrap_or(i64::MIN)], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)? as u64,
                r.get::<_, i64>(5)? as u64,
                r.get::<_, i64>(6)? as u64,
            ))
        })?;
        let mut s = Summary::default();
        for row in rows {
            let (layer, provider, tool, kind, n, b, a) = row?;
            for l in [
                s.by_layer.entry(layer).or_default(),
                s.by_provider.entry(provider).or_default(),
                s.by_tool.entry(tool).or_default(),
            ] {
                l.events += n;
                l.before += b;
                l.after += a;
                l.saved += b.saturating_sub(a);
                if kind == "measured" {
                    l.measured_events += n
                } else {
                    l.estimated_events += n
                }
            }
            s.events += n;
            s.before += b;
            s.after += a;
            if kind == "measured" {
                s.measured_events += n
            } else {
                s.estimated_events += n
            }
        }
        s.saved = s.before.saturating_sub(s.after);
        s.percent = if s.before == 0 {
            0.0
        } else {
            s.saved as f64 * 100.0 / s.before as f64
        };
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(layer: Layer, before: u64, after: u64, kind: CountKind, ts: i64) -> Event {
        Event {
            ts,
            layer,
            provider: Provider::Anthropic,
            tool: "git".into(),
            session: None,
            before,
            after,
            kind,
            recall_id: None,
        }
    }

    #[test]
    fn summary_aggregates() {
        let l = Ledger::open_in_memory().unwrap();
        l.record(&ev(Layer::ReadLess, 1000, 200, CountKind::Measured, 10))
            .unwrap();
        l.record(&ev(Layer::SendLess, 500, 250, CountKind::Estimated, 20))
            .unwrap();
        let s = l.summary(None).unwrap();
        assert_eq!((s.events, s.before, s.after, s.saved), (2, 1500, 450, 1050));
        assert!((s.percent - 70.0).abs() < 1e-9);
        assert_eq!((s.measured_events, s.estimated_events), (1, 1));
        assert_eq!(s.by_layer["read_less"].saved, 800);
        assert_eq!(l.summary(Some(15)).unwrap().events, 1);
    }

    #[test]
    fn empty_summary() {
        let s = Ledger::open_in_memory().unwrap().summary(None).unwrap();
        assert_eq!(s.percent, 0.0);
        assert_eq!(s.events, 0);
    }

    #[test]
    fn concurrent_writers_on_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ledger.db");
        Ledger::open(&p).unwrap();
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let p = p.clone();
                std::thread::spawn(move || {
                    let l = Ledger::open(&p).unwrap();
                    for j in 0..25 {
                        l.record(&ev(
                            Layer::ReadLess,
                            100,
                            10,
                            CountKind::Measured,
                            i * 100 + j,
                        ))
                        .unwrap();
                    }
                })
            })
            .collect();
        handles.into_iter().for_each(|h| h.join().unwrap());
        assert_eq!(Ledger::open(&p).unwrap().summary(None).unwrap().events, 100);
    }

    #[test]
    fn unicode_path_with_spaces() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("dönër kebab").join("läger.db");
        Ledger::open(&p)
            .unwrap()
            .record(&ev(Layer::SayLess, 10, 5, CountKind::Estimated, 1))
            .unwrap();
    }
}
