//! Key mappings a Java mod registered at run time (`KeyMappingHelper
//! .registerKeyMapping`, NeoForge's `RegisterKeyMappingsEvent`), beside
//! vanilla's [`MAPPINGS`](crate::keybinds::MAPPINGS).
//!
//! Controls lists them under their category, [`KeyBinds`](crate::keybinds::KeyBinds)
//! keeps the player's key for each, and `options.txt` stores it as vanilla
//! does (`key_<name>:key.keyboard.c`). Gameplay never listens for them: no
//! physical key is translated to a mod mapping, the mod asks its own
//! `consumeClick`/`isDown`.
//!
//! Names are leaked into `&'static str` once per name so the rest of the key
//! layer (keyed by vanilla's static names) takes them unchanged; the table is
//! bounded at [`MAX`] rows for the process.
//!
//! A mod's name and category are translation keys of its own lang file,
//! which the game's language does not load: [`set_translations`] keeps the
//! jar's lines, and [`label`]/[`category_label`] prefer the game's language,
//! then the jar's line for the current language, then its `en_us` one.

use std::{collections::HashMap, sync::RwLock};

use horizon_client_core::{lang, text::Text};

/// Mod mappings one process keeps; a registration past it is refused.
pub const MAX: usize = 512;

/// One registered mapping: (name, default key, category).
type Row = (&'static str, &'static str, &'static str);

static ROWS: RwLock<Vec<Row>> = RwLock::new(Vec::new());
/// (language code, translation key) → text, from the mods' own lang files.
static TRANSLATIONS: RwLock<Option<HashMap<(String, String), String>>> = RwLock::new(None);

fn leak(text: &str) -> &'static str {
    Box::leak(text.to_owned().into_boxed_str())
}

/// Adds a mapping `name` with `default` key (`key.keyboard.c`) in
/// `category` (`examplemod:main`, `minecraft:misc`, or an old mod's
/// `key.categories.examplemod`). Returns its static name; a name already
/// registered keeps its first row; `None` when the table is full or the name
/// is vanilla's own.
pub fn register(name: &str, default: &str, category: &str) -> Option<&'static str> {
    if crate::keybinds::MAPPINGS.iter().any(|m| m.0 == name) {
        return None;
    }
    let mut rows = ROWS.write().unwrap_or_else(|e| e.into_inner());
    if let Some(row) = rows.iter().find(|row| row.0 == name) {
        return Some(row.0);
    }
    if rows.len() >= MAX {
        return None;
    }
    // A vanilla category keeps the short name vanilla's rows use.
    let category = category.strip_prefix("minecraft:").unwrap_or(category);
    let row = (leak(name), leak(default), leak(category));
    rows.push(row);
    Some(row.0)
}

/// Every registered mapping, in registration order.
pub fn registered() -> Vec<Row> {
    ROWS.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The registered row named `name`.
pub fn find(name: &str) -> Option<Row> {
    ROWS.read()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|row| row.0 == name)
        .copied()
}

/// Vanilla's mappings, then the mods'.
pub fn all() -> Vec<Row> {
    let mut rows = crate::keybinds::MAPPINGS.to_vec();
    rows.extend(registered());
    rows
}

/// Keeps a mod's lang lines for `code` (`en_us`) as the fallback of its
/// names. Later lines for the same key replace earlier ones.
pub fn set_translations(code: &str, lines: impl IntoIterator<Item = (String, String)>) {
    let mut table = TRANSLATIONS.write().unwrap_or_else(|e| e.into_inner());
    let table = table.get_or_insert_with(HashMap::new);
    for (key, text) in lines {
        table.insert((code.to_owned(), key), text);
    }
}

/// A translation key's text: the game's language, else a mod's own line for
/// the current language, else its `en_us` line.
pub fn translation(key: &str) -> Option<String> {
    if lang::has(key) {
        return Some(lang::get(key));
    }
    let table = TRANSLATIONS.read().unwrap_or_else(|e| e.into_inner());
    let table = table.as_ref()?;
    [lang::code(), lang::DEFAULT.to_owned()]
        .into_iter()
        .find_map(|code| table.get(&(code, key.to_owned())).cloned())
}

/// A mapping's Controls label: vanilla's through the language, a mod's
/// through [`translation`], else the name itself (vanilla shows the key).
pub fn label(name: &str) -> Text {
    if lang::has(name) || find(name).is_none() {
        return Text::translate(name, &[]);
    }
    Text::literal(translation(name).unwrap_or_else(|| name.to_owned()))
}

/// The translation key of a category heading: vanilla's short names are
/// `key.category.minecraft.<name>`, an identifier `key.category.<ns>.<path>`
/// (`KeyMapping.Category.label`), and an old mod's key is itself.
pub fn category_key(category: &str) -> String {
    if category.starts_with("key.") {
        return category.to_owned();
    }
    match category.split_once(':') {
        Some((namespace, path)) => format!("key.category.{namespace}.{path}"),
        None => format!("key.category.minecraft.{category}"),
    }
}

/// A category heading's text.
pub fn category_label(category: &str) -> Text {
    let key = category_key(category);
    if lang::has(&key) {
        return Text::translate(&key, &[]);
    }
    Text::literal(translation(&key).unwrap_or(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mapping_registers_once_and_never_shadows_vanilla() {
        let name = register(
            "key.mod_mappings_test.zoom",
            "key.keyboard.c",
            "mod_mappings_test:main",
        )
        .unwrap();
        assert_eq!(
            find(name),
            Some((name, "key.keyboard.c", "mod_mappings_test:main"))
        );
        // The second registration keeps the first row.
        let again = register(
            "key.mod_mappings_test.zoom",
            "key.keyboard.z",
            "mod_mappings_test:other",
        );
        assert_eq!(again, Some(name));
        assert_eq!(find(name).unwrap().1, "key.keyboard.c");
        assert_eq!(register("key.jump", "key.keyboard.j", "movement"), None);
        assert!(all().iter().any(|row| row.0 == name));
        // A vanilla category keeps vanilla's short name.
        let misc = register(
            "key.mod_mappings_test.misc",
            "key.keyboard.unknown",
            "minecraft:misc",
        );
        assert_eq!(find(misc.unwrap()).unwrap().2, "misc");
    }

    #[test]
    fn category_keys_follow_vanilla_and_identifiers() {
        assert_eq!(category_key("misc"), "key.category.minecraft.misc");
        assert_eq!(
            category_key("examplemod:main"),
            "key.category.examplemod.main"
        );
        assert_eq!(
            category_key("key.categories.examplemod"),
            "key.categories.examplemod"
        );
    }

    #[test]
    fn a_mod_label_falls_back_to_its_own_lang_line() {
        let name = register(
            "key.mod_mappings_label.open",
            "key.keyboard.o",
            "mod_mappings_label:main",
        )
        .unwrap();
        // No line anywhere: the name itself, as vanilla shows a missing key.
        assert_eq!(label(name).plain(), "key.mod_mappings_label.open");
        set_translations(
            "en_us",
            [
                (
                    "key.mod_mappings_label.open".to_owned(),
                    "Open Thing".to_owned(),
                ),
                (
                    "key.category.mod_mappings_label.main".to_owned(),
                    "Label Mod".to_owned(),
                ),
            ],
        );
        assert_eq!(label(name).plain(), "Open Thing");
        assert_eq!(
            category_label("mod_mappings_label:main").plain(),
            "Label Mod"
        );
    }
}
