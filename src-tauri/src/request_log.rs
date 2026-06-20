use crate::types::{
    DayTokens, ModelTokens, OpenAiAccountQuota, ProviderLocalUsage, ProviderProtocol,
    ProviderUsageStatus, RequestLogPage, RequestRecord, TokenStats, TokenTotals, TokenUsage,
};
use chrono::{DateTime, Local, Utc};
use rusqlite::{params, Connection};
use std::{
    collections::{BTreeSet, HashMap},
    path::Path,
    sync::Mutex,
};

/// SQLite-backed, permanently persisted request log + token analytics.
pub struct RequestLog {
    conn: Mutex<Connection>,
}

const MAX_PAGE_SIZE: usize = 200;

impl RequestLog {
    pub fn open(path: &Path) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        conn.pragma_update(None, "synchronous", "NORMAL").ok();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS requests (
                id TEXT PRIMARY KEY,
                started_at TEXT NOT NULL,
                model TEXT NOT NULL DEFAULT '',
                requested_model TEXT,
                route_reason TEXT,
                provider_id TEXT,
                provider_name TEXT,
                provider_protocol TEXT,
                status INTEGER NOT NULL DEFAULT 0,
                latency_ms INTEGER NOT NULL DEFAULT 0,
                streaming INTEGER NOT NULL DEFAULT 0,
                error TEXT,
                reasoning_effort TEXT,
                stream_state TEXT,
                stream_error TEXT,
                last_event TEXT,
                stream_bytes INTEGER NOT NULL DEFAULT 0,
                input_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                cache_write_tokens INTEGER NOT NULL DEFAULT 0,
                total_tokens INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_requests_started_at ON requests(started_at DESC);
            CREATE INDEX IF NOT EXISTS idx_requests_model ON requests(model);
            CREATE INDEX IF NOT EXISTS idx_requests_provider_id ON requests(provider_id);
            CREATE TABLE IF NOT EXISTS provider_usage_snapshots (
                provider_id TEXT PRIMARY KEY,
                updated_at TEXT NOT NULL,
                source TEXT NOT NULL,
                error TEXT,
                quota_json TEXT
            );",
        )
        .map_err(|e| e.to_string())?;
        let _ = conn.execute("ALTER TABLE requests ADD COLUMN requested_model TEXT", []);
        let _ = conn.execute("ALTER TABLE requests ADD COLUMN route_reason TEXT", []);
        let _ = conn.execute("ALTER TABLE requests ADD COLUMN reasoning_effort TEXT", []);
        let _ = conn.execute("ALTER TABLE requests ADD COLUMN stream_state TEXT", []);
        let _ = conn.execute("ALTER TABLE requests ADD COLUMN stream_error TEXT", []);
        let _ = conn.execute("ALTER TABLE requests ADD COLUMN last_event TEXT", []);
        let _ = conn.execute(
            "ALTER TABLE requests ADD COLUMN stream_bytes INTEGER NOT NULL DEFAULT 0",
            [],
        );
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn insert(&self, record: &RequestRecord) {
        let conn = self.conn.lock().unwrap();
        let protocol = record
            .provider_protocol
            .as_ref()
            .map(protocol_to_str)
            .map(|s| s.to_string());
        let _ = conn.execute(
            "INSERT OR REPLACE INTO requests
              (id, started_at, model, requested_model, route_reason, provider_id, provider_name, provider_protocol,
               status, latency_ms, streaming, error, reasoning_effort,
               stream_state, stream_error, last_event,
               stream_bytes, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, total_tokens)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)",
            params![
                record.id,
                record.started_at.to_rfc3339(),
                record.model,
                record.requested_model,
                record.route_reason,
                record.provider_id,
                record.provider_name,
                protocol,
                record.status,
                record.latency_ms as i64,
                record.streaming as i64,
                record.error,
                record.reasoning_effort,
                record.stream_state,
                record.stream_error,
                record.last_event,
                record.stream_bytes as i64,
                record.usage.input_tokens as i64,
                record.usage.output_tokens as i64,
                record.usage.cache_read_tokens as i64,
                record.usage.cache_write_tokens as i64,
                record.usage.total_tokens as i64,
            ],
        );
    }

    pub fn update_stream_progress(&self, id: &str, stream_bytes: u64, usage: Option<&TokenUsage>) {
        let conn = self.conn.lock().unwrap();
        if let Some(usage) = usage.filter(|usage| !usage.is_empty()) {
            let _ = conn.execute(
                "UPDATE requests SET stream_bytes=?2, input_tokens=?3, output_tokens=?4,
                    cache_read_tokens=?5, cache_write_tokens=?6, total_tokens=?7
                 WHERE id=?1",
                params![
                    id,
                    stream_bytes as i64,
                    usage.input_tokens as i64,
                    usage.output_tokens as i64,
                    usage.cache_read_tokens as i64,
                    usage.cache_write_tokens as i64,
                    usage.total_tokens as i64,
                ],
            );
            return;
        }

        let _ = conn.execute(
            "UPDATE requests SET stream_bytes=?2 WHERE id=?1",
            params![id, stream_bytes as i64],
        );
    }

    pub fn update_stream_status(
        &self,
        id: &str,
        stream_state: &str,
        stream_error: Option<&str>,
        last_event: Option<&str>,
    ) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "UPDATE requests
             SET stream_state=?2, stream_error=?3, last_event=?4
             WHERE id=?1",
            params![id, stream_state, stream_error, last_event],
        );
    }

    pub fn recent(&self, limit: usize) -> Vec<RequestRecord> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare(
            "SELECT id, started_at, model, provider_id, provider_name, provider_protocol,
                    requested_model, route_reason, status, latency_ms, streaming, error, reasoning_effort,
                    stream_state, stream_error, last_event,
                    stream_bytes, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, total_tokens
             FROM requests ORDER BY started_at DESC LIMIT ?1",
        ) {
            Ok(stmt) => stmt,
            Err(_) => return Vec::new(),
        };
        let rows = stmt
            .query_map(params![limit as i64], row_to_record)
            .and_then(|mapped| mapped.collect::<Result<Vec<_>, _>>())
            .unwrap_or_default();
        rows
    }

    pub fn count(&self) -> u64 {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM requests", [], |row| {
            row.get::<_, i64>(0)
        })
        .map(|count| count.max(0) as u64)
        .unwrap_or(0)
    }

    pub fn page(&self, page: usize, page_size: usize) -> RequestLogPage {
        let page = page.max(1);
        let page_size = if page_size == 0 {
            50
        } else {
            page_size.clamp(1, MAX_PAGE_SIZE)
        };
        let offset = (page - 1).saturating_mul(page_size);
        let conn = self.conn.lock().unwrap();
        let total = conn
            .query_row("SELECT COUNT(*) FROM requests", [], |row| {
                row.get::<_, i64>(0)
            })
            .map(|count| count.max(0) as u64)
            .unwrap_or(0);
        let mut stmt = match conn.prepare(
            "SELECT id, started_at, model, provider_id, provider_name, provider_protocol,
                    requested_model, route_reason, status, latency_ms, streaming, error, reasoning_effort,
                    stream_state, stream_error, last_event,
                    stream_bytes, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, total_tokens
             FROM requests ORDER BY started_at DESC LIMIT ?1 OFFSET ?2",
        ) {
            Ok(stmt) => stmt,
            Err(_) => {
                return RequestLogPage {
                    records: Vec::new(),
                    total,
                    page,
                    page_size,
                }
            }
        };
        let records = stmt
            .query_map(params![page_size as i64, offset as i64], row_to_record)
            .and_then(|mapped| mapped.collect::<Result<Vec<_>, _>>())
            .unwrap_or_default();

        RequestLogPage {
            records,
            total,
            page,
            page_size,
        }
    }

    pub fn clear(&self) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute("DELETE FROM requests", []);
    }

    pub fn stats(&self) -> TokenStats {
        let conn = self.conn.lock().unwrap();
        let now = Local::now();
        let today = now.date_naive();
        let yesterday = today.pred_opt().unwrap_or(today);
        let start_today = day_start_rfc3339(today);
        let start_yesterday = day_start_rfc3339(yesterday);
        let end_yesterday = start_today.clone();
        let start_7 = day_start_rfc3339(today - chrono::Duration::days(6));

        let today_totals = totals_between(&conn, &start_today, None);
        let yesterday_totals = totals_between(&conn, &start_yesterday, Some(&end_yesterday));
        let last7 = totals_between(&conn, &start_7, None);
        let all_time = totals_between(&conn, "", None);

        // 7-day series, one bucket per local day.
        let mut series = Vec::with_capacity(7);
        for offset in (0..7).rev() {
            let day = today - chrono::Duration::days(offset);
            let start = day_start_rfc3339(day);
            let end = day_start_rfc3339(day + chrono::Duration::days(1));
            let t = totals_between(&conn, &start, Some(&end));
            series.push(DayTokens {
                date: day.format("%Y-%m-%d").to_string(),
                total_tokens: t.total_tokens,
                input_tokens: t.input_tokens,
                output_tokens: t.output_tokens,
                requests: t.requests,
            });
        }

        let by_model = model_totals(&conn);

        TokenStats {
            today: today_totals,
            yesterday: yesterday_totals,
            last7,
            all_time,
            series,
            by_model,
        }
    }

    pub fn provider_local_usage(&self) -> Vec<ProviderLocalUsage> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare(
            "SELECT COALESCE(provider_id, ''), model,
                    SUM(input_tokens), SUM(output_tokens), SUM(cache_read_tokens),
                    SUM(cache_write_tokens), SUM(total_tokens), COUNT(*)
             FROM requests
             WHERE provider_id IS NOT NULL AND provider_id != ''
             GROUP BY provider_id, model",
        ) {
            Ok(stmt) => stmt,
            Err(_) => return Vec::new(),
        };
        let mut by_provider: HashMap<String, ProviderLocalUsage> = HashMap::new();
        let rows = match stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?.max(0) as u64,
                row.get::<_, i64>(3)?.max(0) as u64,
                row.get::<_, i64>(4)?.max(0) as u64,
                row.get::<_, i64>(5)?.max(0) as u64,
                row.get::<_, i64>(6)?.max(0) as u64,
                row.get::<_, i64>(7)?.max(0) as u64,
            ))
        }) {
            Ok(rows) => rows,
            Err(_) => return Vec::new(),
        };

        let mut unknown: HashMap<String, BTreeSet<String>> = HashMap::new();
        let mut known_cost: HashMap<String, f64> = HashMap::new();
        for row in rows.flatten() {
            let (
                provider_id,
                model,
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens,
                total_tokens,
                requests,
            ) = row;
            let entry =
                by_provider
                    .entry(provider_id.clone())
                    .or_insert_with(|| ProviderLocalUsage {
                        provider_id: provider_id.clone(),
                        ..ProviderLocalUsage::default()
                    });
            entry.input_tokens += input_tokens;
            entry.output_tokens += output_tokens;
            entry.cache_read_tokens += cache_read_tokens;
            entry.cache_write_tokens += cache_write_tokens;
            entry.total_tokens += total_tokens;
            entry.requests += requests;
            if total_tokens > 0 {
                if let Some(cost) = crate::pricing::estimate_model_cost_usd(
                    &model,
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                    cache_write_tokens,
                ) {
                    *known_cost.entry(provider_id).or_insert(0.0) += cost;
                } else {
                    unknown.entry(provider_id).or_default().insert(model);
                }
            }
        }

        for usage in by_provider.values_mut() {
            usage.estimated_cost_usd = known_cost.get(&usage.provider_id).copied();
            usage.unknown_cost_models = unknown
                .remove(&usage.provider_id)
                .map(|models| models.into_iter().collect())
                .unwrap_or_default();
        }

        let mut values = by_provider.into_values().collect::<Vec<_>>();
        values.sort_by(|a, b| a.provider_id.cmp(&b.provider_id));
        values
    }

    pub fn provider_usage_snapshots(&self) -> Vec<ProviderUsageStatus> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare(
            "SELECT provider_id, updated_at, source, error, quota_json
             FROM provider_usage_snapshots",
        ) {
            Ok(stmt) => stmt,
            Err(_) => return Vec::new(),
        };
        stmt.query_map([], |row| {
            let provider_id: String = row.get(0)?;
            let updated_at = row
                .get::<_, String>(1)
                .ok()
                .and_then(|value| DateTime::parse_from_rfc3339(&value).ok())
                .map(|value| value.with_timezone(&Utc));
            let quota = row
                .get::<_, Option<String>>(4)
                .ok()
                .flatten()
                .and_then(|value| serde_json::from_str::<OpenAiAccountQuota>(&value).ok());
            Ok(ProviderUsageStatus {
                provider_id: provider_id.clone(),
                quota,
                local_usage: ProviderLocalUsage {
                    provider_id,
                    ..ProviderLocalUsage::default()
                },
                updated_at,
                source: row.get(2).unwrap_or_else(|_| "unknown".to_string()),
                error: row.get(3).ok(),
            })
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    pub fn upsert_provider_usage_snapshot(
        &self,
        provider_id: &str,
        source: &str,
        quota: Option<&OpenAiAccountQuota>,
        error: Option<&str>,
    ) {
        let conn = self.conn.lock().unwrap();
        let quota_json = quota.and_then(|quota| serde_json::to_string(quota).ok());
        let _ = conn.execute(
            "INSERT INTO provider_usage_snapshots
                (provider_id, updated_at, source, error, quota_json)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(provider_id) DO UPDATE SET
                updated_at=excluded.updated_at,
                source=excluded.source,
                error=excluded.error,
                quota_json=COALESCE(excluded.quota_json, provider_usage_snapshots.quota_json)",
            params![
                provider_id,
                Utc::now().to_rfc3339(),
                source,
                error,
                quota_json,
            ],
        );
    }
}

fn totals_between(conn: &Connection, start: &str, end: Option<&str>) -> TokenTotals {
    let (sql, has_end) = if end.is_some() {
        (
            "SELECT COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0),
                    COALESCE(SUM(cache_read_tokens),0), COALESCE(SUM(cache_write_tokens),0),
                    COALESCE(SUM(total_tokens),0), COUNT(*)
             FROM requests WHERE started_at >= ?1 AND started_at < ?2",
            true,
        )
    } else if start.is_empty() {
        (
            "SELECT COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0),
                    COALESCE(SUM(cache_read_tokens),0), COALESCE(SUM(cache_write_tokens),0),
                    COALESCE(SUM(total_tokens),0), COUNT(*)
             FROM requests",
            false,
        )
    } else {
        (
            "SELECT COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0),
                    COALESCE(SUM(cache_read_tokens),0), COALESCE(SUM(cache_write_tokens),0),
                    COALESCE(SUM(total_tokens),0), COUNT(*)
             FROM requests WHERE started_at >= ?1",
            false,
        )
    };

    let mapper = |row: &rusqlite::Row| {
        Ok(TokenTotals {
            input_tokens: row.get::<_, i64>(0)? as u64,
            output_tokens: row.get::<_, i64>(1)? as u64,
            cache_read_tokens: row.get::<_, i64>(2)? as u64,
            cache_write_tokens: row.get::<_, i64>(3)? as u64,
            total_tokens: row.get::<_, i64>(4)? as u64,
            requests: row.get::<_, i64>(5)? as u64,
        })
    };

    let result = if has_end {
        conn.query_row(sql, params![start, end.unwrap()], mapper)
    } else if start.is_empty() {
        conn.query_row(sql, [], mapper)
    } else {
        conn.query_row(sql, params![start], mapper)
    };
    result.unwrap_or_default()
}

fn model_totals(conn: &Connection) -> Vec<ModelTokens> {
    let mut stmt = match conn.prepare(
        "SELECT model,
                COALESCE(SUM(total_tokens),0), COALESCE(SUM(input_tokens),0),
                COALESCE(SUM(output_tokens),0), COALESCE(SUM(cache_read_tokens),0),
                COALESCE(SUM(cache_write_tokens),0), COUNT(*)
         FROM requests WHERE model <> ''
         GROUP BY model ORDER BY SUM(total_tokens) DESC",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map([], |row| {
        Ok(ModelTokens {
            model: row.get(0)?,
            total_tokens: row.get::<_, i64>(1)? as u64,
            input_tokens: row.get::<_, i64>(2)? as u64,
            output_tokens: row.get::<_, i64>(3)? as u64,
            cache_read_tokens: row.get::<_, i64>(4)? as u64,
            cache_write_tokens: row.get::<_, i64>(5)? as u64,
            requests: row.get::<_, i64>(6)? as u64,
        })
    })
    .and_then(|m| m.collect())
    .unwrap_or_default()
}

fn row_to_record(row: &rusqlite::Row) -> rusqlite::Result<RequestRecord> {
    let started: String = row.get(1)?;
    let protocol: Option<String> = row.get(5)?;
    Ok(RequestRecord {
        id: row.get(0)?,
        started_at: DateTime::parse_from_rfc3339(&started)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
        model: row.get(2)?,
        requested_model: row.get(6)?,
        route_reason: row.get(7)?,
        provider_id: row.get(3)?,
        provider_name: row.get(4)?,
        provider_protocol: protocol.as_deref().and_then(protocol_from_str),
        status: row.get::<_, i64>(8)? as u16,
        latency_ms: row.get::<_, i64>(9)? as u128,
        streaming: row.get::<_, i64>(10)? != 0,
        error: row.get(11)?,
        reasoning_effort: row.get(12)?,
        stream_state: row.get(13)?,
        stream_error: row.get(14)?,
        last_event: row.get(15)?,
        stream_bytes: row.get::<_, i64>(16)? as u64,
        usage: TokenUsage {
            input_tokens: row.get::<_, i64>(17)? as u64,
            output_tokens: row.get::<_, i64>(18)? as u64,
            cache_read_tokens: row.get::<_, i64>(19)? as u64,
            cache_write_tokens: row.get::<_, i64>(20)? as u64,
            total_tokens: row.get::<_, i64>(21)? as u64,
        },
    })
}

fn day_start_rfc3339(date: chrono::NaiveDate) -> String {
    date.and_hms_opt(0, 0, 0)
        .and_then(|naive| naive.and_local_timezone(Local).single())
        .map(|dt| dt.with_timezone(&Utc).to_rfc3339())
        .unwrap_or_default()
}

fn protocol_to_str(p: &ProviderProtocol) -> &'static str {
    match p {
        ProviderProtocol::OpenAiResponses => "open_ai_responses",
        ProviderProtocol::OpenAiChatCompletions => "open_ai_chat_completions",
        ProviderProtocol::AnthropicMessages => "anthropic_messages",
    }
}

fn protocol_from_str(s: &str) -> Option<ProviderProtocol> {
    match s {
        "open_ai_responses" => Some(ProviderProtocol::OpenAiResponses),
        "open_ai_chat_completions" => Some(ProviderProtocol::OpenAiChatCompletions),
        "anthropic_messages" => Some(ProviderProtocol::AnthropicMessages),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn record(id: &str, offset: i64) -> RequestRecord {
        RequestRecord {
            id: id.into(),
            started_at: Utc::now() + Duration::seconds(offset),
            model: format!("model-{id}"),
            requested_model: None,
            route_reason: Some("direct".into()),
            provider_id: Some("provider-1".into()),
            provider_name: Some("Provider 1".into()),
            provider_protocol: Some(ProviderProtocol::OpenAiResponses),
            status: 200,
            latency_ms: 100,
            streaming: true,
            error: None,
            reasoning_effort: Some("high".into()),
            stream_state: Some("pending".into()),
            stream_error: None,
            last_event: None,
            stream_bytes: 0,
            usage: TokenUsage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                total_tokens: 15,
            },
        }
    }

    #[test]
    fn pages_requests_without_truncating_total_count() {
        let temp = tempfile::tempdir().unwrap();
        let log = RequestLog::open(&temp.path().join("requests.db")).unwrap();
        log.insert(&record("old", 1));
        log.insert(&record("middle", 2));
        log.insert(&record("new", 3));

        let first = log.page(1, 2);
        let second = log.page(2, 2);

        assert_eq!(log.count(), 3);
        assert_eq!(first.total, 3);
        assert_eq!(first.records.len(), 2);
        assert_eq!(first.records[0].id, "new");
        assert_eq!(first.records[1].id, "middle");
        assert_eq!(second.total, 3);
        assert_eq!(second.records.len(), 1);
        assert_eq!(second.records[0].id, "old");
    }

    #[test]
    fn migrates_and_updates_stream_status() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("requests.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE requests (
                    id TEXT PRIMARY KEY,
                    started_at TEXT NOT NULL,
                    model TEXT NOT NULL DEFAULT '',
                    provider_id TEXT,
                    provider_name TEXT,
                    provider_protocol TEXT,
                    status INTEGER NOT NULL DEFAULT 0,
                    latency_ms INTEGER NOT NULL DEFAULT 0,
                    streaming INTEGER NOT NULL DEFAULT 0,
                    error TEXT,
                    reasoning_effort TEXT,
                    input_tokens INTEGER NOT NULL DEFAULT 0,
                    output_tokens INTEGER NOT NULL DEFAULT 0,
                    cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                    cache_write_tokens INTEGER NOT NULL DEFAULT 0,
                    total_tokens INTEGER NOT NULL DEFAULT 0
                )",
            )
            .unwrap();
        }
        let log = RequestLog::open(&path).unwrap();
        log.insert(&record("stream", 1));
        log.update_stream_status(
            "stream",
            "interrupted",
            Some("network error"),
            Some("response.output_text.delta"),
        );

        let records = log.recent(1);
        assert_eq!(records[0].stream_state.as_deref(), Some("interrupted"));
        assert_eq!(records[0].route_reason.as_deref(), Some("direct"));
        assert_eq!(records[0].stream_error.as_deref(), Some("network error"));
        assert_eq!(
            records[0].last_event.as_deref(),
            Some("response.output_text.delta")
        );
    }

    #[test]
    fn migrates_stream_bytes_with_default_zero() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("requests.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE requests (
                    id TEXT PRIMARY KEY,
                    started_at TEXT NOT NULL,
                    model TEXT NOT NULL DEFAULT '',
                    provider_id TEXT,
                    provider_name TEXT,
                    provider_protocol TEXT,
                    status INTEGER NOT NULL DEFAULT 0,
                    latency_ms INTEGER NOT NULL DEFAULT 0,
                    streaming INTEGER NOT NULL DEFAULT 0,
                    error TEXT,
                    reasoning_effort TEXT,
                    stream_state TEXT,
                    stream_error TEXT,
                    last_event TEXT,
                    input_tokens INTEGER NOT NULL DEFAULT 0,
                    output_tokens INTEGER NOT NULL DEFAULT 0,
                    cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                    cache_write_tokens INTEGER NOT NULL DEFAULT 0,
                    total_tokens INTEGER NOT NULL DEFAULT 0
                );
                INSERT INTO requests
                    (id, started_at, model, status, latency_ms, streaming)
                VALUES
                    ('old', '2026-01-01T00:00:00Z', 'gpt-5.5', 200, 10, 1);",
            )
            .unwrap();
        }

        let log = RequestLog::open(&path).unwrap();
        let records = log.recent(1);

        assert_eq!(records[0].id, "old");
        assert_eq!(records[0].stream_bytes, 0);
    }

    #[test]
    fn migrates_route_reason_with_default_none() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("requests.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE requests (
                    id TEXT PRIMARY KEY,
                    started_at TEXT NOT NULL,
                    model TEXT NOT NULL DEFAULT '',
                    requested_model TEXT,
                    provider_id TEXT,
                    provider_name TEXT,
                    provider_protocol TEXT,
                    status INTEGER NOT NULL DEFAULT 0,
                    latency_ms INTEGER NOT NULL DEFAULT 0,
                    streaming INTEGER NOT NULL DEFAULT 0,
                    error TEXT,
                    reasoning_effort TEXT,
                    stream_state TEXT,
                    stream_error TEXT,
                    last_event TEXT,
                    stream_bytes INTEGER NOT NULL DEFAULT 0,
                    input_tokens INTEGER NOT NULL DEFAULT 0,
                    output_tokens INTEGER NOT NULL DEFAULT 0,
                    cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                    cache_write_tokens INTEGER NOT NULL DEFAULT 0,
                    total_tokens INTEGER NOT NULL DEFAULT 0
                );
                INSERT INTO requests
                    (id, started_at, model, status, latency_ms, streaming)
                VALUES
                    ('old', '2026-01-01T00:00:00Z', 'gpt-5.5', 200, 10, 1);",
            )
            .unwrap();
        }

        let log = RequestLog::open(&path).unwrap();
        let records = log.recent(1);

        assert_eq!(records[0].id, "old");
        assert!(records[0].route_reason.is_none());
    }

    #[test]
    fn stream_progress_updates_bytes_and_usage_without_overwriting_state() {
        let temp = tempfile::tempdir().unwrap();
        let log = RequestLog::open(&temp.path().join("requests.db")).unwrap();
        log.insert(&record("stream", 1));
        log.update_stream_status(
            "stream",
            "interrupted",
            Some("network error"),
            Some("response.output_text.delta"),
        );
        let usage = TokenUsage {
            input_tokens: 20,
            output_tokens: 7,
            cache_read_tokens: 3,
            cache_write_tokens: 0,
            total_tokens: 30,
        };
        log.update_stream_progress("stream", 42_000, Some(&usage));

        let records = log.recent(1);
        assert_eq!(records[0].stream_state.as_deref(), Some("interrupted"));
        assert_eq!(records[0].stream_bytes, 42_000);
        assert_eq!(records[0].usage.total_tokens, 30);
    }

    #[test]
    fn provider_local_usage_is_scoped_and_estimates_known_costs() {
        let temp = tempfile::tempdir().unwrap();
        let log = RequestLog::open(&temp.path().join("requests.db")).unwrap();
        let mut first = record("a", 1);
        first.model = "gpt-5.5".into();
        first.provider_id = Some("openai-account".into());
        first.usage = TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            total_tokens: 2_000_000,
        };
        log.insert(&first);

        let mut second = record("b", 2);
        second.model = "unknown-model".into();
        second.provider_id = Some("openai-account".into());
        log.insert(&second);

        let mut third = record("c", 3);
        third.model = "gpt-5.5".into();
        third.provider_id = Some("other-provider".into());
        log.insert(&third);

        let usage = log
            .provider_local_usage()
            .into_iter()
            .find(|usage| usage.provider_id == "openai-account")
            .unwrap();

        assert_eq!(usage.requests, 2);
        assert_eq!(usage.total_tokens, 2_000_015);
        assert!((usage.estimated_cost_usd.unwrap() - 11.25).abs() < 0.0001);
        assert_eq!(usage.unknown_cost_models, vec!["unknown-model"]);
    }

    #[test]
    fn usage_snapshot_error_keeps_existing_quota() {
        let temp = tempfile::tempdir().unwrap();
        let log = RequestLog::open(&temp.path().join("requests.db")).unwrap();
        let quota = OpenAiAccountQuota {
            account_id: Some("acct".into()),
            ..OpenAiAccountQuota::default()
        };
        log.upsert_provider_usage_snapshot("openai-account", "passive", Some(&quota), None);
        log.upsert_provider_usage_snapshot(
            "openai-account",
            "unavailable",
            None,
            Some("network error"),
        );

        let snapshot = log
            .provider_usage_snapshots()
            .into_iter()
            .find(|snapshot| snapshot.provider_id == "openai-account")
            .unwrap();

        assert_eq!(snapshot.source, "unavailable");
        assert_eq!(snapshot.error.as_deref(), Some("network error"));
        assert_eq!(snapshot.quota.unwrap().account_id.as_deref(), Some("acct"));
    }
}
