export type ApiResponse<T> = {
  ok: boolean;
  data?: T;
  error?: {
    code: string;
    message: string;
  };
};

export type DashboardSummary = {
  ready: string;
  setup_required: boolean;
  generated_at_unix_seconds: number;
  server_url?: string | null;
  stats: DashboardStats;
  tunnels: PublicTunnel[];
  map_points: PublicTunnelMapPoint[];
};

export type DashboardStats = {
  total_tunnels: number;
  online_tunnels: number;
  offline_tunnels: number;
  active_sessions: number;
  active_streams: number;
  request_count: number;
  error_count: number;
  bytes_in: number;
  bytes_out: number;
  located_sources: number;
};

export type PublicTunnel = {
  subdomain: string;
  url: string;
  status: string;
  connected: boolean;
  active_sessions: number;
  active_streams: number;
  request_count: number;
  error_count: number;
  bytes_in: number;
  bytes_out: number;
  source: PublicTunnelSource;
  last_seen_at?: string | null;
  expires_at: string;
};

export type PublicTunnelSource = {
  label: string;
  country_code?: string | null;
  country?: string | null;
  region?: string | null;
  city?: string | null;
  latitude?: number | null;
  longitude?: number | null;
  located: boolean;
};

export type PublicTunnelMapPoint = {
  subdomain: string;
  status: string;
  label: string;
  latitude: number;
  longitude: number;
  active_sessions: number;
};

export type AdminStatus = {
  setup_required: boolean;
  pending_restart: boolean;
  active_sessions: number;
  request_count: number;
  error_count: number;
  uptime_seconds: number;
};

export type PageMeta = {
  total: number;
  limit: number;
  offset: number;
  hasMore: boolean;
};

export type PageResult<T> = {
  data: T[];
  meta: PageMeta;
};

export type Tunnel = {
  id: string;
  subdomain: string;
  status: string;
  enabled: boolean;
  created_at: string;
  expires_at: string;
  inspector_enabled: boolean;
  access_policy: string;
  rate_limit_per_minute?: number | null;
  allowed_methods?: string[];
  blocked_path_prefixes?: string[];
};

export type RequestLog = {
  id: string;
  tunnel_id?: string | null;
  tunnel_subdomain?: string | null;
  request_type?: string;
  method?: string | null;
  path?: string | null;
  host?: string | null;
  remote_ip?: string | null;
  status?: number | null;
  started_at?: string;
  duration_ms?: number | null;
  bytes_in?: number | null;
  bytes_out?: number | null;
  error?: string | null;
  ws_close_code?: number | null;
  ws_close_reason?: string | null;
};

export type EventLog = {
  id: string;
  kind: string;
  message?: string | null;
  created_at: string;
};

export type AuditLog = {
  id: string;
  action: string;
  target_type?: string | null;
  target_id?: string | null;
  actor?: string | null;
  result: string;
  remote_ip?: string | null;
  detail?: string | null;
  created_at: string;
};

export type AdminSession = {
  id: string;
  remote_ip?: string | null;
  user_agent?: string | null;
  created_at: string;
  expires_at: string;
  last_seen_at: string;
  revoked_at?: string | null;
  active: boolean;
  current: boolean;
};

export type Alert = {
  severity: string;
  code: string;
  message: string;
  count?: number | null;
};

export type MaintenanceStatus = {
  database_path?: string | null;
  database_size_bytes: number;
  wal_size_bytes: number;
  tunnel_count: number;
  session_count: number;
  request_log_count: number;
  event_count: number;
  audit_log_count: number;
  admin_session_count: number;
  active_runtime_sessions: number;
};

export type UpgradeStatus = {
  auto_upgrade_enabled: boolean;
  release_repo: string;
  effective_release_repo: string;
  release_tag: string;
  current_version: string;
  check_interval_seconds: number;
  idle_window_seconds: number;
  upgrade_in_progress: boolean;
  restart_methods: string[];
  restart_method_checks: { method: string; available: boolean; detail: string }[];
  last_checked_at?: string | null;
  last_result?: string | null;
  last_message?: string | null;
  latest_tag?: string | null;
  update_available?: boolean | null;
};

export type ConfigFieldSchema = {
  key: string;
  category: string;
  env: string;
  value_type: string;
  secret: boolean;
  required: boolean;
  restart_required: boolean;
  hot_reloadable: boolean;
  default: string;
  allowed_values: string[];
  min?: number | null;
  max?: number | null;
  description: string;
};

export type ServerConfig = Record<string, unknown>;

export type SetupStatus = {
  setup_required: boolean;
  has_admin_password: boolean;
  has_domain: boolean;
  has_public_scheme: boolean;
  has_database_url: boolean;
  database_url: string;
};
