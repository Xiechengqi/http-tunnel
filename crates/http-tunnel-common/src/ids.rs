use ulid::Ulid;

pub fn generate_tunnel_id() -> String {
    format!("tun_{}", Ulid::new().to_string().to_lowercase())
}

pub fn generate_session_id() -> String {
    format!("ses_{}", Ulid::new().to_string().to_lowercase())
}

pub fn generate_admin_session_id() -> String {
    format!("adm_{}", Ulid::new().to_string().to_lowercase())
}

pub fn generate_request_id() -> String {
    format!("req_{}", Ulid::new().to_string().to_lowercase())
}

pub fn generate_event_id() -> String {
    format!("evt_{}", Ulid::new().to_string().to_lowercase())
}
