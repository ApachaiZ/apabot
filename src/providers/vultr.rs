//! Provider Vultr (instances, API v2, Bearer API key).
//!
//!   GET  /v2/instances/{id}        → `{instance: {status, label, main_ip, ram, disk, …}}`
//!   POST /v2/instances/{id}/start  → allumer (aucun corps)
//!   POST /v2/instances/{id}/halt   → éteindre (arrêt propre, ≠ power off)
//!   POST /v2/instances/{id}/reboot → redémarrer
//!
//! Unités : `ram` en Mo, `disk` en Go (converti en Mo). Ce sont des TOTAUX
//! de plan → `ramMaxMb` / `diskMaxMb` ; l'usage courant n'est pas exposé.

use serde_json::Value;

use super::{map_state, num, str_at, Auth, Meta, Provider, Rest, Status};
use crate::errors::ProviderError;
use std::time::Duration;

pub const META: Meta = Meta {
    name: "vultr",
    display_name: "Vultr",
    kind: "vps",
    icon: "☁️",
    default_api_url: "https://api.vultr.com",
};

const GET_TIMEOUT: Duration = Duration::from_secs(15);
const POWER_TIMEOUT: Duration = Duration::from_secs(15);

/// Statuts Vultr v2 → vocabulaire commun.
/// `pending` = provisionnement/démarrage en cours → `starting`.
const STATE_MAP: [(&str, &str); 3] =
    [("active", "running"), ("stopped", "stopped"), ("pending", "starting")];

/// Action générique (contrat) → chemin d'action Vultr.
/// `stop` correspond à `/halt` : Vultr distingue halt (arrêt propre) de
/// power off — POST /halt est bien l'arrêt standard.
const ACTION_MAP: [(&str, &str); 3] =
    [("start", "start"), ("stop", "halt"), ("restart", "reboot")];

#[derive(Debug, Clone)]
pub struct Vultr {
    rest: Rest,
}

impl Vultr {
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

impl Provider for Vultr {
    fn meta(&self) -> &'static Meta {
        &META
    }

    fn rest(&self) -> &Rest {
        &self.rest
    }

    async fn fetch_raw(&self, target: &str) -> Result<Value, ProviderError> {
        self.rest.get_json(&format!("/v2/instances/{target}")).await
    }

    async fn send_power_raw(&self, target: &str, action: &str) -> Result<(), ProviderError> {
        let verb = ACTION_MAP
            .iter()
            .find(|(from, _)| *from == action)
            .map(|(_, to)| *to)
            .ok_or_else(|| ProviderError::other(format!("action inconnue pour vultr: \"{action}\"")))?;
        // Les endpoints d'action Vultr n'acceptent aucun corps de requête.
        self.rest
            .post_empty(&format!("/v2/instances/{target}/{verb}"))
            .await
            .map(|_| ())
    }

    fn normalize(&self, raw: &Value) -> Status {
        normalize(raw)
    }
}

pub fn normalize(raw: &Value) -> Status {
    // L'API renvoie {instance: {...}} ; on accepte aussi un objet déjà déplié.
    let instance = raw.get("instance").unwrap_or(raw);
    let raw_state = str_at(instance, &["status"])
        .unwrap_or_else(|| "unknown".to_string())
        .to_lowercase();
    let disk_gb = num(instance, &["disk"]);
    Status {
        name: str_at(instance, &["label"]).or_else(|| str_at(instance, &["hostname"])),
        state: map_state(&raw_state, &STATE_MAP, "unknown"),
        raw_state,
        cpu_pct: None,
        ram_mb: None,
        ram_max_mb: num(instance, &["ram"]), // déjà en Mo (doc officielle)
        disk_mb: None,
        disk_max_mb: disk_gb.map(|g| g * 1024.0), // Go → Mo
        uptime_seconds: None,
        players: None,
        address: str_at(instance, &["main_ip"]).map(|ip| super::Address { ip, port: None }),
        node: str_at(instance, &["region"]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_statuses_and_converts_disk_gb_to_mb() {
        let s = Vultr::new("k", None).normalize(&json!({
            "instance": {"status": "active", "label": "web", "ram": 3145728, "disk": 3840}
        }));
        assert_eq!(s.state, "running");
        assert_eq!(s.ram_max_mb, Some(3145728.0));
        assert_eq!(s.disk_max_mb, Some(3840.0 * 1024.0));
    }

    #[test]
    fn pending_maps_to_starting() {
        let s = Vultr::new("k", None).normalize(&json!({"instance": {"status": "pending"}}));
        assert_eq!(s.state, "starting");
    }
}
