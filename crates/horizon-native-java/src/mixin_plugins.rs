//! Mixin config plugins (`IMixinConfigPlugin`), run the way Mixin runs them.
//!
//! A Mixin config may name a plugin class that decides, in its own code,
//! which of the config's Mixins apply to which targets. The load-time weave
//! never runs that code, so it refuses every plan such a config governs
//! (`config-plugin`). The game-hook tier asks the plugin instead, and this
//! module is how: one **session** per config, with one plugin instance kept
//! on a resident heap for the whole session, in the order Mixin 0.8.7's
//! `MixinConfig`/`MixinProcessor` calls it (read from its bytecode):
//!
//! 1. the no-argument constructor (`getDeclaredConstructor().newInstance()`);
//! 2. `onLoad(mixinPackage)`, with the config's `package` as written (Mixin
//!    appends its trailing `.` only after this call);
//! 3. `getRefMapperConfig()`, whose answer is recorded and otherwise unused:
//!    the tier binds by official names, so there is no refmap to swap;
//! 4. `shouldApplyMixin(targetClassName, mixinClassName)` for every Mixin the
//!    config lists on this side (common list first, then the side's list) and
//!    every target its `@Mixin` names, binary names with dots;
//! 5. `acceptTargets(myTargets, otherTargets)`: the targets this config kept,
//!    and the targets of the jar's other configs (a real pack would add every
//!    other mod's; a plugin only reads them);
//! 6. `getMixins()`: companion Mixins, named relative to the package. Mixin
//!    applies those without asking `shouldApplyMixin`, on every side.
//!
//! Then, for each `(mixin, target)` the plugin kept, `preApply` and
//! `postApply` run once with a stand-in `ClassNode` and `IMixinInfo`. Mixin
//! calls them around the bytecode it applies; the hook tier applies none, so
//! a plugin that only logs or checks names there passes, and one that reads
//! or edits the node stops at the node's first member, which names it.
//!
//! **The bound.** Every call runs on a strict host (an unmodelled host call
//! fails instead of answering a neutral value) under its own 250 ms budget.
//! A failure in steps 1–3, 5 or 6 refuses the whole config with the step and
//! the member it stopped at; a failure in one `shouldApplyMixin` (or its
//! `preApply`/`postApply`) refuses that Mixin only, and the session goes on,
//! keeping what the failed call wrote before it stopped, as a JVM keeps what
//! a thrown call wrote. A refused plan keeps `config-plugin`; the refusal text
//! is in [`ConfigPluginReport`], so nothing is ever applied on a guess.
//!
//! **The platform the plugin sees** is the jar's own, alone: the host is
//! [`NativeJar::seed_host`]'s (its mod id and declared dependencies loaded,
//! its resources and services) with the side's environment type, Fabric's
//! loader and API classes present for a Fabric jar and absent otherwise
//! (`ClassNotFoundException`, which is what a platform probe catches), and
//! NeoForge's and Forge's absent on a Fabric server. A plugin that applies a
//! compat Mixin only when another mod is loaded therefore declines it, as it
//! would with that mod absent. The plugin's static state lives only for the
//! planning sessions of this jar; the entrypoint host starts from its own.

use super::*;

/// The wall-clock bound of one plugin call.
pub(crate) const CONFIG_PLUGIN_CALL_BUDGET: std::time::Duration =
    std::time::Duration::from_millis(250);

/// Companion Mixins one `getMixins()` may add.
const MAX_COMPANION_MIXINS: usize = 512;

/// The `ClassNode` stand-in `preApply`/`postApply` receive. It names no jar
/// class, so its first member access is an unmodelled host call that names
/// the member.
const CLASS_NODE_PREFIX: &str = "mixin-plugin/class-node/";
/// The `IMixinInfo` stand-in `preApply`/`postApply` receive.
const MIXIN_INFO_PREFIX: &str = "mixin-plugin/mixin-info/";

/// What one config's plugin session did, for reports and logs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigPluginReport {
    /// The config's jar resource path.
    pub config: String,
    /// The plugin class, as the config spells it.
    pub plugin: String,
    /// Why the whole config stays refused: the step and what it stopped at.
    /// `None` when the session ran.
    pub refused: Option<String>,
    /// `shouldApplyMixin` questions answered `true`.
    pub applied: usize,
    /// `shouldApplyMixin` questions answered `false`.
    pub declined: usize,
    /// Mixins whose question (or `preApply`/`postApply`) failed, with why;
    /// internal names.
    pub failed: Vec<(String, String)>,
    /// Companion Mixins `getMixins()` added, internal names.
    pub companions: Vec<String>,
}

/// One config's session: its report and the answers the gate reads.
#[derive(Debug, Clone, Default)]
struct PluginSession {
    report: ConfigPluginReport,
    /// `(mixin, target)` internal names → the plugin's answer, or why the
    /// question failed.
    answers: BTreeMap<(String, String), Result<bool, String>>,
}

/// Every plugin session of one jar on one side, as the weave planner asks it.
#[derive(Debug, Clone, Default)]
pub(crate) struct ConfigPlugins {
    sessions: BTreeMap<String, PluginSession>,
    /// Companion Mixins (internal names), from every session that ran.
    companions: BTreeSet<String>,
}

impl ConfigPlugins {
    /// The plugin's verdict on `mixin` at `target` (internal names) under
    /// `config`: `Ok` only when the session ran and the plugin answered
    /// `true` with no failing apply hook.
    pub(crate) fn decide(
        &self,
        config: &str,
        mixin: &str,
        target: &str,
    ) -> Result<(), MixinSkipReason> {
        let session = self
            .sessions
            .get(config)
            .ok_or(MixinSkipReason::ConfigPlugin)?;
        if session.report.refused.is_some() {
            return Err(MixinSkipReason::ConfigPlugin);
        }
        match session.answers.get(&(mixin.to_owned(), target.to_owned())) {
            Some(Ok(true)) => Ok(()),
            _ => Err(MixinSkipReason::ConfigPlugin),
        }
    }

    /// Whether a plugin's `getMixins()` added `mixin` (an internal name).
    pub(crate) fn is_companion(&self, mixin: &str) -> bool {
        self.companions.contains(mixin)
    }

    /// The sessions' reports, in config order of their paths.
    pub(crate) fn reports(&self) -> Vec<ConfigPluginReport> {
        self.sessions
            .values()
            .map(|session| session.report.clone())
            .collect()
    }
}

/// The text of one interpreter failure, bounded for a report row.
fn failure_text(error: &NativeError) -> String {
    bounded_weave_reason(&error.to_string())
}

/// `Value` of a Java string, or `null`.
fn java_text(value: Option<&str>) -> Value {
    Value::Ref(value.map(str::to_owned))
}

/// Binary (dotted) form of an internal name.
fn binary_name(internal: &str) -> String {
    internal.replace('/', ".")
}

impl NativeJar {
    /// Run every config plugin that loads on `side`, in config order, the way
    /// Mixin does (see the module documentation).
    ///
    /// The configs share one host, as they share one class loader: a static
    /// one plugin call set is there for the next, and two configs naming the
    /// same plugin class get two instances of it.
    pub(crate) fn run_config_plugins(
        &self,
        configs: &[MixinConfig],
        config_sides: &BTreeMap<String, MixinSide>,
        side: MixinSide,
    ) -> ConfigPlugins {
        let mut plugins = ConfigPlugins::default();
        let mut host: Option<StdHost> = None;
        for (index, config) in configs.iter().enumerate() {
            let Some(plugin) = config.plugin.as_deref() else {
                continue;
            };
            if config_sides
                .get(&config.path)
                .is_some_and(|only| *only != side)
            {
                continue;
            }
            let host = host.get_or_insert_with(|| self.config_plugin_host(side));
            let session = self.plugin_session(config, plugin, index, configs, side, host);
            plugins
                .companions
                .extend(session.report.companions.iter().cloned());
            plugins.sessions.insert(config.path.clone(), session);
        }
        plugins
    }

    /// Run the jar's config plugins on an entrypoint's own host before its
    /// first entrypoint, as a real loader has run every plugin by then.
    ///
    /// Mixin constructs each config's plugin and calls it (the whole session
    /// above) while it selects configs, before the loader runs a single
    /// entrypoint, and on the same class loader: a static a plugin's `onLoad`
    /// sets is there for the mod's code. whiteout's `onInitialize` throws
    /// "The mixin plugin did not initialize the config!" without it.
    ///
    /// This never refuses the jar. A plugin whose class is not in this jar is
    /// left alone (it belongs to another jar's loader). A session that fails
    /// at a config-level step (construction, `onLoad`, `getRefMapperConfig`,
    /// `acceptTargets`, `getMixins`) is rolled back: the host's static fields,
    /// initialised classes and resident heap return to what they were, so the
    /// entrypoint runs exactly as it did without plugins. A failed single
    /// `shouldApplyMixin` keeps the session, as the gate does. A plugin class
    /// already initialised on this host has had its session (an earlier
    /// entrypoint of the jar), so it is not run twice.
    ///
    /// `kind` picks the side whose list the plugin is asked about: `client`
    /// the client's, every other entrypoint kind the server's. The answers
    /// are discarded; only the gate's own session decides what binds.
    pub(crate) fn prepare_config_plugins_for_entrypoint<H: Host>(&self, kind: &str, host: &mut H) {
        let side = if kind == "client" {
            MixinSide::Client
        } else {
            MixinSide::Server
        };
        let configs = self.mixin_configs();
        if configs.iter().all(|config| config.plugin.is_none()) {
            return;
        }
        let config_sides = self.mixin_config_sides();
        for (index, config) in configs.iter().enumerate() {
            let Some(plugin) = config.plugin.as_deref() else {
                continue;
            };
            let owner = plugin.replace('.', "/");
            if !self.classes.contains_key(&owner)
                || config_sides
                    .get(&config.path)
                    .is_some_and(|only| *only != side)
                || host.persistent_class_state().1.contains(&owner)
            {
                continue;
            }
            let (fields, initialized) = host.persistent_class_state();
            // A copy of the heap to roll back to; the host keeps its own.
            let heap = host.take_persistent_heap();
            if let Some(heap) = &heap {
                let _ = host.store_persistent_heap(heap.clone());
            }
            let session = self.plugin_session(config, plugin, index, &configs, side, host);
            if session.report.refused.is_some() {
                let _ = host.store_persistent_class_state(fields, initialized);
                if let Some(heap) = heap {
                    let _ = host.take_persistent_heap();
                    let _ = host.store_persistent_heap(heap);
                }
            }
        }
    }

    /// The host a plugin runs on: the jar alone on `side`, strict.
    fn config_plugin_host(&self, side: MixinSide) -> StdHost {
        let mut host = self
            .seed_host(StdHost::new())
            .with_environment_type(match side {
                MixinSide::Client => "CLIENT",
                MixinSide::Server => "SERVER",
            });
        host.set_strict_unknown_host_calls(true);
        host
    }

    /// One config's session, steps 1–6 and the apply hooks.
    fn plugin_session<H: Host>(
        &self,
        config: &MixinConfig,
        plugin: &str,
        index: usize,
        configs: &[MixinConfig],
        side: MixinSide,
        host: &mut H,
    ) -> PluginSession {
        let mut session = PluginSession {
            report: ConfigPluginReport {
                config: config.path.clone(),
                plugin: plugin.to_owned(),
                ..ConfigPluginReport::default()
            },
            answers: BTreeMap::new(),
        };
        if let Err(reason) =
            self.drive_plugin(config, plugin, index, configs, side, host, &mut session)
        {
            session.report.refused = Some(reason);
        }
        session
    }

    #[allow(clippy::too_many_arguments)]
    fn drive_plugin<H: Host>(
        &self,
        config: &MixinConfig,
        plugin: &str,
        index: usize,
        configs: &[MixinConfig],
        side: MixinSide,
        host: &mut H,
        session: &mut PluginSession,
    ) -> Result<(), String> {
        let owner = plugin.replace('.', "/");
        if !self.classes.contains_key(&owner) {
            return Err(format!("plugin class {plugin} is not in this jar"));
        }
        // One object per config, as Mixin builds one `PluginHandle` each. The
        // marker is not a `new` marker, so the heap never collects its fields.
        let this = Value::Ref(Some(format!("{owner}#plugin{index}")));
        let call = |host: &mut H,
                    name: &str,
                    descriptor: &str,
                    args: &[Value]|
         -> Result<Option<Value>, String> {
            // The declaring class in the jar's chain; a method that is not
            // there is inherited from a class outside the jar.
            let Some(declaring) = self.entrypoint_method_owner(&owner, name, descriptor) else {
                return Ok(None);
            };
            let target = MethodRef {
                owner: declaring,
                name: name.into(),
                descriptor: descriptor.into(),
            };
            let budget = ExecutionBudget::within(CONFIG_PLUGIN_CALL_BUDGET);
            self.invoke_persistent(&target, Some(this.clone()), args, host, &budget)
                .map(Some)
                .map_err(|error| format!("{name}: {}", failure_text(&error)))
        };
        // 1. The constructor. A plugin without one in the jar (its chain
        // leaves the jar) cannot be built here.
        if call(host, "<init>", "()V", &[])?.is_none() {
            return Err(format!(
                "{plugin} has no no-argument constructor in this jar"
            ));
        }
        // 2-3. `onLoad` and `getRefMapperConfig`; an absent method is an
        // inherited one from outside the jar, which a plugin that extends a
        // library base would need modelled.
        let required = |found: Option<Value>, name: &str| {
            found.ok_or_else(|| format!("{name} is inherited from a class outside the jar"))
        };
        required(
            call(
                host,
                "onLoad",
                "(Ljava/lang/String;)V",
                &[java_text(config.package.as_deref())],
            )?,
            "onLoad",
        )?;
        required(
            call(host, "getRefMapperConfig", "()Ljava/lang/String;", &[])?,
            "getRefMapperConfig",
        )?;
        // 4. `shouldApplyMixin` for every Mixin the config lists here.
        let listed = config.mixins.iter().chain(match side {
            MixinSide::Client => &config.client,
            MixinSide::Server => &config.server,
        });
        let should = "(Ljava/lang/String;Ljava/lang/String;)Z";
        if self
            .entrypoint_method_owner(&owner, "shouldApplyMixin", should)
            .is_none()
        {
            return Err("shouldApplyMixin is inherited from a class outside the jar".into());
        }
        let mut kept_targets = BTreeSet::new();
        let mut kept = Vec::new();
        for mixin_binary in listed {
            let mixin = mixin_binary.replace('.', "/");
            for target in self.mixin_class_targets(&mixin) {
                let answer = call(
                    host,
                    "shouldApplyMixin",
                    should,
                    &[
                        java_text(Some(&binary_name(&target))),
                        java_text(Some(mixin_binary)),
                    ],
                )
                .and_then(|value| match value {
                    Some(Value::Int(answer)) => Ok(answer != 0),
                    other => Err(format!("shouldApplyMixin answered {other:?}")),
                });
                match &answer {
                    Ok(true) => {
                        session.report.applied += 1;
                        kept_targets.insert(binary_name(&target));
                        kept.push((mixin.clone(), target.clone()));
                    }
                    Ok(false) => session.report.declined += 1,
                    Err(reason) => session.report.failed.push((mixin.clone(), reason.clone())),
                }
                session.answers.insert((mixin.clone(), target), answer);
            }
        }
        // 5. `acceptTargets(myTargets, otherTargets)`.
        let other_targets = configs
            .iter()
            .filter(|other| other.path != config.path)
            .flat_map(|other| {
                other
                    .mixins
                    .iter()
                    .chain(&other.client)
                    .chain(&other.server)
            })
            .flat_map(|mixin| self.mixin_class_targets(&mixin.replace('.', "/")))
            .map(|target| binary_name(&target))
            .collect::<BTreeSet<_>>();
        let set = |names: &BTreeSet<String>| {
            collection_value(
                "set",
                &names
                    .iter()
                    .map(|name| java_text(Some(name)))
                    .collect::<Vec<_>>(),
            )
        };
        required(
            call(
                host,
                "acceptTargets",
                "(Ljava/util/Set;Ljava/util/Set;)V",
                &[set(&kept_targets), set(&other_targets)],
            )?,
            "acceptTargets",
        )?;
        // 6. `getMixins()`: companions, relative to the package.
        let companions = required(
            call(host, "getMixins", "()Ljava/util/List;", &[])?,
            "getMixins",
        )?;
        if companions != Value::Ref(None) {
            let names = runtime_collection_values(&*host, &companions)
                .map_err(|error| format!("getMixins: {}", failure_text(&error)))?;
            if names.len() > MAX_COMPANION_MIXINS {
                return Err(format!(
                    "getMixins answered {} companion Mixins, over the bound of {MAX_COMPANION_MIXINS}",
                    names.len()
                ));
            }
            let prefix = config
                .package
                .as_deref()
                .filter(|package| !package.is_empty())
                .map(|package| format!("{package}."))
                .unwrap_or_default();
            for name in names {
                match parse_value_marker(&name) {
                    Ok(Value::Ref(Some(name))) => session
                        .report
                        .companions
                        .push(format!("{prefix}{name}").replace('.', "/")),
                    other => {
                        return Err(format!("getMixins listed a non-string element {other:?}"));
                    }
                }
            }
        }
        // The apply hooks, once per kept (mixin, target), with stand-ins.
        let apply = "(Ljava/lang/String;Lorg/objectweb/asm/tree/ClassNode;Ljava/lang/String;Lorg/spongepowered/asm/mixin/extensibility/IMixinInfo;)V";
        for (mixin, target) in kept {
            let args = [
                java_text(Some(&binary_name(&target))),
                Value::Ref(Some(format!("{CLASS_NODE_PREFIX}{target}"))),
                java_text(Some(&binary_name(&mixin))),
                Value::Ref(Some(format!("{MIXIN_INFO_PREFIX}{mixin}"))),
            ];
            let outcome = call(host, "preApply", apply, &args)
                .and_then(|pre| {
                    pre.ok_or_else(|| "preApply is inherited from a class outside the jar".into())
                })
                .and_then(|_| call(host, "postApply", apply, &args))
                .and_then(|post| {
                    post.ok_or_else(|| "postApply is inherited from a class outside the jar".into())
                });
            if let Err(reason) = outcome {
                session.report.applied -= 1;
                session.report.failed.push((mixin.clone(), reason.clone()));
                session.answers.insert((mixin, target), Err(reason));
            }
        }
        Ok(())
    }

    /// The targets a `@Mixin` class names, internal names, in annotation
    /// order; empty for a class this jar does not hold or that is no Mixin.
    pub(crate) fn mixin_class_targets(&self, mixin: &str) -> Vec<String> {
        self.classes
            .get(mixin)
            .and_then(|class| {
                class
                    .annotations
                    .iter()
                    .find(|annotation| annotation.descriptor == MIXIN_ANNOTATION_DESCRIPTOR)
            })
            .map(mixin_target_owners)
            .unwrap_or_default()
    }
}

/// Mixin's environment object, as `getCurrentEnvironment()` answers it.
const MIXIN_ENVIRONMENT: &str = "mixin-environment";
/// `MixinEnvironment.Side` constants, `mixin-side/<NAME>`.
const MIXIN_SIDE_PREFIX: &str = "mixin-side/";
const MIXIN_ENVIRONMENT_CLASS: &str = "org/spongepowered/asm/mixin/MixinEnvironment";
const MIXIN_SIDE_CLASS: &str = "org/spongepowered/asm/mixin/MixinEnvironment$Side";
/// `MixinEnvironment.Side`'s constants in declaration order (their ordinals).
const MIXIN_SIDES: [&str; 3] = ["UNKNOWN", "CLIENT", "SERVER"];

/// The Mixin platform a config plugin (or any jar code) reaches before the
/// game: the questions it asks the loader's own Mixin, answered from the
/// host's side. Everything else Mixin's environment offers stays an
/// unmodelled call, so a plugin that reads an option or registers a token
/// provider still refuses with that member named.
impl StdHost {
    /// `MixinEnvironment.Side.UNKNOWN`/`CLIENT`/`SERVER`.
    pub(crate) fn mixin_platform_static(&self, r: &MethodRef) -> Option<Value> {
        (r.owner == MIXIN_SIDE_CLASS && MIXIN_SIDES.contains(&r.name.as_str()))
            .then(|| Value::Ref(Some(format!("{MIXIN_SIDE_PREFIX}{}", r.name))))
    }

    /// Static calls: `MixinEnvironment.getCurrentEnvironment()` and
    /// `getDefaultEnvironment()`, and MixinExtras' `MixinExtrasBootstrap.init()`,
    /// which the loader has already run when a plugin's `onLoad` calls it
    /// (Fabric Loader ships MixinExtras and initialises it before any config;
    /// a second `init` does nothing), so it returns at once.
    pub(crate) fn mixin_platform_invoke(&self, r: &MethodRef) -> Option<Value> {
        match (r.owner.as_str(), r.name.as_str(), r.descriptor.as_str()) {
            (
                MIXIN_ENVIRONMENT_CLASS,
                "getCurrentEnvironment" | "getDefaultEnvironment",
                "()Lorg/spongepowered/asm/mixin/MixinEnvironment;",
            ) => Some(Value::Ref(Some(MIXIN_ENVIRONMENT.into()))),
            ("com/llamalad7/mixinextras/MixinExtrasBootstrap", "init", "()V") => Some(Value::Void),
            _ => None,
        }
    }

    /// Instance calls: the environment's `getSide()` (the host's side, as
    /// Fabric sets it from its environment type) and a side constant's
    /// `name()`, `toString()` and `ordinal()`. Any call on the `ClassNode` or
    /// `IMixinInfo` stand-in a plugin's `preApply`/`postApply` receives is
    /// refused with the member named, whatever the host's strictness: the
    /// hook tier applies no bytecode, so there is no node to read or edit.
    pub(crate) fn mixin_platform_instance(
        &self,
        r: &MethodRef,
        receiver: &Value,
    ) -> Option<Result<Value, NativeError>> {
        let marker = expect_ref(receiver)?;
        if marker.starts_with(CLASS_NODE_PREFIX) || marker.starts_with(MIXIN_INFO_PREFIX) {
            return Some(Err(NativeError::UnknownHostCall(r.clone())));
        }
        if marker == MIXIN_ENVIRONMENT
            && r.name == "getSide"
            && r.descriptor == "()Lorg/spongepowered/asm/mixin/MixinEnvironment$Side;"
        {
            let side = if self.environment_type == "SERVER" {
                "SERVER"
            } else {
                "CLIENT"
            };
            return Some(Ok(Value::Ref(Some(format!("{MIXIN_SIDE_PREFIX}{side}")))));
        }
        let side = marker.strip_prefix(MIXIN_SIDE_PREFIX)?;
        let ordinal = MIXIN_SIDES.iter().position(|name| *name == side)?;
        match (r.name.as_str(), r.descriptor.as_str()) {
            ("name" | "toString", "()Ljava/lang/String;") => {
                Some(Ok(Value::Ref(Some(side.into()))))
            }
            ("ordinal", "()I") => Some(Ok(Value::Int(ordinal as i32))),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The measured question: across a list of real jars, how many
    /// server-side game plans name a hook site in a shape the tier serves,
    /// how many bind through every gate, and what the config plugins that
    /// still refuse stop at.
    ///
    /// `HORIZON_JAVA_PLUGIN_CENSUS` is a text file of one jar path per line
    /// (the loader set of `game_hook_tier_measures_the_phase_two_loader_set`,
    /// or every jar of a corpus). Counting follows that test: a plan is
    /// server-side unless its config lists it client-only, and a game plan
    /// when its target class is outside the jar.
    ///
    /// ```sh
    /// HORIZON_JAVA_PLUGIN_CENSUS=jars.txt \
    ///   cargo test -p horizon-native-java --lib -- --ignored plugin_census --nocapture
    /// ```
    #[test]
    #[ignore = "reads a real mod corpus outside the repository"]
    fn plugin_census_of_a_jar_list() {
        let Ok(list) = std::env::var("HORIZON_JAVA_PLUGIN_CENSUS") else {
            eprintln!("skipping: set HORIZON_JAVA_PLUGIN_CENSUS");
            return;
        };
        let paths = std::fs::read_to_string(&list).expect("read the jar list");
        let (mut jars, mut game_plans, mut named, mut bound) = (0, 0, 0, 0);
        let mut unbound: BTreeMap<String, usize> = BTreeMap::new();
        let mut refusals: BTreeMap<String, (usize, usize)> = BTreeMap::new();
        let mut failures: BTreeMap<String, usize> = BTreeMap::new();
        let (mut sessions, mut ran, mut companions) = (0, 0, 0);
        let mut bound_through_plugin = 0;
        let mut extras: BTreeMap<&'static str, (usize, usize, usize)> = BTreeMap::new();
        for path in paths.lines().map(str::trim).filter(|line| !line.is_empty()) {
            let Ok(jar) = NativeJar::from_path(std::path::Path::new(path)) else {
                continue;
            };
            jars += 1;
            let configs = jar.mixin_configs();
            let mut named_here = BTreeSet::new();
            for plan in jar.mixin_injection_plans() {
                let callback = plan.callback.owner.replace('/', ".");
                let (mut server, mut client) = (false, false);
                for config in &configs {
                    let common = config.mixins.contains(&callback);
                    server |= common || config.server.contains(&callback);
                    client |= common || config.client.contains(&callback);
                }
                if (client && !server) || jar.classes.contains_key(&plan.target.owner) {
                    continue;
                }
                game_plans += 1;
                let Some(site) = game_hook_site_at(&plan.target, &plan.at) else {
                    continue;
                };
                if plan.shift.is_some() || plan.slice.is_some() || plan.ordinal.is_some() {
                    continue;
                }
                let anchor = if plan.at.is_empty() {
                    "HEAD"
                } else {
                    plan.at.as_str()
                };
                if game_hook_site_fits(site, anchor, plan.cancellable, MixinSide::Server).is_ok()
                    && game_hook_callback(&jar.classes, &plan.callback, site).is_ok()
                {
                    named += 1;
                    named_here.insert(plan.callback.owner.clone());
                }
            }
            let weave = jar.plan_load_time_weave_with_game_hooks(MixinSide::Server);
            bound += weave.game_hooks.len();
            let sides = jar.mixin_config_sides();
            bound_through_plugin += weave
                .game_hooks
                .iter()
                .filter(|hook| {
                    let owner = &hook.plan.callback.owner;
                    let governing = configs.iter().find(|config| {
                        let binary = owner.replace('/', ".");
                        (config.mixins.contains(&binary) || config.server.contains(&binary))
                            && sides
                                .get(&config.path)
                                .is_none_or(|only| *only == MixinSide::Server)
                    });
                    governing.is_some_and(|config| config.plugin.is_some())
                        || weave
                            .config_plugins
                            .iter()
                            .any(|report| report.companions.contains(owner))
                })
                .count();
            // MixinExtras: (server-side game plans, on a site, bound) by kind.
            for plan in jar.mixin_extras_plans() {
                let callback = plan.callback.owner.replace('/', ".");
                let (mut server, mut client) = (false, false);
                for config in &configs {
                    let common = config.mixins.contains(&callback);
                    server |= common || config.server.contains(&callback);
                    client |= common || config.client.contains(&callback);
                }
                if (client && !server) || jar.classes.contains_key(&plan.target.owner) {
                    continue;
                }
                let anchor = if plan.at.is_empty() {
                    "HEAD"
                } else {
                    plan.at.as_str()
                };
                let row = extras.entry(plan.kind.label()).or_default();
                row.0 += 1;
                row.1 += usize::from(game_hook_site_at(&plan.target, anchor).is_some());
                row.2 += usize::from(weave.game_hooks.iter().any(|hook| {
                    hook.plan.callback == plan.callback && hook.shape != GameHookShape::Inject
                }));
            }
            for skipped in &weave.skipped {
                if !jar.classes.contains_key(&skipped.target.owner)
                    && game_hook_site_at(&skipped.target, &skipped.at).is_some()
                {
                    *unbound
                        .entry(skipped.reason.label().to_owned())
                        .or_default() += 1;
                }
            }
            let name = std::path::Path::new(path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            for report in &weave.config_plugins {
                sessions += 1;
                companions += report.companions.len();
                // Does this plugin govern a plan on a named site?
                let governs = named_here.iter().any(|owner| {
                    let binary = owner.replace('/', ".");
                    configs.iter().any(|config| {
                        config.path == report.config
                            && (config.mixins.contains(&binary) || config.server.contains(&binary))
                    })
                });
                match &report.refused {
                    Some(reason) => {
                        let key: String = reason.chars().take(150).collect();
                        let row = refusals.entry(key).or_default();
                        row.0 += 1;
                        row.1 += usize::from(governs);
                        if governs {
                            eprintln!("refused {name} {}: {reason}", report.plugin);
                        }
                    }
                    None => ran += 1,
                }
                for (mixin, reason) in &report.failed {
                    let key: String = reason.chars().take(320).collect();
                    *failures.entry(key).or_default() += 1;
                    if named_here.contains(mixin) {
                        eprintln!("failed {name} {mixin}: {reason}");
                    }
                }
            }
        }
        println!(
            "{jars} jar(s), {game_plans} server-side game plan(s): {named} name a site in a served shape, {bound} bind"
        );
        println!("{bound_through_plugin} of the bound plans passed a config plugin's session");
        println!("plans on a site that do not bind, by gate:");
        for (reason, count) in &unbound {
            println!("  {count:5} {reason}");
        }
        println!("MixinExtras server-side game plans (on a site, bound):");
        for (kind, (count, on_site, bound)) in &extras {
            println!("  {count:5} {kind} ({on_site} on a site, {bound} bound)");
        }
        println!("{sessions} plugin session(s): {ran} ran, {companions} companion Mixin(s)");
        let mut rows = refusals.into_iter().collect::<Vec<_>>();
        rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        println!("refused sessions by cause (count, governing a named plan):");
        for (reason, (count, governing)) in rows.iter().take(40) {
            println!("  {count:4} {governing:4} {reason}");
        }
        let mut rows = failures.into_iter().collect::<Vec<_>>();
        rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        println!("failed questions by cause:");
        for (reason, count) in rows.iter().take(25) {
            println!("  {count:4} {reason}");
        }
    }
}
