//! Config libraries a HUD mod reads its settings through: their defaults.
//!
//! A client mod gates its HUD on its config (DayCount's `dayCountEnabled`),
//! and the config comes from a library the mod bundles or names as a
//! dependency: cloth-config's `AutoConfig`, YetAnotherConfigLib's
//! `ConfigClassHandler`. Run alone, the library's classes are not there and
//! every call answered neutrally, so the config object was null and every
//! field read as `false`/`0`: the HUD never drew. Run with the library, its
//! serializers reach reflection and file shapes the interpreter does not
//! hold.
//!
//! What both libraries do for a config class the player never edited is
//! construct it with its no-argument constructor, whose field initialisers
//! are the defaults. That is what this model does, once per config class,
//! and keeps the object in the jar's static state so every later
//! `getConfig()` / `instance()` (in a HUD frame, a tick) sees the same
//! object and whatever the mod changed on it:
//!
//! - `AutoConfig.register(Class, factory)` → its holder;
//!   `AutoConfig.getConfigHolder(Class)` → the holder; `holder.getConfig()`
//!   → the object; `save`, `load`, `registerSaveListener`,
//!   `registerLoadListener` are accepted and do nothing (nothing is read from
//!   or written to the player's config folder).
//! - `ConfigClassHandler.createBuilder(Class)` → a builder whose `id`,
//!   `serializer` steps answer itself, `build()` → the handler;
//!   `handler.instance()` → the object, `defaults()` → a fresh default one,
//!   `load()` → `true`, `save()` → nothing.
//!
//! - SuperMartijn642's config lib: `ConfigBuilders.newTomlConfig(...)` is a
//!   builder whose `push`/`pop`/`comment`/`dontSync`/... steps answer
//!   itself; `define(name, default[, min, max])` → a `Supplier` whose `get()`
//!   is the boxed default.
//!
//! A saved config file is not read: a player's own settings are a later
//! step, and the defaults are what a fresh install shows.

use crate::{Host, MethodRef, NativeError, Runtime, Value, expect_ref};

const AUTO_CONFIG: &str = "me/shedaniel/autoconfig/AutoConfig";
const AUTO_HOLDER: &str = "me/shedaniel/autoconfig/ConfigHolder";
const YACL_HANDLER: &str = "dev/isxander/yacl3/config/v2/api/ConfigClassHandler";
const YACL_BUILDER: &str = "dev/isxander/yacl3/config/v2/api/ConfigClassHandler$Builder";
const CONFIGLIB_BUILDERS: &str = "com/supermartijn642/configlib/api/ConfigBuilders";
const CONFIGLIB_BUILDER: &str = "com/supermartijn642/configlib/api/IConfigBuilder";
const CONFIGLIB: &str = "configlib-builder";
/// `configlib-value/<n>`: the `n`th value defined, kept in the jar's statics.
const CONFIGLIB_VALUE: &str = "configlib-value/";
/// Values one jar's config lib builders may define.
const MAX_CONFIGLIB_VALUES: usize = 1024;
/// Marker prefixes: `config-holder/<class>`, `config-builder/<class>`.
const HOLDER: &str = "config-holder/";
const BUILDER: &str = "config-builder/";
/// The static slot a config class's object lives in, so it survives the
/// invocation (jar statics are the resident state).
fn slot(class: &str) -> String {
    format!("{AUTO_CONFIG}#instance/{class}")
}

fn class_arg(args: &[Value]) -> Option<String> {
    Some(expect_ref(args.first()?)?.replace('.', "/"))
}

impl<H: Host> Runtime<'_, H> {
    /// The config library calls this module answers; `None` for any other.
    pub(crate) fn invoke_config_library(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
        args: &[Value],
    ) -> Result<Option<Value>, NativeError> {
        let marker = |text: String| Some(Value::Ref(Some(text)));
        let held = receiver.and_then(expect_ref).cloned();
        if let Some(value) = self.invoke_configlib(r, held.as_deref(), args) {
            return Ok(Some(value));
        }
        Ok(match (r.owner.as_str(), r.name.as_str()) {
            (AUTO_CONFIG, "register" | "getConfigHolder") => {
                let Some(class) = class_arg(args) else {
                    return Ok(None);
                };
                if !self.has_class(&class) {
                    return Ok(None);
                }
                marker(format!("{HOLDER}{class}"))
            }
            (YACL_HANDLER, "createBuilder") => {
                let Some(class) = class_arg(args) else {
                    return Ok(None);
                };
                if !self.has_class(&class) {
                    return Ok(None);
                }
                marker(format!("{BUILDER}{class}"))
            }
            (YACL_BUILDER, "id" | "serializer")
                if held.as_deref().is_some_and(|h| h.starts_with(BUILDER)) =>
            {
                receiver.cloned()
            }
            (YACL_BUILDER, "build") => {
                let Some(class) = held.as_deref().and_then(|h| h.strip_prefix(BUILDER)) else {
                    return Ok(None);
                };
                marker(format!("{HOLDER}{class}"))
            }
            (AUTO_HOLDER | YACL_HANDLER, name) => {
                let Some(class) = held.as_deref().and_then(|h| h.strip_prefix(HOLDER)) else {
                    return Ok(None);
                };
                let class = class.to_owned();
                match name {
                    "getConfig" | "instance" => Some(self.config_instance(&class)?),
                    "defaults" => Some(self.construct_default(&class)?),
                    "load" if r.descriptor.ends_with(")Z") => Some(Value::Int(1)),
                    "save"
                    | "load"
                    | "registerSaveListener"
                    | "registerLoadListener"
                    | "resetToDefault" => {
                        if name == "resetToDefault" {
                            let fresh = self.construct_default(&class)?;
                            self.static_fields.insert(slot(&class), fresh);
                        }
                        Some(Value::default_for(crate::return_desc(&r.descriptor)))
                    }
                    "getConfigClass" | "configClass" => marker(class),
                    _ => None,
                }
            }
            _ => None,
        })
    }

    /// SuperMartijn642's config lib: builders and the defaults they define.
    fn invoke_configlib(
        &mut self,
        r: &MethodRef,
        receiver: Option<&str>,
        args: &[Value],
    ) -> Option<Value> {
        let builder = || Value::Ref(Some(CONFIGLIB.into()));
        match (r.owner.as_str(), r.name.as_str(), receiver) {
            (CONFIGLIB_BUILDERS, name, _) if name.starts_with("new") => Some(builder()),
            (CONFIGLIB_BUILDER, "define", Some(CONFIGLIB)) => {
                let params = r.descriptor.split(')').next()?.trim_start_matches('(');
                let default = args.get(1)?;
                // `define(String, boolean)`, `(String, int, int, int)`,
                // `(String, double, double, double)`, `(String, String...)`,
                // `(String, Enum)`: the default is the second argument.
                let boxed = match &params["Ljava/lang/String;".len().min(params.len())..] {
                    p if p.starts_with('Z') => {
                        crate::boolean_value(matches!(default, Value::Int(v) if *v != 0))
                    }
                    p if p.starts_with('I') => match default {
                        Value::Int(v) => Value::Ref(Some(format!("integer:{v}"))),
                        _ => return None,
                    },
                    p if p.starts_with('D') => match default {
                        Value::Float(v) => crate::double_value(*v),
                        _ => return None,
                    },
                    _ => default.clone(),
                };
                let n = self
                    .static_fields
                    .keys()
                    .filter(|k| k.starts_with("configlib#"))
                    .count();
                if n >= MAX_CONFIGLIB_VALUES {
                    return None;
                }
                self.static_fields.insert(format!("configlib#{n}"), boxed);
                Some(Value::Ref(Some(format!("{CONFIGLIB_VALUE}{n}"))))
            }
            (CONFIGLIB_BUILDER, "build", Some(CONFIGLIB)) => {
                Some(Value::default_for(crate::return_desc(&r.descriptor)))
            }
            (CONFIGLIB_BUILDER, _, Some(CONFIGLIB))
                if r.descriptor.ends_with(&format!("L{CONFIGLIB_BUILDER};")) =>
            {
                Some(builder())
            }
            ("java/util/function/Supplier", "get", Some(value)) => {
                let n = value.strip_prefix(CONFIGLIB_VALUE)?;
                self.static_fields.get(&format!("configlib#{n}")).cloned()
            }
            _ => None,
        }
    }

    /// The config object of `class`, built from its defaults the first time.
    fn config_instance(&mut self, class: &str) -> Result<Value, NativeError> {
        if let Some(value) = self.static_fields.get(&slot(class)) {
            return Ok(value.clone());
        }
        let value = self.construct_default(class)?;
        self.static_fields.insert(slot(class), value.clone());
        Ok(value)
    }

    /// `new <class>()`: the object its field initialisers give.
    fn construct_default(&mut self, class: &str) -> Result<Value, NativeError> {
        let instance = self.new_object(class);
        self.invoke(class, "<init>", "()V", Some(instance.clone()), &[])?;
        Ok(instance)
    }
}
