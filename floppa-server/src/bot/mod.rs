pub mod callback;
pub mod handlers;
pub mod i18n;
pub mod notifications;

use floppa_core::models::Lang;
use teloxide::prelude::*;
use teloxide::types::{MenuButton, WebAppInfo};
use tracing::{info, warn};
use url::Url;

/// Register the command menu in every language and point the chat menu button (next to the
/// message input) at the Mini App, so the app is one tap away instead of the commands list.
///
/// Best-effort and meant to be spawned after the HTTP listener is up: Telegram being slow or
/// unreachable at boot must not keep the API server down, and the menu is cosmetic — the
/// handlers work without it.
pub async fn configure_menu(bot: Bot, web_app_url: Option<Url>) {
    for lang in [Lang::En, Lang::Ru] {
        let commands = i18n::bot_commands(i18n::for_lang(lang));
        let mut request = bot.set_my_commands(commands);
        // English doubles as the fallback for clients in any other language.
        if lang != Lang::En {
            request = request.language_code(lang.to_string());
        }
        if let Err(e) = request.await {
            warn!("Could not register the {lang} bot command menu: {e}");
        }
    }
    info!("Bot command menu registered");

    if let Some(url) = web_app_url {
        let result = bot
            .set_chat_menu_button()
            .menu_button(MenuButton::WebApp {
                text: "Floppa VPN".to_string(),
                web_app: WebAppInfo { url },
            })
            .await;
        match result {
            Ok(_) => info!("Chat menu button set to Mini App"),
            Err(e) => warn!("Could not set the chat menu button: {e}"),
        }
    }
}
