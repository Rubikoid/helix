use std::sync::Arc;

use arc_swap::ArcSwap;
use helix_event::AsyncHook;

use crate::config::Config;
use crate::events;
use crate::handlers::auto_save::AutoSaveHandler;
use crate::handlers::completion::CompletionHandler;
use crate::handlers::signature_help::SignatureHelpHandler;
use crate::{codestats, events};

pub use completion::trigger_auto_completion;
pub use helix_view::handlers::Handlers;

mod auto_save;
pub mod completion;
mod diagnostics;
mod signature_help;
mod snippet;

pub fn setup(config: Arc<ArcSwap<Config>>) -> Handlers {
    events::register();

    let completions = CompletionHandler::new(config.clone()).spawn();
    let signature_hints = SignatureHelpHandler::new().spawn();
    let auto_save = AutoSaveHandler::new().spawn();
    let codestats = codestats::CodeStatsHandler::new(config).spawn();

    let handlers = Handlers {
        completions,
        signature_hints,
        auto_save,
        codestats,
    };

    completion::register_hooks(&handlers);
    signature_help::register_hooks(&handlers);
    auto_save::register_hooks(&handlers);
    diagnostics::register_hooks(&handlers);
    snippet::register_hooks(&handlers);
    codestats::register_hooks(&handlers);

    handlers
}
