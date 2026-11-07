//! Server-wide facts the game-object façades answer from.
//!
//! Both façades a Java callback can meet, the event listeners'
//! (`events.rs`) and the game-hook tier's (`game_hooks.rs`), stand a few
//! bounded markers in for the live `MinecraftServer`, its levels and players.
//! The members those markers answered first were per-object facts (a UUID, a
//! position, a tick count). The ones real jars reach next are about the server
//! as a whole, and were measured as the most common refusals on a dedicated
//! server with the corpus' listening jars (7 November 2026, 172 jars, 99
//! refused callbacks): `MinecraftServer#getWorldPath` blocked 18 callbacks,
//! `registryAccess` 10, `getGameRules` 4. This module answers them from a
//! [`ServerFacts`] snapshot the game fills from its own state:
//!
//! - **World path.** `getWorldPath(LevelResource)` is the world folder with
//!   the resource's id resolved under it, exactly as vanilla's
//!   `LevelStorageAccess.getLevelPath` builds it (`world/.` for `ROOT`). The
//!   folder is spelled inside the interpreter's virtual game folder
//!   (`/horizon/game/...`, the same root `FabricLoader.getGameDir` answers),
//!   because the interpreter's file system is virtual: a mod that reads a
//!   file there finds none and writes defaults, as on a fresh world.
//! - **Game rules.** `getGameRules()` (server or level) is one rules marker;
//!   `GameRules.<RULE>` statics are `game-rule/<id>` markers (every 26.3 rule
//!   id is its field name in lower case, checked against the server jar), and
//!   `get(rule)` answers the boxed value the game holds now.
//! - **Registries.** `registryAccess()` (server, level or player) is one
//!   registry-access marker; `lookupOrThrow(key)` answers a registry marker
//!   for a registry the game listed, whose `containsKey`, `keySet` and `size`
//!   answer from the ids it holds. A registry the game did not list refuses:
//!   unknown to Horizon is not the same as missing in vanilla.
//! - **Networking.** `ServerPlayNetworking.canSend(player, type)` answers
//!   whether that player's client takes the channel ([`ServerFacts::client_channels`]).
//!
//! A second round (same corpus pack grown to 204 jars, 94 refused callbacks)
//! added what came next by count:
//!
//! - **Saved data.** `getDataStorage()` (server or level) is the one storage
//!   `facade_saved_data.rs` keeps (8 callbacks).
//! - **The server thread.** A callback runs on the server thread, so
//!   `getRunningThread()` is the thread `Thread.currentThread()` answers, and
//!   `execute(runnable)`/`executeIfPossible` run the runnable at once, as
//!   vanilla's `BlockableEventLoop.execute` does on its own thread
//!   ([`Runtime::invoke_server_facade`]).
//! - **Layered registries.** `registries()` is a `LayeredRegistryAccess`
//!   whose `compositeAccess()` is the registry access above.
//! - **Commands.** `getCommands().getDispatcher()` is the Brigadier
//!   façade's dispatcher (`brigadier.rs`), so a command registered there
//!   lands in the jar's command arena like a `CommandRegistrationCallback`'s.
//! - **Permissions.** A player's `permissions()` is the permission set of
//!   their level ([`ServerFacts::permission_levels`]).
//!
//! A member of these markers that is not listed refuses with
//! [`NativeError::UnknownHostCall`] naming it, like every other façade call:
//! a neutral value would let a callback carry on with state the game never
//! had.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use crate::{
    MethodRef, NativeError, Value, boolean_value, collection_value, expect_ref, identifier_parse,
    path_join, path_value, stable_hash,
};

/// A game rule's value as the game holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GameRuleValue {
    Bool(bool),
    Int(i32),
}

/// What the game knows about the running server, beyond the per-object facts
/// each firing passes. Every fact is optional in effect: an empty map or a
/// `None` makes the member that needs it refuse.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ServerFacts {
    /// The world folder in the interpreter's virtual file system
    /// (`/horizon/game/world`), without a trailing slash.
    pub world_dir: Option<String>,
    /// Every game rule by its id without namespace (`keep_inventory`).
    pub game_rules: BTreeMap<String, GameRuleValue>,
    /// Registries the game lists by id (`minecraft:block`) → their entry ids.
    pub registries: BTreeMap<String, Arc<BTreeSet<String>>>,
    /// Per player UUID, the payload channels that player's client takes.
    /// A player with no entry takes none (a vanilla client).
    pub client_channels: BTreeMap<u128, BTreeSet<String>>,
    /// `Level.getGameTime()`/`getDayTime()`: every level reads the
    /// overworld's clock, as vanilla's derived levels do.
    pub game_time: Option<i64>,
    pub day_time: Option<i64>,
    /// Per player UUID, their permission level (0-4, vanilla's op levels).
    /// A player with no entry refuses `permissions()`.
    pub permission_levels: BTreeMap<u128, u8>,
}

/// The rules marker every `getGameRules()` answers: vanilla's levels read the
/// one `GameRules` the world data holds.
const RULES: &str = "game/rules/main";
/// The registry-access marker every `registryAccess()` answers.
const REGISTRIES: &str = "game/registries/main";
const REGISTRY: &str = "game/registry/";
/// `MinecraftServer.registries()`: the layered access over the same lists.
const LAYERED: &str = "game/registries/layered";
/// `MinecraftServer.getCommands()`.
const COMMANDS: &str = "game/commands";
/// The marker `Thread.currentThread()` answers: callbacks run on the server
/// thread, so it is also the server's running thread.
const THREAD: &str = "thread/main";
const GAME_RULE: &str = "game-rule/";
const LEVEL_RESOURCE: &str = "level-resource/";

const LEVEL_RESOURCE_CLASS: &str = "net/minecraft/world/level/storage/LevelResource";
const GAME_RULES_CLASS: &str = "net/minecraft/world/level/gamerules/GameRules";
const SERVER_PLAY_NETWORKING: &str = "net/fabricmc/fabric/api/networking/v1/ServerPlayNetworking";

/// `LevelResource`'s constants and the ids vanilla gives them (26.3 server
/// jar, `LevelResource.<clinit>`).
const LEVEL_RESOURCES: &[(&str, &str)] = &[
    ("PLAYER_ADVANCEMENTS_DIR", "players/advancements"),
    ("PLAYER_STATS_DIR", "players/stats"),
    ("PLAYER_DATA_DIR", "players/data"),
    ("PLAYER_OLD_DATA_DIR", "players"),
    ("LEVEL_DATA_FILE", "level.dat"),
    ("OLD_LEVEL_DATA_FILE", "level.dat_old"),
    ("ICON_FILE", "icon.png"),
    ("LOCK_FILE", "session.lock"),
    ("GENERATED_DIR", "generated"),
    ("DATAPACK_DIR", "datapacks"),
    ("MAP_RESOURCE_FILE", "resourcepacks/resources.zip"),
    ("DATA", "data"),
    ("ROOT", "."),
];

fn marker(text: impl Into<String>) -> Value {
    Value::Ref(Some(text.into()))
}

fn refuse(r: &MethodRef) -> Result<Value, NativeError> {
    Err(NativeError::UnknownHostCall(r.clone()))
}

/// A static field this module models: a `LevelResource` constant or a
/// `GameRules` rule.
pub(crate) fn static_field(r: &MethodRef) -> Option<Value> {
    match r.owner.as_str() {
        LEVEL_RESOURCE_CLASS if r.descriptor == format!("L{LEVEL_RESOURCE_CLASS};") => {
            LEVEL_RESOURCES
                .iter()
                .find(|(field, _)| *field == r.name)
                .map(|(_, id)| marker(format!("{LEVEL_RESOURCE}{id}")))
        }
        GAME_RULES_CLASS
            if r.descriptor == "Lnet/minecraft/world/level/gamerules/GameRule;"
                && r.name.bytes().all(|b| b.is_ascii_uppercase() || b == b'_') =>
        {
            Some(marker(format!(
                "{GAME_RULE}{}",
                r.name.to_ascii_lowercase()
            )))
        }
        _ => None,
    }
}

/// `new LevelResource(id)`: the id to keep for the object's marker, or `None`
/// for any other call.
pub(crate) fn level_resource_init(r: &MethodRef, args: &[Value]) -> Option<String> {
    (r.owner == LEVEL_RESOURCE_CLASS
        && r.name == "<init>"
        && r.descriptor == "(Ljava/lang/String;)V")
        .then(|| args.first().and_then(expect_ref).cloned())
        .flatten()
}

/// The arguments with a jar-built `LevelResource` object replaced by the
/// value marker its id names (`resolve` answers the id `new` recorded), so
/// [`server_member`] reads both kinds the same way.
pub(crate) fn with_level_resources(
    args: &[Value],
    resolve: impl Fn(&str) -> Option<String>,
) -> Vec<Value> {
    args.iter()
        .map(|arg| {
            expect_ref(arg)
                .filter(|text| text.starts_with(&format!("{LEVEL_RESOURCE_CLASS}#")))
                .and_then(|text| resolve(text))
                .map_or_else(|| arg.clone(), |id| marker(format!("{LEVEL_RESOURCE}{id}")))
        })
        .collect()
}

/// Whether `text` is a marker [`answer`] owns as a receiver.
pub(crate) fn owns(text: &str) -> bool {
    text == RULES
        || text == REGISTRIES
        || text == LAYERED
        || text == COMMANDS
        || text.starts_with(REGISTRY)
        || text.starts_with(GAME_RULE)
        || text.starts_with(LEVEL_RESOURCE)
}

/// A server-wide member on a server, level or entity marker, or `None` when
/// `r` is not one of them (the caller's own table answers it).
pub(crate) fn server_member(
    facts: &ServerFacts,
    r: &MethodRef,
    args: &[Value],
) -> Option<Result<Value, NativeError>> {
    Some(match (r.name.as_str(), r.descriptor.as_str()) {
        (
            "getWorldPath",
            "(Lnet/minecraft/world/level/storage/LevelResource;)Ljava/nio/file/Path;",
        ) => {
            let resource = args
                .first()
                .and_then(expect_ref)
                .and_then(|text| text.strip_prefix(LEVEL_RESOURCE));
            match (&facts.world_dir, resource) {
                (Some(dir), Some(id)) => Ok(path_value(&path_join(dir, id))),
                _ => refuse(r),
            }
        }
        ("getGameRules", "()Lnet/minecraft/world/level/gamerules/GameRules;") => {
            if facts.game_rules.is_empty() {
                refuse(r)
            } else {
                Ok(marker(RULES))
            }
        }
        ("registryAccess", descriptor)
            if descriptor.starts_with("()Lnet/minecraft/core/RegistryAccess") =>
        {
            if facts.registries.is_empty() {
                refuse(r)
            } else {
                Ok(marker(REGISTRIES))
            }
        }
        ("getDataStorage", "()Lnet/minecraft/world/level/storage/SavedDataStorage;") => {
            Ok(marker(crate::facade_saved_data::STORAGE))
        }
        ("getRunningThread", "()Ljava/lang/Thread;") => Ok(marker(THREAD)),
        // The server's folder: the one the world folder sits in, as a
        // dedicated server's `<level-name>` sits in its working directory.
        ("getServerDirectory", "()Ljava/nio/file/Path;") => {
            match facts
                .world_dir
                .as_deref()
                .and_then(|dir| dir.rsplit_once('/'))
            {
                Some((parent, _)) if !parent.is_empty() => Ok(path_value(parent)),
                _ => refuse(r),
            }
        }
        ("registries", "()Lnet/minecraft/core/LayeredRegistryAccess;") => {
            if facts.registries.is_empty() {
                refuse(r)
            } else {
                Ok(marker(LAYERED))
            }
        }
        ("getCommands", "()Lnet/minecraft/commands/Commands;") => Ok(marker(COMMANDS)),
        _ => return None,
    })
}

/// A player's server-wide member, by the player's UUID: `permissions()`.
/// `None` when `r` is not one.
pub(crate) fn player_member(
    facts: &ServerFacts,
    player: u128,
    r: &MethodRef,
) -> Option<Result<Value, NativeError>> {
    match (r.name.as_str(), r.descriptor.as_str()) {
        ("permissions", "()Lnet/minecraft/server/permissions/PermissionSet;") => {
            Some(match facts.permission_levels.get(&player) {
                // The Brigadier façade's permission set answers its queries.
                Some(level) => Ok(marker(format!("brigadier/permission-set/{level}"))),
                None => refuse(r),
            })
        }
        _ => None,
    }
}

/// The id a registry key argument names: `registry/<id>` (a `Registries`
/// static) or `resource-key/minecraft:root/<id>`.
fn registry_key(value: &Value) -> Option<&str> {
    let text = expect_ref(value)?;
    text.strip_prefix("registry/").or_else(|| {
        text.strip_prefix("resource-key/minecraft:root/")
            .map(|id| id.strip_prefix("identifier/").unwrap_or(id))
    })
}

/// The `namespace:path` an `Identifier` argument names.
fn identifier_text(value: &Value) -> Option<String> {
    let text = expect_ref(value)?.strip_prefix("identifier/")?;
    Some(if text.contains(':') {
        text.to_owned()
    } else {
        format!("minecraft:{text}")
    })
}

/// Answer one call on a marker [`owns`] says is this module's.
pub(crate) fn answer(
    facts: &ServerFacts,
    text: &str,
    r: &MethodRef,
    args: &[Value],
) -> Result<Value, NativeError> {
    let descriptor = r.descriptor.as_str();
    // `Object`'s members hold for every marker.
    match (r.name.as_str(), descriptor) {
        ("hashCode", "()I") => return Ok(Value::Int(stable_hash(text))),
        ("equals", "(Ljava/lang/Object;)Z") => {
            let same = args.first().and_then(expect_ref).is_some_and(|o| o == text);
            return Ok(Value::Int(i32::from(same)));
        }
        _ => {}
    }
    if let Some(id) = text.strip_prefix(GAME_RULE) {
        return match (r.name.as_str(), descriptor) {
            ("id" | "toString", "()Ljava/lang/String;") => Ok(marker(id)),
            ("getIdentifier", "()Lnet/minecraft/resources/Identifier;") => {
                Ok(identifier_parse(&format!("minecraft:{id}")))
            }
            _ => refuse(r),
        };
    }
    if let Some(id) = text.strip_prefix(LEVEL_RESOURCE) {
        return match (r.name.as_str(), descriptor) {
            ("id" | "toString", "()Ljava/lang/String;") => Ok(marker(id)),
            _ => refuse(r),
        };
    }
    if text == RULES {
        let rule = args
            .first()
            .and_then(expect_ref)
            .and_then(|t| t.strip_prefix(GAME_RULE))
            .and_then(|id| facts.game_rules.get(id));
        return match (r.name.as_str(), descriptor, rule) {
            (
                "get",
                "(Lnet/minecraft/world/level/gamerules/GameRule;)Ljava/lang/Object;",
                Some(value),
            ) => Ok(match value {
                GameRuleValue::Bool(b) => boolean_value(*b),
                GameRuleValue::Int(i) => marker(format!("integer:{i}")),
            }),
            (
                "getAsString",
                "(Lnet/minecraft/world/level/gamerules/GameRule;)Ljava/lang/String;",
                Some(value),
            ) => Ok(marker(match value {
                GameRuleValue::Bool(b) => b.to_string(),
                GameRuleValue::Int(i) => i.to_string(),
            })),
            _ => refuse(r),
        };
    }
    if text == LAYERED {
        return match (r.name.as_str(), descriptor) {
            ("compositeAccess", "()Lnet/minecraft/core/RegistryAccess$Frozen;") => {
                Ok(marker(REGISTRIES))
            }
            _ => refuse(r),
        };
    }
    if text == COMMANDS {
        return match (r.name.as_str(), descriptor) {
            ("getDispatcher", "()Lcom/mojang/brigadier/CommandDispatcher;") => {
                Ok(marker(crate::brigadier::DISPATCHER))
            }
            _ => refuse(r),
        };
    }
    if text == REGISTRIES {
        return match (r.name.as_str(), descriptor) {
            (
                "lookupOrThrow",
                "(Lnet/minecraft/resources/ResourceKey;)Lnet/minecraft/core/Registry;",
            )
            | (
                "lookupOrThrow",
                "(Lnet/minecraft/resources/ResourceKey;)Lnet/minecraft/core/HolderLookup$RegistryLookup;",
            ) => match args.first().and_then(registry_key) {
                Some(id) if facts.registries.contains_key(id) => {
                    Ok(marker(format!("{REGISTRY}{id}")))
                }
                _ => refuse(r),
            },
            _ => refuse(r),
        };
    }
    if let Some(id) = text.strip_prefix(REGISTRY) {
        let Some(entries) = facts.registries.get(id) else {
            return refuse(r);
        };
        return match (r.name.as_str(), descriptor) {
            ("size", "()I") => Ok(Value::Int(i32::try_from(entries.len()).unwrap_or(i32::MAX))),
            ("containsKey", "(Lnet/minecraft/resources/Identifier;)Z") => {
                let wanted = args.first().and_then(identifier_text);
                Ok(Value::Int(i32::from(
                    wanted.is_some_and(|wanted| entries.contains(&wanted)),
                )))
            }
            ("keySet", "()Ljava/util/Set;") => {
                let keys: Vec<Value> = entries.iter().map(|id| identifier_parse(id)).collect();
                Ok(collection_value("set", &keys))
            }
            _ => refuse(r),
        };
    }
    refuse(r)
}

/// `ServerPlayNetworking.canSend(player, type|id)`: whether the player's
/// client takes the channel. `player` is the UUID the caller's façade names
/// for the first argument, `channel` the id the second resolves to; `None`
/// for a call this does not model.
pub(crate) fn can_send(
    facts: &ServerFacts,
    r: &MethodRef,
    player: Option<u128>,
    channel: Option<&str>,
) -> Option<Result<Value, NativeError>> {
    if r.owner != SERVER_PLAY_NETWORKING || r.name != "canSend" {
        return None;
    }
    Some(match (player, channel) {
        (Some(player), Some(channel)) => Ok(Value::Int(i32::from(
            facts
                .client_channels
                .get(&player)
                .is_some_and(|channels| channels.contains(channel)),
        ))),
        _ => refuse(r),
    })
}

/// The channel id a `canSend` second argument names: an `Identifier` value,
/// or a payload type through `type_id` (the host's packet-type table).
pub(crate) fn channel_of(
    value: &Value,
    type_id: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    identifier_text(value).or_else(|| expect_ref(value).and_then(|marker| type_id(marker)))
}

impl crate::StdHost {
    /// A field of a façade object read with its receiver during a firing:
    /// a player's `connection` is that player's connection marker, and a
    /// connection's `player` its player (`ServerPlayNetworking.canSend`
    /// takes either). `None` for any other read.
    pub(crate) fn facade_field(&self, receiver: &Value, r: &MethodRef) -> Option<Value> {
        self.game_event.as_deref()?;
        let text = expect_ref(receiver)?;
        match (r.owner.as_str(), r.name.as_str(), r.descriptor.as_str()) {
            (
                "net/minecraft/server/level/ServerPlayer",
                "connection",
                "Lnet/minecraft/server/network/ServerGamePacketListenerImpl;",
            ) => Some(marker(format!(
                "game/connection/{}",
                text.strip_prefix("game/player/")?
            ))),
            (
                "net/minecraft/server/network/ServerGamePacketListenerImpl",
                "player",
                "Lnet/minecraft/server/level/ServerPlayer;",
            ) => Some(marker(format!(
                "game/player/{}",
                text.strip_prefix("game/connection/")?
            ))),
            _ => None,
        }
    }

    /// Instance calls of the server façade that keep state on the jar's
    /// resident host or read the firing's facts directly: the saved data
    /// (`facade_saved_data.rs`) and a façade player's `permissions()`.
    /// `None` leaves the call to the listener façade (`events.rs`).
    pub(crate) fn server_facade_call(
        &mut self,
        r: &MethodRef,
        receiver: &Value,
        args: &[Value],
    ) -> Option<Result<Value, NativeError>> {
        if let Some(answer) = self.saved_data.call(r, receiver, args) {
            return Some(answer);
        }
        let player = expect_ref(receiver)?.strip_prefix("game/player/")?;
        let player = u128::from_str_radix(player, 16).ok()?;
        let firing = self.game_event.as_deref()?;
        player_member(&firing.facade.server, player, r)
    }
}

/// Owners whose `execute(Runnable)` is the server's own event loop.
const EVENT_LOOPS: &[&str] = &[
    "net/minecraft/server/MinecraftServer",
    "net/minecraft/server/dedicated/DedicatedServer",
    "net/minecraft/util/thread/ReentrantBlockableEventLoop",
    "net/minecraft/util/thread/BlockableEventLoop",
];

impl<H: crate::Host> crate::Runtime<'_, H> {
    /// Server-façade calls that run jar code, which the host cannot do: a
    /// saved-data type's constructor (`facade_saved_data.rs`) and a
    /// runnable handed to the server's event loop. `None` for every other
    /// call.
    pub(crate) fn invoke_server_facade(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
        args: &[Value],
    ) -> Result<Option<Value>, NativeError> {
        if let Some(value) = self.invoke_saved_data(r, receiver, args)? {
            return Ok(Some(value));
        }
        // Chat components with style and arguments (`facade_text.rs`).
        if let Some(value) = self.invoke_text(r, receiver, args)? {
            return Ok(Some(value));
        }
        // `server.execute(task)` on the server thread: vanilla's
        // `scheduleExecutables()` is false there (outside a running task),
        // so the task runs now rather than being queued. Only a façade
        // server (a `game/` marker) is the running one.
        let on_server = receiver
            .and_then(expect_ref)
            .is_some_and(|text| text.starts_with("game/"));
        if on_server
            && EVENT_LOOPS.contains(&r.owner.as_str())
            && matches!(r.name.as_str(), "execute" | "executeIfPossible")
            && r.descriptor == "(Ljava/lang/Runnable;)V"
            && let [task] = args
        {
            return match self.invoke_callback(task, &[])? {
                Some(_) => Ok(Some(Value::Void)),
                None => Err(NativeError::Metadata(format!(
                    "{}#{} got a task the interpreter cannot run: {task:?}",
                    r.owner, r.name
                ))),
            };
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(owner: &str, name: &str, descriptor: &str) -> MethodRef {
        MethodRef {
            owner: owner.into(),
            name: name.into(),
            descriptor: descriptor.into(),
        }
    }

    fn facts() -> ServerFacts {
        ServerFacts {
            world_dir: Some("/horizon/game/world".into()),
            game_rules: BTreeMap::from([
                ("keep_inventory".into(), GameRuleValue::Bool(true)),
                ("random_tick_speed".into(), GameRuleValue::Int(3)),
            ]),
            registries: BTreeMap::from([(
                "minecraft:block".into(),
                Arc::new(BTreeSet::from([
                    "minecraft:stone".into(),
                    "minecraft:dirt".into(),
                ])),
            )]),
            client_channels: BTreeMap::from([(7, BTreeSet::from(["demo:sync".into()]))]),
            ..ServerFacts::default()
        }
    }

    #[test]
    fn world_path_resolves_the_level_resource_under_the_world_folder() {
        let root = static_field(&call(
            LEVEL_RESOURCE_CLASS,
            "ROOT",
            "Lnet/minecraft/world/level/storage/LevelResource;",
        ))
        .unwrap();
        let data = static_field(&call(
            LEVEL_RESOURCE_CLASS,
            "PLAYER_DATA_DIR",
            "Lnet/minecraft/world/level/storage/LevelResource;",
        ))
        .unwrap();
        let get = call(
            "net/minecraft/server/MinecraftServer",
            "getWorldPath",
            "(Lnet/minecraft/world/level/storage/LevelResource;)Ljava/nio/file/Path;",
        );
        // Vanilla resolves the id as given: `ROOT` is `world/.`.
        assert_eq!(
            server_member(&facts(), &get, std::slice::from_ref(&root))
                .unwrap()
                .unwrap(),
            path_value("/horizon/game/world/.")
        );
        assert_eq!(
            server_member(&facts(), &get, &[data]).unwrap().unwrap(),
            path_value("/horizon/game/world/players/data")
        );
        // The server's own folder holds the world folder.
        let directory = call(
            "net/minecraft/server/MinecraftServer",
            "getServerDirectory",
            "()Ljava/nio/file/Path;",
        );
        assert_eq!(
            server_member(&facts(), &directory, &[]).unwrap().unwrap(),
            path_value("/horizon/game")
        );
        // No world folder, or a resource the model did not build: refuse.
        let none = ServerFacts::default();
        assert!(server_member(&none, &get, &[root]).unwrap().is_err());
        assert!(
            server_member(&facts(), &get, &[Value::Ref(None)])
                .unwrap()
                .is_err()
        );
    }

    #[test]
    fn game_rules_answer_the_games_values_boxed() {
        let facts = facts();
        let rules = server_member(
            &facts,
            &call(
                "net/minecraft/server/level/ServerLevel",
                "getGameRules",
                "()Lnet/minecraft/world/level/gamerules/GameRules;",
            ),
            &[],
        )
        .unwrap()
        .unwrap();
        let rules = expect_ref(&rules).unwrap().clone();
        let rule = |name: &str| {
            static_field(&call(
                GAME_RULES_CLASS,
                name,
                "Lnet/minecraft/world/level/gamerules/GameRule;",
            ))
            .unwrap()
        };
        let get = call(
            GAME_RULES_CLASS,
            "get",
            "(Lnet/minecraft/world/level/gamerules/GameRule;)Ljava/lang/Object;",
        );
        assert!(owns(&rules));
        assert_eq!(
            answer(&facts, &rules, &get, &[rule("KEEP_INVENTORY")]).unwrap(),
            boolean_value(true)
        );
        assert_eq!(
            answer(&facts, &rules, &get, &[rule("RANDOM_TICK_SPEED")]).unwrap(),
            marker("integer:3")
        );
        // A rule the game does not hold, and a member the model does not
        // list, refuse rather than answer a default.
        assert!(answer(&facts, &rules, &get, &[rule("SPAWN_WARDENS")]).is_err());
        let set = call(
            GAME_RULES_CLASS,
            "set",
            "(Lnet/minecraft/world/level/gamerules/GameRule;Ljava/lang/Object;Lnet/minecraft/server/MinecraftServer;)V",
        );
        assert!(answer(&facts, &rules, &set, &[rule("KEEP_INVENTORY")]).is_err());
    }

    #[test]
    fn registry_access_looks_up_only_registries_the_game_listed() {
        let facts = facts();
        let access = server_member(
            &facts,
            &call(
                "net/minecraft/server/MinecraftServer",
                "registryAccess",
                "()Lnet/minecraft/core/RegistryAccess$Frozen;",
            ),
            &[],
        )
        .unwrap()
        .unwrap();
        let access = expect_ref(&access).unwrap().clone();
        let lookup = call(
            "net/minecraft/core/RegistryAccess$Frozen",
            "lookupOrThrow",
            "(Lnet/minecraft/resources/ResourceKey;)Lnet/minecraft/core/Registry;",
        );
        let blocks = answer(
            &facts,
            &access,
            &lookup,
            &[marker("registry/minecraft:block")],
        )
        .unwrap();
        let blocks = expect_ref(&blocks).unwrap().clone();
        let registry =
            |name: &str, descriptor: &str| call("net/minecraft/core/Registry", name, descriptor);
        assert_eq!(
            answer(&facts, &blocks, &registry("size", "()I"), &[]).unwrap(),
            Value::Int(2)
        );
        let contains = registry("containsKey", "(Lnet/minecraft/resources/Identifier;)Z");
        assert_eq!(
            answer(
                &facts,
                &blocks,
                &contains,
                &[marker("identifier/minecraft:dirt")]
            )
            .unwrap(),
            Value::Int(1)
        );
        assert_eq!(
            answer(
                &facts,
                &blocks,
                &contains,
                &[marker("identifier/minecraft:glass")]
            )
            .unwrap(),
            Value::Int(0)
        );
        // Biomes are a vanilla registry the game did not list here.
        assert!(
            answer(
                &facts,
                &access,
                &lookup,
                &[marker("registry/minecraft:worldgen/biome")]
            )
            .is_err()
        );
    }

    #[test]
    fn can_send_is_true_only_for_a_channel_the_players_client_takes() {
        let facts = facts();
        let r = call(
            SERVER_PLAY_NETWORKING,
            "canSend",
            "(Lnet/minecraft/server/level/ServerPlayer;Lnet/minecraft/network/protocol/common/custom/CustomPacketPayload$Type;)Z",
        );
        assert_eq!(
            can_send(&facts, &r, Some(7), Some("demo:sync"))
                .unwrap()
                .unwrap(),
            Value::Int(1)
        );
        assert_eq!(
            can_send(&facts, &r, Some(7), Some("demo:other"))
                .unwrap()
                .unwrap(),
            Value::Int(0)
        );
        // A vanilla client (no entry) takes nothing.
        assert_eq!(
            can_send(&facts, &r, Some(8), Some("demo:sync"))
                .unwrap()
                .unwrap(),
            Value::Int(0)
        );
        // A player or channel the caller could not name refuses.
        assert!(
            can_send(&facts, &r, None, Some("demo:sync"))
                .unwrap()
                .is_err()
        );
        assert_eq!(
            channel_of(&marker("identifier/demo:sync"), |_| None).as_deref(),
            Some("demo:sync")
        );
    }
}
