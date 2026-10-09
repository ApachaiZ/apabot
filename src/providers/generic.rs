//! Provider GÉNÉRIQUE — pilote n'importe quel panneau hébergeur exposant une
//! API REST, configuré UNIQUEMENT via le `.env` (aucun code pour ajouter un
//! serveur) :
//!
//! ```text
//! PROVIDER=generic
//! PROVIDER_API_URL=https://panel.example.com/api      (obligatoire)
//! PROVIDER_API_KEY=cle-api                            (optionnel)
//! PROVIDER_AUTH_HEADER=X-API-Key                      (défaut : Authorization)
//! PROVIDER_AUTH_VALUE="Bearer {key}"                  (défaut : "{key}")
//! PROVIDER_STATUS_PATH=/services/{id}                 (défaut)
//! PROVIDER_POWER_PATH=/services/{id}/power            (défaut, {action} remplacé)
//! PROVIDER_POWER_METHOD=POST                          (POST|GET)
//! PROVIDER_POWER_BODY={"action":"{action}"}           (vide = pas de corps)
//! PROVIDER_STATUS_UNWRAP=data                         (clés à descendre, virgules)
//! PROVIDER_STATE_RUNNING=running,online               (valeurs « démarré »)
//! PROVIDER_STATE_STOPPED=stopped,offline              (valeurs « arrêté »)
//! ```
//!
//! La normalisation accepte une large table d'alias de champs (`state`/
//! `status`, `memory`/`ram`, `uptime_seconds`/`uptimeSeconds`, `players`/
//! `playerCount`…) et tolère les nombres en chaînes — le dashboard masque
//! de lui-même les métriques absentes. Toute valeur inconnue d'état passe
//! telle quelle (vocabulaire tolérant du contrat).

use serde_json::{json, Value};

use super::{normalize_address, Auth, Meta, Provider, Rest, Status};
use crate::errors::{ExitError, ProviderError};
use crate::i18n::fill;
use std::time::Duration;

pub const META: Meta = Meta {
    name: "generic",
    display_name: "Generic REST",
    kind: "generic",
    icon: "🔌",
    default_api_url: "",
};

// Délais raisonnables pour un panneau inconnu : 30 s en lecture, 120 s en
// power (un stop peut être long, comme YorkHost).
const GET_TIMEOUT: Duration = Duration::from_secs(30);
const POWER_TIMEOUT: Duration = Duration::from_secs(120);

/// Clés candidates par métrique (première présente gagne).
const STATE_KEYS: [&str; 4] = ["state", "status", "power_state", "powerState"];
const CPU_KEYS: [&str; 7] = ["cpu", "cpu_pct", "cpuPct", "cpuPercent", "cpu_usage", "cpuUsage", "load"];
const RAM_KEYS: [&str; 7] = ["ram", "ram_mb", "ramMb", "memory", "memory_mb", "memoryMb", "mem"];
const RAM_MAX_KEYS: [&str; 7] = ["ram_max", "ramMax", "ram_max_mb", "memory_max", "memoryMax", "mem_max", "totalMemory"];
const DISK_KEYS: [&str; 5] = ["disk", "disk_mb", "diskMb", "storage", "storage_mb"];
const DISK_MAX_KEYS: [&str; 5] = ["disk_max", "diskMax", "disk_max_mb", "storage_max", "storageMax"];
const UPTIME_KEYS: [&str; 4] = ["uptime", "uptime_seconds", "uptimeSeconds", "uptime_secs"];
const PLAYERS_KEYS: [&str; 5] = ["players", "player_count", "playerCount", "players_online", "onlinePlayers"];
const NAME_KEYS: [&str; 3] = ["name", "hostname", "label"];
const NODE_KEYS: [&str; 4] = ["node", "location", "datacenter", "region"];
const IP_KEYS: [&str; 5] = ["ip", "ipv4", "address", "host", "ip_address"];

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(|v| v.trim().to_string())
}

fn list_env(name: &str, default: &str) -> Vec<String> {
    env(name)
        .unwrap_or_else(|| default.to_string())
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Première valeur présente parmi les clés candidates (ou leurs alias
/// simples en minuscules).
fn field<'a>(raw: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    for key in keys {
        if let Some(v) = raw.get(key) {
            return Some(v);
        }
    }
    None
}

/// Nombre à la première clé candidate présente, en tolérant une chaîne
/// numérique ("2048").
fn num_alias(raw: &Value, keys: &[&str]) -> Option<f64> {
    match field(raw, keys)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// Chaîne à la première clé candidate présente (ou nombre converti).
fn str_alias(raw: &Value, keys: &[&str]) -> Option<String> {
    match field(raw, keys)? {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub struct Generic {
    rest: Rest,
    status_path: String,
    power_path: String,
    power_method: String,
    power_body: String,
    unwrap_keys: Vec<String>,
    running_values: Vec<String>,
    stopped_values: Vec<String>,
}

impl Generic {
    pub fn new(api_key: &str, api_url: Option<&str>) -> Result<Self, ExitError> {
        let base = api_url
            .map(str::to_string)
            .or_else(|| env("PROVIDER_API_URL"));
        let Some(base) = base else {
            return Err(ExitError::config(
                "The \"generic\" provider requires PROVIDER_API_URL (base URL of the panel API).",
            ));
        };

        // Authentification : en-tête nom + gabarit de valeur (`{key}`).
        let auth = if api_key.is_empty() {
            Auth::None
        } else {
            let header = env("PROVIDER_AUTH_HEADER").unwrap_or_else(|| "Authorization".to_string());
            let value = env("PROVIDER_AUTH_VALUE").unwrap_or_else(|| "{key}".to_string());
            let rendered = fill(&value, &[("key", api_key)]);
            Auth::Header(format!("{header}: {rendered}"))
        };

        Ok(Self {
            rest: Rest::new(base, auth, super::get_timeout(GET_TIMEOUT), POWER_TIMEOUT),
            status_path: env("PROVIDER_STATUS_PATH").unwrap_or_else(|| "/services/{id}".to_string()),
            power_path: env("PROVIDER_POWER_PATH").unwrap_or_else(|| "/services/{id}/power".to_string()),
            power_method: env("PROVIDER_POWER_METHOD")
                .unwrap_or_else(|| "POST".to_string())
                .to_uppercase(),
            power_body: env("PROVIDER_POWER_BODY").unwrap_or_else(|| "{\"action\":\"{action}\"}".to_string()),
            unwrap_keys: list_env("PROVIDER_STATUS_UNWRAP", ""),
            running_values: list_env("PROVIDER_STATE_RUNNING", "running,online,started,on,active,enabled"),
            stopped_values: list_env("PROVIDER_STATE_STOPPED", "stopped,offline,stopping,off,inactive,disabled"),
        })
    }

    /// Chemin avec `{id}` remplacé (testable, pur).
    pub fn status_path_for(&self, target: &str) -> String {
        fill(&self.status_path, &[("id", target)])
    }
}

impl Provider for Generic {
    fn meta(&self) -> &'static Meta {
        &META
    }

    fn rest(&self) -> &Rest {
        &self.rest
    }

    async fn fetch_raw(&self, target: &str) -> Result<Value, ProviderError> {
        let path = self.status_path_for(target);
        let mut raw = self.rest.get_json(&path).await?;
        // Descente éventuelle dans un conteneur (`data`, `service`…).
        for key in &self.unwrap_keys {
            raw = match raw.get(key).cloned() {
                Some(inner) => inner,
                None => break,
            };
        }
        Ok(raw)
    }

    async fn send_power_raw(&self, target: &str, action: &str) -> Result<(), ProviderError> {
        if !["start", "stop", "restart"].contains(&action) {
            return Err(ProviderError::other(format!("action inconnue pour generic: \"{action}\"")));
        }
        let path = fill(&self.power_path, &[("id", target), ("action", action)]);
        match self.power_method.as_str() {
            "GET" => self.rest.get_json(&path).await.map(|_| ()),
            "POST" if self.power_body.is_empty() => self.rest.post_empty(&path).await.map(|_| ()),
            "POST" => {
                let body: Value = match serde_json::from_str(
                    &fill(&self.power_body, &[("action", action)]),
                ) {
                    Ok(v) => v,
                    Err(_) => json!({ "action": action }),
                };
                self.rest.post_json(&path, &body).await.map(|_| ())
            }
            other => Err(ProviderError::other(format!(
                "PROVIDER_POWER_METHOD inconnu: \"{other}\" (attendu POST ou GET)"
            ))),
        }
    }

    fn normalize(&self, raw: &Value) -> Status {
        normalize(raw, &self.running_values, &self.stopped_values)
    }
}

/// Normalisation vers le contrat canonique, guidée par les listes d'états
/// configurées. Les booléens sont acceptés (`true` = démarré, `false` =
/// arrêté) ; les valeurs inconnues passent telles quelles en minuscules.
pub fn normalize(raw: &Value, running_values: &[String], stopped_values: &[String]) -> Status {
    let raw_state = match field(raw, &STATE_KEYS) {
        Some(Value::Bool(true)) => "running".to_string(),
        Some(Value::Bool(false)) => "stopped".to_string(),
        Some(Value::String(s)) => s.trim().to_lowercase(),
        Some(Value::Number(n)) => n.to_string(),
        _ => "unknown".to_string(),
    };
    let state = if running_values.contains(&raw_state) {
        "running".to_string()
    } else if stopped_values.contains(&raw_state) {
        "stopped".to_string()
    } else {
        raw_state.clone()
    };

    let address = match field(raw, &IP_KEYS) {
        Some(v @ Value::Object(_)) => normalize_address(v),
        Some(v) => {
            // Chaîne « ip:port » ou ip simple, port éventuel à côté.
            let mut a = normalize_address(v).unwrap_or_else(|| super::Address {
                ip: match v {
                    Value::String(s) => s.trim().to_string(),
                    other => other.to_string(),
                },
                port: None,
            });
            if a.port.is_none() {
                if let Some(p) = num_alias(raw, &["port"]) {
                    a.port = Some(p as u32);
                }
            }
            Some(a)
        }
        None => None,
    };

    Status {
        name: str_alias(raw, &NAME_KEYS),
        state,
        raw_state,
        cpu_pct: num_alias(raw, &CPU_KEYS),
        ram_mb: num_alias(raw, &RAM_KEYS),
        ram_max_mb: num_alias(raw, &RAM_MAX_KEYS),
        disk_mb: num_alias(raw, &DISK_KEYS),
        disk_max_mb: num_alias(raw, &DISK_MAX_KEYS),
        uptime_seconds: num_alias(raw, &UPTIME_KEYS),
        players: num_alias(raw, &PLAYERS_KEYS),
        address,
        node: str_alias(raw, &NODE_KEYS),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> (Vec<String>, Vec<String>) {
        (
            vec!["running".into(), "online".into()],
            vec!["stopped".into(), "offline".into()],
        )
    }

    #[test]
    fn maps_common_aliases() {
        let (r, s) = defaults();
        let raw = json!({
            "status": "online",
            "memory": "2048",
            "memoryMax": 4096,
            "playerCount": 3,
            "uptimeSeconds": 3600,
            "ip": "1.2.3.4",
            "port": "25565",
            "hostname": "mc-1",
        });
        let status = normalize(&raw, &r, &s);
        assert_eq!(status.state, "running");
        assert_eq!(status.raw_state, "online");
        assert_eq!(status.ram_mb, Some(2048.0));
        assert_eq!(status.ram_max_mb, Some(4096.0));
        assert_eq!(status.players, Some(3.0));
        assert_eq!(status.uptime_seconds, Some(3600.0));
        assert_eq!(status.address.as_ref().map(|a| a.ip.as_str()), Some("1.2.3.4"));
        assert_eq!(status.address.as_ref().and_then(|a| a.port), Some(25565));
        assert_eq!(status.name.as_deref(), Some("mc-1"));
    }

    #[test]
    fn accepts_booleans_and_unknown_states() {
        let (r, s) = defaults();
        let raw = json!({ "state": false });
        assert_eq!(normalize(&raw, &r, &s).state, "stopped");
        let raw = json!({ "state": "rebooting" });
        assert_eq!(normalize(&raw, &r, &s).state, "rebooting");
    }

    #[test]
    fn templates_expand_placeholders() {
        let provider = Generic::new("k", Some("https://panel.example/api")).unwrap();
        assert_eq!(provider.status_path_for("42"), "/services/42");
    }
}
