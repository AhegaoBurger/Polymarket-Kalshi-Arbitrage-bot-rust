//! CSV trackers — signals, trades, executions, sessions.
//! Reference: tracker.py.

use anyhow::{Context, Result};
use chrono::Utc;
use csv::{Writer, WriterBuilder};
use serde::Serialize;
use std::fs::{create_dir_all, File, OpenOptions};
use std::path::PathBuf;
use std::sync::Mutex;

pub struct Tracker {
    pub log_dir: PathBuf,
    pub log_executions: bool,
    signals:    Mutex<Option<Writer<File>>>,
    trades:     Mutex<Option<Writer<File>>>,
    executions: Mutex<Option<Writer<File>>>,
    sessions:   Mutex<Option<Writer<File>>>,
}

impl Tracker {
    pub fn new(log_dir: impl Into<PathBuf>, log_executions: bool) -> Result<Self> {
        let dir = log_dir.into();
        create_dir_all(&dir).with_context(|| format!("create log dir {:?}", dir))?;
        Ok(Self {
            log_dir: dir,
            log_executions,
            signals: Mutex::new(None),
            trades: Mutex::new(None),
            executions: Mutex::new(None),
            sessions: Mutex::new(None),
        })
    }

    fn writer_for(&self, name: &str) -> Result<Writer<File>> {
        let path = self.log_dir.join(name);
        // Skip header emission when appending to an existing non-empty file —
        // otherwise resumed sessions write a duplicate header mid-file, breaking
        // pandas / spreadsheet imports.
        let has_existing_data = path.metadata().map(|m| m.len() > 0).unwrap_or(false);
        let f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open {:?}", path))?;
        Ok(WriterBuilder::new()
            .has_headers(!has_existing_data)
            .from_writer(f))
    }

    pub fn log_signal(&self, row: SignalRow) -> Result<()> {
        let mut guard = self.signals.lock().unwrap();
        if guard.is_none() {
            *guard = Some(self.writer_for("signals.csv")?);
        }
        let w = guard.as_mut().unwrap();
        w.serialize(row)?;
        w.flush()?;
        Ok(())
    }

    pub fn log_trade(&self, row: TradeRow) -> Result<()> {
        let mut guard = self.trades.lock().unwrap();
        if guard.is_none() {
            *guard = Some(self.writer_for("trades.csv")?);
        }
        let w = guard.as_mut().unwrap();
        w.serialize(row)?;
        w.flush()?;
        Ok(())
    }

    pub fn log_execution(&self, row: ExecutionRow) -> Result<()> {
        if !self.log_executions {
            return Ok(());
        }
        let mut guard = self.executions.lock().unwrap();
        if guard.is_none() {
            *guard = Some(self.writer_for("executions.csv")?);
        }
        let w = guard.as_mut().unwrap();
        w.serialize(row)?;
        w.flush()?;
        Ok(())
    }

    pub fn log_session(&self, row: SessionRow) -> Result<()> {
        let mut guard = self.sessions.lock().unwrap();
        if guard.is_none() {
            *guard = Some(self.writer_for("sessions.csv")?);
        }
        let w = guard.as_mut().unwrap();
        w.serialize(row)?;
        w.flush()?;
        Ok(())
    }

    pub fn now_iso() -> String {
        Utc::now().to_rfc3339()
    }
}

#[derive(Debug, Serialize)]
pub struct SignalRow {
    pub ts: String,
    pub window_ts: i64,
    pub side: String,
    pub btc_delta_pct: f64,
    pub seconds_remaining: u64,
    pub market_price: f64,
    pub true_prob: f64,
    pub edge: f64,
    pub bet_usd: f64,
    pub vol: f64,
    pub skip_reason: String,
}

#[derive(Debug, Serialize)]
pub struct TradeRow {
    pub ts: String,
    pub window_ts: i64,
    pub side: String,
    pub price: f64,
    pub shares: i64,
    pub usd: f64,
    pub order_id: String,
    pub status: String,
    pub resolution: Option<String>,
    pub pnl: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct ExecutionRow {
    pub ts: String,
    pub endpoint: String,
    pub method: String,
    pub status: u16,
    pub latency_ms: u64,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SessionRow {
    pub ts: String,
    pub event: String,
    pub bankroll: f64,
    pub trades_count: u32,
    pub wins: u32,
    pub losses: u32,
    pub session_pnl: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn signal_log_creates_file_and_writes() {
        let dir = tempdir().unwrap();
        let t = Tracker::new(dir.path(), true).unwrap();
        t.log_signal(SignalRow {
            ts: "2026-05-14T12:00:00Z".into(),
            window_ts: 1,
            side: "UP".into(),
            btc_delta_pct: 0.1,
            seconds_remaining: 150,
            market_price: 0.7,
            true_prob: 0.88,
            edge: 0.18,
            bet_usd: 12.5,
            vol: 0.12,
            skip_reason: "".into(),
        })
        .unwrap();
        let body = std::fs::read_to_string(dir.path().join("signals.csv")).unwrap();
        assert!(body.contains("UP"));
        assert!(body.contains("0.88"));
    }
}
