use http_tunnel_common::ServerConfig;
use http_tunnel_protocol::Frame;
use sqlx::{Row, SqlitePool};
use std::{
    collections::{HashMap, VecDeque},
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{broadcast, mpsc, Mutex, RwLock};

#[derive(Clone)]
pub struct AppState {
    pub config_path: String,
    pub config: Arc<RwLock<ServerConfig>>,
    pub pool: SqlitePool,
    pub admin_tokens: Arc<RwLock<HashMap<String, SystemTime>>>,
    pub sessions_by_subdomain: Arc<RwLock<HashMap<String, SessionPool>>>,
    pub pending_streams: Arc<RwLock<HashMap<u64, PendingStream>>>,
    pub tunnel_create_hits: Arc<RwLock<HashMap<String, VecDeque<Instant>>>>,
    pub admin_login_hits: Arc<RwLock<HashMap<String, VecDeque<Instant>>>>,
    pub per_tunnel_hits: Arc<RwLock<HashMap<String, VecDeque<Instant>>>>,
    pub tunnel_traffic: Arc<RwLock<HashMap<String, TunnelTrafficCounters>>>,
    pub upgrade_events: broadcast::Sender<String>,
    pub upgrade_lock: Arc<Mutex<()>>,
    pub last_proxy_activity_unix_ms: Arc<AtomicU64>,
    pub started_at: SystemTime,
    next_stream_id: Arc<AtomicU64>,
}

#[derive(Debug, Clone)]
pub struct ActiveSession {
    pub tunnel_id: String,
    pub session_id: String,
    pub tx: mpsc::Sender<Frame>,
    pub last_seen: Arc<RwLock<Instant>>,
    pub metrics: SessionRuntimeMetrics,
    pub tunnel_traffic: TunnelTrafficCounters,
}

impl ActiveSession {
    pub fn mark_selected(&self) {
        self.metrics.selected_count.fetch_add(1, Ordering::Relaxed);
        self.metrics
            .last_selected_unix_ms
            .store(unix_now_millis(), Ordering::Relaxed);
    }

    pub fn runtime_metrics(&self) -> RuntimeSessionMetricsSnapshot {
        self.metrics.snapshot()
    }
}

#[derive(Debug, Clone, Default)]
pub struct SessionRuntimeMetrics {
    active_streams: Arc<AtomicUsize>,
    bytes_in: Arc<AtomicU64>,
    bytes_out: Arc<AtomicU64>,
    selected_count: Arc<AtomicU64>,
    last_selected_unix_ms: Arc<AtomicU64>,
}

impl SessionRuntimeMetrics {
    pub fn stream_started(&self) {
        self.active_streams.fetch_add(1, Ordering::Relaxed);
    }

    pub fn stream_finished(&self) {
        let _ = self
            .active_streams
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                Some(value.saturating_sub(1))
            });
    }

    pub fn add_bytes_in(&self, bytes: usize) {
        self.bytes_in
            .fetch_add(u64::try_from(bytes).unwrap_or(u64::MAX), Ordering::Relaxed);
    }

    pub fn add_bytes_out(&self, bytes: usize) {
        self.bytes_out
            .fetch_add(u64::try_from(bytes).unwrap_or(u64::MAX), Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> RuntimeSessionMetricsSnapshot {
        RuntimeSessionMetricsSnapshot {
            active_streams: self.active_streams.load(Ordering::Relaxed),
            bytes_in: self.bytes_in.load(Ordering::Relaxed),
            bytes_out: self.bytes_out.load(Ordering::Relaxed),
            selected_count: self.selected_count.load(Ordering::Relaxed),
            last_selected_unix_ms: match self.last_selected_unix_ms.load(Ordering::Relaxed) {
                0 => None,
                value => Some(value),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RuntimeSessionMetricsSnapshot {
    pub active_streams: usize,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub selected_count: u64,
    pub last_selected_unix_ms: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct TunnelTrafficCounters {
    bytes_in: Arc<AtomicU64>,
    bytes_out: Arc<AtomicU64>,
}

impl TunnelTrafficCounters {
    pub fn add_bytes_in(&self, bytes: usize) {
        self.add_bytes_in_u64(u64::try_from(bytes).unwrap_or(u64::MAX));
    }

    pub fn add_bytes_out(&self, bytes: usize) {
        self.add_bytes_out_u64(u64::try_from(bytes).unwrap_or(u64::MAX));
    }

    pub fn snapshot(&self) -> TunnelTrafficSnapshot {
        TunnelTrafficSnapshot {
            bytes_in: self.bytes_in.load(Ordering::Relaxed),
            bytes_out: self.bytes_out.load(Ordering::Relaxed),
        }
    }

    fn set_at_least(&self, snapshot: TunnelTrafficSnapshot) {
        update_atomic_max(&self.bytes_in, snapshot.bytes_in);
        update_atomic_max(&self.bytes_out, snapshot.bytes_out);
    }

    fn add_bytes_in_u64(&self, bytes: u64) {
        saturating_add_atomic(&self.bytes_in, bytes);
    }

    fn add_bytes_out_u64(&self, bytes: u64) {
        saturating_add_atomic(&self.bytes_out, bytes);
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TunnelTrafficSnapshot {
    pub bytes_in: u64,
    pub bytes_out: u64,
}

#[derive(Debug, Clone, Default)]
pub struct SessionPool {
    pub sessions: Vec<ActiveSession>,
    pub next_index: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPoolPolicy {
    SingleReplace,
    SingleReject,
    RoundRobin,
    LeastLoaded,
}

#[derive(Debug, Default)]
pub struct RegisterSessionResult {
    pub rejected: bool,
    pub replaced: Vec<ActiveSession>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingStreamType {
    Http,
    WebSocket,
}

#[derive(Debug, Clone)]
pub struct PendingStream {
    pub tunnel_id: String,
    pub session_id: String,
    pub stream_type: PendingStreamType,
    pub tx: mpsc::Sender<Frame>,
    pub session_metrics: SessionRuntimeMetrics,
}

impl AppState {
    pub fn new(config_path: String, config: ServerConfig, pool: SqlitePool) -> Self {
        let (upgrade_events, _) = broadcast::channel(128);
        Self {
            config_path,
            config: Arc::new(RwLock::new(config)),
            pool,
            admin_tokens: Arc::new(RwLock::new(HashMap::new())),
            sessions_by_subdomain: Arc::new(RwLock::new(HashMap::new())),
            pending_streams: Arc::new(RwLock::new(HashMap::new())),
            tunnel_create_hits: Arc::new(RwLock::new(HashMap::new())),
            admin_login_hits: Arc::new(RwLock::new(HashMap::new())),
            per_tunnel_hits: Arc::new(RwLock::new(HashMap::new())),
            tunnel_traffic: Arc::new(RwLock::new(HashMap::new())),
            upgrade_events,
            upgrade_lock: Arc::new(Mutex::new(())),
            last_proxy_activity_unix_ms: Arc::new(AtomicU64::new(unix_now_millis())),
            started_at: SystemTime::now(),
            next_stream_id: Arc::new(AtomicU64::new(1)),
        }
    }

    pub fn next_stream_id(&self) -> u64 {
        self.next_stream_id.fetch_add(1, Ordering::Relaxed)
    }

    pub async fn initialize_tunnel_traffic_from_request_logs(&self) -> Result<(), sqlx::Error> {
        let rows = sqlx::query(
            "SELECT tunnel_id, \
                    COALESCE(SUM(COALESCE(bytes_in, 0)), 0) AS bytes_in, \
                    COALESCE(SUM(COALESCE(bytes_out, 0)), 0) AS bytes_out \
             FROM request_logs GROUP BY tunnel_id",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut traffic = self.tunnel_traffic.write().await;
        for row in rows {
            let tunnel_id = row.get::<String, _>("tunnel_id");
            let counters = traffic.entry(tunnel_id).or_default().clone();
            counters.set_at_least(TunnelTrafficSnapshot {
                bytes_in: non_negative_u64(row.get::<i64, _>("bytes_in")),
                bytes_out: non_negative_u64(row.get::<i64, _>("bytes_out")),
            });
        }
        Ok(())
    }

    pub async fn tunnel_traffic_counters(&self, tunnel_id: &str) -> TunnelTrafficCounters {
        if let Some(counters) = self.tunnel_traffic.read().await.get(tunnel_id).cloned() {
            return counters;
        }
        self.tunnel_traffic
            .write()
            .await
            .entry(tunnel_id.to_string())
            .or_default()
            .clone()
    }

    pub async fn tunnel_traffic_snapshots(&self) -> HashMap<String, TunnelTrafficSnapshot> {
        self.tunnel_traffic
            .read()
            .await
            .iter()
            .map(|(tunnel_id, counters)| (tunnel_id.clone(), counters.snapshot()))
            .collect()
    }

    pub async fn register_session(
        &self,
        subdomain: &str,
        session: ActiveSession,
        policy: SessionPoolPolicy,
    ) -> RegisterSessionResult {
        let mut sessions = self.sessions_by_subdomain.write().await;
        let pool = sessions.entry(subdomain.to_string()).or_default();
        pool.sessions.retain(|current| !current.tx.is_closed());

        match policy {
            SessionPoolPolicy::SingleReject => {
                if pool
                    .sessions
                    .iter()
                    .any(|current| current.session_id != session.session_id)
                {
                    return RegisterSessionResult {
                        rejected: true,
                        replaced: Vec::new(),
                    };
                }
                pool.sessions.clear();
                pool.sessions.push(session);
                pool.next_index = 0;
            }
            SessionPoolPolicy::SingleReplace => {
                let replaced = pool
                    .sessions
                    .iter()
                    .filter(|current| current.session_id != session.session_id)
                    .cloned()
                    .collect::<Vec<_>>();
                pool.sessions.clear();
                pool.sessions.push(session);
                pool.next_index = 0;
                return RegisterSessionResult {
                    rejected: false,
                    replaced,
                };
            }
            SessionPoolPolicy::RoundRobin | SessionPoolPolicy::LeastLoaded => {
                pool.sessions
                    .retain(|current| current.session_id != session.session_id);
                pool.sessions.push(session);
                if pool.next_index >= pool.sessions.len() {
                    pool.next_index = 0;
                }
            }
        }

        RegisterSessionResult {
            rejected: false,
            replaced: Vec::new(),
        }
    }

    pub async fn select_session(
        &self,
        subdomain: &str,
        tunnel_id: &str,
        policy: SessionPoolPolicy,
    ) -> Option<ActiveSession> {
        let mut sessions = self.sessions_by_subdomain.write().await;
        let pool = sessions.get_mut(subdomain)?;
        pool.sessions
            .retain(|session| session.tunnel_id == tunnel_id && !session.tx.is_closed());
        if pool.sessions.is_empty() {
            sessions.remove(subdomain);
            return None;
        }
        let index = match policy {
            SessionPoolPolicy::RoundRobin => {
                let index = pool.next_index % pool.sessions.len();
                pool.next_index = (index + 1) % pool.sessions.len();
                index
            }
            SessionPoolPolicy::LeastLoaded => pool
                .sessions
                .iter()
                .enumerate()
                .min_by_key(|(_, session)| {
                    let metrics = session.runtime_metrics();
                    (
                        metrics.active_streams,
                        metrics.selected_count,
                        metrics.last_selected_unix_ms.unwrap_or(0),
                    )
                })
                .map(|(index, _)| index)
                .unwrap_or(0),
            SessionPoolPolicy::SingleReplace | SessionPoolPolicy::SingleReject => 0,
        };
        let selected = pool.sessions.get(index).cloned();
        if let Some(session) = selected.as_ref() {
            session.mark_selected();
            self.mark_proxy_activity();
        }
        selected
    }

    pub async fn insert_pending_stream(&self, stream_id: u64, stream: PendingStream) {
        self.mark_proxy_activity();
        stream.session_metrics.stream_started();
        self.pending_streams.write().await.insert(stream_id, stream);
    }

    pub async fn remove_pending_stream(&self, stream_id: u64) -> Option<PendingStream> {
        let stream = self.pending_streams.write().await.remove(&stream_id);
        if let Some(stream) = stream.as_ref() {
            stream.session_metrics.stream_finished();
            self.mark_proxy_activity();
        }
        stream
    }

    pub async fn remove_pending_streams_for_session(
        &self,
        tunnel_id: &str,
        session_id: &str,
    ) -> Vec<(u64, PendingStream)> {
        let mut pending = self.pending_streams.write().await;
        let stream_ids = pending
            .iter()
            .filter(|(_, stream)| stream.tunnel_id == tunnel_id && stream.session_id == session_id)
            .map(|(stream_id, _)| *stream_id)
            .collect::<Vec<_>>();
        let mut removed = Vec::new();
        for stream_id in stream_ids {
            if let Some(stream) = pending.remove(&stream_id) {
                stream.session_metrics.stream_finished();
                removed.push((stream_id, stream));
            }
        }
        if !removed.is_empty() {
            self.mark_proxy_activity();
        }
        removed
    }

    pub fn mark_proxy_activity(&self) {
        self.last_proxy_activity_unix_ms
            .store(unix_now_millis(), Ordering::Relaxed);
    }

    pub async fn proxy_idle_for(&self, duration: Duration) -> bool {
        if !self.pending_streams.read().await.is_empty() {
            return false;
        }
        let last = self.last_proxy_activity_unix_ms.load(Ordering::Relaxed);
        unix_now_millis().saturating_sub(last)
            >= u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
    }

    pub async fn pending_stream_count_for_session(
        &self,
        tunnel_id: &str,
        session_id: &str,
    ) -> usize {
        self.pending_streams
            .read()
            .await
            .values()
            .filter(|stream| stream.tunnel_id == tunnel_id && stream.session_id == session_id)
            .count()
    }

    pub async fn remove_session(&self, subdomain: &str, session_id: &str) -> Option<ActiveSession> {
        let mut sessions = self.sessions_by_subdomain.write().await;
        let pool = sessions.get_mut(subdomain)?;
        let position = pool
            .sessions
            .iter()
            .position(|session| session.session_id == session_id)?;
        let session = pool.sessions.remove(position);
        if pool.next_index > position {
            pool.next_index -= 1;
        }
        if pool.sessions.is_empty() {
            sessions.remove(subdomain);
        }
        Some(session)
    }

    pub async fn remove_sessions_for_tunnel(&self, tunnel_id: &str) -> Vec<ActiveSession> {
        let mut sessions = self.sessions_by_subdomain.write().await;
        let mut removed = Vec::new();
        let keys = sessions.keys().cloned().collect::<Vec<_>>();
        for key in keys {
            let Some(pool) = sessions.get_mut(&key) else {
                continue;
            };
            let mut index = 0;
            while index < pool.sessions.len() {
                if pool.sessions[index].tunnel_id == tunnel_id {
                    removed.push(pool.sessions.remove(index));
                } else {
                    index += 1;
                }
            }
            if pool.next_index >= pool.sessions.len() {
                pool.next_index = 0;
            }
            if pool.sessions.is_empty() {
                sessions.remove(&key);
            }
        }
        removed
    }

    pub async fn sessions_for_tunnel(&self, tunnel_id: &str) -> Vec<ActiveSession> {
        self.sessions_by_subdomain
            .read()
            .await
            .values()
            .flat_map(|pool| pool.sessions.iter())
            .filter(|session| session.tunnel_id == tunnel_id && !session.tx.is_closed())
            .cloned()
            .collect()
    }

    pub async fn active_session_count(&self) -> usize {
        self.sessions_by_subdomain
            .read()
            .await
            .values()
            .map(|pool| {
                pool.sessions
                    .iter()
                    .filter(|session| !session.tx.is_closed())
                    .count()
            })
            .sum()
    }

    pub async fn active_tunnel_ids(&self) -> Vec<String> {
        self.sessions_by_subdomain
            .read()
            .await
            .values()
            .flat_map(|pool| pool.sessions.iter())
            .filter(|session| !session.tx.is_closed())
            .map(|session| session.tunnel_id.clone())
            .collect()
    }

    pub async fn has_active_tunnel_session(&self, tunnel_id: &str) -> bool {
        self.sessions_by_subdomain
            .read()
            .await
            .values()
            .flat_map(|pool| pool.sessions.iter())
            .any(|session| session.tunnel_id == tunnel_id && !session.tx.is_closed())
    }
}

pub fn effective_session_pool_policy(config: &ServerConfig) -> SessionPoolPolicy {
    match config.session_pool_policy.as_str() {
        "single_reject" => SessionPoolPolicy::SingleReject,
        "round_robin" => SessionPoolPolicy::RoundRobin,
        "least_loaded" => SessionPoolPolicy::LeastLoaded,
        "single_replace" if config.duplicate_session_policy == "reject" => {
            SessionPoolPolicy::SingleReject
        }
        _ => SessionPoolPolicy::SingleReplace,
    }
}

fn unix_now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

fn saturating_add_atomic(value: &AtomicU64, amount: u64) {
    let _ = value.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_add(amount))
    });
}

fn update_atomic_max(value: &AtomicU64, minimum: u64) {
    let _ = value.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        (current < minimum).then_some(minimum)
    });
}

fn non_negative_u64(value: i64) -> u64 {
    u64::try_from(value.max(0)).unwrap_or_default()
}
