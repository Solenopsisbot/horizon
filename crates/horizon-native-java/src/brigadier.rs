//! Commands a jar registers through Brigadier, recorded as a typed tree and
//! run later with a bounded `CommandContext`.
//!
//! A Fabric mod registers its commands in a `CommandRegistrationCallback`
//! (`dispatcher.register(Commands.literal("x").then(...).executes(...))`), a
//! NeoForge/Forge mod in a `RegisterCommandsEvent` listener. Both are game
//! listeners (`events.rs`, [`crate::events::GameEvent::RegisterCommands`]): the
//! game fires them when its server builds the command tree and hands the
//! callback the bounded dispatcher marker this module answers.
//!
//! # The tree
//!
//! Brigadier's own semantics, on an arena of node ids instead of Java objects,
//! so the sharing and merging of a real dispatcher are kept exactly:
//!
//! - `then(builder)` builds the child *at that call* into the builder's
//!   argument container, merging a same-named child (`CommandNode.addChild`:
//!   the newer command wins, grandchildren merge recursively);
//! - `build()` makes a fresh node whose children are the container's (shared,
//!   not copied), and `register` adds it under the root the same way and
//!   returns the built node, not the merged one;
//! - `executes`/`requires`/`redirect`/`fork`/`suggests` keep their argument as
//!   a callback marker. Nothing runs at registration.
//!
//! [`StdHost::command_tree`] snapshots what the root reaches as a
//! [`CommandTree`] the game merges into its own.
//!
//! # Running a command
//!
//! [`NativeJar::run_command`] runs a node's `Command.run(CommandContext)` with
//! a `CommandContext` marker whose `getArgument`/`StringArgumentType.getString`
//! /`EntityArgument.getPlayer`... answer the values the game parsed, and whose
//! `getSource()` is a `CommandSourceStack` stand-in: `sendSuccess`,
//! `sendFailure`, `sendSystemMessage` and a player's `sendSystemMessage` are
//! collected as [`CommandMessage`]s for the game to deliver, `permissions()`
//! answers the source's operator level, `getPlayer`/`getServer`/`getLevel`
//! answer the event façade's markers (`events.rs`), so a player marker answers
//! its name and UUID there. The run is strict: any call outside the modelled
//! surface refuses the run ([`CommandOutcome::Refused`]) instead of carrying on
//! with an invented value.
//!
//! # Permissions (26.3)
//!
//! 26.3 replaced `hasPermission(int)` with permission sets. The façade models
//! the level-based subset vanilla's own commands use: `Commands.LEVEL_*` and
//! `Commands.hasPermission(check)` (a level predicate), `Permissions.COMMANDS_*`
//! and `new Permission.HasCommandLevel(PermissionLevel.X)`, `source
//! .permissions().hasPermission(permission)`, `LevelBasedPermissionSet` and
//! `PermissionLevel`; plus fabric-permissions-api's `Permissions.check`/
//! `require`, answered as the library does without a permissions provider (the
//! fallback level or default). A `requires` lambda keeps its marker:
//! [`NativeJar::requirement_level`] classifies it for the tree the clients get,
//! and the game evaluates it again with the real source before running.

use std::{collections::BTreeMap, sync::Arc};

use crate::{
    ExecutionBudget, MethodRef, NativeError, NativeJar, StdHost, Value, arg_count, boolean_value,
    component_value,
    events::{EventContext, EventFacade, EventFiring, FacadePlayer, GameEvent},
    expect_ref, expect_string, identifier_value, parse_callback_marker, return_desc, stable_hash,
    uuid_value,
};

/// The dispatcher a registration callback receives.
pub const DISPATCHER: &str = "brigadier/dispatcher";
/// The `CommandBuildContext` a registration callback receives: only passed
/// along (to `ItemArgument.item(context)` and the like).
pub const BUILD_CONTEXT: &str = "brigadier/build-context";
const CONTEXT: &str = "brigadier/context";
const COMMAND_SELECTION: &str = "net/minecraft/commands/Commands$CommandSelection";
const SYNTAX_EXCEPTION: &str = "com/mojang/brigadier/exceptions/CommandSyntaxException";
const PERMISSION_LEVEL: &str = "net/minecraft/server/permissions/PermissionLevel";
const PERMISSION_LEVELS: [&str; 5] = ["ALL", "MODERATORS", "GAMEMASTERS", "ADMINS", "OWNERS"];
/// `net.minecraft.ChatFormatting`'s constants (26.3), in declaration order.
const CHAT_FORMATTING: [&str; 22] = [
    "BLACK",
    "DARK_BLUE",
    "DARK_GREEN",
    "DARK_AQUA",
    "DARK_RED",
    "DARK_PURPLE",
    "GOLD",
    "GRAY",
    "DARK_GRAY",
    "BLUE",
    "GREEN",
    "AQUA",
    "RED",
    "LIGHT_PURPLE",
    "YELLOW",
    "WHITE",
    "OBFUSCATED",
    "BOLD",
    "STRIKETHROUGH",
    "UNDERLINE",
    "ITALIC",
    "RESET",
];
/// A requirement no source meets (fabric-permissions-api's `require(perm,
/// false)` without a provider). One past the owners' level.
pub const NOBODY: u8 = 5;

/// Nodes, builders and argument types one host may hold. A real mod builds a
/// few hundred nodes; past this the registration refuses rather than growing
/// host state without bound.
const MAX_NODES: usize = 8192;
const MAX_BUILDERS: usize = 8192;
const MAX_ARGUMENT_TYPES: usize = 2048;
/// Messages one command run may queue, and exception types one host may name.
const MAX_MESSAGES: usize = 256;
const MAX_EXCEPTION_TYPES: usize = 1024;
/// How deep a `CommandNode.addChild` merge may recurse.
const MAX_MERGE_DEPTH: usize = 64;

/// What an argument node parses, as the jar asked for it.
#[derive(Clone, Debug, PartialEq)]
pub enum ArgumentKind {
    Bool,
    Integer {
        min: i32,
        max: i32,
    },
    Long {
        min: i64,
        max: i64,
    },
    Float {
        min: f32,
        max: f32,
    },
    Double {
        min: f64,
        max: f64,
    },
    /// `StringArgumentType.word()`.
    Word,
    /// `StringArgumentType.string()`: a word or a quoted phrase.
    Quoted,
    /// `StringArgumentType.greedyString()`.
    Greedy,
    Entity {
        single: bool,
        players: bool,
    },
    GameProfile,
    BlockPos,
    ColumnPos,
    Vec3 {
        center: bool,
    },
    Vec2,
    Identifier,
    Uuid,
    Dimension,
    Item,
    BlockState,
    Component,
    Message,
    GameMode,
    Time {
        min: i32,
    },
    /// `ResourceArgument.resource(context, key)` (`tag: false`) or
    /// `ResourceOrTagArgument.resourceOrTag` over the registry `registry`
    /// (`minecraft:worldgen/biome`).
    Resource {
        registry: String,
        tag: bool,
    },
    HexColor,
    /// A type the façade does not model (a mod's own `ArgumentType` class or
    /// an unlisted vanilla one), by the class or factory that made it.
    Unmodelled(String),
}

/// One node's kind.
#[derive(Clone, Debug, PartialEq)]
pub enum NodeKind {
    Root,
    Literal(String),
    Argument { name: String, kind: ArgumentKind },
}

impl NodeKind {
    /// `CommandNode.getName`.
    pub fn name(&self) -> &str {
        match self {
            NodeKind::Root => "",
            NodeKind::Literal(name) | NodeKind::Argument { name, .. } => name,
        }
    }
}

/// A node's `requires` predicate.
#[derive(Clone, Debug, PartialEq)]
pub enum Requirement {
    /// An operator level (`Commands.hasPermission(Commands.LEVEL_GAMEMASTERS)`):
    /// a source of that level or higher may use it. [`NOBODY`] for none.
    Level(u8),
    /// A jar predicate (`source -> source.permissions().hasPermission(...)`).
    Predicate(Value),
}

/// One node of a recorded tree.
#[derive(Clone, Debug, PartialEq)]
pub struct TreeNode {
    pub kind: NodeKind,
    /// Child node ids, in registration order.
    pub children: Vec<usize>,
    /// The `Command` callback marker run when parsing ends here.
    pub command: Option<Value>,
    pub requirement: Option<Requirement>,
    /// The node this one redirects to (`redirect`, `fork`).
    pub redirect: Option<usize>,
    /// A `fork` or a redirect with a modifier: the source changes on the way,
    /// which the game does not model.
    pub modifier: bool,
    /// A custom `SuggestionProvider` (`suggests`).
    pub suggests: Option<Value>,
}

impl TreeNode {
    fn new(kind: NodeKind) -> Self {
        Self {
            kind,
            children: Vec::new(),
            command: None,
            requirement: None,
            redirect: None,
            modifier: false,
            suggests: None,
        }
    }
}

/// What a dispatcher's root reaches: node 0 is the root, ids index `nodes`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CommandTree {
    pub nodes: Vec<TreeNode>,
}

impl CommandTree {
    /// The root's children: one per command the jar registered.
    pub fn roots(&self) -> impl Iterator<Item = (usize, &TreeNode)> {
        self.nodes
            .first()
            .into_iter()
            .flat_map(|root| root.children.iter())
            .map(|&id| (id, &self.nodes[id]))
    }
}

/// An `ArgumentBuilder` the jar is still chaining.
#[derive(Clone, Debug)]
struct Builder {
    /// The node `build()` makes.
    template: TreeNode,
    /// Brigadier's builder keeps its arguments in a `RootCommandNode`, so
    /// `then` merges same-named children; this is that container's id.
    arguments: usize,
}

/// The Brigadier state of one host: the arena every built node lives in, the
/// builders being chained, the argument types, and the running command.
#[derive(Clone, Debug, Default)]
pub struct BrigadierState {
    nodes: Vec<TreeNode>,
    /// The dispatcher's root, once something touched it.
    root: Option<usize>,
    builders: BTreeMap<usize, Builder>,
    next_builder: usize,
    arguments: Vec<ArgumentKind>,
    /// Levels of `new Permission.HasCommandLevel(level)` and `new
    /// PermissionCheck.Require(permission)` objects, by object marker.
    permission_objects: BTreeMap<String, u8>,
    /// Messages of `new SimpleCommandExceptionType(message)`, by marker.
    exception_types: BTreeMap<String, CommandText>,
    /// The text each `CommandSyntaxException` this host raised or created
    /// carries, by its message, so the outcome keeps a translation key.
    exception_texts: BTreeMap<String, CommandText>,
    next_exception: usize,
    exec: Option<Box<Execution>>,
}

/// The source a command runs as (`CommandSourceStack`).
#[derive(Clone, Debug, PartialEq)]
pub struct CommandSource {
    /// `getTextName`: the player's name, `Server` for the console.
    pub name: String,
    /// Operator level 0..=4.
    pub level: u8,
    /// The player, when a player runs it.
    pub player: Option<u128>,
    /// Dimension id of `getLevel`.
    pub dimension: String,
}

/// One parsed argument value, as the game resolved it.
#[derive(Clone, Debug, PartialEq)]
pub enum CommandArgument {
    Bool(bool),
    Int(i32),
    Long(i64),
    /// A `float` (already rounded to `f32`) or a `double`.
    Float(f64),
    Double(f64),
    Str(String),
    /// An entity selector that selected only players.
    Players(Vec<u128>),
    BlockPos([i32; 3]),
    Identifier(String),
    Uuid(u128),
    /// A dimension id.
    Dimension(String),
}

/// One run of one command.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandInvocation {
    /// The whole command line, without its slash (`CommandContext.getInput`).
    pub input: String,
    pub source: CommandSource,
    /// The arguments of the context that runs, by name.
    pub arguments: Vec<(String, CommandArgument)>,
}

/// Text a command produced: a literal, or a translation key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandText {
    pub text: String,
    pub translate: bool,
}

impl CommandText {
    fn key(key: &str) -> Self {
        Self {
            text: key.to_owned(),
            translate: true,
        }
    }

    /// The text of a component marker (`component/literal/...`).
    fn of(value: &Value) -> Self {
        let raw = expect_string(value);
        if let Some(key) = raw.strip_prefix("component/translatable/") {
            return Self::key(key);
        }
        Self {
            text: raw
                .strip_prefix("component/literal/")
                .unwrap_or(raw)
                .to_owned(),
            translate: false,
        }
    }
}

/// Where a message goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageKind {
    /// `sendSuccess(supplier, broadcastToAdmins)`.
    Success { broadcast: bool },
    /// `sendFailure`.
    Failure,
    /// `source.sendSystemMessage`.
    System,
    /// `player.sendSystemMessage`, to that player.
    Player(u128),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandMessage {
    pub kind: MessageKind,
    pub text: CommandText,
}

/// How a command run ended.
#[derive(Clone, Debug, PartialEq)]
pub enum CommandOutcome {
    /// `Command.run` returned this result.
    Returned(i32),
    /// It threw a `CommandSyntaxException`: vanilla shows its message.
    Syntax(CommandText),
    /// The façade does not model what it reached, or the interpreter refused
    /// it (the reason, for the log); the player sees vanilla's
    /// `command.failed`.
    Refused(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct CommandRun {
    pub outcome: CommandOutcome,
    /// What the command said, in order, up to its end.
    pub messages: Vec<CommandMessage>,
}

/// A message as the command queued it: a `sendSuccess` keeps its supplier,
/// which runs after the command returns (the interpreter cannot call back
/// into the jar from a host call).
#[derive(Clone, Debug)]
enum Pending {
    Supplier { supplier: Value, broadcast: bool },
    Text(CommandMessage),
}

#[derive(Clone, Debug)]
struct Execution {
    invocation: Option<CommandInvocation>,
    /// The sources this execution named: 0 is the one the command runs as.
    sources: Vec<CommandSource>,
    pending: Vec<Pending>,
}

fn marker(text: String) -> Value {
    Value::Ref(Some(text))
}

fn node_marker(id: usize) -> Value {
    marker(format!("brigadier/node/{id}"))
}

fn level_name(level: u8) -> &'static str {
    PERMISSION_LEVELS[usize::from(level.min(4))]
}

fn permission_level_marker(level: u8) -> Value {
    marker(format!("enum/{PERMISSION_LEVEL}/{}", level_name(level)))
}

fn uuid_text(uuid: u128) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        (uuid >> 96) as u32,
        (uuid >> 80) as u16,
        (uuid >> 64) as u16,
        (uuid >> 48) as u16,
        uuid & 0xffff_ffff_ffff
    )
}

/// The id after `prefix` in a `brigadier/<kind>/<id>` marker.
fn marker_id(value: &Value, prefix: &str) -> Option<usize> {
    expect_ref(value)?.strip_prefix(prefix)?.parse().ok()
}

/// The operator level a permission-ish marker stands for.
fn level_of(value: &Value) -> Option<u8> {
    let text = expect_ref(value)?;
    for prefix in [
        "brigadier/permission/",
        "brigadier/permission-check/",
        "brigadier/permission-set/",
        "brigadier/requires/",
    ] {
        if let Some(level) = text.strip_prefix(prefix) {
            return level.parse().ok();
        }
    }
    let name = text.strip_prefix(&format!("enum/{PERMISSION_LEVEL}/"))?;
    PERMISSION_LEVELS
        .iter()
        .position(|n| *n == name)
        .map(|at| at as u8)
}

/// A host-raised `CommandSyntaxException` the jar can catch.
fn syntax_error(message: &str) -> NativeError {
    NativeError::UncaughtException {
        class: SYNTAX_EXCEPTION.into(),
        message: Some(message.to_owned()),
        frame: None,
    }
}

fn int_arg(args: &[Value], at: usize) -> Option<i32> {
    match args.get(at)? {
        Value::Int(v) => Some(*v),
        _ => None,
    }
}

fn float_arg(args: &[Value], at: usize) -> Option<f64> {
    match args.get(at)? {
        Value::Float(v) => Some(*v),
        Value::Int(v) => Some(f64::from(*v)),
        _ => None,
    }
}

fn long_arg(args: &[Value], at: usize) -> Option<i64> {
    match args.get(at)? {
        Value::Long(v) => Some(*v),
        Value::Int(v) => Some(i64::from(*v)),
        _ => None,
    }
}

/// The argument type a vanilla or Brigadier static factory makes, if the
/// façade models it.
fn argument_factory(r: &MethodRef, args: &[Value]) -> Option<ArgumentKind> {
    let owner = r.owner.as_str();
    let class = owner.rsplit('/').next()?;
    let brigadier = owner.starts_with("com/mojang/brigadier/arguments/");
    let vanilla = owner.starts_with("net/minecraft/commands/arguments/");
    if !brigadier && !vanilla {
        return None;
    }
    Some(match (class, r.name.as_str()) {
        ("BoolArgumentType", "bool") => ArgumentKind::Bool,
        ("IntegerArgumentType", "integer") => ArgumentKind::Integer {
            min: int_arg(args, 0).unwrap_or(i32::MIN),
            max: int_arg(args, 1).unwrap_or(i32::MAX),
        },
        ("LongArgumentType", "longArg") => ArgumentKind::Long {
            min: long_arg(args, 0).unwrap_or(i64::MIN),
            max: long_arg(args, 1).unwrap_or(i64::MAX),
        },
        ("FloatArgumentType", "floatArg") => ArgumentKind::Float {
            min: float_arg(args, 0).map_or(f32::MIN, |v| v as f32),
            max: float_arg(args, 1).map_or(f32::MAX, |v| v as f32),
        },
        ("DoubleArgumentType", "doubleArg") => ArgumentKind::Double {
            min: float_arg(args, 0).unwrap_or(f64::MIN),
            max: float_arg(args, 1).unwrap_or(f64::MAX),
        },
        ("StringArgumentType", "word") => ArgumentKind::Word,
        ("StringArgumentType", "string") => ArgumentKind::Quoted,
        ("StringArgumentType", "greedyString") => ArgumentKind::Greedy,
        ("EntityArgument", "entity") => ArgumentKind::Entity {
            single: true,
            players: false,
        },
        ("EntityArgument", "entities") => ArgumentKind::Entity {
            single: false,
            players: false,
        },
        ("EntityArgument", "player") => ArgumentKind::Entity {
            single: true,
            players: true,
        },
        ("EntityArgument", "players") => ArgumentKind::Entity {
            single: false,
            players: true,
        },
        ("GameProfileArgument", "gameProfile") => ArgumentKind::GameProfile,
        ("BlockPosArgument", "blockPos") => ArgumentKind::BlockPos,
        ("ColumnPosArgument", "columnPos") => ArgumentKind::ColumnPos,
        ("Vec3Argument", "vec3") => ArgumentKind::Vec3 {
            center: int_arg(args, 0).is_none_or(|v| v != 0),
        },
        ("Vec2Argument", "vec2") => ArgumentKind::Vec2,
        ("IdentifierArgument", "id") => ArgumentKind::Identifier,
        ("UuidArgument", "uuid") => ArgumentKind::Uuid,
        ("DimensionArgument", "dimension") => ArgumentKind::Dimension,
        ("ItemArgument", "item") => ArgumentKind::Item,
        ("BlockStateArgument", "block") => ArgumentKind::BlockState,
        ("ComponentArgument", "textComponent") => ArgumentKind::Component,
        ("MessageArgument", "message") => ArgumentKind::Message,
        ("GameModeArgument", "gameMode") => ArgumentKind::GameMode,
        ("TimeArgument", "time") => ArgumentKind::Time {
            min: int_arg(args, 0).unwrap_or(0),
        },
        ("HexColorArgument", "hexColor") => ArgumentKind::HexColor,
        ("ResourceArgument" | "ResourceOrTagArgument", "resource" | "resourceOrTag") => {
            // The key is `Registries.X`, a `registry/<id>` marker.
            let registry = expect_ref(args.get(1)?)?
                .strip_prefix("registry/")?
                .to_owned();
            ArgumentKind::Resource {
                registry,
                tag: r.name == "resourceOrTag",
            }
        }
        _ => return None,
    })
}

impl BrigadierState {
    fn push_node(&mut self, node: TreeNode) -> Result<usize, NativeError> {
        if self.nodes.len() >= MAX_NODES {
            return Err(NativeError::Metadata(format!(
                "brigadier: more than {MAX_NODES} command nodes"
            )));
        }
        self.nodes.push(node);
        Ok(self.nodes.len() - 1)
    }

    fn root(&mut self) -> Result<usize, NativeError> {
        match self.root {
            Some(root) => Ok(root),
            None => {
                let root = self.push_node(TreeNode::new(NodeKind::Root))?;
                self.root = Some(root);
                Ok(root)
            }
        }
    }

    fn new_builder(&mut self, kind: NodeKind) -> Result<Value, NativeError> {
        if self.builders.len() >= MAX_BUILDERS {
            return Err(NativeError::Metadata(format!(
                "brigadier: more than {MAX_BUILDERS} argument builders"
            )));
        }
        let arguments = self.push_node(TreeNode::new(NodeKind::Root))?;
        let id = self.next_builder;
        self.next_builder += 1;
        self.builders.insert(
            id,
            Builder {
                template: TreeNode::new(kind),
                arguments,
            },
        );
        Ok(marker(format!("brigadier/builder/{id}")))
    }

    fn new_argument_type(&mut self, kind: ArgumentKind) -> Result<Value, NativeError> {
        if self.arguments.len() >= MAX_ARGUMENT_TYPES {
            return Err(NativeError::Metadata(format!(
                "brigadier: more than {MAX_ARGUMENT_TYPES} argument types"
            )));
        }
        self.arguments.push(kind);
        Ok(marker(format!(
            "brigadier/argtype/{}",
            self.arguments.len() - 1
        )))
    }

    /// The argument type an `argument(name, type)` call passed.
    fn argument_kind(&self, value: &Value) -> ArgumentKind {
        if let Some(id) = marker_id(value, "brigadier/argtype/")
            && let Some(kind) = self.arguments.get(id)
        {
            return kind.clone();
        }
        let text = expect_string(value);
        ArgumentKind::Unmodelled(match text.split_once('#') {
            Some((class, _)) => class.to_owned(),
            None if text.is_empty() => "null".to_owned(),
            None => text.to_owned(),
        })
    }

    /// `ArgumentBuilder.build()`.
    fn build(&mut self, builder: usize) -> Result<usize, NativeError> {
        let Builder {
            template,
            arguments,
        } = self
            .builders
            .get(&builder)
            .cloned()
            .ok_or_else(|| NativeError::Metadata("brigadier: unknown builder".into()))?;
        let children = self.nodes[arguments].children.clone();
        let mut node = template;
        node.children = children;
        self.push_node(node)
    }

    /// `CommandNode.addChild`: a same-named child takes the newer command and
    /// merges the grandchildren; otherwise the node is added as is.
    fn add_child(&mut self, parent: usize, child: usize, depth: usize) -> Result<(), NativeError> {
        if depth > MAX_MERGE_DEPTH {
            return Err(NativeError::Metadata(
                "brigadier: command tree merge nested too deep".into(),
            ));
        }
        if matches!(self.nodes[child].kind, NodeKind::Root) {
            return Err(NativeError::UncaughtException {
                class: "java/lang/UnsupportedOperationException".into(),
                message: Some(
                    "Cannot add a RootCommandNode as a child to any other CommandNode".into(),
                ),
                frame: None,
            });
        }
        let name = self.nodes[child].kind.name().to_owned();
        let existing = self.nodes[parent]
            .children
            .iter()
            .copied()
            .find(|&c| self.nodes[c].kind.name() == name);
        match existing {
            Some(existing) if existing != child => {
                if let Some(command) = self.nodes[child].command.clone() {
                    self.nodes[existing].command = Some(command);
                }
                for grandchild in self.nodes[child].children.clone() {
                    self.add_child(existing, grandchild, depth + 1)?;
                }
            }
            Some(_) => {}
            None => self.nodes[parent].children.push(child),
        }
        Ok(())
    }

    /// The node a `then`/`redirect`/`addChild` argument names: a built node,
    /// or a builder (built at that call, as `then(ArgumentBuilder)` does).
    fn node_of(&mut self, value: &Value) -> Result<usize, NativeError> {
        if let Some(id) = marker_id(value, "brigadier/node/").filter(|&id| id < self.nodes.len()) {
            return Ok(id);
        }
        if let Some(id) = marker_id(value, "brigadier/builder/") {
            return self.build(id);
        }
        if value == &Value::Ref(Some(DISPATCHER.into())) {
            return self.root();
        }
        Err(NativeError::Metadata(format!(
            "brigadier: {} is not a command node",
            expect_string(value)
        )))
    }

    /// The requirement a `requires(predicate)` argument stands for.
    fn requirement(value: &Value) -> Requirement {
        match expect_ref(value).and_then(|t| t.strip_prefix("brigadier/requires/")) {
            Some(level) => Requirement::Level(level.parse().unwrap_or(NOBODY)),
            None => Requirement::Predicate(value.clone()),
        }
    }

    /// The tree the dispatcher's root reaches (children and redirects),
    /// renumbered from 0.
    fn tree(&self) -> CommandTree {
        let Some(root) = self.root else {
            return CommandTree::default();
        };
        let mut ids = BTreeMap::new();
        let mut order = vec![root];
        ids.insert(root, 0usize);
        let mut at = 0;
        while at < order.len() {
            let node = &self.nodes[order[at]];
            for next in node.children.iter().chain(node.redirect.iter()) {
                if !ids.contains_key(next) {
                    ids.insert(*next, order.len());
                    order.push(*next);
                }
            }
            at += 1;
        }
        let nodes = order
            .iter()
            .map(|&id| {
                let mut node = self.nodes[id].clone();
                node.children = node.children.iter().map(|c| ids[c]).collect();
                node.redirect = node.redirect.map(|r| ids[&r]);
                node
            })
            .collect();
        CommandTree { nodes }
    }

    fn exec(&self) -> Option<&Execution> {
        self.exec.as_deref()
    }

    fn source(&self, value: &Value) -> Option<&CommandSource> {
        let id = marker_id(value, "brigadier/source/")?;
        self.exec()?.sources.get(id)
    }

    fn queue(&mut self, pending: Pending) -> Result<(), NativeError> {
        let exec = self
            .exec
            .as_deref_mut()
            .ok_or_else(|| NativeError::Metadata("brigadier: no command is running".into()))?;
        if exec.pending.len() >= MAX_MESSAGES {
            return Err(NativeError::Metadata(format!(
                "brigadier: a command sent more than {MAX_MESSAGES} messages"
            )));
        }
        exec.pending.push(pending);
        Ok(())
    }

    /// A new `CommandSyntaxException` marker carrying `text`.
    fn new_exception(
        &mut self,
        text: CommandText,
        host_messages: &mut BTreeMap<String, Option<String>>,
    ) -> Value {
        let marker_text = format!("{SYNTAX_EXCEPTION}#brigadier{}", self.next_exception);
        self.next_exception += 1;
        if self.exception_texts.len() < MAX_EXCEPTION_TYPES {
            self.exception_texts.insert(text.text.clone(), text.clone());
        }
        host_messages.insert(marker_text.clone(), Some(text.text));
        marker(marker_text)
    }
}

impl StdHost {
    /// The command tree the jar's registration callbacks built, from the
    /// dispatcher's root. Empty when it registered none.
    pub fn command_tree(&self) -> CommandTree {
        self.brigadier.tree()
    }

    /// Every text the Brigadier state holds that may name a jar object (a
    /// callback's captures, a requirement lambda), for the heap's roots.
    pub(crate) fn brigadier_root_texts<'a>(&'a self, out: &mut Vec<&'a str>) {
        for node in &self.brigadier.nodes {
            for value in
                node.command
                    .iter()
                    .chain(node.suggests.iter())
                    .chain(match &node.requirement {
                        Some(Requirement::Predicate(value)) => Some(value),
                        _ => None,
                    })
            {
                if let Value::Ref(Some(text)) = value {
                    out.push(text);
                }
            }
        }
        for builder in self.brigadier.builders.values() {
            let template = &builder.template;
            for value in template.command.iter().chain(template.suggests.iter()) {
                if let Value::Ref(Some(text)) = value {
                    out.push(text);
                }
            }
            if let Some(Requirement::Predicate(Value::Ref(Some(text)))) = &template.requirement {
                out.push(text);
            }
        }
        out.extend(self.brigadier.permission_objects.keys().map(String::as_str));
        out.extend(self.brigadier.exception_types.keys().map(String::as_str));
    }

    /// Statics of the command surface: permission levels, checks and sets,
    /// `Commands.CommandSelection` and its `includeDedicated` flags (an
    /// instance field of a host object reaches the host as a static read).
    pub(crate) fn brigadier_static(&self, r: &MethodRef) -> Option<Value> {
        let level = |l: u8| {
            Some(marker(format!(
                "brigadier/{}/{l}",
                match r.descriptor.as_str() {
                    "Lnet/minecraft/server/permissions/Permission;" => "permission",
                    "Lnet/minecraft/server/permissions/PermissionCheck;" => "permission-check",
                    _ => "permission-set",
                }
            )))
        };
        match (r.owner.as_str(), r.name.as_str()) {
            ("net/minecraft/commands/Commands", name) if name.starts_with("LEVEL_") => {
                let at = PERMISSION_LEVELS
                    .iter()
                    .position(|n| *n == &name["LEVEL_".len()..])?;
                level(at as u8)
            }
            ("net/minecraft/server/permissions/Permissions", name) => match name {
                "COMMANDS_MODERATOR" => level(1),
                "COMMANDS_GAMEMASTER" | "COMMANDS_ENTITY_SELECTORS" => level(2),
                "COMMANDS_ADMIN" => level(3),
                "COMMANDS_OWNER" => level(4),
                // Every player may chat; only levels are modelled.
                "CHAT_SEND_MESSAGES"
                | "CHAT_SEND_COMMANDS"
                | "CHAT_RECEIVE_PLAYER_MESSAGES"
                | "CHAT_RECEIVE_SYSTEM_MESSAGES" => level(0),
                _ => None,
            },
            ("net/minecraft/server/permissions/PermissionSet", "ALL_PERMISSIONS") => level(4),
            ("net/minecraft/server/permissions/PermissionSet", "NO_PERMISSIONS") => level(0),
            ("net/minecraft/server/permissions/LevelBasedPermissionSet", name) => {
                let at = ["ALL", "MODERATOR", "GAMEMASTER", "ADMIN", "OWNER"]
                    .iter()
                    .position(|n| *n == name)?;
                level(at as u8)
            }
            (PERMISSION_LEVEL, name) => PERMISSION_LEVELS
                .contains(&name)
                .then(|| marker(format!("enum/{PERMISSION_LEVEL}/{name}"))),
            // The formatting a command styles its feedback with: a constant
            // to pass to `withStyle`, which keeps the text.
            ("net/minecraft/ChatFormatting", name) if CHAT_FORMATTING.contains(&name) => {
                Some(marker(format!("enum/net/minecraft/ChatFormatting/{name}")))
            }
            (COMMAND_SELECTION, "ALL" | "DEDICATED" | "INTEGRATED") => {
                Some(marker(format!("enum/{COMMAND_SELECTION}/{}", r.name)))
            }
            (COMMAND_SELECTION, "includeDedicated" | "includeIntegrated") => {
                let dedicated = self.game_event.as_ref().is_some_and(|f| f.facade.dedicated);
                let wanted = r.name == "includeDedicated";
                Some(Value::Int(i32::from(dedicated == wanted)))
            }
            _ => None,
        }
    }

    /// Static calls of the command surface: builder and argument-type
    /// factories, `Commands.hasPermission`, the argument getters of a running
    /// command, and fabric-permissions-api.
    pub(crate) fn brigadier_invoke(
        &mut self,
        r: &MethodRef,
        args: &[Value],
    ) -> Option<Result<Value, NativeError>> {
        let owner = r.owner.as_str();
        let name = r.name.as_str();
        match (owner, name) {
            (
                "net/minecraft/commands/Commands"
                | "com/mojang/brigadier/builder/LiteralArgumentBuilder",
                "literal",
            ) => {
                let literal = expect_string(args.first()?).to_owned();
                return Some(self.brigadier.new_builder(NodeKind::Literal(literal)));
            }
            (
                "net/minecraft/commands/Commands"
                | "com/mojang/brigadier/builder/RequiredArgumentBuilder",
                "argument",
            ) => {
                let name = expect_string(args.first()?).to_owned();
                let kind = self.brigadier.argument_kind(args.get(1)?);
                return Some(
                    self.brigadier
                        .new_builder(NodeKind::Argument { name, kind }),
                );
            }
            ("net/minecraft/commands/Commands", "hasPermission") => {
                let level = level_of(args.first()?)?;
                return Some(Ok(marker(format!("brigadier/requires/{level}"))));
            }
            ("net/minecraft/server/permissions/LevelBasedPermissionSet", "forLevel") => {
                let level = level_of(args.first()?)?;
                return Some(Ok(marker(format!("brigadier/permission-set/{level}"))));
            }
            // `Component.empty()`: an empty literal (command feedback appends
            // to one).
            ("net/minecraft/network/chat/Component", "empty") => {
                return Some(Ok(component_value("literal", "")));
            }
            (PERMISSION_LEVEL, "byId") => {
                let id = int_arg(args, 0)?;
                return Some(Ok(permission_level_marker(id.clamp(0, 4) as u8)));
            }
            // Kotlin's null checks, which a Kotlin mod's command bodies run on
            // every parameter: a pure check, `NullPointerException` on null.
            // The lenient entrypoint path answered them neutrally; a strict
            // command run would refuse them as unmodelled.
            (
                "kotlin/jvm/internal/Intrinsics",
                "checkNotNull"
                | "checkNotNullParameter"
                | "checkNotNullExpressionValue"
                | "checkParameterIsNotNull"
                | "checkExpressionValueIsNotNull",
            ) if return_desc(&r.descriptor) == "V" => {
                return Some(match args.first() {
                    Some(Value::Ref(None)) | None => Err(NativeError::UncaughtException {
                        class: "java/lang/NullPointerException".into(),
                        message: args
                            .get(1)
                            .map(|name| format!("{} must not be null", expect_string(name))),
                        frame: None,
                    }),
                    Some(_) => Ok(Value::Void),
                });
            }
            ("me/lucko/fabric/api/permissions/v0/Permissions", _) => {
                return self.lucko_permissions(r, args);
            }
            _ => {}
        }
        if r.descriptor
            .starts_with("(Lcom/mojang/brigadier/context/CommandContext;")
            && (owner.starts_with("com/mojang/brigadier/arguments/")
                || owner.starts_with("net/minecraft/commands/arguments/"))
            && let Some(answer) = self.argument_getter(r, args)
        {
            return Some(answer);
        }
        let kind = argument_factory(r, args)?;
        // A getter (`getInteger`) of a type is not a factory; those take the
        // context and were answered above or refuse below.
        Some(self.brigadier.new_argument_type(kind))
    }

    /// fabric-permissions-api without a permissions provider: `check` answers
    /// its fallback (a level against the source's, or the default boolean),
    /// `require` the predicate of that fallback.
    fn lucko_permissions(
        &mut self,
        r: &MethodRef,
        args: &[Value],
    ) -> Option<Result<Value, NativeError>> {
        let last = crate::descriptor_arguments(&r.descriptor).pop()?;
        match (r.name.as_str(), last.as_str()) {
            ("require", "I") => {
                let level = int_arg(args, 1)?.clamp(0, 4);
                Some(Ok(marker(format!("brigadier/requires/{level}"))))
            }
            ("require", "Z") => {
                let level = if int_arg(args, 1)? != 0 { 0 } else { NOBODY };
                Some(Ok(marker(format!("brigadier/requires/{level}"))))
            }
            ("require", _) => Some(Ok(marker(format!("brigadier/requires/{NOBODY}")))),
            ("check", "Z") => Some(Ok(Value::Int(int_arg(args, 2)?))),
            ("check", "I") => {
                let wanted = int_arg(args, 2)?;
                let level = self.subject_level(args.first()?)?;
                Some(Ok(Value::Int(i32::from(i32::from(level) >= wanted))))
            }
            // No provider: `Tristate.DEFAULT`, which `check` reads as false.
            ("check", _) => Some(Ok(Value::Int(0))),
            _ => None,
        }
    }

    /// The operator level of a source or entity a permission check names:
    /// the source's, or the running source's when the entity is its player.
    fn subject_level(&self, value: &Value) -> Option<u8> {
        if let Some(source) = self.brigadier.source(value) {
            return Some(source.level);
        }
        let exec = self.brigadier.exec()?;
        let player = expect_ref(value)?.strip_prefix("game/player/")?;
        let uuid = u128::from_str_radix(player, 16).ok()?;
        let source = exec.sources.first()?;
        Some(if source.player == Some(uuid) {
            source.level
        } else {
            0
        })
    }

    /// The running command's argument `name`.
    fn command_argument(&self, args: &[Value], at: usize) -> Result<&CommandArgument, NativeError> {
        let name = args.get(at).map(expect_string).unwrap_or_default();
        let invocation = self
            .brigadier
            .exec()
            .and_then(|exec| exec.invocation.as_ref())
            .ok_or_else(|| NativeError::Metadata("brigadier: no command is running".into()))?;
        invocation
            .arguments
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, value)| value)
            .ok_or_else(|| NativeError::UncaughtException {
                class: "java/lang/IllegalArgumentException".into(),
                message: Some(format!("No such argument '{name}' exists on this command")),
                frame: None,
            })
    }

    /// `IntegerArgumentType.getInteger(context, name)` and its siblings.
    fn argument_getter(
        &mut self,
        r: &MethodRef,
        args: &[Value],
    ) -> Option<Result<Value, NativeError>> {
        let class = r.owner.rsplit('/').next()?;
        let name_at = crate::descriptor_arguments(&r.descriptor)
            .len()
            .checked_sub(1)?;
        let value = match self.command_argument(args, name_at) {
            Ok(value) => value.clone(),
            Err(error) => return Some(Err(error)),
        };
        let mismatch = || {
            Some(Err(NativeError::UncaughtException {
                class: "java/lang/IllegalArgumentException".into(),
                message: Some(format!(
                    "Argument '{}' is not the type {}#{} reads",
                    args.get(name_at).map(expect_string).unwrap_or_default(),
                    class,
                    r.name
                )),
                frame: None,
            }))
        };
        Some(Ok(match (class, r.name.as_str(), value) {
            ("IntegerArgumentType", "getInteger", CommandArgument::Int(v)) => Value::Int(v),
            ("LongArgumentType", "getLong", CommandArgument::Long(v)) => Value::Long(v),
            ("FloatArgumentType", "getFloat", CommandArgument::Float(v))
            | ("DoubleArgumentType", "getDouble", CommandArgument::Double(v)) => Value::Float(v),
            ("BoolArgumentType", "getBool", CommandArgument::Bool(v)) => Value::Int(i32::from(v)),
            ("StringArgumentType", "getString", CommandArgument::Str(v)) => marker(v),
            ("EntityArgument", "getPlayer" | "getEntity", CommandArgument::Players(players)) => {
                match players.as_slice() {
                    [one] => marker(format!("game/player/{one:032x}")),
                    [] => {
                        let key = if r.name == "getPlayer" {
                            "argument.player.notfound"
                        } else {
                            "argument.entity.notfound.entity"
                        };
                        self.brigadier
                            .exception_texts
                            .insert(key.into(), CommandText::key(key));
                        return Some(Err(syntax_error(key)));
                    }
                    _ => return mismatch(),
                }
            }
            (
                "EntityArgument",
                "getPlayers" | "getOptionalPlayers" | "getEntities" | "getOptionalEntities",
                CommandArgument::Players(players),
            ) => {
                if players.is_empty() && matches!(r.name.as_str(), "getPlayers" | "getEntities") {
                    let key = if r.name == "getPlayers" {
                        "argument.player.notfound"
                    } else {
                        "argument.entity.notfound.entity"
                    };
                    self.brigadier
                        .exception_texts
                        .insert(key.into(), CommandText::key(key));
                    return Some(Err(syntax_error(key)));
                }
                let values = players
                    .iter()
                    .map(|uuid| marker(format!("game/player/{uuid:032x}")))
                    .collect();
                let list = format!(
                    "java/util/ArrayList#brigadier-{}",
                    stable_hash(&format!("{players:?}"))
                );
                self.mutable_collections.insert(list.clone(), values);
                marker(list)
            }
            (
                "BlockPosArgument",
                "getBlockPos" | "getLoadedBlockPos" | "getSpawnablePos",
                CommandArgument::BlockPos([x, y, z]),
            ) => marker(format!("block-pos/{x},{y},{z}")),
            ("IdentifierArgument", "getId", CommandArgument::Identifier(id)) => {
                let (namespace, path) = id.split_once(':').unwrap_or(("minecraft", &id));
                identifier_value(namespace, path)
            }
            ("UuidArgument", "getUuid", CommandArgument::Uuid(uuid)) => {
                return Some(uuid_value(&uuid_text(uuid)));
            }
            ("DimensionArgument", "getDimension", CommandArgument::Dimension(id)) => {
                marker(format!("game/level/{id}"))
            }
            _ => return mismatch(),
        }))
    }

    /// `context.getArgument(name, Class)`: the boxed value.
    fn context_argument(&mut self, args: &[Value]) -> Result<Value, NativeError> {
        Ok(match self.command_argument(args, 0)?.clone() {
            CommandArgument::Bool(v) => boolean_value(v),
            CommandArgument::Int(v) => Value::Int(v),
            CommandArgument::Long(v) => Value::Long(v),
            CommandArgument::Float(v) | CommandArgument::Double(v) => Value::Float(v),
            CommandArgument::Str(v) => marker(v),
            CommandArgument::Identifier(id) => {
                let (namespace, path) = id.split_once(':').unwrap_or(("minecraft", &id));
                identifier_value(namespace, path)
            }
            CommandArgument::Uuid(uuid) => uuid_value(&uuid_text(uuid))?,
            // An `EntitySelector`, `Coordinates` or `ResourceKey` is resolved
            // by the type's own getter; the raw object is not modelled.
            other => {
                return Err(NativeError::Metadata(format!(
                    "brigadier: getArgument of a {other:?} argument is not modelled"
                )));
            }
        })
    }

    /// Instance calls on the command surface's markers. `None` leaves the call
    /// to the rest of the host.
    pub(crate) fn brigadier_call(
        &mut self,
        r: &MethodRef,
        receiver: &Value,
        args: &[Value],
    ) -> Option<Result<Value, NativeError>> {
        let text = expect_ref(receiver)?.clone();
        let name = r.name.as_str();
        // The objects `new` made of permission and exception classes.
        if name == "<init>" {
            return self.brigadier_init(r, &text, args);
        }
        if text == "game-event/register_commands" {
            return match name {
                "getDispatcher" => Some(Ok(marker(DISPATCHER.into()))),
                "getBuildContext" => Some(Ok(marker(BUILD_CONTEXT.into()))),
                "getCommandSelection" => {
                    let dedicated = self.game_event.as_ref().is_some_and(|f| f.facade.dedicated);
                    let selection = if dedicated { "DEDICATED" } else { "INTEGRATED" };
                    Some(Ok(marker(format!("enum/{COMMAND_SELECTION}/{selection}"))))
                }
                _ => None,
            };
        }
        if let Some(answer) = self.permission_call(r, &text, args) {
            return Some(answer);
        }
        if text == DISPATCHER {
            return Some(self.dispatcher_call(r, args));
        }
        if let Some(id) = marker_id(receiver, "brigadier/builder/") {
            return Some(self.builder_call(r, id, receiver, args));
        }
        if let Some(id) = marker_id(receiver, "brigadier/node/") {
            return Some(self.node_call(r, id, args));
        }
        if text == CONTEXT {
            return Some(match name {
                "getSource" => Ok(marker("brigadier/source/0".into())),
                "getArgument" => self.context_argument(args),
                "getInput" => Ok(self
                    .brigadier
                    .exec()
                    .and_then(|e| e.invocation.as_ref())
                    .map_or(Value::Ref(None), |i| marker(i.input.clone()))),
                _ => Err(NativeError::UnknownHostCall(r.clone())),
            });
        }
        if text.starts_with("brigadier/source/") {
            return Some(self.source_call(r, receiver, args));
        }
        if let Some(type_marker) = text
            .strip_prefix("com/mojang/brigadier/exceptions/SimpleCommandExceptionType#")
            .map(|_| text.clone())
            && matches!(name, "create" | "createWithContext")
        {
            let message = self
                .brigadier
                .exception_types
                .get(&type_marker)
                .cloned()
                .unwrap_or_else(|| CommandText::key("command.failed"));
            let mut messages = std::mem::take(&mut self.throwable_messages);
            let value = self.brigadier.new_exception(message, &mut messages);
            self.throwable_messages = messages;
            return Some(Ok(value));
        }
        // `player.sendSystemMessage(component)` while a command runs: to them.
        if self.brigadier.exec.is_some()
            && let Some(player) = text.strip_prefix("game/player/")
            && name == "sendSystemMessage"
            && r.descriptor == "(Lnet/minecraft/network/chat/Component;)V"
            && let Ok(uuid) = u128::from_str_radix(player, 16)
        {
            let text = CommandText::of(args.first()?);
            return Some(
                self.brigadier
                    .queue(Pending::Text(CommandMessage {
                        kind: MessageKind::Player(uuid),
                        text,
                    }))
                    .map(|()| Value::Void),
            );
        }
        // `block-pos/x,y,z`: the coordinates a command's position argument gave.
        if let Some(coords) = text.strip_prefix("block-pos/")
            && let ("getX" | "getY" | "getZ", "()I") = (name, r.descriptor.as_str())
        {
            let at = match name {
                "getX" => 0,
                "getY" => 1,
                _ => 2,
            };
            let value = coords.split(',').nth(at)?.parse().ok()?;
            return Some(Ok(Value::Int(value)));
        }
        // Styling a command's message: the text stays, the style is dropped
        // (component markers carry text only).
        if matches!(
            r.owner.as_str(),
            "net/minecraft/network/chat/MutableComponent" | "net/minecraft/network/chat/Component"
        ) && text.starts_with("component/")
            && matches!(name, "withStyle" | "withColor" | "setStyle")
        {
            return Some(Ok(receiver.clone()));
        }
        None
    }

    /// `new Permission.HasCommandLevel(level)`, `new PermissionCheck.Require
    /// (permission)`, `new SimpleCommandExceptionType(message)`.
    fn brigadier_init(
        &mut self,
        r: &MethodRef,
        receiver: &str,
        args: &[Value],
    ) -> Option<Result<Value, NativeError>> {
        match r.owner.as_str() {
            "net/minecraft/server/permissions/Permission$HasCommandLevel"
            | "net/minecraft/server/permissions/PermissionCheck$Require" => {
                let level = level_of(args.first()?).or_else(|| {
                    self.brigadier
                        .permission_objects
                        .get(expect_string(args.first()?))
                        .copied()
                })?;
                if self.brigadier.permission_objects.len() < MAX_EXCEPTION_TYPES {
                    self.brigadier
                        .permission_objects
                        .insert(receiver.to_owned(), level);
                }
                Some(Ok(Value::Void))
            }
            "com/mojang/brigadier/exceptions/SimpleCommandExceptionType" => {
                let text = CommandText::of(args.first()?);
                if self.brigadier.exception_types.len() < MAX_EXCEPTION_TYPES {
                    self.brigadier
                        .exception_types
                        .insert(receiver.to_owned(), text);
                }
                Some(Ok(Value::Void))
            }
            _ => None,
        }
    }

    /// Permission sets, permissions, checks, levels and level predicates.
    fn permission_call(
        &mut self,
        r: &MethodRef,
        text: &str,
        args: &[Value],
    ) -> Option<Result<Value, NativeError>> {
        let own = level_of(&marker(text.to_owned()))
            .or_else(|| self.brigadier.permission_objects.get(text).copied())?;
        let other = |host: &Self, value: &Value| {
            level_of(value).or_else(|| {
                host.brigadier
                    .permission_objects
                    .get(expect_string(value))
                    .copied()
            })
        };
        let flag = |b: bool| Some(Ok(Value::Int(i32::from(b))));
        let name = r.name.as_str();
        if text.starts_with("brigadier/permission-set/") {
            return match name {
                "hasPermission" => flag(own >= other(self, args.first()?)?),
                "level" => Some(Ok(permission_level_marker(own))),
                _ => Some(Err(NativeError::UnknownHostCall(r.clone()))),
            };
        }
        if text.starts_with("brigadier/permission-check/")
            || (r.owner.ends_with("PermissionCheck$Require") && name == "check")
        {
            return match name {
                "check" => flag(other(self, args.first()?)? >= own),
                _ => Some(Err(NativeError::UnknownHostCall(r.clone()))),
            };
        }
        if text.starts_with("brigadier/requires/") {
            return match name {
                "test" => {
                    let level = self.subject_level(args.first()?).unwrap_or(0);
                    flag(level >= own)
                }
                _ => Some(Err(NativeError::UnknownHostCall(r.clone()))),
            };
        }
        if text.starts_with(&format!("enum/{PERMISSION_LEVEL}/")) {
            return match name {
                "id" | "ordinal" => Some(Ok(Value::Int(i32::from(own)))),
                "isEqualOrHigherThan" => flag(own >= other(self, args.first()?)?),
                "getSerializedName" => Some(Ok(marker(level_name(own).to_ascii_lowercase()))),
                _ => None,
            };
        }
        // A `HasCommandLevel` object.
        match name {
            "level" => Some(Ok(permission_level_marker(own))),
            _ => None,
        }
    }

    /// `CommandDispatcher` members.
    fn dispatcher_call(&mut self, r: &MethodRef, args: &[Value]) -> Result<Value, NativeError> {
        match r.name.as_str() {
            "register" => {
                let built = self.brigadier.node_of(
                    args.first()
                        .ok_or_else(|| NativeError::UnknownHostCall(r.clone()))?,
                )?;
                let root = self.brigadier.root()?;
                self.brigadier.add_child(root, built, 0)?;
                Ok(node_marker(built))
            }
            "getRoot" => Ok(node_marker(self.brigadier.root()?)),
            _ => Err(NativeError::UnknownHostCall(r.clone())),
        }
    }

    /// `ArgumentBuilder` (literal and required) members.
    fn builder_call(
        &mut self,
        r: &MethodRef,
        id: usize,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Value, NativeError> {
        let unknown = || NativeError::UnknownHostCall(r.clone());
        let builder = self
            .brigadier
            .builders
            .get(&id)
            .cloned()
            .ok_or_else(unknown)?;
        let first = args.first();
        match r.name.as_str() {
            "then" => {
                if builder.template.redirect.is_some() {
                    return Err(NativeError::UncaughtException {
                        class: "java/lang/IllegalStateException".into(),
                        message: Some("Cannot add children to a redirected node".into()),
                        frame: None,
                    });
                }
                let child = self.brigadier.node_of(first.ok_or_else(unknown)?)?;
                self.brigadier.add_child(builder.arguments, child, 0)?;
                Ok(receiver.clone())
            }
            "executes" | "requires" | "suggests" | "redirect" | "fork" | "forward" => {
                let value = first.cloned().unwrap_or(Value::Ref(None));
                let target = match r.name.as_str() {
                    "redirect" | "fork" | "forward" => {
                        if !self.brigadier.nodes[builder.arguments].children.is_empty() {
                            return Err(NativeError::UncaughtException {
                                class: "java/lang/IllegalStateException".into(),
                                message: Some("Cannot forward a node with children".into()),
                                frame: None,
                            });
                        }
                        Some(self.brigadier.node_of(&value)?)
                    }
                    _ => None,
                };
                let modifier = match r.name.as_str() {
                    "fork" => true,
                    "redirect" | "forward" => {
                        args.get(1).is_some_and(|m| m != &Value::Ref(None))
                            || (r.name == "forward" && args.get(2) == Some(&Value::Int(1)))
                    }
                    _ => false,
                };
                let entry = self.brigadier.builders.get_mut(&id).ok_or_else(unknown)?;
                let template = &mut entry.template;
                match r.name.as_str() {
                    "executes" => template.command = (value != Value::Ref(None)).then_some(value),
                    "requires" => template.requirement = Some(BrigadierState::requirement(&value)),
                    "suggests" => template.suggests = (value != Value::Ref(None)).then_some(value),
                    _ => {
                        template.redirect = target;
                        template.modifier = modifier;
                    }
                }
                Ok(receiver.clone())
            }
            "build" => Ok(node_marker(self.brigadier.build(id)?)),
            "getLiteral" | "getName" => Ok(marker(builder.template.kind.name().to_owned())),
            "getCommand" => Ok(builder.template.command.clone().unwrap_or(Value::Ref(None))),
            "getRedirect" => Ok(builder
                .template
                .redirect
                .map_or(Value::Ref(None), node_marker)),
            "getArguments" => {
                let children = self.brigadier.nodes[builder.arguments].children.clone();
                Ok(self.node_list(&children))
            }
            _ => Err(unknown()),
        }
    }

    fn node_list(&mut self, ids: &[usize]) -> Value {
        let list = format!(
            "java/util/ArrayList#brigadier-nodes-{}",
            stable_hash(&format!("{ids:?}"))
        );
        self.mutable_collections
            .insert(list.clone(), ids.iter().copied().map(node_marker).collect());
        marker(list)
    }

    /// `CommandNode` members.
    fn node_call(
        &mut self,
        r: &MethodRef,
        id: usize,
        args: &[Value],
    ) -> Result<Value, NativeError> {
        let unknown = || NativeError::UnknownHostCall(r.clone());
        let node = self.brigadier.nodes.get(id).cloned().ok_or_else(unknown)?;
        match r.name.as_str() {
            "getName" | "getLiteral" => Ok(marker(node.kind.name().to_owned())),
            "getUsageText" => Ok(marker(match &node.kind {
                NodeKind::Argument { name, .. } => format!("<{name}>"),
                kind => kind.name().to_owned(),
            })),
            "getChild" => {
                let wanted = args.first().map(expect_string).unwrap_or_default();
                Ok(node
                    .children
                    .iter()
                    .copied()
                    .find(|&c| self.brigadier.nodes[c].kind.name() == wanted)
                    .map_or(Value::Ref(None), node_marker))
            }
            "getChildren" => Ok(self.node_list(&node.children)),
            "getCommand" => Ok(node.command.unwrap_or(Value::Ref(None))),
            "getRedirect" => Ok(node.redirect.map_or(Value::Ref(None), node_marker)),
            "isFork" => Ok(Value::Int(i32::from(node.modifier))),
            "addChild" => {
                let child = self.brigadier.node_of(args.first().ok_or_else(unknown)?)?;
                self.brigadier.add_child(id, child, 0)?;
                Ok(Value::Void)
            }
            "hashCode" => Ok(Value::Int(stable_hash(&format!("brigadier/node/{id}")))),
            _ => Err(unknown()),
        }
    }

    /// `CommandSourceStack` members of a running command's source.
    fn source_call(
        &mut self,
        r: &MethodRef,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Value, NativeError> {
        let unknown = || NativeError::UnknownHostCall(r.clone());
        let source = self
            .brigadier
            .source(receiver)
            .cloned()
            .ok_or_else(unknown)?;
        let player = || {
            source
                .player
                .map(|uuid| marker(format!("game/player/{uuid:032x}")))
        };
        let level_marker = || marker(format!("game/level/{}", source.dimension));
        let first = args.first();
        match (r.name.as_str(), r.descriptor.as_str()) {
            ("permissions", _) => Ok(marker(format!("brigadier/permission-set/{}", source.level))),
            // The pre-26 API, which some multi-version jars still call.
            ("hasPermission", "(I)Z") => {
                let wanted = int_arg(args, 0).ok_or_else(unknown)?;
                Ok(Value::Int(i32::from(i32::from(source.level) >= wanted)))
            }
            ("getServer", _) => Ok(marker("game/server".into())),
            ("getLevel", _) => Ok(level_marker()),
            ("isPlayer", "()Z") => Ok(Value::Int(i32::from(source.player.is_some()))),
            ("isSilent", "()Z") => Ok(Value::Int(0)),
            ("getPlayer" | "getEntity", _) => Ok(player().unwrap_or(Value::Ref(None))),
            ("getPlayerOrException" | "getEntityOrException", _) => match player() {
                Some(player) => Ok(player),
                None => {
                    let key = if r.name == "getPlayerOrException" {
                        "permissions.requires.player"
                    } else {
                        "permissions.requires.entity"
                    };
                    self.brigadier
                        .exception_texts
                        .insert(key.into(), CommandText::key(key));
                    Err(syntax_error(key))
                }
            },
            // The server's registries, as `MinecraftServer.registryAccess()`
            // answers them (`facade_server.rs`).
            ("registryAccess", _) => {
                let facts = self
                    .game_event
                    .as_ref()
                    .map(|firing| Arc::clone(&firing.facade.server))
                    .ok_or_else(unknown)?;
                crate::facade_server::server_member(&facts, r, args)
                    .unwrap_or_else(|| Err(unknown()))
            }
            ("getTextName", _) => Ok(marker(source.name.clone())),
            ("getDisplayName", _) => Ok(component_value("literal", &source.name)),
            ("getOnlinePlayerNames", _) => {
                let names = self
                    .game_event
                    .as_ref()
                    .map(|f| {
                        f.facade
                            .players
                            .iter()
                            .map(|p| marker(p.name.clone()))
                            .collect()
                    })
                    .unwrap_or_default();
                let list = "java/util/ArrayList#brigadier-online-players".to_owned();
                self.mutable_collections.insert(list.clone(), names);
                Ok(marker(list))
            }
            ("sendSuccess", _) => {
                let supplier = first.cloned().ok_or_else(unknown)?;
                let broadcast = args.get(1) == Some(&Value::Int(1));
                self.brigadier.queue(Pending::Supplier {
                    supplier,
                    broadcast,
                })?;
                Ok(Value::Void)
            }
            ("sendFailure" | "sendSystemMessage", "(Lnet/minecraft/network/chat/Component;)V") => {
                let kind = if r.name == "sendFailure" {
                    MessageKind::Failure
                } else {
                    MessageKind::System
                };
                let text = CommandText::of(first.ok_or_else(unknown)?);
                self.brigadier
                    .queue(Pending::Text(CommandMessage { kind, text }))?;
                Ok(Value::Void)
            }
            ("hashCode", "()I") => Ok(Value::Int(stable_hash(expect_string(receiver)))),
            ("equals", "(Ljava/lang/Object;)Z") => {
                Ok(Value::Int(i32::from(first == Some(receiver))))
            }
            _ => Err(unknown()),
        }
    }

    /// `instanceof` on the command surface's markers.
    pub(crate) fn brigadier_instance_of(&self, marker: &str, wanted: &str) -> bool {
        if marker.starts_with("brigadier/source/") {
            return matches!(
                wanted,
                "net/minecraft/commands/CommandSourceStack"
                    | "net/minecraft/commands/SharedSuggestionProvider"
                    | "net/minecraft/commands/ExecutionCommandSource"
                    | "net/minecraft/server/permissions/PermissionSetSupplier"
            );
        }
        if marker.starts_with("brigadier/node/") {
            return wanted.starts_with("com/mojang/brigadier/tree/");
        }
        if marker.starts_with("brigadier/builder/") {
            return wanted.starts_with("com/mojang/brigadier/builder/");
        }
        false
    }

    /// How many host calls the log holds, so a command's calls can be rolled
    /// back once it ran (a command run every tick must not grow the log).
    fn brigadier_log_marks(&self) -> (usize, usize, usize) {
        (
            self.calls.len(),
            self.invocations.len(),
            self.posted_events.len(),
        )
    }

    fn brigadier_truncate_log(&mut self, (calls, invocations, posted): (usize, usize, usize)) {
        self.calls.truncate(calls);
        self.invocations.truncate(invocations);
        self.posted_events.truncate(posted);
    }
}

impl<H: crate::Host> crate::Runtime<'_, H> {
    /// Whether calling `r` on `receiver` is calling a lambda's one abstract
    /// method through an interface neither the jar nor the JDK model
    /// declares (log4j's `TriConsumer`, which a command's feedback goes
    /// through). The marker records its creating site, not the interface
    /// method, so this asks for what a JVM call would need: a lambda of a jar
    /// method, a call with exactly the arguments that method takes after its
    /// captures, the same void-ness, and not an `Object` member.
    pub(crate) fn foreign_functional_call(
        &self,
        r: &MethodRef,
        receiver: &Value,
        args: &[Value],
    ) -> bool {
        if matches!(r.name.as_str(), "equals" | "hashCode" | "toString") {
            return false;
        }
        let Some(text) = expect_ref(receiver).filter(|t| t.starts_with("@indy:")) else {
            return false;
        };
        let Ok(callback) = parse_callback_marker(text) else {
            return false;
        };
        let Some(method) = self
            .class(&callback.target.owner)
            .and_then(|class| class.method(&callback.target.name, &callback.target.descriptor))
        else {
            return false;
        };
        let takes = arg_count(&callback.target.descriptor) + usize::from(!method.is_static());
        // An erased generic interface returns `Object` where the lambda's
        // method returns its own type: only void-ness has to agree.
        takes == callback.captures.len() + args.len()
            && (return_desc(&r.descriptor) == "V")
                == (return_desc(&callback.target.descriptor) == "V")
    }

    /// `String.join(separator, CharSequence...)` and `String.join(separator,
    /// Iterable)`: command feedback builds its lines with them. The varargs
    /// array lives in the interpreter, so this runs here rather than in the
    /// host; a `StringBuilder` element reads its text, `null` reads `null`.
    pub(crate) fn invoke_string_join(&mut self, r: &MethodRef, args: &[Value]) -> Option<Value> {
        if r.owner != "java/lang/String" || r.name != "join" {
            return None;
        }
        let text = |runtime: &Self, value: &Value| match value {
            Value::Ref(Some(marker)) => runtime
                .string_builders
                .get(marker)
                .cloned()
                .unwrap_or_else(|| crate::value_text(value)),
            other => crate::value_text(other),
        };
        let separator = text(self, args.first()?);
        let parts: Vec<String> = match r.descriptor.as_str() {
            "(Ljava/lang/CharSequence;[Ljava/lang/CharSequence;)Ljava/lang/String;" => {
                let array = expect_ref(args.get(1)?)?;
                let values = self.arrays.get(array)?.clone();
                values.iter().map(|value| text(self, value)).collect()
            }
            "(Ljava/lang/CharSequence;Ljava/lang/Iterable;)Ljava/lang/String;" => {
                self.host.mutable_collection_values(args.get(1)?)?
            }
            _ => return None,
        };
        Some(marker(parts.join(&separator)))
    }
}

/// The level-probe source a requirement is classified with: a player, so a
/// predicate that asks `isPlayer()` first still answers.
const PROBE_PLAYER: u128 = u128::MAX;

impl NativeJar {
    /// Follow a lambda that only forwards to another captured lambda
    /// (`handler::accept`) to the jar method it names, as `events.rs` does.
    fn command_callback_target(
        &self,
        mut target: MethodRef,
        mut captures: Vec<Value>,
    ) -> (MethodRef, Vec<Value>) {
        for _ in 0..8 {
            if self.classes.contains_key(&target.owner) {
                break;
            }
            let Some(Value::Ref(Some(inner))) = captures.first() else {
                break;
            };
            let Ok(inner) = parse_callback_marker(inner) else {
                break;
            };
            let mut next = inner.captures;
            next.extend(captures.drain(1..));
            (target, captures) = (inner.target, next);
        }
        (target, captures)
    }

    /// Call a functional-interface value the jar handed over (`Command`,
    /// `Predicate`, `Supplier`): a lambda or method reference to a jar method,
    /// or an object of a jar class implementing `method`+`descriptor`.
    fn invoke_functional(
        &self,
        callback: &Value,
        method: &str,
        descriptor: &str,
        args: Vec<Value>,
        host: &mut StdHost,
        budget: &ExecutionBudget,
    ) -> Result<Value, NativeError> {
        let text = expect_ref(callback)
            .ok_or_else(|| NativeError::Metadata(format!("a null {method} callback was called")))?;
        if text.starts_with("@indy:") {
            let parsed = parse_callback_marker(text)?;
            let (target, mut captures) =
                self.command_callback_target(parsed.target, parsed.captures);
            let found = self
                .classes
                .get(&target.owner)
                .and_then(|class| class.method(&target.name, &target.descriptor))
                .ok_or_else(|| {
                    NativeError::Metadata(format!(
                        "{method} callback {}#{} is not a method of this jar",
                        target.owner, target.name
                    ))
                })?;
            let receiver = if found.is_static() {
                None
            } else if captures.is_empty() {
                return Err(NativeError::Metadata(
                    "instance callback has no captured receiver".into(),
                ));
            } else {
                Some(captures.remove(0))
            };
            captures.extend(args);
            if captures.len() != arg_count(&target.descriptor) {
                return Err(NativeError::Metadata(format!(
                    "{method} callback {}#{}{} takes {} arguments, got {}",
                    target.owner,
                    target.name,
                    target.descriptor,
                    arg_count(&target.descriptor),
                    captures.len()
                )));
            }
            return self.invoke_persistent(&target, receiver, &captures, host, budget);
        }
        if let Some((class, _)) = text.split_once('#')
            && self.classes.contains_key(class)
        {
            let mut current = class.to_owned();
            for _ in 0..32 {
                let Some(file) = self.classes.get(&current) else {
                    break;
                };
                if file.method(method, descriptor).is_some() {
                    let target = MethodRef {
                        owner: current,
                        name: method.into(),
                        descriptor: descriptor.into(),
                    };
                    return self.invoke_persistent(
                        &target,
                        Some(callback.clone()),
                        &args,
                        host,
                        budget,
                    );
                }
                current = file.super_name.clone();
            }
        }
        Err(NativeError::Metadata(format!(
            "{method} callback {text} is not a lambda or object of this jar"
        )))
    }

    /// Run `f` with a command execution open on `host`: the façade answers
    /// for `sources`, strictly, and the call log is rolled back afterwards.
    fn with_command_execution<T>(
        &self,
        host: &mut StdHost,
        facade: Arc<EventFacade>,
        invocation: Option<CommandInvocation>,
        sources: Vec<CommandSource>,
        f: impl FnOnce(&Self, &mut StdHost) -> T,
    ) -> (T, Vec<Pending>) {
        let marks = host.brigadier_log_marks();
        let strict = std::mem::replace(&mut host.strict_unknown_host_calls, true);
        let context = EventContext {
            level: sources.first().map(|s| s.dimension.clone()),
            player: sources.first().and_then(|s| s.player),
            ..EventContext::default()
        };
        let outer_event = host.game_event.replace(Box::new(EventFiring::new(
            GameEvent::RegisterCommands,
            facade,
            context,
        )));
        let outer = host.brigadier.exec.replace(Box::new(Execution {
            invocation,
            sources,
            pending: Vec::new(),
        }));
        let out = f(self, host);
        let exec = std::mem::replace(&mut host.brigadier.exec, outer);
        host.game_event = outer_event;
        host.strict_unknown_host_calls = strict;
        host.brigadier_truncate_log(marks);
        (out, exec.map(|e| e.pending).unwrap_or_default())
    }

    /// Whether `requirement` lets `source` use a node. A level compares;
    /// a jar predicate runs (`Predicate.test(source)`), strictly.
    pub fn check_requirement(
        &self,
        requirement: &Requirement,
        source: &CommandSource,
        facade: Arc<EventFacade>,
        host: &mut StdHost,
        budget: &ExecutionBudget,
    ) -> Result<bool, NativeError> {
        let predicate = match requirement {
            Requirement::Level(level) => return Ok(source.level >= *level),
            Requirement::Predicate(predicate) => predicate.clone(),
        };
        let (result, _) =
            self.with_command_execution(host, facade, None, vec![source.clone()], |jar, host| {
                jar.invoke_functional(
                    &predicate,
                    "test",
                    "(Ljava/lang/Object;)Z",
                    vec![marker("brigadier/source/0".into())],
                    host,
                    budget,
                )
            });
        Ok(result? == Value::Int(1))
    }

    /// The operator level the tree clients get shows a node with
    /// `requirement` at: the lowest level whose probe player passes it, on a
    /// server that is `dedicated` or not.
    ///
    /// The tree is built once, but vanilla asks a `requires` predicate per
    /// source and per use, so a probe cannot settle every predicate: one that
    /// lets no probe player through (a console-only check, one that reads a
    /// config) is owners-only in the tree, never hidden from everyone, and the
    /// game runs it again with the real source before the command runs. A
    /// predicate that refuses (reaches past the façade) is an error, which
    /// the caller treats the same way.
    pub fn requirement_level(
        &self,
        requirement: &Requirement,
        dedicated: bool,
        host: &mut StdHost,
        budget: &ExecutionBudget,
    ) -> Result<u8, NativeError> {
        let facade = Arc::new(EventFacade {
            dedicated,
            players: vec![FacadePlayer {
                uuid: PROBE_PLAYER,
                name: "Player".into(),
                level: "minecraft:overworld".into(),
                entity_id: 0,
            }],
            ..EventFacade::default()
        });
        for level in 0..=4 {
            let source = CommandSource {
                name: "Player".into(),
                level,
                player: Some(PROBE_PLAYER),
                dimension: "minecraft:overworld".into(),
            };
            if self.check_requirement(requirement, &source, Arc::clone(&facade), host, budget)? {
                return Ok(level);
            }
        }
        Ok(4)
    }

    /// Run one node's `Command.run(context)` for `invocation`, then the
    /// suppliers of its `sendSuccess` calls, through `host` (the host its
    /// registration ran on, so its statics are the mod's). `facade` is the
    /// server snapshot player and server markers answer from.
    pub fn run_command(
        &self,
        command: &Value,
        invocation: &CommandInvocation,
        facade: Arc<EventFacade>,
        host: &mut StdHost,
        budget: &ExecutionBudget,
    ) -> CommandRun {
        let sources = vec![invocation.source.clone()];
        let (result, pending) = self.with_command_execution(
            host,
            Arc::clone(&facade),
            Some(invocation.clone()),
            sources.clone(),
            |jar, host| {
                jar.invoke_functional(
                    command,
                    "run",
                    "(Lcom/mojang/brigadier/context/CommandContext;)I",
                    vec![marker(CONTEXT.into())],
                    host,
                    budget,
                )
            },
        );
        let mut messages = Vec::new();
        let mut refused = None;
        for item in pending {
            match item {
                Pending::Text(message) => messages.push(message),
                Pending::Supplier {
                    supplier,
                    broadcast,
                } => {
                    let (component, _) = self.with_command_execution(
                        host,
                        Arc::clone(&facade),
                        Some(invocation.clone()),
                        sources.clone(),
                        |jar, host| {
                            jar.invoke_functional(
                                &supplier,
                                "get",
                                "()Ljava/lang/Object;",
                                Vec::new(),
                                host,
                                budget,
                            )
                        },
                    );
                    match component {
                        Ok(component) => messages.push(CommandMessage {
                            kind: MessageKind::Success { broadcast },
                            text: CommandText::of(&component),
                        }),
                        Err(error) => {
                            refused.get_or_insert_with(|| format!("sendSuccess supplier: {error}"));
                        }
                    }
                }
            }
        }
        let outcome = match (result, refused) {
            (Err(NativeError::UncaughtException { class, message, .. }), _)
                if class == SYNTAX_EXCEPTION =>
            {
                let text = message.as_deref().map_or_else(
                    || CommandText::key("command.failed"),
                    |m| {
                        host.brigadier
                            .exception_texts
                            .get(m)
                            .cloned()
                            .unwrap_or(CommandText {
                                text: m.to_owned(),
                                translate: false,
                            })
                    },
                );
                CommandOutcome::Syntax(text)
            }
            (Err(error), _) => CommandOutcome::Refused(error.to_string()),
            (Ok(_), Some(reason)) => CommandOutcome::Refused(reason),
            (Ok(Value::Int(result)), None) => CommandOutcome::Returned(result),
            (Ok(other), None) => {
                CommandOutcome::Refused(format!("Command.run returned {other:?}, not an int"))
            }
        };
        CommandRun { outcome, messages }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> StdHost {
        StdHost::new()
    }

    fn call(
        host: &mut StdHost,
        owner: &str,
        name: &str,
        desc: &str,
        receiver: &Value,
        args: &[Value],
    ) -> Value {
        let r = MethodRef {
            owner: owner.into(),
            name: name.into(),
            descriptor: desc.into(),
        };
        host.brigadier_call(&r, receiver, args)
            .expect("modelled")
            .expect("answered")
    }

    fn invoke(host: &mut StdHost, owner: &str, name: &str, desc: &str, args: &[Value]) -> Value {
        let r = MethodRef {
            owner: owner.into(),
            name: name.into(),
            descriptor: desc.into(),
        };
        host.brigadier_invoke(&r, args)
            .expect("modelled")
            .expect("answered")
    }

    const LITERAL: &str =
        "(Ljava/lang/String;)Lcom/mojang/brigadier/builder/LiteralArgumentBuilder;";
    const THEN: &str = "(Lcom/mojang/brigadier/builder/ArgumentBuilder;)Lcom/mojang/brigadier/builder/ArgumentBuilder;";
    const EXECUTES: &str =
        "(Lcom/mojang/brigadier/Command;)Lcom/mojang/brigadier/builder/ArgumentBuilder;";
    const REGISTER: &str = "(Lcom/mojang/brigadier/builder/LiteralArgumentBuilder;)Lcom/mojang/brigadier/tree/LiteralCommandNode;";
    const BUILDER: &str = "com/mojang/brigadier/builder/LiteralArgumentBuilder";

    fn literal(host: &mut StdHost, name: &str) -> Value {
        invoke(
            host,
            "net/minecraft/commands/Commands",
            "literal",
            LITERAL,
            &[marker(name.into())],
        )
    }

    fn register(host: &mut StdHost, builder: &Value) -> Value {
        call(
            host,
            "com/mojang/brigadier/CommandDispatcher",
            "register",
            REGISTER,
            &marker(DISPATCHER.into()),
            std::slice::from_ref(builder),
        )
    }

    #[test]
    fn then_builds_the_child_at_that_call() {
        // Brigadier's `then` stores `argument.build()` immediately: a command
        // set on the child builder afterwards is not in the tree.
        let mut h = host();
        let root = literal(&mut h, "home");
        let child = literal(&mut h, "set");
        call(
            &mut h,
            BUILDER,
            "then",
            THEN,
            &root,
            std::slice::from_ref(&child),
        );
        call(
            &mut h,
            BUILDER,
            "executes",
            EXECUTES,
            &child,
            &[marker("@indy:late".into())],
        );
        register(&mut h, &root);
        let tree = h.command_tree();
        let (_, home) = tree.roots().next().unwrap();
        let set = &tree.nodes[home.children[0]];
        assert_eq!(set.kind, NodeKind::Literal("set".into()));
        assert_eq!(set.command, None);
    }

    #[test]
    fn registering_a_name_twice_merges_like_add_child() {
        // `/home a` then `/home b` with a new command on `home`: one root
        // with both children, and the newer command.
        let mut h = host();
        for (child, command) in [("a", "@indy:first"), ("b", "@indy:second")] {
            let root = literal(&mut h, "home");
            let leaf = literal(&mut h, child);
            call(&mut h, BUILDER, "then", THEN, &root, &[leaf]);
            call(
                &mut h,
                BUILDER,
                "executes",
                EXECUTES,
                &root,
                &[marker(command.into())],
            );
            register(&mut h, &root);
        }
        let tree = h.command_tree();
        let roots: Vec<_> = tree.roots().collect();
        assert_eq!(roots.len(), 1);
        let home = roots[0].1;
        assert_eq!(home.command, Some(marker("@indy:second".into())));
        let names: Vec<_> = home
            .children
            .iter()
            .map(|&c| tree.nodes[c].kind.name().to_owned())
            .collect();
        assert_eq!(names, ["a", "b"]);
    }

    #[test]
    fn permission_levels_compare_like_the_level_based_set() {
        let mut h = host();
        let gamemaster = h
            .brigadier_static(&MethodRef {
                owner: "net/minecraft/server/permissions/Permissions".into(),
                name: "COMMANDS_GAMEMASTER".into(),
                descriptor: "Lnet/minecraft/server/permissions/Permission;".into(),
            })
            .unwrap();
        let has = "(Lnet/minecraft/server/permissions/Permission;)Z";
        let set = |level: u8| marker(format!("brigadier/permission-set/{level}"));
        let owner = "net/minecraft/server/permissions/PermissionSet";
        assert_eq!(
            call(
                &mut h,
                owner,
                "hasPermission",
                has,
                &set(1),
                std::slice::from_ref(&gamemaster)
            ),
            Value::Int(0)
        );
        assert_eq!(
            call(
                &mut h,
                owner,
                "hasPermission",
                has,
                &set(2),
                std::slice::from_ref(&gamemaster)
            ),
            Value::Int(1)
        );
        // `new Permission.HasCommandLevel(PermissionLevel.ADMINS)`.
        let admins = h
            .brigadier_static(&MethodRef {
                owner: PERMISSION_LEVEL.into(),
                name: "ADMINS".into(),
                descriptor: format!("L{PERMISSION_LEVEL};"),
            })
            .unwrap();
        let object = marker("net/minecraft/server/permissions/Permission$HasCommandLevel#7".into());
        call(
            &mut h,
            "net/minecraft/server/permissions/Permission$HasCommandLevel",
            "<init>",
            "(Lnet/minecraft/server/permissions/PermissionLevel;)V",
            &object,
            &[admins],
        );
        assert_eq!(
            call(
                &mut h,
                owner,
                "hasPermission",
                has,
                &set(2),
                std::slice::from_ref(&object)
            ),
            Value::Int(0)
        );
        assert_eq!(
            call(&mut h, owner, "hasPermission", has, &set(3), &[object]),
            Value::Int(1)
        );
    }

    #[test]
    fn requires_with_a_level_check_records_the_level() {
        let mut h = host();
        let check = h
            .brigadier_static(&MethodRef {
                owner: "net/minecraft/commands/Commands".into(),
                name: "LEVEL_GAMEMASTERS".into(),
                descriptor: "Lnet/minecraft/server/permissions/PermissionCheck;".into(),
            })
            .unwrap();
        let predicate = invoke(
            &mut h,
            "net/minecraft/commands/Commands",
            "hasPermission",
            "(Lnet/minecraft/server/permissions/PermissionCheck;)Lnet/minecraft/server/permissions/PermissionProviderCheck;",
            &[check],
        );
        let root = literal(&mut h, "op_only");
        call(
            &mut h,
            BUILDER,
            "requires",
            "(Ljava/util/function/Predicate;)Lcom/mojang/brigadier/builder/ArgumentBuilder;",
            &root,
            &[predicate],
        );
        register(&mut h, &root);
        let tree = h.command_tree();
        assert_eq!(
            tree.roots().next().unwrap().1.requirement,
            Some(Requirement::Level(2))
        );
    }

    #[test]
    fn an_unknown_argument_type_is_kept_as_unmodelled() {
        let mut h = host();
        let custom = marker("example/ColourArgument#3".into());
        let node = invoke(
            &mut h,
            "net/minecraft/commands/Commands",
            "argument",
            "(Ljava/lang/String;Lcom/mojang/brigadier/arguments/ArgumentType;)Lcom/mojang/brigadier/builder/RequiredArgumentBuilder;",
            &[marker("colour".into()), custom],
        );
        let root = literal(&mut h, "paint");
        call(&mut h, BUILDER, "then", THEN, &root, &[node]);
        register(&mut h, &root);
        let tree = h.command_tree();
        let paint = tree.roots().next().unwrap().1;
        assert_eq!(
            tree.nodes[paint.children[0]].kind,
            NodeKind::Argument {
                name: "colour".into(),
                kind: ArgumentKind::Unmodelled("example/ColourArgument".into())
            }
        );
    }

    #[test]
    fn redirect_refuses_a_node_with_children_like_brigadier() {
        let mut h = host();
        let target = literal(&mut h, "teleport");
        let target_node = register(&mut h, &target);
        let alias = literal(&mut h, "tp2");
        let child = literal(&mut h, "x");
        call(&mut h, BUILDER, "then", THEN, &alias, &[child]);
        let r = MethodRef {
            owner: BUILDER.into(),
            name: "redirect".into(),
            descriptor: "(Lcom/mojang/brigadier/tree/CommandNode;)Lcom/mojang/brigadier/builder/ArgumentBuilder;".into(),
        };
        let result = h.brigadier_call(&r, &alias, &[target_node]).unwrap();
        assert!(
            matches!(result, Err(NativeError::UncaughtException { class, .. }) if class == "java/lang/IllegalStateException")
        );
    }
}
