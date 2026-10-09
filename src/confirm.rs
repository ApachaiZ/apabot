//! Double confirmation par boutons (portage de `lib/confirm.js`).
//!
//! Toute action destructive exige un clic de validation. Le motif Discord
//! est toujours le même :
//! 1. afficher la carte de confirmation sur la RÉPONSE ORIGINALE de
//!    l'interaction de commande (le placeholder du `defer_ephemeral`) ;
//! 2. ouvrir un **collecteur** (`ComponentCollector`) filtré sur l'auteur et
//!    les IDs de boutons, avec un délai ;
//! 3. au clic : `defer` (on acquitte l'interaction), puis on REMPLACE la
//!    carte — la carte « Traitement » porte immédiatement le GIF `loading`,
//!    visible pendant TOUTE l'attente (lecture d'état + POST power), pas
//!    seulement entre deux étapes.
//!
//! ⚠️ Point Discord critique (et cause historique d'un bug 10008
//! « Unknown Message ») : une réponse éphémère n'est PAS accessible via le
//! token du bot (`PATCH /channels/…` échoue). Toute édition doit passer par
//! le TOKEN DE L'INTERACTION de commande — [`edit_original`] — valable
//! 15 minutes, soit l'équivalent exact du `interaction.editReply` du JS.
//! Le cycle complet tient largement dans la fenêtre : 60 s de confirmation
//! + 10 min de suivi maximum.

use poise::serenity_prelude as serenity;
use rand::RngCore;
use serenity::{
    Builder, ButtonStyle, CreateActionRow, CreateButton, CreateEmbed, EditInteractionResponse,
    Message, MessageId,
};
use std::time::Duration;

use crate::commands::{Context, Error};
use crate::embeds::{card, Tone};
use crate::assets;

pub const CONFIRM_TIMEOUT: Duration = Duration::from_secs(60);

/// Identifiant de bouton aléatoire (16 octets hex). Le préfixe `cnf:` rend
/// les logs d'interaction Discord plus lisibles.
fn button_id(prefix: &str, suffix: &str) -> String {
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("{prefix}:{hex}:{suffix}")
}

/// Token de l'interaction de COMMANDE — le sésame d'édition de l'éphémère.
/// `poise` ne l'expose pas par une méthode : on lit le champ public de
/// `ApplicationContext` (le bot n'utilise que des commandes slash).
fn interaction_token(ctx: &Context<'_>) -> String {
    match ctx {
        poise::Context::Application(app) => app.interaction.token.clone(),
        poise::Context::Prefix(_) => String::new(),
    }
}

/// Édite la réponse originale de l'interaction (la carte éphémère) via le
/// token d'interaction, et retourne le message édité (son id alimente le
/// collecteur de boutons). C'est le SEUL chemin d'édition autorisé pour
/// l'éphémère — jamais `Message::edit` (token du bot).
pub async fn edit_original(
    ctx: &Context<'_>,
    builder: EditInteractionResponse,
) -> Result<Message, Error> {
    let token = interaction_token(ctx);
    // `Builder::execute` = PATCH /webhooks/{app}/{token}/messages/@original,
    // en gérant lui-même les pièces jointes (multipart) du builder.
    let msg = builder.execute(ctx.http(), token.as_str()).await?;
    Ok(msg)
}

/// Affiche la carte de confirmation et attend un clic.
///
/// Retourne `(id_du_message, confirmé)` : l'id permet à l'appelant de
/// poursuivre les éditions (loader de transmission, suivi…) sur le MÊME
/// message — via [`edit_original`], jamais via `Message::edit`. En cas
/// d'annulation ou d'expiration, la carte finale (« Annulé » / « Expiré »)
/// a déjà été posée.
pub async fn confirm(ctx: &Context<'_>, embed: CreateEmbed) -> Result<(MessageId, bool), Error> {
    let catalog = ctx.data().catalog;
    let yes_id = button_id("cnf", "yes");
    let no_id = button_id("cnf", "no");

    let row = CreateActionRow::Buttons(vec![
        CreateButton::new(&yes_id)
            .label(catalog.confirm.confirm)
            .style(ButtonStyle::Danger),
        CreateButton::new(&no_id)
            .label(catalog.confirm.cancel)
            .style(ButtonStyle::Secondary),
    ]);

    // La carte remplace le placeholder du defer : une seule réponse éditée
    // du début à la fin (parité avec le editReply du JS).
    let msg = edit_original(
        ctx,
        EditInteractionResponse::new()
            .embed(embed)
            .components(vec![row])
            .allowed_mentions(serenity::CreateAllowedMentions::new()),
    )
    .await?;

    // Collecteur : uniquement l'auteur, uniquement ce message, 60 s.
    // (`ComponentInteractionCollector::new` attend un `ShardMessenger` :
    // le `Context` serenity l'expose via son champ `shard`.)
    let author_id = ctx.author().id;
    let collector = serenity::collector::ComponentInteractionCollector::new(&ctx.serenity_context().shard)
        .message_id(msg.id)
        .author_id(author_id)
        .timeout(CONFIRM_TIMEOUT);

    match collector.next().await {
        Some(click) => {
            // `defer` = acquittement silencieux (équivalent deferUpdate du JS) :
            // le clic ne laisse pas « l'application n'a pas répondu ».
            let _ = click.defer(ctx.http()).await;
            let clean = EditInteractionResponse::new().components(Vec::new());
            if click.data.custom_id == no_id {
                let c = catalog.confirm.cancelled;
                edit_original(ctx, clean.embed(card(c.title, c.body, Tone::Info, None))).await?;
                Ok((msg.id, false))
            } else {
                let c = catalog.confirm.processing;
                // Le GIF `loading` arrive ICI, dès le clic : l'utilisateur
                // le voit pendant toute l'attente (lecture d'état fraîche,
                // POST power…), pas seulement pendant la transmission.
                let loading = assets::gif_image("loading");
                let mut embed = card(c.title, c.body, Tone::Wait, None);
                let mut builder = clean;
                if let (Some(url), Some(attachment)) =
                    (loading.image_url.as_deref(), loading.attachment.as_ref())
                {
                    embed = embed.image(url);
                    builder = builder.new_attachment(attachment.clone());
                }
                edit_original(ctx, builder.embed(embed)).await?;
                Ok((msg.id, true))
            }
        }
        None => {
            // Expiration : on retire les boutons pour éviter un clic sur une
            // interaction morte.
            let c = catalog.confirm.expired;
            edit_original(
                ctx,
                EditInteractionResponse::new()
                    .components(Vec::new())
                    .embed(card(c.title, c.body, Tone::Wait, None)),
            )
            .await?;
            Ok((msg.id, false))
        }
    }
}
