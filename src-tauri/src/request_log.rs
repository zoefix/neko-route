use crate::types::{
    ClaudeContextPressureSample, ContextBridgeDiagnostics, DayTokens, HealthCell, ModelDaySeries,
    ModelTokens, OfficialAccountQuota, ProviderLocalUsage, ProviderProtocol, ProviderUsageStatus,
    RequestLogPage, RequestRecord, TokenStats, TokenTotals, TokenUsage,
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
                context_bridge_json TEXT,
                input_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                cache_write_tokens INTEGER NOT NULL DEFAULT 0,
                total_tokens INTEGER NOT NULL DEFAULT 0,
                ctx_input_tokens INTEGER NOT NULL DEFAULT 0,
                ctx_output_tokens INTEGER NOT NULL DEFAULT 0,
                ctx_cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                ctx_cache_write_tokens INTEGER NOT NULL DEFAULT 0,
                ctx_total_tokens INTEGER NOT NULL DEFAULT 0,
                cost_usd REAL,
                image_preview TEXT,
                upstream_model TEXT
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
            );
            CREATE TABLE IF NOT EXISTS claude_context_pressure (
                provider_id TEXT NOT NULL,
                model TEXT NOT NULL,
                context_key TEXT NOT NULL,
                input_tokens INTEGER NOT NULL DEFAULT 0,
                body_bytes INTEGER NOT NULL DEFAULT 0,
                requires_precompression INTEGER NOT NULL DEFAULT 0,
                context_full_body_bytes INTEGER NOT NULL DEFAULT 0,
                compression_stage TEXT,
                compaction_summary TEXT,
                compaction_updated_at TEXT,
                updated_at TEXT NOT NULL,
                PRIMARY KEY(provider_id, model, context_key)
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
        let _ = conn.execute(
            "ALTER TABLE requests ADD COLUMN context_bridge_json TEXT",
            [],
        );
        for column in [
            "ctx_input_tokens",
            "ctx_output_tokens",
            "ctx_cache_read_tokens",
            "ctx_cache_write_tokens",
            "ctx_total_tokens",
        ] {
            let _ = conn.execute(
                &format!("ALTER TABLE requests ADD COLUMN {column} INTEGER NOT NULL DEFAULT 0"),
                [],
            );
        }
        let _ = conn.execute("ALTER TABLE requests ADD COLUMN cost_usd REAL", []);
        let _ = conn.execute("ALTER TABLE requests ADD COLUMN image_preview TEXT", []);
        let _ = conn.execute("ALTER TABLE requests ADD COLUMN upstream_model TEXT", []);
        let _ = conn.execute(
            "ALTER TABLE claude_context_pressure ADD COLUMN requires_precompression INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE claude_context_pressure ADD COLUMN context_full_body_bytes INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE claude_context_pressure ADD COLUMN compression_stage TEXT",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE claude_context_pressure ADD COLUMN compaction_summary TEXT",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE claude_context_pressure ADD COLUMN compaction_updated_at TEXT",
            [],
        );
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_claude_context_pressure_updated_at ON claude_context_pressure(updated_at DESC)",
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
        let context_bridge_json = record
            .context_bridge
            .as_ref()
            .and_then(|diagnostics| serde_json::to_string(diagnostics).ok());
        // 模型 ID 现在是随机的(neko-model-xxx)，查不到市场定价；改用请求方原始模型名
        // (如 gpt-5.4)来估算消费，回退到 model。
        let pricing_model = record
            .upstream_model
            .as_deref()
            .filter(|model| !model.is_empty())
            .or_else(|| {
                record
                    .requested_model
                    .as_deref()
                    .filter(|model| !model.is_empty())
            })
            .unwrap_or(record.model.as_str());
        let cost_usd = record.cost_usd.or_else(|| {
            crate::pricing::estimate_model_cost_usd(
                pricing_model,
                record.usage.input_tokens,
                record.usage.output_tokens,
                record.usage.cache_read_tokens,
                record.usage.cache_write_tokens,
            )
        });
        let _ = conn.execute(
            "INSERT OR REPLACE INTO requests
              (id, started_at, model, requested_model, route_reason, provider_id, provider_name, provider_protocol,
               status, latency_ms, streaming, error, reasoning_effort,
               stream_state, stream_error, last_event,
               stream_bytes, context_bridge_json, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, total_tokens,
               ctx_input_tokens, ctx_output_tokens, ctx_cache_read_tokens, ctx_cache_write_tokens, ctx_total_tokens, cost_usd, image_preview, upstream_model)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29,?30,?31)",
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
                context_bridge_json,
                record.usage.input_tokens as i64,
                record.usage.output_tokens as i64,
                record.usage.cache_read_tokens as i64,
                record.usage.cache_write_tokens as i64,
                record.usage.total_tokens as i64,
                record.context_usage.input_tokens as i64,
                record.context_usage.output_tokens as i64,
                record.context_usage.cache_read_tokens as i64,
                record.context_usage.cache_write_tokens as i64,
                record.context_usage.total_tokens as i64,
                cost_usd,
                record.image_preview,
                record.upstream_model,
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

    /// 流式结束后写入「上下文体积」(清理前)，并按上游模型 + 已记录的消费 usage 重算 cost。
    pub fn finalize_request_breakdown(&self, id: &str, context_usage: &TokenUsage) {
        let conn = self.conn.lock().unwrap();
        let cost: Option<f64> = conn
            .query_row(
                "SELECT model, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, requested_model, upstream_model
                 FROM requests WHERE id=?1",
                params![id],
                |row| {
                    let model: String = row.get(0)?;
                    let requested: Option<String> = row.get(5)?;
                    let upstream: Option<String> = row.get(6)?;
                    let pricing_model = upstream
                        .as_deref()
                        .filter(|model| !model.is_empty())
                        .or_else(|| requested.as_deref().filter(|model| !model.is_empty()))
                        .unwrap_or(model.as_str());
                    Ok(crate::pricing::estimate_model_cost_usd(
                        pricing_model,
                        row.get::<_, i64>(1)? as u64,
                        row.get::<_, i64>(2)? as u64,
                        row.get::<_, i64>(3)? as u64,
                        row.get::<_, i64>(4)? as u64,
                    ))
                },
            )
            .unwrap_or(None);
        let _ = conn.execute(
            "UPDATE requests SET ctx_input_tokens=?2, ctx_output_tokens=?3,
                ctx_cache_read_tokens=?4, ctx_cache_write_tokens=?5, ctx_total_tokens=?6, cost_usd=?7
             WHERE id=?1",
            params![
                id,
                context_usage.input_tokens as i64,
                context_usage.output_tokens as i64,
                context_usage.cache_read_tokens as i64,
                context_usage.cache_write_tokens as i64,
                context_usage.total_tokens as i64,
                cost,
            ],
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

    /// 用当前配置把历史记录里缺失的 upstream_model 补上（旧记录写入时没这列，估价查不到价）。
    pub fn backfill_upstream_models(&self, mappings: &[(String, String)]) {
        let conn = self.conn.lock().unwrap();
        for (model_id, upstream) in mappings {
            if model_id.is_empty() || upstream.is_empty() {
                continue;
            }
            let _ = conn.execute(
                "UPDATE requests SET upstream_model=?1
                 WHERE model=?2 AND (upstream_model IS NULL OR upstream_model='')",
                params![upstream, model_id],
            );
        }
    }

    pub fn recent(&self, limit: usize) -> Vec<RequestRecord> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare(
            "SELECT id, started_at, model, provider_id, provider_name, provider_protocol,
                    requested_model, route_reason, status, latency_ms, streaming, error, reasoning_effort,
                    stream_state, stream_error, last_event,
                    stream_bytes, context_bridge_json, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, total_tokens,
                    ctx_input_tokens, ctx_output_tokens, ctx_cache_read_tokens, ctx_cache_write_tokens, ctx_total_tokens, cost_usd, image_preview, upstream_model
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

    /// 某模型最近 `limit` 条请求的健康格子（status/latency/stream_state），按时间倒序、最新在前。
    pub fn health_cells(&self, model: &str, limit: usize) -> Vec<HealthCell> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare(
            "SELECT status, latency_ms, stream_state FROM requests
             WHERE model = ?1 ORDER BY started_at DESC LIMIT ?2",
        ) {
            Ok(stmt) => stmt,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![model, limit as i64], |row| {
            Ok(HealthCell {
                status: row.get::<_, i64>(0)? as u16,
                latency_ms: row.get::<_, i64>(1)? as u64,
                stream_state: row.get::<_, Option<String>>(2)?,
            })
        })
        .and_then(|mapped| mapped.collect::<Result<Vec<_>, _>>())
        .unwrap_or_default()
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
                    stream_bytes, context_bridge_json, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, total_tokens,
                    ctx_input_tokens, ctx_output_tokens, ctx_cache_read_tokens, ctx_cache_write_tokens, ctx_total_tokens, cost_usd, image_preview, upstream_model
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

    pub fn upsert_claude_context_pressure(
        &self,
        provider_id: &str,
        model: &str,
        context_key: &str,
        input_tokens: u64,
        body_bytes: u64,
    ) {
        if provider_id.trim().is_empty()
            || model.trim().is_empty()
            || context_key.trim().is_empty()
            || input_tokens == 0
            || body_bytes == 0
        {
            return;
        }
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT INTO claude_context_pressure
                (provider_id, model, context_key, input_tokens, body_bytes, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(provider_id, model, context_key) DO UPDATE SET
                input_tokens=excluded.input_tokens,
                body_bytes=excluded.body_bytes,
                updated_at=excluded.updated_at",
            params![
                provider_id,
                model,
                context_key,
                input_tokens as i64,
                body_bytes as i64,
                Utc::now().to_rfc3339(),
            ],
        );
    }

    pub fn claude_context_pressure(
        &self,
        provider_id: &str,
        model: &str,
        context_key: &str,
    ) -> Option<ClaudeContextPressureSample> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT compaction_summary
             FROM claude_context_pressure
             WHERE provider_id=?1 AND model=?2 AND context_key=?3",
            params![provider_id, model, context_key],
            row_to_claude_context_pressure,
        )
        .ok()
    }

    pub fn upsert_claude_compaction(
        &self,
        provider_id: &str,
        model: &str,
        context_key: &str,
        summary: &str,
    ) {
        if provider_id.trim().is_empty()
            || model.trim().is_empty()
            || context_key.trim().is_empty()
            || summary.trim().is_empty()
        {
            return;
        }
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT INTO claude_context_pressure
                (provider_id, model, context_key, input_tokens, body_bytes, compaction_summary, compaction_updated_at, updated_at)
             VALUES (?1, ?2, ?3, 0, 0, ?4, ?5, ?5)
             ON CONFLICT(provider_id, model, context_key) DO UPDATE SET
                compaction_summary=excluded.compaction_summary,
                compaction_updated_at=excluded.compaction_updated_at",
            params![provider_id, model, context_key, summary, now],
        );
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
            let cost = cost_between(&conn, &start, &end);
            series.push(DayTokens {
                date: day.format("%Y-%m-%d").to_string(),
                total_tokens: t.total_tokens,
                input_tokens: t.input_tokens,
                output_tokens: t.output_tokens,
                cache_read_tokens: t.cache_read_tokens,
                cache_write_tokens: t.cache_write_tokens,
                requests: t.requests,
                cost_usd: cost,
            });
        }

        let by_model = model_totals(&conn);
        let model_trends = model_day_trends(&conn, today);

        TokenStats {
            today: today_totals,
            yesterday: yesterday_totals,
            last7,
            all_time,
            series,
            by_model,
            model_trends,
        }
    }

    pub fn provider_local_usage(&self) -> Vec<ProviderLocalUsage> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare(
            "SELECT COALESCE(provider_id, ''), COALESCE(NULLIF(upstream_model, ''), NULLIF(requested_model, ''), model),
                    SUM(input_tokens), SUM(output_tokens), SUM(cache_read_tokens),
                    SUM(cache_write_tokens), SUM(total_tokens), COUNT(*)
             FROM requests
             WHERE provider_id IS NOT NULL AND provider_id != ''
             GROUP BY provider_id, COALESCE(NULLIF(upstream_model, ''), NULLIF(requested_model, ''), model)",
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
                .and_then(|value| serde_json::from_str::<OfficialAccountQuota>(&value).ok());
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
        quota: Option<&OfficialAccountQuota>,
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

/// 某时间段内的估算消费：按上游模型分组聚合 token 再按市场定价求和。
fn cost_between(conn: &Connection, start: &str, end: &str) -> f64 {
    let mut stmt = match conn.prepare(
        "SELECT COALESCE(NULLIF(upstream_model, ''), NULLIF(requested_model, ''), model),
                COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0),
                COALESCE(SUM(cache_read_tokens),0), COALESCE(SUM(cache_write_tokens),0)
         FROM requests WHERE started_at >= ?1 AND started_at < ?2
         GROUP BY COALESCE(NULLIF(upstream_model, ''), NULLIF(requested_model, ''), model)",
    ) {
        Ok(s) => s,
        Err(_) => return 0.0,
    };
    let rows = stmt.query_map(params![start, end], |row| {
        let model: String = row.get(0)?;
        let input = row.get::<_, i64>(1)? as u64;
        let output = row.get::<_, i64>(2)? as u64;
        let cache_read = row.get::<_, i64>(3)? as u64;
        let cache_write = row.get::<_, i64>(4)? as u64;
        Ok(
            crate::pricing::estimate_model_cost_usd(&model, input, output, cache_read, cache_write)
                .unwrap_or(0.0),
        )
    });
    match rows {
        Ok(iter) => iter.filter_map(Result::ok).sum(),
        Err(_) => 0.0,
    }
}

/// 每个模型最近 7 天的每日总 token（旧到新，与 series 对齐），按累计量降序。
fn model_day_trends(conn: &Connection, today: chrono::NaiveDate) -> Vec<ModelDaySeries> {
    let mut map: HashMap<String, Vec<u64>> = HashMap::new();
    for i in 0..7usize {
        let day = today - chrono::Duration::days(6 - i as i64);
        let start = day_start_rfc3339(day);
        let end = day_start_rfc3339(day + chrono::Duration::days(1));
        let mut stmt = match conn.prepare(
            "SELECT COALESCE(NULLIF(upstream_model, ''), NULLIF(requested_model, ''), model),
                    COALESCE(SUM(total_tokens), 0)
             FROM requests
             WHERE started_at >= ?1 AND started_at < ?2 AND total_tokens > 0
             GROUP BY COALESCE(NULLIF(upstream_model, ''), NULLIF(requested_model, ''), model)",
        ) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let rows = stmt.query_map(params![start, end], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
        });
        if let Ok(iter) = rows {
            for entry in iter.flatten() {
                map.entry(entry.0).or_insert_with(|| vec![0u64; 7])[i] = entry.1;
            }
        }
    }
    let mut out: Vec<ModelDaySeries> = map
        .into_iter()
        .map(|(model, daily)| ModelDaySeries { model, daily })
        .filter(|s| s.daily.iter().sum::<u64>() > 0)
        .collect();
    out.sort_by(|a, b| {
        b.daily
            .iter()
            .sum::<u64>()
            .cmp(&a.daily.iter().sum::<u64>())
    });
    out
}

fn model_totals(conn: &Connection) -> Vec<ModelTokens> {
    let mut stmt = match conn.prepare(
        "SELECT COALESCE(NULLIF(upstream_model, ''), NULLIF(requested_model, ''), model),
                COALESCE(SUM(total_tokens),0), COALESCE(SUM(input_tokens),0),
                COALESCE(SUM(output_tokens),0), COALESCE(SUM(cache_read_tokens),0),
                COALESCE(SUM(cache_write_tokens),0), COUNT(*)
         FROM requests WHERE model <> ''
         GROUP BY COALESCE(NULLIF(upstream_model, ''), NULLIF(requested_model, ''), model) ORDER BY SUM(total_tokens) DESC",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map([], |row| {
        let model: String = row.get(0)?;
        let total_tokens = row.get::<_, i64>(1)? as u64;
        let input_tokens = row.get::<_, i64>(2)? as u64;
        let output_tokens = row.get::<_, i64>(3)? as u64;
        let cache_read_tokens = row.get::<_, i64>(4)? as u64;
        let cache_write_tokens = row.get::<_, i64>(5)? as u64;
        let requests = row.get::<_, i64>(6)? as u64;
        let cost_usd = crate::pricing::estimate_model_cost_usd(
            &model,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
        )
        .unwrap_or(0.0);
        Ok(ModelTokens {
            model,
            total_tokens,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            requests,
            cost_usd,
        })
    })
    .and_then(|m| m.collect())
    .unwrap_or_default()
}

fn row_to_record(row: &rusqlite::Row) -> rusqlite::Result<RequestRecord> {
    let started: String = row.get(1)?;
    let protocol: Option<String> = row.get(5)?;
    let context_bridge_json: Option<String> = row.get(17)?;
    let model: String = row.get(2)?;
    let requested_model: Option<String> = row.get(6)?;
    let upstream_model: Option<String> = row.get(30)?;
    let usage = TokenUsage {
        input_tokens: row.get::<_, i64>(18)? as u64,
        output_tokens: row.get::<_, i64>(19)? as u64,
        cache_read_tokens: row.get::<_, i64>(20)? as u64,
        cache_write_tokens: row.get::<_, i64>(21)? as u64,
        total_tokens: row.get::<_, i64>(22)? as u64,
    };
    // cost 为空(旧记录、或随机 model id 写入时算不出)时，查询时用请求方原始模型实时估算定价。
    let cost_usd = row
        .get::<_, Option<f64>>(28)?
        .filter(|cost| *cost > 0.0)
        .or_else(|| {
            let pricing_model = upstream_model
                .as_deref()
                .filter(|value| !value.is_empty())
                .or_else(|| requested_model.as_deref().filter(|value| !value.is_empty()))
                .unwrap_or(model.as_str());
            crate::pricing::estimate_model_cost_usd(
                pricing_model,
                usage.input_tokens,
                usage.output_tokens,
                usage.cache_read_tokens,
                usage.cache_write_tokens,
            )
        });
    Ok(RequestRecord {
        id: row.get(0)?,
        started_at: DateTime::parse_from_rfc3339(&started)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
        model,
        requested_model,
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
        context_bridge: context_bridge_json
            .as_deref()
            .and_then(|value| serde_json::from_str::<ContextBridgeDiagnostics>(value).ok()),
        usage,
        context_usage: TokenUsage {
            input_tokens: row.get::<_, i64>(23)? as u64,
            output_tokens: row.get::<_, i64>(24)? as u64,
            cache_read_tokens: row.get::<_, i64>(25)? as u64,
            cache_write_tokens: row.get::<_, i64>(26)? as u64,
            total_tokens: row.get::<_, i64>(27)? as u64,
        },
        upstream_model,
        cost_usd,
        image_preview: row.get::<_, Option<String>>(29)?,
    })
}

fn row_to_claude_context_pressure(
    row: &rusqlite::Row,
) -> rusqlite::Result<ClaudeContextPressureSample> {
    Ok(ClaudeContextPressureSample {
        compaction_summary: row.get::<_, Option<String>>(0).unwrap_or_default(),
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
        ProviderProtocol::OpenAiImages => "open_ai_images",
        ProviderProtocol::GeminiImage => "gemini_image",
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
            upstream_model: None,
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
            context_bridge: None,
            usage: TokenUsage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                total_tokens: 15,
            },
            context_usage: TokenUsage::default(),
            cost_usd: None,
            image_preview: None,
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
    fn estimates_cost_from_upstream_model_when_id_is_random() {
        let temp = tempfile::tempdir().unwrap();
        let log = RequestLog::open(&temp.path().join("requests.db")).unwrap();
        // 真实直连场景：model 是随机路由 id、requested_model 为空，只有 upstream_model 能查到价。
        let mut rec = record("r1", 0);
        rec.model = "neko-model-87ec675d".into();
        rec.requested_model = None;
        rec.upstream_model = Some("gpt-5.5".into());
        rec.usage = TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            total_tokens: 2_000_000,
        };
        log.insert(&rec);
        // gpt-5.5: 1M input @ $1.25 + 1M output @ $10 = $11.25。
        assert!((log.recent(1)[0].cost_usd.unwrap() - 11.25).abs() < 0.0001);
    }

    #[test]
    fn backfill_fills_upstream_and_enables_pricing() {
        let temp = tempfile::tempdir().unwrap();
        let log = RequestLog::open(&temp.path().join("requests.db")).unwrap();
        // 旧记录：没有 upstream_model，随机 id 查不到价。
        let mut rec = record("r1", 0);
        rec.model = "neko-model-abc".into();
        rec.requested_model = None;
        rec.upstream_model = None;
        rec.usage = TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            total_tokens: 2_000_000,
        };
        log.insert(&rec);
        assert!(log.recent(1)[0].cost_usd.is_none());
        // 用当前配置补全上游后即可估价。
        log.backfill_upstream_models(&[("neko-model-abc".into(), "gpt-5.5".into())]);
        assert!((log.recent(1)[0].cost_usd.unwrap() - 11.25).abs() < 0.0001);
    }

    #[test]
    fn finalize_breakdown_writes_volume_and_recomputes_cost() {
        let temp = tempfile::tempdir().unwrap();
        let log = RequestLog::open(&temp.path().join("requests.db")).unwrap();
        let mut rec = record("r1", 0);
        rec.model = "claude-opus-4-8".into();
        rec.usage = TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            total_tokens: 1_000_000,
        };
        log.insert(&rec);
        // 体积(清理前)远大于消费；cost 按消费 1M input @ $15 重算。
        let ctx = TokenUsage {
            input_tokens: 2_000_000,
            output_tokens: 100,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            total_tokens: 2_000_100,
        };
        log.finalize_request_breakdown("r1", &ctx);
        let got = log.recent(1).remove(0);
        assert_eq!(got.context_usage.input_tokens, 2_000_000);
        assert_eq!(got.usage.input_tokens, 1_000_000);
        assert!((got.cost_usd.unwrap() - 15.0).abs() < 0.0001);
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
        let quota = OfficialAccountQuota {
            account_id: Some("acct".into()),
            ..OfficialAccountQuota::default()
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

    #[test]
    fn claude_compaction_summary_roundtrip() {
        let temp = tempfile::tempdir().unwrap();
        let log = RequestLog::open(&temp.path().join("requests.db")).unwrap();

        log.upsert_claude_compaction("provider", "claude", "key:abc", "first summary");
        let sample = log
            .claude_context_pressure("provider", "claude", "key:abc")
            .unwrap();
        assert_eq!(sample.compaction_summary.as_deref(), Some("first summary"));

        log.upsert_claude_compaction("provider", "claude", "key:abc", "second summary");
        let sample = log
            .claude_context_pressure("provider", "claude", "key:abc")
            .unwrap();
        assert_eq!(sample.compaction_summary.as_deref(), Some("second summary"));

        // pressure 写入不应清空已存的 compaction 摘要。
        log.upsert_claude_context_pressure("provider", "claude", "key:abc", 100, 200);
        let sample = log
            .claude_context_pressure("provider", "claude", "key:abc")
            .unwrap();
        assert_eq!(sample.compaction_summary.as_deref(), Some("second summary"));
    }
}
