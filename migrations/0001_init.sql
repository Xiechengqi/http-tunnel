CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    category TEXT NOT NULL,
    requires_restart BOOLEAN NOT NULL DEFAULT FALSE,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS schema_migrations (
    version TEXT PRIMARY KEY,
    applied_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS tunnels (
    id TEXT PRIMARY KEY,
    subdomain TEXT NOT NULL UNIQUE,
    token_hash TEXT NOT NULL,
    status TEXT NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    connected_at TIMESTAMP,
    disconnected_at TIMESTAMP,
    expires_at TIMESTAMP NOT NULL,
    client_ip TEXT,
    client_user_agent TEXT,
    access_policy TEXT NOT NULL DEFAULT 'public',
    access_token_hash TEXT,
    access_username TEXT,
    access_password_hash TEXT,
    allowed_methods TEXT,
    blocked_path_prefixes TEXT,
    inspector_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    rate_limit_per_minute INTEGER
);

CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    tunnel_id TEXT NOT NULL REFERENCES tunnels(id) ON DELETE CASCADE,
    connected_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    disconnected_at TIMESTAMP,
    disconnect_reason TEXT,
    last_seen_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    client_version TEXT,
    client_capabilities TEXT,
    remote_addr TEXT
);

CREATE TABLE IF NOT EXISTS request_logs (
    id TEXT PRIMARY KEY,
    tunnel_id TEXT NOT NULL REFERENCES tunnels(id) ON DELETE CASCADE,
    session_id TEXT REFERENCES sessions(id) ON DELETE SET NULL,
    request_type TEXT NOT NULL DEFAULT 'http',
    method TEXT NOT NULL,
    path TEXT NOT NULL,
    host TEXT,
    remote_ip TEXT,
    user_agent TEXT,
    status INTEGER,
    started_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    completed_at TIMESTAMP,
    duration_ms INTEGER,
    bytes_in INTEGER,
    bytes_out INTEGER,
    error TEXT,
    ws_message_count INTEGER,
    ws_close_code INTEGER,
    ws_close_reason TEXT,
    replay_of TEXT REFERENCES request_logs(id) ON DELETE SET NULL
);

CREATE TABLE IF NOT EXISTS request_inspections (
    request_log_id TEXT PRIMARY KEY REFERENCES request_logs(id) ON DELETE CASCADE,
    request_headers TEXT NOT NULL,
    request_content_type TEXT,
    request_body_preview BLOB NOT NULL DEFAULT x'',
    request_body_preview_encoding TEXT NOT NULL DEFAULT 'utf8',
    request_body_truncated BOOLEAN NOT NULL DEFAULT FALSE,
    response_status INTEGER,
    response_headers TEXT,
    response_content_type TEXT,
    response_body_preview BLOB NOT NULL DEFAULT x'',
    response_body_preview_encoding TEXT NOT NULL DEFAULT 'utf8',
    response_body_truncated BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS events (
    id TEXT PRIMARY KEY,
    tunnel_id TEXT REFERENCES tunnels(id) ON DELETE SET NULL,
    session_id TEXT REFERENCES sessions(id) ON DELETE SET NULL,
    kind TEXT NOT NULL,
    message TEXT,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS audit_logs (
    id TEXT PRIMARY KEY,
    actor TEXT,
    remote_ip TEXT,
    action TEXT NOT NULL,
    target_type TEXT,
    target_id TEXT,
    result TEXT NOT NULL,
    detail TEXT,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS admin_sessions (
    id TEXT PRIMARY KEY,
    token_hash TEXT NOT NULL UNIQUE,
    remote_ip TEXT,
    user_agent TEXT,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    expires_at TIMESTAMP NOT NULL,
    last_seen_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    revoked_at TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_request_logs_tunnel_started ON request_logs(tunnel_id, started_at);
CREATE INDEX IF NOT EXISTS idx_request_logs_error_started ON request_logs(error, started_at);
CREATE INDEX IF NOT EXISTS idx_request_logs_status_started ON request_logs(status, started_at);
CREATE INDEX IF NOT EXISTS idx_sessions_tunnel_connected ON sessions(tunnel_id, connected_at);
