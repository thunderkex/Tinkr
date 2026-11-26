use crate::{io_err, provider::Provider, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path, time::Duration};

pub const RECALL_ROUNDTRIP_OVERHEAD: u64 = 24;
pub const DEFAULT_MARKER_TOKENS: u64 = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Default)]
pub struct NetMeta {
    pub agent: String,
    pub marker_tokens: u64,
    pub rule_tokens: u64,
}

pub fn detect_active_agent() -> String {
    if let Ok(a) = std::env::var("TINKR_AGENT") {
        let t = a.trim().to_ascii_lowercase();
        if !t.is_empty() {
            return t;
        }
    }
    if std::env::var_os("CLAUDECODE").is_some() || std::env::var_os("CLAUDE_PROJECT_DIR").is_some()
    {
        return "claude".into();
    }
    if std::env::var_os("CODEX_CI").is_some() || std::env::var_os("CODEX_SANDBOX_TYPE").is_some() {
        return "codex".into();
    }
    if std::env::var_os("ANTIGRAVITY_CLI_ALIAS").is_some()
        || std::env::var_os("AGY_ACCOUNT").is_some()
    {
        return "antigravity".into();
    }
    "cli".into()
}

fn default_net_meta(e: &Event) -> NetMeta {
    let agent = match e.layer {
        Layer::SayLess if e.tool == "antigravity" => "antigravity".to_string(),
        Layer::SendLess | Layer::SayLess => "proxy".to_string(),
        Layer::ReadLess if e.tool.starts_with("mcp-") => "mcp".to_string(),
        Layer::ReadLess => detect_active_agent(),
    };
    let marker_tokens = if e.recall_id.is_some() && e.after >= DEFAULT_MARKER_TOKENS {
        DEFAULT_MARKER_TOKENS
    } else {
        0
    };
    let rule_tokens = if e.layer == Layer::SayLess {
        crate::say_less::rule_token_count()
    } else {
        0
    };
    NetMeta {
        agent,
        marker_tokens,
        rule_tokens,
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct LayerSummary {
    pub events: u64,
    pub before: u64,
    pub after: u64,
    pub saved: u64,
    pub gross_saved: i64,
    pub rule_tokens: u64,
    pub marker_tokens: u64,
    pub recall_tokens: u64,
    pub net_saved: i64,
    pub net_percent: f64,
    pub recalls: u64,
    pub recall_rate_pct: f64,
    pub measured_events: u64,
    pub estimated_events: u64,
}

impl LayerSummary {
    fn finalize(&mut self) {
        let body_after = self.after.saturating_sub(self.marker_tokens);
        self.gross_saved = (self.before as i64) - (body_after as i64);
        self.net_saved = self.gross_saved
            - (self.rule_tokens as i64)
            - (self.marker_tokens as i64)
            - (self.recall_tokens as i64);
        self.net_percent = if self.before == 0 {
            0.0
        } else {
            (self.net_saved as f64) * 100.0 / (self.before as f64)
        };
        self.recall_rate_pct = if self.events == 0 {
            0.0
        } else {
            (self.recalls as f64) * 100.0 / (self.events as f64)
        };
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProfileNetStatus {
    pub key: String,
    pub layer: String,
    pub tool: String,
    pub events: u64,
    pub net_saved: i64,
    pub recall_rate_pct: f64,
    pub reason: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct Summary {
    pub events: u64,
    pub before: u64,
    pub after: u64,
    pub saved: u64,
    pub percent: f64,
    pub gross_saved: i64,
    pub rule_tokens: u64,
    pub marker_tokens: u64,
    pub recall_tokens: u64,
    pub net_saved: i64,
    pub net_percent: f64,
    pub recalls: u64,
    pub recall_rate_pct: f64,
    pub measured_events: u64,
    pub estimated_events: u64,
    pub by_layer: BTreeMap<String, LayerSummary>,
    pub by_provider: BTreeMap<String, LayerSummary>,
    pub by_tool: BTreeMap<String, LayerSummary>,
    pub by_agent: BTreeMap<String, LayerSummary>,
    pub auto_disabled: Vec<ProfileNetStatus>,
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
  recall_id TEXT,
  agent TEXT NOT NULL DEFAULT 'cli',
  marker_tokens INTEGER NOT NULL DEFAULT 0,
  rule_tokens INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS events_ts ON events(ts);
CREATE TABLE IF NOT EXISTS recalls(
  id INTEGER PRIMARY KEY,
  ts INTEGER NOT NULL,
  recall_id TEXT NOT NULL,
  tool TEXT NOT NULL,
  event_id INTEGER,
  layer TEXT NOT NULL DEFAULT 'read_less',
  agent TEXT NOT NULL DEFAULT 'cli',
  provider TEXT NOT NULL DEFAULT 'none',
  recalled_tokens INTEGER NOT NULL DEFAULT 0,
  within_3_turns INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX IF NOT EXISTS recalls_ts ON recalls(ts);
";

const MIGRATIONS: &[&str] = &[
    "ALTER TABLE events ADD COLUMN agent TEXT NOT NULL DEFAULT 'cli'",
    "ALTER TABLE events ADD COLUMN marker_tokens INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE events ADD COLUMN rule_tokens INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE recalls ADD COLUMN event_id INTEGER",
    "ALTER TABLE recalls ADD COLUMN layer TEXT NOT NULL DEFAULT 'read_less'",
    "ALTER TABLE recalls ADD COLUMN agent TEXT NOT NULL DEFAULT 'cli'",
    "ALTER TABLE recalls ADD COLUMN provider TEXT NOT NULL DEFAULT 'none'",
    "ALTER TABLE recalls ADD COLUMN recalled_tokens INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE recalls ADD COLUMN within_3_turns INTEGER NOT NULL DEFAULT 1",
];

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
        for m in MIGRATIONS {
            let _ = conn.execute(m, []);
        }
        Ok(Self { conn })
    }

    fn init(conn: Connection) -> Result<Self> {
        Ok(Self::init_conn(conn)?)
    }

    pub fn record(&self, e: &Event) -> Result<i64> {
        let meta = default_net_meta(e);
        self.record_net(e, &meta)
    }

    pub fn record_net(&self, e: &Event, meta: &NetMeta) -> Result<i64> {
        let tool = crate::recall::redact_str(&e.tool);
        let session = e.session.as_deref().map(crate::recall::redact_str);
        let agent = if meta.agent.trim().is_empty() {
            "cli".to_string()
        } else {
            crate::recall::redact_str(&meta.agent).into_owned()
        };
        let clamped_marker = meta.marker_tokens.min(e.after);
        self.conn.execute(
            "INSERT INTO events(ts,layer,provider,tool,session,tokens_before,tokens_after,kind,recall_id,agent,marker_tokens,rule_tokens)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                e.ts,
                e.layer.as_str(),
                e.provider.as_str(),
                tool.as_ref(),
                session.as_deref(),
                e.before as i64,
                e.after as i64,
                e.kind.as_str(),
                e.recall_id,
                agent,
                clamped_marker as i64,
                meta.rule_tokens as i64
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn record_recall(&self, recall_id: &str, ts: i64) -> Result<()> {
        let id_clean = recall_id.trim().to_ascii_lowercase();
        let like_pat = format!("{id_clean}%");
        let matched: Option<(i64, String, String, String, String, i64)> = self
            .conn
            .query_row(
                "SELECT id, layer, provider, tool, agent, tokens_before
                 FROM events WHERE recall_id = ?1 OR recall_id LIKE ?2
                 ORDER BY id DESC LIMIT 1",
                params![id_clean, like_pat],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .ok();

        let (ev_id, layer, provider, tool, agent, recalled_tokens, within_3_turns) = match matched {
            Some((eid, lyr, prov, tl, ag, before)) => {
                let events_after: i64 = self
                    .conn
                    .query_row(
                        "SELECT COUNT(*) FROM events WHERE id > ?1",
                        params![eid],
                        |r| r.get(0),
                    )
                    .unwrap_or(0);
                let within = i64::from(events_after <= 3);
                let rtok = (before.max(0) as u64) + RECALL_ROUNDTRIP_OVERHEAD;
                (Some(eid), lyr, prov, tl, ag, rtok, within)
            }
            None => (
                None,
                "read_less".to_string(),
                "none".to_string(),
                "unknown".to_string(),
                detect_active_agent(),
                RECALL_ROUNDTRIP_OVERHEAD,
                1,
            ),
        };

        self.conn.execute(
            "INSERT INTO recalls(ts, recall_id, tool, event_id, layer, agent, provider, recalled_tokens, within_3_turns)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                ts,
                id_clean,
                tool,
                ev_id,
                layer,
                agent,
                provider,
                recalled_tokens as i64,
                within_3_turns
            ],
        )?;
        Ok(())
    }

    pub fn recall_count(&self, tool: &str, since: i64) -> Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM recalls WHERE tool=?1 AND ts>=?2",
            params![tool, since],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    pub fn has_session_event(&self, layer: Layer, session: &str) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM events WHERE layer=?1 AND session=?2",
            params![layer.as_str(), session],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    pub fn negative_net_profiles(&self, min_samples: u64) -> Result<Vec<ProfileNetStatus>> {
        let s = self.summary(None)?;
        let mut out = Vec::new();
        for (tool, ls) in &s.by_tool {
            if ls.events >= min_samples && ls.net_saved <= 0 {
                let layer = self.dominant_layer_for_tool(tool).unwrap_or_else(|| "read_less".into());
                let key = format!("{layer}:{tool}");
                out.push(ProfileNetStatus {
                    reason: format!(
                        "NET {:+} tok over {} samples (recall rate {:.1}%)",
                        ls.net_saved, ls.events, ls.recall_rate_pct
                    ),
                    key,
                    layer,
                    tool: tool.clone(),
                    events: ls.events,
                    net_saved: ls.net_saved,
                    recall_rate_pct: ls.recall_rate_pct,
                });
            }
        }
        Ok(out)
    }

    fn dominant_layer_for_tool(&self, tool: &str) -> Option<String> {
        self.conn
            .query_row(
                "SELECT layer FROM events WHERE tool=?1 GROUP BY layer ORDER BY COUNT(*) DESC LIMIT 1",
                params![tool],
                |r| r.get(0),
            )
            .ok()
    }

    pub fn summary(&self, since: Option<i64>) -> Result<Summary> {
        let min_ts = since.unwrap_or(i64::MIN);
        let mut stmt = self.conn.prepare(
            "SELECT layer, provider, tool, agent, kind, COUNT(*),
                    COALESCE(SUM(tokens_before),0), COALESCE(SUM(tokens_after),0),
                    COALESCE(SUM(marker_tokens),0), COALESCE(SUM(rule_tokens),0)
             FROM events WHERE ts>=?1
             GROUP BY layer, provider, tool, agent, kind",
        )?;
        let rows = stmt.query_map(params![min_ts], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)? as u64,
                r.get::<_, i64>(6)? as u64,
                r.get::<_, i64>(7)? as u64,
                r.get::<_, i64>(8)? as u64,
                r.get::<_, i64>(9)? as u64,
            ))
        })?;

        let mut s = Summary::default();
        for row in rows {
            let (layer, provider, tool, agent, kind, n, b, a, mtok, rtok) = row?;
            for bucket in [
                s.by_layer.entry(layer).or_default(),
                s.by_provider.entry(provider).or_default(),
                s.by_tool.entry(tool).or_default(),
                s.by_agent.entry(agent).or_default(),
            ] {
                bucket.events += n;
                bucket.before += b;
                bucket.after += a;
                bucket.saved += b.saturating_sub(a);
                bucket.marker_tokens += mtok;
                bucket.rule_tokens += rtok;
                if kind == "measured" {
                    bucket.measured_events += n;
                } else {
                    bucket.estimated_events += n;
                }
            }
            s.events += n;
            s.before += b;
            s.after += a;
            s.marker_tokens += mtok;
            s.rule_tokens += rtok;
            if kind == "measured" {
                s.measured_events += n;
            } else {
                s.estimated_events += n;
            }
        }

        let mut rstmt = self.conn.prepare(
            "SELECT layer, provider, tool, agent,
                    COALESCE(SUM(within_3_turns),0),
                    COALESCE(SUM(recalled_tokens),0)
             FROM recalls WHERE ts>=?1
             GROUP BY layer, provider, tool, agent",
        )?;
        let rrows = rstmt.query_map(params![min_ts], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)? as u64,
                r.get::<_, i64>(5)? as u64,
            ))
        })?;
        for rrow in rrows {
            let (layer, provider, tool, agent, w3_count, r_tokens) = rrow?;
            for bucket in [
                s.by_layer.entry(layer).or_default(),
                s.by_provider.entry(provider).or_default(),
                s.by_tool.entry(tool).or_default(),
                s.by_agent.entry(agent).or_default(),
            ] {
                bucket.recalls += w3_count;
                bucket.recall_tokens += r_tokens;
            }
            s.recalls += w3_count;
            s.recall_tokens += r_tokens;
        }

        for v in s.by_layer.values_mut() {
            v.finalize();
        }
        for v in s.by_provider.values_mut() {
            v.finalize();
        }
        for v in s.by_tool.values_mut() {
            v.finalize();
        }
        for v in s.by_agent.values_mut() {
            v.finalize();
        }

        s.saved = s.before.saturating_sub(s.after);
        s.percent = if s.before == 0 {
            0.0
        } else {
            s.saved as f64 * 100.0 / s.before as f64
        };
        let body_after = s.after.saturating_sub(s.marker_tokens);
        s.gross_saved = (s.before as i64) - (body_after as i64);
        s.net_saved = s.gross_saved
            - (s.rule_tokens as i64)
            - (s.marker_tokens as i64)
            - (s.recall_tokens as i64);
        s.net_percent = if s.before == 0 {
            0.0
        } else {
            (s.net_saved as f64) * 100.0 / (s.before as f64)
        };
        s.recall_rate_pct = if s.events == 0 {
            0.0
        } else {
            (s.recalls as f64) * 100.0 / (s.events as f64)
        };

        for (tool, ls) in &s.by_tool {
            if ls.events >= crate::auto::MIN_NET_DISABLE_SAMPLES && ls.net_saved <= 0 {
                let layer = self
                    .dominant_layer_for_tool(tool)
                    .unwrap_or_else(|| "read_less".into());
                s.auto_disabled.push(ProfileNetStatus {
                    key: format!("{layer}:{tool}"),
                    layer,
                    tool: tool.clone(),
                    events: ls.events,
                    net_saved: ls.net_saved,
                    recall_rate_pct: ls.recall_rate_pct,
                    reason: format!(
                        "NET {:+} tok over {} samples (recall rate {:.1}%)",
                        ls.net_saved, ls.events, ls.recall_rate_pct
                    ),
                });
            }
        }
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
    fn summary_aggregates_gross_and_net() {
        let l = Ledger::open_in_memory().unwrap();
        l.record_net(
            &Event {
                recall_id: Some("a1b2c3".into()),
                ..ev(Layer::ReadLess, 1000, 215, CountKind::Measured, 10)
            },
            &NetMeta {
                agent: "claude".into(),
                marker_tokens: 15,
                rule_tokens: 0,
            },
        )
        .unwrap();
        l.record_net(
            &ev(Layer::SendLess, 500, 250, CountKind::Estimated, 20),
            &NetMeta {
                agent: "proxy".into(),
                marker_tokens: 0,
                rule_tokens: 50,
            },
        )
        .unwrap();
        let s = l.summary(None).unwrap();
        assert_eq!((s.events, s.before, s.after, s.saved), (2, 1500, 465, 1035));
        assert_eq!(s.gross_saved, 1050);
        assert_eq!(s.net_saved, 985);
        assert_eq!(s.by_agent["claude"].net_saved, 785);
        assert_eq!(s.by_agent["proxy"].net_saved, 200);
        assert_eq!(l.summary(Some(15)).unwrap().events, 1);
    }

    #[test]
    fn recalls_deduct_roundtrip_and_flag_negative_net_profiles() {
        let l = Ledger::open_in_memory().unwrap();
        for i in 0..20 {
            let rid = format!("dead{:02x}", i);
            l.record_net(
                &Event {
                    ts: 100 + i,
                    layer: Layer::ReadLess,
                    provider: Provider::None,
                    tool: "noisy-custom".into(),
                    session: None,
                    before: 400,
                    after: 300,
                    kind: CountKind::Measured,
                    recall_id: Some(rid.clone()),
                },
                &NetMeta {
                    agent: "claude".into(),
                    marker_tokens: 14,
                    rule_tokens: 0,
                },
            )
            .unwrap();
            if i < 6 {
                l.record_recall(&rid, 101 + i).unwrap();
            }
        }
        let s = l.summary(None).unwrap();
        assert!(s.by_tool["noisy-custom"].net_saved < 0);
        assert_eq!(s.auto_disabled.len(), 1);
        assert_eq!(s.auto_disabled[0].key, "read_less:noisy-custom");
    }

    #[test]
    fn empty_summary() {
        let s = Ledger::open_in_memory().unwrap().summary(None).unwrap();
        assert_eq!(s.percent, 0.0);
        assert_eq!(s.net_saved, 0);
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
            .record(&ev(Layer::SayLess, 500, 100, CountKind::Estimated, 1))
            .unwrap();
    }
}
