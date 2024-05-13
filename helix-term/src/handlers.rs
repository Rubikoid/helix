use std::sync::Arc;

use arc_swap::ArcSwap;
use helix_event::AsyncHook;

use crate::config::Config;
use crate::handlers::completion::CompletionHandler;
use crate::handlers::signature_help::SignatureHelpHandler;
use crate::{codestats, events};

pub use completion::trigger_auto_completion;
pub use helix_view::handlers::Handlers;

mod completion;
mod signature_help;

pub fn setup(config: Arc<ArcSwap<Config>>) -> Handlers {
    events::register();

    let completions = CompletionHandler::new(config.clone()).spawn();
    let signature_hints = SignatureHelpHandler::new().spawn();
    let codestats = codestats::CodeStatsHandler::new(config).spawn();

    let handlers = Handlers {
        completions,
        signature_hints,
        codestats,
    };

    completion::register_hooks(&handlers);
    signature_help::register_hooks(&handlers);
    codestats::register_hooks(&handlers);

    handlers
}
