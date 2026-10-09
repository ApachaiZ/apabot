//! Commande `/users` (owner uniquement) — portage de `lib/manage.js`.
//!
//! add / remove : l'option « membre » porte l'ID du membre choisi parmi les
//! suggestions filtrables produites par le BOT (autocomplétion) — pas le
//! sélecteur de Discord, non filtrable. Suggestions : add = uniquement les
//! non-opérateurs, remove = uniquement les opérateurs.
//!
//! list : pseudos affichés plutôt que des IDs bruts (plus lisible), paginé
//! par blocs de 20, éphémère.
//!
//! clear : confirmation puis révocation de TOUS les opérateurs (l'owner est
//! conservé — il est défini par la config et ne peut pas être révoqué).

use poise::serenity_prelude as serenity;
use serenity::UserId;

use crate::commands::{reply, Context, Error};
use crate::confirm;
use crate::embeds::{card, Tone};
use crate::i18n::fill;
use crate::logger;
use crate::members;

/// Pseudo Discord d'un membre, ou `None` s'il n'est plus joignable (a quitté
/// le serveur, cache incomplet…). L'appelant retombe alors sur l'ID.
async fn member_name(ctx: &Context<'_>, id: &str) -> Option<String> {
    let guild_id = ctx.guild_id()?;
    let member = guild_id.member(ctx.http(), UserId::new(id.parse().ok()?)).await.ok()?;
    Some(member.user.name)
}

/// Autocomplétion de l'option « membre » (add → non-opérateurs,
/// remove → opérateurs).
async fn member_autocomplete(
    ctx: Context<'_>,
    partial: &str,
) -> Vec<serenity::AutocompleteChoice> {
    let subcommand = ctx.command().name.clone();
    members::member_choices(&ctx, partial, &subcommand)
        .await
        .unwrap_or_default()
}

/// Vérification owner (réponse « Accès refusé » sinon). Retourne false si
/// l'appel doit s'arrêter.
async fn ensure_owner(ctx: &Context<'_>) -> Result<bool, Error> {
    let data = ctx.data();
    if data.state.is_owner(&ctx.author().id.to_string()) {
        return Ok(true);
    }
    let c = data.catalog.manage.access_denied;
    ctx.send(reply(card(c.title, c.body, Tone::Bad, None))).await?;
    Ok(false)
}

/// ✅ Autorise un membre.
#[poise::command(slash_command, ephemeral)]
pub async fn add(
    ctx: Context<'_>,
    #[description = "Member to authorize (type to search)"]
    #[autocomplete = "member_autocomplete"]
    member: String,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    if !ensure_owner(&ctx).await? {
        return Ok(());
    }
    let data = ctx.data();
    let catalog = data.catalog;
    let target_id = member;
    let Some(guild_id) = ctx.guild_id() else {
        return Ok(());
    };

    // Validation de la sélection.
    let member_obj = match guild_id.member(ctx.http(), UserId::new(target_id.parse()?)).await {
        Ok(m) => m,
        Err(_) => {
            let c = catalog.manage.invalid;
            ctx.send(reply(card(c.title, c.body, Tone::Bad, None))).await?;
            return Ok(());
        }
    };
    if member_obj.user.bot || data.state.is_owner(&target_id) {
        let c = catalog.manage.invalid;
        ctx.send(reply(card(c.title, c.body, Tone::Bad, None))).await?;
        return Ok(());
    }
    let users = data.state.get_users();
    if users.contains(&target_id) {
        let c = catalog.manage.no_change;
        ctx.send(reply(card(c.title, c.body_add, Tone::Info, None))).await?;
        return Ok(());
    }

    // Confirmation du changement d'accès.
    let confirm_body = fill(
        catalog.manage.confirm.body,
        &[
            ("verb", catalog.manage.confirm.verb_add),
            ("username", &member_obj.user.name),
        ],
    );
    let confirmed = confirm::confirm(&ctx, card(catalog.manage.confirm.title, &confirm_body, Tone::Wait, None)).await?.1;
    if !confirmed {
        return Ok(());
    }

    // Re-vérification : le membre doit toujours exister et être humain.
    let still = guild_id.member(ctx.http(), UserId::new(target_id.parse()?)).await;
    if still.map(|m| m.user.bot).unwrap_or(true) {
        return Err("Selected member no longer eligible".into());
    }

    // Sauvegarde chiffrée + mémoire.
    let mut next = users;
    next.push(target_id.clone());
    next.sort();
    next.dedup();
    data.state.save_users(next);

    logger::info(fill(
        catalog.audit.operator_granted,
        &[
            ("username", &member_obj.user.name),
            ("target", &logger::short_id(&target_id)),
            ("ownerId", &logger::short_id(ctx.author().id.to_string())),
        ],
    ));
    let c = catalog.manage.done;
    let body = fill(c.body_add, &[("username", &member_obj.user.name)]);
    ctx.send(reply(card(c.title, &body, Tone::Good, None))).await?;
    Ok(())
}

/// ❌ Révoque un membre.
#[poise::command(slash_command, ephemeral)]
pub async fn remove(
    ctx: Context<'_>,
    #[description = "Member to revoke (type to search)"]
    #[autocomplete = "member_autocomplete"]
    member: String,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    if !ensure_owner(&ctx).await? {
        return Ok(());
    }
    let data = ctx.data();
    let catalog = data.catalog;
    let target_id = member;
    let Some(guild_id) = ctx.guild_id() else {
        return Ok(());
    };

    let member_obj = match guild_id.member(ctx.http(), UserId::new(target_id.parse()?)).await {
        Ok(m) => m,
        Err(_) => {
            let c = catalog.manage.invalid;
            ctx.send(reply(card(c.title, c.body, Tone::Bad, None))).await?;
            return Ok(());
        }
    };
    if member_obj.user.bot || data.state.is_owner(&target_id) {
        let c = catalog.manage.invalid;
        ctx.send(reply(card(c.title, c.body, Tone::Bad, None))).await?;
        return Ok(());
    }
    let users = data.state.get_users();
    if !users.contains(&target_id) {
        let c = catalog.manage.no_change;
        ctx.send(reply(card(c.title, c.body_remove, Tone::Info, None))).await?;
        return Ok(());
    }

    let confirm_body = fill(
        catalog.manage.confirm.body,
        &[
            ("verb", catalog.manage.confirm.verb_remove),
            ("username", &member_obj.user.name),
        ],
    );
    let confirmed = confirm::confirm(&ctx, card(catalog.manage.confirm.title, &confirm_body, Tone::Wait, None)).await?.1;
    if !confirmed {
        return Ok(());
    }

    let next: Vec<String> = users.into_iter().filter(|id| id != &target_id).collect();
    data.state.save_users(next);

    logger::info(fill(
        catalog.audit.operator_revoked,
        &[
            ("username", &member_obj.user.name),
            ("target", &logger::short_id(&target_id)),
            ("ownerId", &logger::short_id(ctx.author().id.to_string())),
        ],
    ));
    let c = catalog.manage.done;
    let body = fill(c.body_remove, &[("username", &member_obj.user.name)]);
    ctx.send(reply(card(c.title, &body, Tone::Good, None))).await?;
    Ok(())
}

/// 📋 Liste les opérateurs autorisés (paginé par 20).
#[poise::command(slash_command, ephemeral)]
pub async fn list(ctx: Context<'_>) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    if !ensure_owner(&ctx).await? {
        return Ok(());
    }
    let data = ctx.data();
    let catalog = data.catalog;
    let users = data.state.get_users();

    let owner = member_name(&ctx, &data.cfg.owner_id).await.unwrap_or_else(|| data.cfg.owner_id.clone());
    let header = fill(catalog.manage.list.header, &[("owner", &owner)]);

    let mut entries = Vec::new();
    for id in &users {
        let name = member_name(&ctx, id).await.unwrap_or_else(|| id.clone());
        entries.push(fill(catalog.manage.list.entry, &[("name", &name)]));
    }

    const PAGE_SIZE: usize = 20;
    let mut pages: Vec<Vec<String>> = entries
        .chunks(PAGE_SIZE)
        .map(|chunk| chunk.to_vec())
        .collect();
    if pages.is_empty() {
        pages.push(Vec::new());
    }
    let total_pages = pages.len();
    for (page_index, page) in pages.into_iter().enumerate() {
        let body = format!("{header}\n{}", page.join("\n"));
        let title = if total_pages > 1 {
            fill(
                catalog.manage.list.title_many,
                &[
                    ("page", &(page_index + 1).to_string()),
                    ("totalPages", &total_pages.to_string()),
                ],
            )
        } else {
            catalog.manage.list.title_one.to_string()
        };
        if page_index == 0 {
            ctx.send(reply(card(&title, &body, Tone::Info, None))).await?;
        } else {
            // Suites éphémères EXPLICITES : en poise, les followups ne sont
            // pas éphémères par défaut.
            let followup = poise::CreateReply::default()
                .embed(card(&title, &body, Tone::Info, None))
                .ephemeral(true)
                .allowed_mentions(serenity::CreateAllowedMentions::new());
            ctx.send(followup).await?;
        }
    }
    Ok(())
}

/// 🧹 Révoque tous les opérateurs (owner conservé).
#[poise::command(slash_command, ephemeral)]
pub async fn clear(ctx: Context<'_>) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    if !ensure_owner(&ctx).await? {
        return Ok(());
    }
    let data = ctx.data();
    let catalog = data.catalog;

    let confirmed = confirm::confirm(
        &ctx,
        card(catalog.manage.clear.confirm_title, catalog.manage.clear.confirm_body, Tone::Wait, None),
    )
    .await?
    .1;
    if !confirmed {
        return Ok(());
    }

    data.state.save_users(Vec::new());
    logger::info(fill(
        catalog.audit.operators_cleared,
        &[("ownerId", &logger::short_id(ctx.author().id.to_string()))],
    ));
    let c = catalog.manage.clear;
    ctx.send(reply(card(c.done_title, c.done_body, Tone::Good, None))).await?;
    Ok(())
}
