//! Watchdog : surveillance périodique + alertes Discord (portage de
//! `lib/watchdog.js`).
//!
//! À chaque tick (défaut 60 s, premier après 15 s) :
//! - l'état de CHAQUE service est lu (via le cache 5 s partagé) ;
//! - les vérifications tournent EN PARALLÈLE (`join_all`, comme le
//!   `Promise.all` du JS) ;
//! - alertes si : transition running → autre (hors action volontaire),
//!   CPU > 90 %, RAM > 90 %, disque > 90 % ;
//! - anti-spam : 15 min entre deux alertes de même type pour un service ;
//! - délai de grâce de 5 min après un arrêt VOLONTAIRE (aucune fausse
//!   alerte après un /stop) ;
//! - le verrou est lu en LECTURE PURE (`read_lock_snapshot`) : un scan ne
//!   réécrit jamais `active.lock`.
//!
//! Conception Rust : le watchdog est une TÂCHE tokio détachée (`spawn`),
//! détentrice de son propre état (dernier état, dernières alertes) —
//! aucune donnée partagée avec les commandes, sauf en lecture.

use poise::serenity_prelude as serenity;
use serenity::{ChannelId, CreateMessage, Http};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::api::Api;
use crate::config::Config;
use crate::embeds::{card, state, Tone};
use crate::i18n::{fill, Catalog};
use crate::logger;
use crate::services::display_name;
use crate::state::State;

const DEFAULT_INTERVAL: Duration = Duration::from_secs(60);
const ALERT_COOLDOWN: Duration = Duration::from_secs(15 * 60);
const CPU_THRESHOLD: f64 = 90.0;
const RAM_THRESHOLD: f64 = 90.0;
const DISK_THRESHOLD: f64 = 90.0;
/// Après la fin d'une action power volontaire, la transition
/// running → arrêt est ATTENDUE : alertes crash ignorées pendant ce délai.
const CRASH_GRACE: Duration = Duration::from_secs(5 * 60);

/// État interne du watchdog : dernier état vu par service + horodatage des
/// dernières alertes (anti-spam). Partagé entre les vérifications parallèles
/// via `Mutex` — les sections critiques sont courtes, sans await.
struct WatchdogState {
    last_state: HashMap<String, String>,
    /// Dernier nom AFFICHÉ par service (celui de l'API) : les logs d'échec
    /// restent lisibles (« ApaWorld ») même quand la vérification échoue.
    last_name: HashMap<String, String>,
    last_alert: HashMap<String, Instant>,
}

impl WatchdogState {
    /// Anti-spam : renvoie true si une alerte de ce type peut repartir.
    fn can_alert(&mut self, key: &str) -> bool {
        let now = Instant::now();
        match self.last_alert.get(key) {
            Some(at) if at.elapsed() < ALERT_COOLDOWN => false,
            _ => {
                self.last_alert.insert(key.to_string(), now);
                true
            }
        }
    }
}

/// Démarre le watchdog (appelé une fois, après le Ready gateway).
/// Sans `DISCORD_ALERT_CHANNEL_ID` : désactivé, aucun scan, aucune alerte.
pub fn start(http: Arc<Http>, cfg: Config, api: Arc<Api>, st: Arc<State>) {
    let catalog = cfg.language_catalog();
    let Some(channel_id) = cfg.alert_channel_id.clone() else {
        logger::info(catalog.audit.watchdog_disabled);
        return;
    };
    let interval_ms = cfg
        .alert_interval_ms
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_INTERVAL.as_millis() as u64);
    logger::info(fill(
        catalog.audit.watchdog_started,
        &[("interval", &interval_ms.to_string()), ("channel", &channel_id)],
    ));

    tokio::spawn(async move {
        // Premier scan 15 s après la connexion Discord (comme le JS).
        tokio::time::sleep(Duration::from_secs(15)).await;
        let channel = ChannelId::new(channel_id.parse::<u64>().unwrap_or_default());
        let shared = Arc::new(Mutex::new(WatchdogState {
            last_state: HashMap::new(),
            last_name: HashMap::new(),
            last_alert: HashMap::new(),
        }));
        let mut ticker = tokio::time::interval(Duration::from_millis(interval_ms));
        loop {
            ticker.tick().await;
            // Vérifications EN PARALLÈLE sur l'ensemble des services.
            // L'environnement d'envoi est construit UNE fois par tick puis
            // emprunté par chaque vérification (il doit vivre au moins
            // jusqu'à la fin du `join_all`).
            let env = CheckEnv {
                http: http.clone(),
                channel,
                shared: shared.clone(),
            };
            let checks = cfg
                .services
                .iter()
                .map(|(alias, id)| {
                    check_service(&cfg, api.clone(), st.clone(), &env, alias, id)
                })
                .collect::<Vec<_>>();
            futures::future::join_all(checks).await;
        }
    });
}

/// Contexte d'envoi partagé par les vérifications parallèles d'un tick.
struct CheckEnv {
    http: Arc<Http>,
    channel: ChannelId,
    shared: Arc<Mutex<WatchdogState>>,
}

/// Vérification d'un service + alertes éventuelles.
async fn check_service(
    cfg: &Config,
    api: Arc<Api>,
    st: Arc<State>,
    env: &CheckEnv,
    alias: &str,
    service_id: &str,
) {
    let catalog = cfg.language_catalog();
    let status = match api.get(service_id, false).await {
        Ok(s) => s,
        Err(e) => {
            // Nom mémorisé au dernier scan réussi (repli : alias brut).
            let name = {
                let ws = match env.shared.lock() {
                    Ok(g) => g,
                    Err(poisoned) => poisoned.into_inner(),
                };
                ws.last_name
                    .get(alias)
                    .cloned()
                    .unwrap_or_else(|| alias.to_string())
            };
            logger::error(fill(
                catalog.audit.watchdog_check_failed,
                &[("alias", &name), ("error", &e.to_string())],
            ));
            return;
        }
    };
    let current_state = state(&status);
    let name = display_name(&status, alias);

    // Décisions d'alerte sous le verrou (sections courtes)…
    let mut alerts: Vec<(String, String, Tone)> = Vec::new();
    {
        let mut ws = match env.shared.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let previous = ws.last_state.get(alias).cloned();

        // Alerte « down » : uniquement sur une transition running → autre.
        if previous.as_deref() == Some("running") && current_state != "running" {
            let locks = st.read_lock_snapshot(); // LECTURE PURE
            let in_grace = st.lock_release_elapsed(alias) < CRASH_GRACE;
            if !locks.contains_key(alias) && !in_grace && ws.can_alert(&format!("{alias}:crash")) {
                let address = status
                    .address
                    .as_ref()
                    .map(|a| match a.port {
                        Some(port) => format!("{}:{port}", a.ip),
                        None => a.ip.clone(),
                    })
                    .unwrap_or_else(|| "unknown".to_string());
                alerts.push((
                    fill(catalog.watchdog.down.title, &[("name", &name)]),
                    fill(
                        catalog.watchdog.down.body,
                        &[
                            ("state", &current_state),
                            ("address", &address),
                            ("provider", cfg.provider_label),
                        ],
                    ),
                    Tone::Bad,
                ));
            }
        }

        if let Some(cpu) = status.cpu_pct {
            if cpu > CPU_THRESHOLD && ws.can_alert(&format!("{alias}:cpu")) {
                alerts.push((
                    fill(catalog.watchdog.high_cpu.title, &[("name", &name)]),
                    fill(
                        catalog.watchdog.high_cpu.body,
                        &[
                            ("pct", &format!("{cpu:.1}")),
                            ("threshold", &CPU_THRESHOLD.to_string()),
                        ],
                    ),
                    Tone::Wait,
                ));
            }
        }
        if let (Some(used), Some(max)) = (status.ram_mb, status.ram_max_mb) {
            if max > 0.0 {
                let pct = used / max * 100.0;
                if pct > RAM_THRESHOLD && ws.can_alert(&format!("{alias}:ram")) {
                    alerts.push((
                        fill(catalog.watchdog.high_ram.title, &[("name", &name)]),
                        fill(
                            catalog.watchdog.high_ram.body,
                            &[
                                ("pct", &format!("{pct:.1}")),
                                ("used", &format!("{used:.0}")),
                                ("max", &format!("{max:.0}")),
                            ],
                        ),
                        Tone::Wait,
                    ));
                }
            }
        }
        if let (Some(used), Some(max)) = (status.disk_mb, status.disk_max_mb) {
            if max > 0.0 {
                let pct = used / max * 100.0;
                if pct > DISK_THRESHOLD && ws.can_alert(&format!("{alias}:disk")) {
                    alerts.push((
                        fill(catalog.watchdog.high_disk.title, &[("name", &name)]),
                        fill(
                            catalog.watchdog.high_disk.body,
                            &[
                                ("pct", &format!("{pct:.1}")),
                                ("used", &format!("{used:.0}")),
                                ("max", &format!("{max:.0}")),
                            ],
                        ),
                        Tone::Wait,
                    ));
                }
            }
        }

        ws.last_state.insert(alias.to_string(), current_state);
        ws.last_name.insert(alias.to_string(), name.clone());
    }

    // …puis envoi HORS verrou (les attentes réseau ne bloquent personne).
    for (title, body, tone) in alerts {
        send_alert(&env.http, env.channel, title, body, tone, catalog).await;
    }
}

/// Envoie une alerte dans le salon configuré (mentions désactivées).
async fn send_alert(
    http: &Arc<Http>,
    channel: ChannelId,
    title: String,
    body: String,
    tone: Tone,
    catalog: &Catalog,
) {
    let message = CreateMessage::new()
        .add_embed(card(&title, &body, tone, Some("APABOT • WATCHDOG")))
        .allowed_mentions(serenity::CreateAllowedMentions::new());
    match channel.send_message(http, message).await {
        Ok(_) => logger::warn(fill(catalog.audit.watchdog_alert_sent, &[("title", &title)])),
        Err(e) => logger::error(fill(
            catalog.audit.watchdog_alert_failed,
            &[("error", &e.to_string())],
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cooldown_blocks_duplicate_alerts() {
        let mut ws = WatchdogState {
            last_state: HashMap::new(),
            last_name: HashMap::new(),
            last_alert: HashMap::new(),
        };
        assert!(ws.can_alert("x:crash"));
        assert!(!ws.can_alert("x:crash"));
        assert!(ws.can_alert("x:cpu")); // un AUTRE type passe
    }
}
