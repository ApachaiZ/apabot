//! Couche d'accès aux providers : cache court (5 s) + déduplication en vol.
//!
//! Parité avec `lib/api.js`, avec une amélioration de conception :
//!
//! - **Cache 5 s** par service : les GET redondants (watchdog + /status)
//!   sont mutualisés. Les appels critiques (avant/après un POST power,
//!   /ping) passent `fresh = true` et ignorent le cache.
//! - **Déduplication inconditionnelle** : deux appels simultanés (fresh ou
//!   non) partagent LA MÊME requête HTTP en vol. En Rust, c'est un
//!   [`tokio::sync::OnceCell`] dans une [`Arc`] : le premier arrivant lance
//!   le fetch, tous les autres attendent le même résultat — sans verrou
//!   tenu pendant l'attente réseau (le `Mutex` ne protège que l'insertion
//!   dans la map). C'est plus efficace que le `Map<Promise>` du JS.
//!
//! Le choix du type `Box<dyn Provider>` (trait object) permet de sélectionner
//! le provider au démarrage à partir de `PROVIDER=` sans toucher au code.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, OnceCell};

use crate::errors::ProviderError;
use crate::providers::{AnyProvider, Provider, Status};

const CACHE_TTL: Duration = Duration::from_secs(5);

/// Une requête en vol PARTAGÉE : le premier arrivant l'exécute, les autres
/// attendent le même résultat (l'alias de type rend la map lisible).
type SharedFetch = Arc<OnceCell<Result<Status, ProviderError>>>;

pub struct Api {
    provider: AnyProvider,
    /// id → (horodatage, état normalisé) — consulté uniquement hors `fresh`.
    cache: Mutex<HashMap<String, (Instant, Status)>>,
    /// id → requête en vol partagée. `OnceCell` ne s'exécute qu'une fois,
    /// quel que soit le nombre d'awaiters concurrents.
    inflight: Mutex<HashMap<String, SharedFetch>>,
}

impl Api {
    pub fn new(provider: AnyProvider) -> Self {
        Self {
            provider,
            cache: Mutex::new(HashMap::new()),
            inflight: Mutex::new(HashMap::new()),
        }
    }

    /// Lit l'état en cache SANS réseau (l'entrée peut avoir expiré : le
    /// cache 5 s n'est pas rafraîchi ici). Sert à NOMMER un service dans
    /// les logs quand la vérification vient d'échouer — jamais à afficher
    /// un état de santé.
    pub async fn get_cached(&self, service_id: &str) -> Option<Status> {
        self.cache
            .lock()
            .await
            .get(service_id)
            .map(|(_, s)| s.clone())
    }

    /// Invalide le cache d'un service (après un POST power) — ou tout le
    /// cache si `service_id` est `None`.
    pub async fn clear_cache(&self, service_id: Option<&str>) {
        let mut cache = self.cache.lock().await;
        match service_id {
            Some(id) => {
                cache.remove(id);
            }
            None => cache.clear(),
        }
    }

    /// Lit l'état normalisé d'un service. `fresh = true` ignore le cache
    /// (mais pas la déduplication en vol).
    pub async fn get(&self, service_id: &str, fresh: bool) -> Result<Status, ProviderError> {
        // 1. Une requête est-elle déjà en vol pour ce service ?
        if let Some(cell) = self.inflight.lock().await.get(service_id).cloned() {
            // `get_or_init` ne lance le futur qu'une fois ; les concurrents
            // attendent le même résultat. Le clone est gratuit (Arc).
            return cell
                .get_or_init(|| async { self.fetch_normalized(service_id).await })
                .await
                .clone();
        }

        // 2. Sinon : cache 5 s (sauf fresh).
        if !fresh {
            if let Some((at, status)) = self.cache.lock().await.get(service_id) {
                if at.elapsed() < CACHE_TTL {
                    return Ok(status.clone());
                }
            }
        }

        // 3. Aucun résultat exploitable : on publie une nouvelle cellule en
        //    vol, on la remplit, puis on nettoie.
        let cell: SharedFetch = Arc::new(OnceCell::new());
        self.inflight
            .lock()
            .await
            .insert(service_id.to_string(), cell.clone());
        let result = cell
            .get_or_init(|| async { self.fetch_normalized(service_id).await })
            .await
            .clone();
        self.inflight.lock().await.remove(service_id);
        result
    }

    /// Fetch brut + normalisation + remplissage du cache.
    async fn fetch_normalized(&self, service_id: &str) -> Result<Status, ProviderError> {
        let raw = self.provider.fetch_raw(service_id).await?;
        let status = self.provider.normalize(&raw);
        self.cache
            .lock()
            .await
            .insert(service_id.to_string(), (Instant::now(), status.clone()));
        Ok(status)
    }

    /// Envoie UNE action power. Aucun retry ici : l'incertitude de
    /// transmission est gérée par l'appelant (power.rs), jamais en silence.
    pub async fn send_power(&self, service_id: &str, action: &str) -> Result<(), ProviderError> {
        self.provider.send_power_raw(service_id, action).await
    }
}
