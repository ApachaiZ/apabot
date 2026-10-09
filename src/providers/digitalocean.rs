//! Provider DigitalOcean (droplets, API v2, Bearer token).
//!
//!   GET  /v2/droplets/{id}          → `{droplet: {status, name, …}}`
//!   POST /v2/droplets/{id}/actions  → `{"type": "power_on"|"power_off"|"reboot"}`
//!
//! Statuts : `active` → running, `off` → stopped, `new` → starting, tout le
//! reste (`archive`…) → unknown. Pas de métriques temps réel : champs `None`.

use serde_json::{json, Value};

use super::{map_state, str_at, Auth, Meta, Provider, Rest, Status};
use crate::errors::ProviderError;
use std::time::Duration;

pub const META: Meta = Meta {
    name: "digitalocean",
    display_name: "DigitalOcean",
    kind: "vps",
    icon: "🌊",
    default_api_url: "https://api.digitalocean.com",
};

const GET_TIMEOUT: Duration = Duration::from_secs(15);
const POWER_TIMEOUT: Duration = Duration::from_secs(15);

const STATE_MAP: [(&str, &str); 3] =
    [("active", "running"), ("off", "stopped"), ("new", "starting")];

/// Action générique (contrat) → type d'action DigitalOcean.
const POWER_ACTIONS: [(&str, &str); 3] =
    [("start", "power_on"), ("stop", "power_off"), ("restart", "reboot")];

#[derive(Debug, Clone)]
pub struct Digitalocean {
    rest: Rest,
}

impl Digitalocean {
    pub fn new(api_key: &str, api_url: Option<&str>) -> Self {
        Self {
            rest: Rest::new(
                api_url.unwrap_or(META.default_api_url).to_string(),
                Auth::Bearer(api_key.to_string()),
                super::get_timeout(GET_TIMEOUT),
                POWER_TIMEOUT,
            ),
        }
    }
}

impl Provider for Digitalocean {
    fn meta(&self) -> &'static Meta {
        &META
    }

    fn rest(&self) -> &Rest {
        &self.rest
    }

    async fn fetch_raw(&self, target: &str) -> Result<Value, ProviderError> {
        // L'API répond {droplet: …} : on retourne le droplet lui-même (repli
        // sur l'enveloppe entière) pour que normalize() reçoive un droplet.
        let raw = self.rest.get_json(&format!("/v2/droplets/{target}")).await?;
        Ok(raw.get("droplet").cloned().unwrap_or(raw))
    }

    async fn send_power_raw(&self, target: &str, action: &str) -> Result<(), ProviderError> {
        let kind = POWER_ACTIONS
            .iter()
            .find(|(from, _)| *from == action)
            .map(|(_, to)| *to)
            .ok_or_else(|| ProviderError::other(format!("DigitalOcean: action non supportée \"{action}\"")))?;
        self.rest
            .post_json(
                &format!("/v2/droplets/{target}/actions"),
                &json!({ "type": kind }),
            )
            .await
            .map(|_| ())
    }

    fn normalize(&self, raw: &Value) -> Status {
        normalize(raw)
    }
}

pub fn normalize(raw: &Value) -> Status {
    let raw_state = str_at(raw, &["status"])
        .unwrap_or_else(|| "unknown".to_string())
        .to_lowercase();
    Status {
        name: str_at(raw, &["name"]),
        state: map_state(&raw_state, &STATE_MAP, "unknown"),
        raw_state,
        cpu_pct: None,
        ram_mb: None,
        ram_max_mb: None,
        disk_mb: None,
        disk_max_mb: None,
        uptime_seconds: None,
        players: None,
        // Première interface IPv4 publique (le port est toujours None :
        // DigitalOcean ne gère pas de port d'application).
        address: raw
            .get("networks")
            .and_then(|n| n.get("v4"))
            .and_then(|v4| v4.as_array())
            .and_then(|list| {
                list.iter().find_map(|net| {
                    if net.get("type").and_then(|t| t.as_str()) == Some("public") {
                        str_at(net, &["ip_address"]).map(|ip| super::Address { ip, port: None })
                    } else {
                        None
                    }
                })
            }),
        node: str_at(raw, &["region", "slug"]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_statuses() {
        let norm = |raw: Value| Digitalocean::new("k", None).normalize(&raw);
        assert_eq!(norm(json!({"status": "active"})).state, "running");
        assert_eq!(norm(json!({"status": "off"})).state, "stopped");
        assert_eq!(norm(json!({"status": "new"})).state, "starting");
        assert_eq!(norm(json!({"status": "archive"})).state, "unknown");
    }

    #[test]
    fn picks_first_public_ipv4() {
        let s = Digitalocean::new("k", None).normalize(&json!({
            "status": "active",
            "networks": {"v4": [
                {"type": "private", "ip_address": "10.0.0.1"},
                {"type": "public", "ip_address": "198.51.100.9"}
            ]}
        }));
        assert_eq!(s.address.unwrap().ip, "198.51.100.9");
    }
}
