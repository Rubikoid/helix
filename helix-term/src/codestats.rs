use chrono::{DateTime, Local};
use helix_core::{Range, Selection, SmallVec, Tendril, Transaction};
use once_cell::sync::Lazy;
use std::{
    borrow::Cow,
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::Instant;
use ureq::Agent;

use helix_view::{
    codestats::CodeStatsEvent,
    events::{DocumentDidChange, Quit},
    Document,
};

use crate::{compositor, config::Config as GlobalConfig, events::PostInsertChar, ui::PromptEvent};
use arc_swap::ArcSwap;
use helix_event::{register_hook, send_blocking, send_blocking_freezing, CancelTx};
use helix_view::handlers::Handlers;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default, deny_unknown_fields)]
pub struct Config {
    pub server: String,
    pub key: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: "https://codestats.net/".to_string(),
            key: None,
        }
    }
}

#[derive(Debug)]
pub(super) struct CodeStatsHandler {
    trigger: Option<CodeStatsEvent>,
    request: Option<CancelTx>,
    config: Arc<ArcSwap<GlobalConfig>>,
    agent: Agent,
    last_send: DateTime<Local>,
}

impl CodeStatsHandler {
    pub fn new(config: Arc<ArcSwap<GlobalConfig>>) -> CodeStatsHandler {
        let agent = ureq::AgentBuilder::new()
            .timeout_read(Duration::from_secs(5))
            .timeout_write(Duration::from_secs(5))
            .https_only(true)
            .user_agent("Helix/1.0")
            .build();

        let local_time = Local::now();

        CodeStatsHandler {
            trigger: None,
            request: None,
            config: config,
            agent: agent,
            last_send: local_time,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct CodeStatsPulseXP {
    language: String,
    xp: u32,
}

#[derive(Serialize, Deserialize)]
struct CodeStatsPulse {
    coded_at: String, // or dt
    xps: Vec<CodeStatsPulseXP>,
}

impl CodeStatsHandler {
    fn should_send(&self, now: &DateTime<Local>) -> bool {
        (*now - self.last_send).num_seconds() > 10
    }
}

impl helix_event::AsyncHook for CodeStatsHandler {
    type Event = CodeStatsEvent;

    fn handle_event(
        &mut self,
        event: Self::Event,
        timeout: Option<tokio::time::Instant>,
    ) -> Option<tokio::time::Instant> {
        /*
            Логика работы

            Если прилетел ForceSend - надо отправить rightnow, без дебаунса.
            Если прилетел Update - надо зашэдулить на N секунд.
        */

        // match self.trigger {
        //     Some(_) => {}
        //     None => self.trigger = Some(event),
        // }

        // if self
        //     .trigger
        //     .as_ref()
        //     .is_some_and(|trigger| *trigger == event)
        // {
        //     timeout
        // } else {
        match event {
            CodeStatsEvent::Update => {
                self.trigger = Some(event);
                Some(Instant::now() + Duration::from_secs(10))
            }
            CodeStatsEvent::ForceSend => {
                self.trigger = Some(event);
                self.finish_debounce();
                None
            }
            CodeStatsEvent::Cancel => {
                self.trigger = None;
                None
            }
        }
        // }
    }

    fn finish_debounce(&mut self) {
        let trigger = self.trigger.take().expect("debounce always has a trigger");

        // pull actual config
        let cfg = &self.config.load_full().codestats;

        // check there a key in config
        let Some(key) = &cfg.key else {
            return;
        };

        // copy xps array
        let local_xps = {
            let mut xps_ptr = XPS.lock().expect("why i cant lock XPS...");

            // if no data -> return
            if xps_ptr.is_empty() {
                return;
            }

            // copy dict
            let xps_copy = xps_ptr.clone();

            // and clean it
            xps_ptr.clear();
            xps_copy
        };

        let now = Local::now();

        // check now, if it should not be sended AND trigger is not ForceSend...
        if !self.should_send(&now) && trigger != CodeStatsEvent::ForceSend {
            return;
        }

        // build request
        let pulse = CodeStatsPulse {
            coded_at: now.to_rfc3339(),
            xps: local_xps
                .into_iter()
                .map(|(lang, xp)| CodeStatsPulseXP {
                    language: lang,
                    xp: xp,
                })
                .collect(),
        };

        // just for debbuging ;)
        let j = serde_json::to_string(&pulse).unwrap();

        // send request
        let path = format!("{0}api/my/pulses", cfg.server);
        let resp = self
            .agent
            .post(&path)
            .set("X-API-Token", key)
            .send_json(pulse);

        match resp {
            Ok(x) => match x.into_string() {
                Ok(data) => log::info!("Sended {j:#?} ok: {data:#?}"),
                Err(x) => log::warn!("Reading server resp for {j:#?} error: {x:#?}"),
            },
            Err(x) => log::warn!("Sending data {j:#?} error: {x:#?}"),
        }

        // update last send
        self.last_send = now;
    }
}

pub fn resolve_language(doc: &Document) -> Option<String> {
    let raw_language = &doc.language;

    // get language code
    match raw_language {
        Some(raw_language) => match &raw_language.codestats_language {
            Some(language) => Some(language.clone()),
            None => {
                log::warn!("Language error: no lang def: {0}", raw_language.language_id);
                return None;
            }
        },
        None => {
            log::warn!("Language error: no lang obj");
            Some("Plain text".to_string())
        }
    }
}

pub static XPS: Lazy<Mutex<HashMap<String, u32>>> = Lazy::new(|| Mutex::new(HashMap::new()));

#[inline]
pub fn add_xp(language: String, diff: u32) {
    *XPS.lock().unwrap().entry(language).or_insert(0) += diff;
}

pub fn xp_empty() -> bool {
    XPS.lock().unwrap().is_empty()
}

pub fn count_total_xp() -> u32 {
    XPS.lock().unwrap().values().sum()
}

pub fn register_hooks(handlers: &Handlers) {
    log::info!("CodeStats hook registred");

    let tx = handlers.codestats.clone();
    register_hook!(move |event: &mut DocumentDidChange<'_>| {
        // let old = &event.old_text;
        // let new = &event.doc.text();
        // log::info!("document changed: new is {0:#?} than old", new.cmp(old));

        let Some(language) = resolve_language(event.doc) else {
            return anyhow::Ok(());
        };

        add_xp(language, 1);

        send_blocking_freezing(&tx, CodeStatsEvent::Update);

        anyhow::Ok(())
    });

    let tx = handlers.codestats.clone();
    register_hook!(move |event: &mut Quit| {
        send_blocking_freezing(&tx, CodeStatsEvent::ForceSend);
        anyhow::Ok(())
    });
}

pub fn typeablecmd_get_info(
    cx: &mut compositor::Context,
    args: &[Cow<str>],
    event: PromptEvent,
) -> anyhow::Result<()> {
    let config = cx.editor.config();
    let (view, doc) = current!(cx.editor);

    let mut data: Tendril = Tendril::from("C::S info:\n");
    for (lang, count) in XPS.lock().unwrap().iter() {
        data.push_str(format!("Lang: {0}, count: {1}\n", lang, count).as_str());
    }
    data.push_str("C::S info end\n");

    let selection = doc.selection(view.id);
    let mut changes = Vec::with_capacity(selection.len());
    let mut ranges = SmallVec::with_capacity(selection.len());

    let output_len = data.chars().count();
    let mut offset = 0isize;

    for range in selection.ranges() {
        let (from, to, deleted_len) = (range.to(), range.to(), 0);

        // These `usize`s cannot underflow because selection ranges cannot overlap.
        let anchor = to
            .checked_add_signed(offset)
            .expect("Selection ranges cannot overlap")
            .checked_sub(deleted_len)
            .expect("Selection ranges cannot overlap");
        let new_range = Range::new(anchor, anchor + output_len).with_direction(range.direction());
        ranges.push(new_range);
        offset = offset
            .checked_add_unsigned(output_len)
            .expect("Selection ranges cannot overlap")
            .checked_sub_unsigned(deleted_len)
            .expect("Selection ranges cannot overlap");

        changes.push((from, to, Some(data.clone())));
        break;
    }

    let transaction = Transaction::change(doc.text(), changes.into_iter())
        .with_selection(Selection::new(ranges, selection.primary_index()));
    doc.apply(&transaction, view.id);
    doc.append_changes_to_history(view);

    // after replace cursor may be out of bounds, do this to
    // make sure cursor is in view and update scroll as well
    view.ensure_cursor_in_view(doc, config.scrolloff);
    anyhow::Ok(())
}

pub fn typeablecmd_send_info(
    cx: &mut compositor::Context,
    args: &[Cow<str>],
    event: PromptEvent,
) -> anyhow::Result<()> {
    cx.editor.handlers.trigger_codestats_send();
    anyhow::Ok(())
}
