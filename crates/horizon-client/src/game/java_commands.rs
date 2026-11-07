//! Commands the Java jars register through Brigadier, in the server's command
//! tree and run from it.
//!
//! A jar registers its commands from a `CommandRegistrationCallback` (Fabric)
//! or a `RegisterCommandsEvent` listener (NeoForge/Forge); `events.rs` names
//! those listeners [`GameEvent::RegisterCommands`] at bootstrap. The batch
//! hands every jar with one to [`adopt`], which keeps the parsed jar and its
//! resident host (the one its event listeners and payload handlers share, so a
//! command sees the statics they wrote), per execution role.
//!
//! The first time a server builds its command tree (`register_native_commands`,
//! the native mods' extension point) the registration listeners run, each
//! jar's recorded tree ([`horizon_native_java::brigadier`]) is merged into one
//! the way Brigadier's `CommandNode.addChild` merges (later jars win a
//! command, children join), and the result becomes:
//!
//! - a root in the commands report's format, added to the native mods' root,
//!   so every player's `ClientboundCommandsPacket` carries the Java commands
//!   their level allows (the client parses, colours and completes them from
//!   it, as from any server's tree);
//! - a Horizon [`Dispatcher`] the server parses a command line with.
//!
//! `Game::run_command` asks [`Game::run_java_command`] first: a line whose
//! first word is a Java command is parsed with that dispatcher, the `requires`
//! lambdas on its path are run with the real source, and the node's
//! `Command.run` runs with the parsed values ([`NativeJar::run_command`]). Its
//! `sendSuccess`/`sendFailure`/`sendSystemMessage` lines reach the source's
//! chat or the console like a vanilla command's feedback. A run the façade
//! cannot carry (a member it does not model, its time budget, an interpreter
//! refusal) answers vanilla's `command.failed` and logs why; it never panics
//! the server.
//!
//! What is left out, with a log line: a root vanilla or a native mod already
//! has (Brigadier would merge into vanilla's node; Horizon's vanilla commands
//! are not Brigadier nodes), and an argument whose type the façade does not
//! model (a mod's own `ArgumentType`, with the nodes below it). Custom
//! `SuggestionProvider`s do not run: clients complete from the argument type.

use std::{
    collections::{BTreeMap, VecDeque},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use horizon_client::command::{Arg, ArgType, Dispatcher, Node, NodeKind as HNodeKind};
use horizon_client::text::Text;
use horizon_native_java::{
    ExecutionBudget, NativeJar, StdHost, Value,
    brigadier::{
        ArgumentKind, CommandArgument, CommandInvocation, CommandOutcome, CommandSource,
        CommandText, CommandTree, MessageKind, NOBODY, NodeKind, Requirement,
    },
    events::{EventContext, EventFacade, EventFiring, GameEvent, GameListener},
};
use serde_json::{Map, Value as Json, json};

use super::{
    Game,
    execution_role::ExecutionRole,
    java_events::handler_role,
    java_payloads::{HandlerRole, ResidentHost, lock_resident},
};

/// The command id every executable Java node carries in the server's
/// dispatcher; the node itself says which callback runs.
const JAVA_COMMAND: &str = "java";

/// How long one registration callback, one command and one `requires`
/// predicate may run. Registration builds a tree once; a command may do real
/// work; a predicate runs on every use of its node.
const REGISTER_BUDGET: Duration = Duration::from_secs(2);
const COMMAND_BUDGET: Duration = Duration::from_secs(1);
const REQUIREMENT_BUDGET: Duration = Duration::from_millis(50);

/// Brigadier requirement levels in the report's names (`commands.json`).
const LEVELS: [&str; 5] = ["", "moderators", "gamemasters", "admins", "owners"];

/// One jar that registers commands.
struct CommandJar {
    file: String,
    jar: Arc<NativeJar>,
    host: ResidentHost,
    listeners: Vec<GameListener>,
}

/// One node of the merged tree: what it parses, and which jar's callbacks it
/// runs.
#[derive(Clone, Debug)]
struct MergedNode {
    kind: NodeKind,
    children: Vec<usize>,
    /// (jar index, `Command` marker).
    command: Option<(usize, Value)>,
    /// (jar index, predicate) of a `requires` lambda, re-run with the real
    /// source before the command runs.
    predicate: Option<(usize, Value)>,
    /// The operator level the node needs in the tree ([`NOBODY`]: none).
    level: u8,
    redirect: Option<usize>,
    /// A `fork` or redirect modifier: not run (the source would change).
    modifier: bool,
}

/// The merged command tree of one role.
#[derive(Default)]
struct RoleCommands {
    jars: Vec<CommandJar>,
    /// The registration listeners ran.
    registered: bool,
    /// Node 0 is the root.
    nodes: Vec<MergedNode>,
    /// The tree the server parses with, numbered like `nodes`.
    dispatcher: Dispatcher,
    /// The root in the report's format, for the players' trees.
    report: Json,
    /// What was left out of the tree, with why (for tests and the log).
    left_out: Vec<String>,
}

static STORE: Mutex<BTreeMap<HandlerRole, RoleCommands>> = Mutex::new(BTreeMap::new());

/// Some role kept a jar that registers commands: `run_command`'s fast path.
static ANY: AtomicBool = AtomicBool::new(false);

fn store() -> std::sync::MutexGuard<'static, BTreeMap<HandlerRole, RoleCommands>> {
    STORE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Forgets a previous batch's command jars and tree for one role.
pub(super) fn clear_as(role: HandlerRole) {
    let mut store = store();
    store.remove(&role);
    ANY.store(!store.is_empty(), Ordering::Relaxed);
}

/// Keeps one bootstrapped jar if it registered a command callback. Returns
/// how many registration listeners it has.
pub(super) fn adopt(
    role: ExecutionRole,
    path: &Path,
    jar: &Arc<NativeJar>,
    host: &ResidentHost,
) -> usize {
    if role.is_client() {
        return 0;
    }
    let listeners: Vec<GameListener> = lock_resident(host)
        .game_listeners(jar, true)
        .listeners
        .into_iter()
        .filter(|l| l.event == GameEvent::RegisterCommands)
        .collect();
    if listeners.is_empty() {
        return 0;
    }
    let count = listeners.len();
    let file = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let mut store = store();
    let entry = store.entry(handler_role(role)).or_default();
    entry.jars.push(CommandJar {
        file,
        jar: Arc::clone(jar),
        host: Arc::clone(host),
        listeners,
    });
    // A later jar joins a tree that was already built: build it again.
    entry.registered = false;
    ANY.store(true, Ordering::Relaxed);
    count
}

/// The firing a registration listener runs under: only its `dedicated` flag
/// matters (`Commands.CommandSelection`).
fn registration_firing(dedicated: bool) -> EventFiring {
    EventFiring::new(
        GameEvent::RegisterCommands,
        Arc::new(EventFacade {
            dedicated,
            ..EventFacade::default()
        }),
        EventContext::default(),
    )
}

/// One jar's registration: runs its listeners and returns the tree they
/// built, with each predicate's tree level (by tree node id). A listener that
/// fails is logged and the rest still run, as a Fabric invoker would not stop
/// the server for one bad callback.
fn register_jar(
    file: &str,
    jar: &NativeJar,
    host: &mut StdHost,
    listeners: &[GameListener],
    dedicated: bool,
) -> (CommandTree, BTreeMap<usize, u8>) {
    for listener in listeners {
        let mut firing = registration_firing(dedicated);
        let budget = ExecutionBudget::within(REGISTER_BUDGET);
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            jar.run_game_listener(listener, &mut firing, host, &budget)
        }));
        let reason = match run {
            Ok(Ok(_)) => continue,
            Ok(Err(error)) => error.to_string(),
            Err(_) => "panicked".to_owned(),
        };
        let target = listener.target();
        log::warn!(
            "Horizon: Java mod {file} command registration {}#{} refused: {}",
            target.owner,
            target.name,
            bounded(&reason)
        );
    }
    let tree = host.command_tree();
    let mut levels = BTreeMap::new();
    for (id, node) in tree.nodes.iter().enumerate() {
        let Some(requirement @ Requirement::Predicate(_)) = &node.requirement else {
            continue;
        };
        let budget = ExecutionBudget::within(REQUIREMENT_BUDGET);
        let level = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            jar.requirement_level(requirement, dedicated, host, &budget)
        }));
        // A predicate that reaches past the façade is for owners in the tree;
        // the real source runs it again before the command does.
        let level = match level {
            Ok(Ok(level)) => level,
            Ok(Err(error)) => {
                log::debug!(
                    "Horizon: Java mod {file} `requires` of /{} classified as owners-only: {}",
                    node.kind.name(),
                    bounded(&error.to_string())
                );
                4
            }
            Err(_) => 4,
        };
        levels.insert(id, level);
    }
    (tree, levels)
}

fn bounded(reason: &str) -> String {
    let mut reason = reason.replace(['\n', '\r'], " ");
    if reason.chars().count() > 240 {
        reason = reason.chars().take(239).collect();
        reason.push('…');
    }
    reason
}

/// The Horizon argument type of a façade argument kind, `None` when the
/// façade does not model it.
fn arg_type(kind: &ArgumentKind) -> Option<ArgType> {
    Some(match kind {
        ArgumentKind::Bool => ArgType::Bool,
        ArgumentKind::Integer { min, max } => ArgType::Integer {
            min: *min,
            max: *max,
        },
        // Horizon has no long type: a double over the same bounds reads the
        // same digits (as a received `brigadier:long` node does).
        ArgumentKind::Long { min, max } => ArgType::Double {
            min: *min as f64,
            max: *max as f64,
        },
        ArgumentKind::Float { min, max } => ArgType::Float {
            min: *min,
            max: *max,
        },
        ArgumentKind::Double { min, max } => ArgType::Double {
            min: *min,
            max: *max,
        },
        ArgumentKind::Word => ArgType::Word,
        ArgumentKind::Quoted => ArgType::Str,
        ArgumentKind::Greedy => ArgType::Greedy,
        ArgumentKind::Entity { single, players } => ArgType::Entity {
            single: *single,
            players: *players,
        },
        ArgumentKind::GameProfile => ArgType::GameProfile,
        ArgumentKind::BlockPos => ArgType::BlockPos,
        ArgumentKind::ColumnPos => ArgType::ColumnPos,
        ArgumentKind::Vec3 { center } => ArgType::Vec3 { center: *center },
        ArgumentKind::Vec2 => ArgType::Vec2,
        ArgumentKind::Identifier => ArgType::Id("none"),
        ArgumentKind::Uuid => ArgType::Uuid,
        ArgumentKind::Dimension => ArgType::Dimension,
        ArgumentKind::Item => ArgType::Item,
        ArgumentKind::BlockState => ArgType::BlockState,
        ArgumentKind::Component => ArgType::Component,
        ArgumentKind::Message => ArgType::Message,
        ArgumentKind::GameMode => ArgType::GameMode,
        ArgumentKind::Time { min } => ArgType::Time { min: *min },
        ArgumentKind::Resource {
            registry,
            tag: false,
        } => ArgType::Res(intern(registry)),
        ArgumentKind::Resource {
            registry,
            tag: true,
        } => ArgType::ResOrTag(intern(registry)),
        ArgumentKind::HexColor => ArgType::HexColor,
        ArgumentKind::Unmodelled(_) => return None,
    })
}

/// A registry id kept for the life of the game (argument types hold
/// `&'static str`): each distinct id is stored once, and a server names a
/// bounded set of registries.
fn intern(name: &str) -> &'static str {
    static NAMES: Mutex<BTreeMap<String, &'static str>> = Mutex::new(BTreeMap::new());
    let mut names = NAMES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(&kept) = names.get(name) {
        return kept;
    }
    let kept: &'static str = Box::leak(name.to_owned().into_boxed_str());
    names.insert(name.to_owned(), kept);
    kept
}

/// The report's parser and properties of an argument kind.
fn report_parser(kind: &ArgumentKind) -> Option<(&'static str, Json)> {
    let bounds = |min: Option<Json>, max: Option<Json>| {
        let mut p = Map::new();
        if let Some(min) = min {
            p.insert("min".into(), min);
        }
        if let Some(max) = max {
            p.insert("max".into(), max);
        }
        if p.is_empty() {
            Json::Null
        } else {
            Json::Object(p)
        }
    };
    Some(match kind {
        ArgumentKind::Bool => ("brigadier:bool", Json::Null),
        ArgumentKind::Integer { min, max } => (
            "brigadier:integer",
            bounds(
                (*min != i32::MIN).then(|| json!(min)),
                (*max != i32::MAX).then(|| json!(max)),
            ),
        ),
        ArgumentKind::Long { min, max } => (
            "brigadier:long",
            bounds(
                (*min != i64::MIN).then(|| json!(min)),
                (*max != i64::MAX).then(|| json!(max)),
            ),
        ),
        ArgumentKind::Float { min, max } => (
            "brigadier:float",
            bounds(
                (*min != f32::MIN).then(|| json!(min)),
                (*max != f32::MAX).then(|| json!(max)),
            ),
        ),
        ArgumentKind::Double { min, max } => (
            "brigadier:double",
            bounds(
                (*min != f64::MIN).then(|| json!(min)),
                (*max != f64::MAX).then(|| json!(max)),
            ),
        ),
        ArgumentKind::Word => ("brigadier:string", json!({"type": "word"})),
        ArgumentKind::Quoted => ("brigadier:string", json!({"type": "phrase"})),
        ArgumentKind::Greedy => ("brigadier:string", json!({"type": "greedy"})),
        ArgumentKind::Entity { single, players } => (
            "minecraft:entity",
            json!({
                "amount": if *single { "single" } else { "multiple" },
                "type": if *players { "players" } else { "entities" },
            }),
        ),
        ArgumentKind::GameProfile => ("minecraft:game_profile", Json::Null),
        ArgumentKind::BlockPos => ("minecraft:block_pos", Json::Null),
        ArgumentKind::ColumnPos => ("minecraft:column_pos", Json::Null),
        ArgumentKind::Vec3 { .. } => ("minecraft:vec3", Json::Null),
        ArgumentKind::Vec2 => ("minecraft:vec2", Json::Null),
        ArgumentKind::Identifier => ("minecraft:resource_location", Json::Null),
        ArgumentKind::Uuid => ("minecraft:uuid", Json::Null),
        ArgumentKind::Dimension => ("minecraft:dimension", Json::Null),
        ArgumentKind::Item => ("minecraft:item_stack", Json::Null),
        ArgumentKind::BlockState => ("minecraft:block_state", Json::Null),
        ArgumentKind::Component => ("minecraft:component", Json::Null),
        ArgumentKind::Message => ("minecraft:message", Json::Null),
        ArgumentKind::GameMode => ("minecraft:gamemode", Json::Null),
        ArgumentKind::Time { min } => ("minecraft:time", json!({"min": min})),
        ArgumentKind::Resource { registry, tag } => (
            if *tag {
                "minecraft:resource_or_tag"
            } else {
                "minecraft:resource"
            },
            json!({"registry": registry}),
        ),
        ArgumentKind::HexColor => ("minecraft:hex_color", Json::Null),
        ArgumentKind::Unmodelled(_) => return None,
    })
}

impl RoleCommands {
    /// Merges one jar's tree under the merged root: Brigadier's `addChild`
    /// (a same-named child takes the newer command and merges the
    /// grandchildren). `taken` names the roots vanilla or a native mod has.
    fn merge_jar(
        &mut self,
        jar: usize,
        tree: &CommandTree,
        levels: &BTreeMap<usize, u8>,
        taken: &dyn Fn(&str) -> bool,
    ) {
        let file = self.jars[jar].file.clone();
        if self.nodes.is_empty() {
            self.nodes.push(MergedNode {
                kind: NodeKind::Root,
                children: Vec::new(),
                command: None,
                predicate: None,
                level: 0,
                redirect: None,
                modifier: false,
            });
        }
        // Tree node id -> merged node id, for redirects and shared nodes.
        let mut map: BTreeMap<usize, usize> = BTreeMap::from([(0, 0)]);
        let mut redirects = Vec::new();
        let Some(root) = tree.nodes.first() else {
            return;
        };
        for &child in &root.children {
            let name = tree.nodes[child].kind.name();
            if taken(name) {
                self.left_out.push(format!(
                    "{file}: /{name}: vanilla or a native mod has /{name}"
                ));
                continue;
            }
            self.merge_node(
                jar,
                tree,
                levels,
                0,
                child,
                &mut map,
                &mut redirects,
                &file,
                0,
            );
        }
        for (merged, target) in redirects {
            match map.get(&target) {
                Some(&to) => self.nodes[merged].redirect = Some(to),
                None => self.left_out.push(format!(
                    "{file}: /{}: its redirect target was left out",
                    self.nodes[merged].kind.name()
                )),
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn merge_node(
        &mut self,
        jar: usize,
        tree: &CommandTree,
        levels: &BTreeMap<usize, u8>,
        parent: usize,
        id: usize,
        map: &mut BTreeMap<usize, usize>,
        redirects: &mut Vec<(usize, usize)>,
        file: &str,
        depth: usize,
    ) {
        if depth > 64 {
            return;
        }
        let node = &tree.nodes[id];
        if let NodeKind::Argument {
            name,
            kind: ArgumentKind::Unmodelled(class),
        } = &node.kind
        {
            self.left_out.push(format!(
                "{file}: <{name}>: argument type {class} is not modelled"
            ));
            return;
        }
        // The same tree node reached again (Brigadier shares built nodes):
        // link the one already merged.
        if let Some(&merged) = map.get(&id) {
            if !self.nodes[parent].children.contains(&merged) {
                self.nodes[parent].children.push(merged);
            }
            return;
        }
        let name = node.kind.name();
        let existing = self.nodes[parent]
            .children
            .iter()
            .copied()
            .find(|&c| self.nodes[c].kind.name() == name);
        let level = match &node.requirement {
            Some(Requirement::Level(level)) => *level,
            Some(Requirement::Predicate(_)) => levels.get(&id).copied().unwrap_or(4),
            None => 0,
        };
        let merged = match existing {
            Some(existing) => {
                if let Some(command) = &node.command {
                    self.nodes[existing].command = Some((jar, command.clone()));
                }
                existing
            }
            None => {
                self.nodes.push(MergedNode {
                    kind: node.kind.clone(),
                    children: Vec::new(),
                    command: node.command.clone().map(|c| (jar, c)),
                    predicate: match &node.requirement {
                        Some(Requirement::Predicate(p)) => Some((jar, p.clone())),
                        _ => None,
                    },
                    level,
                    redirect: None,
                    modifier: node.modifier,
                });
                let merged = self.nodes.len() - 1;
                self.nodes[parent].children.push(merged);
                if let Some(target) = node.redirect {
                    redirects.push((merged, target));
                }
                merged
            }
        };
        map.insert(id, merged);
        for &child in &node.children {
            self.merge_node(
                jar,
                tree,
                levels,
                merged,
                child,
                map,
                redirects,
                file,
                depth + 1,
            );
        }
    }

    /// Builds the server's dispatcher and the players' report root from the
    /// merged nodes.
    fn finish(&mut self) {
        let mut d = Dispatcher::new();
        d.nodes = self
            .nodes
            .iter()
            .map(|node| Node {
                kind: match &node.kind {
                    NodeKind::Root => HNodeKind::Root,
                    NodeKind::Literal(name) => HNodeKind::Literal(name.clone()),
                    NodeKind::Argument { name, kind } => HNodeKind::Argument {
                        name: name.clone(),
                        ty: arg_type(kind).unwrap_or(ArgType::Word),
                    },
                },
                children: node.children.clone(),
                command: node.command.as_ref().map(|_| JAVA_COMMAND),
                redirect: node.redirect,
                permission: node.level,
                suggests: None,
                restricted: node.level > 0,
            })
            .collect();
        if d.nodes.is_empty() {
            d = Dispatcher::new();
        }
        self.dispatcher = d;
        let paths = self.paths();
        let children = self.report_children(0, &paths, 0);
        self.report = if children.is_empty() {
            Json::Null
        } else {
            json!({"type": "root", "children": children})
        };
    }

    /// Each node's first path from the root, by name (report redirects are
    /// paths).
    fn paths(&self) -> BTreeMap<usize, Vec<String>> {
        let mut paths = BTreeMap::from([(0usize, Vec::new())]);
        let mut queue = VecDeque::from([0usize]);
        while let Some(n) = queue.pop_front() {
            for &c in self.nodes.get(n).map_or(&[][..], |n| &n.children) {
                if !paths.contains_key(&c) {
                    let mut path = paths[&n].clone();
                    path.push(self.nodes[c].kind.name().to_owned());
                    paths.insert(c, path);
                    queue.push_back(c);
                }
            }
        }
        paths
    }

    /// The report's children of `node`: a node nobody may use, and one that
    /// neither runs, nor leads anywhere, nor redirects, is left out (the
    /// report reads such a dead end as a fork to the root).
    fn report_children(
        &self,
        node: usize,
        paths: &BTreeMap<usize, Vec<String>>,
        depth: usize,
    ) -> Map<String, Json> {
        let mut out = Map::new();
        if depth > 64 {
            return out;
        }
        for &c in &self.nodes[node].children {
            let child = &self.nodes[c];
            if child.level >= NOBODY {
                continue;
            }
            let mut entry = Map::new();
            match &child.kind {
                NodeKind::Literal(_) => {
                    entry.insert("type".into(), json!("literal"));
                }
                NodeKind::Argument { kind, .. } => {
                    let Some((parser, properties)) = report_parser(kind) else {
                        continue;
                    };
                    entry.insert("type".into(), json!("argument"));
                    entry.insert("parser".into(), json!(parser));
                    if !properties.is_null() {
                        entry.insert("properties".into(), properties);
                    }
                }
                NodeKind::Root => continue,
            }
            let executable = child.command.is_some();
            entry.insert("executable".into(), json!(executable));
            if child.level > 0 {
                entry.insert(
                    "permissions".into(),
                    json!({"type": "require", "permission": {"level": LEVELS[usize::from(child.level.min(4))]}}),
                );
            }
            let redirect = child.redirect.and_then(|r| paths.get(&r));
            if let Some(path) = redirect {
                entry.insert("redirect".into(), json!(path));
            }
            let grandchildren = self.report_children(c, paths, depth + 1);
            if grandchildren.is_empty() && !executable && redirect.is_none() {
                continue;
            }
            if !grandchildren.is_empty() {
                entry.insert("children".into(), Json::Object(grandchildren));
            }
            out.insert(child.kind.name().to_owned(), Json::Object(entry));
        }
        out
    }

    /// Whether `name` is a root command of this tree.
    fn has_root(&self, name: &str) -> bool {
        self.nodes.first().is_some_and(|root| {
            root.children
                .iter()
                .any(|&c| self.nodes[c].kind.name() == name)
        })
    }
}

/// Runs every jar's registration for `role` once, merges the trees, and
/// builds the parse tree and report. `taken` names roots vanilla or a native
/// mod has. A role with no command jar does nothing.
fn ensure_registered(role: HandlerRole, dedicated: bool, taken: &dyn Fn(&str) -> bool) {
    let mut store = store();
    let Some(commands) = store.get_mut(&role) else {
        return;
    };
    if commands.registered {
        return;
    }
    commands.registered = true;
    commands.nodes.clear();
    commands.left_out.clear();
    for index in 0..commands.jars.len() {
        let (tree, levels) = {
            let entry = &commands.jars[index];
            let mut host = lock_resident(&entry.host);
            register_jar(
                &entry.file,
                &entry.jar,
                &mut host,
                &entry.listeners,
                dedicated,
            )
        };
        commands.merge_jar(index, &tree, &levels, taken);
    }
    commands.finish();
    let roots = commands.nodes.first().map_or(0, |root| root.children.len());
    if roots > 0 {
        log::info!(
            "Horizon: Java commands: {roots} root command(s) from {} jar(s)",
            commands.jars.len()
        );
    }
    for reason in &commands.left_out {
        log::warn!("Horizon: a Java mod's command was left out: {reason}");
    }
}

/// One parsed argument: read as is, or resolved against the game first (a
/// selector's players, coordinates relative to the source).
enum Parsed {
    Ready(String, CommandArgument),
    Deferred(String, Arg),
}

/// The parsed arguments of the context that runs, as the jar reads them.
fn invocation_arguments(
    commands: &RoleCommands,
    context: &horizon_client::command::Context,
) -> Result<Vec<Parsed>, String> {
    let mut out = Vec::new();
    for parsed in &context.args {
        // The node that parsed it says what the jar asked for.
        let kind = context
            .nodes
            .iter()
            .find_map(|&(node, _)| match &commands.nodes[node].kind {
                NodeKind::Argument { name, kind } if *name == parsed.name => Some(kind.clone()),
                _ => None,
            });
        let name = parsed.name.clone();
        let value = match (&parsed.value, kind) {
            (Arg::Bool(v), _) => CommandArgument::Bool(*v),
            (Arg::Int(v), _) => CommandArgument::Int(*v),
            (Arg::Float(v), _) => CommandArgument::Float(f64::from(*v)),
            (Arg::Double(v), Some(ArgumentKind::Long { .. })) => CommandArgument::Long(*v as i64),
            (Arg::Double(v), _) => CommandArgument::Double(*v),
            (Arg::Str(v), Some(ArgumentKind::Uuid)) => {
                let hex: String = v.chars().filter(|c| *c != '-').collect();
                CommandArgument::Uuid(u128::from_str_radix(&hex, 16).map_err(|e| e.to_string())?)
            }
            (Arg::Str(v) | Arg::Resource(v), Some(ArgumentKind::Dimension)) => {
                CommandArgument::Dimension(horizon_client::command::args::normalize(v))
            }
            (Arg::Str(v) | Arg::Resource(v), Some(ArgumentKind::Identifier)) => {
                CommandArgument::Identifier(horizon_client::command::args::normalize(v))
            }
            (Arg::Str(v), _) => CommandArgument::Str(v.clone()),
            (other @ (Arg::Entity(_) | Arg::Coords(_)), _) => {
                out.push(Parsed::Deferred(name, other.clone()));
                continue;
            }
            // A value no getter of the façade reads (an item, a block
            // state...): the jar's getter refuses if it asks for it.
            _ => continue,
        };
        out.push(Parsed::Ready(name, value));
    }
    Ok(out)
}

/// What one line did, for the caller to report.
#[derive(Debug, PartialEq)]
enum Ran {
    /// The command ran and returned.
    Ok,
    /// A vanilla-shaped failure the source sees as is.
    Failed(Text),
    /// The façade could not carry the run; the source sees `command.failed`.
    Refused(String),
}

fn text_of(text: &CommandText) -> Text {
    // A styled component or one with translation arguments: vanilla's JSON,
    // decoded as a server's chat component is, and sent to vanilla clients
    // as the jar built it.
    if let Some(json) = text
        .component
        .as_deref()
        .and_then(|json| serde_json::from_str::<Json>(json).ok())
    {
        return horizon_client::component::to_text(&json, &mut |s| Text::literal(s))
            .with_component(json);
    }
    if text.translate {
        Text::translate(&text.text, &[])
    } else {
        Text::literal(text.text.clone())
    }
}

impl Game<'_> {
    /// The role whose Java commands this `Game` runs: a server's.
    fn java_command_role(&self) -> Option<HandlerRole> {
        if self.remote.is_some() {
            return None;
        }
        let server = self.serving.is_some()
            || self.options.dedicated.is_some()
            || !self.options.execution_role.is_client();
        server.then(|| handler_role(self.options.execution_role))
    }

    /// The roots vanilla, Horizon or a native mod (`native`) has.
    fn java_command_taken(native: &Json) -> impl Fn(&str) -> bool + '_ {
        move |name: &str| {
            horizon_client::command::vanilla::game_dispatcher()
                .find(name)
                .is_some()
                || native["children"].get(name).is_some()
        }
    }

    /// The Java commands' root in the report's format, merged into the
    /// native mods' `tree` (`register_native_commands`): what every player's
    /// command tree adds. Runs the registration callbacks the first time.
    pub(super) fn merge_java_command_tree(&mut self, tree: &mut Json) {
        if !ANY.load(Ordering::Relaxed) {
            return;
        }
        let Some(role) = self.java_command_role() else {
            return;
        };
        let dedicated = self.options.dedicated.is_some();
        let native = tree.clone();
        ensure_registered(role, dedicated, &Self::java_command_taken(&native));
        let store = store();
        let Some(Json::Object(java)) = store.get(&role).map(|c| &c.report["children"]) else {
            return;
        };
        if tree.is_null() {
            *tree = json!({"type": "root", "children": {}});
        }
        if let Some(children) = tree["children"].as_object_mut() {
            for (name, node) in java {
                children.entry(name.clone()).or_insert_with(|| node.clone());
            }
        }
    }

    /// `run_command`'s first step: a line whose first word is a Java root
    /// command runs here. `None` when it is not one (the game's own
    /// dispatcher takes it); otherwise whether it succeeded.
    pub(super) fn run_java_command(&mut self, line: &str) -> Option<bool> {
        if !ANY.load(Ordering::Relaxed) {
            return None;
        }
        let role = self.java_command_role()?;
        let first = line.split(' ').next().unwrap_or_default();
        {
            let registered = store().get(&role).is_some_and(|c| c.registered);
            if !registered {
                ensure_registered(
                    role,
                    self.options.dedicated.is_some(),
                    &Self::java_command_taken(&Json::Null),
                );
            }
            if !store().get(&role).is_some_and(|c| c.has_root(first)) {
                return None;
            }
        }
        let ran = self.run_java_line(role, line);
        Some(match ran {
            Ran::Ok => true,
            Ran::Failed(text) => {
                self.command_failure(text);
                false
            }
            Ran::Refused(reason) => {
                log::warn!(
                    "Horizon: Java command /{} failed: {}",
                    line.split(' ').next().unwrap_or_default(),
                    bounded(&reason)
                );
                self.command_failure(Text::translate("command.failed", &[]));
                false
            }
        })
    }

    /// The source of the command running now: the guest whose turn it is,
    /// or the server (console, command blocks).
    fn java_command_source(&self) -> CommandSource {
        let level = self.ui.registries.permission;
        let guest = self
            .lan_acting
            .and_then(|conn| self.lan.as_ref()?.hub.guest(conn));
        match guest {
            Some(g) => CommandSource {
                name: g.name.clone(),
                level,
                player: Some(g.uuid),
                dimension: g.dimension.clone(),
            },
            None => CommandSource {
                name: "Server".into(),
                level,
                player: None,
                dimension: self.dim.id().to_owned(),
            },
        }
    }

    /// The UUIDs of the players a selector selected, or why not.
    fn java_selected_players(
        &mut self,
        selector: &horizon_client::command::args::Selector,
    ) -> Result<Vec<u128>, String> {
        let acting = self
            .lan_acting
            .and_then(|conn| self.lan.as_ref()?.hub.guest(conn).map(|g| g.uuid));
        let mut out = Vec::new();
        for target in self.select(selector) {
            let uuid = match target {
                super::commands::Target::Player => acting,
                super::commands::Target::Guest(conn) => self
                    .lan
                    .as_ref()
                    .and_then(|h| h.hub.guest(conn))
                    .map(|g| g.uuid),
                _ => {
                    return Err(
                        "an entity argument selected a non-player, which the façade does not model"
                            .into(),
                    );
                }
            };
            out.extend(uuid);
        }
        Ok(out)
    }

    fn run_java_line(&mut self, role: HandlerRole, line: &str) -> Ran {
        let source = self.java_command_source();
        // Parse and find what runs, then let go of the store: resolving a
        // selector and delivering feedback reach the rest of the game.
        let (path, context, input) = {
            let store = store();
            let Some(commands) = store.get(&role) else {
                return Ran::Refused("no Java command tree".into());
            };
            let parse = commands.dispatcher.parse(line, &self.ui.registries);
            let context = match parse.executable() {
                Ok((context, _)) => context.clone(),
                Err(error) => return Ran::Failed(error.with_context()),
            };
            // Every node on the way, through redirects.
            let mut path = Vec::new();
            let mut at = Some(&parse.context);
            while let Some(c) = at {
                path.extend(c.nodes.iter().map(|&(node, _)| node));
                at = c.child.as_deref();
            }
            (path, context, parse.input.clone())
        };
        let parsed = {
            let store = store();
            let Some(commands) = store.get(&role) else {
                return Ran::Refused("no Java command tree".into());
            };
            match invocation_arguments(commands, &context) {
                Ok(parsed) => parsed,
                Err(error) => return Ran::Refused(error),
            }
        };
        // Selectors and coordinates resolve against the game, outside the
        // store's lock.
        let mut arguments = Vec::new();
        for item in parsed {
            match item {
                Parsed::Ready(name, value) => arguments.push((name, value)),
                Parsed::Deferred(name, Arg::Entity(selector)) => {
                    match self.java_selected_players(&selector) {
                        Ok(players) => arguments.push((name, CommandArgument::Players(players))),
                        Err(error) => return Ran::Refused(error),
                    }
                }
                Parsed::Deferred(name, Arg::Coords(coords)) => {
                    let p = self.resolve_coords(&coords);
                    arguments.push((name, CommandArgument::BlockPos(p.map(|v| v.floor() as i32))));
                }
                Parsed::Deferred(..) => {}
            }
        }
        let facade = self.java_event_facade(None);
        let invocation = CommandInvocation {
            input,
            source: source.clone(),
            arguments,
        };
        let run = {
            let store = store();
            let Some(commands) = store.get(&role) else {
                return Ran::Refused("no Java command tree".into());
            };
            // The `requires` lambdas on the way, with the real source.
            for &node in &path {
                let merged = &commands.nodes[node];
                if merged.modifier {
                    return Ran::Refused(format!(
                        "/{}: a redirect that changes the source is not modelled",
                        merged.kind.name()
                    ));
                }
                if let Some((jar, predicate)) = &merged.predicate {
                    let entry = &commands.jars[*jar];
                    let budget = ExecutionBudget::within(REQUIREMENT_BUDGET);
                    let allowed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        entry.jar.check_requirement(
                            &Requirement::Predicate(predicate.clone()),
                            &source,
                            Arc::clone(&facade),
                            &mut lock_resident(&entry.host),
                            &budget,
                        )
                    }));
                    match allowed {
                        Ok(Ok(true)) => {}
                        Ok(Ok(false)) => {
                            return Ran::Failed(Text::translate("command.unknown.command", &[]));
                        }
                        Ok(Err(error)) => return Ran::Refused(format!("requires: {error}")),
                        Err(_) => return Ran::Refused("requires: panicked".into()),
                    }
                }
            }
            let Some(&(last, _)) = context.nodes.last() else {
                return Ran::Refused("an empty command context".into());
            };
            let Some((jar, command)) = commands.nodes[last].command.clone() else {
                return Ran::Failed(Text::translate("command.unknown.command", &[]));
            };
            let entry = &commands.jars[jar];
            let budget = ExecutionBudget::within(COMMAND_BUDGET);
            let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                entry.jar.run_command(
                    &command,
                    &invocation,
                    Arc::clone(&facade),
                    &mut lock_resident(&entry.host),
                    &budget,
                )
            }));
            match run {
                Ok(run) => (run, entry.file.clone()),
                Err(_) => return Ran::Refused("the command panicked".into()),
            }
        };
        let (run, file) = run;
        log::debug!(
            "Horizon: Java command {line:?} from {file}: {:?}, {} message(s)",
            run.outcome,
            run.messages.len()
        );
        for message in &run.messages {
            let text = text_of(&message.text);
            match message.kind {
                MessageKind::Success { broadcast: true } => self.feedback(text),
                MessageKind::Success { broadcast: false } => {
                    let silent = self.command_source.is_some_and(|s| s.silent);
                    let accepts = self.console_broadcast.is_some()
                        || self.ui.rules.bool("send_command_feedback");
                    if !silent && accepts {
                        self.system_message(text);
                    }
                }
                MessageKind::Failure => self.command_failure(text),
                MessageKind::System => self.system_message(text),
                MessageKind::Player(uuid) => {
                    if source.player == Some(uuid) {
                        self.system_message(text);
                    } else {
                        self.native_tell(uuid, text);
                    }
                }
            }
        }
        match run.outcome {
            CommandOutcome::Returned(_) => Ran::Ok,
            CommandOutcome::Syntax(text) => Ran::Failed(text_of(&text)),
            CommandOutcome::Refused(reason) => Ran::Refused(format!("{file}: {reason}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_client::command::Registries;
    use std::{io::Write, path::PathBuf, process::Command};

    #[test]
    fn a_styled_component_shows_its_colours_and_reaches_vanilla_unchanged() {
        let json = json!({
            "text": "Set to ",
            "color": "gray",
            "extra": [{"text": "7", "color": "gold", "bold": true}],
        });
        let text = text_of(&CommandText {
            text: "Set to 7".into(),
            translate: false,
            component: Some(json.to_string()),
        });
        assert_eq!(text.plain(), "Set to 7");
        // Gray parent, gold bold sibling: ChatFormatting's GRAY and GOLD.
        assert_eq!(text.spans[0].style.color, Some(0xAAAAAA));
        assert_eq!(text.spans[1].style.color, Some(0xFFAA00));
        assert_eq!(text.spans[1].style.bold, Some(true));
        assert_eq!(text.component(), json);
    }

    /// The Brigadier, game and loader classes the mod compiles against; only
    /// the mod's own classes go into the jar (the host answers these).
    const STUBS: &[(&str, &str)] = &[
        (
            "net/fabricmc/fabric/api/event/Event.java",
            "package net.fabricmc.fabric.api.event;\n\
             public abstract class Event<T> { public void register(T listener) {} }",
        ),
        (
            "net/fabricmc/fabric/api/command/v2/CommandRegistrationCallback.java",
            "package net.fabricmc.fabric.api.command.v2;\n\
             public interface CommandRegistrationCallback {\n\
               net.fabricmc.fabric.api.event.Event<CommandRegistrationCallback> EVENT = null;\n\
               void register(com.mojang.brigadier.CommandDispatcher<net.minecraft.commands.CommandSourceStack> d,\n\
                 Object c, Object s); }",
        ),
        (
            "com/mojang/brigadier/CommandDispatcher.java",
            "package com.mojang.brigadier;\n\
             public class CommandDispatcher<S> {\n\
               public Object register(com.mojang.brigadier.builder.LiteralArgumentBuilder<S> b) { return null; } }",
        ),
        (
            "com/mojang/brigadier/Command.java",
            "package com.mojang.brigadier;\n\
             public interface Command<S> { int run(com.mojang.brigadier.context.CommandContext<S> c); }",
        ),
        (
            "com/mojang/brigadier/context/CommandContext.java",
            "package com.mojang.brigadier.context;\n\
             public class CommandContext<S> { public S getSource() { return null; } }",
        ),
        (
            "com/mojang/brigadier/builder/ArgumentBuilder.java",
            "package com.mojang.brigadier.builder;\n\
             public abstract class ArgumentBuilder<S, T extends ArgumentBuilder<S, T>> {\n\
               public T then(ArgumentBuilder<S, ?> a) { return null; }\n\
               public T executes(com.mojang.brigadier.Command<S> c) { return null; }\n\
               public T requires(java.util.function.Predicate<S> p) { return null; } }",
        ),
        (
            "com/mojang/brigadier/builder/LiteralArgumentBuilder.java",
            "package com.mojang.brigadier.builder;\n\
             public class LiteralArgumentBuilder<S> extends ArgumentBuilder<S, LiteralArgumentBuilder<S>> {}",
        ),
        (
            "com/mojang/brigadier/builder/RequiredArgumentBuilder.java",
            "package com.mojang.brigadier.builder;\n\
             public class RequiredArgumentBuilder<S, T> extends ArgumentBuilder<S, RequiredArgumentBuilder<S, T>> {}",
        ),
        (
            "com/mojang/brigadier/arguments/IntegerArgumentType.java",
            "package com.mojang.brigadier.arguments;\n\
             public class IntegerArgumentType {\n\
               public static IntegerArgumentType integer(int min, int max) { return null; }\n\
               public static int getInteger(com.mojang.brigadier.context.CommandContext<?> c, String n) { return 0; } }",
        ),
        (
            "net/minecraft/commands/Commands.java",
            "package net.minecraft.commands;\n\
             import com.mojang.brigadier.builder.*;\n\
             public class Commands {\n\
               public static final Object LEVEL_GAMEMASTERS = null;\n\
               public static LiteralArgumentBuilder<CommandSourceStack> literal(String s) { return null; }\n\
               public static <T> RequiredArgumentBuilder<CommandSourceStack, T> argument(String s,\n\
                 com.mojang.brigadier.arguments.IntegerArgumentType t) { return null; }\n\
               public static java.util.function.Predicate<CommandSourceStack> hasPermission(Object c) { return null; } }",
        ),
        (
            "net/minecraft/commands/CommandSourceStack.java",
            "package net.minecraft.commands;\n\
             public class CommandSourceStack {\n\
               public void sendSuccess(java.util.function.Supplier<net.minecraft.network.chat.Component> s, boolean b) {} }",
        ),
        (
            "net/minecraft/network/chat/Component.java",
            "package net.minecraft.network.chat;\n\
             public interface Component { static MutableComponent literal(String s) { return null; } }",
        ),
        (
            "net/minecraft/network/chat/MutableComponent.java",
            "package net.minecraft.network.chat;\n\
             public class MutableComponent implements Component {}",
        ),
    ];

    /// `/jtool add <amount>`, `/jtool reset` (game masters) and, registered
    /// again on the same root, `/jtool info`; plus `/give`, which vanilla has.
    const MOD: &str = r#"
package demo;
import com.mojang.brigadier.arguments.IntegerArgumentType;
import net.fabricmc.fabric.api.command.v2.CommandRegistrationCallback;
import net.minecraft.commands.Commands;
import net.minecraft.network.chat.Component;
public final class ToolMod {
  public void onInitialize() {
    CommandRegistrationCallback.EVENT.register((dispatcher, context, selection) -> {
      dispatcher.register(Commands.literal("jtool")
        .then(Commands.literal("add").then(Commands.argument("amount", IntegerArgumentType.integer(1, 64))
          .executes(ctx -> {
            int amount = IntegerArgumentType.getInteger(ctx, "amount");
            ctx.getSource().sendSuccess(() -> Component.literal("added " + amount), false);
            return amount;
          })))
        .then(Commands.literal("reset").requires(Commands.hasPermission(Commands.LEVEL_GAMEMASTERS))
          .executes(ctx -> 0)));
      dispatcher.register(Commands.literal("jtool").then(Commands.literal("info").executes(ctx -> 7)));
      dispatcher.register(Commands.literal("give").executes(ctx -> 1));
    });
  }
}
"#;

    /// The mod's jar, or `None` without a Java toolchain.
    fn tool_jar() -> Option<PathBuf> {
        let root =
            std::env::temp_dir().join(format!("horizon-java-commands-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut sources = Vec::new();
        for (path, text) in STUBS.iter().chain([("demo/ToolMod.java", MOD)].iter()) {
            let file = root.join("src").join(path);
            std::fs::create_dir_all(file.parent()?).ok()?;
            std::fs::write(&file, text).ok()?;
            sources.push(file);
        }
        let javac = std::env::var_os("JAVA_HOME")
            .map(|home| PathBuf::from(home).join("bin/javac"))
            .unwrap_or_else(|| PathBuf::from("javac"));
        let classes = root.join("classes");
        let status = Command::new(javac)
            .args(["--release", "17", "-d"])
            .arg(&classes)
            .args(&sources)
            .status()
            .ok()?;
        if !status.success() {
            return None;
        }
        let jar = root.join("tool.jar");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&jar).ok()?);
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("fabric.mod.json", options).ok()?;
        zip.write_all(
            br#"{"id":"tool_mod","version":"1","entrypoints":{"main":["demo.ToolMod"]}}"#,
        )
        .ok()?;
        for entry in std::fs::read_dir(classes.join("demo")).ok()? {
            let path = entry.ok()?.path();
            let name = format!("demo/{}", path.file_name()?.to_string_lossy());
            zip.start_file(name, options).ok()?;
            zip.write_all(&std::fs::read(&path).ok()?).ok()?;
        }
        zip.finish().ok()?;
        Some(jar)
    }

    fn registries(permission: u8) -> Registries {
        Registries {
            permission,
            ..Registries::default()
        }
    }

    #[test]
    fn a_registered_command_joins_the_tree_parses_suggests_and_runs() {
        let _guard = super::super::java_mods::test_lock();
        let role = HandlerRole::DedicatedServer;
        clear_as(role);
        let Some(path) = tool_jar() else {
            eprintln!("skipped: no javac");
            return;
        };
        let jar = Arc::new(NativeJar::from_path(&path).unwrap());
        let entry = jar.discover_entrypoint("main").unwrap();
        let mut host = StdHost::new();
        jar.execute(&entry.class, entry.method.as_deref(), "main", &mut host)
            .unwrap();
        let resident: ResidentHost = Arc::new(Mutex::new(host));
        assert_eq!(
            adopt(ExecutionRole::DedicatedServer, &path, &jar, &resident),
            1
        );
        let taken = |name: &str| {
            horizon_client::command::vanilla::game_dispatcher()
                .find(name)
                .is_some()
        };
        ensure_registered(role, true, &taken);

        let store = store();
        let commands = &store[&role];
        // `/give` is vanilla's: left out, with why.
        assert!(commands.has_root("jtool") && !commands.has_root("give"));
        assert!(
            commands.left_out.iter().any(|r| r.contains("/give")),
            "{:?}",
            commands.left_out
        );

        // The players' tree: both registrations of /jtool in one root, the
        // operator-only node marked, the bounds on the integer.
        let jtool = &commands.report["children"]["jtool"]["children"];
        let mut names: Vec<&String> = jtool.as_object().unwrap().keys().collect();
        names.sort();
        assert_eq!(names, ["add", "info", "reset"]);
        assert_eq!(
            jtool["reset"]["permissions"]["permission"]["level"],
            "gamemasters"
        );
        let amount = &jtool["add"]["children"]["amount"];
        assert_eq!(amount["parser"], "brigadier:integer");
        assert_eq!(amount["properties"], json!({"min": 1, "max": 64}));
        assert_eq!(amount["executable"], true);

        // A player of level 0 completes what they may use, and cannot reach
        // the operator node; the server parses the line with the same tree.
        let player = registries(0);
        let typed = commands.dispatcher.parse("jtool ", &player);
        let (_, list) = commands.dispatcher.client_suggestions(&typed, 6, &player);
        let offered: Vec<&str> = list.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(offered, ["add", "info"]);
        assert!(
            commands
                .dispatcher
                .parse("jtool reset", &player)
                .executable()
                .is_err()
        );
        assert!(
            commands
                .dispatcher
                .parse("jtool reset", &registries(2))
                .executable()
                .is_ok()
        );
        assert!(
            commands
                .dispatcher
                .parse("jtool add 65", &player)
                .executable()
                .is_err()
        );

        // `/jtool add 5` runs the jar's callback with the parsed int.
        let parse = commands.dispatcher.parse("jtool add 5", &player);
        let (context, _) = parse.executable().unwrap();
        let arguments: Vec<(String, CommandArgument)> = invocation_arguments(commands, context)
            .unwrap()
            .into_iter()
            .filter_map(|p| match p {
                Parsed::Ready(name, value) => Some((name, value)),
                Parsed::Deferred(..) => None,
            })
            .collect();
        assert_eq!(arguments, [("amount".to_owned(), CommandArgument::Int(5))]);
        let &(last, _) = context.nodes.last().unwrap();
        let (jar_index, command) = commands.nodes[last].command.clone().unwrap();
        let entry = &commands.jars[jar_index];
        let run = entry.jar.run_command(
            &command,
            &CommandInvocation {
                input: "jtool add 5".into(),
                source: CommandSource {
                    name: "Server".into(),
                    level: 4,
                    player: None,
                    dimension: "minecraft:overworld".into(),
                },
                arguments,
            },
            Arc::new(EventFacade::default()),
            &mut lock_resident(&entry.host),
            &ExecutionBudget::within(Duration::from_secs(5)),
        );
        assert_eq!(run.outcome, CommandOutcome::Returned(5));
        assert_eq!(run.messages.len(), 1);
        assert_eq!(
            run.messages[0].kind,
            MessageKind::Success { broadcast: false }
        );
        assert_eq!(text_of(&run.messages[0].text).plain(), "added 5");
        drop(store);
        clear_as(role);
    }
}
