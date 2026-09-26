//! `cahoots settings`: every setting on a page at the terminal, or one at a
//! time from the command line (`settings set`, `settings reset`). What each
//! setting is and how it is written is `crate::settings`; this is where it
//! meets a person (hard rule 11).

use serde_json::json;

use super::SettingsAction;
use crate::config::UserConfig;
use crate::dirs::Dirs;
use crate::exit::{Envelope, Exit, Fail, Res};
use crate::meter::MeterFile;
use crate::settings::{self, Key, Setting};

pub fn settings(action: Option<SettingsAction>) -> Res<Envelope> {
    let dirs = Dirs::resolve()?;
    let file = dirs.config_file();
    let key = match action {
        None => return Err(no_page()),
        Some(SettingsAction::Set { key, value }) => {
            let now = current(&dirs)?;
            let setting = settings::find(&now, &key)?;
            let value = setting.parse(&value)?;
            settings::set(&file, setting.key, &value)?;
            setting.key
        }
        Some(SettingsAction::Reset { key: name }) => {
            // The file must be good to change it at all.
            current(&dirs)?;
            let key = Key::parse(&name)
                .ok_or_else(|| Fail::new(Exit::Usage, format!("no setting is called {name:?}")))?;
            settings::reset(&file, key)?;
            key
        }
    };
    let after = current(&dirs)?;
    Ok(Envelope::new(Exit::Ok, None).with_data(json!({
        "changed": [changed(key, after.iter().find(|setting| setting.key == key))],
    })))
}

/// Every setting as the files say now.
fn current(dirs: &Dirs) -> Res<Vec<Setting>> {
    let config = UserConfig::load(&dirs.config_file())?;
    let found = MeterFile::load(dirs)?;
    Ok(settings::current(
        &config,
        found.as_ref(),
        crate::env::path_var().as_deref(),
    ))
}

/// One change, as the envelope lists it: the setting's value after it, and
/// where that value comes from.
fn changed(key: Key, now: Option<&Setting>) -> serde_json::Value {
    json!({
        "key": key.name(),
        "value": now.and_then(|s| s.value.as_ref()).map(settings::Value::to_json),
        "origin": now.map_or(settings::Origin::Default, |s| s.origin).as_str(),
    })
}

/// There is no terminal to show the page at.
fn no_page() -> Fail {
    Fail::new(
        Exit::Usage,
        "the settings page is shown at a terminal (stdin and stderr, `TERM` not `dumb`) — \
         `cahoots settings set <key> <value>` changes one setting without it, and \
         `cahoots settings reset <key>` puts one back",
    )
}
