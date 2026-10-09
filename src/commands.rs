//! Slash commands (framework poise) + handlers globaux.
//!
//! poise = framework de commandes construit sur serenity. Il remplace :
//! - `lib/commands.js` (définitions) ;
//! - `lib/interaction.js` (aiguillage + permissions + gestion d'erreurs) ;
//! - `lib/deploy.js` est porté séparément (hash + enregistrement groupé).
//!
//! Les données globales ([`Data`]) sont partagées entre TOUTES les
//! commandes via `Arc` (géré par poise) : config immuable, couche API
//! (cache + dédup), état (users/verrous/sessions) et catalogue i18n.

use poise::serenity_prelude as serenity;
use rand::RngCore;
use serenity::{ButtonStyle, CreateActionRow, CreateButton, CreateEmbed, CreateInteractionResponseFollowup, MessageId};
use std::sync::Arc;
use std::time::Duration;

use crate::api::Api;
use crate::config::Config;
use crate::embeds::{card, dashboard, Tone};
use crate::errors::{error_text, ErrorContext, ProviderError};
use crate::i18n::{fill, Catalog};
use crate::logger;
use crate::manage::{add, clear, list, remove};
use crate::members;
use crate::power::{self, Action};
use futures::StreamExt;
use crate::services::{display_name, pick_service};
use crate::state::State;
use crate::watchdog;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Context<'a> = poise::Context<'a, Data, Error>;

/// Données globales du bot.
///
/// `api` et `state` sont derrière des `Arc` : ils sont PARTAGÉS avec des
/// tâches détachées (le watchdog) qui vivent indépendamment des commandes.
/// `cfg` est immuable après le démarrage : pas de partage nécessaire au-delà
/// de la référence fournie par poise. `catalog` est une référence statique
/// COPIABLE (les catalogues sont compilés dans le binaire).
pub struct Data {
    pub cfg: Config,
    pub api: Arc<Api>,
    pub state: Arc<State>,
    pub catalog: &'static Catalog,
}

/// Réponse standard du bot : éphémère par défaut (poise), mentions
/// désactivées (le bot ne ping jamais personne, même en cas d'erreur).
pub fn reply(embed: CreateEmbed) -> poise::CreateReply {
    // `CreateAllowedMentions::new()` = aucune mention autorisée (liste
    // `parse` vide) : le bot ne ping jamais personne.
    poise::CreateReply::default()
        .embed(embed)
        .allowed_mentions(serenity::CreateAllowedMentions::new())
}

// ── Aides partagées par les commandes ──────────────────────────────────────

/// Contrôle d'accès opérateur : réponse « Accès refusé » si non autorisé.
async fn ensure_operator(ctx: &Context<'_>) -> Result<bool, Error> {
    let data = ctx.data();
    if data.state.is_allowed(&ctx.author().id.to_string()) {
        return Ok(true);
    }
    let c = data.catalog.common.access_denied_member;
    ctx.send(reply(card(c.title, c.body, Tone::Bad, None))).await?;
    Ok(false)
}

/// Résolution du service ciblé : option explicite → session utilisateur →
/// service par défaut. Gère aussi la session OBSOLÈTE (alias retiré de la
/// config entre-temps) avec purge + message explicite.
async fn resolve_target(ctx: &Context<'_>, service: Option<String>) -> Result<Option<(String, String)>, Error> {
    let data = ctx.data();
    let wanted = service.or_else(|| data.state.get_session(&ctx.author().id.to_string()));
    if let Some(alias) = &wanted {
        if !data.cfg.services.iter().any(|(a, _)| a == alias) {
            data.state.clear_session(&ctx.author().id.to_string());
            let c = data.catalog.common.session_reset;
            let body = fill(c.body, &[("alias", alias)]);
            ctx.send(reply(card(c.title, &body, Tone::Wait, None))).await?;
            return Ok(None);
        }
    }
    match pick_service(&data.cfg.services, wanted.as_deref()) {
        Ok((alias, id)) => Ok(Some((alias.to_string(), id.to_string()))),
        Err(message) => {
            let c = data.catalog.common.unknown_service;
            ctx.send(reply(card(c.title, &message, Tone::Bad, None))).await?;
            Ok(None)
        }
    }
}

// ── Autocomplétion des services ────────────────────────────────────────────
//
// Amélioration par rapport au JS : les choix statiques étaient plafonnés à
// 25 par Discord (les services surnuméraires devenaient invisibles).
// L'autocomplétion n'a PAS cette limite et filtre au fil de la frappe.

async fn service_autocomplete(ctx: Context<'_>, partial: &str) -> Vec<serenity::AutocompleteChoice> {
    let partial = partial.to_lowercase();
    ctx.data()
        .cfg
        .services
        .iter()
        .filter(|(alias, _)| alias.to_lowercase().contains(&partial))
        .map(|(alias, id)| serenity::AutocompleteChoice::new(format!("{alias} ({id})"), alias.clone()))
        .collect()
}

// ── Commandes ──────────────────────────────────────────────────────────────

/// 📊 Dashboard live du serveur.
#[poise::command(slash_command, ephemeral)]
pub async fn status(
    ctx: Context<'_>,
    #[description = "Target game server (defaults to your session service, then the primary one)"]
    #[autocomplete = "service_autocomplete"]
    service: Option<String>,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    if !ensure_operator(&ctx).await? {
        return Ok(());
    }
    let Some((alias, id)) = resolve_target(&ctx, service).await? else {
        return Ok(());
    };
    // Lecture non-fresh : le cache 5 s partagé avec le watchdog évite un
    // GET redondant si un scan vient de passer.
    let status = ctx.data().api.get(&id, false).await?;
    let embed = dashboard(&status, &alias, ctx.data().catalog);

    // Bouton « Rafraîchir » : re-lit l'état et réédite CE message — une
    // tâche détachée écoute les clics et expire après REFRESH_TIMEOUT.
    let refresh_id = random_hex(8);
    let row = CreateActionRow::Buttons(vec![CreateButton::new(&refresh_id)
        .label(ctx.data().catalog.status.refresh)
        .style(ButtonStyle::Secondary)]);
    let handle = ctx.send(reply(embed).components(vec![row])).await?;
    let message_id = handle.message().await?.id;
    spawn_refresh_button(&ctx, message_id, refresh_id, alias, id);
    Ok(())
}

/// Fenêtre de vie du bouton Rafraîchir : 10 min, sous la limite des 15 min
/// du token d'interaction (passée laquelle l'édition échouerait).
const REFRESH_TIMEOUT: Duration = Duration::from_secs(600);

/// Identifiant aléatoire de bouton (16 octets hex).
fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Tâche détachée du bouton Rafraîchir : chaque clic relit l'état et RÉÉDITE
/// le message éphémère via le TOKEN D'INTERACTION (`edit_followup`) — jamais
/// le token du bot (un éphémère lui est invisible, erreur 10008). À
/// l'expiration, le bouton est retiré (interaction morte).
fn spawn_refresh_button(
    ctx: &Context<'_>,
    message_id: MessageId,
    refresh_id: String,
    alias: String,
    service_id: String,
) {
    // Tout ce qui traverse la frontière de la tâche 'static est cloné ici.
    let http = ctx.serenity_context().http.clone();
    let shard = ctx.serenity_context().shard.clone();
    let interaction = match ctx {
        poise::Context::Application(app) => app.interaction.clone(),
        poise::Context::Prefix(_) => return,
    };
    let api = ctx.data().api.clone();
    let catalog = ctx.data().catalog;
    let author_id = ctx.author().id;

    tokio::spawn(async move {
        let mut collector = Box::pin(
            serenity::collector::ComponentInteractionCollector::new(&shard)
                .message_id(message_id)
                .author_id(author_id)
                .timeout(REFRESH_TIMEOUT)
                .stream(),
        );
        while let Some(click) = collector.next().await {
            // `defer` acquitte le clic (pas de « l'application n'a pas répondu »).
            let _ = click.defer(&http).await;
            let row = CreateActionRow::Buttons(vec![CreateButton::new(&refresh_id)
                .label(catalog.status.refresh)
                .style(ButtonStyle::Secondary)]);
            let builder = match api.get(&service_id, true).await {
                Ok(status) => {
                    CreateInteractionResponseFollowup::new()
                        .embed(dashboard(&status, &alias, catalog))
                        .components(vec![row])
                }
                Err(err) => {
                    // Lecture ratée : carte d'erreur dédiée, bouton retiré.
                    CreateInteractionResponseFollowup::new()
                        .embed(card(
                            catalog.common.generic_error.title,
                            &error_text(&err, ErrorContext::Other, catalog),
                            Tone::Bad,
                            None,
                        ))
                        .components(Vec::new())
                }
            };
            let _ = interaction.edit_followup(&http, message_id, builder).await;
        }
        // Expiration : on retire le bouton pour éviter un clic mort.
        let _ = interaction
            .edit_followup(
                &http,
                message_id,
                CreateInteractionResponseFollowup::new().components(Vec::new()),
            )
            .await;
    });
}

/// ▶️ Démarre le serveur (avec confirmation et suivi).
#[poise::command(slash_command, ephemeral)]
pub async fn start(
    ctx: Context<'_>,
    #[description = "Target game server (defaults to your session service, then the primary one)"]
    #[autocomplete = "service_autocomplete"]
    service: Option<String>,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    if !ensure_operator(&ctx).await? {
        return Ok(());
    }
    let Some((alias, _)) = resolve_target(&ctx, service).await? else {
        return Ok(());
    };
    power::power(ctx, Action::Start, Some(alias)).await
}

/// ⏹️ Arrête le serveur (avec confirmation et suivi).
#[poise::command(slash_command, ephemeral)]
pub async fn stop(
    ctx: Context<'_>,
    #[description = "Target game server (defaults to your session service, then the primary one)"]
    #[autocomplete = "service_autocomplete"]
    service: Option<String>,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    if !ensure_operator(&ctx).await? {
        return Ok(());
    }
    let Some((alias, _)) = resolve_target(&ctx, service).await? else {
        return Ok(());
    };
    power::power(ctx, Action::Stop, Some(alias)).await
}

/// 🔁 Redémarre le serveur (avec confirmation et suivi).
#[poise::command(slash_command, ephemeral)]
pub async fn restart(
    ctx: Context<'_>,
    #[description = "Target game server (defaults to your session service, then the primary one)"]
    #[autocomplete = "service_autocomplete"]
    service: Option<String>,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    if !ensure_operator(&ctx).await? {
        return Ok(());
    }
    let Some((alias, _)) = resolve_target(&ctx, service).await? else {
        return Ok(());
    };
    power::power(ctx, Action::Restart, Some(alias)).await
}

/// 🏓 Mesure la latence aller-retour vers l'API du provider.
#[poise::command(slash_command, ephemeral)]
pub async fn ping(
    ctx: Context<'_>,
    #[description = "Target game server (defaults to your session service, then the primary one)"]
    #[autocomplete = "service_autocomplete"]
    service: Option<String>,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    if !ensure_operator(&ctx).await? {
        return Ok(());
    }
    let Some((alias, id)) = resolve_target(&ctx, service).await? else {
        return Ok(());
    };
    let catalog = ctx.data().catalog;
    // fresh : ne pas fausser la mesure avec le cache 5 s.
    let start = std::time::Instant::now();
    match ctx.data().api.get(&id, true).await {
        Ok(status) => {
            let ms = start.elapsed().as_millis();
            let tone = if ms < 300 {
                Tone::Good
            } else if ms < 1000 {
                Tone::Wait
            } else {
                Tone::Bad
            };
            let body = fill(
                catalog.ping.pong.body,
                &[("alias", &display_name(&status, &alias)), ("ms", &ms.to_string())],
            );
            ctx.send(reply(card(catalog.ping.pong.title, &body, tone, None))).await?;
        }
        Err(err) => {
            let status_part = err
                .status
                .map(|s| fill(catalog.ping.failed.status_suffix, &[("status", &s.to_string())]))
                .unwrap_or_default();
            // Nom réel en repli depuis le cache : le fetch vient d'échouer.
            let cached_name = ctx
                .data()
                .api
                .get_cached(&id)
                .await
                .map(|s| display_name(&s, &alias))
                .unwrap_or_else(|| alias.clone());
            let body = fill(
                catalog.ping.failed.body,
                &[("alias", &cached_name), ("statusPart", &status_part)],
            );
            ctx.send(reply(card(catalog.ping.failed.title, &body, Tone::Bad, None))).await?;
        }
    }
    Ok(())
}

/// 🎯 Définit le serveur par défaut pour la session de l'utilisateur.
#[poise::command(slash_command, ephemeral)]
pub async fn server(
    ctx: Context<'_>,
    #[description = "Game server to use by default"]
    #[autocomplete = "service_autocomplete"]
    service: String,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    if !ensure_operator(&ctx).await? {
        return Ok(());
    }
    let data = ctx.data();
    if !data.cfg.services.iter().any(|(alias, _)| alias == &service) {
        let c = data.catalog.common.unknown_service;
        let body = fill(c.body, &[("alias", &service)]);
        ctx.send(reply(card(c.title, &body, Tone::Bad, None))).await?;
        return Ok(());
    }
    data.state.set_session(&ctx.author().id.to_string(), &service);
    let c = data.catalog.common.default_service;
    let body = fill(c.body, &[("alias", &service)]);
    ctx.send(reply(card(c.title, &body, Tone::Info, None))).await?;
    Ok(())
}

/// 👑 Gestion des opérateurs (sous-commandes dans `manage.rs`).
#[poise::command(slash_command, ephemeral, subcommands("add", "remove", "list", "clear"))]
pub async fn users(_ctx: Context<'_>) -> Result<(), Error> {
    // Jamais appelée directement : poise aiguille vers les sous-commandes.
    Ok(())
}

/// 📜 Dernières lignes de log (owner uniquement).
#[poise::command(slash_command, ephemeral)]
pub async fn logs(
    ctx: Context<'_>,
    #[description = "Only show lines containing this text"] filter: Option<String>,
) -> Result<(), Error> {
    crate::logs_cmd::logs(&ctx, filter).await
}

/// ❓ Rappel des commandes et permissions.
#[poise::command(slash_command, ephemeral)]
pub async fn help(ctx: Context<'_>) -> Result<(), Error> {
    let c = ctx.data().catalog.help;
    ctx.send(reply(card(c.title, c.body, Tone::Info, None))).await?;
    Ok(())
}

// ── Liste des commandes (ordre d'enregistrement Discord) ───────────────────

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![
        status(),
        start(),
        stop(),
        restart(),
        ping(),
        server(),
        users(),
        logs(),
        help(),
    ]
}

/// Événement `Ready` : bannière de démarrage (console + log), préchargement
/// des membres (politique de cooldown de `members.rs`) et démarrage du
/// watchdog — même séquence que le `clientReady` du JS.
pub async fn on_ready(
    ctx: &serenity::Context,
    event: &serenity::FullEvent,
    _framework: poise::FrameworkContext<'_, Data, Error>,
    data: &Data,
) {
    let serenity::FullEvent::Ready { data_about_bot } = event else {
        return;
    };
    logger::banner(&[
        "🎮  APABOT • GAME CONTROL".to_string(),
        format!("👤  {}", data_about_bot.user.tag()),
        format!(
            "🎯  services: {}",
            data.cfg
                .services
                .iter()
                .map(|(alias, _)| alias.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        format!("🌐  language: {}", data.cfg.language),
        format!(
            "💻  rust {}  •  pid {}",
            env!("CARGO_PKG_VERSION"),
            std::process::id()
        ),
        format!("📅  {}", chrono::Utc::now().format("%Y-%m-%d %H:%M:%S")),
    ]);

    // Précharge les membres de la guilde (menu /users) en respectant le
    // cooldown opcode 8 de Discord (politique centralisée dans members.rs).
    if let Ok(guild_id) = data.cfg.guild_id.parse::<u64>() {
        let _ = members::refresh_members(ctx, serenity::GuildId::new(guild_id), &data.cfg.token).await;
    }

    // Watchdog : tâche détachée qui vit avec le process. Le handler ne
    // reçoit qu'une RÉFÉRENCE vers les données — on clone les `Arc` internes
    // pour transférer la propriété à la tâche.
    watchdog::start(
        ctx.http.clone(),
        data.cfg.clone(),
        data.api.clone(),
        data.state.clone(),
    );
}

// ── Hook global : guilde configurée uniquement ─────────────────────────────

/// Refuse toute interaction hors de la guilde configurée (exécuté avant
/// CHAQUE commande, y compris /help — même politique que le JS).
/// `command_check` renvoie `Ok(bool)` : `false` annule l'exécution de la
/// commande SANS erreur (le framework s'arrête proprement).
pub fn command_check(ctx: Context<'_>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<bool, Error>> + Send + '_>> {
    let expected = ctx.data().cfg.guild_id.clone();
    let catalog = ctx.data().catalog;
    Box::pin(async move {
        let ok = ctx.guild_id().map(|g| g.to_string() == expected).unwrap_or(false);
        if !ok {
            let c = catalog.common.access_denied_guild;
            let _ = ctx.send(reply(card(c.title, c.body, Tone::Bad, None))).await;
        }
        Ok(ok)
    })
}

// ── Gestion d'erreurs centralisée ──────────────────────────────────────────

/// Équivalent du try/catch de `interaction.js` : chaque cause produit un
/// message DÉDIÉ (401 clé API, 404 ID de service, timeout, réseau…) au lieu
/// d'un « vérifiez votre clé API » fourre-tout.
pub async fn handle_error(err: poise::FrameworkError<'_, Data, Error>) {
    match err {
        poise::FrameworkError::Command { error, ctx, .. } => {
            let catalog = ctx.data().catalog;
            let context = match ctx.command().name.as_str() {
                "start" | "stop" | "restart" => ErrorContext::Power,
                _ => ErrorContext::Other,
            };
            let (title, body) = match error.downcast_ref::<ProviderError>() {
                Some(provider_error) => (
                    catalog.common.generic_error.title,
                    error_text(provider_error, context, catalog),
                ),
                None => (catalog.common.generic_error.title, error.to_string()),
            };
            logger::error(format!("Request failure: {body}"));
            if let Err(reply_error) = ctx.send(reply(card(title, &body, Tone::Bad, None))).await {
                logger::error(format!("Could not update Discord response: {reply_error}"));
            }
        }
        poise::FrameworkError::CommandPanic { payload, ctx, .. } => {
            // poise contient le panic (catch_unwind) : le bot survit. On
            // journalise le détail et on montre une carte d'erreur — sans
            // quoi l'interaction différée resterait « en cours » pour
            // toujours.
            let catalog = ctx.data().catalog;
            let detail = payload.as_deref().unwrap_or("non-string panic payload");
            logger::error(format!("Command panic: {detail}"));
            let _ = ctx
                .send(reply(card(
                    catalog.common.generic_error.title,
                    catalog.errors.internal_panic,
                    Tone::Bad,
                    None,
                )))
                .await;
        }
        other => {
            logger::error(format!("Framework error: {other}"));
        }
    }
}
