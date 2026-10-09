//! Power actions : démarrage / arrêt / redémarrage (portage de `lib/power.js`).
//!
//! Cycle de vie (chaque étape est une GARANTIE, voir le README) :
//! 1. permission → verrou par service → double confirmation ;
//! 2. re-vérification (permission + verrou) APRÈS la confirmation ;
//! 3. pose du verrou persistant (`active.lock`) ;
//! 4. état initial (fresh) ; si déjà dans l'état cible → carte directe ;
//! 5. POST power UNIQUE (jamais de retry) ;
//! 6. en cas d'erreur de transmission : soit échec immédiat (rien n'est
//!    parti), soit vérification d'état, soit bascule sur le suivi ;
//! 7. suivi silencieux : polling à backoff 5 s → 30 s, détection de
//!    transition/uptime reset, 2 mesures stables exigées, 10 min max,
//!    bouton « Arrêter le suivi » ;
//! 8. relâchement du verrou + délai de grâce anti-fausse-alerte.
//!
//! Conception Rust : la « course » JS entre un `setTimeout` et une Promise
//! de clic devient un [`tokio::select!`] entre un sommeil et le collecteur
//! de boutons — deux futurs, une seule boucle, zéro blocage.

use futures::StreamExt;
use poise::serenity_prelude as serenity;
use rand::RngCore;
use serenity::{
    ButtonStyle, CreateActionRow, CreateButton, CreateEmbed, CreateMessage,
    EditInteractionResponse, MessageId,
};
use std::time::{Duration, Instant};

use crate::assets::{self, Gif};
use crate::commands::{Context, Data, Error};
use crate::confirm;
use crate::embeds::{card, offline, online, state, Tone};
use crate::errors::{may_have_been_transmitted, ProviderError};
use crate::i18n::{fill, Catalog, PowerAction};
use crate::logger;
use crate::services::{display_name, pick_service};
use crate::stats;

/// Backoff progressif : `[5s ×3, 10s ×3, 15s ×2, 20s ×2, puis 30s]`.
/// Latence courte pour les actions rapides (stop), charge API réduite pour
/// les longues (start) — le tableau exact du JS.
const POLL_DELAYS: [Duration; 11] = [
    Duration::from_secs(5),
    Duration::from_secs(5),
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(10),
    Duration::from_secs(10),
    Duration::from_secs(15),
    Duration::from_secs(15),
    Duration::from_secs(20),
    Duration::from_secs(20),
    Duration::from_secs(30),
];

/// Suivi maximal d'une action avant de déclarer la fin non confirmée.
const TRACKING_TIMEOUT: Duration = Duration::from_secs(600);

/// Au-delà de 5 échecs de lecture consécutifs, on abandonne le suivi.
const MAX_FAILURES: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Start,
    Stop,
    Restart,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Start => "start",
            Action::Stop => "stop",
            Action::Restart => "restart",
        }
    }

    /// Textes localisés de l'action (verbe, majuscule, titre de confirmation).
    pub fn text(self, catalog: &Catalog) -> PowerAction {
        match self {
            Action::Start => catalog.power.actions.start,
            Action::Stop => catalog.power.actions.stop,
            Action::Restart => catalog.power.actions.restart,
        }
    }
}

/// Ligne d'estimation « ⏱ En moyenne : 51 s » — durée moyenne PERSISTÉE des
/// transactions réussies précédentes (lib/stats). Aucun compteur en direct.
fn eta_line(action: Action, catalog: &Catalog) -> String {
    stats::avg_seconds(action.as_str())
        .map(|seconds| format!("\n{}", fill(catalog.power.eta, &[("seconds", &seconds.to_string())])))
        .unwrap_or_default()
}

/// Notification privée (DM) : déclenche une vraie notification push sur
/// l'appareil de l'utilisateur. Si les DMs sont désactivés, on échoue
/// silencieusement — la carte éphémère dans le salon reste le repli.
async fn notify_user(ctx: &Context<'_>, embed: CreateEmbed) {
    let message = CreateMessage::new()
        .add_embed(embed)
        .allowed_mentions(serenity::CreateAllowedMentions::new());
    // `direct_message` attend un `impl CacheHttp` : le `&Context` serenity
    // en est un (implémentation générique `&T where T: CacheHttp`).
    let _ = ctx.author().direct_message(ctx.serenity_context(), message).await;
}

/// Édite la carte de suivi : embed + GIF (optionnel) + boutons, sur la
/// RÉPONSE ORIGINALE de l'interaction. Toutes les éditions passent par le
/// token d'interaction ([`confirm::edit_original`]) : un message éphémère
/// est invisible pour le token du bot (erreur Discord 10008).
///
/// Le remplacement des pièces jointes est EXPLICITE : une édition ne retire
/// pas d'elle-même le GIF précédent — `clear_attachments` s'en charge.
async fn edit_card(
    ctx: &Context<'_>,
    mut embed: CreateEmbed,
    gif: Option<&Gif>,
    components: Vec<CreateActionRow>,
) -> Result<(), Error> {
    let edit = match gif {
        // GIF présent : `new_attachment` REMPLACE l'ensemble des pièces
        // jointes (l'ancien GIF disparaît) par le nouveau.
        Some(g) if g.attachment.is_some() => {
            embed = embed.image(g.image_url.as_deref().unwrap_or_default());
            EditInteractionResponse::new()
                .embed(embed)
                .new_attachment(g.attachment.as_ref().unwrap().clone())
                .components(components)
        }
        // Pas de GIF : la liste des pièces jointes est vidée EXPLICITEMENT
        // (une édition ne retire pas d'elle-même les anciens GIFs).
        _ => EditInteractionResponse::new()
            .embed(embed)
            .clear_attachments()
            .components(components),
    };
    confirm::edit_original(ctx, edit).await?;
    Ok(())
}

/// L'état cible est-il atteint ?
/// - `transition` : le service est passé par un état non-running pendant le
///   suivi (détection du cycle redémarrage) ;
/// - `use_initial` : variante de VÉRIFICATION post-erreur (un restart est
///   accompli si l'état initial était arrêté et que le service tourne).
fn reached(action: Action, current: &crate::providers::Status, before: &crate::providers::Status, transition: bool, use_initial: bool) -> bool {
    let uptime_reset = before.uptime_seconds.is_some()
        && current.uptime_seconds.is_some()
        && current.uptime_seconds < before.uptime_seconds;
    match action {
        Action::Stop => offline(current),
        Action::Start => online(current),
        Action::Restart => online(current) && ((use_initial && offline(before)) || transition || uptime_reset),
    }
}

/// Vérification ponctuelle après un échec de transmission : jusqu'à 3
/// lectures espacées du premier délai de backoff, SANS aucune nouvelle
/// requête power. Retourne l'état si l'objectif est atteint.
async fn verify_target_state(
    data: &Data,
    service_id: &str,
    action: Action,
    before: &crate::providers::Status,
) -> Option<crate::providers::Status> {
    for attempt in 0..3 {
        if attempt > 0 {
            tokio::time::sleep(POLL_DELAYS[0]).await;
        }
        if let Ok(current) = data.api.get(service_id, true).await {
            if reached(action, &current, before, false, true) {
                return Some(current);
            }
        }
    }
    None
}

/// Point d'entrée des commandes `/start`, `/stop`, `/restart`.
pub async fn power(ctx: Context<'_>, action: Action, service_alias: Option<String>) -> Result<(), Error> {
    let data = ctx.data();
    let catalog = data.catalog;

    // Vérification de permission EN AMONT : éviter d'exposer l'état du
    // verrou à un utilisateur non autorisé.
    if !data.state.is_allowed(&ctx.author().id.to_string()) {
        let c = catalog.power.access_denied;
        ctx.send(crate::commands::reply(card(c.title, c.body, Tone::Bad, None))).await?;
        return Ok(());
    }

    // Résolution du service ciblé (alias explicite → session → défaut).
    let (alias, service_id) = match pick_service(&data.cfg.services, service_alias.as_deref()) {
        Ok((alias, id)) => (alias.to_string(), id.to_string()),
        Err(message) => {
            let c = catalog.common.generic_error;
            ctx.send(crate::commands::reply(card(c.title, &message, Tone::Bad, None))).await?;
            return Ok(());
        }
    };
    let act = action.text(catalog);

    // Nom affiché : celui de l'API si disponible, sinon l'alias. Lecture
    // non-fresh (cache 5 s partagé avec le watchdog) ; un échec n'empêche
    // JAMAIS l'action — l'alias reste le repli.
    let mut service_name = alias.clone();
    if let Ok(status) = data.api.get(&service_id, false).await {
        service_name = display_name(&status, &alias);
    }

    // Relecture du verrou à chaque appel : l'état disque est la source de
    // vérité (un autre processus ou une édition manuelle peut l'avoir modifié).
    let active = data.state.load_persisted_lock();
    if let Some(active_action) = active.get(&alias) {
        let c = catalog.power.action_in_progress;
        let body = fill(c.body, &[("action", active_action), ("name", &service_name)]);
        ctx.send(crate::commands::reply(card(c.title, &body, Tone::Wait, None))).await?;
        return Ok(());
    }

    // ── Confirmation ──
    let warning = if matches!(action, Action::Stop | Action::Restart) {
        catalog.power.confirm.warning
    } else {
        ""
    };
    let confirm_body = format!(
        "{warning}{}",
        fill(catalog.power.confirm.body, &[("verb", act.verb), ("name", &service_name)])
    );
    let (track_msg, confirmed) = confirm::confirm(&ctx, card(act.title, &confirm_body, Tone::Wait, None)).await?;
    if !confirmed {
        return Ok(());
    }

    // Re-vérification POST-confirmation : une révocation pendant l'attente
    // doit prendre effet immédiatement.
    if !data.state.is_allowed(&ctx.author().id.to_string()) {
        let c = catalog.power.access_revoked;
        edit_card(&ctx, card(c.title, c.body, Tone::Bad, None), None, Vec::new()).await?;
        return Ok(());
    }
    // Une autre action a pu démarrer pendant la confirmation.
    let active_now = data.state.load_persisted_lock();
    if active_now.contains_key(&alias) {
        let c = catalog.power.busy;
        let body = fill(c.body, &[("name", &service_name)]);
        edit_card(&ctx, card(c.title, &body, Tone::Wait, None), None, Vec::new()).await?;
        return Ok(());
    }

    // ── Verrou posé : l'action est lancée ──
    data.state.write_lock(&alias, action.as_str());
    let action_start = Instant::now();

    // Le `finally` du JS : relâchement + délai de grâce, quoi qu'il arrive.
    let result = run_action(&ctx, track_msg, action, &alias, &service_id, action_start).await;
    data.state.record_lock_release(&alias);
    data.state.clear_lock(&alias);
    result
}

/// Cœur de l'action (après verrouillage). `track_msg` identifie la carte de
/// confirmation (la réponse originale) : toutes les étapes suivantes
/// l'ÉDITENT via le token d'interaction — aucun nouveau message,
/// 100 % éphémère comme le JS.
async fn run_action(
    ctx: &Context<'_>,
    track_msg: MessageId,
    action: Action,
    alias: &str,
    service_id: &str,
    action_start: Instant,
) -> Result<(), Error> {
    let data = ctx.data();
    let catalog = data.catalog;
    let act = action.text(catalog);
    let author_id = ctx.author().id;

    // Vérification d'état AVANT le loader : pas de GIF de chargement quand
    // il n'y a rien à faire.
    let before = data.api.get(service_id, true).await?;
    // Nom affiché du service pour les logs d'audit : celui de l'API
    // (« ApaWorld ») plutôt que l'alias brut (« default »).
    let name = display_name(&before, alias);
    if (action == Action::Start && online(&before)) || (action == Action::Stop && offline(&before)) {
        let c = catalog.power.already_in_target_state;
        let body = fill(c.body, &[("state", &state(&before))]);
        let gif = assets::gif_image("error");
        edit_card(ctx, card(c.title, &body, Tone::Good, None), Some(&gif), Vec::new()).await?;
        return Ok(());
    }

    // ── POST power : UNE seule requête, jamais de retry ──
    // (Le GIF `loading` est déjà en place depuis le clic de confirmation —
    // voir confirm.rs : le loader couvre la lecture d'état ET la transmission.)
    let mut send_error: Option<ProviderError> = None;
    let mut had_send_error = false;
    let power_start = Instant::now();
    match data.api.send_power(service_id, action.as_str()).await {
        Ok(()) => {
            logger::info(fill(
                catalog.audit.power_send_ok,
                &[
                    ("action", action.as_str()),
                    ("alias", name.as_str()),
                    ("ms", &power_start.elapsed().as_millis().to_string()),
                ],
            ));
        }
        Err(err) => {
            send_error = Some(err.clone());
            had_send_error = true;
            logger::error(fill(
                catalog.audit.power_send_failed,
                &[
                    ("action", action.as_str()),
                    ("alias", name.as_str()),
                    ("error", &err.message.clone()),
                    ("ms", &power_start.elapsed().as_millis().to_string()),
                ],
            ));
        }
    }
    // Le cache contient l'état pré-POST : invalidation obligatoire, même en
    // cas d'erreur (sinon un /status suivant servirait un état obsolète).
    data.api.clear_cache(Some(service_id)).await;

    if let Some(err) = send_error.as_ref() {
        if !may_have_been_transmitted(err) {
            // Rien n'a pu partir (4xx, réseau injoignable…) : erreur immédiate.
            return Err(err.clone().into());
        }
        // L'action a PU être transmise (timeout, 5xx après exécution…) :
        // vérification rapide de l'état réel — uniquement des LECTURES.
        if let Some(confirmed) = verify_target_state(data, service_id, action, &before).await {
            logger::info(fill(
                catalog.audit.power_applied_despite_error,
                &[
                    ("action", action.as_str()),
                    ("alias", name.as_str()),
                    ("state", &state(&confirmed)),
                ],
            ));
            stats::record(action.as_str(), action_start.elapsed().as_millis() as u64);
            let c = catalog.power.applied_despite_error;
            let body = fill(
                c.body,
                &[
                    ("cap", act.cap),
                    ("state", &state(&confirmed)),
                    ("provider", data.cfg.provider_label),
                ],
            );
            let gif = assets::gif_image("success");
            edit_card(ctx, card(c.title, &body, Tone::Good, None), Some(&gif), Vec::new()).await?;
            notify_user(ctx, card(c.title, &body, Tone::Good, None)).await;
            return Ok(());
        }
        // …puis bascule sur le suivi silencieux normal. Aucune seconde
        // requête power n'est JAMAIS envoyée.
        logger::warn(fill(
            catalog.audit.power_send_error_polling,
            &[
                ("action", action.as_str()),
                ("alias", name.as_str()),
                ("error", err.message.as_str()),
            ],
        ));
    }

    logger::info(fill(
        catalog.audit.power_requested,
        &[
            ("action", action.as_str()),
            ("username", &ctx.author().name.clone()),
            ("userId", &logger::short_id(author_id.to_string())),
            ("alias", name.as_str()),
        ],
    ));

    // ── Suivi silencieux ──
    let ack_key = if send_error.is_some() {
        catalog.power.accepted_after_error
    } else {
        catalog.power.accepted
    };
    let ack_body = format!("{}{}", fill(ack_key.body, &[("cap", act.cap)]), eta_line(action, catalog));
    let waiting = assets::gif_image("waiting");

    // Bouton « Arrêter le suivi » : l'action reste irréversible (la requête
    // est déjà partie), on arrête seulement la surveillance.
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let uuid: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let cancel_id = format!("pw:{uuid}:cancel");
    let cancel_row = CreateActionRow::Buttons(vec![CreateButton::new(&cancel_id)
        .label(catalog.power.cancel_tracking)
        .style(ButtonStyle::Secondary)]);
    edit_card(ctx, card(ack_key.title, &ack_body, Tone::Wait, None), Some(&waiting), vec![cancel_row.clone()]).await?;

    // Collecteur du bouton d'annulation (même message, uniquement l'auteur).
    // `ComponentInteractionCollector` écoute la gateway via le shard ; on le
    // transforme en Stream pour pouvoir le mettre en course avec le sommeil
    // dans la boucle de suivi (`tokio::select!`).
    let mut collector = Box::pin(
        serenity::collector::ComponentInteractionCollector::new(&ctx.serenity_context().shard)
            .message_id(track_msg)
            .author_id(author_id)
            .timeout(TRACKING_TIMEOUT)
            .stream(),
    );

    let start = Instant::now();
    let mut iteration = 0usize;
    let mut transition = false;
    let mut stable: u32 = 0;
    let mut last_known = before.clone();
    let mut failures: u32 = 0;
    let mut cancelled = false;
    let mut collector_done = false;

    'tracking: while start.elapsed() < TRACKING_TIMEOUT {
        let delay = POLL_DELAYS[iteration.min(POLL_DELAYS.len() - 1)];
        iteration += 1;

        // Course entre le sommeil et le clic d'annulation : le premier des
        // deux futurs prêts gagne (équivalent du Promise.race du JS).
        if collector_done {
            tokio::time::sleep(delay).await;
        } else {
            tokio::select! {
                click = collector.next() => {
                    match click {
                        Some(click) => {
                            let _ = click.defer(ctx.http()).await;
                            cancelled = true;
                            break 'tracking;
                        }
                        None => {
                            // Le collecteur a expiré : on continue en
                            // polling pur jusqu'à la deadline.
                            collector_done = true;
                            continue;
                        }
                    }
                }
                _ = tokio::time::sleep(delay) => {}
            }
        }

        // ── Un tour de polling ──
        let current = match data.api.get(service_id, true).await {
            Ok(status) => {
                last_known = status.clone();
                failures = 0;
                status
            }
            Err(_) => {
                failures += 1;
                if failures >= MAX_FAILURES {
                    break 'tracking;
                }
                continue;
            }
        };

        if !online(&current) {
            transition = true;
        }
        let is_reached = reached(action, &current, &before, transition, false);
        stable = if is_reached { stable + 1 } else { 0 };
        if stable >= 2 {
            let seconds = start.elapsed().as_secs();
            logger::info(fill(
                catalog.audit.power_confirmed,
                &[
                    ("action", action.as_str()),
                    ("alias", name.as_str()),
                    ("provider", data.cfg.provider_label),
                    ("seconds", &seconds.to_string()),
                ],
            ));
            let tail = if action != Action::Stop {
                catalog.power.completed.tail
            } else {
                ""
            };
            let body = format!(
                "{}{}",
                fill(
                    catalog.power.completed.body,
                    &[
                        ("cap", act.cap),
                        ("provider", data.cfg.provider_label),
                        ("state", &state(&current).to_uppercase()),
                        ("seconds", &seconds.to_string()),
                    ],
                ),
                tail
            );
            stats::record(action.as_str(), action_start.elapsed().as_millis() as u64);
            let gif = assets::gif_image("success");
            edit_card(ctx, card(catalog.power.completed.title, &body, Tone::Good, None), Some(&gif), Vec::new()).await?;
            notify_user(ctx, card(catalog.power.completed.title, &body, Tone::Good, None)).await;
            return Ok(());
        }
    }

    // ── Sortie sans confirmation ──
    if cancelled {
        logger::info(fill(
            catalog.audit.power_tracking_cancelled,
            &[("action", action.as_str()), ("alias", name.as_str())],
        ));
        let c = catalog.power.tracking_cancelled;
        edit_card(ctx, card(c.title, c.body, Tone::Info, None), None, Vec::new()).await?;
        return Ok(());
    }

    logger::warn(fill(
        catalog.audit.power_not_confirmed,
        &[
            ("action", action.as_str()),
            ("alias", name.as_str()),
            ("failures", &failures.to_string()),
        ],
    ));
    let c = catalog.power.not_confirmed;
    let body_key = if had_send_error {
        c.body_transmit_error
    } else if failures >= MAX_FAILURES {
        c.body_failed
    } else {
        c.body_timeout
    };
    let body = fill(body_key, &[("state", &state(&last_known))]);
    let gif = assets::gif_image("error");
    edit_card(ctx, card(c.title, &body, Tone::Wait, None), Some(&gif), Vec::new()).await?;
    notify_user(ctx, card(c.title, &body, Tone::Wait, None)).await;
    Ok(())
}
