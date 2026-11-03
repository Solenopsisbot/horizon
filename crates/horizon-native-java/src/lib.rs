//! A bounded native probe for simple Java mod entrypoints.
//!
//! This is deliberately smaller than a JVM. It parses class files, discovers
//! Fabric metadata, and executes a small no-argument bytecode subset. Calls
//! outside the loaded jar cross [`Host`], so unsupported behavior is explicit.

use std::{collections::BTreeMap, fs::File, path::Path};

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i32),
    Ref(Option<String>),
    Void,
}

impl Value {
    fn default_for(desc: &str) -> Self {
        match desc {
            "V" => Self::Void,
            "Z" | "B" | "C" | "S" | "I" => Self::Int(0),
            // Wide primitive values use the bounded integer representation.
            "J" | "D" => Self::Int(0),
            _ => Self::Ref(None),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodRef {
    pub owner: String,
    pub name: String,
    pub descriptor: String,
}

/// A bounded native translation for one Mixin callback.
///
/// The native path deliberately accepts only the common `@Inject(at = HEAD)`
/// shape.  The callback is invoked before the target method, so the target
/// keeps its normal return value.  `cancellable` is retained in the plan to
/// make the unsupported contract explicit: cancellation requires a real
/// `CallbackInfo` object and is left to the JVM fallback for now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MixinInjectionPlan {
    pub target: MethodRef,
    pub callback: MethodRef,
    pub at: String,
    pub cancellable: bool,
}

impl MixinInjectionPlan {
    pub fn inject_at_head(target: MethodRef, callback: MethodRef) -> Self {
        Self {
            target,
            callback,
            at: "HEAD".into(),
            cancellable: false,
        }
    }
}

/// Results from executing one supported native Mixin translation.
#[derive(Debug, Clone, PartialEq)]
pub struct MixinExecutionReport {
    pub callback_result: Value,
    pub target_result: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HostInvocation {
    pub method: MethodRef,
    pub receiver: Option<Value>,
    pub args: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RegistryEntry {
    pub registry: String,
    pub id: String,
    pub value: Value,
    /// Optional typed content supplied by a Horizon-owned host.
    ///
    /// Java registration callbacks still produce opaque values by default.
    /// A loader adapter can attach a declaration once it has translated the
    /// callback's object construction into Horizon's content vocabulary.
    pub declaration: Option<RegistryDeclaration>,
}

/// A bounded native record for NeoForge payload registration.  The actual
/// network channel remains a game-owned boundary; retaining the direction and
/// values lets the launcher validate that bootstrap reached the registration
/// stage without pretending to encode arbitrary packet bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct PayloadRegistration {
    pub direction: String,
    pub payload_type: Value,
    pub codec: Value,
    pub handler: Value,
}

/// Metadata for one Fabric `EntrypointContainer` exposed by the bounded
/// loader host. The container is a query value: invoking the returned Java
/// object still requires a supported entrypoint class in the loaded jar and
/// is not dispatched through an arbitrary host classpath.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FabricEntrypointContainer {
    pub kind: String,
    pub class: String,
    pub provider: String,
}

/// A capability lookup crossing the native boundary. The native probe records
/// the request and returns only a value explicitly seeded by a game adapter;
/// absent values stay empty rather than inventing a live block capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityRequest {
    pub owner: String,
    pub capability: String,
    pub position: Option<String>,
    pub side: Option<String>,
}

impl CapabilityRequest {
    fn key(&self) -> String {
        format!(
            "{}|{}|{}|{}",
            self.owner,
            self.capability,
            self.position.as_deref().unwrap_or(""),
            self.side.as_deref().unwrap_or("")
        )
    }
}

impl RegistryEntry {
    pub fn key(&self) -> String {
        format!("{}/{}", self.registry, self.id)
    }

    /// Attach a typed declaration while preserving the Java-side value marker.
    pub fn with_declaration(mut self, declaration: RegistryDeclaration) -> Self {
        self.declaration = Some(declaration);
        self
    }

    /// Build an entry for an explicitly translated Horizon declaration.
    pub fn typed(
        registry: impl Into<String>,
        id: impl Into<String>,
        declaration: RegistryDeclaration,
    ) -> Self {
        let registry = registry.into();
        let id = id.into();
        Self {
            value: Value::Ref(Some(format!("typed/{registry}/{id}"))),
            registry,
            id,
            declaration: Some(declaration),
        }
    }
}

/// Typed content that a Horizon-owned loader adapter can export before the
/// immutable game registries are built. These declarations intentionally use
/// strings for properties: the adapter validates and converts them into the
/// richer `horizon-mods` content structs at the resource boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryDeclaration {
    Block {
        template: String,
        properties: BTreeMap<String, String>,
        item: bool,
    },
    Item {
        template: String,
        properties: BTreeMap<String, String>,
    },
    MobEffect {
        category: String,
        color: u32,
    },
    /// A data recipe emitted into `data/<namespace>/recipe/<path>.json` by
    /// the game-owned content adapter. The JSON is validated at metadata
    /// load time and remains data-only; no recipe code executes here.
    Recipe {
        json: String,
    },
    /// A data ingredient emitted into `data/<namespace>/ingredient/<path>.json`
    /// by the game-owned content adapter.
    Ingredient {
        json: String,
    },
    Custom {
        kind: String,
        properties: BTreeMap<String, String>,
    },
}

/// Destination for declarations that have crossed the Java-to-Horizon
/// translation boundary. This is separate from [`RegistrySink`] because an
/// opaque Java object is not safe to install in a live native registry.
pub trait TypedRegistrySink {
    fn register_typed(&mut self, entry: &RegistryEntry) -> Result<(), String>;
}

/// Game-independent destination for deterministic Java registration values.
///
/// A Horizon integration can implement this trait for its native registry
/// tables without making the bytecode interpreter depend on `horizon-client`.
pub trait RegistrySink {
    fn register(&mut self, entry: &RegistryEntry) -> Result<(), String>;
}

/// A callback registration selected for one event dispatch.
///
/// The native interpreter does not execute Java `Consumer` objects. It emits
/// these deterministic work items so a Horizon-owned host can route the event
/// to a native callback or an explicitly supported Java method.
#[derive(Debug, Clone, PartialEq)]
pub struct EventDispatch {
    pub listener: HostInvocation,
    pub event: Value,
}

pub trait Host {
    fn get_static(&mut self, _: &MethodRef) -> Result<Value, NativeError> {
        Ok(Value::Ref(None))
    }
    fn invoke(&mut self, r: &MethodRef, _: &[Value]) -> Result<Value, NativeError> {
        Ok(Value::default_for(return_desc(&r.descriptor)))
    }

    /// Invoke an instance method with its receiver preserved. Existing hosts
    /// keep their old behavior through the default forwarding implementation.
    fn invoke_instance(
        &mut self,
        r: &MethodRef,
        _receiver: &Value,
        args: &[Value],
    ) -> Result<Value, NativeError> {
        self.invoke(r, args)
    }
}

#[derive(Debug, Default)]
pub struct NullHost;
impl Host for NullHost {}

/// Deterministic, game-independent mappings for the small standard-library
/// surface commonly reached during mod bootstrap.
#[derive(Debug, Clone)]
pub struct StdHost {
    properties: BTreeMap<String, String>,
    /// Explicitly seeded environment values exposed through `System.getenv`.
    /// The host process environment is never consulted.
    environment: BTreeMap<String, String>,
    /// Explicitly seeded classpath resources exposed through the bounded
    /// `ClassLoader` query surface. The host never opens files or consults a
    /// process classpath.
    resources: BTreeMap<String, String>,
    /// Bounded bytes for resources explicitly supplied by a loaded jar or
    /// adapter. Marker-only resources keep an empty stream.
    resource_bytes: BTreeMap<String, Vec<u8>>,
    resource_streams: BTreeMap<String, (Vec<u8>, usize)>,
    resource_marks: BTreeMap<String, usize>,
    /// Explicitly seeded class names exposed through `ClassLoader.loadClass`.
    /// Values are class markers, and no host classpath or filesystem lookup is
    /// ever attempted.
    classes: BTreeMap<String, String>,
    /// Explicitly seeded Java service providers. Native execution never scans
    /// the host classpath; callers must supply the service and provider class
    /// names they want visible to `ServiceLoader`.
    service_providers: BTreeMap<String, Vec<String>>,
    /// Deterministic epoch value returned by `System.currentTimeMillis`.
    ///
    /// The native interpreter represents wide primitives with its bounded
    /// integer value, so this is intentionally an `i32` rather than a host
    /// clock read.  Keeping the value seeded makes bootstrap replayable.
    current_time_millis: i32,
    mods: BTreeMap<String, String>,
    environment_type: String,
    development_environment: bool,
    calls: Vec<MethodRef>,
    invocations: Vec<HostInvocation>,
    posted_events: Vec<Value>,
    registrations: Vec<RegistryEntry>,
    payload_registrations: Vec<PayloadRegistration>,
    /// Fabric lifecycle entries visible through `FabricLoader#getEntrypoints`.
    /// Entries are seeded by the loaded jar, so this query never scans a host
    /// classpath or invents providers.
    fabric_entrypoint_containers: Vec<FabricEntrypointContainer>,
    /// Identifiers retained by `CustomPacketPayload.Type` values created by
    /// the bounded networking façade.
    packet_types: BTreeMap<String, String>,
    holders: BTreeMap<String, Value>,
    mutable_collections: BTreeMap<String, Vec<Value>>,
    iterators: BTreeMap<String, (Vec<Value>, usize)>,
    /// Callback-local values returned by NeoForge's `LazyOptional` façade.
    /// The marker is intentionally host-owned: an opaque Java object must not
    /// become a live native capability until a game adapter supplies it.
    lazy_optionals: BTreeMap<String, Option<Value>>,
    capability_values: BTreeMap<String, Value>,
    capability_requests: Vec<CapabilityRequest>,
    /// Messages and causes captured by bounded Throwable constructors. The
    /// interpreter keeps objects as markers, so this small side table is
    /// enough for bootstrap diagnostics without modelling Java stack traces.
    throwable_messages: BTreeMap<String, Option<String>>,
    throwable_causes: BTreeMap<String, Option<Value>>,
    next_lazy_optional: usize,
    next_fabric_event: usize,
    next_iterator: usize,
}

impl Default for StdHost {
    fn default() -> Self {
        Self::new()
    }
}

impl StdHost {
    pub fn new() -> Self {
        Self {
            properties: BTreeMap::from([("line.separator".into(), "\n".into())]),
            environment: BTreeMap::new(),
            resources: BTreeMap::new(),
            resource_bytes: BTreeMap::new(),
            resource_streams: BTreeMap::new(),
            resource_marks: BTreeMap::new(),
            classes: BTreeMap::new(),
            service_providers: BTreeMap::new(),
            current_time_millis: 0,
            mods: BTreeMap::new(),
            environment_type: "CLIENT".into(),
            development_environment: false,
            calls: Vec::new(),
            invocations: Vec::new(),
            posted_events: Vec::new(),
            registrations: Vec::new(),
            payload_registrations: Vec::new(),
            fabric_entrypoint_containers: Vec::new(),
            packet_types: BTreeMap::new(),
            holders: BTreeMap::new(),
            mutable_collections: BTreeMap::new(),
            iterators: BTreeMap::new(),
            lazy_optionals: BTreeMap::new(),
            capability_values: BTreeMap::new(),
            capability_requests: Vec::new(),
            throwable_messages: BTreeMap::new(),
            throwable_causes: BTreeMap::new(),
            next_lazy_optional: 0,
            next_fabric_event: 0,
            next_iterator: 0,
        }
    }

    pub fn with_property(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.properties.insert(key.into(), value.into());
        self
    }

    /// Seed a deterministic environment value returned by `System.getenv`.
    /// Unseeded keys remain absent; this method never reads the process environment.
    pub fn with_environment_variable(
        mut self,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.environment.insert(key.into(), value.into());
        self
    }

    /// Seed a deterministic classpath resource marker. `ClassLoader` resource
    /// queries return this marker when `name` is requested; absent names stay
    /// null. This is metadata only and never reads a host file.
    pub fn with_resource(mut self, name: impl Into<String>, marker: impl Into<String>) -> Self {
        let name = name.into().trim_start_matches('/').to_owned();
        let marker = marker.into();
        self.resources.insert(name, marker.clone());
        self.resource_streams
            .entry(marker)
            .or_insert((Vec::new(), 0));
        self
    }

    /// Seed bounded bytes for an explicitly supplied resource. This never
    /// opens a host path or consults a process classpath.
    pub fn with_resource_bytes(mut self, name: impl Into<String>, bytes: Vec<u8>) -> Self {
        let name = name.into().trim_start_matches('/').to_owned();
        let marker = self
            .resources
            .get(&name)
            .cloned()
            .unwrap_or_else(|| format!("resource-bytes/{name}"));
        self.resources.insert(name.clone(), marker.clone());
        self.resource_bytes.insert(name, bytes.clone());
        self.resource_streams.insert(marker, (bytes, 0));
        self
    }

    /// Query an explicitly seeded resource marker without crossing into the
    /// host filesystem or process classpath.
    pub fn resource(&self, name: &str) -> Option<&str> {
        self.resources
            .get(name.strip_prefix('/').unwrap_or(name))
            .map(String::as_str)
    }

    /// Alias named for callers that want to make the query boundary explicit.
    pub fn query_resource(&self, name: &str) -> Option<&str> {
        self.resource(name)
    }

    fn resource_enumeration(&mut self, name: &str) -> Value {
        let marker = format!("resource-enumeration/{}", self.next_iterator);
        self.next_iterator = self.next_iterator.saturating_add(1);
        let values = self
            .resource(name)
            .map(|value| vec![Value::Ref(Some(value.to_owned()))])
            .unwrap_or_default();
        self.iterators.insert(marker.clone(), (values, 0));
        Value::Ref(Some(marker))
    }

    fn resource_stream(&mut self, name: &str) -> Value {
        let name = name.strip_prefix('/').unwrap_or(name);
        let Some(marker) = self.resource(name).map(str::to_owned) else {
            return Value::Ref(None);
        };
        if let Some(bytes) = self.resource_bytes.get(name).cloned() {
            self.resource_streams.insert(marker.clone(), (bytes, 0));
        } else {
            self.resource_streams
                .entry(marker.clone())
                .or_insert((Vec::new(), 0));
        }
        Value::Ref(Some(marker))
    }

    /// Seed one deterministic class marker for the bounded
    /// `ClassLoader.loadClass` contract. Java binary names and internal names
    /// are normalized to the same slash-separated key.
    pub fn with_class(mut self, name: impl Into<String>) -> Self {
        let internal = name.into().replace('.', "/");
        self.classes.insert(internal.clone(), internal);
        self
    }

    /// Query an explicitly seeded class marker without crossing into the host
    /// classpath or filesystem.
    pub fn class_marker(&self, name: &str) -> Option<&str> {
        self.classes
            .get(&name.replace('.', "/"))
            .map(String::as_str)
    }

    /// Seed one deterministic provider for a Java service. Provider discovery
    /// remains explicit and invocation-local; no JDK or host classpath lookup
    /// is performed.
    pub fn with_service_provider(
        mut self,
        service: impl Into<String>,
        provider: impl Into<String>,
    ) -> Self {
        let providers = self.service_providers.entry(service.into()).or_default();
        let provider = provider.into();
        if !providers.contains(&provider) {
            providers.push(provider);
        }
        self
    }

    /// Return the explicitly seeded providers for a service, if any.
    pub fn service_providers(&self, service: &str) -> Option<&[String]> {
        self.service_providers.get(service).map(Vec::as_slice)
    }

    /// Set the deterministic value returned by `System.currentTimeMillis`.
    ///
    /// Native probes must not read the machine clock; callers that need a
    /// stable non-zero epoch can seed one explicitly for the invocation.
    pub fn with_current_time_millis(mut self, value: i32) -> Self {
        self.current_time_millis = value;
        self
    }

    pub fn with_mod(mut self, id: impl Into<String>, version: impl Into<String>) -> Self {
        self.mods.insert(id.into(), version.into());
        self
    }

    /// Set Fabric's environment marker returned by `getEnvironmentType`.
    /// The default is `CLIENT`, matching the native probe's client-first
    /// behavior; server integrations should select `SERVER`.
    pub fn with_environment_type(mut self, environment_type: impl Into<String>) -> Self {
        self.environment_type = environment_type.into();
        self
    }

    /// Set Fabric's development-environment marker returned by
    /// `isDevelopmentEnvironment`.
    pub fn with_development_environment(mut self, development_environment: bool) -> Self {
        self.development_environment = development_environment;
        self
    }

    fn mutable_values(&self, receiver: &Value) -> Option<Vec<String>> {
        let marker = expect_ref(receiver)?;
        self.mutable_collections
            .get(marker)
            .map(|values| values.iter().map(value_marker).collect::<Vec<_>>())
    }

    fn ensure_mutable_collection(&mut self, receiver: &Value) {
        if let Some(marker) = expect_ref(receiver)
            && matches!(
                marker
                    .split_once('#')
                    .map_or(marker.as_str(), |(class, _)| class),
                "java/util/ArrayList"
                    | "java/util/HashMap"
                    | "java/util/LinkedHashMap"
                    | "java/util/HashSet"
                    | "java/util/LinkedHashSet"
            )
        {
            self.mutable_collections.entry(marker.clone()).or_default();
        }
    }

    fn lazy_optional(&mut self, value: Option<Value>) -> Value {
        let marker = format!("lazy-optional/{}", self.next_lazy_optional);
        self.next_lazy_optional = self.next_lazy_optional.saturating_add(1);
        self.lazy_optionals.insert(marker.clone(), value);
        Value::Ref(Some(marker))
    }

    fn lazy_contents(&self, receiver: &Value) -> Option<Option<Value>> {
        let marker = expect_ref(receiver)?;
        self.lazy_optionals.get(marker).cloned()
    }

    pub fn calls(&self) -> &[MethodRef] {
        &self.calls
    }

    pub fn invocations(&self) -> &[HostInvocation] {
        &self.invocations
    }

    /// Events posted through the bounded NeoForge event-bus contract, in
    /// invocation order.  The native host does not model the full event
    /// inheritance/cancellation graph; callers can inspect these values and
    /// route them through an explicitly supported Horizon hook.
    pub fn posted_events(&self) -> &[Value] {
        &self.posted_events
    }

    pub fn registrations(&self) -> &[RegistryEntry] {
        &self.registrations
    }

    /// Attach a Horizon-owned declaration to a registration recorded while a
    /// Java entrypoint executed. The lookup is explicit and all-or-nothing:
    /// opaque entries remain diagnostic until a loader adapter supplies a
    /// declaration, and an already-enriched entry cannot be replaced silently.
    pub fn attach_registration_declaration(
        &mut self,
        registry: &str,
        id: &str,
        declaration: RegistryDeclaration,
    ) -> Result<(), String> {
        let entry = self
            .registrations
            .iter_mut()
            .find(|entry| entry.registry == registry && entry.id == id)
            .ok_or_else(|| format!("native registration {registry}/{id} was not recorded"))?;
        if entry.declaration.is_some() {
            return Err(format!(
                "native registration {registry}/{id} already has a declaration"
            ));
        }
        entry.declaration = Some(declaration);
        Ok(())
    }

    pub fn payload_registrations(&self) -> &[PayloadRegistration] {
        &self.payload_registrations
    }

    /// Add one bounded Fabric entrypoint container to the loader view.
    /// Duplicate `(kind, class, provider)` rows are ignored to keep discovery
    /// deterministic when a launcher seeds the same jar more than once.
    pub fn with_fabric_entrypoint(
        mut self,
        kind: impl Into<String>,
        class: impl Into<String>,
        provider: impl Into<String>,
    ) -> Self {
        let entry = FabricEntrypointContainer {
            kind: kind.into(),
            class: class.into(),
            provider: provider.into(),
        };
        if !self.fabric_entrypoint_containers.contains(&entry) {
            self.fabric_entrypoint_containers.push(entry);
        }
        self
    }

    /// Inspect the explicitly seeded Fabric entrypoint containers.
    pub fn fabric_entrypoint_containers(&self) -> &[FabricEntrypointContainer] {
        &self.fabric_entrypoint_containers
    }

    /// Seed one capability result for a game-owned block/entity adapter.
    /// Requests without a matching seed remain empty.
    pub fn seed_capability(
        &mut self,
        owner: impl Into<String>,
        capability: impl Into<String>,
        position: Option<impl Into<String>>,
        side: Option<impl Into<String>>,
        value: Value,
    ) {
        let request = CapabilityRequest {
            owner: owner.into(),
            capability: capability.into(),
            position: position.map(Into::into),
            side: side.map(Into::into),
        };
        self.capability_values.insert(request.key(), value);
    }

    /// Capability lookups observed by the native runtime in invocation order.
    pub fn capability_requests(&self) -> &[CapabilityRequest] {
        &self.capability_requests
    }

    pub fn apply_registrations<S: RegistrySink>(&self, sink: &mut S) -> Result<usize, String> {
        let mut applied = 0;
        for entry in &self.registrations {
            sink.register(entry)?;
            applied += 1;
        }
        Ok(applied)
    }

    /// Apply only entries that a loader adapter translated into typed content.
    pub fn apply_typed_registrations<S: TypedRegistrySink>(
        &self,
        sink: &mut S,
    ) -> Result<usize, String> {
        let mut applied = 0;
        for entry in &self.registrations {
            if entry.declaration.is_some() {
                sink.register_typed(entry)?;
                applied += 1;
            }
        }
        Ok(applied)
    }

    pub fn listener_registrations(&self) -> impl Iterator<Item = &HostInvocation> {
        self.invocations.iter().filter(|invocation| {
            ((is_event_bus_owner(&invocation.method.owner)
                && matches!(invocation.method.name.as_str(), "addListener" | "register"))
                || (invocation.method.owner == "net/fabricmc/fabric/api/event/Event"
                    && invocation.method.name == "register"))
                && invocation.args.iter().any(|value| {
                    matches!(value, Value::Ref(Some(marker)) if marker.starts_with("@indy:"))
                })
        })
    }

    fn listener_receives_cancelled(listener: &HostInvocation) -> bool {
        if listener.method.owner == "net/fabricmc/fabric/api/event/Event" {
            return true;
        }
        listener
            .args
            .iter()
            .any(|value| matches!(value, Value::Int(1)))
    }

    pub fn dispatch_event(&self, event: Value) -> Vec<EventDispatch> {
        let cancelled = event_is_cancelled(&event);
        self.listener_registrations()
            .filter(|listener| !cancelled || Self::listener_receives_cancelled(listener))
            .cloned()
            .map(|listener| EventDispatch {
                listener,
                event: event.clone(),
            })
            .collect()
    }

    fn post_event(&mut self, event: Value) -> bool {
        let cancelled = event_is_cancelled(&event);
        self.posted_events.push(event);
        cancelled
    }

    fn record(&mut self, r: &MethodRef, receiver: Option<&Value>, args: &[Value]) {
        self.calls.push(r.clone());
        self.invocations.push(HostInvocation {
            method: r.clone(),
            receiver: receiver.cloned(),
            args: args.to_vec(),
        });
    }

    fn argument<'a>(
        r: &MethodRef,
        args: &'a [Value],
        index: usize,
    ) -> Result<&'a Value, NativeError> {
        args.get(index).ok_or_else(|| {
            NativeError::Metadata(format!(
                "{}#{}{} missing argument {index}",
                r.owner, r.name, r.descriptor
            ))
        })
    }
}

impl Host for StdHost {
    fn get_static(&mut self, r: &MethodRef) -> Result<Value, NativeError> {
        self.record(r, None, &[]);
        match (r.owner.as_str(), r.name.as_str(), r.descriptor.as_str()) {
            ("java/lang/Boolean", "TRUE", "Ljava/lang/Boolean;") => {
                Ok(Value::Ref(Some("true".into())))
            }
            ("java/lang/Boolean", "FALSE", "Ljava/lang/Boolean;") => {
                Ok(Value::Ref(Some("false".into())))
            }
            ("java/util/Locale", "US", "Ljava/util/Locale;") => {
                Ok(Value::Ref(Some("locale/US".into())))
            }
            (
                "net/fabricmc/loader/api/FabricLoader",
                "getInstance",
                "()Lnet/fabricmc/loader/api/FabricLoader;",
            ) => Ok(Value::Ref(Some("fabric-loader".into()))),
            ("net/fabricmc/api/EnvType", "CLIENT", "Lnet/fabricmc/api/EnvType;") => {
                Ok(Value::Ref(Some("CLIENT".into())))
            }
            ("net/fabricmc/api/EnvType", "SERVER", "Lnet/fabricmc/api/EnvType;") => {
                Ok(Value::Ref(Some("SERVER".into())))
            }
            (
                "net/neoforged/fml/javafmlmod/FMLJavaModLoadingContext",
                "get",
                "()Lnet/neoforged/fml/javafmlmod/FMLJavaModLoadingContext;",
            ) => Ok(Value::Ref(Some(
                "net/neoforged/fml/javafmlmod/FMLJavaModLoadingContext".into(),
            ))),
            (
                "net/minecraftforge/fml/javafmlmod/FMLJavaModLoadingContext",
                "get",
                "()Lnet/minecraftforge/fml/javafmlmod/FMLJavaModLoadingContext;",
            ) => Ok(Value::Ref(Some(
                "net/neoforged/fml/javafmlmod/FMLJavaModLoadingContext".into(),
            ))),
            ("net/neoforged/fml/ModList", "get", "()Lnet/neoforged/fml/ModList;") => {
                Ok(Value::Ref(Some("neoforge-mod-list".into())))
            }
            ("net/minecraftforge/fml/ModList", "get", "()Lnet/minecraftforge/fml/ModList;") => {
                Ok(Value::Ref(Some("neoforge-mod-list".into())))
            }
            _ => Ok(Value::default_for(&r.descriptor)),
        }
    }

    fn invoke(&mut self, r: &MethodRef, args: &[Value]) -> Result<Value, NativeError> {
        self.record(r, None, args);
        match (r.owner.as_str(), r.name.as_str(), r.descriptor.as_str()) {
            // ServiceLoader is deterministic only when its provider set is
            // explicitly seeded. Never inspect the host JVM class path.
            ("java/util/ServiceLoader", "load" | "loadInstalled", descriptor)
                if descriptor.starts_with("(Ljava/lang/Class;") =>
            {
                let service = expect_string(Self::argument(r, args, 0)?).replace('.', "/");
                if self.service_providers.contains_key(&service) {
                    Ok(Value::Ref(Some(format!("service-loader/{service}"))))
                } else {
                    Err(NativeError::Metadata(format!(
                        "java/util/ServiceLoader has no explicitly seeded providers for {service}; host classpath scanning is disabled"
                    )))
                }
            }
            ("java/text/NumberFormat", "getIntegerInstance", "()Ljava/text/NumberFormat;") => {
                Ok(Value::Ref(Some("number-format/integer/US".into())))
            }
            (
                "java/text/NumberFormat",
                "getIntegerInstance",
                "(Ljava/util/Locale;)Ljava/text/NumberFormat;",
            ) => {
                let locale = Self::argument(r, args, 0)?;
                if !matches!(locale, Value::Ref(Some(marker)) if marker == "locale/US") {
                    return Err(NativeError::Metadata(
                        "java/text/NumberFormat.getIntegerInstance supports only Locale.US".into(),
                    ));
                }
                Ok(Value::Ref(Some("number-format/integer/US".into())))
            }
            (owner, "get", descriptor)
                if is_capability_manager_owner(owner) && descriptor.ends_with("Capability;") =>
            {
                Ok(Value::Ref(Some(format!(
                    "capability/{}",
                    args.first()
                        .map(value_marker)
                        .unwrap_or_else(|| "token/null".into())
                ))))
            }
            (owner, "empty", descriptor)
                if is_lazy_optional_owner(owner) && descriptor.ends_with("LazyOptional;") =>
            {
                Ok(self.lazy_optional(None))
            }
            (owner, "ofNullable", descriptor)
                if is_lazy_optional_owner(owner) && descriptor.ends_with("LazyOptional;") =>
            {
                Ok(self.lazy_optional(match Self::argument(r, args, 0)? {
                    Value::Ref(None) => None,
                    value => Some(value.clone()),
                }))
            }
            (owner, "of", descriptor)
                if is_lazy_optional_owner(owner) && descriptor.ends_with("LazyOptional;") =>
            {
                // A Supplier callback belongs to the loaded jar and is
                // executed by NativeJar when a callback dispatch is
                // available. Keep this optional empty until then rather
                // than treating the callback marker as a game value.
                Ok(self.lazy_optional(None))
            }
            (
                owner @ ("net/neoforged/neoforge/registries/DeferredRegister"
                | "net/minecraftforge/registries/DeferredRegister"),
                "create",
                descriptor,
            ) if descriptor.ends_with(&format!("L{owner};")) => {
                let registry = args
                    .first()
                    .and_then(expect_ref)
                    .map_or("registry", String::as_str);
                let namespace = args
                    .get(1)
                    .and_then(expect_ref)
                    .map_or("default", String::as_str);
                Ok(Value::Ref(Some(format!(
                    "deferred-register/{registry}/{namespace}"
                ))))
            }
            ("net/fabricmc/fabric/api/event/EventFactory", "createArrayBacked", descriptor)
                if descriptor.ends_with("Lnet/fabricmc/fabric/api/event/Event;") =>
            {
                let marker = format!("fabric-event/{}", self.next_fabric_event);
                self.next_fabric_event = self.next_fabric_event.saturating_add(1);
                Ok(Value::Ref(Some(marker)))
            }
            ("java/lang/Integer", "parseInt", "(Ljava/lang/String;I)I") => {
                let text = expect_string(Self::argument(r, args, 0)?);
                let radix = expect_int(r, Self::argument(r, args, 1)?)?;
                let radix = u32::try_from(radix).map_err(|_| {
                    NativeError::Metadata(
                        "java/lang/Integer.parseInt received an invalid radix".into(),
                    )
                })?;
                let value = i32::from_str_radix(text, radix).map_err(|_| {
                    NativeError::Metadata(format!(
                        "invalid integer argument for {}#{}",
                        r.owner, r.name
                    ))
                })?;
                Ok(Value::Int(value))
            }
            ("java/lang/Integer", "parseInt", "(Ljava/lang/String;)I") => {
                let value = match Self::argument(r, args, 0)? {
                    Value::Ref(Some(value)) => value.parse::<i32>().map_err(|_| {
                        NativeError::Metadata(format!(
                            "invalid integer argument for {}#{}",
                            r.owner, r.name
                        ))
                    })?,
                    _ => {
                        return Err(NativeError::Metadata(format!(
                            "{}#{} expects a string",
                            r.owner, r.name
                        )));
                    }
                };
                Ok(Value::Int(value))
            }
            ("java/lang/Byte", "parseByte", "(Ljava/lang/String;)B") => Ok(Value::Int(i32::from(
                parse_java_byte(Self::argument(r, args, 0)?).map_err(|_| {
                    NativeError::Metadata(
                        "invalid byte argument for java/lang/Byte.parseByte".into(),
                    )
                })?,
            ))),
            ("java/lang/Byte", "valueOf", "(Ljava/lang/String;)Ljava/lang/Byte;") => {
                let value = parse_java_byte(Self::argument(r, args, 0)?).map_err(|_| {
                    NativeError::Metadata("invalid byte argument for java/lang/Byte.valueOf".into())
                })?;
                Ok(Value::Ref(Some(format!("byte:{value}"))))
            }
            ("java/lang/Short", "parseShort", "(Ljava/lang/String;)S") => Ok(Value::Int(
                i32::from(parse_java_short(Self::argument(r, args, 0)?)?),
            )),
            ("java/lang/Short", "parseShort", "(Ljava/lang/String;I)S") => {
                let text = expect_string(Self::argument(r, args, 0)?);
                let radix = short_radix(r, Self::argument(r, args, 1)?)?;
                let value = i16::from_str_radix(text, radix).map_err(|_| {
                    NativeError::Metadata(
                        "invalid short argument for java/lang/Short.parseShort".into(),
                    )
                })?;
                Ok(Value::Int(i32::from(value)))
            }
            ("java/lang/Short", "valueOf", "(S)Ljava/lang/Short;") => {
                let value = expect_int(r, Self::argument(r, args, 0)?)?;
                let value = i16::try_from(value).map_err(|_| {
                    NativeError::Metadata(
                        "invalid short argument for java/lang/Short.valueOf".into(),
                    )
                })?;
                Ok(Value::Ref(Some(format!("short:{value}"))))
            }
            ("java/lang/Short", "valueOf", "(Ljava/lang/String;)Ljava/lang/Short;") => {
                let value = parse_java_short(Self::argument(r, args, 0)?)?;
                Ok(Value::Ref(Some(format!("short:{value}"))))
            }
            ("java/lang/Short", "valueOf", "(Ljava/lang/String;I)Ljava/lang/Short;") => {
                let text = expect_string(Self::argument(r, args, 0)?);
                let radix = short_radix(r, Self::argument(r, args, 1)?)?;
                let value = i16::from_str_radix(text, radix).map_err(|_| {
                    NativeError::Metadata(
                        "invalid short argument for java/lang/Short.valueOf".into(),
                    )
                })?;
                Ok(Value::Ref(Some(format!("short:{value}"))))
            }
            ("java/lang/Integer", "valueOf", "(I)Ljava/lang/Integer;") => {
                let value = expect_int(r, Self::argument(r, args, 0)?)?;
                Ok(Value::Ref(Some(format!("integer:{value}"))))
            }
            ("java/lang/Integer", "valueOf", "(Ljava/lang/String;)Ljava/lang/Integer;") => {
                let value = expect_string(Self::argument(r, args, 0)?)
                    .parse::<i32>()
                    .map_err(|_| {
                        NativeError::Metadata(
                            "invalid integer argument for java/lang/Integer.valueOf".into(),
                        )
                    })?;
                Ok(Value::Ref(Some(format!("integer:{value}"))))
            }
            ("java/lang/Integer", "valueOf", "(Ljava/lang/String;I)Ljava/lang/Integer;") => {
                let text = expect_string(Self::argument(r, args, 0)?);
                let radix = expect_int(r, Self::argument(r, args, 1)?)?;
                let radix = u32::try_from(radix).map_err(|_| {
                    NativeError::Metadata(
                        "java/lang/Integer.valueOf received an invalid radix".into(),
                    )
                })?;
                let value = i32::from_str_radix(text, radix).map_err(|_| {
                    NativeError::Metadata(
                        "invalid integer argument for java/lang/Integer.valueOf".into(),
                    )
                })?;
                Ok(Value::Ref(Some(format!("integer:{value}"))))
            }
            ("java/lang/Integer", "compare", "(II)I") => {
                let left = expect_int(r, Self::argument(r, args, 0)?)?;
                let right = expect_int(r, Self::argument(r, args, 1)?)?;
                Ok(Value::Int(match left.cmp(&right) {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                }))
            }
            ("java/lang/Integer", "toHexString", "(I)Ljava/lang/String;") => Ok(Value::Ref(Some(
                format!("{:x}", expect_int(r, Self::argument(r, args, 0)?)? as u32),
            ))),
            ("java/lang/Character", "isLetter", "(C)Z")
            | ("java/lang/Character", "isLetter", "(I)Z") => Ok(Value::Int(
                java_code_point(Self::argument(r, args, 0)?).is_some_and(char::is_alphabetic)
                    as i32,
            )),
            ("java/lang/Character", "isDigit", "(C)Z")
            | ("java/lang/Character", "isDigit", "(I)Z") => Ok(Value::Int(
                java_code_point(Self::argument(r, args, 0)?).is_some_and(char::is_numeric) as i32,
            )),
            ("java/lang/Character", "toLowerCase", "(C)C")
            | ("java/lang/Character", "toLowerCase", "(I)I") => {
                let value = java_code_point(Self::argument(r, args, 0)?)
                    .and_then(|c| c.to_lowercase().next());
                Ok(Value::Int(value.map_or(0xfffd, u32::from) as i32))
            }
            ("java/lang/Character", "valueOf", "(C)Ljava/lang/Character;") => {
                let value = expect_int(r, Self::argument(r, args, 0)?)?;
                Ok(Value::Ref(Some(format!("character:{value}"))))
            }
            ("java/lang/Double", "parseDouble", "(Ljava/lang/String;)D") => {
                Ok(double_value(parse_java_float(Self::argument(r, args, 0)?)?))
            }
            ("java/lang/Double", "valueOf", "(Ljava/lang/String;)Ljava/lang/Double;") => {
                Ok(double_value(parse_java_float(Self::argument(r, args, 0)?)?))
            }
            ("java/lang/Double", "isNaN", "(D)Z") | ("java/lang/Double", "isInfinite", "(D)Z") => {
                let value = expect_float(r, Self::argument(r, args, 0)?)?;
                let result = if r.name == "isNaN" {
                    value.is_nan()
                } else {
                    value.is_infinite()
                };
                Ok(Value::Int(result as i32))
            }
            ("java/lang/Float", "parseFloat", "(Ljava/lang/String;)F") => {
                Ok(float_value(parse_java_float(Self::argument(r, args, 0)?)?))
            }
            ("java/lang/Float", "valueOf", "(Ljava/lang/String;)Ljava/lang/Float;") => {
                Ok(float_value(parse_java_float(Self::argument(r, args, 0)?)?))
            }
            ("java/lang/Float", "isNaN", "(F)Z") | ("java/lang/Float", "isInfinite", "(F)Z") => {
                let value = expect_float(r, Self::argument(r, args, 0)?)?;
                let result = if r.name == "isNaN" {
                    value.is_nan()
                } else {
                    value.is_infinite()
                };
                Ok(Value::Int(result as i32))
            }
            ("java/lang/Long", "compare", "(JJ)I") => {
                let left = expect_long(r, Self::argument(r, args, 0)?)?;
                let right = expect_long(r, Self::argument(r, args, 1)?)?;
                Ok(Value::Int(match left.cmp(&right) {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                }))
            }
            ("java/lang/Long", "toHexString", "(J)Ljava/lang/String;") => Ok(Value::Ref(Some(
                format!("{:x}", expect_long(r, Self::argument(r, args, 0)?)? as u64),
            ))),
            ("java/lang/Long", "parseLong", "(Ljava/lang/String;)J") => {
                Ok(long_value(parse_java_long(Self::argument(r, args, 0)?)?))
            }
            ("java/lang/Long", "parseLong", "(Ljava/lang/String;I)J") => {
                let text = expect_string(Self::argument(r, args, 0)?);
                let radix = expect_int(r, Self::argument(r, args, 1)?)?;
                let radix = u32::try_from(radix).map_err(|_| {
                    NativeError::Metadata(
                        "java/lang/Long.parseLong received an invalid radix".into(),
                    )
                })?;
                let value = i64::from_str_radix(text, radix).map_err(|_| {
                    NativeError::Metadata(
                        "invalid long argument for java/lang/Long.parseLong".into(),
                    )
                })?;
                Ok(long_value(value))
            }
            ("java/lang/Long", "valueOf", "(Ljava/lang/String;)Ljava/lang/Long;") => {
                Ok(long_value(parse_java_long(Self::argument(r, args, 0)?)?))
            }
            ("java/lang/Long", "toString", "(J)Ljava/lang/String;") => Ok(Value::Ref(Some(
                expect_long(r, Self::argument(r, args, 0)?)?.to_string(),
            ))),
            ("java/lang/Boolean", "parseBoolean", "(Ljava/lang/String;)Z") => {
                let value = match Self::argument(r, args, 0)? {
                    Value::Ref(Some(value)) => value.eq_ignore_ascii_case("true"),
                    _ => false,
                };
                Ok(Value::Int(value as i32))
            }
            ("java/lang/Boolean", "valueOf", "(Z)Ljava/lang/Boolean;") => {
                let value = expect_int(r, Self::argument(r, args, 0)?)? != 0;
                Ok(Value::Ref(Some(format!("boolean:{value}"))))
            }
            ("java/lang/Boolean", "valueOf", "(Ljava/lang/String;)Ljava/lang/Boolean;") => {
                let value = expect_string(Self::argument(r, args, 0)?).eq_ignore_ascii_case("true");
                Ok(Value::Ref(Some(format!("boolean:{value}"))))
            }
            (
                "java/lang/Enum",
                "valueOf",
                "(Ljava/lang/Class;Ljava/lang/String;)Ljava/lang/Enum;",
            ) => {
                let class = class_binary_name(Self::argument(r, args, 0)?).replace('.', "/");
                let name = expect_string(Self::argument(r, args, 1)?);
                if name.is_empty() {
                    return Err(NativeError::Metadata(
                        "java/lang/Enum.valueOf received an empty name".into(),
                    ));
                }
                Ok(Value::Ref(Some(format!("enum/{class}/{name}"))))
            }
            // UUIDs are common in loader metadata, player identity, and
            // registration diagnostics. Keep them as canonical markers so
            // equality/string formatting is deterministic without a JVM.
            ("java/util/UUID", "randomUUID", "()Ljava/util/UUID;") => {
                Ok(uuid_value("00000000-0000-4000-8000-000000000000")?)
            }
            ("java/util/UUID", "fromString", "(Ljava/lang/String;)Ljava/util/UUID;") => {
                Ok(uuid_value(expect_string(Self::argument(r, args, 0)?))?)
            }
            // Class.forName is intentionally not exposed: loading arbitrary
            // classes from disk would escape the bounded native sandbox. The
            // class-literal path (`Foo.class`) still provides the metadata
            // queries bootstrap code commonly needs.
            ("java/util/Optional", "empty", "()Ljava/util/Optional;") => Ok(Value::Ref(None)),
            ("java/util/Optional", "of", "(Ljava/lang/Object;)Ljava/util/Optional;") => {
                let value = Self::argument(r, args, 0)?;
                if matches!(value, Value::Ref(None)) {
                    return Err(NativeError::Metadata(
                        "java/util/Optional.of received null".into(),
                    ));
                }
                Ok(optional_wrap(value.clone()))
            }
            ("java/util/Optional", "ofNullable", "(Ljava/lang/Object;)Ljava/util/Optional;") => {
                Ok(match Self::argument(r, args, 0)? {
                    Value::Ref(None) => Value::Ref(None),
                    value => optional_wrap(value.clone()),
                })
            }
            ("java/util/OptionalInt", "empty", "()Ljava/util/OptionalInt;") => Ok(Value::Ref(None)),
            ("java/util/OptionalInt", "of", "(I)Ljava/util/OptionalInt;") => {
                Ok(Value::Ref(Some(format!(
                    "optional-int/{}",
                    expect_int(r, Self::argument(r, args, 0)?)?
                ))))
            }
            ("java/util/OptionalLong", "empty", "()Ljava/util/OptionalLong;") => {
                Ok(Value::Ref(None))
            }
            ("java/util/OptionalLong", "of", "(J)Ljava/util/OptionalLong;") => {
                Ok(Value::Ref(Some(format!(
                    "optional-long/{}",
                    expect_long(r, Self::argument(r, args, 0)?)?
                ))))
            }
            ("java/util/OptionalDouble", "empty", "()Ljava/util/OptionalDouble;") => {
                Ok(Value::Ref(None))
            }
            ("java/util/OptionalDouble", "of", "(D)Ljava/util/OptionalDouble;") => {
                Ok(Value::Ref(Some(format!(
                    "optional-double/{}",
                    expect_float(r, Self::argument(r, args, 0)?)?
                ))))
            }
            // Keep Arrays.asList bounded when the interpreter passes an
            // opaque array marker. The marker is retained as the list's
            // single value; expanding runtime-owned array storage here would
            // cross the host boundary and defeat invocation isolation.
            ("java/util/Arrays", "asList", "([Ljava/lang/Object;)Ljava/util/List;") => {
                Ok(collection_value("list", args))
            }
            ("java/util/stream/Collectors", "toList", "()Ljava/util/stream/Collector;") => {
                Ok(Value::Ref(Some("collector/list".into())))
            }
            ("java/util/stream/Collectors", "toSet", "()Ljava/util/stream/Collector;") => {
                Ok(Value::Ref(Some("collector/set".into())))
            }
            ("java/util/List", "of", descriptor) if descriptor.ends_with("Ljava/util/List;") => {
                Ok(collection_value("list", args))
            }
            ("java/util/Set", "of", descriptor) if descriptor.ends_with("Ljava/util/Set;") => {
                Ok(collection_value("set", args))
            }
            ("java/util/Map", "of", descriptor) if descriptor.ends_with("Ljava/util/Map;") => {
                Ok(collection_value("map", args))
            }
            ("java/util/Collections", "emptyList", "()Ljava/util/List;") => {
                Ok(collection_empty("list"))
            }
            ("java/util/Collections", "emptySet", "()Ljava/util/Set;") => {
                Ok(collection_empty("set"))
            }
            ("java/util/Collections", "emptyMap", "()Ljava/util/Map;") => {
                Ok(collection_empty("map"))
            }
            ("java/util/Collections", "singletonList", "(Ljava/lang/Object;)Ljava/util/List;") => {
                Ok(collection_value("list", args))
            }
            (
                "java/util/Collections",
                "singletonMap",
                "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/util/Map;",
            ) => Ok(collection_value("map", args)),
            ("java/util/Collections", "unmodifiableList", "(Ljava/util/List;)Ljava/util/List;")
            | ("java/util/Collections", "unmodifiableSet", "(Ljava/util/Set;)Ljava/util/Set;")
            | ("java/util/Collections", "unmodifiableMap", "(Ljava/util/Map;)Ljava/util/Map;") => {
                Ok(Self::argument(r, args, 0)?.clone())
            }
            ("java/util/Objects", "equals", "(Ljava/lang/Object;Ljava/lang/Object;)Z") => Ok(
                Value::Int((Self::argument(r, args, 0)? == Self::argument(r, args, 1)?) as i32),
            ),
            ("java/util/Objects", "hashCode", "(Ljava/lang/Object;)I") => {
                let value = Self::argument(r, args, 0)?;
                Ok(Value::Int(match value {
                    Value::Ref(None) => 0,
                    _ => stable_hash(&value_text(value)),
                }))
            }
            (
                "java/util/Objects",
                "requireNonNullElse",
                "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
            ) => {
                let value = Self::argument(r, args, 0)?;
                if !matches!(value, Value::Ref(None)) {
                    return Ok(value.clone());
                }
                let fallback = Self::argument(r, args, 1)?;
                if matches!(fallback, Value::Ref(None)) {
                    return Err(NativeError::Metadata(
                        "java/util/Objects.requireNonNullElse received null fallback".into(),
                    ));
                }
                Ok(fallback.clone())
            }
            ("java/util/Objects", "requireNonNull", descriptor)
                if descriptor.starts_with("(Ljava/lang/Object;")
                    && descriptor.ends_with(")Ljava/lang/Object;") =>
            {
                let value = Self::argument(r, args, 0)?;
                if matches!(value, Value::Ref(None)) {
                    return Err(NativeError::Metadata(
                        "java/util/Objects.requireNonNull received null".into(),
                    ));
                }
                Ok(value.clone())
            }
            ("java/util/Objects", "toString", "(Ljava/lang/Object;)Ljava/lang/String;") => {
                Ok(Value::Ref(Some(value_text(Self::argument(r, args, 0)?))))
            }
            ("java/lang/Math", "abs", "(I)I") => {
                let value = expect_int(r, Self::argument(r, args, 0)?)?;
                value
                    .checked_abs()
                    .map(Value::Int)
                    .ok_or_else(|| NativeError::Arithmetic {
                        class: r.owner.clone(),
                        method: r.name.clone(),
                    })
            }
            ("java/lang/Math", "max", "(II)I") => Ok(Value::Int(
                expect_int(r, Self::argument(r, args, 0)?)?
                    .max(expect_int(r, Self::argument(r, args, 1)?)?),
            )),
            ("java/lang/Math", "min", "(II)I") => Ok(Value::Int(
                expect_int(r, Self::argument(r, args, 0)?)?
                    .min(expect_int(r, Self::argument(r, args, 1)?)?),
            )),
            ("java/lang/Math", "floorDiv", "(II)I") => {
                let left = expect_int(r, Self::argument(r, args, 0)?)?;
                let right = expect_int(r, Self::argument(r, args, 1)?)?;
                if right == 0 {
                    return Err(NativeError::Arithmetic {
                        class: r.owner.clone(),
                        method: r.name.clone(),
                    });
                }
                let quotient = i64::from(left) / i64::from(right);
                let remainder = i64::from(left) % i64::from(right);
                let quotient = if remainder != 0 && (left < 0) != (right < 0) {
                    quotient - 1
                } else {
                    quotient
                };
                Ok(Value::Int(quotient as i32))
            }
            ("java/lang/Math", "floorMod", "(II)I") => {
                let left = expect_int(r, Self::argument(r, args, 0)?)?;
                let right = expect_int(r, Self::argument(r, args, 1)?)?;
                if right == 0 {
                    return Err(NativeError::Arithmetic {
                        class: r.owner.clone(),
                        method: r.name.clone(),
                    });
                }
                // Java computes `a - floorDiv(a, b) * b` in i32 arithmetic, so
                // a negative divisor produces a negative remainder and the
                // MIN_VALUE/-1 overflow wraps to zero.
                let quotient = i64::from(left) / i64::from(right);
                let remainder = i64::from(left) % i64::from(right);
                let quotient = if remainder != 0 && (left < 0) != (right < 0) {
                    quotient - 1
                } else {
                    quotient
                };
                Ok(Value::Int(
                    (i64::from(left) - quotient * i64::from(right)) as i32,
                ))
            }
            ("java/lang/Math", "addExact", "(II)I") => {
                let left = expect_int(r, Self::argument(r, args, 0)?)?;
                let right = expect_int(r, Self::argument(r, args, 1)?)?;
                left.checked_add(right)
                    .map(Value::Int)
                    .ok_or_else(|| NativeError::Arithmetic {
                        class: r.owner.clone(),
                        method: r.name.clone(),
                    })
            }
            ("java/lang/String", "valueOf", "(Ljava/lang/Object;)Ljava/lang/String;") => {
                Ok(Value::Ref(Some(value_text(Self::argument(r, args, 0)?))))
            }
            // Chat components are a common loader/bootstrap value (mod
            // names, registration diagnostics, and event messages). Keep the
            // text-bearing subset deterministic so native entrypoints can
            // construct and inspect a component without a JVM façade.
            ("net/minecraft/network/chat/Component", name, descriptor)
                if matches!(
                    name,
                    "literal" | "translatable" | "translatableEscape" | "translatableWithFallback"
                ) && descriptor.ends_with("Lnet/minecraft/network/chat/MutableComponent;") =>
            {
                let text = expect_string(Self::argument(r, args, 0)?);
                let kind = if name == "literal" {
                    "literal"
                } else {
                    "translatable"
                };
                Ok(component_value(kind, text))
            }
            ("java/lang/String", "valueOf", "(I)Ljava/lang/String;")
            | ("java/lang/Integer", "toString", "(I)Ljava/lang/String;") => Ok(Value::Ref(Some(
                expect_int(r, Self::argument(r, args, 0)?)?.to_string(),
            ))),
            ("java/lang/Short", "toString", "(S)Ljava/lang/String;") => Ok(Value::Ref(Some(
                i16::try_from(expect_int(r, Self::argument(r, args, 0)?)?)
                    .map_err(|_| {
                        NativeError::Metadata(
                            "invalid short argument for java/lang/Short.toString".into(),
                        )
                    })?
                    .to_string(),
            ))),
            ("java/lang/String", "valueOf", "(Z)Ljava/lang/String;")
            | ("java/lang/Boolean", "toString", "(Z)Ljava/lang/String;") => Ok(Value::Ref(Some(
                match expect_int(r, Self::argument(r, args, 0)?)? {
                    0 => "false".into(),
                    _ => "true".into(),
                },
            ))),
            ("java/lang/String", "valueOf", "(C)Ljava/lang/String;") => Ok(Value::Ref(Some(
                char::from_u32(expect_int(r, Self::argument(r, args, 0)?)? as u32)
                    .unwrap_or('\u{fffd}')
                    .to_string(),
            ))),
            ("java/lang/Double", "toString", "(D)Ljava/lang/String;")
            | ("java/lang/Float", "toString", "(F)Ljava/lang/String;")
            | ("java/lang/String", "valueOf", "(D)Ljava/lang/String;")
            | ("java/lang/String", "valueOf", "(F)Ljava/lang/String;") => Ok(Value::Ref(Some(
                format_java_float(expect_float(r, Self::argument(r, args, 0)?)?),
            ))),
            ("java/nio/file/Paths", "get", "(Ljava/lang/String;)Ljava/nio/file/Path;") => {
                Ok(path_value(expect_string(Self::argument(r, args, 0)?)))
            }
            (
                "net/minecraft/resources/Identifier" | "net/minecraft/resources/ResourceLocation",
                name,
                descriptor,
            ) if descriptor.ends_with("Lnet/minecraft/resources/Identifier;")
                || descriptor.ends_with("Lnet/minecraft/resources/ResourceLocation;") =>
            {
                let value = match name {
                    "fromNamespaceAndPath" | "tryBuild" => {
                        let namespace = args.first().map(expect_string).unwrap_or("minecraft");
                        let path = args.get(1).map(expect_string).unwrap_or_default();
                        identifier_value(namespace, path)
                    }
                    "withDefaultNamespace" => identifier_value(
                        "minecraft",
                        args.first().map(expect_string).unwrap_or_default(),
                    ),
                    "parse" | "tryParse" => {
                        identifier_parse(args.first().map(expect_string).unwrap_or_default())
                    }
                    _ => Value::Ref(None),
                };
                Ok(value)
            }
            ("net/minecraft/resources/ResourceKey", "create", descriptor)
                if descriptor.ends_with("Lnet/minecraft/resources/ResourceKey;") =>
            {
                Ok(Value::Ref(Some(format!(
                    "resource-key/{}/{}",
                    args.first()
                        .map(value_marker)
                        .unwrap_or_else(|| "null".into()),
                    args.get(1)
                        .map(value_marker)
                        .unwrap_or_else(|| "null".into())
                ))))
            }
            ("net/minecraft/tags/TagKey", "create", descriptor)
                if descriptor.ends_with("Lnet/minecraft/tags/TagKey;") =>
            {
                Ok(tag_key_value(
                    args.first().map(expect_string).unwrap_or_default(),
                    args.get(1).map(expect_string).unwrap_or_default(),
                ))
            }
            ("java/lang/System", "getProperty", "(Ljava/lang/String;)Ljava/lang/String;") => {
                let key = match Self::argument(r, args, 0)? {
                    Value::Ref(Some(key)) => key,
                    _ => return Ok(Value::Ref(None)),
                };
                Ok(Value::Ref(self.properties.get(key).cloned()))
            }
            (
                "java/lang/System",
                "getProperty",
                "(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;",
            ) => {
                let key = match Self::argument(r, args, 0)? {
                    Value::Ref(Some(key)) => key,
                    _ => return Ok(Value::Ref(None)),
                };
                let default = match Self::argument(r, args, 1)? {
                    Value::Ref(value) => value.clone(),
                    _ => None,
                };
                Ok(Value::Ref(self.properties.get(key).cloned().or(default)))
            }
            ("java/lang/System", "getProperties", "()Ljava/util/Properties;") => {
                let values = self
                    .properties
                    .iter()
                    .flat_map(|(key, value)| {
                        [
                            Value::Ref(Some(key.clone())),
                            Value::Ref(Some(value.clone())),
                        ]
                    })
                    .collect::<Vec<_>>();
                Ok(collection_value("map", &values))
            }
            ("java/lang/System", "getenv", "(Ljava/lang/String;)Ljava/lang/String;") => {
                let key = match Self::argument(r, args, 0)? {
                    Value::Ref(Some(key)) => key,
                    _ => return Ok(Value::Ref(None)),
                };
                Ok(Value::Ref(self.environment.get(key).cloned()))
            }
            ("java/lang/System", "getenv", "()Ljava/util/Map;") => {
                let values = self
                    .environment
                    .iter()
                    .flat_map(|(key, value)| {
                        [
                            Value::Ref(Some(key.clone())),
                            Value::Ref(Some(value.clone())),
                        ]
                    })
                    .collect::<Vec<_>>();
                Ok(collection_value("map", &values))
            }
            ("java/lang/System", "identityHashCode", "(Ljava/lang/Object;)I") => {
                let value = Self::argument(r, args, 0)?;
                Ok(Value::Int(match value {
                    Value::Ref(None) => 0,
                    _ => stable_hash(&value_text(value)),
                }))
            }
            ("java/lang/System", "currentTimeMillis", "()J") => {
                Ok(Value::Int(self.current_time_millis))
            }
            ("java/lang/System", "lineSeparator", "()Ljava/lang/String;") => {
                Ok(Value::Ref(Some("\n".into())))
            }
            (
                "java/lang/ClassLoader",
                "getSystemResource",
                "(Ljava/lang/String;)Ljava/net/URL;",
            ) => {
                let name = expect_string(Self::argument(r, args, 0)?);
                Ok(Value::Ref(self.resource(name).map(str::to_owned)))
            }
            (
                "java/lang/ClassLoader",
                "getSystemResourceAsStream",
                "(Ljava/lang/String;)Ljava/io/InputStream;",
            ) => {
                let name = expect_string(Self::argument(r, args, 0)?);
                Ok(self.resource_stream(name))
            }
            (
                "java/lang/ClassLoader",
                "getResources" | "getSystemResources",
                "(Ljava/lang/String;)Ljava/util/Enumeration;",
            ) => {
                let name = expect_string(Self::argument(r, args, 0)?);
                Ok(self.resource_enumeration(name))
            }
            ("java/lang/Thread", "currentThread", "()Ljava/lang/Thread;") => {
                Ok(Value::Ref(Some("thread/main".into())))
            }
            (
                "net/fabricmc/loader/api/FabricLoader",
                "getInstance",
                "()Lnet/fabricmc/loader/api/FabricLoader;",
            ) => Ok(Value::Ref(Some("fabric-loader".into()))),
            _ => Ok(Value::default_for(&r.descriptor)),
        }
    }

    fn invoke_instance(
        &mut self,
        r: &MethodRef,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Value, NativeError> {
        self.record(r, Some(receiver), args);
        if is_throwable_owner(&r.owner) {
            let marker = expect_ref(receiver)
                .ok_or_else(|| NativeError::Metadata("malformed Throwable marker".into()))?;
            if r.name == "<init>" {
                let message = args.first().and_then(|value| match value {
                    Value::Ref(Some(text)) => Some(text.clone()),
                    Value::Ref(None) => None,
                    _ => None,
                });
                let cause = args.iter().find_map(|value| match value {
                    Value::Ref(Some(value)) if is_throwable_marker(value) => {
                        Some(Value::Ref(Some(value.clone())))
                    }
                    _ => None,
                });
                self.throwable_messages.insert(marker.clone(), message);
                self.throwable_causes.insert(marker.clone(), cause);
                return Ok(Value::Void);
            }
            match (r.name.as_str(), r.descriptor.as_str()) {
                ("getMessage", "()Ljava/lang/String;") => {
                    return Ok(self
                        .throwable_messages
                        .get(marker)
                        .and_then(|message| message.clone())
                        .map_or(Value::Ref(None), |message| Value::Ref(Some(message))));
                }
                ("getCause", "()Ljava/lang/Throwable;") => {
                    return Ok(self
                        .throwable_causes
                        .get(marker)
                        .cloned()
                        .flatten()
                        .unwrap_or(Value::Ref(None)));
                }
                ("initCause", "(Ljava/lang/Throwable;)Ljava/lang/Throwable;") => {
                    let cause = args.first().cloned().unwrap_or(Value::Ref(None));
                    self.throwable_causes.insert(marker.clone(), Some(cause));
                    return Ok(receiver.clone());
                }
                ("toString", "()Ljava/lang/String;") => {
                    let class = marker
                        .split_once('#')
                        .map_or(marker.as_str(), |(class, _)| class);
                    let class = class.rsplit('/').next().unwrap_or(class);
                    let Some(Some(message)) = self.throwable_messages.get(marker) else {
                        return Ok(Value::Ref(Some(class.to_owned())));
                    };
                    return Ok(Value::Ref(Some(format!("{class}: {message}"))));
                }
                _ => {}
            }
        }
        if r.owner == "java/lang/Thread" {
            let marker = expect_ref(receiver)
                .ok_or_else(|| NativeError::Metadata("malformed java/lang/Thread marker".into()))?;
            if marker != "thread/main" {
                return Err(NativeError::Metadata(
                    "unsupported java/lang/Thread marker".into(),
                ));
            }
            return match (r.name.as_str(), r.descriptor.as_str()) {
                ("getName", "()Ljava/lang/String;") => Ok(Value::Ref(Some("main".into()))),
                ("isDaemon", "()Z") => Ok(Value::Int(0)),
                ("getContextClassLoader", "()Ljava/lang/ClassLoader;") => {
                    Ok(Value::Ref(Some("classloader/bootstrap".into())))
                }
                _ => Err(NativeError::MissingMethod(format!(
                    "java/lang/Thread#{}{}",
                    r.name, r.descriptor
                ))),
            };
        }
        if r.owner == "java/lang/ClassLoader" {
            let marker = expect_ref(receiver).ok_or_else(|| {
                NativeError::Metadata("malformed java/lang/ClassLoader marker".into())
            })?;
            if marker != "classloader/bootstrap" {
                return Err(NativeError::Metadata(
                    "unsupported java/lang/ClassLoader marker".into(),
                ));
            }
            if r.name == "loadClass"
                && matches!(
                    r.descriptor.as_str(),
                    "(Ljava/lang/String;)Ljava/lang/Class;"
                        | "(Ljava/lang/String;Z)Ljava/lang/Class;"
                )
            {
                let name = expect_string(Self::argument(r, args, 0)?);
                return self
                    .class_marker(name)
                    .map(|marker| Value::Ref(Some(marker.to_owned())))
                    .ok_or_else(|| {
                        NativeError::Metadata(format!(
                            "java/lang/ClassLoader.loadClass {name} was not explicitly seeded; host classpath scanning is disabled"
                        ))
                    });
            }
            if matches!(r.name.as_str(), "getResource" | "getResourceAsStream")
                && r.descriptor == "(Ljava/lang/String;)Ljava/lang/Object;"
            {
                let name = expect_string(Self::argument(r, args, 0)?);
                return Ok(Value::Ref(self.resource(name).map(str::to_owned)));
            }
            if r.name == "getResource" && r.descriptor == "(Ljava/lang/String;)Ljava/net/URL;" {
                let name = expect_string(Self::argument(r, args, 0)?);
                return Ok(Value::Ref(self.resource(name).map(str::to_owned)));
            }
            if r.name == "getResourceAsStream"
                && r.descriptor == "(Ljava/lang/String;)Ljava/io/InputStream;"
            {
                let name = expect_string(Self::argument(r, args, 0)?);
                return Ok(self.resource_stream(name));
            }
            if r.name == "getResources"
                && r.descriptor == "(Ljava/lang/String;)Ljava/util/Enumeration;"
            {
                let name = expect_string(Self::argument(r, args, 0)?);
                return Ok(self.resource_enumeration(name));
            }
        }
        if r.owner == "java/io/InputStream" || r.owner == "java/io/ByteArrayInputStream" {
            let marker = expect_ref(receiver).ok_or_else(|| {
                NativeError::Metadata("malformed java/io/InputStream marker".into())
            })?;
            let (bytes, index) = self.resource_streams.get_mut(marker).ok_or_else(|| {
                NativeError::Metadata("unknown java/io/InputStream marker".into())
            })?;
            return match (r.name.as_str(), r.descriptor.as_str()) {
                ("read", "()I") => {
                    let value = bytes.get(*index).copied().map_or(-1, i32::from);
                    if value >= 0 {
                        *index += 1;
                    }
                    Ok(Value::Int(value))
                }
                ("available", "()I") => Ok(Value::Int(
                    bytes.len().saturating_sub(*index).min(i32::MAX as usize) as i32,
                )),
                ("markSupported", "()Z") => Ok(Value::Int(1)),
                ("mark", "(I)V") => {
                    self.resource_marks.insert(marker.to_owned(), *index);
                    Ok(Value::Void)
                }
                ("reset", "()V") => {
                    let mark = self.resource_marks.get(marker).copied().ok_or_else(|| {
                        NativeError::Metadata("java/io/InputStream has no mark".into())
                    })?;
                    *index = mark;
                    Ok(Value::Void)
                }
                ("skip", "(J)J") => {
                    let amount = expect_int(r, Self::argument(r, args, 0)?)?.max(0) as usize;
                    let skipped = amount.min(bytes.len().saturating_sub(*index));
                    *index += skipped;
                    Ok(Value::Int(skipped as i32))
                }
                ("skipNBytes", "(J)V") => {
                    let amount = expect_int(r, Self::argument(r, args, 0)?)?.max(0) as usize;
                    let remaining = bytes.len().saturating_sub(*index);
                    if amount > remaining {
                        return Err(NativeError::Metadata(
                            "java/io/InputStream skipNBytes exhausted".into(),
                        ));
                    }
                    *index += amount;
                    Ok(Value::Void)
                }
                ("close", "()V") => {
                    *index = bytes.len();
                    Ok(Value::Void)
                }
                _ => Err(NativeError::MissingMethod(format!(
                    "{}#{}{}",
                    r.owner, r.name, r.descriptor
                ))),
            };
        }
        if r.owner == "java/text/NumberFormat"
            && r.name == "format"
            && matches!(
                r.descriptor.as_str(),
                "(I)Ljava/lang/String;" | "(J)Ljava/lang/String;"
            )
        {
            let marker = expect_ref(receiver)
                .ok_or_else(|| NativeError::Metadata("malformed NumberFormat marker".into()))?;
            if marker != "number-format/integer/US" {
                return Err(NativeError::Metadata(
                    "unsupported NumberFormat marker".into(),
                ));
            }
            let value = expect_int(r, Self::argument(r, args, 0)?)?;
            return Ok(Value::Ref(Some(format_integer_us(value))));
        }
        if r.owner == "java/util/Locale" {
            let marker = expect_ref(receiver)
                .ok_or_else(|| NativeError::Metadata("malformed Locale marker".into()))?;
            if marker != "locale/US" {
                return Err(NativeError::Metadata("unsupported Locale marker".into()));
            }
            return match (r.name.as_str(), r.descriptor.as_str()) {
                ("getLanguage", "()Ljava/lang/String;") => Ok(Value::Ref(Some("en".into()))),
                ("getCountry", "()Ljava/lang/String;") => Ok(Value::Ref(Some("US".into()))),
                ("toLanguageTag", "()Ljava/lang/String;") => Ok(Value::Ref(Some("en-US".into()))),
                ("toString", "()Ljava/lang/String;") => Ok(Value::Ref(Some("en_US".into()))),
                _ => Err(NativeError::Metadata(format!(
                    "unsupported Locale.US query {}#{}{}",
                    r.owner, r.name, r.descriptor
                ))),
            };
        }
        let receiver_marker = expect_ref(receiver).cloned();
        if let Some(marker) = receiver_marker.as_deref() {
            let callback_result = match (r.owner.as_str(), r.name.as_str(), r.descriptor.as_str()) {
                (
                    "java/util/function/Predicate",
                    "and",
                    "(Ljava/util/function/Predicate;)Ljava/util/function/Predicate;",
                ) => args.first().map(|other| {
                    Value::Ref(Some(format!(
                        "@fn:predicate.and:{marker}|{}",
                        expect_ref(other).map_or("@fn:predicate.false", String::as_str)
                    )))
                }),
                (
                    "java/util/function/Predicate",
                    "or",
                    "(Ljava/util/function/Predicate;)Ljava/util/function/Predicate;",
                ) => args.first().map(|other| {
                    Value::Ref(Some(format!(
                        "@fn:predicate.or:{marker}|{}",
                        expect_ref(other).map_or("@fn:predicate.false", String::as_str)
                    )))
                }),
                ("java/util/function/Predicate", "negate", "()Ljava/util/function/Predicate;") => {
                    Some(Value::Ref(Some(format!("@fn:predicate.not:{marker}"))))
                }
                (
                    "java/util/function/Consumer",
                    "andThen",
                    "(Ljava/util/function/Consumer;)Ljava/util/function/Consumer;",
                ) => args.first().map(|other| {
                    Value::Ref(Some(format!(
                        "@fn:consumer.then:{marker}|{}",
                        expect_ref(other).map_or("@fn:consumer.empty", String::as_str)
                    )))
                }),
                (
                    "java/util/function/Function",
                    "compose",
                    "(Ljava/util/function/Function;)Ljava/util/function/Function;",
                ) => args.first().map(|other| {
                    Value::Ref(Some(format!(
                        "@fn:function.compose:{marker}|{}",
                        expect_ref(other).map_or("@fn:function.empty", String::as_str)
                    )))
                }),
                (
                    "java/util/function/Function",
                    "andThen",
                    "(Ljava/util/function/Function;)Ljava/util/function/Function;",
                ) => args.first().map(|other| {
                    Value::Ref(Some(format!(
                        "@fn:function.then:{marker}|{}",
                        expect_ref(other).map_or("@fn:function.empty", String::as_str)
                    )))
                }),
                _ => None,
            };
            if let Some(value) = callback_result {
                return Ok(value);
            }
        }
        if r.owner == "java/lang/reflect/Field"
            && matches!(
                r.name.as_str(),
                "getName" | "getType" | "getModifiers" | "toString"
            )
        {
            let marker = receiver_marker.as_deref().ok_or_else(|| {
                NativeError::Metadata("malformed java/lang/reflect/Field marker".into())
            })?;
            let method = marker.strip_prefix("reflect-field/").ok_or_else(|| {
                NativeError::Metadata("malformed java/lang/reflect/Field marker".into())
            })?;
            let (_, member) = method.split_once('#').ok_or_else(|| {
                NativeError::Metadata("reflective field marker has no name separator".into())
            })?;
            let (name, descriptor_and_access) = member.split_once(':').unwrap_or((member, ""));
            let (descriptor, access) = descriptor_and_access
                .rsplit_once(':')
                .map_or((descriptor_and_access, 0), |(descriptor, access)| {
                    (descriptor, access.parse::<u16>().unwrap_or(0))
                });
            return match (r.name.as_str(), r.descriptor.as_str()) {
                ("getName", "()Ljava/lang/String;") => Ok(Value::Ref(Some(name.to_owned()))),
                ("getType", "()Ljava/lang/Class;") => {
                    let class_name = descriptor
                        .strip_prefix('L')
                        .and_then(|value| value.strip_suffix(';'))
                        .unwrap_or(descriptor);
                    Ok(Value::Ref(Some(class_name.to_owned())))
                }
                ("getModifiers", "()I") => Ok(Value::Int(i32::from(access))),
                ("toString", "()Ljava/lang/String;") => Ok(Value::Ref(Some(marker.to_owned()))),
                _ => Err(NativeError::MissingMethod(format!(
                    "java/lang/reflect/Field#{}{}",
                    r.name, r.descriptor
                ))),
            };
        }
        if r.owner == "java/lang/reflect/Method"
            && matches!(
                r.name.as_str(),
                "getName" | "getParameterCount" | "toString"
            )
        {
            let marker = receiver_marker.as_deref().ok_or_else(|| {
                NativeError::Metadata("malformed java/lang/reflect/Method marker".into())
            })?;
            let method = marker.strip_prefix("reflect-method/").ok_or_else(|| {
                NativeError::Metadata("malformed java/lang/reflect/Method marker".into())
            })?;
            let (_, member) = method.split_once('#').ok_or_else(|| {
                NativeError::Metadata("reflective method marker has no name separator".into())
            })?;
            let (name, descriptor) = member
                .find('(')
                .map_or((member, None), |at| (&member[..at], Some(&member[at..])));
            return match (r.name.as_str(), r.descriptor.as_str()) {
                ("getName", "()Ljava/lang/String;") => Ok(Value::Ref(Some(name.to_owned()))),
                ("getParameterCount", "()I") => {
                    Ok(Value::Int(descriptor.map_or(0, arg_count) as i32))
                }
                ("toString", "()Ljava/lang/String;") => Ok(Value::Ref(Some(marker.to_owned()))),
                _ => Err(NativeError::MissingMethod(format!(
                    "java/lang/reflect/Method#{}{}",
                    r.name, r.descriptor
                ))),
            };
        }
        if is_capability_owner(&r.owner) && r.name == "isRegistered" && r.descriptor == "()Z" {
            return Ok(Value::Int(1));
        }
        if is_capability_owner(&r.owner)
            && r.name == "orEmpty"
            && r.descriptor.ends_with("LazyOptional;")
        {
            let value = Self::argument(r, args, 0)?.clone();
            let requested = Self::argument(r, args, 1)?;
            let matches_capability = expect_ref(receiver)
                .zip(expect_ref(requested))
                .is_some_and(|(capability, requested)| capability == requested);
            return Ok(self.lazy_optional(
                (matches_capability && !matches!(value, Value::Ref(None))).then_some(value),
            ));
        }
        if (r.owner == "net/minecraft/world/level/Level"
            || r.owner == "net/minecraft/world/level/block/entity/BlockEntity")
            && r.name == "getCapability"
            && r.descriptor.ends_with("LazyOptional;")
            && args.len() >= 2
        {
            let capability = capability_text(Self::argument(r, args, 0)?);
            let (position, side) = if args.len() >= 3 {
                (
                    Some(capability_text(Self::argument(r, args, 1)?)),
                    Some(capability_text(Self::argument(r, args, 2)?)),
                )
            } else {
                (None, Some(capability_text(Self::argument(r, args, 1)?)))
            };
            let request = CapabilityRequest {
                owner: r.owner.clone(),
                capability,
                position,
                side,
            };
            let value = self.capability_values.get(&request.key()).cloned();
            self.capability_requests.push(request);
            return Ok(self.lazy_optional(value));
        }
        if is_lazy_optional_owner(&r.owner) {
            let contents = self.lazy_contents(receiver).flatten();
            match (r.name.as_str(), r.descriptor.as_str()) {
                ("isPresent", "()Z") => return Ok(Value::Int(contents.is_some() as i32)),
                ("isEmpty", "()Z") => return Ok(Value::Int(contents.is_none() as i32)),
                ("resolve", "()Ljava/lang/Object;") | ("get", "()Ljava/lang/Object;") => {
                    return Ok(contents.unwrap_or(Value::Ref(None)));
                }
                ("orElse", "(Ljava/lang/Object;)Ljava/lang/Object;") => {
                    return Ok(contents.unwrap_or(Self::argument(r, args, 0)?.clone()));
                }
                ("orElseGet", "(Ljava/util/function/Supplier;)Ljava/lang/Object;") => {
                    if let Some(value) = contents {
                        return Ok(value);
                    }
                    let supplier = Self::argument(r, args, 0)?;
                    return self.invoke_instance(
                        &MethodRef {
                            owner: "java/util/function/Supplier".into(),
                            name: "get".into(),
                            descriptor: "()Ljava/lang/Object;".into(),
                        },
                        supplier,
                        &[],
                    );
                }
                ("map", descriptor) if descriptor.ends_with("LazyOptional;") => {
                    let Some(value) = contents else {
                        return Ok(self.lazy_optional(None));
                    };
                    let function = Self::argument(r, args, 0)?;
                    let mapped = self.invoke_instance(
                        &MethodRef {
                            owner: "java/util/function/Function".into(),
                            name: "apply".into(),
                            descriptor: "(Ljava/lang/Object;)Ljava/lang/Object;".into(),
                        },
                        function,
                        &[value],
                    )?;
                    return Ok(self.lazy_optional(match mapped {
                        Value::Ref(None) => None,
                        value => Some(value),
                    }));
                }
                ("cast", descriptor) if descriptor.ends_with("LazyOptional;") => {
                    return Ok(receiver.clone());
                }
                ("ifPresent", "(Ljava/util/function/Consumer;)V") => {
                    if let Some(value) = contents {
                        let consumer = Self::argument(r, args, 0)?;
                        self.invoke_instance(
                            &MethodRef {
                                owner: "java/util/function/Consumer".into(),
                                name: "accept".into(),
                                descriptor: "(Ljava/lang/Object;)V".into(),
                            },
                            consumer,
                            &[value],
                        )?;
                    }
                    return Ok(Value::Void);
                }
                ("addListener", descriptor) if descriptor.ends_with("LazyOptional;") => {
                    return Ok(receiver.clone());
                }
                ("invalidate", "()V") => {
                    if let Some(marker) = receiver_marker.as_deref()
                        && let Some(value) = self.lazy_optionals.get_mut(marker)
                    {
                        *value = None;
                    }
                    return Ok(Value::Void);
                }
                ("resolveOptional", "()Ljava/util/Optional;") => {
                    return Ok(contents.map_or(Value::Ref(None), optional_wrap));
                }
                _ => {}
            }
        }
        if let Some(marker) = receiver_marker.as_deref()
            && is_collection_owner(&r.owner)
        {
            if r.name == "removeIf"
                && r.descriptor == "(Ljava/util/function/Predicate;)Z"
                && !is_map_owner(&r.owner)
            {
                let predicate = Self::argument(r, args, 0)?.clone();
                let values = host_collection_values(self, receiver)?
                    .into_iter()
                    .map(|value| parse_collection_value(&value))
                    .collect::<Result<Vec<_>, _>>()?;
                let mut kept = Vec::with_capacity(values.len());
                let mut removed = false;
                for value in values {
                    let result = self.invoke_instance(
                        &MethodRef {
                            owner: "java/util/function/Predicate".into(),
                            name: "test".into(),
                            descriptor: "(Ljava/lang/Object;)Z".into(),
                        },
                        &predicate,
                        std::slice::from_ref(&value),
                    )?;
                    if matches!(result, Value::Int(flag) if flag != 0) {
                        removed = true;
                    } else {
                        kept.push(value);
                    }
                }
                self.mutable_collections.insert(marker.to_owned(), kept);
                return Ok(Value::Int(removed as i32));
            }
            if r.name == "forEach"
                && r.descriptor == "(Ljava/util/function/Consumer;)V"
                && !is_map_owner(&r.owner)
            {
                let callback = Self::argument(r, args, 0)?.clone();
                let values = host_collection_values(self, receiver)?
                    .into_iter()
                    .map(|value| parse_collection_value(&value))
                    .collect::<Result<Vec<_>, _>>()?;
                for value in values {
                    self.invoke_instance(
                        &MethodRef {
                            owner: "java/util/function/Consumer".into(),
                            name: "accept".into(),
                            descriptor: "(Ljava/lang/Object;)V".into(),
                        },
                        &callback,
                        &[value],
                    )?;
                }
                return Ok(Value::Void);
            }
            match (r.owner.as_str(), r.name.as_str(), r.descriptor.as_str()) {
                (_, "<init>", _) => {
                    self.ensure_mutable_collection(receiver);
                    return Ok(Value::Void);
                }
                (
                    "java/util/List" | "java/util/AbstractList" | "java/util/ArrayList",
                    "add",
                    "(Ljava/lang/Object;)Z",
                ) => {
                    let value = Self::argument(r, args, 0)?.clone();
                    self.mutable_collections
                        .entry(marker.to_owned())
                        .or_default()
                        .push(value);
                    return Ok(Value::Int(1));
                }
                (
                    "java/util/List" | "java/util/AbstractList" | "java/util/ArrayList",
                    "add",
                    "(ILjava/lang/Object;)V",
                ) => {
                    let index = usize::try_from(expect_int(r, Self::argument(r, args, 0)?)?)
                        .map_err(|_| NativeError::Metadata("list index is negative".into()))?;
                    let value = Self::argument(r, args, 1)?.clone();
                    let values = self
                        .mutable_collections
                        .entry(marker.to_owned())
                        .or_default();
                    if index > values.len() {
                        return Err(NativeError::Metadata("list index is out of bounds".into()));
                    }
                    values.insert(index, value);
                    return Ok(Value::Void);
                }
                (
                    "java/util/List" | "java/util/AbstractList" | "java/util/ArrayList",
                    "addAll",
                    "(Ljava/util/Collection;)Z",
                ) => {
                    let source = Self::argument(r, args, 0)?;
                    let additions = host_collection_values(self, source)?
                        .into_iter()
                        .map(|value| parse_collection_value(&value))
                        .collect::<Result<Vec<_>, _>>()?;
                    let changed = !additions.is_empty();
                    self.mutable_collections
                        .entry(marker.to_owned())
                        .or_default()
                        .extend(additions);
                    return Ok(Value::Int(changed as i32));
                }
                (
                    "java/util/List" | "java/util/AbstractList" | "java/util/ArrayList",
                    "addAll",
                    "(ILjava/util/Collection;)Z",
                ) => {
                    let index = usize::try_from(expect_int(r, Self::argument(r, args, 0)?)?)
                        .map_err(|_| NativeError::Metadata("list index is negative".into()))?;
                    let source = Self::argument(r, args, 1)?;
                    let additions = host_collection_values(self, source)?
                        .into_iter()
                        .map(|value| parse_collection_value(&value))
                        .collect::<Result<Vec<_>, _>>()?;
                    let values = self
                        .mutable_collections
                        .entry(marker.to_owned())
                        .or_default();
                    if index > values.len() {
                        return Err(NativeError::Metadata("list index is out of bounds".into()));
                    }
                    let changed = !additions.is_empty();
                    values.splice(index..index, additions);
                    return Ok(Value::Int(changed as i32));
                }
                (
                    "java/util/List" | "java/util/AbstractList" | "java/util/ArrayList",
                    "set",
                    "(ILjava/lang/Object;)Ljava/lang/Object;",
                ) => {
                    let index = usize::try_from(expect_int(r, Self::argument(r, args, 0)?)?)
                        .map_err(|_| NativeError::Metadata("list index is negative".into()))?;
                    let value = Self::argument(r, args, 1)?.clone();
                    let values = self
                        .mutable_collections
                        .entry(marker.to_owned())
                        .or_default();
                    let slot = values.get_mut(index).ok_or_else(|| {
                        NativeError::Metadata("list index is out of bounds".into())
                    })?;
                    return Ok(std::mem::replace(slot, value));
                }
                (
                    "java/util/List" | "java/util/AbstractList" | "java/util/ArrayList",
                    "remove",
                    "(I)Ljava/lang/Object;",
                ) => {
                    let index = usize::try_from(expect_int(r, Self::argument(r, args, 0)?)?)
                        .map_err(|_| NativeError::Metadata("list index is negative".into()))?;
                    let values = self
                        .mutable_collections
                        .entry(marker.to_owned())
                        .or_default();
                    if index >= values.len() {
                        return Err(NativeError::Metadata("list index is out of bounds".into()));
                    }
                    return Ok(values.remove(index));
                }
                (
                    "java/util/List" | "java/util/AbstractList" | "java/util/ArrayList",
                    "remove",
                    "(Ljava/lang/Object;)Z",
                ) => {
                    let needle = Self::argument(r, args, 0)?;
                    let values = self
                        .mutable_collections
                        .entry(marker.to_owned())
                        .or_default();
                    if let Some(index) = values.iter().position(|value| value == needle) {
                        values.remove(index);
                        return Ok(Value::Int(1));
                    }
                    return Ok(Value::Int(0));
                }
                (
                    "java/util/List" | "java/util/AbstractList" | "java/util/ArrayList",
                    "clear",
                    "()V",
                ) => {
                    self.mutable_collections
                        .entry(marker.to_owned())
                        .or_default()
                        .clear();
                    return Ok(Value::Void);
                }
                (owner, "removeAll", "(Ljava/util/Collection;)Z")
                    if is_collection_owner(owner) && !is_map_owner(owner) =>
                {
                    let source = Self::argument(r, args, 0)?;
                    let removed = host_collection_values(self, source)?
                        .into_iter()
                        .map(|value| parse_collection_value(&value))
                        .collect::<Result<Vec<_>, _>>()?;
                    let values = self
                        .mutable_collections
                        .entry(marker.to_owned())
                        .or_default();
                    let before = values.len();
                    values.retain(|value| !removed.iter().any(|needle| needle == value));
                    return Ok(Value::Int((before != values.len()) as i32));
                }
                (owner, "retainAll", "(Ljava/util/Collection;)Z")
                    if is_collection_owner(owner) && !is_map_owner(owner) =>
                {
                    let source = Self::argument(r, args, 0)?;
                    let retained = host_collection_values(self, source)?
                        .into_iter()
                        .map(|value| parse_collection_value(&value))
                        .collect::<Result<Vec<_>, _>>()?;
                    let values = self
                        .mutable_collections
                        .entry(marker.to_owned())
                        .or_default();
                    let before = values.len();
                    values.retain(|value| retained.iter().any(|needle| needle == value));
                    return Ok(Value::Int((before != values.len()) as i32));
                }
                (
                    "java/util/Set"
                    | "java/util/AbstractSet"
                    | "java/util/HashSet"
                    | "java/util/LinkedHashSet",
                    "add",
                    "(Ljava/lang/Object;)Z",
                ) => {
                    let value = Self::argument(r, args, 0)?.clone();
                    let values = self
                        .mutable_collections
                        .entry(marker.to_owned())
                        .or_default();
                    if values.iter().any(|existing| existing == &value) {
                        return Ok(Value::Int(0));
                    }
                    values.push(value);
                    return Ok(Value::Int(1));
                }
                (
                    "java/util/Set"
                    | "java/util/AbstractSet"
                    | "java/util/HashSet"
                    | "java/util/LinkedHashSet",
                    "remove",
                    "(Ljava/lang/Object;)Z",
                ) => {
                    let needle = Self::argument(r, args, 0)?;
                    let values = self
                        .mutable_collections
                        .entry(marker.to_owned())
                        .or_default();
                    if let Some(index) = values.iter().position(|value| value == needle) {
                        values.remove(index);
                        return Ok(Value::Int(1));
                    }
                    return Ok(Value::Int(0));
                }
                (
                    "java/util/Set"
                    | "java/util/AbstractSet"
                    | "java/util/HashSet"
                    | "java/util/LinkedHashSet",
                    "clear",
                    "()V",
                ) => {
                    self.mutable_collections
                        .entry(marker.to_owned())
                        .or_default()
                        .clear();
                    return Ok(Value::Void);
                }
                (
                    "java/util/Map"
                    | "java/util/AbstractMap"
                    | "java/util/HashMap"
                    | "java/util/LinkedHashMap",
                    "computeIfAbsent",
                    "(Ljava/lang/Object;Ljava/util/function/Function;)Ljava/lang/Object;",
                ) => {
                    let key = Self::argument(r, args, 0)?.clone();
                    let function = Self::argument(r, args, 1)?;
                    let values = self
                        .mutable_collections
                        .entry(marker.to_owned())
                        .or_default();
                    if let Some(index) = values.chunks_exact(2).position(|pair| pair[0] == key) {
                        return Ok(values[index * 2 + 1].clone());
                    }
                    let computed = match function {
                        Value::Ref(Some(callback)) if callback.starts_with("@indy:") => {
                            Value::Ref(Some(format!("computed/{}", value_marker(&key))))
                        }
                        Value::Ref(None) => Value::Ref(None),
                        other => Value::Ref(Some(format!(
                            "computed/{}/{}",
                            value_marker(&key),
                            value_marker(other)
                        ))),
                    };
                    if !matches!(computed, Value::Ref(None)) {
                        values.extend([key, computed.clone()]);
                    }
                    return Ok(computed);
                }
                (
                    "java/util/Map"
                    | "java/util/AbstractMap"
                    | "java/util/HashMap"
                    | "java/util/LinkedHashMap",
                    "computeIfPresent",
                    "(Ljava/lang/Object;Ljava/util/function/BiFunction;)Ljava/lang/Object;",
                ) => {
                    let key = Self::argument(r, args, 0)?.clone();
                    let function = Self::argument(r, args, 1)?.clone();
                    let current = self.mutable_collections.get(marker).and_then(|values| {
                        values
                            .chunks_exact(2)
                            .find(|pair| pair[0] == key)
                            .map(|pair| pair[1].clone())
                    });
                    let Some(current) = current else {
                        return Ok(Value::Ref(None));
                    };
                    let computed = self.invoke_instance(
                        &MethodRef {
                            owner: "java/util/function/BiFunction".into(),
                            name: "apply".into(),
                            descriptor: "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;"
                                .into(),
                        },
                        &function,
                        &[key.clone(), current],
                    )?;
                    let values = self
                        .mutable_collections
                        .entry(marker.to_owned())
                        .or_default();
                    if let Some(index) = values.chunks_exact(2).position(|pair| pair[0] == key) {
                        if matches!(computed, Value::Ref(None)) {
                            values.drain(index * 2..index * 2 + 2);
                            return Ok(Value::Ref(None));
                        }
                        values[index * 2 + 1] = computed.clone();
                    }
                    return Ok(computed);
                }
                (
                    "java/util/Map"
                    | "java/util/AbstractMap"
                    | "java/util/HashMap"
                    | "java/util/LinkedHashMap",
                    "forEach",
                    "(Ljava/util/function/BiConsumer;)V",
                ) => {
                    let callback = Self::argument(r, args, 0)?.clone();
                    let entries = self
                        .mutable_collections
                        .entry(marker.to_owned())
                        .or_default()
                        .chunks_exact(2)
                        .map(|pair| [pair[0].clone(), pair[1].clone()])
                        .collect::<Vec<_>>();
                    for [key, value] in entries {
                        self.invoke_instance(
                            &MethodRef {
                                owner: "java/util/function/BiConsumer".into(),
                                name: "accept".into(),
                                descriptor: "(Ljava/lang/Object;Ljava/lang/Object;)V".into(),
                            },
                            &callback,
                            &[key, value],
                        )?;
                    }
                    return Ok(Value::Void);
                }
                (
                    "java/util/Map"
                    | "java/util/AbstractMap"
                    | "java/util/HashMap"
                    | "java/util/LinkedHashMap",
                    "merge",
                    "(Ljava/lang/Object;Ljava/lang/Object;Ljava/util/function/BiFunction;)Ljava/lang/Object;",
                ) => {
                    let key = Self::argument(r, args, 0)?.clone();
                    let incoming = Self::argument(r, args, 1)?.clone();
                    let function = Self::argument(r, args, 2)?.clone();
                    let values = self
                        .mutable_collections
                        .entry(marker.to_owned())
                        .or_default();
                    if let Some(index) = values.chunks_exact(2).position(|pair| pair[0] == key) {
                        let current = values[index * 2 + 1].clone();
                        let merged = self.invoke_instance(
                            &MethodRef {
                                owner: "java/util/function/BiFunction".into(),
                                name: "apply".into(),
                                descriptor:
                                    "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;"
                                        .into(),
                            },
                            &function,
                            &[current, incoming],
                        )?;
                        let values = self
                            .mutable_collections
                            .entry(marker.to_owned())
                            .or_default();
                        if matches!(merged, Value::Ref(None)) {
                            values.drain(index * 2..index * 2 + 2);
                            return Ok(Value::Ref(None));
                        }
                        values[index * 2 + 1] = merged.clone();
                        return Ok(merged);
                    }
                    values.extend([key, incoming.clone()]);
                    return Ok(incoming);
                }
                (
                    "java/util/Map"
                    | "java/util/AbstractMap"
                    | "java/util/HashMap"
                    | "java/util/LinkedHashMap",
                    "putIfAbsent",
                    "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
                ) => {
                    let key = Self::argument(r, args, 0)?.clone();
                    let value = Self::argument(r, args, 1)?.clone();
                    let values = self
                        .mutable_collections
                        .entry(marker.to_owned())
                        .or_default();
                    if let Some(index) = values.chunks_exact(2).position(|pair| pair[0] == key) {
                        return Ok(values[index * 2 + 1].clone());
                    }
                    values.extend([key, value]);
                    return Ok(Value::Ref(None));
                }
                (
                    "java/util/Map"
                    | "java/util/AbstractMap"
                    | "java/util/HashMap"
                    | "java/util/LinkedHashMap",
                    "replace",
                    "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
                ) => {
                    let key = Self::argument(r, args, 0)?;
                    let value = Self::argument(r, args, 1)?.clone();
                    let values = self
                        .mutable_collections
                        .entry(marker.to_owned())
                        .or_default();
                    if let Some(index) = values.chunks_exact(2).position(|pair| &pair[0] == key) {
                        return Ok(std::mem::replace(&mut values[index * 2 + 1], value));
                    }
                    return Ok(Value::Ref(None));
                }
                (
                    "java/util/Map"
                    | "java/util/AbstractMap"
                    | "java/util/HashMap"
                    | "java/util/LinkedHashMap",
                    "replace",
                    "(Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;)Z",
                ) => {
                    let key = Self::argument(r, args, 0)?;
                    let old = Self::argument(r, args, 1)?;
                    let new = Self::argument(r, args, 2)?.clone();
                    let values = self
                        .mutable_collections
                        .entry(marker.to_owned())
                        .or_default();
                    if let Some(index) = values
                        .chunks_exact(2)
                        .position(|pair| &pair[0] == key && &pair[1] == old)
                    {
                        values[index * 2 + 1] = new;
                        return Ok(Value::Int(1));
                    }
                    return Ok(Value::Int(0));
                }
                (
                    "java/util/Map"
                    | "java/util/AbstractMap"
                    | "java/util/HashMap"
                    | "java/util/LinkedHashMap",
                    "replaceAll",
                    "(Ljava/util/function/BiFunction;)V",
                ) => {
                    let callback = Self::argument(r, args, 0)?.clone();
                    let entries = self
                        .mutable_collections
                        .entry(marker.to_owned())
                        .or_default()
                        .chunks_exact(2)
                        .map(|pair| [pair[0].clone(), pair[1].clone()])
                        .collect::<Vec<_>>();
                    let mut replacements = Vec::with_capacity(entries.len());
                    for [key, value] in entries {
                        replacements.push(
                            self.invoke_instance(
                                &MethodRef {
                                    owner: "java/util/function/BiFunction".into(),
                                    name: "apply".into(),
                                    descriptor:
                                        "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;"
                                            .into(),
                                },
                                &callback,
                                &[key, value],
                            )?,
                        );
                    }
                    let values = self
                        .mutable_collections
                        .entry(marker.to_owned())
                        .or_default();
                    for (index, value) in replacements.into_iter().enumerate() {
                        values[index * 2 + 1] = value;
                    }
                    return Ok(Value::Void);
                }
                (
                    "java/util/Map"
                    | "java/util/AbstractMap"
                    | "java/util/HashMap"
                    | "java/util/LinkedHashMap",
                    "put",
                    "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
                ) => {
                    let key = Self::argument(r, args, 0)?.clone();
                    let value = Self::argument(r, args, 1)?.clone();
                    let values = self
                        .mutable_collections
                        .entry(marker.to_owned())
                        .or_default();
                    for pair in values.chunks_exact_mut(2) {
                        if pair[0] == key {
                            return Ok(std::mem::replace(&mut pair[1], value));
                        }
                    }
                    values.extend([key, value]);
                    return Ok(Value::Ref(None));
                }
                (
                    "java/util/Map"
                    | "java/util/AbstractMap"
                    | "java/util/HashMap"
                    | "java/util/LinkedHashMap",
                    "putAll",
                    "(Ljava/util/Map;)V",
                ) => {
                    let source = Self::argument(r, args, 0)?;
                    let entries = host_collection_values(self, source)?;
                    let mut pairs = entries.chunks_exact(2);
                    let mut updates = Vec::new();
                    for pair in &mut pairs {
                        updates.push((
                            parse_collection_value(&pair[0])?,
                            parse_collection_value(&pair[1])?,
                        ));
                    }
                    if !pairs.remainder().is_empty() {
                        return Err(NativeError::Metadata(
                            "map marker contains an odd number of entries".into(),
                        ));
                    }
                    let values = self
                        .mutable_collections
                        .entry(marker.to_owned())
                        .or_default();
                    for (key, value) in updates {
                        if let Some(index) = values.chunks_exact(2).position(|pair| pair[0] == key)
                        {
                            values[index * 2 + 1] = value;
                        } else {
                            values.extend([key, value]);
                        }
                    }
                    return Ok(Value::Void);
                }
                (
                    "java/util/Map"
                    | "java/util/AbstractMap"
                    | "java/util/HashMap"
                    | "java/util/LinkedHashMap",
                    "remove",
                    "(Ljava/lang/Object;Ljava/lang/Object;)Z",
                ) => {
                    let key = Self::argument(r, args, 0)?;
                    let expected = Self::argument(r, args, 1)?;
                    let values = self
                        .mutable_collections
                        .entry(marker.to_owned())
                        .or_default();
                    if let Some(index) = values
                        .chunks_exact(2)
                        .position(|pair| &pair[0] == key && &pair[1] == expected)
                    {
                        let index = index * 2;
                        values.drain(index..index + 2);
                        return Ok(Value::Int(1));
                    }
                    return Ok(Value::Int(0));
                }
                (
                    "java/util/Map"
                    | "java/util/AbstractMap"
                    | "java/util/HashMap"
                    | "java/util/LinkedHashMap",
                    "remove",
                    "(Ljava/lang/Object;)Ljava/lang/Object;",
                ) => {
                    let key = Self::argument(r, args, 0)?;
                    let values = self
                        .mutable_collections
                        .entry(marker.to_owned())
                        .or_default();
                    if let Some(index) = values.chunks_exact(2).position(|pair| &pair[0] == key) {
                        let index = index * 2;
                        values.remove(index);
                        return Ok(values.remove(index));
                    }
                    return Ok(Value::Ref(None));
                }
                (
                    "java/util/Map"
                    | "java/util/AbstractMap"
                    | "java/util/HashMap"
                    | "java/util/LinkedHashMap",
                    "clear",
                    "()V",
                ) => {
                    self.mutable_collections
                        .entry(marker.to_owned())
                        .or_default()
                        .clear();
                    return Ok(Value::Void);
                }
                _ => {}
            }
        }
        if r.owner == "net/fabricmc/fabric/api/event/Event" {
            match r.name.as_str() {
                "register" => return Ok(Value::Void),
                "invoker" => {
                    let marker = receiver_marker.unwrap_or_else(|| "fabric-event/unknown".into());
                    return Ok(Value::Ref(Some(format!("{marker}/invoker"))));
                }
                _ => {}
            }
        }
        if r.owner == "net/minecraft/network/protocol/common/custom/CustomPacketPayload$Type" {
            let marker = expect_ref(receiver).ok_or_else(|| {
                NativeError::Metadata("malformed packet payload type marker".into())
            })?;
            match (r.name.as_str(), r.descriptor.as_str()) {
                (
                    "<init>",
                    "(Lnet/minecraft/resources/Identifier;Lnet/minecraft/network/codec/StreamCodec;)V",
                ) => {
                    let (namespace, path) = identifier_parts(Self::argument(r, args, 0)?);
                    self.packet_types
                        .insert(marker.to_owned(), format!("{namespace}:{path}"));
                    return Ok(Value::Void);
                }
                ("id", "()Lnet/minecraft/resources/Identifier;") => {
                    let id = self
                        .packet_types
                        .get(marker)
                        .map_or("minecraft:unknown", String::as_str);
                    return Ok(identifier_parse(id));
                }
                ("toString", "()Ljava/lang/String;") => {
                    let id = self
                        .packet_types
                        .get(marker)
                        .map_or("minecraft:unknown", String::as_str);
                    return Ok(Value::Ref(Some(id.to_owned())));
                }
                ("hashCode", "()I") => {
                    let id = self
                        .packet_types
                        .get(marker)
                        .map_or("minecraft:unknown", String::as_str);
                    return Ok(Value::Int(stable_hash(id)));
                }
                ("equals", "(Ljava/lang/Object;)Z") => {
                    let other = expect_ref(Self::argument(r, args, 0)?)
                        .and_then(|other| self.packet_types.get(other));
                    return Ok(Value::Int((self.packet_types.get(marker) == other) as i32));
                }
                _ => {}
            }
        }
        if r.owner == "java/util/ServiceLoader" {
            let marker = receiver_marker.as_deref().ok_or_else(|| {
                NativeError::Metadata("malformed java/util/ServiceLoader marker".into())
            })?;
            let service = marker.strip_prefix("service-loader/").ok_or_else(|| {
                NativeError::Metadata("unsupported java/util/ServiceLoader marker".into())
            })?;
            if r.name == "iterator" && r.descriptor == "()Ljava/util/Iterator;" {
                let values = self
                    .service_providers
                    .get(service)
                    .ok_or_else(|| {
                        NativeError::Metadata(format!(
                            "unknown seeded ServiceLoader service {service}"
                        ))
                    })?
                    .iter()
                    .cloned()
                    .map(|provider| Value::Ref(Some(provider)))
                    .collect::<Vec<_>>();
                let iterator = format!("iterator/{}", self.next_iterator);
                self.next_iterator = self.next_iterator.saturating_add(1);
                self.iterators.insert(iterator.clone(), (values, 0));
                return Ok(Value::Ref(Some(iterator)));
            }
            if r.name == "reload" && r.descriptor == "()V" {
                return Ok(Value::Void);
            }
        }
        if r.owner == "java/util/Enumeration"
            && matches!(r.descriptor.as_str(), "()Z" | "()Ljava/lang/Object;")
        {
            let marker = receiver_marker.as_deref().ok_or_else(|| {
                NativeError::Metadata("malformed java/util/Enumeration marker".into())
            })?;
            let (values, index) = self.iterators.get_mut(marker).ok_or_else(|| {
                NativeError::Metadata("unknown java/util/Enumeration marker".into())
            })?;
            match (r.name.as_str(), r.descriptor.as_str()) {
                ("hasMoreElements", "()Z") => {
                    return Ok(Value::Int((*index < values.len()) as i32));
                }
                ("nextElement", "()Ljava/lang/Object;") => {
                    let value = values.get(*index).cloned().ok_or_else(|| {
                        NativeError::Metadata("java/util/Enumeration exhausted".into())
                    })?;
                    *index += 1;
                    return Ok(value);
                }
                _ => {}
            }
        }
        // NeoForge event subclasses inherit this query from the common event
        // base, but javac may emit the concrete subclass as the owner. Keep
        // the marker-based cancellation state visible regardless of that
        // symbolic owner choice; mutating cancellation remains a game-host
        // responsibility.
        if r.name == "isCanceled" && r.descriptor == "()Z" {
            return Ok(Value::Int(event_is_cancelled(receiver) as i32));
        }
        if r.name == "iterator"
            && r.descriptor == "()Ljava/util/Iterator;"
            && is_collection_owner(&r.owner)
            && !is_map_owner(&r.owner)
        {
            let values = host_collection_values(self, receiver)?
                .into_iter()
                .map(|value| parse_collection_value(&value))
                .collect::<Result<Vec<_>, _>>()?;
            let marker = format!("iterator/{}", self.next_iterator);
            self.next_iterator = self.next_iterator.saturating_add(1);
            self.iterators.insert(marker.clone(), (values, 0));
            return Ok(Value::Ref(Some(marker)));
        }
        if matches!(
            r.owner.as_str(),
            "java/util/Iterator" | "java/util/ListIterator"
        ) {
            let marker = receiver_marker.as_deref().ok_or_else(|| {
                NativeError::Metadata("malformed java/util/Iterator marker".into())
            })?;
            if r.name == "forEachRemaining" && r.descriptor == "(Ljava/util/function/Consumer;)V" {
                let callback = Self::argument(r, args, 0)?.clone();
                let remaining = {
                    let (values, index) = self.iterators.get_mut(marker).ok_or_else(|| {
                        NativeError::Metadata("unknown java/util/Iterator marker".into())
                    })?;
                    let remaining = values.get(*index..).unwrap_or_default().to_vec();
                    *index = values.len();
                    remaining
                };
                for value in remaining {
                    self.invoke_instance(
                        &MethodRef {
                            owner: "java/util/function/Consumer".into(),
                            name: "accept".into(),
                            descriptor: "(Ljava/lang/Object;)V".into(),
                        },
                        &callback,
                        &[value],
                    )?;
                }
                return Ok(Value::Void);
            }
            let (values, index) = self
                .iterators
                .get_mut(marker)
                .ok_or_else(|| NativeError::Metadata("unknown java/util/Iterator marker".into()))?;
            match (r.name.as_str(), r.descriptor.as_str()) {
                ("hasNext", "()Z") => return Ok(Value::Int((*index < values.len()) as i32)),
                ("next", "()Ljava/lang/Object;") => {
                    let value = values.get(*index).cloned().ok_or_else(|| {
                        NativeError::Metadata("java/util/Iterator exhausted".into())
                    })?;
                    *index += 1;
                    return Ok(value);
                }
                _ => {}
            }
        }
        if r.owner == "java/util/Map$Entry" {
            let (key, value) = map_entry_parts(receiver)?;
            return match (r.name.as_str(), r.descriptor.as_str()) {
                ("getKey", "()Ljava/lang/Object;") => Ok(key),
                ("getValue", "()Ljava/lang/Object;") => Ok(value),
                ("equals", "(Ljava/lang/Object;)Z") => {
                    let equal = map_entry_parts(Self::argument(r, args, 0)?).is_ok_and(
                        |(other_key, other_value)| key == other_key && value == other_value,
                    );
                    Ok(Value::Int(equal as i32))
                }
                ("hashCode", "()I") => Ok(Value::Int(stable_hash(&format!(
                    "map-entry/{},{}",
                    value_marker(&key),
                    value_marker(&value)
                )))),
                ("toString", "()Ljava/lang/String;") => Ok(Value::Ref(Some(format!(
                    "{}={}",
                    value_marker(&key),
                    value_marker(&value)
                )))),
                _ => Err(NativeError::MissingMethod(format!(
                    "{}#{}{}",
                    r.owner, r.name, r.descriptor
                ))),
            };
        }
        match (r.owner.as_str(), r.name.as_str(), r.descriptor.as_str()) {
            ("java/nio/file/Path", "toString", "()Ljava/lang/String;") => {
                Ok(Value::Ref(Some(path_text(receiver).to_owned())))
            }
            ("java/nio/file/Path", "toAbsolutePath", "()Ljava/nio/file/Path;")
            | ("java/nio/file/Path", "normalize", "()Ljava/nio/file/Path;") => {
                Ok(path_value(path_text(receiver)))
            }
            ("java/nio/file/Path", "resolve", "(Ljava/lang/String;)Ljava/nio/file/Path;") => {
                let child = expect_string(Self::argument(r, args, 0)?);
                Ok(path_value(&path_join(path_text(receiver), child)))
            }
            ("java/nio/file/Path", "resolve", "(Ljava/nio/file/Path;)Ljava/nio/file/Path;") => {
                let child = path_text(Self::argument(r, args, 0)?);
                Ok(path_value(&path_join(path_text(receiver), child)))
            }
            ("java/nio/file/Path", "getFileName", "()Ljava/nio/file/Path;") => {
                let name = path_text(receiver).rsplit('/').next().unwrap_or_default();
                Ok(path_value(name))
            }
            ("java/lang/Class", "getName", "()Ljava/lang/String;")
            | ("java/lang/Class", "getTypeName", "()Ljava/lang/String;")
            | ("java/lang/Class", "getCanonicalName", "()Ljava/lang/String;") => {
                Ok(Value::Ref(Some(class_binary_name(receiver))))
            }
            ("java/lang/Class", "getSimpleName", "()Ljava/lang/String;") => {
                let name = class_binary_name(receiver);
                Ok(Value::Ref(Some(
                    name.rsplit('.').next().unwrap_or(&name).to_owned(),
                )))
            }
            ("java/lang/Class", "isPrimitive", "()Z") => {
                Ok(Value::Int(class_is_primitive(receiver) as i32))
            }
            ("java/lang/Class", "isArray", "()Z") => {
                Ok(Value::Int(class_is_array(receiver) as i32))
            }
            ("java/lang/Class", "isInterface", "()Z") => Ok(Value::Int(0)),
            ("java/lang/Class", "desiredAssertionStatus", "()Z") => Ok(Value::Int(0)),
            ("java/lang/Class", "isAssignableFrom", "(Ljava/lang/Class;)Z") => {
                let target = class_binary_name(receiver);
                let source = class_binary_name(Self::argument(r, args, 0)?);
                Ok(Value::Int(class_is_assignable(&target, &source) as i32))
            }
            (owner, "size", "()I") if is_collection_owner(owner) => {
                let values = host_collection_values(self, receiver)?;
                let size = if is_map_owner(owner) {
                    values.len() / 2
                } else {
                    values.len()
                };
                Ok(Value::Int(size as i32))
            }
            (owner, "isEmpty", "()Z") if is_collection_owner(owner) => Ok(Value::Int(
                host_collection_values(self, receiver)?.is_empty() as i32,
            )),
            (owner, "contains", "(Ljava/lang/Object;)Z") if is_collection_owner(owner) => {
                let needle = value_marker(Self::argument(r, args, 0)?);
                Ok(Value::Int(
                    host_collection_values(self, receiver)?
                        .iter()
                        .any(|value| value == &needle) as i32,
                ))
            }
            (owner, "containsAll", "(Ljava/util/Collection;)Z")
                if is_collection_owner(owner) && !is_map_owner(owner) =>
            {
                let source = host_collection_values(self, Self::argument(r, args, 0)?)?;
                let values = host_collection_values(self, receiver)?
                    .into_iter()
                    .map(|value| parse_collection_value(&value))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Value::Int(
                    source
                        .into_iter()
                        .map(|value| parse_collection_value(&value))
                        .collect::<Result<Vec<_>, _>>()?
                        .iter()
                        .all(|needle| values.iter().any(|value| value == needle))
                        as i32,
                ))
            }
            (owner, "indexOf", "(Ljava/lang/Object;)I") if is_list_owner(owner) => {
                let needle = value_marker(Self::argument(r, args, 0)?);
                Ok(Value::Int(
                    host_collection_values(self, receiver)?
                        .iter()
                        .position(|value| value == &needle)
                        .map_or(-1, |index| index as i32),
                ))
            }
            (owner, "lastIndexOf", "(Ljava/lang/Object;)I") if is_list_owner(owner) => {
                let needle = value_marker(Self::argument(r, args, 0)?);
                Ok(Value::Int(
                    host_collection_values(self, receiver)?
                        .iter()
                        .rposition(|value| value == &needle)
                        .map_or(-1, |index| index as i32),
                ))
            }
            (owner, "get", "(I)Ljava/lang/Object;") if is_list_owner(owner) => {
                let index = expect_int(r, Self::argument(r, args, 0)?)?;
                let index = usize::try_from(index)
                    .map_err(|_| NativeError::Metadata("list index is negative".into()))?;
                Ok(parse_collection_value(
                    host_collection_values(self, receiver)?
                        .get(index)
                        .ok_or_else(|| {
                            NativeError::Metadata("list index is out of bounds".into())
                        })?,
                )?)
            }
            (owner, "get", "(Ljava/lang/Object;)Ljava/lang/Object;") if is_map_owner(owner) => {
                let needle = value_marker(Self::argument(r, args, 0)?);
                let values = host_collection_values(self, receiver)?;
                let mut pairs = values.chunks_exact(2);
                Ok(pairs
                    .find(|pair| pair[0] == needle)
                    .map(|pair| parse_collection_value(&pair[1]))
                    .transpose()?
                    .unwrap_or(Value::Ref(None)))
            }
            (owner, "getOrDefault", "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;")
                if is_map_owner(owner) =>
            {
                let needle = value_marker(Self::argument(r, args, 0)?);
                let fallback = Self::argument(r, args, 1)?.clone();
                let values = host_collection_values(self, receiver)?;
                let mut pairs = values.chunks_exact(2);
                if let Some(pair) = pairs.find(|pair| pair[0] == needle) {
                    parse_collection_value(&pair[1])
                } else {
                    Ok(fallback)
                }
            }
            (owner, "containsKey", "(Ljava/lang/Object;)Z") if is_map_owner(owner) => {
                let needle = value_marker(Self::argument(r, args, 0)?);
                Ok(Value::Int(
                    host_collection_values(self, receiver)?
                        .chunks_exact(2)
                        .any(|pair| pair[0] == needle) as i32,
                ))
            }
            (owner, "containsValue", "(Ljava/lang/Object;)Z") if is_map_owner(owner) => {
                let needle = value_marker(Self::argument(r, args, 0)?);
                Ok(Value::Int(
                    host_collection_values(self, receiver)?
                        .chunks_exact(2)
                        .any(|pair| pair[1] == needle) as i32,
                ))
            }
            (
                "java/util/Map"
                | "java/util/HashMap"
                | "java/util/LinkedHashMap"
                | "java/util/Properties",
                "entrySet",
                "()Ljava/util/Set;",
            ) => {
                let values = host_collection_values(self, receiver)?;
                let entries = values
                    .chunks_exact(2)
                    .map(|pair| {
                        let key = parse_collection_value(&pair[0])?;
                        let value = parse_collection_value(&pair[1])?;
                        Ok(map_entry_value(&key, &value))
                    })
                    .collect::<Result<Vec<_>, NativeError>>()?;
                Ok(collection_value("set", &entries))
            }
            (
                "java/util/Map"
                | "java/util/HashMap"
                | "java/util/LinkedHashMap"
                | "java/util/Properties",
                "keySet",
                "()Ljava/util/Set;",
            ) => {
                let values = host_collection_values(self, receiver)?;
                let keys = values
                    .chunks_exact(2)
                    .map(|pair| parse_collection_value(&pair[0]))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(collection_value("set", &keys))
            }
            (
                "java/util/Map"
                | "java/util/HashMap"
                | "java/util/LinkedHashMap"
                | "java/util/Properties",
                "values",
                "()Ljava/util/Collection;",
            ) => {
                let values = host_collection_values(self, receiver)?;
                let values = values
                    .chunks_exact(2)
                    .map(|pair| parse_collection_value(&pair[1]))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(collection_value("list", &values))
            }
            (owner, "toString", "()Ljava/lang/String;") if is_collection_owner(owner) => {
                Ok(Value::Ref(Some(collection_text(receiver)?)))
            }
            // Number's primitive accessors are inherited by every boxed numeric
            // type. Keep the conversion rules in one bounded path so a value
            // written as (for example) a Long can be read through Number.
            ("java/lang/Number", "intValue", "()I") => {
                Ok(Value::Int(number_int_value(receiver).ok_or_else(|| {
                    NativeError::Metadata("malformed java/lang/Number marker".into())
                })?))
            }
            ("java/lang/Number", "longValue", "()J") => {
                Ok(long_value(number_long_value(receiver).ok_or_else(
                    || NativeError::Metadata("malformed java/lang/Number marker".into()),
                )?))
            }
            ("java/lang/Number", "shortValue", "()S") => {
                Ok(Value::Int(number_short_value(receiver).ok_or_else(
                    || NativeError::Metadata("malformed java/lang/Number marker".into()),
                )?))
            }
            ("java/lang/Number", "byteValue", "()B") => {
                Ok(Value::Int(number_byte_value(receiver).ok_or_else(
                    || NativeError::Metadata("malformed java/lang/Number marker".into()),
                )?))
            }
            ("java/lang/Number", "floatValue", "()F") => {
                Ok(float_value(boxed_number(receiver).ok_or_else(|| {
                    NativeError::Metadata("malformed java/lang/Number marker".into())
                })?))
            }
            ("java/lang/Number", "doubleValue", "()D") => {
                Ok(double_value(boxed_number(receiver).ok_or_else(|| {
                    NativeError::Metadata("malformed java/lang/Number marker".into())
                })?))
            }
            ("java/lang/Integer", "intValue", "()I") => {
                Ok(Value::Int(boxed_integer(receiver).ok_or_else(|| {
                    NativeError::Metadata("malformed java/lang/Integer marker".into())
                })?))
            }
            ("java/lang/Integer", "longValue", "()J")
            | ("java/lang/Integer", "shortValue", "()S")
            | ("java/lang/Integer", "byteValue", "()B") => {
                Ok(Value::Int(boxed_integer(receiver).ok_or_else(|| {
                    NativeError::Metadata("malformed java/lang/Integer marker".into())
                })?))
            }
            ("java/lang/Integer", "toString", "()Ljava/lang/String;") => Ok(Value::Ref(Some(
                boxed_integer(receiver)
                    .ok_or_else(|| {
                        NativeError::Metadata("malformed java/lang/Integer marker".into())
                    })?
                    .to_string(),
            ))),
            ("java/lang/Integer", "hashCode", "()I") => {
                Ok(Value::Int(boxed_integer(receiver).ok_or_else(|| {
                    NativeError::Metadata("malformed java/lang/Integer marker".into())
                })?))
            }
            ("java/lang/Integer", "equals", "(Ljava/lang/Object;)Z") => Ok(Value::Int(
                (boxed_integer(receiver).is_some()
                    && boxed_integer(Self::argument(r, args, 0)?)
                        .is_some_and(|other| Some(other) == boxed_integer(receiver)))
                    as i32,
            )),
            ("java/lang/Character", "charValue", "()C") => {
                Ok(Value::Int(boxed_character(receiver).ok_or_else(|| {
                    NativeError::Metadata("malformed java/lang/Character marker".into())
                })?))
            }
            ("java/lang/Character", "toString", "()Ljava/lang/String;") => Ok(Value::Ref(Some(
                char::from_u32(boxed_character(receiver).ok_or_else(|| {
                    NativeError::Metadata("malformed java/lang/Character marker".into())
                })? as u32)
                .unwrap_or('\u{fffd}')
                .to_string(),
            ))),
            ("java/lang/Byte", "byteValue", "()B") => {
                Ok(Value::Int(boxed_byte(receiver).ok_or_else(|| {
                    NativeError::Metadata("malformed java/lang/Byte marker".into())
                })? as i32))
            }
            ("java/lang/Short", "shortValue", "()S") => {
                Ok(Value::Int(i32::from(boxed_short(receiver).ok_or_else(
                    || NativeError::Metadata("malformed java/lang/Short marker".into()),
                )?)))
            }
            ("java/lang/Short", "toString", "()Ljava/lang/String;") => Ok(Value::Ref(Some(
                boxed_short(receiver)
                    .ok_or_else(|| {
                        NativeError::Metadata("malformed java/lang/Short marker".into())
                    })?
                    .to_string(),
            ))),
            ("java/lang/Long", "longValue", "()J") => {
                Ok(long_value(boxed_long(receiver).ok_or_else(|| {
                    NativeError::Metadata("malformed java/lang/Long marker".into())
                })?))
            }
            ("java/lang/Long", "toString", "()Ljava/lang/String;") => Ok(Value::Ref(Some(
                boxed_long(receiver)
                    .ok_or_else(|| NativeError::Metadata("malformed java/lang/Long marker".into()))?
                    .to_string(),
            ))),
            ("java/lang/Double", "doubleValue", "()D")
            | ("java/lang/Float", "doubleValue", "()D") => {
                Ok(double_value(boxed_float(receiver).ok_or_else(|| {
                    NativeError::Metadata("malformed java/lang/Double marker".into())
                })?))
            }
            ("java/lang/Double", "floatValue", "()F")
            | ("java/lang/Float", "floatValue", "()F") => {
                Ok(float_value(boxed_float(receiver).ok_or_else(|| {
                    NativeError::Metadata("malformed java/lang/Float marker".into())
                })?))
            }
            ("java/lang/Double", "toString", "()Ljava/lang/String;")
            | ("java/lang/Float", "toString", "()Ljava/lang/String;") => Ok(Value::Ref(Some(
                format_java_float(boxed_float(receiver).ok_or_else(|| {
                    NativeError::Metadata("malformed floating-point marker".into())
                })?),
            ))),
            ("java/lang/Double", "isNaN", "()Z") | ("java/lang/Float", "isNaN", "()Z") => {
                Ok(Value::Int(
                    boxed_float(receiver)
                        .ok_or_else(|| {
                            NativeError::Metadata("malformed floating-point marker".into())
                        })?
                        .is_nan() as i32,
                ))
            }
            ("java/lang/Double", "isInfinite", "()Z")
            | ("java/lang/Float", "isInfinite", "()Z") => Ok(Value::Int(
                boxed_float(receiver)
                    .ok_or_else(|| NativeError::Metadata("malformed floating-point marker".into()))?
                    .is_infinite() as i32,
            )),
            ("java/lang/Boolean", "booleanValue", "()Z") => {
                Ok(Value::Int(boxed_boolean(receiver).ok_or_else(|| {
                    NativeError::Metadata("malformed java/lang/Boolean marker".into())
                })? as i32))
            }
            ("java/lang/Boolean", "toString", "()Ljava/lang/String;") => Ok(Value::Ref(Some(
                boxed_boolean(receiver)
                    .ok_or_else(|| {
                        NativeError::Metadata("malformed java/lang/Boolean marker".into())
                    })?
                    .to_string(),
            ))),
            ("java/lang/Boolean", "hashCode", "()I") => Ok(Value::Int(
                if boxed_boolean(receiver).ok_or_else(|| {
                    NativeError::Metadata("malformed java/lang/Boolean marker".into())
                })? {
                    1231
                } else {
                    1237
                },
            )),
            ("java/lang/Boolean", "equals", "(Ljava/lang/Object;)Z") => Ok(Value::Int(
                (boxed_boolean(receiver).is_some()
                    && boxed_boolean(Self::argument(r, args, 0)?)
                        .is_some_and(|other| Some(other) == boxed_boolean(receiver)))
                    as i32,
            )),
            ("java/lang/StringBuilder", "<init>", _) | ("java/lang/StringBuffer", "<init>", _) => {
                Ok(Value::Void)
            }
            ("java/lang/StringBuilder", "append", descriptor)
            | ("java/lang/StringBuffer", "append", descriptor)
                if descriptor.ends_with(")Ljava/lang/StringBuilder;")
                    || descriptor.ends_with(")Ljava/lang/StringBuffer;") =>
            {
                let current = builder_contents(receiver).unwrap_or("");
                let added = builder_append_text(r, args)?;
                Ok(Value::Ref(Some(format!("string-builder/{current}{added}"))))
            }
            ("java/lang/StringBuilder", "toString", "()Ljava/lang/String;")
            | ("java/lang/StringBuffer", "toString", "()Ljava/lang/String;") => Ok(Value::Ref(
                Some(builder_contents(receiver).unwrap_or("").to_owned()),
            )),
            ("java/lang/StringBuilder", "length", "()I")
            | ("java/lang/StringBuffer", "length", "()I") => Ok(Value::Int(
                builder_contents(receiver)
                    .unwrap_or("")
                    .encode_utf16()
                    .count() as i32,
            )),
            ("java/lang/String", "length", "()I") => Ok(Value::Int(
                expect_string(receiver).encode_utf16().count() as i32,
            )),
            ("java/lang/String", "isEmpty", "()Z") => {
                Ok(Value::Int(expect_string(receiver).is_empty() as i32))
            }
            ("java/lang/String", "startsWith", "(Ljava/lang/String;)Z") => {
                let prefix = Self::argument(r, args, 0)?;
                Ok(Value::Int(
                    (expect_ref(prefix).is_some()
                        && expect_string(receiver).starts_with(expect_string(prefix)))
                        as i32,
                ))
            }
            ("java/lang/String", "endsWith", "(Ljava/lang/String;)Z") => {
                let suffix = Self::argument(r, args, 0)?;
                Ok(Value::Int(
                    (expect_ref(suffix).is_some()
                        && expect_string(receiver).ends_with(expect_string(suffix)))
                        as i32,
                ))
            }
            ("java/lang/String", "contains", "(Ljava/lang/CharSequence;)Z") => {
                let needle = Self::argument(r, args, 0)?;
                Ok(Value::Int(
                    (expect_ref(needle).is_some()
                        && expect_string(receiver).contains(expect_string(needle)))
                        as i32,
                ))
            }
            ("java/lang/String", "equalsIgnoreCase", "(Ljava/lang/String;)Z") => {
                let other = Self::argument(r, args, 0)?;
                Ok(Value::Int(
                    (expect_ref(other).is_some()
                        && expect_string(receiver).eq_ignore_ascii_case(expect_string(other)))
                        as i32,
                ))
            }
            ("java/lang/String", "hashCode", "()I") => {
                Ok(Value::Int(stable_hash(expect_string(receiver))))
            }
            ("java/lang/String", "concat", "(Ljava/lang/String;)Ljava/lang/String;") => {
                let suffix = expect_string(Self::argument(r, args, 0)?);
                Ok(Value::Ref(Some(format!(
                    "{}{suffix}",
                    expect_string(receiver)
                ))))
            }
            ("java/lang/String", "repeat", "(I)Ljava/lang/String;") => {
                let count = expect_int(r, Self::argument(r, args, 0)?)?;
                let count = usize::try_from(count)
                    .ok()
                    .filter(|count| *count <= 1 << 20)
                    .ok_or_else(|| {
                        NativeError::Metadata(format!(
                            "{}#{} repeat count is invalid",
                            r.owner, r.name
                        ))
                    })?;
                Ok(Value::Ref(Some(expect_string(receiver).repeat(count))))
            }
            ("java/lang/String", "indexOf", "(Ljava/lang/String;)I") => {
                Ok(Value::Int(utf16_index_of(
                    expect_string(receiver),
                    expect_string(Self::argument(r, args, 0)?),
                    0,
                )))
            }
            ("java/lang/String", "indexOf", "(Ljava/lang/String;I)I") => {
                let start = expect_int(r, Self::argument(r, args, 1)?)?;
                Ok(Value::Int(utf16_index_of(
                    expect_string(receiver),
                    expect_string(Self::argument(r, args, 0)?),
                    start,
                )))
            }
            ("java/lang/String", "lastIndexOf", "(Ljava/lang/String;)I") => {
                Ok(Value::Int(utf16_last_index_of(
                    expect_string(receiver),
                    expect_string(Self::argument(r, args, 0)?),
                )))
            }
            (
                "java/lang/String",
                "replace",
                "(Ljava/lang/CharSequence;Ljava/lang/CharSequence;)Ljava/lang/String;",
            ) => {
                let from = expect_string(Self::argument(r, args, 0)?);
                let to = expect_string(Self::argument(r, args, 1)?);
                Ok(Value::Ref(Some(expect_string(receiver).replace(from, to))))
            }
            ("java/lang/String", "replace", "(CC)Ljava/lang/String;") => {
                let from = char::from_u32(expect_int(r, Self::argument(r, args, 0)?)? as u32)
                    .unwrap_or('\u{fffd}');
                let to = char::from_u32(expect_int(r, Self::argument(r, args, 1)?)? as u32)
                    .unwrap_or('\u{fffd}');
                Ok(Value::Ref(Some(
                    expect_string(receiver).replace(from, &to.to_string()),
                )))
            }
            ("java/lang/String", "trim", "()Ljava/lang/String;") => Ok(Value::Ref(Some(
                expect_string(receiver)
                    .trim_matches(|c: char| c <= '\u{0020}')
                    .to_owned(),
            ))),
            ("java/lang/String", "strip", "()Ljava/lang/String;") => {
                Ok(Value::Ref(Some(expect_string(receiver).trim().to_owned())))
            }
            ("java/lang/String", "isBlank", "()Z") => {
                Ok(Value::Int(expect_string(receiver).trim().is_empty() as i32))
            }
            ("java/lang/String", "substring", "(I)Ljava/lang/String;") => {
                let start = expect_int(r, Self::argument(r, args, 0)?)?;
                let value = expect_string(receiver);
                let units: Vec<u16> = value.encode_utf16().collect();
                let start = usize::try_from(start).map_err(|_| {
                    NativeError::Metadata(format!(
                        "{}#{} has a negative substring index",
                        r.owner, r.name
                    ))
                })?;
                let sliced = units.get(start..).ok_or_else(|| {
                    NativeError::Metadata(format!(
                        "{}#{} substring index out of bounds",
                        r.owner, r.name
                    ))
                })?;
                Ok(Value::Ref(Some(String::from_utf16_lossy(sliced))))
            }
            ("java/lang/String", "substring", "(II)Ljava/lang/String;") => {
                let start = expect_int(r, Self::argument(r, args, 0)?)?;
                let end = expect_int(r, Self::argument(r, args, 1)?)?;
                let value = expect_string(receiver);
                let units: Vec<u16> = value.encode_utf16().collect();
                let start = usize::try_from(start).map_err(|_| {
                    NativeError::Metadata(format!(
                        "{}#{} has a negative substring index",
                        r.owner, r.name
                    ))
                })?;
                let end = usize::try_from(end).map_err(|_| {
                    NativeError::Metadata(format!(
                        "{}#{} has a negative substring index",
                        r.owner, r.name
                    ))
                })?;
                let sliced = (start <= end && end <= units.len())
                    .then(|| &units[start..end])
                    .ok_or_else(|| {
                        NativeError::Metadata(format!(
                            "{}#{} substring index out of bounds",
                            r.owner, r.name
                        ))
                    })?;
                Ok(Value::Ref(Some(String::from_utf16_lossy(sliced))))
            }
            ("java/lang/String", "charAt", "(I)C") => {
                let index = expect_int(r, Self::argument(r, args, 0)?)?;
                let index = usize::try_from(index).map_err(|_| {
                    NativeError::Metadata(format!(
                        "{}#{} has a negative character index",
                        r.owner, r.name
                    ))
                })?;
                let unit = expect_string(receiver)
                    .encode_utf16()
                    .nth(index)
                    .ok_or_else(|| {
                        NativeError::Metadata(format!(
                            "{}#{} character index out of bounds",
                            r.owner, r.name
                        ))
                    })?;
                Ok(Value::Int(i32::from(unit)))
            }
            ("java/lang/String", "toLowerCase", "()Ljava/lang/String;") => Ok(Value::Ref(Some(
                expect_string(receiver).to_ascii_lowercase(),
            ))),
            ("java/lang/String", "toUpperCase", "()Ljava/lang/String;") => Ok(Value::Ref(Some(
                expect_string(receiver).to_ascii_uppercase(),
            ))),
            ("java/lang/String", "equals", "(Ljava/lang/Object;)Z") => {
                let other = Self::argument(r, args, 0)?;
                Ok(Value::Int(
                    (expect_ref(receiver) == expect_ref(other)) as i32,
                ))
            }
            ("java/lang/String", "toString", "()Ljava/lang/String;")
            | ("java/lang/Object", "toString", "()Ljava/lang/String;") => Ok(Value::Ref(Some(
                expect_ref(receiver)
                    .cloned()
                    .unwrap_or_else(|| "null".into()),
            ))),
            ("java/lang/Enum", "name", "()Ljava/lang/String;")
            | ("java/lang/Enum", "toString", "()Ljava/lang/String;") => {
                Ok(Value::Ref(Some(enum_name(receiver).to_owned())))
            }
            ("java/lang/Enum", "ordinal", "()I") => Ok(Value::Int(enum_ordinal(receiver))),
            ("java/lang/Enum", "hashCode", "()I") => {
                Ok(Value::Int(stable_hash(expect_string(receiver))))
            }
            ("java/lang/Enum", "equals", "(Ljava/lang/Object;)Z") => Ok(Value::Int(
                (expect_ref(receiver) == expect_ref(Self::argument(r, args, 0)?)) as i32,
            )),
            (
                "net/minecraft/network/chat/MutableComponent",
                "getString",
                "()Ljava/lang/String;",
            )
            | ("net/minecraft/network/chat/Component", "getString", "()Ljava/lang/String;") => {
                Ok(Value::Ref(Some(component_text(receiver))))
            }
            ("net/minecraft/network/chat/MutableComponent", "append", descriptor)
            | ("net/minecraft/network/chat/Component", "append", descriptor)
                if descriptor.ends_with(")Lnet/minecraft/network/chat/MutableComponent;") =>
            {
                let current = component_text(receiver);
                let suffix = Self::argument(r, args, 0)?;
                let suffix = if descriptor.contains("Ljava/lang/String;") {
                    expect_string(suffix).to_owned()
                } else {
                    component_text(suffix)
                };
                Ok(component_value("literal", &format!("{current}{suffix}")))
            }
            ("net/minecraft/network/chat/MutableComponent", "copy", descriptor)
            | ("net/minecraft/network/chat/Component", "copy", descriptor)
                if descriptor == "()Lnet/minecraft/network/chat/MutableComponent;" =>
            {
                Ok(component_value("literal", &component_text(receiver)))
            }
            ("java/util/UUID", "toString", "()Ljava/lang/String;") => Ok(Value::Ref(Some(
                uuid_text(receiver)
                    .ok_or_else(|| NativeError::Metadata("malformed UUID marker".into()))?
                    .to_owned(),
            ))),
            ("java/util/UUID", "equals", "(Ljava/lang/Object;)Z") => Ok(Value::Int(
                (uuid_text(receiver).is_some()
                    && uuid_text(Self::argument(r, args, 0)?)
                        .is_some_and(|other| uuid_text(receiver) == Some(other)))
                    as i32,
            )),
            ("java/util/UUID", "hashCode", "()I") => Ok(Value::Int(stable_hash(
                uuid_text(receiver)
                    .ok_or_else(|| NativeError::Metadata("malformed UUID marker".into()))?,
            ))),
            ("net/fabricmc/api/EnvType", "isClient", "()Z") => {
                Ok(Value::Int((expect_string(receiver) == "CLIENT") as i32))
            }
            ("net/fabricmc/api/EnvType", "toString", "()Ljava/lang/String;") => {
                Ok(Value::Ref(Some(enum_name(receiver).to_owned())))
            }
            ("java/util/Optional", "toString", "()Ljava/lang/String;") => {
                Ok(Value::Ref(Some(match expect_ref(receiver) {
                    Some(value) => format!(
                        "Optional[{}]",
                        value.strip_prefix("optional/").unwrap_or(value)
                    ),
                    None => "Optional.empty".to_owned(),
                })))
            }
            ("java/util/Optional", "hashCode", "()I") => {
                Ok(Value::Int(expect_ref(receiver).map_or(0, |value| {
                    stable_hash(value.strip_prefix("optional/").unwrap_or(value))
                })))
            }
            ("java/util/Optional", "equals", "(Ljava/lang/Object;)Z") => {
                let left = expect_ref(receiver)
                    .map(|value| value.strip_prefix("optional/").unwrap_or(value));
                let right = expect_ref(Self::argument(r, args, 0)?)
                    .map(|value| value.strip_prefix("optional/").unwrap_or(value));
                Ok(Value::Int((left == right) as i32))
            }
            ("java/util/Optional", "isPresent", "()Z") => {
                Ok(Value::Int(expect_ref(receiver).is_some() as i32))
            }
            ("java/util/Optional", "isEmpty", "()Z") => {
                Ok(Value::Int(expect_ref(receiver).is_none() as i32))
            }
            ("java/util/OptionalInt", "isPresent", "()Z") => {
                Ok(Value::Int(optional_int_value(receiver).is_some() as i32))
            }
            ("java/util/OptionalInt", "isEmpty", "()Z") => {
                Ok(Value::Int(optional_int_value(receiver).is_none() as i32))
            }
            ("java/util/OptionalInt", "getAsInt", "()I") => optional_int_value(receiver)
                .map_or_else(
                    || {
                        Err(NativeError::Metadata(
                            "OptionalInt.getAsInt on empty optional".into(),
                        ))
                    },
                    |value| Ok(Value::Int(value)),
                ),
            ("java/util/OptionalInt", "orElse", "(I)I") => Ok(Value::Int(
                optional_int_value(receiver).unwrap_or(expect_int(r, Self::argument(r, args, 0)?)?),
            )),
            ("java/util/OptionalInt", "toString", "()Ljava/lang/String;") => {
                Ok(Value::Ref(Some(match optional_int_value(receiver) {
                    Some(value) => format!("OptionalInt[{}]", value),
                    None => "OptionalInt.empty".into(),
                })))
            }
            ("java/util/OptionalInt", "hashCode", "()I") => {
                Ok(Value::Int(optional_int_value(receiver).unwrap_or(0)))
            }
            ("java/util/OptionalInt", "equals", "(Ljava/lang/Object;)Z") => Ok(Value::Int(
                (optional_int_value(receiver) == optional_int_value(Self::argument(r, args, 0)?))
                    as i32,
            )),
            ("java/util/OptionalLong", "isPresent", "()Z") => {
                Ok(Value::Int(optional_long_value(receiver).is_some() as i32))
            }
            ("java/util/OptionalLong", "isEmpty", "()Z") => {
                Ok(Value::Int(optional_long_value(receiver).is_none() as i32))
            }
            ("java/util/OptionalLong", "getAsLong", "()J") => optional_long_value(receiver)
                .map(long_value)
                .ok_or_else(|| {
                    NativeError::Metadata("OptionalLong.getAsLong on empty optional".into())
                }),
            ("java/util/OptionalLong", "orElse", "(J)J") => Ok(long_value(
                optional_long_value(receiver)
                    .unwrap_or(expect_long(r, Self::argument(r, args, 0)?)?),
            )),
            ("java/util/OptionalLong", "toString", "()Ljava/lang/String;") => {
                Ok(Value::Ref(Some(match optional_long_value(receiver) {
                    Some(value) => format!("OptionalLong[{}]", value),
                    None => "OptionalLong.empty".into(),
                })))
            }
            ("java/util/OptionalLong", "hashCode", "()I") => Ok(Value::Int(
                optional_long_value(receiver)
                    .map(|value| stable_hash(&format!("optional-long/{value}")))
                    .unwrap_or(0),
            )),
            ("java/util/OptionalLong", "equals", "(Ljava/lang/Object;)Z") => Ok(Value::Int(
                (optional_long_value(receiver) == optional_long_value(Self::argument(r, args, 0)?))
                    as i32,
            )),
            ("java/util/OptionalDouble", "isPresent", "()Z") => {
                Ok(Value::Int(optional_double_value(receiver).is_some() as i32))
            }
            ("java/util/OptionalDouble", "isEmpty", "()Z") => {
                Ok(Value::Int(optional_double_value(receiver).is_none() as i32))
            }
            ("java/util/OptionalDouble", "getAsDouble", "()D") => optional_double_value(receiver)
                .map(double_value)
                .ok_or_else(|| {
                    NativeError::Metadata("OptionalDouble.getAsDouble on empty optional".into())
                }),
            ("java/util/OptionalDouble", "orElse", "(D)D") => Ok(double_value(
                optional_double_value(receiver)
                    .unwrap_or(expect_float(r, Self::argument(r, args, 0)?)?),
            )),
            ("java/util/OptionalDouble", "toString", "()Ljava/lang/String;") => {
                Ok(Value::Ref(Some(match optional_double_value(receiver) {
                    Some(value) => format!("OptionalDouble[{}]", value),
                    None => "OptionalDouble.empty".into(),
                })))
            }
            ("java/util/OptionalDouble", "hashCode", "()I") => Ok(Value::Int(
                optional_double_value(receiver)
                    .map(|value| stable_hash(&format!("optional-double/{value}")))
                    .unwrap_or(0),
            )),
            ("java/util/OptionalDouble", "equals", "(Ljava/lang/Object;)Z") => Ok(Value::Int(
                (optional_double_bits(receiver)
                    == optional_double_bits(Self::argument(r, args, 0)?)) as i32,
            )),
            (
                "java/util/Optional",
                "map",
                "(Ljava/util/function/Function;)Ljava/util/Optional;",
            ) => {
                let Some(value) = expect_ref(receiver) else {
                    return Ok(Value::Ref(None));
                };
                let value = value.strip_prefix("optional/").unwrap_or(value);
                let function = Self::argument(r, args, 0)?;
                let applied = self.invoke_instance(
                    &MethodRef {
                        owner: "java/util/function/Function".into(),
                        name: "apply".into(),
                        descriptor: "(Ljava/lang/Object;)Ljava/lang/Object;".into(),
                    },
                    function,
                    &[Value::Ref(Some(value.to_owned()))],
                )?;
                Ok(match applied {
                    Value::Ref(Some(value)) => Value::Ref(Some(format!("optional/{value}"))),
                    Value::Ref(None) => Value::Ref(None),
                    value => Value::Ref(Some(format!("optional/{}", value_marker(&value)))),
                })
            }
            (
                "java/util/Optional",
                "filter",
                "(Ljava/util/function/Predicate;)Ljava/util/Optional;",
            ) => {
                let Some(_value) = expect_ref(receiver) else {
                    return Ok(Value::Ref(None));
                };
                let predicate = Self::argument(r, args, 0)?;
                let accepted = matches!(
                    predicate,
                    Value::Ref(Some(marker)) if marker.starts_with("@indy:")
                );
                Ok(if accepted {
                    receiver.clone()
                } else {
                    Value::Ref(None)
                })
            }
            ("java/util/Optional", "ifPresent", "(Ljava/util/function/Consumer;)V") => {
                if let Some(value) = expect_ref(receiver) {
                    let value = value.strip_prefix("optional/").unwrap_or(value);
                    let consumer = Self::argument(r, args, 0)?;
                    self.invoke_instance(
                        &MethodRef {
                            owner: "java/util/function/Consumer".into(),
                            name: "accept".into(),
                            descriptor: "(Ljava/lang/Object;)V".into(),
                        },
                        consumer,
                        &[Value::Ref(Some(value.to_owned()))],
                    )?;
                }
                Ok(Value::Void)
            }
            ("java/util/Optional", "get", "()Ljava/lang/Object;") => {
                Ok(match expect_ref(receiver) {
                    Some(value) => Value::Ref(Some(
                        value.strip_prefix("optional/").unwrap_or(value).to_owned(),
                    )),
                    None => receiver.clone(),
                })
            }
            ("java/util/Optional", "orElse", "(Ljava/lang/Object;)Ljava/lang/Object;") => {
                if expect_ref(receiver).is_some() {
                    self.invoke_instance(
                        &MethodRef {
                            owner: "java/util/Optional".into(),
                            name: "get".into(),
                            descriptor: "()Ljava/lang/Object;".into(),
                        },
                        receiver,
                        &[],
                    )
                } else {
                    Ok(Self::argument(r, args, 0)?.clone())
                }
            }
            (
                "java/util/Optional",
                "orElseGet",
                "(Ljava/util/function/Supplier;)Ljava/lang/Object;",
            ) => {
                if expect_ref(receiver).is_some() {
                    self.invoke_instance(
                        &MethodRef {
                            owner: "java/util/Optional".into(),
                            name: "get".into(),
                            descriptor: "()Ljava/lang/Object;".into(),
                        },
                        receiver,
                        &[],
                    )
                } else {
                    let supplier = Self::argument(r, args, 0)?;
                    self.invoke_instance(
                        &MethodRef {
                            owner: "java/util/function/Supplier".into(),
                            name: "get".into(),
                            descriptor: "()Ljava/lang/Object;".into(),
                        },
                        supplier,
                        &[],
                    )
                }
            }
            ("java/util/Optional", "or", "(Ljava/util/function/Supplier;)Ljava/util/Optional;") => {
                if expect_ref(receiver).is_some() {
                    Ok(receiver.clone())
                } else {
                    let supplier = Self::argument(r, args, 0)?;
                    let supplied = self.invoke_instance(
                        &MethodRef {
                            owner: "java/util/function/Supplier".into(),
                            name: "get".into(),
                            descriptor: "()Ljava/lang/Object;".into(),
                        },
                        supplier,
                        &[],
                    )?;
                    Ok(match supplied {
                        Value::Ref(Some(value)) if value.starts_with("optional/") => {
                            Value::Ref(Some(value))
                        }
                        Value::Ref(None) => Value::Ref(None),
                        value => optional_wrap(value),
                    })
                }
            }
            ("java/util/Optional", "orElseThrow", "()Ljava/lang/Object;") => {
                if expect_ref(receiver).is_some() {
                    self.invoke_instance(
                        &MethodRef {
                            owner: "java/util/Optional".into(),
                            name: "get".into(),
                            descriptor: "()Ljava/lang/Object;".into(),
                        },
                        receiver,
                        &[],
                    )
                } else {
                    Err(NativeError::Metadata(
                        "Optional.orElseThrow on empty optional".into(),
                    ))
                }
            }
            (
                "java/util/Optional",
                "orElseThrow",
                "(Ljava/util/function/Supplier;)Ljava/lang/Object;",
            ) => {
                if expect_ref(receiver).is_some() {
                    self.invoke_instance(
                        &MethodRef {
                            owner: "java/util/Optional".into(),
                            name: "get".into(),
                            descriptor: "()Ljava/lang/Object;".into(),
                        },
                        receiver,
                        &[],
                    )
                } else {
                    let supplier = Self::argument(r, args, 0)?;
                    self.invoke_instance(
                        &MethodRef {
                            owner: "java/util/function/Supplier".into(),
                            name: "get".into(),
                            descriptor: "()Ljava/lang/Object;".into(),
                        },
                        supplier,
                        &[],
                    )
                }
            }
            (
                "java/util/function/UnaryOperator",
                "apply",
                "(Ljava/lang/Object;)Ljava/lang/Object;",
            )
            | ("java/util/function/Function", "apply", "(Ljava/lang/Object;)Ljava/lang/Object;") => {
                Ok(Self::argument(r, args, 0)?.clone())
            }
            (
                "java/util/function/BiFunction",
                "apply",
                "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
            ) => Ok(Self::argument(r, args, 1)?.clone()),
            ("java/util/function/Consumer", "accept", "(Ljava/lang/Object;)V") => {
                if let Some(marker) = receiver_marker.as_deref()
                    && let Some(rest) = marker.strip_prefix("@fn:consumer.then:")
                    && let Some((left, right)) = rest.split_once('|')
                {
                    self.invoke_instance(r, &Value::Ref(Some(left.to_owned())), args)?;
                    self.invoke_instance(r, &Value::Ref(Some(right.to_owned())), args)?;
                }
                Ok(Value::Void)
            }
            (
                "java/util/function/BiConsumer",
                "accept",
                "(Ljava/lang/Object;Ljava/lang/Object;)V",
            ) => Ok(Value::Void),
            ("java/util/function/Predicate", "test", "(Ljava/lang/Object;)Z") => Ok(Value::Int(
                expect_ref(receiver).is_some_and(|marker| predicate_marker_value(marker)) as i32,
            )),
            ("java/util/function/Supplier", "get", "()Ljava/lang/Object;") => Ok(Value::Ref(None)),
            (
                "net/minecraft/resources/Identifier" | "net/minecraft/resources/ResourceLocation",
                name,
                descriptor,
            ) => {
                let (namespace, path) = identifier_parts(receiver);
                match (name, descriptor) {
                    ("getNamespace", "()Ljava/lang/String;") => {
                        Ok(Value::Ref(Some(namespace.to_owned())))
                    }
                    ("getPath", "()Ljava/lang/String;") => Ok(Value::Ref(Some(path.to_owned()))),
                    ("toString", "()Ljava/lang/String;") => {
                        Ok(Value::Ref(Some(format!("{namespace}:{path}"))))
                    }
                    ("toShortString", "()Ljava/lang/String;") => {
                        Ok(Value::Ref(Some(if namespace == "minecraft" {
                            path.to_owned()
                        } else {
                            format!("{namespace}/{path}")
                        })))
                    }
                    ("toLanguageKey", "()Ljava/lang/String;") => Ok(Value::Ref(Some(format!(
                        "{namespace}.{}",
                        path.replace('/', ".")
                    )))),
                    ("toLanguageKey", "(Ljava/lang/String;)Ljava/lang/String;") => {
                        Ok(Value::Ref(Some(format!(
                            "{}.{}.{}",
                            expect_string(Self::argument(r, args, 0)?),
                            namespace,
                            path.replace('/', ".")
                        ))))
                    }
                    ("withPrefix", descriptor)
                        if descriptor.ends_with("Lnet/minecraft/resources/Identifier;")
                            || descriptor
                                .ends_with("Lnet/minecraft/resources/ResourceLocation;") =>
                    {
                        Ok(identifier_value(
                            namespace,
                            &format!("{}{}", expect_string(Self::argument(r, args, 0)?), path),
                        ))
                    }
                    ("withSuffix", descriptor)
                        if descriptor.ends_with("Lnet/minecraft/resources/Identifier;")
                            || descriptor
                                .ends_with("Lnet/minecraft/resources/ResourceLocation;") =>
                    {
                        Ok(identifier_value(
                            namespace,
                            &format!("{path}{}", expect_string(Self::argument(r, args, 0)?)),
                        ))
                    }
                    ("withPath", descriptor)
                        if descriptor.ends_with("Lnet/minecraft/resources/Identifier;")
                            || descriptor
                                .ends_with("Lnet/minecraft/resources/ResourceLocation;") =>
                    {
                        let apply = MethodRef {
                            owner: "java/util/function/UnaryOperator".into(),
                            name: "apply".into(),
                            descriptor: "(Ljava/lang/Object;)Ljava/lang/Object;".into(),
                        };
                        let transformed = self.invoke_instance(
                            &apply,
                            Self::argument(r, args, 0)?,
                            &[Value::Ref(Some(path.to_owned()))],
                        )?;
                        Ok(identifier_value(namespace, expect_string(&transformed)))
                    }
                    ("hashCode", "()I") => {
                        Ok(Value::Int(stable_hash(&format!("{namespace}:{path}"))))
                    }
                    ("equals", "(Ljava/lang/Object;)Z") => {
                        let (other_namespace, other_path) =
                            identifier_parts(Self::argument(r, args, 0)?);
                        Ok(Value::Int(
                            (namespace == other_namespace && path == other_path) as i32,
                        ))
                    }
                    _ => Ok(Value::default_for(&r.descriptor)),
                }
            }
            ("net/minecraft/resources/ResourceKey", name, descriptor) => {
                let marker = expect_string(receiver);
                match (name, descriptor) {
                    (
                        "identifier",
                        "()Lnet/minecraft/resources/Identifier;"
                        | "()Lnet/minecraft/resources/ResourceLocation;",
                    ) => Ok(identifier_value("minecraft", marker)),
                    (
                        "registry",
                        "()Lnet/minecraft/resources/Identifier;"
                        | "()Lnet/minecraft/resources/ResourceLocation;",
                    ) => Ok(identifier_value("minecraft", marker)),
                    (
                        "location",
                        "()Lnet/minecraft/resources/Identifier;"
                        | "()Lnet/minecraft/resources/ResourceLocation;",
                    ) => Ok(identifier_value("minecraft", marker)),
                    ("hashCode", "()I") => Ok(Value::Int(stable_hash(marker))),
                    _ => Ok(Value::default_for(&r.descriptor)),
                }
            }
            ("net/minecraft/tags/TagKey", name, descriptor) => {
                let (registry, location) = tag_key_parts(receiver);
                match (name, descriptor) {
                    ("registry", "()Lnet/minecraft/resources/ResourceKey;") => {
                        Ok(Value::Ref(Some(registry.to_owned())))
                    }
                    (
                        "location",
                        "()Lnet/minecraft/resources/Identifier;"
                        | "()Lnet/minecraft/resources/ResourceLocation;",
                    ) => Ok(Value::Ref(Some(location.to_owned()))),
                    ("isFor", "(Lnet/minecraft/resources/ResourceKey;)Z") => Ok(Value::Int(
                        (registry == expect_string(Self::argument(r, args, 0)?)) as i32,
                    )),
                    ("hashCode", "()I") => {
                        Ok(Value::Int(stable_hash(&format!("{registry}|{location}"))))
                    }
                    ("equals", "(Ljava/lang/Object;)Z") => Ok(Value::Int(
                        (expect_string(Self::argument(r, args, 0)?) == expect_string(receiver))
                            as i32,
                    )),
                    ("toString", "()Ljava/lang/String;") => {
                        Ok(Value::Ref(Some(format!("TagKey[{registry} / {location}]"))))
                    }
                    _ => Ok(Value::default_for(&r.descriptor)),
                }
            }
            ("net/fabricmc/loader/api/FabricLoader", "isModLoaded", "(Ljava/lang/String;)Z") => {
                let id = match Self::argument(r, args, 0)? {
                    Value::Ref(Some(id)) => id,
                    _ => return Ok(Value::Int(0)),
                };
                Ok(Value::Int(self.mods.contains_key(id) as i32))
            }
            (
                "net/neoforged/fml/ModList" | "net/minecraftforge/fml/ModList",
                "isLoaded",
                "(Ljava/lang/String;)Z",
            ) => {
                let id = match Self::argument(r, args, 0)? {
                    Value::Ref(Some(id)) => id,
                    _ => return Ok(Value::Int(0)),
                };
                Ok(Value::Int(self.mods.contains_key(id) as i32))
            }
            (
                "net/neoforged/fml/ModList" | "net/minecraftforge/fml/ModList",
                "getModContainerById",
                "(Ljava/lang/String;)Ljava/util/Optional;",
            ) => {
                let id = match Self::argument(r, args, 0)? {
                    Value::Ref(Some(id)) => id,
                    _ => return Ok(Value::Ref(None)),
                };
                Ok(Value::Ref(self.mods.get(id).map(|version| {
                    format!("neoforge-mod-container/{id}/{version}")
                })))
            }
            (
                "net/neoforged/fml/ModList" | "net/minecraftforge/fml/ModList",
                "getMods",
                "()Ljava/util/List;",
            ) => {
                let values = self
                    .mods
                    .iter()
                    .map(|(id, version)| {
                        Value::Ref(Some(format!("neoforge-mod-container/{id}/{version}")))
                    })
                    .collect::<Vec<_>>();
                Ok(collection_value("list", &values))
            }
            (
                "net/fabricmc/loader/api/FabricLoader",
                "getModContainer",
                "(Ljava/lang/String;)Ljava/util/Optional;",
            ) => {
                let id = match Self::argument(r, args, 0)? {
                    Value::Ref(Some(id)) => id,
                    _ => return Ok(Value::Ref(None)),
                };
                Ok(Value::Ref(
                    self.mods
                        .get(id)
                        .map(|version| format!("mod-container/{id}/{version}")),
                ))
            }
            ("net/fabricmc/loader/api/FabricLoader", "getAllMods", "()Ljava/util/List;") => {
                let values = self
                    .mods
                    .iter()
                    .map(|(id, version)| Value::Ref(Some(format!("mod-container/{id}/{version}"))))
                    .collect::<Vec<_>>();
                Ok(collection_value("list", &values))
            }
            (
                "net/fabricmc/loader/api/FabricLoader",
                "getEntrypoints" | "getEntrypointContainers",
                "(Ljava/lang/String;)Ljava/util/List;",
            ) => {
                let kind = expect_string(Self::argument(r, args, 0)?);
                let values = self
                    .fabric_entrypoint_containers
                    .iter()
                    .filter(|entry| entry.kind == kind)
                    .map(|entry| {
                        Value::Ref(Some(format!(
                            "fabric-entrypoint/{}/{}/{}",
                            entry.kind, entry.class, entry.provider
                        )))
                    })
                    .collect::<Vec<_>>();
                Ok(collection_value("list", &values))
            }
            ("net/fabricmc/loader/api/FabricLoader", "getGameDir", "()Ljava/nio/file/Path;") => {
                Ok(Value::Ref(Some("path:/horizon/game".into())))
            }
            ("net/fabricmc/loader/api/FabricLoader", "getConfigDir", "()Ljava/nio/file/Path;") => {
                Ok(Value::Ref(Some("path:/horizon/config".into())))
            }
            ("net/fabricmc/loader/api/FabricLoader", "getModsDir", "()Ljava/nio/file/Path;") => {
                Ok(Value::Ref(Some("path:/horizon/mods".into())))
            }
            (
                "net/fabricmc/loader/api/FabricLoader",
                "getEnvironmentType",
                "()Lnet/fabricmc/api/EnvType;",
            ) => Ok(Value::Ref(Some(self.environment_type.clone()))),
            (
                "net/fabricmc/loader/api/FabricLoader",
                "getMappingResolver",
                "()Lnet/fabricmc/loader/api/MappingResolver;",
            ) => Ok(Value::Ref(Some("fabric-mapping-resolver".into()))),
            ("net/fabricmc/loader/api/FabricLoader", "isDevelopmentEnvironment", "()Z") => {
                Ok(Value::Int(self.development_environment as i32))
            }
            (
                "net/fabricmc/loader/api/MappingResolver",
                "mapClassName" | "unmapClassName",
                "(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;",
            ) => Ok(Value::Ref(Some(
                expect_string(Self::argument(r, args, 1)?).to_owned(),
            ))),
            (
                "net/fabricmc/loader/api/MappingResolver",
                "mapFieldName" | "mapMethodName",
                "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;",
            ) => Ok(Value::Ref(Some(
                expect_string(Self::argument(r, args, 2)?).to_owned(),
            ))),
            (
                "net/fabricmc/loader/api/MappingResolver",
                "mapDescriptor",
                "(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;",
            ) => Ok(Value::Ref(Some(
                expect_string(Self::argument(r, args, 1)?).to_owned(),
            ))),
            (
                "net/fabricmc/loader/api/EntrypointContainer",
                "getEntrypointClass",
                "()Ljava/lang/Class;",
            ) => Ok(Value::Ref(
                fabric_entrypoint_part(expect_ref(receiver), 2)
                    .map(|class| class.replace('/', ".")),
            )),
            (
                "net/fabricmc/loader/api/EntrypointContainer",
                "getEntrypoint",
                "()Ljava/lang/Object;",
            ) => Ok(Value::Ref(
                fabric_entrypoint_part(expect_ref(receiver), 2)
                    .map(|class| format!("fabric-entrypoint-instance/{class}")),
            )),
            (
                "net/fabricmc/loader/api/EntrypointContainer",
                "getProvider",
                "()Lnet/fabricmc/loader/api/ModContainer;",
            ) => Ok(Value::Ref(
                fabric_entrypoint_part(expect_ref(receiver), 3).map(|provider| {
                    let version = self
                        .mods
                        .get(&provider)
                        .map(String::as_str)
                        .unwrap_or("unknown");
                    format!("mod-container/{provider}/{version}")
                }),
            )),
            (
                "net/fabricmc/loader/api/ModContainer",
                "getMetadata",
                "()Lnet/fabricmc/loader/api/metadata/ModMetadata;",
            ) => Ok(Value::Ref(Some(
                expect_ref(receiver)
                    .cloned()
                    .unwrap_or_else(|| "mod-container/unknown/unknown".into())
                    .replacen("mod-container/", "metadata/", 1),
            ))),
            (
                "net/fabricmc/loader/api/ModContainer",
                "getRootPaths",
                "()Ljava/util/Collection;",
            ) => {
                let id = metadata_part(expect_ref(receiver), 1);
                Ok(collection_value(
                    "list",
                    &[path_value(&format!("/horizon/mods/{id}"))],
                ))
            }
            (
                "net/neoforged/fml/ModContainer" | "net/minecraftforge/fml/ModContainer",
                "getModId",
                "()Ljava/lang/String;",
            ) => Ok(Value::Ref(Some(metadata_part(expect_ref(receiver), 1)))),
            ("net/fabricmc/loader/api/metadata/ModMetadata", "getId", "()Ljava/lang/String;") => {
                Ok(Value::Ref(Some(metadata_part(expect_ref(receiver), 1))))
            }
            (
                "net/fabricmc/loader/api/metadata/ModMetadata",
                "getVersion",
                "()Lnet/fabricmc/loader/api/Version;",
            ) => Ok(Value::Ref(Some(metadata_part(expect_ref(receiver), 2)))),
            ("net/fabricmc/loader/api/metadata/ModMetadata", "getName", "()Ljava/lang/String;") => {
                Ok(Value::Ref(Some(metadata_part(expect_ref(receiver), 1))))
            }
            ("net/fabricmc/loader/api/metadata/ModMetadata", "getType", "()Ljava/lang/String;") => {
                Ok(Value::Ref(Some("release".into())))
            }
            (
                "net/fabricmc/loader/api/metadata/ModMetadata",
                "getDescription",
                "()Ljava/lang/String;",
            ) => Ok(Value::Ref(Some(String::new()))),
            (
                "net/fabricmc/loader/api/metadata/ModMetadata",
                "getProvides",
                "()Ljava/util/Collection;",
            ) => Ok(collection_empty("list")),
            (
                "net/fabricmc/loader/api/metadata/ModMetadata",
                "getAuthors" | "getContributors" | "getLicense",
                "()Ljava/util/Collection;",
            ) => Ok(collection_empty("list")),
            (
                "net/fabricmc/loader/api/metadata/ModMetadata",
                "getIconPath",
                "(I)Ljava/util/Optional;",
            ) => Ok(Value::Ref(None)),
            (
                "net/fabricmc/loader/api/metadata/ModMetadata",
                "getDependencies",
                "()Ljava/util/Map;",
            ) => Ok(collection_empty("map")),
            (
                "net/fabricmc/loader/api/Version",
                "getFriendlyString" | "getRaw" | "toString",
                "()Ljava/lang/String;",
            ) => {
                let value = expect_string(receiver);
                let value = value.strip_prefix("version/").unwrap_or(value);
                Ok(Value::Ref(Some(
                    value.rsplit('/').next().unwrap_or(value).to_owned(),
                )))
            }
            (
                "net/neoforged/fml/javafmlmod/FMLJavaModLoadingContext",
                "getModEventBus",
                "()Lnet/neoforged/bus/api/IEventBus;",
            ) => Ok(Value::Ref(Some("net/neoforged/bus/api/IEventBus".into()))),
            (
                "net/minecraftforge/fml/javafmlmod/FMLJavaModLoadingContext",
                "getModEventBus",
                "()Lnet/minecraftforge/eventbus/api/IEventBus;",
            ) => Ok(Value::Ref(Some(
                "net/minecraftforge/eventbus/api/IEventBus".into(),
            ))),
            (owner, "addListener", descriptor)
                if is_event_bus_owner(owner) && descriptor.ends_with("IEventBus;") =>
            {
                Ok(receiver.clone())
            }
            (owner, "register", descriptor)
                if is_event_bus_owner(owner) && descriptor.ends_with("IEventBus;") =>
            {
                Ok(receiver.clone())
            }
            (owner, "unregister", "(Ljava/lang/Object;)V") if is_event_bus_owner(owner) => {
                let target = Self::argument(r, args, 0)?.clone();
                self.invocations.retain(|invocation| {
                    let listener = (is_event_bus_owner(&invocation.method.owner)
                        && matches!(invocation.method.name.as_str(), "addListener" | "register"))
                        || (invocation.method.owner == "net/fabricmc/fabric/api/event/Event"
                            && invocation.method.name == "register");
                    !listener || !invocation.args.iter().any(|value| value == &target)
                });
                Ok(Value::Void)
            }
            (owner, "post", descriptor)
                if is_event_bus_owner(owner)
                    && descriptor.ends_with(")Z")
                    && (descriptor.contains("Lnet/neoforged/bus/api/Event;")
                        || descriptor.contains("Lnet/minecraftforge/eventbus/api/Event;")) =>
            {
                let event = Self::argument(r, args, 0)?.clone();
                Ok(Value::Int(self.post_event(event) as i32))
            }
            (
                "net/neoforged/neoforge/registries/DeferredRegister"
                | "net/minecraftforge/registries/DeferredRegister",
                "register",
                descriptor,
            ) if descriptor.ends_with("Lnet/neoforged/neoforge/registries/DeferredHolder;")
                || descriptor.ends_with("Lnet/minecraftforge/registries/RegistryObject;") =>
            {
                let id = args
                    .first()
                    .and_then(expect_ref)
                    .map_or("entry", String::as_str);
                let key = format!(
                    "{}/{}",
                    expect_ref(receiver)
                        .map_or("deferred-register/registry/default", String::as_str,),
                    id
                );
                let value = Value::Ref(Some(format!("registered/{key}")));
                self.registrations.push(RegistryEntry {
                    registry: expect_ref(receiver)
                        .map_or("deferred-register/registry/default", String::as_str)
                        .to_owned(),
                    id: id.to_owned(),
                    value: value.clone(),
                    declaration: None,
                });
                self.holders.insert(key.clone(), value.clone());
                Ok(Value::Ref(Some(format!("holder/{key}"))))
            }
            (
                "net/neoforged/neoforge/registries/DeferredRegister"
                | "net/minecraftforge/registries/DeferredRegister",
                "getEntries",
                "()Ljava/util/Collection;",
            ) => {
                let registry = expect_ref(receiver).map_or("", String::as_str);
                let entries = self
                    .registrations
                    .iter()
                    .filter(|entry| entry.registry == registry)
                    .map(|entry| {
                        Value::Ref(Some(format!("holder/{}/{}", entry.registry, entry.id)))
                    })
                    .collect::<Vec<_>>();
                Ok(collection_value("list", &entries))
            }
            ("net/neoforged/neoforge/registries/DeferredHolder", "get", "()Ljava/lang/Object;")
            | (
                "net/neoforged/neoforge/registries/DeferredHolder",
                "value",
                "()Ljava/lang/Object;",
            ) => {
                let key = expect_ref(receiver)
                    .and_then(|value| value.strip_prefix("holder/"))
                    .unwrap_or("deferred-register/registry/default/entry");
                Ok(self
                    .holders
                    .get(key)
                    .cloned()
                    .unwrap_or_else(|| Value::Ref(Some(format!("registered/{key}")))))
            }
            (
                "net/neoforged/neoforge/registries/DeferredHolder",
                "getOptional",
                "()Ljava/util/Optional;",
            ) => {
                let key = expect_ref(receiver)
                    .and_then(|value| value.strip_prefix("holder/"))
                    .unwrap_or("deferred-register/registry/default/entry");
                Ok(Value::Ref(Some(format!("optional/registered/{key}"))))
            }
            (
                "net/neoforged/neoforge/registries/DeferredHolder",
                "getKey",
                "()Lnet/minecraft/resources/ResourceKey;",
            ) => Ok(Value::Ref(Some(format!(
                "resource-key/{}",
                expect_ref(receiver).map_or("holder/", String::as_str)
            )))),
            (
                "net/neoforged/neoforge/registries/DeferredHolder",
                "getId",
                "()Lnet/minecraft/resources/Identifier;"
                | "()Lnet/minecraft/resources/ResourceLocation;",
            ) => Ok(Value::Ref(Some(format!(
                "identifier/{}",
                expect_ref(receiver).map_or("holder/", String::as_str)
            )))),
            ("net/neoforged/neoforge/registries/DeferredHolder", "isPresent", "()Z") => {
                Ok(Value::Int(1))
            }
            ("net/minecraftforge/registries/RegistryObject", "get", "()Ljava/lang/Object;")
            | (
                "net/minecraftforge/registries/RegistryObject",
                "orElse",
                "(Ljava/lang/Object;)Ljava/lang/Object;",
            )
            | (
                "net/minecraftforge/registries/RegistryObject",
                "orElseGet",
                "(Ljava/util/function/Supplier;)Ljava/lang/Object;",
            ) => {
                let key = expect_ref(receiver)
                    .and_then(|value| value.strip_prefix("holder/"))
                    .unwrap_or("deferred-register/registry/default/entry");
                Ok(self
                    .holders
                    .get(key)
                    .cloned()
                    .unwrap_or_else(|| Value::Ref(Some(format!("registered/{key}")))))
            }
            ("net/minecraftforge/registries/RegistryObject", "isPresent", "()Z") => {
                Ok(Value::Int(1))
            }
            (
                "net/minecraftforge/registries/RegistryObject",
                "getId",
                "()Lnet/minecraft/resources/ResourceLocation;",
            )
            | (
                "net/minecraftforge/registries/RegistryObject",
                "getRegistryName",
                "()Lnet/minecraft/resources/ResourceLocation;",
            ) => Ok(Value::Ref(Some(format!(
                "identifier/{}",
                expect_ref(receiver).map_or("holder/", String::as_str)
            )))),
            (
                "net/minecraftforge/registries/RegistryObject",
                "getKey",
                "()Lnet/minecraft/resources/ResourceKey;",
            ) => Ok(Value::Ref(Some(format!(
                "resource-key/{}",
                expect_ref(receiver).map_or("holder/", String::as_str)
            )))),
            (
                "net/minecraftforge/registries/RegistryObject",
                "getOptional",
                "()Ljava/util/Optional;",
            ) => Ok(Value::Ref(Some(format!(
                "optional/registered/{}",
                expect_ref(receiver)
                    .and_then(|value| value.strip_prefix("holder/"))
                    .unwrap_or("deferred-register/registry/default/entry")
            )))),
            (
                "net/minecraftforge/registries/IForgeRegistryEntry",
                "getRegistryName",
                "()Lnet/minecraft/resources/ResourceLocation;",
            ) => Ok(Value::Ref(Some(format!(
                "identifier/{}",
                expect_ref(receiver).map_or("registered/", String::as_str)
            )))),
            (
                "net/minecraftforge/registries/IForgeRegistryEntry",
                "setRegistryName",
                descriptor,
            ) if descriptor.ends_with("Lnet/minecraftforge/registries/IForgeRegistryEntry;") => {
                Ok(receiver.clone())
            }
            ("net/minecraft/core/Holder", "value", "()Ljava/lang/Object;")
            | ("net/minecraft/core/Holder$Reference", "value", "()Ljava/lang/Object;") => {
                let key = expect_ref(receiver)
                    .and_then(|value| value.strip_prefix("holder/"))
                    .unwrap_or("deferred-register/registry/default/entry");
                Ok(self
                    .holders
                    .get(key)
                    .cloned()
                    .unwrap_or_else(|| Value::Ref(Some(format!("registered/{key}")))))
            }
            ("net/minecraft/core/Holder", "isBound", "()Z")
            | ("net/minecraft/core/Holder$Reference", "isBound", "()Z") => Ok(Value::Int(
                expect_ref(receiver).is_some_and(|value| value.starts_with("holder/")) as i32,
            )),
            ("net/minecraft/core/Holder", "unwrapKey", "()Ljava/util/Optional;")
            | ("net/minecraft/core/Holder$Reference", "unwrapKey", "()Ljava/util/Optional;") => {
                let key = expect_ref(receiver)
                    .and_then(|value| value.strip_prefix("holder/"))
                    .unwrap_or("deferred-register/registry/default/entry");
                Ok(Value::Ref(Some(format!("optional/resource-key/{key}"))))
            }
            (
                "net/neoforged/neoforge/registries/DeferredRegister",
                "register",
                "(Lnet/neoforged/bus/api/IEventBus;)V",
            ) => Ok(Value::Void),
            (
                "net/neoforged/neoforge/registries/RegisterEvent",
                "register",
                "(Lnet/minecraft/resources/ResourceKey;Lnet/minecraft/resources/Identifier;Ljava/util/function/Supplier;)V",
            ) if args.len() >= 3 => {
                let registry = expect_ref(Self::argument(r, args, 0)?)
                    .map(|value| value.strip_prefix("resource-key/").unwrap_or(value))
                    .unwrap_or("minecraft:unknown")
                    .to_owned();
                let (namespace, path) = identifier_parts(Self::argument(r, args, 1)?);
                self.registrations.push(RegistryEntry {
                    registry,
                    id: format!("{namespace}:{path}"),
                    value: Self::argument(r, args, 2)?.clone(),
                    declaration: None,
                });
                Ok(Value::Void)
            }
            (
                "net/neoforged/neoforge/registries/RegisterEvent",
                "getRegistryKey",
                "()Lnet/minecraft/resources/ResourceKey;",
            ) => Ok(Value::Ref(Some(
                "net/minecraft/resources/ResourceKey".into(),
            ))),
            (
                "net/neoforged/neoforge/registries/RegisterEvent",
                "getRegistry",
                "()Lnet/minecraft/core/Registry;",
            )
            | (
                "net/neoforged/neoforge/registries/RegisterEvent",
                "getRegistry",
                "(Lnet/minecraft/resources/ResourceKey;)Lnet/minecraft/core/Registry;",
            ) => Ok(Value::Ref(Some("net/minecraft/core/Registry".into()))),
            (
                "net/neoforged/neoforge/network/event/RegisterPayloadHandlersEvent",
                "registrar",
                "(Ljava/lang/String;)Lnet/neoforged/neoforge/network/registration/PayloadRegistrar;",
            ) => Ok(Value::Ref(Some(
                "net/neoforged/neoforge/network/registration/PayloadRegistrar".into(),
            ))),
            ("net/neoforged/neoforge/network/registration/PayloadRegistrar", _, descriptor)
                if descriptor.ends_with(
                    "Lnet/neoforged/neoforge/network/registration/PayloadRegistrar;",
                ) =>
            {
                if matches!(
                    r.name.as_str(),
                    "playToServer"
                        | "playToClient"
                        | "playBidirectional"
                        | "configurationToServer"
                        | "configurationToClient"
                        | "configurationBidirectional"
                ) && args.len() >= 3
                {
                    self.payload_registrations.push(PayloadRegistration {
                        direction: r.name.clone(),
                        payload_type: args[0].clone(),
                        codec: args[1].clone(),
                        handler: args[2].clone(),
                    });
                }
                Ok(receiver.clone())
            }
            // Forge's legacy SimpleChannel carries an id plus separate
            // encoder/decoder callbacks. Record the message class, encoder,
            // and handler through the same bounded bidirectional payload
            // marker; byte encoding, channel ownership, and sends remain a
            // game-owned boundary.
            ("net/minecraftforge/network/simple/SimpleChannel", "registerMessage", descriptor)
                if descriptor.ends_with("Lnet/minecraftforge/network/simple/SimpleChannel;")
                    && args.len() >= 5 =>
            {
                self.payload_registrations.push(PayloadRegistration {
                    direction: "playBidirectional".into(),
                    payload_type: args[1].clone(),
                    codec: args[2].clone(),
                    handler: args[4].clone(),
                });
                Ok(receiver.clone())
            }
            _ => Ok(Value::default_for(&r.descriptor)),
        }
    }
}

fn expect_int(r: &MethodRef, value: &Value) -> Result<i32, NativeError> {
    match value {
        Value::Int(value) => Ok(*value),
        _ => Err(NativeError::Metadata(format!(
            "{}#{} expects an integer",
            r.owner, r.name
        ))),
    }
}

fn expect_ref(value: &Value) -> Option<&String> {
    match value {
        Value::Ref(value) => value.as_ref(),
        _ => None,
    }
}

fn expect_string(value: &Value) -> &str {
    expect_ref(value).map_or("", String::as_str)
}

fn path_text(value: &Value) -> &str {
    expect_string(value)
        .strip_prefix("path:")
        .unwrap_or_else(|| expect_string(value))
}

fn path_value(value: &str) -> Value {
    Value::Ref(Some(format!("path:{value}")))
}

fn path_join(base: &str, child: &str) -> String {
    if child.starts_with('/') || base.is_empty() {
        child.to_owned()
    } else {
        format!("{}/{}", base.trim_end_matches('/'), child)
    }
}

fn class_descriptor(value: &Value) -> Option<String> {
    let name = expect_string(value);
    if name.is_empty() {
        return None;
    }
    if name.starts_with('[') {
        return Some(name.to_owned());
    }
    let primitive = match name {
        "boolean" | "Z" => Some("Z"),
        "byte" | "B" => Some("B"),
        "char" | "C" => Some("C"),
        "short" | "S" => Some("S"),
        "int" | "I" => Some("I"),
        "long" | "J" => Some("J"),
        "float" | "F" => Some("F"),
        "double" | "D" => Some("D"),
        "void" | "V" => Some("V"),
        _ => None,
    };
    primitive
        .map(str::to_owned)
        .or_else(|| Some(format!("L{};", name.replace('.', "/"))))
}

fn class_binary_name(value: &Value) -> String {
    let name = expect_string(value);
    if name.starts_with('[') {
        // JVM array descriptors use slash-separated object names. Preserve
        // the descriptor shape while matching Class#getName's dotted form.
        return name.replace('/', ".");
    }
    name.replace('/', ".")
}

fn class_is_primitive(value: &Value) -> bool {
    matches!(
        class_binary_name(value).as_str(),
        "boolean"
            | "byte"
            | "char"
            | "short"
            | "int"
            | "long"
            | "float"
            | "double"
            | "void"
            | "Z"
            | "B"
            | "C"
            | "S"
            | "I"
            | "J"
            | "F"
            | "D"
            | "V"
    )
}

fn class_is_array(value: &Value) -> bool {
    class_binary_name(value).starts_with('[')
}

fn class_is_assignable(target: &str, source: &str) -> bool {
    !target.is_empty()
        && !source.is_empty()
        && (target == source || target == "java.lang.Object" || target == "java.lang.Cloneable")
}

fn builder_contents(value: &Value) -> Option<&str> {
    let value = expect_ref(value)?;
    if matches!(
        value
            .split_once('#')
            .map_or(value.as_str(), |(class, _)| class),
        "java/lang/StringBuilder" | "java/lang/StringBuffer"
    ) {
        Some("")
    } else {
        value.strip_prefix("string-builder/")
    }
}

fn builder_append_text(r: &MethodRef, args: &[Value]) -> Result<String, NativeError> {
    let value = StdHost::argument(r, args, 0)?;
    let text = match r.descriptor.as_str() {
        "(Ljava/lang/String;)Ljava/lang/StringBuilder;"
        | "(Ljava/lang/String;)Ljava/lang/StringBuffer;" => match value {
            Value::Ref(Some(value)) => value.clone(),
            Value::Ref(None) => "null".into(),
            _ => String::new(),
        },
        "(Ljava/lang/Object;)Ljava/lang/StringBuilder;"
        | "(Ljava/lang/Object;)Ljava/lang/StringBuffer;" => match value {
            Value::Ref(Some(value)) => value.clone(),
            Value::Ref(None) => "null".into(),
            Value::Int(value) => value.to_string(),
            Value::Void => "void".into(),
        },
        "(I)Ljava/lang/StringBuilder;" | "(I)Ljava/lang/StringBuffer;" => {
            expect_int(r, value)?.to_string()
        }
        "(Z)Ljava/lang/StringBuilder;" | "(Z)Ljava/lang/StringBuffer;" => {
            match expect_int(r, value)? {
                0 => "false".into(),
                _ => "true".into(),
            }
        }
        "(C)Ljava/lang/StringBuilder;" | "(C)Ljava/lang/StringBuffer;" => {
            char::from_u32(expect_int(r, value)? as u32)
                .unwrap_or('\u{fffd}')
                .to_string()
        }
        _ => value_marker(value),
    };
    Ok(text)
}

fn identifier_value(namespace: &str, path: &str) -> Value {
    Value::Ref(Some(format!("identifier/{namespace}:{path}")))
}

fn component_value(kind: &str, text: &str) -> Value {
    Value::Ref(Some(format!("component/{kind}/{text}")))
}

fn component_text(value: &Value) -> String {
    let raw = expect_ref(value).map_or("", |value| value.as_str());
    raw.strip_prefix("component/literal/")
        .or_else(|| raw.strip_prefix("component/translatable/"))
        .unwrap_or(raw)
        .to_owned()
}

fn identifier_parse(value: &str) -> Value {
    let (namespace, path) = value.split_once(':').unwrap_or(("minecraft", value));
    if namespace.is_empty() || path.is_empty() {
        Value::Ref(None)
    } else {
        identifier_value(namespace, path)
    }
}

fn identifier_parts(value: &Value) -> (&str, &str) {
    let raw = expect_string(value);
    let value = raw.strip_prefix("identifier/").unwrap_or(raw);
    value.split_once(':').unwrap_or(("minecraft", value))
}

fn tag_key_value(registry: &str, location: &str) -> Value {
    Value::Ref(Some(format!("tag-key/{registry}|{location}")))
}

fn tag_key_parts(value: &Value) -> (&str, &str) {
    expect_string(value)
        .strip_prefix("tag-key/")
        .and_then(|value| value.split_once('|'))
        .unwrap_or(("", ""))
}

fn stable_hash(value: &str) -> i32 {
    value.encode_utf16().fold(0_i32, |hash, unit| {
        hash.wrapping_mul(31).wrapping_add(i32::from(unit))
    })
}

fn utf16_index_of(haystack: &str, needle: &str, from: i32) -> i32 {
    let haystack = haystack.encode_utf16().collect::<Vec<_>>();
    let needle = needle.encode_utf16().collect::<Vec<_>>();
    let start = from.max(0) as usize;
    if needle.is_empty() {
        return start.min(haystack.len()) as i32;
    }
    haystack
        .get(start..)
        .and_then(|tail| {
            tail.windows(needle.len())
                .position(|window| window == needle)
        })
        .map_or(-1, |index| (start + index) as i32)
}

fn utf16_last_index_of(haystack: &str, needle: &str) -> i32 {
    let haystack = haystack.encode_utf16().collect::<Vec<_>>();
    let needle = needle.encode_utf16().collect::<Vec<_>>();
    if needle.is_empty() {
        return haystack.len() as i32;
    }
    haystack
        .windows(needle.len())
        .rposition(|window| window == needle)
        .map_or(-1, |index| index as i32)
}

fn event_is_cancelled(value: &Value) -> bool {
    expect_ref(value).is_some_and(|marker| {
        marker.starts_with("cancelled/")
            || marker.starts_with("event/cancelled/")
            || marker.starts_with("@cancelled:")
    })
}

/// Forge 1.20-era event-bus names are ABI-compatible with the bounded
/// NeoForge surface we expose. Keep the alias explicit so newer Forge-only
/// APIs do not accidentally become supported by prefix matching.
fn is_event_bus_owner(owner: &str) -> bool {
    matches!(
        owner,
        "net/neoforged/bus/api/IEventBus" | "net/minecraftforge/eventbus/api/IEventBus"
    )
}

fn uuid_value(text: &str) -> Result<Value, NativeError> {
    let canonical = canonical_uuid(text)?;
    Ok(Value::Ref(Some(format!("uuid/{canonical}"))))
}

fn uuid_text(value: &Value) -> Option<&str> {
    expect_ref(value)?.strip_prefix("uuid/")
}

fn canonical_uuid(text: &str) -> Result<String, NativeError> {
    let bytes = text.as_bytes();
    if bytes.len() != 36
        || ![8, 13, 18, 23].iter().all(|&index| bytes[index] == b'-')
        || bytes
            .iter()
            .enumerate()
            .any(|(index, byte)| ![8, 13, 18, 23].contains(&index) && !byte.is_ascii_hexdigit())
    {
        return Err(NativeError::Metadata(format!(
            "invalid UUID string {text:?}"
        )));
    }
    Ok(text.to_ascii_lowercase())
}

fn fabric_entrypoint_part(value: Option<&String>, index: usize) -> Option<String> {
    let marker = value?.strip_prefix("fabric-entrypoint/")?;
    marker.split('/').nth(index - 1).map(str::to_owned)
}

fn metadata_part(value: Option<&String>, index: usize) -> String {
    value
        .and_then(|value| value.split('/').nth(index))
        .unwrap_or("unknown")
        .to_owned()
}

fn enum_name(value: &Value) -> &str {
    expect_string(value)
        .rsplit_once('/')
        .map_or_else(|| expect_string(value), |(_, name)| name)
}

fn enum_ordinal(value: &Value) -> i32 {
    match enum_name(value) {
        "CLIENT" => 0,
        "SERVER" => 1,
        _ => 0,
    }
}

fn value_marker(value: &Value) -> String {
    match value {
        Value::Int(value) => format!("i{value}"),
        Value::Ref(Some(value)) => format!("r{}", value.replace('\\', "\\\\").replace(',', "\\,")),
        Value::Ref(None) => "null".to_owned(),
        Value::Void => "void".to_owned(),
    }
}

fn is_throwable_owner(owner: &str) -> bool {
    owner == "java/lang/Throwable"
        || owner.ends_with("Exception")
        || owner.ends_with("Error")
        || owner.ends_with("Throwable")
}

fn is_throwable_marker(marker: &str) -> bool {
    marker
        .split_once('#')
        .is_some_and(|(owner, _)| is_throwable_owner(owner))
}

fn capability_text(value: &Value) -> String {
    match value {
        Value::Int(value) => value.to_string(),
        Value::Ref(Some(value)) => value.replace("\\,", ",").replace("\\\\", "\\"),
        Value::Ref(None) => "".to_owned(),
        Value::Void => "void".to_owned(),
    }
}

// Forge and NeoForge kept the same bounded capability contracts while moving
// the classes between packages. Treat the owner names as aliases, preserving
// the marker semantics without fabricating a live capability implementation.
fn is_capability_manager_owner(owner: &str) -> bool {
    matches!(
        owner,
        "net/neoforged/neoforge/capabilities/CapabilityManager"
            | "net/minecraftforge/common/capabilities/CapabilityManager"
    )
}

fn is_capability_owner(owner: &str) -> bool {
    matches!(
        owner,
        "net/neoforged/neoforge/capabilities/Capability"
            | "net/minecraftforge/common/capabilities/Capability"
    )
}

fn is_lazy_optional_owner(owner: &str) -> bool {
    matches!(
        owner,
        "net/neoforged/neoforge/common/util/LazyOptional"
            | "net/minecraftforge/common/util/LazyOptional"
    )
}

fn optional_wrap(value: Value) -> Value {
    match value {
        Value::Ref(Some(value)) if value.starts_with("optional/") => Value::Ref(Some(value)),
        Value::Ref(Some(value)) => Value::Ref(Some(format!("optional/{value}"))),
        Value::Ref(None) | Value::Void => Value::Ref(None),
        value => Value::Ref(Some(format!("optional/{}", value_marker(&value)))),
    }
}

fn optional_int_value(value: &Value) -> Option<i32> {
    expect_ref(value)
        .and_then(|marker| marker.strip_prefix("optional-int/"))
        .and_then(|value| value.parse().ok())
}

fn optional_long_value(value: &Value) -> Option<i64> {
    expect_ref(value)
        .and_then(|marker| marker.strip_prefix("optional-long/"))
        .and_then(|value| value.parse().ok())
}

fn optional_double_value(value: &Value) -> Option<f64> {
    expect_ref(value)
        .and_then(|marker| marker.strip_prefix("optional-double/"))
        .and_then(|value| value.parse().ok())
}

fn optional_double_bits(value: &Value) -> Option<u64> {
    optional_double_value(value).map(f64::to_bits)
}

fn collection_empty(kind: &str) -> Value {
    Value::Ref(Some(kind.to_owned()))
}

fn value_text(value: &Value) -> String {
    match value {
        Value::Ref(Some(value)) => {
            if let Some(value) = value.strip_prefix("integer:") {
                return value.to_owned();
            }
            if let Some(value) = value.strip_prefix("boolean:") {
                return value.to_owned();
            }
            value.clone()
        }
        Value::Ref(None) => "null".into(),
        Value::Int(value) => value.to_string(),
        Value::Void => "void".into(),
    }
}

/// Implement the small deterministic `String.format` subset used by loader diagnostics.
///
/// Supported conversions are `%s`, `%d`, `%f`, `%b`, `%c`, `%%`, and `%n` with
/// implicit, sequential arguments. Flags, widths, precisions, argument indexes,
/// and locale-sensitive conversions return an explicit metadata error.
fn format_java_string_call<H: Host>(
    runtime: &Runtime<'_, H>,
    r: &MethodRef,
    args: &[Value],
) -> Result<Value, NativeError> {
    let (format, values) = match r.descriptor.as_str() {
        "(Ljava/lang/String;[Ljava/lang/Object;)Ljava/lang/String;" => (
            expect_string(
                args.first()
                    .ok_or_else(|| NativeError::Metadata("String.format missing format".into()))?,
            ),
            args.get(1),
        ),
        "(Ljava/util/Locale;Ljava/lang/String;[Ljava/lang/Object;)Ljava/lang/String;" => (
            expect_string(
                args.get(1)
                    .ok_or_else(|| NativeError::Metadata("String.format missing format".into()))?,
            ),
            args.get(2),
        ),
        _ => {
            return Err(NativeError::Metadata(format!(
                "unsupported String.format descriptor {}",
                r.descriptor
            )));
        }
    };
    let values =
        values.ok_or_else(|| NativeError::Metadata("String.format missing arguments".into()))?;
    let values = runtime
        .array(values)
        .map_err(|_| NativeError::Metadata("String.format arguments must be an Object[]".into()))?;
    let mut output = String::with_capacity(format.len());
    let mut next = 0usize;
    let bytes = format.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'%' {
            let ch = format[i..].chars().next().expect("valid format offset");
            output.push(ch);
            i += ch.len_utf8();
            continue;
        }
        i += 1;
        let conversion = *bytes
            .get(i)
            .ok_or_else(|| NativeError::Metadata("unterminated String.format conversion".into()))?
            as char;
        i += 1;
        match conversion {
            '%' => output.push('%'),
            'n' => output.push('\n'),
            's' => output.push_str(&value_text(values.get(next).ok_or_else(|| {
                NativeError::Metadata("String.format missing %s argument".into())
            })?)),
            'd' => {
                let value = values.get(next).ok_or_else(|| {
                    NativeError::Metadata("String.format missing %d argument".into())
                })?;
                let value = boxed_integer(value).ok_or_else(|| {
                    NativeError::Metadata("String.format %d requires an integer".into())
                })?;
                output.push_str(&value.to_string());
            }
            'f' => {
                let value = values.get(next).ok_or_else(|| {
                    NativeError::Metadata("String.format missing %f argument".into())
                })?;
                let value = boxed_float(value).ok_or_else(|| {
                    NativeError::Metadata("String.format %f requires a floating-point value".into())
                })?;
                output.push_str(&format_java_float(value));
            }
            'b' => {
                let value = values.get(next).ok_or_else(|| {
                    NativeError::Metadata("String.format missing %b argument".into())
                })?;
                output.push_str(if matches!(value, Value::Ref(None)) {
                    "false"
                } else {
                    "true"
                });
            }
            'c' => {
                let value = values.get(next).ok_or_else(|| {
                    NativeError::Metadata("String.format missing %c argument".into())
                })?;
                let value = boxed_integer(value)
                    .and_then(|v| char::from_u32(v as u32))
                    .ok_or_else(|| {
                        NativeError::Metadata("String.format %c requires a character".into())
                    })?;
                output.push(value);
            }
            other => {
                return Err(NativeError::Metadata(format!(
                    "unsupported String.format conversion %{other}"
                )));
            }
        }
        if !matches!(conversion, '%' | 'n') {
            next += 1;
        }
    }
    Ok(Value::Ref(Some(output)))
}

fn boxed_integer(value: &Value) -> Option<i32> {
    match value {
        Value::Int(value) => Some(*value),
        Value::Ref(Some(value)) => value
            .strip_prefix("integer:")
            .and_then(|value| value.parse::<i32>().ok()),
        _ => None,
    }
}

fn java_code_point(value: &Value) -> Option<char> {
    let code_point = boxed_integer(value)?;
    (0..=0x10ffff)
        .contains(&code_point)
        .then(|| char::from_u32(code_point as u32))
        .flatten()
}

fn boxed_character(value: &Value) -> Option<i32> {
    match value {
        // Java `char` is an unsigned UTF-16 code unit, so isolated surrogates
        // are valid boxed values even though they are not Rust scalar values.
        Value::Int(value) => (0..=0xffff).contains(value).then_some(*value),
        Value::Ref(Some(value)) => value
            .strip_prefix("character:")
            .and_then(|value| value.parse::<i32>().ok())
            .and_then(|value| (0..=0xffff).contains(&value).then_some(value)),
        _ => None,
    }
}

/// Read any of the host's boxed numeric markers as a Java `double`-like value.
/// The native probe does not model primitive wide values separately, so all
/// conversions pass through this helper before applying Java's narrowing cast.
fn boxed_number(value: &Value) -> Option<f64> {
    match value {
        Value::Int(value) => Some(f64::from(*value)),
        Value::Ref(Some(value)) => value
            .strip_prefix("integer:")
            .or_else(|| value.strip_prefix("byte:"))
            .or_else(|| value.strip_prefix("short:"))
            .or_else(|| value.strip_prefix("long:"))
            .or_else(|| value.strip_prefix("double:"))
            .or_else(|| value.strip_prefix("float:"))
            .and_then(|value| value.parse::<f64>().ok()),
        _ => None,
    }
}

fn number_int_value(value: &Value) -> Option<i32> {
    match value {
        Value::Ref(Some(value)) if value.starts_with("long:") => value
            .strip_prefix("long:")?
            .parse::<i64>()
            .ok()
            .map(|v| v as i32),
        _ => boxed_number(value).map(|v| v as i32),
    }
}

fn number_long_value(value: &Value) -> Option<i64> {
    match value {
        Value::Ref(Some(value)) if value.starts_with("long:") => {
            value.strip_prefix("long:")?.parse::<i64>().ok()
        }
        _ => boxed_number(value).map(|v| v as i64),
    }
}

fn number_short_value(value: &Value) -> Option<i32> {
    number_long_value(value).map(|v| (v as i16) as i32)
}

fn number_byte_value(value: &Value) -> Option<i32> {
    number_long_value(value).map(|v| (v as i8) as i32)
}

fn parse_java_byte(value: &Value) -> Result<i8, std::num::ParseIntError> {
    expect_string(value).trim().parse::<i8>()
}

fn parse_java_short(value: &Value) -> Result<i16, NativeError> {
    expect_string(value).parse::<i16>().map_err(|_| {
        NativeError::Metadata("invalid short argument for java/lang/Short.parseShort".into())
    })
}

fn short_radix(r: &MethodRef, value: &Value) -> Result<u32, NativeError> {
    let radix = expect_int(r, value)?;
    if !(2..=36).contains(&radix) {
        return Err(NativeError::Metadata(format!(
            "{} received an invalid radix",
            r.name
        )));
    }
    Ok(radix as u32)
}

fn boxed_byte(value: &Value) -> Option<i8> {
    match value {
        Value::Int(value) => i8::try_from(*value).ok(),
        Value::Ref(Some(value)) => value
            .strip_prefix("byte:")
            .and_then(|value| value.parse::<i8>().ok()),
        _ => None,
    }
}

fn boxed_short(value: &Value) -> Option<i16> {
    match value {
        Value::Int(value) => i16::try_from(*value).ok(),
        Value::Ref(Some(value)) => value
            .strip_prefix("short:")
            .and_then(|value| value.parse::<i16>().ok()),
        _ => None,
    }
}

fn parse_java_long(value: &Value) -> Result<i64, NativeError> {
    expect_string(value).trim().parse::<i64>().map_err(|_| {
        NativeError::Metadata("invalid long argument for java/lang/Long.parseLong".into())
    })
}

fn long_value(value: i64) -> Value {
    Value::Ref(Some(format!("long:{value}")))
}

fn boxed_long(value: &Value) -> Option<i64> {
    match value {
        Value::Int(value) => Some(i64::from(*value)),
        Value::Ref(Some(value)) => value
            .strip_prefix("long:")
            .and_then(|value| value.parse::<i64>().ok()),
        _ => None,
    }
}

fn expect_long(r: &MethodRef, value: &Value) -> Result<i64, NativeError> {
    boxed_long(value).ok_or_else(|| {
        NativeError::Metadata(format!("{}#{} expects a long value", r.owner, r.name))
    })
}

fn boxed_boolean(value: &Value) -> Option<bool> {
    match value {
        Value::Int(value) => Some(*value != 0),
        Value::Ref(Some(value)) => value
            .strip_prefix("boolean:")
            .and_then(|value| value.parse::<bool>().ok()),
        _ => None,
    }
}

fn parse_java_float(value: &Value) -> Result<f64, NativeError> {
    let text = expect_string(value).trim();
    let normalized = match text {
        "Infinity" | "+Infinity" => "inf",
        "-Infinity" => "-inf",
        other => other,
    };
    normalized
        .parse::<f64>()
        .map_err(|_| NativeError::Metadata(format!("invalid floating-point argument: {text}")))
}

fn double_value(value: f64) -> Value {
    Value::Ref(Some(format!("double:{value}")))
}

fn float_value(value: f64) -> Value {
    Value::Ref(Some(format!("float:{}", value as f32)))
}

fn boxed_float(value: &Value) -> Option<f64> {
    match value {
        Value::Int(value) => Some(f64::from(*value)),
        Value::Ref(Some(value)) => value
            .strip_prefix("double:")
            .or_else(|| value.strip_prefix("float:"))
            .and_then(|value| value.parse::<f64>().ok()),
        _ => None,
    }
}

fn expect_float(r: &MethodRef, value: &Value) -> Result<f64, NativeError> {
    boxed_float(value).ok_or_else(|| {
        NativeError::Metadata(format!(
            "{}#{} expects a floating-point value",
            r.owner, r.name
        ))
    })
}

fn format_integer_us(value: i32) -> String {
    let negative = value < 0;
    let digits = value.unsigned_abs().to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index != 0 && (digits.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    if negative {
        format!("-{grouped}")
    } else {
        grouped
    }
}

fn format_java_float(value: f64) -> String {
    if value.is_nan() {
        "NaN".into()
    } else if value == f64::INFINITY {
        "Infinity".into()
    } else if value == f64::NEG_INFINITY {
        "-Infinity".into()
    } else {
        value.to_string()
    }
}

fn collection_value(kind: &str, values: &[Value]) -> Value {
    let values = values
        .iter()
        .map(value_marker)
        .collect::<Vec<_>>()
        .join(",");
    Value::Ref(Some(if values.is_empty() {
        kind.to_owned()
    } else {
        format!("{kind}/{values}")
    }))
}

fn map_entry_value(key: &Value, value: &Value) -> Value {
    Value::Ref(Some(format!(
        "map-entry/{},{}",
        value_marker(key),
        value_marker(value)
    )))
}

fn map_entry_parts(value: &Value) -> Result<(Value, Value), NativeError> {
    let marker = expect_ref(value)
        .and_then(|marker| marker.strip_prefix("map-entry/"))
        .ok_or_else(|| NativeError::Metadata("expected a native map entry marker".into()))?;
    let parts = split_capture_markers(marker)?;
    if parts.len() != 2 {
        return Err(NativeError::Metadata(
            "map entry marker must contain key and value".into(),
        ));
    }
    Ok((
        parse_value_marker(&parts[0])?,
        parse_value_marker(&parts[1])?,
    ))
}

fn is_collection_owner(owner: &str) -> bool {
    matches!(
        owner,
        "java/util/Collection"
            | "java/util/List"
            | "java/util/Set"
            | "java/util/AbstractCollection"
            | "java/util/AbstractList"
            | "java/util/AbstractSet"
            | "java/util/ArrayList"
            | "java/util/HashSet"
            | "java/util/LinkedHashSet"
            | "java/util/Map"
            | "java/util/AbstractMap"
            | "java/util/HashMap"
            | "java/util/LinkedHashMap"
            | "java/util/Properties"
    )
}

fn is_list_owner(owner: &str) -> bool {
    matches!(
        owner,
        "java/util/List" | "java/util/AbstractList" | "java/util/ArrayList"
    )
}

fn is_map_owner(owner: &str) -> bool {
    matches!(
        owner,
        "java/util/Map"
            | "java/util/AbstractMap"
            | "java/util/HashMap"
            | "java/util/LinkedHashMap"
            | "java/util/Properties"
    )
}

fn collection_values(value: &Value) -> Result<Vec<String>, NativeError> {
    let marker = expect_string(value);
    let (kind, values) = marker.split_once('/').unwrap_or((marker, ""));
    if !matches!(kind, "list" | "set" | "map") {
        return Err(NativeError::Metadata(format!(
            "expected a native collection marker, got {marker:?}"
        )));
    }
    if values.is_empty() {
        return Ok(Vec::new());
    }
    split_capture_markers(values)
}

fn host_collection_values(host: &StdHost, value: &Value) -> Result<Vec<String>, NativeError> {
    host.mutable_values(value)
        .map_or_else(|| collection_values(value), Ok)
}

fn parse_collection_value(value: &str) -> Result<Value, NativeError> {
    parse_value_marker(value)
}

fn collection_text(value: &Value) -> Result<String, NativeError> {
    let values = collection_values(value)?;
    let marker = expect_string(value);
    let kind = marker.split('/').next().unwrap_or(marker);
    let values = values
        .iter()
        .map(|value| match parse_collection_value(value) {
            Ok(Value::Ref(Some(value))) => value,
            Ok(Value::Ref(None)) => "null".into(),
            Ok(Value::Int(value)) => value.to_string(),
            Ok(Value::Void) => "void".into(),
            Err(_) => value.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    Ok(match kind {
        "map" => format!("{{{values}}}"),
        _ => format!("[{values}]"),
    })
}

fn instance_field_key(receiver: &Value, field: &MethodRef) -> Option<String> {
    expect_ref(receiver).map(|receiver| {
        format!(
            "{receiver}|{}#{}{}",
            field.owner, field.name, field.descriptor
        )
    })
}

fn expect_array_marker(value: &Value) -> Result<&str, NativeError> {
    match value {
        Value::Ref(Some(marker)) if marker.starts_with("array/") => Ok(marker),
        _ => Err(NativeError::Metadata(
            "expected a native array marker".into(),
        )),
    }
}

fn array_length(value: i32) -> Result<usize, NativeError> {
    usize::try_from(value)
        .ok()
        .filter(|length| *length <= 1 << 20)
        .ok_or_else(|| NativeError::Metadata("array length is invalid".into()))
}

fn array_index(value: i32) -> Result<usize, NativeError> {
    usize::try_from(value).map_err(|_| NativeError::Metadata("array index is negative".into()))
}

#[derive(Debug, Clone)]
pub struct Method {
    pub name: String,
    pub descriptor: String,
    pub access: u16,
    pub code: Option<Vec<u8>>,
    pub max_locals: u16,
}
impl Method {
    pub fn is_static(&self) -> bool {
        self.access & 8 != 0
    }
}

#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub descriptor: String,
    pub access: u16,
}

impl Field {
    pub fn is_static(&self) -> bool {
        self.access & 8 != 0
    }
}

#[derive(Debug, Clone)]
pub struct ClassFile {
    pub name: String,
    pub super_name: String,
    pool: Vec<Cp>,
    bootstrap_methods: Vec<BootstrapMethod>,
    pub methods: Vec<Method>,
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone)]
struct BootstrapMethod {
    method_ref: u16,
    arguments: Vec<u16>,
}

impl ClassFile {
    pub fn parse(bytes: &[u8]) -> Result<Self, NativeError> {
        let mut r = Reader::new(bytes);
        if r.u4()? != 0xCAFE_BABE {
            return Err(NativeError::InvalidClass("bad magic"));
        }
        r.skip(4)?;
        let pool = read_pool(&mut r)?;
        let _access = r.u2()?;
        let this = r.u2()?;
        let super_index = r.u2()?;
        let name = class_name(&pool, this)?;
        let super_name = class_name(&pool, super_index)?;
        let interface_count = r.u2()? as usize;
        r.skip(interface_count * 2)?;
        let fields = read_fields(&mut r, &pool)?;
        let methods = read_methods(&mut r, &pool)?;
        let bootstrap_methods = read_bootstrap_methods(&mut r, &pool)?;
        Ok(Self {
            name,
            super_name,
            pool,
            bootstrap_methods,
            methods,
            fields,
        })
    }

    pub fn method(&self, name: &str, desc: &str) -> Option<&Method> {
        self.methods
            .iter()
            .find(|m| m.name == name && m.descriptor == desc)
    }

    fn method_ref(&self, index: u16) -> Result<MethodRef, NativeError> {
        let (class, nt) = match self.pool.get(index as usize) {
            Some(Cp::MethodRef { class, nt }) | Some(Cp::InterfaceMethodRef { class, nt }) => {
                (*class, *nt)
            }
            other => return Err(NativeError::InvalidConstant(index, format!("{other:?}"))),
        };
        let (name, descriptor) = name_type(&self.pool, nt)?;
        Ok(MethodRef {
            owner: class_name(&self.pool, class)?,
            name,
            descriptor,
        })
    }

    fn field_ref(&self, index: u16) -> Result<MethodRef, NativeError> {
        let (class, nt) = match self.pool.get(index as usize) {
            Some(Cp::FieldRef { class, nt }) => (*class, *nt),
            other => return Err(NativeError::InvalidConstant(index, format!("{other:?}"))),
        };
        let (name, descriptor) = name_type(&self.pool, nt)?;
        Ok(MethodRef {
            owner: class_name(&self.pool, class)?,
            name,
            descriptor,
        })
    }

    fn invoke_dynamic_descriptor(&self, index: u16) -> Result<String, NativeError> {
        let nt = match self.pool.get(index as usize) {
            Some(Cp::InvokeDynamic { nt, .. }) => *nt,
            other => return Err(NativeError::InvalidConstant(index, format!("{other:?}"))),
        };
        Ok(name_type(&self.pool, nt)?.1)
    }

    fn invoke_dynamic_target(&self, index: u16) -> Result<Option<MethodRef>, NativeError> {
        let bootstrap = match self.pool.get(index as usize) {
            Some(Cp::InvokeDynamic { bootstrap, .. }) => *bootstrap as usize,
            other => return Err(NativeError::InvalidConstant(index, format!("{other:?}"))),
        };
        let Some(bootstrap) = self.bootstrap_methods.get(bootstrap) else {
            return Ok(None);
        };
        let implementation = bootstrap.arguments.get(1).copied().filter(|index| {
            matches!(
                self.pool.get(*index as usize),
                Some(Cp::MethodHandle { .. })
            )
        });
        let handle_index = implementation.unwrap_or(bootstrap.method_ref);
        let handle = match self.pool.get(handle_index as usize) {
            Some(Cp::MethodHandle { reference_index }) => *reference_index,
            other => {
                return Err(NativeError::InvalidConstant(
                    handle_index,
                    format!("{other:?}"),
                ));
            }
        };
        Ok(Some(self.method_ref(handle)?))
    }
}

#[derive(Debug, Clone)]
enum Cp {
    Empty,
    Utf8(String),
    Integer(i32),
    Float(f32),
    Long(i64),
    Class(u16),
    String(u16),
    FieldRef { class: u16, nt: u16 },
    MethodRef { class: u16, nt: u16 },
    InterfaceMethodRef { class: u16, nt: u16 },
    MethodHandle { reference_index: u16 },
    MethodType,
    InvokeDynamic { bootstrap: u16, nt: u16 },
    NameType { name: u16, descriptor: u16 },
}

#[derive(Debug)]
pub enum NativeError {
    Truncated,
    InvalidClass(&'static str),
    InvalidConstant(u16, String),
    MissingMethod(String),
    UnsupportedOpcode {
        class: String,
        method: String,
        opcode: u8,
        pc: usize,
    },
    StackUnderflow {
        class: String,
        method: String,
    },
    BadReturn {
        class: String,
        method: String,
    },
    TypeMismatch {
        class: String,
        method: String,
        expected: &'static str,
    },
    Arithmetic {
        class: String,
        method: String,
    },
    InvalidBranch {
        class: String,
        method: String,
        target: isize,
    },
    Zip(String),
    Metadata(String),
}
impl std::fmt::Display for NativeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for NativeError {}

#[derive(Debug, Clone, Default)]
pub struct NativeJar {
    classes: BTreeMap<String, ClassFile>,
    /// Resource paths present in the jar, represented by deterministic
    /// markers for the bounded native host.  The native interpreter does not
    /// expose host file handles or arbitrary bytes, but loader code can still
    /// resolve the same resource names it would see on the jar classpath.
    resources: BTreeMap<String, String>,
    resource_bytes: BTreeMap<String, Vec<u8>>,
    pub fabric_id: Option<String>,
    pub fabric_version: Option<String>,
    /// The first mod's id/version from Forge or NeoForge `mods.toml`.
    /// These loaders do not put these in a JSON entrypoint declaration, so
    /// the native launcher keeps them beside the Fabric metadata fields.
    pub neoforge_id: Option<String>,
    pub neoforge_version: Option<String>,
    /// Required mod ids from Fabric or NeoForge metadata, excluding loader-provided ids.
    pub required_mods: Vec<String>,
    /// Forge/NeoForge metadata can mark a mod as client-only. The game-side
    /// loader uses this to keep the constructor off integrated/dedicated
    /// server roles rather than executing a client class and failing later.
    pub client_side_only: bool,
    pub fabric_entrypoints: BTreeMap<String, Vec<Entrypoint>>,
    pub neoforge_entrypoints: Vec<String>,
    /// Explicit Horizon-owned declarations from `META-INF/horizon-native.json`.
    pub native_id: Option<String>,
    pub native_declarations: Vec<RegistryEntry>,
}

const MAX_RESOURCE_BYTES: usize = 4 * 1024 * 1024;
const MAX_RESOURCE_TOTAL: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entrypoint {
    pub class: String,
    pub method: Option<String>,
    /// Fabric entrypoint adapters change the invocation contract. They are
    /// retained for diagnostics until a native adapter implementation exists.
    pub adapter: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionReport {
    pub class: String,
    pub method: String,
    pub calls: usize,
}

impl NativeJar {
    pub fn from_classes(classes: impl IntoIterator<Item = ClassFile>) -> Self {
        let classes = classes.into_iter().collect::<Vec<_>>();
        let neoforge_entrypoints = classes
            .iter()
            .filter(|class| {
                class
                    .pool
                    .iter()
                    .any(|constant| matches!(constant, Cp::Utf8(value) if is_mod_annotation(value)))
            })
            .map(|class| class.name.clone())
            .collect();
        Self {
            classes: classes.into_iter().map(|c| (c.name.clone(), c)).collect(),
            neoforge_entrypoints,
            ..Self::default()
        }
    }

    pub fn from_path(path: &Path) -> Result<Self, NativeError> {
        let file = File::open(path).map_err(|e| NativeError::Zip(e.to_string()))?;
        let mut zip = zip::ZipArchive::new(file).map_err(|e| NativeError::Zip(e.to_string()))?;
        let mut classes = Vec::new();
        let mut resources = BTreeMap::new();
        let mut resource_bytes = BTreeMap::new();
        let mut resource_total = 0usize;
        let mut neoforge_entrypoints = Vec::new();
        let mut metadata = None;
        let mut neoforge_metadata = None;
        let mut forge_metadata = None;
        let mut native_metadata = None;
        for i in 0..zip.len() {
            let mut e = zip
                .by_index(i)
                .map_err(|e| NativeError::Zip(e.to_string()))?;
            let name = e.name().to_owned();
            if !name.ends_with('/') && !name.ends_with(".class") {
                resources
                    .entry(name.clone())
                    .or_insert_with(|| format!("jar-resource/{name}"));
                let metadata_entry = matches!(
                    name.as_str(),
                    "fabric.mod.json"
                        | "META-INF/neoforge.mods.toml"
                        | "META-INF/mods.toml"
                        | "META-INF/horizon-native.json"
                );
                let exceeds_limit = e.size() > MAX_RESOURCE_BYTES as u64
                    || e.size() > MAX_RESOURCE_TOTAL.saturating_sub(resource_total) as u64;
                if exceeds_limit {
                    if metadata_entry {
                        return Err(NativeError::Metadata(format!(
                            "resource {name} exceeds bounded native size limits"
                        )));
                    }
                    continue;
                }
                let mut bytes = Vec::new();
                std::io::Read::read_to_end(&mut e, &mut bytes)
                    .map_err(|e| NativeError::Zip(e.to_string()))?;
                if bytes.len() <= MAX_RESOURCE_BYTES
                    && resource_total.saturating_add(bytes.len()) <= MAX_RESOURCE_TOTAL
                {
                    resource_total = resource_total.saturating_add(bytes.len());
                    resource_bytes.insert(name.clone(), bytes.clone());
                }
                let text = || {
                    String::from_utf8(bytes.clone())
                        .map_err(|e| NativeError::Metadata(e.to_string()))
                };
                if name == "fabric.mod.json" {
                    metadata = Some(text()?);
                } else if name == "META-INF/neoforge.mods.toml" {
                    neoforge_metadata = Some(text()?);
                } else if name == "META-INF/mods.toml" {
                    forge_metadata = Some(text()?);
                } else if name == "META-INF/horizon-native.json" {
                    native_metadata = Some(text()?);
                }
                continue;
            } else if name.ends_with(".class")
                && !name.ends_with("module-info.class")
                && !name.ends_with("package-info.class")
                && !name.starts_with("META-INF/versions/")
            {
                let mut bytes = Vec::new();
                std::io::Read::read_to_end(&mut e, &mut bytes)
                    .map_err(|e| NativeError::Zip(e.to_string()))?;
                let class = ClassFile::parse(&bytes)?;
                if class
                    .pool
                    .iter()
                    .any(|constant| matches!(constant, Cp::Utf8(value) if is_mod_annotation(value)))
                {
                    neoforge_entrypoints.push(class.name.clone());
                }
                classes.push(class);
            }
        }
        let (fabric_id, fabric_version, fabric_entrypoints, required_mods) = metadata
            .map(|m| parse_metadata(&m))
            .transpose()?
            .unwrap_or((None, None, BTreeMap::new(), Vec::new()));
        let loader_metadata = neoforge_metadata.or(forge_metadata);
        let (neoforge_id, neoforge_version, neoforge_required_mods, client_side_only) =
            loader_metadata
                .map(|text| parse_neoforge_metadata(&text))
                .transpose()?
                .unwrap_or((None, None, Vec::new(), false));
        let (native_id, native_declarations) = native_metadata
            .map(|text| parse_native_metadata(&text))
            .transpose()?
            .unwrap_or((None, Vec::new()));
        let mut jar = Self::from_classes(classes);
        jar.resources = resources;
        jar.resource_bytes = resource_bytes;
        jar.fabric_id = fabric_id;
        jar.fabric_version = fabric_version;
        jar.neoforge_id = neoforge_id;
        jar.neoforge_version = neoforge_version;
        jar.client_side_only = client_side_only;
        if !neoforge_required_mods.is_empty() {
            let mut required = required_mods;
            required.extend(neoforge_required_mods);
            required.sort();
            required.dedup();
            jar.required_mods = required;
        } else {
            jar.required_mods = required_mods;
        }
        jar.fabric_entrypoints = fabric_entrypoints;
        jar.neoforge_entrypoints = neoforge_entrypoints;
        jar.native_id = native_id;
        jar.native_declarations = native_declarations;
        Ok(jar)
    }

    pub fn discover_entrypoint(&self, kind: &str) -> Option<Entrypoint> {
        self.discover_entrypoints(kind).into_iter().next()
    }

    /// Discover every entrypoint declared for a loader kind, preserving the
    /// metadata order. Fabric permits multiple classes for one lifecycle
    /// key, and NeoForge permits multiple `@Mod` classes in a jar; callers
    /// that execute a jar should run all of them rather than silently
    /// dropping everything after the first declaration.
    pub fn discover_entrypoints(&self, kind: &str) -> Vec<Entrypoint> {
        if let Some(entries) = self.fabric_entrypoints.get(kind) {
            return entries.clone();
        }
        if kind == "neoforge" {
            return self
                .neoforge_entrypoints
                .iter()
                .map(|class| Entrypoint {
                    class: class.replace('/', "."),
                    method: None,
                    adapter: None,
                })
                .collect();
        }
        Vec::new()
    }

    /// Seed a host with the mod identity and required mod ids declared by
    /// this jar.  The command-line probe uses this before invoking an
    /// entrypoint so loader queries such as `isModLoaded` and
    /// `getModContainer` observe the jar being executed.  The host remains
    /// explicit and deterministic: dependency versions are represented by a
    /// stable `unknown` marker until the launcher supplies resolved versions.
    pub fn seed_host(&self, mut host: StdHost) -> StdHost {
        for (name, marker) in &self.resources {
            host = host.with_resource(name, marker);
            if let Some(bytes) = self.resource_bytes.get(name) {
                host = host.with_resource_bytes(name, bytes.clone());
            }
        }
        if let Some(id) = self.fabric_id.as_deref().or(self.neoforge_id.as_deref()) {
            let version = self
                .fabric_version
                .as_deref()
                .or(self.neoforge_version.as_deref())
                .unwrap_or("unknown");
            host = host.with_mod(id, version);
        }
        for dependency in &self.required_mods {
            host = host.with_mod(dependency, "unknown");
        }
        let provider = self
            .fabric_id
            .as_deref()
            .or(self.neoforge_id.as_deref())
            .unwrap_or("unknown");
        for (kind, entries) in &self.fabric_entrypoints {
            for entry in entries {
                host = host.with_fabric_entrypoint(kind, &entry.class, provider);
            }
        }
        host
    }

    /// Return the resource paths discovered in the jar in deterministic order.
    /// Entries are metadata only; callers must use the bounded host contract
    /// rather than opening the jar through an arbitrary filesystem path.
    pub fn resource_names(&self) -> impl Iterator<Item = &str> {
        self.resources.keys().map(String::as_str)
    }

    pub fn execute<H: Host>(
        &self,
        class: &str,
        explicit_method: Option<&str>,
        kind: &str,
        host: &mut H,
    ) -> Result<ExecutionReport, NativeError> {
        let class_name = class.replace('.', "/");
        let is_neoforge = self
            .neoforge_entrypoints
            .iter()
            .any(|entry| entry == &class_name);
        let method = explicit_method.unwrap_or(if is_neoforge {
            "<init>"
        } else {
            match kind {
                "main" => "onInitialize",
                "client" => "onInitializeClient",
                "server" => "onInitializeServer",
                "neoforge" => "<init>",
                other => {
                    return Err(NativeError::Metadata(format!(
                        "unknown entrypoint kind {other}"
                    )));
                }
            }
        });
        let mut runtime = Runtime {
            classes: &self.classes,
            host,
            calls: 0,
            fields: BTreeMap::new(),
            static_fields: BTreeMap::new(),
            initialized_classes: std::collections::BTreeSet::new(),
            arrays: BTreeMap::new(),
            next_array: 0,
            next_object: 0,
            streams: BTreeMap::new(),
            next_stream: 0,
        };
        if !runtime.classes.contains_key(&class_name) {
            return Err(NativeError::MissingMethod(format!("class {class}")));
        }
        runtime.ensure_initialized(&class_name)?;
        let file = runtime
            .classes
            .get(&class_name)
            .ok_or_else(|| NativeError::MissingMethod(format!("class {class}")))?;
        let is_static = file.method(method, "()V").is_some_and(Method::is_static);
        if !is_static {
            let constructor = file
                .methods
                .iter()
                .find(|candidate| {
                    candidate.name == "<init>" && (candidate.descriptor == "()V" || is_neoforge)
                })
                .cloned()
                .ok_or_else(|| NativeError::MissingMethod(format!("{class_name}#<init>")))?;
            let constructor_args = neutral_args(&constructor.descriptor);
            runtime.invoke(
                &class_name,
                "<init>",
                &constructor.descriptor,
                Some(Value::Ref(Some(class_name.clone()))),
                &constructor_args,
            )?;
        }
        if method != "<init>" {
            runtime.invoke(&class_name, method, "()V", None, &[])?;
        }
        Ok(ExecutionReport {
            class: class.to_owned(),
            method: method.to_owned(),
            calls: runtime.calls,
        })
    }

    pub fn invoke_method<H: Host>(
        &self,
        class: &str,
        method: &str,
        descriptor: &str,
        receiver: Option<Value>,
        host: &mut H,
    ) -> Result<Value, NativeError> {
        self.invoke_method_with_args(class, method, descriptor, receiver, &[], host)
    }

    pub fn invoke_method_with_args<H: Host>(
        &self,
        class: &str,
        method: &str,
        descriptor: &str,
        receiver: Option<Value>,
        args: &[Value],
        host: &mut H,
    ) -> Result<Value, NativeError> {
        let class_name = class.replace('.', "/");
        let mut runtime = Runtime {
            classes: &self.classes,
            host,
            calls: 0,
            fields: BTreeMap::new(),
            static_fields: BTreeMap::new(),
            initialized_classes: std::collections::BTreeSet::new(),
            arrays: BTreeMap::new(),
            next_array: 0,
            next_object: 0,
            streams: BTreeMap::new(),
            next_stream: 0,
        };
        runtime.invoke(&class_name, method, descriptor, receiver, args)
    }

    /// Execute one bounded native Mixin translation at a method head.
    ///
    /// The supported shape is the common callback form: a `void` callback
    /// method runs before the target method, with the target's arguments and,
    /// when present, a trailing `CallbackInfo` marker.  The marker is a
    /// deterministic value owned by this runtime; it does not expose a Java
    /// object graph or cancellation state.  Cancellable injections therefore
    /// stay on the JVM fallback until a native callback-info implementation is
    /// available.
    pub fn execute_mixin_at_head<H: Host>(
        &self,
        plan: &MixinInjectionPlan,
        receiver: Option<Value>,
        args: &[Value],
        host: &mut H,
    ) -> Result<MixinExecutionReport, NativeError> {
        if plan.at != "HEAD" {
            return Err(NativeError::Metadata(format!(
                "native Mixin translation supports only @Inject(at = HEAD), got {}",
                plan.at
            )));
        }
        if plan.cancellable {
            return Err(NativeError::Metadata(
                "native @Inject(at = HEAD) does not support cancellable callbacks".into(),
            ));
        }

        let target_class = self.classes.get(&plan.target.owner).ok_or_else(|| {
            NativeError::MissingMethod(format!(
                "{}#{}{}",
                plan.target.owner, plan.target.name, plan.target.descriptor
            ))
        })?;
        let target_method = target_class
            .method(&plan.target.name, &plan.target.descriptor)
            .ok_or_else(|| {
                NativeError::MissingMethod(format!(
                    "{}#{}{}",
                    plan.target.owner, plan.target.name, plan.target.descriptor
                ))
            })?;
        if args.len() != arg_count(&plan.target.descriptor) {
            return Err(NativeError::Metadata(format!(
                "Mixin target {}#{}{} expected {} arguments, got {}",
                plan.target.owner,
                plan.target.name,
                plan.target.descriptor,
                arg_count(&plan.target.descriptor),
                args.len()
            )));
        }

        let callback_class = self.classes.get(&plan.callback.owner).ok_or_else(|| {
            NativeError::MissingMethod(format!(
                "{}#{}{}",
                plan.callback.owner, plan.callback.name, plan.callback.descriptor
            ))
        })?;
        let callback_method = callback_class
            .method(&plan.callback.name, &plan.callback.descriptor)
            .ok_or_else(|| {
                NativeError::MissingMethod(format!(
                    "{}#{}{}",
                    plan.callback.owner, plan.callback.name, plan.callback.descriptor
                ))
            })?;
        if return_desc(&plan.callback.descriptor) != "V" {
            return Err(NativeError::Metadata(format!(
                "native Mixin callback {}#{}{} must return void",
                plan.callback.owner, plan.callback.name, plan.callback.descriptor
            )));
        }

        let target_args = descriptor_arguments(&plan.target.descriptor);
        let callback_args = descriptor_arguments(&plan.callback.descriptor);
        let mut callback_values = args.to_vec();
        if callback_args == target_args {
            // Exact argument mirroring is useful for injectors that target a
            // static helper or a façade callback with no CallbackInfo value.
        } else if callback_args.len() == target_args.len() + 1
            && callback_args
                .last()
                .is_some_and(|arg| is_callback_info_descriptor(arg))
            && callback_args[..target_args.len()] == target_args[..]
        {
            callback_values.push(Value::Ref(Some(format!(
                "mixin/callback-info/{}#{}",
                plan.target.owner, plan.target.name
            ))));
        } else {
            return Err(NativeError::Metadata(format!(
                "native Mixin callback {}#{}{} must mirror target arguments and may add one trailing CallbackInfo",
                plan.callback.owner, plan.callback.name, plan.callback.descriptor
            )));
        }

        let callback_receiver = (!callback_method.is_static()).then(|| {
            receiver
                .clone()
                .unwrap_or_else(|| Value::Ref(Some(plan.callback.owner.clone())))
        });
        let callback_result = self.invoke_method_with_args(
            &plan.callback.owner,
            &plan.callback.name,
            &plan.callback.descriptor,
            callback_receiver,
            &callback_values,
            host,
        )?;
        let target_receiver = (!target_method.is_static()).then(|| {
            receiver
                .clone()
                .unwrap_or_else(|| Value::Ref(Some(plan.target.owner.clone())))
        });
        let target_result = self.invoke_method_with_args(
            &plan.target.owner,
            &plan.target.name,
            &plan.target.descriptor,
            target_receiver,
            args,
            host,
        )?;
        Ok(MixinExecutionReport {
            callback_result,
            target_result,
        })
    }

    /// Execute one listener work item produced by [`StdHost::dispatch_event`].
    ///
    /// The callback marker carries the resolved method handle and captured
    /// values. This method only invokes targets that are present in this jar;
    /// loader callbacks and arbitrary Java object graphs remain host work.
    pub fn execute_dispatch<H: Host>(
        &self,
        dispatch: &EventDispatch,
        host: &mut H,
    ) -> Result<Value, NativeError> {
        let marker = dispatch
            .listener
            .args
            .iter()
            .rev()
            .find_map(|value| match value {
                Value::Ref(Some(value)) if value.starts_with("@indy:") => Some(value.as_str()),
                _ => None,
            })
            .ok_or_else(|| {
                NativeError::Metadata("event listener has no invokedynamic marker".into())
            })?;
        let callback = parse_callback_marker(marker)?;
        let class = self.classes.get(&callback.target.owner).ok_or_else(|| {
            NativeError::MissingMethod(format!(
                "{}#{}{}",
                callback.target.owner, callback.target.name, callback.target.descriptor
            ))
        })?;
        let method = class
            .method(&callback.target.name, &callback.target.descriptor)
            .ok_or_else(|| {
                NativeError::MissingMethod(format!(
                    "{}#{}{}",
                    callback.target.owner, callback.target.name, callback.target.descriptor
                ))
            })?;
        let (receiver, mut args) = if method.is_static() {
            (None, callback.captures)
        } else {
            let receiver = callback.captures.first().cloned().ok_or_else(|| {
                NativeError::Metadata("instance callback has no captured receiver".into())
            })?;
            (
                Some(receiver),
                callback.captures.into_iter().skip(1).collect(),
            )
        };
        args.push(dispatch.event.clone());
        let expected = arg_count(&callback.target.descriptor);
        if args.len() != expected {
            return Err(NativeError::Metadata(format!(
                "callback {}#{}{} expected {expected} arguments, got {}",
                callback.target.owner,
                callback.target.name,
                callback.target.descriptor,
                args.len()
            )));
        }
        self.invoke_method_with_args(
            &callback.target.owner,
            &callback.target.name,
            &callback.target.descriptor,
            receiver,
            &args,
            host,
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
struct CallbackMarker {
    target: MethodRef,
    captures: Vec<Value>,
}

fn parse_callback_marker(marker: &str) -> Result<CallbackMarker, NativeError> {
    let mut parts = marker.splitn(5, ':');
    if parts.next() != Some("@indy") {
        return Err(NativeError::Metadata(
            "invalid callback marker prefix".into(),
        ));
    }
    let _source = parts
        .next()
        .ok_or_else(|| NativeError::Metadata("callback marker has no source".into()))?;
    let _index = parts
        .next()
        .ok_or_else(|| NativeError::Metadata("callback marker has no call-site index".into()))?;
    let target = parts
        .next()
        .ok_or_else(|| NativeError::Metadata("callback marker has no target".into()))?;
    let captures = parts
        .next()
        .ok_or_else(|| NativeError::Metadata("callback marker has no captures".into()))?;
    let (owner, method_and_descriptor) = target
        .split_once('#')
        .ok_or_else(|| NativeError::Metadata("callback target has no owner separator".into()))?;
    let descriptor_at = method_and_descriptor
        .find('(')
        .ok_or_else(|| NativeError::Metadata("callback target has no descriptor".into()))?;
    let target = MethodRef {
        owner: owner.to_owned(),
        name: method_and_descriptor[..descriptor_at].to_owned(),
        descriptor: method_and_descriptor[descriptor_at..].to_owned(),
    };
    let captures = if captures.is_empty() {
        Vec::new()
    } else {
        split_capture_markers(captures)?
            .into_iter()
            .map(|capture| parse_value_marker(&capture))
            .collect::<Result<Vec<_>, _>>()?
    };
    Ok(CallbackMarker { target, captures })
}

/// Evaluate the bounded truth surface of a predicate marker.  Java's default
/// `Predicate` combinators are represented as markers, so they remain usable
/// even though the interpreter does not materialize interface instances.
fn predicate_marker_value(marker: &str) -> bool {
    if let Some(rest) = marker.strip_prefix("@fn:predicate.not:") {
        return !predicate_marker_value(rest);
    }
    if let Some(rest) = marker.strip_prefix("@fn:predicate.and:")
        && let Some((left, right)) = rest.split_once('|')
    {
        return predicate_marker_value(left) && predicate_marker_value(right);
    }
    if let Some(rest) = marker.strip_prefix("@fn:predicate.or:")
        && let Some((left, right)) = rest.split_once('|')
    {
        return predicate_marker_value(left) || predicate_marker_value(right);
    }
    marker.starts_with("@indy:")
}

fn split_capture_markers(value: &str) -> Result<Vec<String>, NativeError> {
    let mut captures = Vec::new();
    let mut current = String::new();
    let mut escaped = false;
    for ch in value.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == ',' {
            captures.push(std::mem::take(&mut current));
        } else {
            current.push(ch);
        }
    }
    if escaped {
        return Err(NativeError::Metadata(
            "callback capture ends with an escape".into(),
        ));
    }
    captures.push(current);
    Ok(captures)
}

fn parse_value_marker(marker: &str) -> Result<Value, NativeError> {
    if marker == "null" {
        return Ok(Value::Ref(None));
    }
    if marker == "void" {
        return Ok(Value::Void);
    }
    if let Some(value) = marker.strip_prefix('i') {
        return value
            .parse::<i32>()
            .map(Value::Int)
            .map_err(|_| NativeError::Metadata(format!("invalid integer capture {marker}")));
    }
    if let Some(value) = marker.strip_prefix('r') {
        return Ok(Value::Ref(Some(value.to_owned())));
    }
    Err(NativeError::Metadata(format!(
        "invalid callback capture {marker}"
    )))
}

struct Runtime<'a, H> {
    classes: &'a BTreeMap<String, ClassFile>,
    host: &'a mut H,
    calls: usize,
    fields: BTreeMap<String, Value>,
    static_fields: BTreeMap<String, Value>,
    initialized_classes: std::collections::BTreeSet<String>,
    arrays: BTreeMap<String, Vec<Value>>,
    next_array: usize,
    next_object: usize,
    streams: BTreeMap<String, Vec<Value>>,
    next_stream: usize,
}
impl<H: Host> Runtime<'_, H> {
    fn static_field_key(field: &MethodRef) -> String {
        format!("{}#{}{}", field.owner, field.name, field.descriptor)
    }

    /// Run a jar-owned class initializer once before the first active use.
    ///
    /// Fabric and NeoForge entrypoints commonly keep identifiers, registry
    /// keys, or deferred registers in static fields initialized by `<clinit>`.
    /// Treating those fields as host-owned defaults silently turns a valid
    /// bootstrap into a no-op, so the bounded runtime models only this JVM
    /// initialization rule while retaining its per-invocation lifetime.
    fn ensure_initialized(&mut self, class: &str) -> Result<(), NativeError> {
        if !self.classes.contains_key(class) || !self.initialized_classes.insert(class.to_owned()) {
            return Ok(());
        }
        if self
            .classes
            .get(class)
            .and_then(|file| file.method("<clinit>", "()V"))
            .is_some()
        {
            self.invoke(class, "<clinit>", "()V", None, &[])?;
        }
        Ok(())
    }

    /// Resolve a captured invokedynamic marker when its target is in this
    /// jar. Unknown or malformed markers stay on the host path, preserving
    /// the bounded runtime's neutral fallback for unsupported lambdas.
    fn invoke_callback(
        &mut self,
        marker: &Value,
        args: &[Value],
    ) -> Result<Option<Value>, NativeError> {
        let Some(marker) = expect_ref(marker) else {
            return Ok(None);
        };
        let Ok(callback) = parse_callback_marker(marker) else {
            return Ok(None);
        };
        let Some(class) = self.classes.get(&callback.target.owner) else {
            return Ok(None);
        };
        let Some(method) = class.method(&callback.target.name, &callback.target.descriptor) else {
            return Ok(None);
        };
        let (receiver, mut target_args) = if method.is_static() {
            (None, callback.captures)
        } else {
            let Some(receiver) = callback.captures.first().cloned() else {
                return Err(NativeError::Metadata(
                    "instance callback has no captured receiver".into(),
                ));
            };
            (
                Some(receiver),
                callback.captures.into_iter().skip(1).collect(),
            )
        };
        target_args.extend_from_slice(args);
        let expected = arg_count(&callback.target.descriptor);
        if target_args.len() != expected {
            return Err(NativeError::Metadata(format!(
                "callback {}#{}{} expected {expected} arguments, got {}",
                callback.target.owner,
                callback.target.name,
                callback.target.descriptor,
                target_args.len()
            )));
        }
        Ok(Some(self.invoke(
            &callback.target.owner,
            &callback.target.name,
            &callback.target.descriptor,
            receiver,
            &target_args,
        )?))
    }

    fn invoke_optional(
        &mut self,
        r: &MethodRef,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Option<Value>, NativeError> {
        let present =
            expect_ref(receiver).map(|value| value.strip_prefix("optional/").unwrap_or(value));
        let callback = match r.name.as_str() {
            "map" if r.descriptor == "(Ljava/util/function/Function;)Ljava/util/Optional;" => {
                args.first()
            }
            "ifPresent" if r.descriptor == "(Ljava/util/function/Consumer;)V" => args.first(),
            "filter" if r.descriptor == "(Ljava/util/function/Predicate;)Ljava/util/Optional;" => {
                args.first()
            }
            "orElseGet" if r.descriptor == "(Ljava/util/function/Supplier;)Ljava/lang/Object;" => {
                args.first()
            }
            _ => return Ok(None),
        };
        let Some(callback) = callback else {
            return Ok(None);
        };
        let Some(value) = present else {
            return match r.name.as_str() {
                "map" => Ok(Some(Value::Ref(None))),
                "ifPresent" => Ok(Some(Value::Void)),
                "orElseGet" => self
                    .invoke_callback(callback, &[])
                    .map(|result| Some(result.unwrap_or(Value::Ref(None)))),
                _ => Ok(None),
            };
        };
        if r.name == "orElseGet" {
            return Ok(Some(Value::Ref(Some(value.to_owned()))));
        }
        if r.name == "filter" {
            let Some(result) =
                self.invoke_callback(callback, &[Value::Ref(Some(value.to_owned()))])?
            else {
                return Ok(None);
            };
            return Ok(Some(match result {
                Value::Int(flag) if flag != 0 => receiver.clone(),
                _ => Value::Ref(None),
            }));
        }
        let Some(result) = self.invoke_callback(callback, &[Value::Ref(Some(value.to_owned()))])?
        else {
            return Ok(None);
        };
        match r.name.as_str() {
            "map" => Ok(Some(match result {
                Value::Ref(Some(value)) if value.starts_with("optional/") => {
                    Value::Ref(Some(value))
                }
                Value::Ref(Some(value)) => Value::Ref(Some(format!("optional/{value}"))),
                Value::Ref(None) | Value::Void => Value::Ref(None),
                value => Value::Ref(Some(format!("optional/{}", value_marker(&value)))),
            })),
            "ifPresent" => Ok(Some(Value::Void)),
            "orElseGet" => Ok(Some(result)),
            _ => Ok(None),
        }
    }

    fn new_array(&mut self, class: &str, method: &str) -> String {
        let marker = format!("array/{class}/{method}/{}", self.next_array);
        self.next_array += 1;
        marker
    }

    fn new_object(&mut self, class: &str) -> Value {
        let marker = format!("{class}#{}", self.next_object);
        self.next_object = self.next_object.saturating_add(1);
        Value::Ref(Some(marker))
    }

    /// Resolve `Class.forName` only against classes already loaded from the
    /// current jar. This keeps reflection useful for bootstrap code without
    /// permitting arbitrary classpath or filesystem loading.
    fn class_for_name(&self, args: &[Value]) -> Result<Value, NativeError> {
        let binary = expect_string(
            args.first()
                .ok_or_else(|| NativeError::Metadata("Class.forName needs a class name".into()))?,
        );
        let internal = binary.replace('.', "/");
        Ok(if self.classes.contains_key(&internal) {
            Value::Ref(Some(internal))
        } else {
            Value::Ref(None)
        })
    }

    /// Execute the small reflective-array surface used by Fabric API helpers.
    /// This operates on arrays owned by the current bounded invocation; it
    /// never loads classes or reaches the filesystem.
    fn invoke_reflect_array(
        &mut self,
        r: &MethodRef,
        args: &[Value],
        class: &str,
        method: &str,
    ) -> Result<Option<Value>, NativeError> {
        if r.owner != "java/lang/reflect/Array" {
            return Ok(None);
        }
        let value = match (r.name.as_str(), r.descriptor.as_str()) {
            ("newInstance", "(Ljava/lang/Class;I)Ljava/lang/Object;") => {
                let length = expect_int(r, StdHost::argument(r, args, 1)?)?;
                let length = array_length(length)?;
                let marker = self.new_array(class, method);
                self.arrays
                    .insert(marker.clone(), vec![Value::Ref(None); length]);
                Value::Ref(Some(marker))
            }
            ("getLength", "(Ljava/lang/Object;)I") => {
                Value::Int(self.array(StdHost::argument(r, args, 0)?)?.len() as i32)
            }
            ("get", "(Ljava/lang/Object;I)Ljava/lang/Object;")
            | ("getInt", "(Ljava/lang/Object;I)I") => {
                let index = array_index(expect_int(r, StdHost::argument(r, args, 1)?)?)?;
                self.array(StdHost::argument(r, args, 0)?)?
                    .get(index)
                    .cloned()
                    .ok_or_else(|| {
                        NativeError::Metadata(format!("array index {index} out of bounds"))
                    })?
            }
            ("getBoolean", "(Ljava/lang/Object;I)Z") => {
                let index = array_index(expect_int(r, StdHost::argument(r, args, 1)?)?)?;
                let value = self
                    .array(StdHost::argument(r, args, 0)?)?
                    .get(index)
                    .cloned()
                    .ok_or_else(|| {
                        NativeError::Metadata(format!("array index {index} out of bounds"))
                    })?;
                Value::Int(expect_int(r, &value)?)
            }
            ("getLong", "(Ljava/lang/Object;I)J") => {
                let index = array_index(expect_int(r, StdHost::argument(r, args, 1)?)?)?;
                self.array(StdHost::argument(r, args, 0)?)?
                    .get(index)
                    .cloned()
                    .ok_or_else(|| {
                        NativeError::Metadata(format!("array index {index} out of bounds"))
                    })?
            }
            ("set", "(Ljava/lang/Object;ILjava/lang/Object;)V") => {
                let index = array_index(expect_int(r, StdHost::argument(r, args, 1)?)?)?;
                let marker = expect_array_marker(StdHost::argument(r, args, 0)?)?;
                let slot = self
                    .arrays
                    .get_mut(marker)
                    .ok_or_else(|| {
                        NativeError::Metadata("array is not owned by this invocation".into())
                    })?
                    .get_mut(index)
                    .ok_or_else(|| {
                        NativeError::Metadata(format!("array index {index} out of bounds"))
                    })?;
                *slot = StdHost::argument(r, args, 2)?.clone();
                Value::Void
            }
            ("setInt", "(Ljava/lang/Object;II)V") => {
                let index = array_index(expect_int(r, StdHost::argument(r, args, 1)?)?)?;
                let marker = expect_array_marker(StdHost::argument(r, args, 0)?)?;
                let slot = self
                    .arrays
                    .get_mut(marker)
                    .ok_or_else(|| {
                        NativeError::Metadata("array is not owned by this invocation".into())
                    })?
                    .get_mut(index)
                    .ok_or_else(|| {
                        NativeError::Metadata(format!("array index {index} out of bounds"))
                    })?;
                *slot = Value::Int(expect_int(r, StdHost::argument(r, args, 2)?)?);
                Value::Void
            }
            ("setBoolean", "(Ljava/lang/Object;IZ)V") => {
                let index = array_index(expect_int(r, StdHost::argument(r, args, 1)?)?)?;
                let marker = expect_array_marker(StdHost::argument(r, args, 0)?)?;
                let slot = self
                    .arrays
                    .get_mut(marker)
                    .ok_or_else(|| {
                        NativeError::Metadata("array is not owned by this invocation".into())
                    })?
                    .get_mut(index)
                    .ok_or_else(|| {
                        NativeError::Metadata(format!("array index {index} out of bounds"))
                    })?;
                *slot = Value::Int(expect_int(r, StdHost::argument(r, args, 2)?)?);
                Value::Void
            }
            ("setLong", "(Ljava/lang/Object;IJ)V") => {
                let index = array_index(expect_int(r, StdHost::argument(r, args, 1)?)?)?;
                let marker = expect_array_marker(StdHost::argument(r, args, 0)?)?;
                let slot = self
                    .arrays
                    .get_mut(marker)
                    .ok_or_else(|| {
                        NativeError::Metadata("array is not owned by this invocation".into())
                    })?
                    .get_mut(index)
                    .ok_or_else(|| {
                        NativeError::Metadata(format!("array index {index} out of bounds"))
                    })?;
                *slot = Value::Int(expect_int(r, StdHost::argument(r, args, 2)?)?);
                Value::Void
            }
            _ => return Ok(None),
        };
        Ok(Some(value))
    }

    /// Resolve the no-disk reflection surface needed by small bootstrap
    /// helpers. Class lookups produce markers, and Method.invoke dispatches
    /// only to methods already present in this jar. The parameter Class[] is
    /// intentionally used only for linkage; overload selection is bounded by
    /// the runtime argument count carried by Object[].
    fn invoke_reflect_method(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
        args: &[Value],
    ) -> Result<Option<Value>, NativeError> {
        if r.owner == "java/lang/Class"
            && matches!(r.name.as_str(), "getFields" | "getDeclaredFields")
            && r.descriptor == "()[Ljava/lang/reflect/Field;"
        {
            let class = expect_string(receiver.unwrap_or(&Value::Ref(None))).to_owned();
            let declared = r.name == "getDeclaredFields";
            let fields = self
                .classes
                .get(&class)
                .map(|file| {
                    file.fields
                        .iter()
                        .filter(|field| declared || field.access & 1 != 0)
                        .map(|field| {
                            Value::Ref(Some(format!(
                                "reflect-field/{class}#{}:{}:{}",
                                field.name, field.descriptor, field.access
                            )))
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let marker = self.new_array("java/lang/reflect/Field", "getDeclaredFields");
            self.arrays.insert(marker.clone(), fields);
            return Ok(Some(Value::Ref(Some(marker))));
        }
        if r.owner == "java/lang/Class"
            && matches!(r.name.as_str(), "getField" | "getDeclaredField")
            && r.descriptor == "(Ljava/lang/String;)Ljava/lang/reflect/Field;"
        {
            let class = expect_string(receiver.unwrap_or(&Value::Ref(None))).to_owned();
            let name = expect_string(StdHost::argument(r, args, 0)?);
            let marker = self
                .classes
                .get(&class)
                .and_then(|file| {
                    file.fields.iter().find(|field| {
                        field.name == name
                            && (r.name == "getDeclaredField" || field.access & 1 != 0)
                    })
                })
                .map(|field| {
                    format!(
                        "reflect-field/{class}#{}:{}:{}",
                        field.name, field.descriptor, field.access
                    )
                })
                .unwrap_or_else(|| format!("reflect-field/{class}#{name}"));
            return Ok(Some(Value::Ref(Some(marker))));
        }
        if r.owner == "java/lang/Class"
            && matches!(r.name.as_str(), "getMethods" | "getDeclaredMethods")
            && r.descriptor == "()[Ljava/lang/reflect/Method;"
        {
            let class = expect_string(receiver.unwrap_or(&Value::Ref(None))).to_owned();
            let methods = self
                .classes
                .get(&class)
                .map(|file| {
                    file.methods
                        .iter()
                        .filter(|method| method.name != "<init>")
                        .map(|method| {
                            Value::Ref(Some(format!(
                                "reflect-method/{class}#{}{}",
                                method.name, method.descriptor
                            )))
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let marker = self.new_array("java/lang/reflect/Method", "getMethods");
            self.arrays.insert(marker.clone(), methods);
            return Ok(Some(Value::Ref(Some(marker))));
        }
        if r.owner == "java/lang/Class"
            && matches!(r.name.as_str(), "getMethod" | "getDeclaredMethod")
            && r.descriptor.ends_with(")Ljava/lang/reflect/Method;")
        {
            let class = expect_string(receiver.unwrap_or(&Value::Ref(None)));
            let name = expect_string(StdHost::argument(r, args, 0)?);
            let parameter_types = args
                .get(1)
                .and_then(|value| self.array(value).ok())
                .cloned()
                .unwrap_or_default();
            let descriptor = if parameter_types.is_empty() {
                self.classes.get(class).and_then(|file| {
                    file.methods
                        .iter()
                        .find(|candidate| {
                            candidate.name == name && arg_count(&candidate.descriptor) == 0
                        })
                        .map(|candidate| candidate.descriptor.clone())
                })
            } else {
                let mut descriptor = String::from("(");
                let mut supported = true;
                for parameter in &parameter_types {
                    if let Some(value) = class_descriptor(parameter) {
                        descriptor.push_str(&value);
                    } else {
                        supported = false;
                        break;
                    }
                }
                if supported {
                    descriptor.push(')');
                    self.classes
                        .get(class)
                        .and_then(|file| {
                            file.methods
                                .iter()
                                .find(|candidate| {
                                    candidate.name == name
                                        && candidate.descriptor.starts_with(&descriptor)
                                })
                                .map(|candidate| candidate.descriptor.clone())
                        })
                        .or(Some(descriptor))
                } else {
                    None
                }
            };
            let marker = descriptor.map_or_else(
                || format!("reflect-method/{class}#{name}"),
                |descriptor| format!("reflect-method/{class}#{name}{descriptor}"),
            );
            return Ok(Some(Value::Ref(Some(marker))));
        }
        if r.owner != "java/lang/reflect/Method" || r.name != "invoke" {
            return Ok(None);
        }
        let marker = expect_string(receiver.unwrap_or(&Value::Ref(None)));
        let marker = marker.strip_prefix("reflect-method/").ok_or_else(|| {
            NativeError::Metadata("malformed java/lang/reflect/Method marker".into())
        })?;
        let (owner, member) = marker.split_once('#').ok_or_else(|| {
            NativeError::Metadata("reflective method marker has no name separator".into())
        })?;
        let (name, descriptor) = member
            .find('(')
            .map_or((member, None), |at| (&member[..at], Some(&member[at..])));
        let invocation_args = StdHost::argument(r, args, 1)?;
        let invocation_args = if matches!(invocation_args, Value::Ref(None)) {
            Vec::new()
        } else {
            self.array(invocation_args)?.clone()
        };
        let class_file = self
            .classes
            .get(owner)
            .ok_or_else(|| NativeError::MissingMethod(format!("{owner}#{name}")))?;
        let target = class_file
            .methods
            .iter()
            .find(|candidate| {
                candidate.name == name
                    && descriptor.is_none_or(|descriptor| candidate.descriptor == descriptor)
                    && arg_count(&candidate.descriptor) == invocation_args.len()
            })
            .cloned()
            .ok_or_else(|| NativeError::MissingMethod(format!("{owner}#{name}")))?;
        let target_receiver = if target.is_static() {
            None
        } else {
            Some(StdHost::argument(r, args, 0)?.clone())
        };
        Ok(Some(self.invoke(
            owner,
            &target.name,
            &target.descriptor,
            target_receiver,
            &invocation_args,
        )?))
    }

    fn array(&self, value: &Value) -> Result<&Vec<Value>, NativeError> {
        let marker = expect_array_marker(value)?;
        self.arrays
            .get(marker)
            .ok_or_else(|| NativeError::Metadata("array is not owned by this invocation".into()))
    }

    fn new_stream(&mut self, class: &str, method: &str, values: Vec<Value>) -> Value {
        let marker = format!("stream/{class}/{method}/{}", self.next_stream);
        self.next_stream += 1;
        self.streams.insert(marker.clone(), values);
        Value::Ref(Some(marker))
    }

    fn stream(&self, value: &Value) -> Result<&Vec<Value>, NativeError> {
        let marker = expect_ref(value)
            .filter(|marker| marker.starts_with("stream/"))
            .ok_or_else(|| NativeError::Metadata("expected a native stream marker".into()))?;
        self.streams
            .get(marker)
            .ok_or_else(|| NativeError::Metadata("stream is not owned by this invocation".into()))
    }

    fn invoke_stream(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
        args: &[Value],
        class: &str,
        method: &str,
    ) -> Result<Option<Value>, NativeError> {
        if r.owner == "java/util/Optional"
            && r.name == "stream"
            && r.descriptor == "()Ljava/util/stream/Stream;"
        {
            let Some(receiver) = receiver else {
                return Ok(None);
            };
            let values = expect_ref(receiver)
                .map(|value| value.strip_prefix("optional/").unwrap_or(value))
                .map(|value| Value::Ref(Some(value.to_owned())))
                .into_iter()
                .collect();
            return Ok(Some(self.new_stream(class, method, values)));
        }
        if r.name == "stream"
            && r.descriptor == "()Ljava/util/stream/Stream;"
            && receiver.is_some_and(|_| {
                matches!(
                    r.owner.as_str(),
                    "java/util/Collection"
                        | "java/util/List"
                        | "java/util/Set"
                        | "java/util/AbstractCollection"
                        | "java/util/AbstractList"
                        | "java/util/AbstractSet"
                )
            })
        {
            let values = collection_values(receiver.expect("checked above"))?
                .into_iter()
                .map(|value| parse_collection_value(&value))
                .collect::<Result<Vec<_>, _>>()?;
            return Ok(Some(self.new_stream(class, method, values)));
        }
        if !r.owner.starts_with("java/util/stream/") {
            return Ok(None);
        }
        let Some(receiver) = receiver else {
            return Ok(None);
        };
        let values = self.stream(receiver)?.clone();
        let result = match (r.name.as_str(), r.descriptor.as_str()) {
            ("map", "(Ljava/util/function/Function;)Ljava/util/stream/Stream;") => {
                let callback = StdHost::argument(r, args, 0)?;
                let mut mapped = Vec::with_capacity(values.len());
                for value in values {
                    let result =
                        match self.invoke_callback(callback, std::slice::from_ref(&value))? {
                            Some(result) => result,
                            None => self.host.invoke_instance(
                                &MethodRef {
                                    owner: "java/util/function/Function".into(),
                                    name: "apply".into(),
                                    descriptor: "(Ljava/lang/Object;)Ljava/lang/Object;".into(),
                                },
                                callback,
                                &[value],
                            )?,
                        };
                    mapped.push(result);
                }
                self.new_stream(class, method, mapped)
            }
            ("filter", "(Ljava/util/function/Predicate;)Ljava/util/stream/Stream;") => {
                let callback = StdHost::argument(r, args, 0)?;
                let mut filtered = Vec::with_capacity(values.len());
                for value in values {
                    let result =
                        match self.invoke_callback(callback, std::slice::from_ref(&value))? {
                            Some(result) => result,
                            None => self.host.invoke_instance(
                                &MethodRef {
                                    owner: "java/util/function/Predicate".into(),
                                    name: "test".into(),
                                    descriptor: "(Ljava/lang/Object;)Z".into(),
                                },
                                callback,
                                std::slice::from_ref(&value),
                            )?,
                        };
                    if matches!(result, Value::Int(flag) if flag != 0) {
                        filtered.push(value);
                    }
                }
                self.new_stream(class, method, filtered)
            }
            ("distinct", "()Ljava/util/stream/Stream;") => {
                let mut distinct = Vec::with_capacity(values.len());
                for value in values {
                    if !distinct.contains(&value) {
                        distinct.push(value);
                    }
                }
                self.new_stream(class, method, distinct)
            }
            ("limit", "(J)Ljava/util/stream/Stream;") => {
                let limit = expect_int(r, StdHost::argument(r, args, 0)?)?.max(0) as usize;
                self.new_stream(class, method, values.into_iter().take(limit).collect())
            }
            ("skip", "(J)Ljava/util/stream/Stream;") => {
                let skip = expect_int(r, StdHost::argument(r, args, 0)?)?.max(0) as usize;
                self.new_stream(class, method, values.into_iter().skip(skip).collect())
            }
            ("anyMatch", "(Ljava/util/function/Predicate;)Z") => {
                let callback = StdHost::argument(r, args, 0)?;
                let mut matched = false;
                for value in values {
                    let result =
                        match self.invoke_callback(callback, std::slice::from_ref(&value))? {
                            Some(result) => result,
                            None => self.host.invoke_instance(
                                &MethodRef {
                                    owner: "java/util/function/Predicate".into(),
                                    name: "test".into(),
                                    descriptor: "(Ljava/lang/Object;)Z".into(),
                                },
                                callback,
                                std::slice::from_ref(&value),
                            )?,
                        };
                    if matches!(result, Value::Int(flag) if flag != 0) {
                        matched = true;
                        break;
                    }
                }
                Value::Int(matched as i32)
            }
            ("allMatch", "(Ljava/util/function/Predicate;)Z") => {
                let callback = StdHost::argument(r, args, 0)?;
                let mut matched = true;
                for value in values {
                    let result =
                        match self.invoke_callback(callback, std::slice::from_ref(&value))? {
                            Some(result) => result,
                            None => self.host.invoke_instance(
                                &MethodRef {
                                    owner: "java/util/function/Predicate".into(),
                                    name: "test".into(),
                                    descriptor: "(Ljava/lang/Object;)Z".into(),
                                },
                                callback,
                                std::slice::from_ref(&value),
                            )?,
                        };
                    if !matches!(result, Value::Int(flag) if flag != 0) {
                        matched = false;
                        break;
                    }
                }
                Value::Int(matched as i32)
            }
            ("noneMatch", "(Ljava/util/function/Predicate;)Z") => {
                let callback = StdHost::argument(r, args, 0)?;
                let mut matched = true;
                for value in values {
                    let result =
                        match self.invoke_callback(callback, std::slice::from_ref(&value))? {
                            Some(result) => result,
                            None => self.host.invoke_instance(
                                &MethodRef {
                                    owner: "java/util/function/Predicate".into(),
                                    name: "test".into(),
                                    descriptor: "(Ljava/lang/Object;)Z".into(),
                                },
                                callback,
                                std::slice::from_ref(&value),
                            )?,
                        };
                    if matches!(result, Value::Int(flag) if flag != 0) {
                        matched = false;
                        break;
                    }
                }
                Value::Int(matched as i32)
            }
            ("findFirst" | "findAny", "()Ljava/util/Optional;") => {
                optional_wrap(values.into_iter().next().unwrap_or(Value::Ref(None)))
            }
            ("count", "()J") => Value::Int(values.len() as i32),
            ("toList", "()Ljava/util/List;") => collection_value("list", &values),
            ("toArray", "()[Ljava/lang/Object;") => {
                let marker = self.new_array(class, method);
                self.arrays.insert(marker.clone(), values);
                Value::Ref(Some(marker))
            }
            ("collect", "(Ljava/util/stream/Collector;)Ljava/lang/Object;") => {
                let collector = expect_ref(StdHost::argument(r, args, 0)?).ok_or_else(|| {
                    NativeError::Metadata("Stream.collect needs a collector".into())
                })?;
                match collector.as_str() {
                    "collector/list" => collection_value("list", &values),
                    "collector/set" => collection_value("set", &values),
                    _ => return Ok(None),
                }
            }
            ("isEmpty", "()Z") => Value::Int(values.is_empty() as i32),
            ("forEach", "(Ljava/util/function/Consumer;)V") => {
                let callback = StdHost::argument(r, args, 0)?;
                for value in values {
                    let result = self.invoke_callback(callback, std::slice::from_ref(&value))?;
                    if result.is_none() {
                        self.host.invoke_instance(
                            &MethodRef {
                                owner: "java/util/function/Consumer".into(),
                                name: "accept".into(),
                                descriptor: "(Ljava/lang/Object;)V".into(),
                            },
                            callback,
                            &[value],
                        )?;
                    }
                }
                Value::Void
            }
            _ => return Ok(None),
        };
        Ok(Some(result))
    }

    fn invoke(
        &mut self,
        class: &str,
        method: &str,
        desc: &str,
        receiver: Option<Value>,
        args: &[Value],
    ) -> Result<Value, NativeError> {
        if method != "<clinit>" {
            self.ensure_initialized(class)?;
        }
        let file = self
            .classes
            .get(class)
            .ok_or_else(|| NativeError::MissingMethod(format!("{class}#{method}{desc}")))?;
        let target = file
            .method(method, desc)
            .cloned()
            .ok_or_else(|| NativeError::MissingMethod(format!("{class}#{method}{desc}")))?;
        let Some(ref code) = target.code else {
            return Ok(Value::default_for(return_desc(desc)));
        };
        let mut locals = vec![Value::Ref(None); target.max_locals.max(1) as usize];
        let mut local = 0;
        if !target.is_static() {
            locals[0] = receiver.unwrap_or(Value::Ref(None));
            local = 1;
        }
        for arg in args {
            if let Some(slot) = locals.get_mut(local) {
                *slot = arg.clone();
                local += 1;
            }
        }
        let mut stack = Vec::new();
        let mut pc = 0;
        while pc < code.len() {
            let at = pc;
            let op = code[pc];
            pc += 1;
            match op {
                0x00 => {}
                0x01 => stack.push(Value::Ref(None)),
                0x02..=0x08 => stack.push(Value::Int(op as i32 - 3)),
                0x09..=0x0f => stack.push(Value::Int(if op == 0x09 || op == 0x0b || op == 0x0e {
                    0
                } else {
                    1
                })),
                0x10 => {
                    let b = *code.get(pc).ok_or(NativeError::Truncated)?;
                    pc += 1;
                    stack.push(Value::Int(b as i8 as i32));
                }
                0x11 => {
                    let v = read_u16(code, &mut pc)? as i16;
                    stack.push(Value::Int(v as i32));
                }
                0x15..=0x19 => {
                    let index = *code.get(pc).ok_or(NativeError::Truncated)? as usize;
                    pc += 1;
                    stack.push(locals.get(index).cloned().unwrap_or(Value::Ref(None)));
                }
                0x12 => {
                    let i = *code.get(pc).ok_or(NativeError::Truncated)? as u16;
                    pc += 1;
                    stack.push(constant_value(&file.pool, i)?);
                }
                0x13 => {
                    let i = read_u16(code, &mut pc)?;
                    stack.push(constant_value(&file.pool, i)?);
                }
                0x14 => {
                    let i = read_u16(code, &mut pc)?;
                    stack.push(constant_value(&file.pool, i)?);
                }
                0x60 | 0x61 | 0x64 | 0x65 | 0x68 | 0x69 | 0x6c | 0x6d | 0x70 | 0x71 => {
                    let rhs = pop_int(&mut stack, class, method)?;
                    let lhs = pop_int(&mut stack, class, method)?;
                    let value = match op {
                        0x60 | 0x61 => lhs.checked_add(rhs),
                        0x64 | 0x65 => lhs.checked_sub(rhs),
                        0x68 | 0x69 => lhs.checked_mul(rhs),
                        0x6c | 0x6d => lhs.checked_div(rhs),
                        0x70 | 0x71 => lhs.checked_rem(rhs),
                        _ => None,
                    }
                    .ok_or_else(|| NativeError::Arithmetic {
                        class: class.to_owned(),
                        method: method.to_owned(),
                    })?;
                    stack.push(Value::Int(value));
                }
                0x74 => {
                    let value = pop_int(&mut stack, class, method)?;
                    stack.push(Value::Int(value.wrapping_neg()));
                }
                0x78..=0x83 => {
                    let rhs = pop_int(&mut stack, class, method)?;
                    let lhs = pop_int(&mut stack, class, method)?;
                    let value = match op {
                        0x78 | 0x79 => {
                            lhs.wrapping_shl((rhs & if op == 0x79 { 0x3f } else { 0x1f }) as u32)
                        }
                        0x7a | 0x7b => {
                            lhs.wrapping_shr((rhs & if op == 0x7b { 0x3f } else { 0x1f }) as u32)
                        }
                        0x7c | 0x7d => {
                            ((lhs as u32) >> (rhs & if op == 0x7d { 0x3f } else { 0x1f })) as i32
                        }
                        0x7e | 0x7f => lhs & rhs,
                        0x80 | 0x81 => lhs | rhs,
                        0x82 | 0x83 => lhs ^ rhs,
                        _ => unreachable!(),
                    };
                    stack.push(Value::Int(value));
                }
                0x84 => {
                    let index = *code.get(pc).ok_or(NativeError::Truncated)? as usize;
                    let amount = *code.get(pc + 1).ok_or(NativeError::Truncated)? as i8 as i32;
                    pc += 2;
                    let value = match locals.get(index) {
                        Some(Value::Int(value)) => value.saturating_add(amount),
                        _ => {
                            return Err(NativeError::TypeMismatch {
                                class: class.to_owned(),
                                method: method.to_owned(),
                                expected: "int local",
                            });
                        }
                    };
                    if let Some(local) = locals.get_mut(index) {
                        *local = Value::Int(value);
                    } else {
                        return Err(NativeError::TypeMismatch {
                            class: class.to_owned(),
                            method: method.to_owned(),
                            expected: "valid local",
                        });
                    }
                }
                0x85..=0x93 => {
                    let value = pop_int(&mut stack, class, method)?;
                    let value = match op {
                        0x91 => (value as i8) as i32,
                        0x92 => (value as u8) as i32,
                        0x93 => (value as i16) as i32,
                        _ => value,
                    };
                    stack.push(Value::Int(value));
                }
                0x94..=0x98 => {
                    let rhs = pop_int(&mut stack, class, method)?;
                    let lhs = pop_int(&mut stack, class, method)?;
                    stack.push(Value::Int(lhs.cmp(&rhs) as i32));
                }
                0x1a..=0x1d => stack.push(locals[(op - 0x1a) as usize].clone()),
                0x1e..=0x21 => stack.push(locals[(op - 0x1e) as usize].clone()),
                0x22..=0x25 => stack.push(locals[(op - 0x22) as usize].clone()),
                0x26..=0x29 => stack.push(locals[(op - 0x26) as usize].clone()),
                0x2a..=0x2d => stack.push(locals[(op - 0x2a) as usize].clone()),
                0x3b..=0x3e => locals[(op - 0x3b) as usize] = pop(&mut stack, class, method)?,
                0x3f..=0x42 => locals[(op - 0x3f) as usize] = pop(&mut stack, class, method)?,
                0x43..=0x46 => locals[(op - 0x43) as usize] = pop(&mut stack, class, method)?,
                0x47..=0x4a => locals[(op - 0x47) as usize] = pop(&mut stack, class, method)?,
                0x4b..=0x4e => locals[(op - 0x4b) as usize] = pop(&mut stack, class, method)?,
                0x36..=0x3a => {
                    let index = *code.get(pc).ok_or(NativeError::Truncated)? as usize;
                    pc += 1;
                    let value = pop(&mut stack, class, method)?;
                    if let Some(local) = locals.get_mut(index) {
                        *local = value;
                    }
                }
                0x57 => {
                    let _ = pop(&mut stack, class, method)?;
                }
                0x58 => {
                    let _ = pop(&mut stack, class, method)?;
                    let _ = pop(&mut stack, class, method)?;
                }
                0x59 => {
                    let v = stack
                        .last()
                        .cloned()
                        .ok_or_else(|| underflow(class, method))?;
                    stack.push(v);
                }
                0x5a => {
                    let value1 = pop(&mut stack, class, method)?;
                    let value2 = pop(&mut stack, class, method)?;
                    stack.extend([value1.clone(), value2, value1]);
                }
                0x5b => {
                    let value1 = pop(&mut stack, class, method)?;
                    let value2 = pop(&mut stack, class, method)?;
                    let value3 = pop(&mut stack, class, method)?;
                    stack.extend([value1.clone(), value3, value2, value1]);
                }
                0x5c => {
                    let value1 = pop(&mut stack, class, method)?;
                    let value2 = pop(&mut stack, class, method)?;
                    stack.extend([value2.clone(), value1.clone(), value2, value1]);
                }
                0x5d => {
                    let value1 = pop(&mut stack, class, method)?;
                    let value2 = pop(&mut stack, class, method)?;
                    let value3 = pop(&mut stack, class, method)?;
                    stack.extend([value2.clone(), value1.clone(), value3, value2, value1]);
                }
                0x5e => {
                    let value1 = pop(&mut stack, class, method)?;
                    let value2 = pop(&mut stack, class, method)?;
                    let value3 = pop(&mut stack, class, method)?;
                    let value4 = pop(&mut stack, class, method)?;
                    stack.extend([
                        value2.clone(),
                        value1.clone(),
                        value4,
                        value3,
                        value2,
                        value1,
                    ]);
                }
                0x5f => {
                    let value1 = pop(&mut stack, class, method)?;
                    let value2 = pop(&mut stack, class, method)?;
                    stack.extend([value1, value2]);
                }
                0x99..=0x9e => {
                    let value = pop_int(&mut stack, class, method)?;
                    let take = match op {
                        0x99 => value == 0,
                        0x9a => value != 0,
                        0x9b => value < 0,
                        0x9c => value >= 0,
                        0x9d => value > 0,
                        _ => value <= 0,
                    };
                    let offset = read_i16(code, &mut pc)?;
                    if take {
                        pc = branch_target(code, at, offset, class, method)?;
                    }
                }
                0x9f..=0xa4 => {
                    let rhs = pop_int(&mut stack, class, method)?;
                    let lhs = pop_int(&mut stack, class, method)?;
                    let take = match op {
                        0x9f => lhs == rhs,
                        0xa0 => lhs != rhs,
                        0xa1 => lhs < rhs,
                        0xa2 => lhs >= rhs,
                        0xa3 => lhs > rhs,
                        _ => lhs <= rhs,
                    };
                    let offset = read_i16(code, &mut pc)?;
                    if take {
                        pc = branch_target(code, at, offset, class, method)?;
                    }
                }
                0xa5 | 0xa6 => {
                    let rhs = pop(&mut stack, class, method)?;
                    let lhs = pop(&mut stack, class, method)?;
                    let take = if op == 0xa5 { lhs == rhs } else { lhs != rhs };
                    let offset = read_i16(code, &mut pc)?;
                    if take {
                        pc = branch_target(code, at, offset, class, method)?;
                    }
                }
                0xa7 => {
                    let offset = read_i16(code, &mut pc)?;
                    pc = branch_target(code, at, offset, class, method)?;
                }
                0xaa | 0xab => {
                    let padding = (4 - (pc % 4)) % 4;
                    pc = pc.checked_add(padding).ok_or(NativeError::Truncated)?;
                    let default = read_i32(code, &mut pc)?;
                    let key = pop_int(&mut stack, class, method)?;
                    let offset = if op == 0xaa {
                        let low = read_i32(code, &mut pc)?;
                        let high = read_i32(code, &mut pc)?;
                        let count = high
                            .checked_sub(low)
                            .and_then(|span| span.checked_add(1))
                            .filter(|count| (0..=4096).contains(count))
                            .ok_or(NativeError::InvalidClass("invalid tableswitch range"))?
                            as usize;
                        let mut selected = default;
                        for index in 0..count {
                            let candidate = read_i32(code, &mut pc)?;
                            if low + index as i32 == key {
                                selected = candidate;
                            }
                        }
                        selected
                    } else {
                        let count = read_i32(code, &mut pc)?;
                        if !(0..=4096).contains(&count) {
                            return Err(NativeError::InvalidClass("invalid lookupswitch count"));
                        }
                        let mut selected = default;
                        for _ in 0..count {
                            let match_key = read_i32(code, &mut pc)?;
                            let candidate = read_i32(code, &mut pc)?;
                            if match_key == key {
                                selected = candidate;
                            }
                        }
                        selected
                    };
                    pc = branch_target_i32(code, at, offset, class, method)?;
                }
                0xc6 | 0xc7 => {
                    let value = pop(&mut stack, class, method)?;
                    let is_null = matches!(value, Value::Ref(None));
                    let take = if op == 0xc6 { is_null } else { !is_null };
                    let offset = read_i16(code, &mut pc)?;
                    if take {
                        pc = branch_target(code, at, offset, class, method)?;
                    }
                }
                0xbb => {
                    let i = read_u16(code, &mut pc)?;
                    stack.push(self.new_object(&class_name(&file.pool, i)?));
                }
                0xbc => {
                    let atype = *code.get(pc).ok_or(NativeError::Truncated)?;
                    pc += 1;
                    let length = pop_int(&mut stack, class, method)?;
                    let length = array_length(length)?;
                    if !(4..=11).contains(&atype) {
                        return Err(NativeError::InvalidClass("invalid newarray type"));
                    }
                    let marker = self.new_array(class, method);
                    self.arrays
                        .insert(marker.clone(), vec![Value::Int(0); length]);
                    stack.push(Value::Ref(Some(marker)));
                }
                0xbd => {
                    let i = read_u16(code, &mut pc)?;
                    let length = array_length(pop_int(&mut stack, class, method)?)?;
                    let _component = class_name(&file.pool, i)?;
                    let marker = self.new_array(class, method);
                    self.arrays
                        .insert(marker.clone(), vec![Value::Ref(None); length]);
                    stack.push(Value::Ref(Some(marker)));
                }
                0xbe => {
                    let array = pop(&mut stack, class, method)?;
                    let values = self.array(&array)?;
                    stack.push(Value::Int(values.len() as i32));
                }
                0x2e..=0x35 => {
                    let index = array_index(pop_int(&mut stack, class, method)?)?;
                    let array = pop(&mut stack, class, method)?;
                    let values = self.array(&array)?;
                    let value = values.get(index).cloned().ok_or_else(|| {
                        NativeError::Metadata(format!("array index {index} out of bounds"))
                    })?;
                    stack.push(value);
                }
                0x4f..=0x56 => {
                    let value = pop(&mut stack, class, method)?;
                    let index = array_index(pop_int(&mut stack, class, method)?)?;
                    let array = pop(&mut stack, class, method)?;
                    let marker = expect_array_marker(&array)?;
                    let values = self.arrays.get_mut(marker).ok_or_else(|| {
                        NativeError::Metadata("array is not owned by this invocation".into())
                    })?;
                    let slot = values.get_mut(index).ok_or_else(|| {
                        NativeError::Metadata(format!("array index {index} out of bounds"))
                    })?;
                    *slot = value;
                }
                0xc0 => {
                    // The bounded runtime does not model a JVM type lattice;
                    // consume the class operand and preserve the reference.
                    let _ = read_u16(code, &mut pc)?;
                    let _ = stack
                        .last()
                        .cloned()
                        .ok_or_else(|| underflow(class, method))?;
                }
                0xc1 => {
                    let _ = read_u16(code, &mut pc)?;
                    let _ = pop(&mut stack, class, method)?;
                    stack.push(Value::Int(0));
                }
                0xc2 | 0xc3 => {
                    // The bounded runtime is single-threaded; monitorenter
                    // and monitorexit consume their receiver without adding
                    // JVM monitor state.
                    let _ = pop(&mut stack, class, method)?;
                }
                0xb2 => {
                    let i = read_u16(code, &mut pc)?;
                    let field = file.field_ref(i)?;
                    let value = match self.static_fields.get(&Self::static_field_key(&field)) {
                        Some(value) => value.clone(),
                        None => self.host.get_static(&field)?,
                    };
                    stack.push(if value == Value::Ref(None) {
                        Value::default_for(&field.descriptor)
                    } else {
                        value
                    });
                }
                0xb3 => {
                    let i = read_u16(code, &mut pc)?;
                    let field = file.field_ref(i)?;
                    let value = pop(&mut stack, class, method)?;
                    self.static_fields
                        .insert(Self::static_field_key(&field), value);
                }
                0xb4 => {
                    let i = read_u16(code, &mut pc)?;
                    let r = file.field_ref(i)?;
                    let receiver = pop(&mut stack, class, method)?;
                    let value = match instance_field_key(&receiver, &r)
                        .and_then(|key| self.fields.get(&key).cloned())
                    {
                        Some(value) => value,
                        None => self.host.get_static(&r)?,
                    };
                    stack.push(if value == Value::Ref(None) {
                        Value::default_for(&r.descriptor)
                    } else {
                        value
                    });
                }
                0xb5 => {
                    let i = read_u16(code, &mut pc)?;
                    let r = file.field_ref(i)?;
                    let value = pop(&mut stack, class, method)?;
                    let receiver = pop(&mut stack, class, method)?;
                    if let Some(key) = instance_field_key(&receiver, &r) {
                        self.fields.insert(key, value);
                    }
                }
                0xb6..=0xb9 => {
                    let i = read_u16(code, &mut pc)?;
                    if op == 0xb9 {
                        // `invokeinterface` carries an argument-count byte and
                        // a required zero byte after its constant-pool index.
                        let _count = *code.get(pc).ok_or(NativeError::Truncated)?;
                        pc += 1;
                        let zero = *code.get(pc).ok_or(NativeError::Truncated)?;
                        pc += 1;
                        if zero != 0 {
                            return Err(NativeError::InvalidClass(
                                "invokeinterface trailing byte is not zero",
                            ));
                        }
                    }
                    let r = file.method_ref(i)?;
                    let n = arg_count(&r.descriptor);
                    let mut args = (0..n)
                        .map(|_| pop(&mut stack, class, method))
                        .collect::<Result<Vec<_>, _>>()?;
                    args.reverse();
                    let receiver = if op == 0xb8 {
                        None
                    } else {
                        Some(pop(&mut stack, class, method)?)
                    };
                    self.calls += 1;
                    let value = if r.owner == "java/lang/String"
                        && r.name == "format"
                        && (r.descriptor
                            == "(Ljava/lang/String;[Ljava/lang/Object;)Ljava/lang/String;"
                            || r.descriptor
                                == "(Ljava/util/Locale;Ljava/lang/String;[Ljava/lang/Object;)Ljava/lang/String;")
                    {
                        format_java_string_call(self, &r, &args)?
                    } else if let Some(value) =
                        self.invoke_reflect_array(&r, &args, class, method)?
                    {
                        value
                    } else if let Some(value) =
                        self.invoke_stream(&r, receiver.as_ref(), &args, class, method)?
                    {
                        value
                    } else if let Some(value) =
                        self.invoke_reflect_method(&r, receiver.as_ref(), &args)?
                    {
                        value
                    } else if r.owner == "java/lang/Class"
                        && r.name == "forName"
                        && r.descriptor.ends_with(")Ljava/lang/Class;")
                    {
                        self.class_for_name(&args)?
                    } else if self.classes.contains_key(&r.owner) {
                        self.invoke(&r.owner, &r.name, &r.descriptor, receiver, &args)?
                    } else {
                        let optional = receiver
                            .as_ref()
                            .map(|receiver| self.invoke_optional(&r, receiver, &args))
                            .transpose()?
                            .flatten();
                        if let Some(value) = optional {
                            value
                        } else if matches!(
                            r.owner.as_str(),
                            "java/util/function/Function"
                                | "java/util/function/UnaryOperator"
                                | "java/util/function/Consumer"
                                | "java/util/function/Predicate"
                                | "java/util/function/BiConsumer"
                                | "java/util/function/BiFunction"
                                | "java/util/function/Supplier"
                        ) {
                            if let Some(receiver) = receiver.as_ref() {
                                if let Some(value) = self.invoke_callback(receiver, &args)? {
                                    value
                                } else {
                                    self.host.invoke_instance(&r, receiver, &args)?
                                }
                            } else {
                                self.host.invoke(&r, &args)?
                            }
                        } else if let Some(receiver) = receiver.as_ref() {
                            self.host.invoke_instance(&r, receiver, &args)?
                        } else {
                            self.host.invoke(&r, &args)?
                        }
                    };
                    if value != Value::Void {
                        stack.push(value);
                    }
                }
                0xba => {
                    // Preserve captured values as a stable marker. This is
                    // enough for a host to record a callback registration,
                    // while full bootstrap-method execution remains outside
                    // the bounded interpreter.
                    let index = read_u16(code, &mut pc)?;
                    let zero = read_u16(code, &mut pc)?;
                    if zero != 0 {
                        return Err(NativeError::InvalidClass(
                            "invokedynamic trailing bytes are not zero",
                        ));
                    }
                    let descriptor = file.invoke_dynamic_descriptor(index)?;
                    let captures = arg_count(&descriptor);
                    let mut captured = (0..captures)
                        .map(|_| pop(&mut stack, class, method))
                        .collect::<Result<Vec<_>, _>>()?;
                    captured.reverse();
                    let target = file.invoke_dynamic_target(index)?.map(|target| {
                        format!("{}#{}{}", target.owner, target.name, target.descriptor)
                    });
                    let marker = format!(
                        "@indy:{class}#{method}:{index}:{}:{}",
                        target.as_deref().unwrap_or("?"),
                        captured
                            .iter()
                            .map(value_marker)
                            .collect::<Vec<_>>()
                            .join(",")
                    );
                    if return_desc(&descriptor) != "V" {
                        stack.push(Value::Ref(Some(marker)));
                    }
                }
                0xbf => {
                    // Exception paths are not modeled. Stop this bounded call
                    // with the declared neutral return value instead of
                    // leaking an unsupported bytecode failure.
                    if !stack.is_empty() {
                        let _ = stack.pop();
                    }
                    return Ok(Value::default_for(return_desc(desc)));
                }
                0xac..=0xb0 => return pop(&mut stack, class, method),
                0xb1 => return Ok(Value::Void),
                opcode => {
                    return Err(NativeError::UnsupportedOpcode {
                        class: class.to_owned(),
                        method: method.to_owned(),
                        opcode,
                        pc: at,
                    });
                }
            }
        }
        Err(NativeError::BadReturn {
            class: class.to_owned(),
            method: method.to_owned(),
        })
    }
}

fn underflow(class: &str, method: &str) -> NativeError {
    NativeError::StackUnderflow {
        class: class.to_owned(),
        method: method.to_owned(),
    }
}
fn pop(stack: &mut Vec<Value>, class: &str, method: &str) -> Result<Value, NativeError> {
    stack.pop().ok_or_else(|| underflow(class, method))
}

fn pop_int(stack: &mut Vec<Value>, class: &str, method: &str) -> Result<i32, NativeError> {
    match pop(stack, class, method)? {
        Value::Int(value) => Ok(value),
        _ => Err(NativeError::TypeMismatch {
            class: class.to_owned(),
            method: method.to_owned(),
            expected: "int",
        }),
    }
}

fn branch_target(
    code: &[u8],
    instruction: usize,
    offset: i16,
    class: &str,
    method: &str,
) -> Result<usize, NativeError> {
    let target = instruction as isize + offset as isize;
    if target < 0 || target as usize >= code.len() {
        return Err(NativeError::InvalidBranch {
            class: class.to_owned(),
            method: method.to_owned(),
            target,
        });
    }
    Ok(target as usize)
}

fn branch_target_i32(
    code: &[u8],
    instruction: usize,
    offset: i32,
    class: &str,
    method: &str,
) -> Result<usize, NativeError> {
    let target = instruction as isize + offset as isize;
    if target < 0 || target as usize >= code.len() {
        return Err(NativeError::InvalidBranch {
            class: class.to_owned(),
            method: method.to_owned(),
            target,
        });
    }
    Ok(target as usize)
}

fn return_desc(desc: &str) -> &str {
    desc.rsplit_once(')').map_or("V", |(_, ret)| ret)
}
fn arg_count(desc: &str) -> usize {
    let Some(end) = desc.find(')') else { return 0 };
    let b = desc.as_bytes();
    let mut i = 1;
    let mut n = 0;
    while i < end {
        while i < end && b[i] == b'[' {
            i += 1;
        }
        if i >= end {
            break;
        }
        if b[i] == b'L' {
            while i < end && b[i] != b';' {
                i += 1;
            }
        }
        i += 1;
        n += 1;
    }
    n
}

fn descriptor_arguments(desc: &str) -> Vec<String> {
    let Some(end) = desc.find(')') else {
        return Vec::new();
    };
    let bytes = desc.as_bytes();
    let mut at = 1;
    let mut arguments = Vec::new();
    while at < end {
        let start = at;
        while at < end && bytes[at] == b'[' {
            at += 1;
        }
        if at >= end {
            break;
        }
        if bytes[at] == b'L' {
            while at < end && bytes[at] != b';' {
                at += 1;
            }
            at = (at + 1).min(end);
        } else {
            at += 1;
        }
        arguments.push(desc[start..at].to_owned());
    }
    arguments
}

fn is_callback_info_descriptor(desc: &str) -> bool {
    matches!(
        desc,
        "Lorg/spongepowered/asm/mixin/injection/callback/CallbackInfo;"
            | "Lorg/spongepowered/asm/mixin/injection/callback/CallbackInfoReturnable;"
    )
}

fn neutral_args(desc: &str) -> Vec<Value> {
    let Some(end) = desc.find(')') else {
        return Vec::new();
    };
    let bytes = desc.as_bytes();
    let mut at = 1;
    let mut args = Vec::new();
    while at < end {
        let start = at;
        while at < end && bytes[at] == b'[' {
            at += 1;
        }
        if at >= end {
            break;
        }
        if bytes[at] == b'L' {
            while at < end && bytes[at] != b';' {
                at += 1;
            }
            at = (at + 1).min(end);
            args.push(Value::Ref(Some(desc[start..at].to_owned())));
        } else {
            at += 1;
            args.push(Value::Int(0));
        }
    }
    args
}
fn read_u16(code: &[u8], pc: &mut usize) -> Result<u16, NativeError> {
    let b = code.get(*pc..*pc + 2).ok_or(NativeError::Truncated)?;
    *pc += 2;
    Ok(u16::from_be_bytes([b[0], b[1]]))
}
fn read_i16(code: &[u8], pc: &mut usize) -> Result<i16, NativeError> {
    Ok(read_u16(code, pc)? as i16)
}
fn read_i32(code: &[u8], pc: &mut usize) -> Result<i32, NativeError> {
    let b = code.get(*pc..*pc + 4).ok_or(NativeError::Truncated)?;
    *pc += 4;
    Ok(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn constant_value(pool: &[Cp], index: u16) -> Result<Value, NativeError> {
    match pool.get(index as usize) {
        Some(Cp::Class(_)) => Ok(Value::Ref(Some(class_name(pool, index)?))),
        Some(Cp::String(i)) => match pool.get(*i as usize) {
            Some(Cp::Utf8(s)) => Ok(Value::Ref(Some(s.clone()))),
            x => Err(NativeError::InvalidConstant(index, format!("{x:?}"))),
        },
        Some(Cp::Integer(v)) => Ok(Value::Int(*v)),
        Some(Cp::Float(v)) => Ok(Value::Int(*v as i32)),
        Some(Cp::Long(v)) => Ok(Value::Int(
            (*v).clamp(i32::MIN as i64, i32::MAX as i64) as i32
        )),
        x => Err(NativeError::InvalidConstant(index, format!("{x:?}"))),
    }
}
fn class_name(pool: &[Cp], index: u16) -> Result<String, NativeError> {
    match pool.get(index as usize) {
        Some(Cp::Class(i)) => match pool.get(*i as usize) {
            Some(Cp::Utf8(s)) => Ok(s.clone()),
            x => Err(NativeError::InvalidConstant(index, format!("{x:?}"))),
        },
        x => Err(NativeError::InvalidConstant(index, format!("{x:?}"))),
    }
}
fn name_type(pool: &[Cp], index: u16) -> Result<(String, String), NativeError> {
    match pool.get(index as usize) {
        Some(Cp::NameType { name, descriptor }) => {
            Ok((utf8(pool, *name)?, utf8(pool, *descriptor)?))
        }
        x => Err(NativeError::InvalidConstant(index, format!("{x:?}"))),
    }
}
fn utf8(pool: &[Cp], index: u16) -> Result<String, NativeError> {
    match pool.get(index as usize) {
        Some(Cp::Utf8(s)) => Ok(s.clone()),
        x => Err(NativeError::InvalidConstant(index, format!("{x:?}"))),
    }
}

fn parse_metadata(
    text: &str,
) -> Result<
    (
        Option<String>,
        Option<String>,
        BTreeMap<String, Vec<Entrypoint>>,
        Vec<String>,
    ),
    NativeError,
> {
    let v: serde_json::Value =
        serde_json::from_str(text).map_err(|e| NativeError::Metadata(e.to_string()))?;
    let id = v.get("id").and_then(|x| x.as_str()).map(str::to_owned);
    let version = v.get("version").and_then(|x| match x {
        serde_json::Value::String(s) => Some(s.to_owned()),
        serde_json::Value::Object(o) => o
            .get("semver")
            .or_else(|| o.get("raw"))
            .and_then(|x| x.as_str())
            .map(str::to_owned),
        _ => None,
    });
    let mut required_mods = v
        .get("depends")
        .and_then(serde_json::Value::as_object)
        .into_iter()
        .flatten()
        .map(|(id, _)| id.clone())
        .filter(|id| {
            !matches!(
                id.as_str(),
                "minecraft" | "fabricloader" | "java" | "neoforge" | "forge"
            )
        })
        .collect::<Vec<_>>();
    required_mods.sort();
    required_mods.dedup();
    let mut entrypoints = BTreeMap::new();
    if let Some(groups) = v.get("entrypoints").and_then(serde_json::Value::as_object) {
        for (kind, group) in groups {
            let values = match group {
                serde_json::Value::Array(values) => values.clone(),
                value => vec![value.clone()],
            };
            let mut entries = Vec::new();
            for value in values {
                let Some(spec) = value.as_str().map(str::to_owned).or_else(|| {
                    value
                        .get("value")
                        .and_then(|v| v.as_str())
                        .map(str::to_owned)
                }) else {
                    continue;
                };
                let (class, method) = spec
                    .split_once("::")
                    .map_or((spec.as_str(), None), |(class, method)| {
                        (class, Some(method))
                    });
                entries.push(Entrypoint {
                    class: class.to_owned(),
                    method: method.map(str::to_owned),
                    adapter: value
                        .get("adapter")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                });
            }
            entrypoints.insert(kind.clone(), entries);
        }
    }
    Ok((id, version, entrypoints, required_mods))
}

/// Forge 1.x and NeoForge use the same `@Mod` bootstrap convention, but the
/// annotation moved packages when NeoForge forked. Keep both markers so a
/// legacy Forge 26.3 jar is discoverable by the bounded native probe.
fn is_mod_annotation(value: &str) -> bool {
    matches!(
        value,
        "Lnet/neoforged/fml/common/Mod;" | "Lnet/minecraftforge/fml/common/Mod;"
    )
}

/// Read the small, stable part of NeoForge's loader metadata needed by the
/// native launcher.  This intentionally accepts only the first `[[mods]]`
/// record and required dependency records; the full loader validates version
/// ranges and display metadata elsewhere.
fn parse_neoforge_metadata(
    text: &str,
) -> Result<(Option<String>, Option<String>, Vec<String>, bool), NativeError> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Section {
        None,
        Mods,
        Dependency,
    }
    let mut section = Section::None;
    let mut id = None;
    let mut version = None;
    let mut dependency = None;
    let mut dependency_required = false;
    let mut required = Vec::new();
    let mut client_side_only = false;
    let mut flush = |dependency: &mut Option<String>, required_flag: &mut bool| {
        if *required_flag {
            if let Some(id) = dependency.take() {
                if !matches!(
                    id.as_str(),
                    "minecraft" | "neoforge" | "forge" | "java" | "fabricloader"
                ) {
                    required.push(id);
                }
            }
        } else {
            dependency.take();
        }
        *required_flag = false;
    };
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("[[mods]]") {
            if section == Section::Dependency {
                flush(&mut dependency, &mut dependency_required);
            }
            section = if id.is_none() {
                Section::Mods
            } else {
                Section::None
            };
            continue;
        }
        if line.starts_with("[[dependencies.") {
            if section == Section::Dependency {
                flush(&mut dependency, &mut dependency_required);
            }
            section = Section::Dependency;
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim().trim_matches('"').trim_matches('\'');
        match (section, key) {
            (Section::Mods, "modId") if id.is_none() => id = Some(value.to_owned()),
            (Section::Mods, "version") if version.is_none() => version = Some(value.to_owned()),
            (Section::Mods, "clientSideOnly") => {
                client_side_only = value.eq_ignore_ascii_case("true");
            }
            (Section::Dependency, "modId") => dependency = Some(value.to_owned()),
            (Section::Dependency, "type") => {
                dependency_required |= value.eq_ignore_ascii_case("required")
            }
            (Section::Dependency, "mandatory") => {
                dependency_required |= value.eq_ignore_ascii_case("true")
            }
            _ => {}
        }
    }
    if section == Section::Dependency {
        flush(&mut dependency, &mut dependency_required);
    }
    required.sort();
    required.dedup();
    Ok((id, version, required, client_side_only))
}

fn parse_native_metadata(text: &str) -> Result<(Option<String>, Vec<RegistryEntry>), NativeError> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| NativeError::Metadata(e.to_string()))?;
    if !value.is_object() {
        return Err(NativeError::Metadata(
            "native metadata root is not an object".into(),
        ));
    }
    let id = match value.get("id") {
        None => None,
        Some(value) => Some(
            value
                .as_str()
                .ok_or_else(|| NativeError::Metadata("native metadata id is not a string".into()))?
                .to_owned(),
        ),
    };
    let entries = match value.get("registrations") {
        None => return Ok((id, Vec::new())),
        Some(value) => value.as_array().ok_or_else(|| {
            NativeError::Metadata("native metadata registrations is not an array".into())
        })?,
    };
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let object = entry
            .as_object()
            .ok_or_else(|| NativeError::Metadata("native registration is not an object".into()))?;
        let required = |name: &str| {
            object
                .get(name)
                .and_then(|v| v.as_str())
                .map(str::to_owned)
                .ok_or_else(|| NativeError::Metadata(format!("native registration missing {name}")))
        };
        let registry = required("registry")?;
        let entry_id = required("id")?;
        let kind = required("kind")?;
        let properties = match object.get("properties") {
            None => BTreeMap::new(),
            Some(value) => {
                let values = value.as_object().ok_or_else(|| {
                    NativeError::Metadata("native registration properties is not an object".into())
                })?;
                values
                    .iter()
                    .map(|(key, value)| {
                        let value = match value {
                            serde_json::Value::String(value) => Ok(value.clone()),
                            serde_json::Value::Bool(value) => Ok(value.to_string()),
                            serde_json::Value::Number(value) => Ok(value.to_string()),
                            _ => Err(NativeError::Metadata(format!(
                                "native registration property {key} is not a string, boolean, or number"
                            ))),
                        }?;
                        Ok((key.clone(), value))
                    })
                    .collect::<Result<BTreeMap<_, _>, NativeError>>()?
            }
        };
        let item = match object.get("item") {
            None => true,
            Some(value) => value.as_bool().ok_or_else(|| {
                NativeError::Metadata("native block item is not a boolean".into())
            })?,
        };
        let declaration = match kind.as_str() {
            "block" => RegistryDeclaration::Block {
                template: required_from(object, "template")?,
                properties,
                item,
            },
            "item" => RegistryDeclaration::Item {
                template: required_from(object, "template")?,
                properties,
            },
            "mob_effect" | "mob-effect" => RegistryDeclaration::MobEffect {
                category: required_from(object, "category")?,
                color: object
                    .get("color")
                    .and_then(|v| v.as_u64())
                    .and_then(|v| u32::try_from(v).ok())
                    .ok_or_else(|| {
                        NativeError::Metadata("native effect color is invalid".into())
                    })?,
            },
            "recipe" => {
                let json = object
                    .get("recipe")
                    .or_else(|| object.get("json"))
                    .ok_or_else(|| NativeError::Metadata("native recipe is missing json".into()))?;
                if !json.is_object() {
                    return Err(NativeError::Metadata(
                        "native recipe json is not an object".into(),
                    ));
                }
                RegistryDeclaration::Recipe {
                    json: serde_json::to_string(json)
                        .map_err(|e| NativeError::Metadata(e.to_string()))?,
                }
            }
            "ingredient" => {
                let json = object
                    .get("ingredient")
                    .or_else(|| object.get("json"))
                    .ok_or_else(|| {
                        NativeError::Metadata("native ingredient is missing json".into())
                    })?;
                if !json.is_object() {
                    return Err(NativeError::Metadata(
                        "native ingredient json is not an object".into(),
                    ));
                }
                RegistryDeclaration::Ingredient {
                    json: serde_json::to_string(json)
                        .map_err(|e| NativeError::Metadata(e.to_string()))?,
                }
            }
            "custom" => RegistryDeclaration::Custom {
                kind: required_from(object, "custom_kind")?,
                properties,
            },
            other => {
                return Err(NativeError::Metadata(format!(
                    "unknown native registration kind {other}"
                )));
            }
        };
        out.push(RegistryEntry::typed(registry, entry_id, declaration));
    }
    Ok((id, out))
}

fn required_from(
    object: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<String, NativeError> {
    object
        .get(name)
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .ok_or_else(|| NativeError::Metadata(format!("native registration missing {name}")))
}

fn read_pool(r: &mut Reader<'_>) -> Result<Vec<Cp>, NativeError> {
    let count = r.u2()? as usize;
    let mut pool = vec![Cp::Empty];
    let mut i = 1;
    while i < count {
        let tag = r.u1()?;
        let c = match tag {
            1 => {
                let length = r.u2()? as usize;
                Cp::Utf8(String::from_utf8_lossy(r.bytes(length)?).into_owned())
            }
            3 => Cp::Integer(r.u4()? as i32),
            7 => Cp::Class(r.u2()?),
            8 => Cp::String(r.u2()?),
            9 => Cp::FieldRef {
                class: r.u2()?,
                nt: r.u2()?,
            },
            10 => Cp::MethodRef {
                class: r.u2()?,
                nt: r.u2()?,
            },
            11 => Cp::InterfaceMethodRef {
                class: r.u2()?,
                nt: r.u2()?,
            },
            12 => Cp::NameType {
                name: r.u2()?,
                descriptor: r.u2()?,
            },
            4 => Cp::Float(f32::from_bits(r.u4()?)),
            5 => {
                let high = r.u4()? as u64;
                let low = r.u4()? as u64;
                let value = ((high << 32) | low) as i64;
                Cp::Long(value)
            }
            6 => {
                // Doubles use the same two-slot pool shape. The bounded
                // value model keeps a neutral integer placeholder.
                r.skip(8)?;
                Cp::Long(0)
            }
            15 => {
                let _ = r.u1()?;
                Cp::MethodHandle {
                    reference_index: r.u2()?,
                }
            }
            16 => {
                let _ = r.u2()?;
                Cp::MethodType
            }
            19 | 20 => {
                r.skip(2)?;
                Cp::Empty
            }
            17 | 18 => {
                if tag == 18 {
                    Cp::InvokeDynamic {
                        bootstrap: r.u2()?,
                        nt: r.u2()?,
                    }
                } else {
                    r.skip(4)?;
                    Cp::Empty
                }
            }
            _ => return Err(NativeError::InvalidClass("unsupported constant tag")),
        };
        pool.push(c);
        if tag == 5 || tag == 6 {
            pool.push(Cp::Empty);
            i += 1;
        }
        i += 1;
    }
    Ok(pool)
}
fn read_fields(r: &mut Reader<'_>, pool: &[Cp]) -> Result<Vec<Field>, NativeError> {
    let n = r.u2()? as usize;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let access = r.u2()?;
        let name = utf8(pool, r.u2()?)?;
        let descriptor = utf8(pool, r.u2()?)?;
        skip_attributes(r, pool)?;
        out.push(Field {
            name,
            descriptor,
            access,
        });
    }
    Ok(out)
}
fn read_methods(r: &mut Reader<'_>, pool: &[Cp]) -> Result<Vec<Method>, NativeError> {
    let n = r.u2()? as usize;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let access = r.u2()?;
        let name = utf8(pool, r.u2()?)?;
        let descriptor = utf8(pool, r.u2()?)?;
        let attrs = r.u2()? as usize;
        let mut code = None;
        let mut max_locals = 0;
        for _ in 0..attrs {
            let attr = utf8(pool, r.u2()?)?;
            let len = r.u4()? as usize;
            let bytes = r.bytes(len)?;
            if attr == "Code" {
                let mut c = Reader::new(bytes);
                let _ = c.u2()?;
                max_locals = c.u2()?;
                let len = c.u4()? as usize;
                code = Some(c.bytes(len)?.to_owned());
            }
        }
        out.push(Method {
            name,
            descriptor,
            access,
            code,
            max_locals,
        });
    }
    Ok(out)
}
fn skip_attributes(r: &mut Reader<'_>, pool: &[Cp]) -> Result<(), NativeError> {
    let n = r.u2()? as usize;
    for _ in 0..n {
        let _ = utf8(pool, r.u2()?)?;
        let length = r.u4()? as usize;
        r.skip(length)?;
    }
    Ok(())
}

fn read_bootstrap_methods(
    r: &mut Reader<'_>,
    pool: &[Cp],
) -> Result<Vec<BootstrapMethod>, NativeError> {
    let count = r.u2()? as usize;
    let mut methods = Vec::new();
    for _ in 0..count {
        let name = utf8(pool, r.u2()?)?;
        let length = r.u4()? as usize;
        let bytes = r.bytes(length)?;
        if name != "BootstrapMethods" {
            continue;
        }
        let mut body = Reader::new(bytes);
        let entries = body.u2()? as usize;
        methods.reserve(entries);
        for _ in 0..entries {
            let method_ref = body.u2()?;
            let argument_count = body.u2()? as usize;
            let mut arguments = Vec::with_capacity(argument_count);
            for _ in 0..argument_count {
                arguments.push(body.u2()?);
            }
            methods.push(BootstrapMethod {
                method_ref,
                arguments,
            });
        }
    }
    Ok(methods)
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], NativeError> {
        let end = self.at.checked_add(n).ok_or(NativeError::Truncated)?;
        let b = self.bytes.get(self.at..end).ok_or(NativeError::Truncated)?;
        self.at = end;
        Ok(b)
    }
    fn skip(&mut self, n: usize) -> Result<(), NativeError> {
        self.bytes(n).map(|_| ())
    }
    fn u1(&mut self) -> Result<u8, NativeError> {
        Ok(self.bytes(1)?[0])
    }
    fn u2(&mut self) -> Result<u16, NativeError> {
        let b = self.bytes(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }
    fn u4(&mut self) -> Result<u32, NativeError> {
        let b = self.bytes(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn string_format_supports_loader_diagnostic_subset() {
        let mut host = NullHost;
        let runtime = Runtime {
            classes: &BTreeMap::new(),
            host: &mut host,
            calls: 0,
            fields: BTreeMap::new(),
            static_fields: BTreeMap::new(),
            initialized_classes: std::collections::BTreeSet::new(),
            arrays: BTreeMap::from([(
                "array/test/args/0".into(),
                vec![Value::Int(7), Value::Ref(Some("ok".into()))],
            )]),
            next_array: 0,
            next_object: 0,
            streams: BTreeMap::new(),
            next_stream: 0,
        };
        let method = MethodRef {
            owner: "java/lang/String".into(),
            name: "format".into(),
            descriptor: "(Ljava/lang/String;[Ljava/lang/Object;)Ljava/lang/String;".into(),
        };
        let value = format_java_string_call(
            &runtime,
            &method,
            &[
                Value::Ref(Some("%d/%s%%".into())),
                Value::Ref(Some("array/test/args/0".into())),
            ],
        )
        .unwrap();
        assert_eq!(value, Value::Ref(Some("7/ok%".into())));
    }

    #[test]
    fn string_format_rejects_widths_explicitly() {
        let mut host = NullHost;
        let runtime = Runtime {
            classes: &BTreeMap::new(),
            host: &mut host,
            calls: 0,
            fields: BTreeMap::new(),
            static_fields: BTreeMap::new(),
            initialized_classes: std::collections::BTreeSet::new(),
            arrays: BTreeMap::from([("array/test/args/0".into(), vec![Value::Int(7)])]),
            next_array: 0,
            next_object: 0,
            streams: BTreeMap::new(),
            next_stream: 0,
        };
        let method = MethodRef {
            owner: "java/lang/String".into(),
            name: "format".into(),
            descriptor: "(Ljava/lang/String;[Ljava/lang/Object;)Ljava/lang/String;".into(),
        };
        let error = format_java_string_call(
            &runtime,
            &method,
            &[
                Value::Ref(Some("%03d".into())),
                Value::Ref(Some("array/test/args/0".into())),
            ],
        )
        .unwrap_err();
        assert!(
            matches!(error, NativeError::Metadata(message) if message.contains("unsupported String.format conversion %0"))
        );
    }

    #[test]
    fn descriptor_count_handles_arrays() {
        assert_eq!(arg_count("(I[Ljava/lang/String;)V"), 2);
    }
    #[test]
    fn metadata_accepts_object_version() {
        assert_eq!(
            parse_metadata(r#"{"id":"demo","version":{"semver":"1.2.3"},"entrypoints":{"client":"demo.Client::boot"}}"#).unwrap(),
            (
                Some("demo".into()),
                Some("1.2.3".into()),
                BTreeMap::from([(
                    "client".into(),
                    vec![Entrypoint {
                            class: "demo.Client".into(),
                        method: Some("boot".into()),
                        adapter: None,
                    }]
                )]),
                Vec::<String>::new(),
            )
        );
    }

    #[test]
    fn metadata_collects_required_mod_ids_for_native_loader_seeding() {
        let (_, _, _, required) = parse_metadata(
            r#"{"id":"demo","version":"1","depends":{"minecraft":"1.21","fabricloader":"*","neoforge":"*","forge":"*","fabric-api":"*","cloth-config":"*"}}"#,
        )
        .unwrap();
        assert_eq!(required, ["cloth-config", "fabric-api"]);
    }

    #[test]
    fn metadata_retains_fabric_entrypoint_adapter_for_loader_diagnostics() {
        let (_, _, entrypoints, _) = parse_metadata(
            r#"{"id":"demo","entrypoints":{"main":[{"value":"demo.Main","adapter":"net.fabricmc.api.ModInitializer"}]}}"#,
        )
        .unwrap();
        assert_eq!(
            entrypoints["main"][0],
            Entrypoint {
                class: "demo.Main".into(),
                method: None,
                adapter: Some("net.fabricmc.api.ModInitializer".into()),
            }
        );
    }

    #[test]
    fn neoforge_metadata_collects_identity_and_required_mod_ids() {
        let (id, version, required, client_side_only) = parse_neoforge_metadata(
            r#"
modLoader = "javafml"

[[mods]]
clientSideOnly = true
modId = "ruby"
version = "1.2.3"

[[dependencies.ruby]]
modId = "neoforge"
type = "required"

[[dependencies.ruby]]
modId = "geckolib"
type = "required"

[[dependencies.ruby]]
modId = "jei"
type = "optional"

[[dependencies.ruby]]
modId = "curios"
mandatory = true

[[dependencies.ruby]]
modId = "fabric-api"
mandatory = true
"#,
        )
        .unwrap();
        assert_eq!(id.as_deref(), Some("ruby"));
        assert_eq!(version.as_deref(), Some("1.2.3"));
        assert_eq!(required, ["curios", "fabric-api", "geckolib"]);
        assert!(client_side_only);
    }

    #[test]
    fn jar_reads_neoforge_metadata_for_native_loader_seeding() {
        let path = std::env::temp_dir().join(format!(
            "horizon-native-neoforge-metadata-{}.jar",
            std::process::id()
        ));
        let file = File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file(
            "META-INF/neoforge.mods.toml",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(
            br#"[[mods]]
modId = "ruby"
version = "1.2.3"
[[dependencies.ruby]]
modId = "neoforge"
type = "required"
[[dependencies.ruby]]
modId = "geckolib"
mandatory = true
"#,
        )
        .unwrap();
        zip.finish().unwrap();
        let jar = NativeJar::from_path(&path).unwrap();
        assert_eq!(jar.neoforge_id.as_deref(), Some("ruby"));
        assert_eq!(jar.neoforge_version.as_deref(), Some("1.2.3"));
        assert_eq!(jar.required_mods, ["geckolib"]);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn jar_reads_legacy_forge_mods_toml_for_native_loader_seeding() {
        let path = std::env::temp_dir().join(format!(
            "horizon-native-forge-metadata-{}.jar",
            std::process::id()
        ));
        let file = File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file(
            "META-INF/mods.toml",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(
            br#"[[mods]]
modId = "legacy"
version = "1.0"
[[dependencies.legacy]]
modId = "library"
mandatory = true
"#,
        )
        .unwrap();
        zip.finish().unwrap();
        let jar = NativeJar::from_path(&path).unwrap();
        assert_eq!(jar.neoforge_id.as_deref(), Some("legacy"));
        assert_eq!(jar.neoforge_version.as_deref(), Some("1.0"));
        assert_eq!(jar.required_mods, ["library"]);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn jar_reads_typed_native_metadata() {
        let path = std::env::temp_dir().join(format!(
            "horizon-native-metadata-{}.jar",
            std::process::id()
        ));
        let file = File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file(
            "META-INF/horizon-native.json",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(
            br##"{"id":"ruby","registrations":[{"registry":"minecraft:item","id":"ruby:wand","kind":"item","template":"minecraft:stick"},{"registry":"minecraft:recipe","id":"ruby:lamp","kind":"recipe","recipe":{"type":"minecraft:crafting_shaped","pattern":["#"]}}]}"##,
        )
        .unwrap();
        zip.finish().unwrap();
        let jar = NativeJar::from_path(&path).unwrap();
        assert_eq!(jar.native_id.as_deref(), Some("ruby"));
        assert_eq!(jar.native_declarations.len(), 2);
        assert_eq!(jar.native_declarations[0].id, "ruby:wand");
        assert!(matches!(
            jar.native_declarations[1].declaration,
            Some(RegistryDeclaration::Recipe { .. })
        ));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn jar_seeds_bundled_resources_into_the_bounded_host() {
        let path = std::env::temp_dir().join(format!(
            "horizon-native-resource-seeding-{}.jar",
            std::process::id()
        ));
        let file = File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("fabric.mod.json", options).unwrap();
        zip.write_all(
            br#"{"id":"resource-fixture","version":"1.0","entrypoints":{"main":"demo.Main"}}"#,
        )
        .unwrap();
        zip.start_file("assets/resource-fixture/lang/en_us.json", options)
            .unwrap();
        zip.write_all(br#"{"item.resource-fixture.wand":"Wand"}"#)
            .unwrap();
        zip.start_file("data/resource-fixture/recipes/wand.json", options)
            .unwrap();
        zip.write_all(br#"{"type":"minecraft:crafting_shaped"}"#)
            .unwrap();
        zip.finish().unwrap();

        let jar = NativeJar::from_path(&path).unwrap();
        let names = jar.resource_names().collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "assets/resource-fixture/lang/en_us.json",
                "data/resource-fixture/recipes/wand.json",
                "fabric.mod.json",
            ]
        );
        let mut host = jar.seed_host(StdHost::new());
        assert_eq!(
            host.resource("assets/resource-fixture/lang/en_us.json"),
            Some("jar-resource/assets/resource-fixture/lang/en_us.json")
        );
        assert_eq!(
            host.resource("/data/resource-fixture/recipes/wand.json"),
            Some("jar-resource/data/resource-fixture/recipes/wand.json")
        );
        let stream = MethodRef {
            owner: "java/lang/ClassLoader".into(),
            name: "getSystemResourceAsStream".into(),
            descriptor: "(Ljava/lang/String;)Ljava/io/InputStream;".into(),
        };
        let stream_value = host
            .invoke(
                &stream,
                &[Value::Ref(Some(
                    "assets/resource-fixture/lang/en_us.json".into(),
                ))],
            )
            .unwrap();
        let read = MethodRef {
            owner: "java/io/InputStream".into(),
            name: "read".into(),
            descriptor: "()I".into(),
        };
        assert_eq!(
            host.invoke_instance(&read, &stream_value, &[]).unwrap(),
            Value::Int(i32::from(b'{'))
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn unsupported_opcode_is_explicit() {
        let class = ClassFile {
            name: "demo/Main".into(),
            super_name: "java/lang/Object".into(),
            pool: vec![Cp::Empty],
            bootstrap_methods: Vec::new(),
            fields: Vec::new(),
            methods: vec![Method {
                name: "onInitialize".into(),
                descriptor: "()V".into(),
                access: 9,
                code: Some(vec![0xfe]),
                max_locals: 0,
            }],
        };
        let jar = NativeJar::from_classes([class]);
        let e = jar
            .execute("demo/Main", None, "main", &mut NullHost)
            .unwrap_err();
        assert!(matches!(
            e,
            NativeError::UnsupportedOpcode { opcode: 0xfe, .. }
        ));
    }

    #[test]
    fn arithmetic_and_conditional_branch_execute() {
        let class = ClassFile {
            name: "demo/Math".into(),
            super_name: "java/lang/Object".into(),
            pool: vec![Cp::Empty],
            bootstrap_methods: Vec::new(),
            fields: Vec::new(),
            methods: vec![
                Method {
                    name: "sum".into(),
                    descriptor: "()I".into(),
                    access: 9,
                    code: Some(vec![0x05, 0x06, 0x60, 0xac]),
                    max_locals: 0,
                },
                Method {
                    name: "branch".into(),
                    descriptor: "()I".into(),
                    access: 9,
                    code: Some(vec![0x03, 0x99, 0, 5, 0x04, 0xac, 0x08, 0xac]),
                    max_locals: 0,
                },
                Method {
                    name: "narrow".into(),
                    descriptor: "()I".into(),
                    access: 9,
                    code: Some(vec![0x02, 0x91, 0xac]),
                    max_locals: 0,
                },
                Method {
                    name: "same".into(),
                    descriptor: "(Ljava/lang/Object;Ljava/lang/Object;)I".into(),
                    access: 9,
                    code: Some(vec![0x2a, 0x2b, 0xa5, 0, 5, 0x03, 0xac, 0x04, 0xac]),
                    max_locals: 2,
                },
            ],
        };
        let jar = NativeJar::from_classes([class]);
        let mut host = NullHost;
        assert_eq!(
            jar.invoke_method("demo.Math", "sum", "()I", None, &mut host)
                .unwrap(),
            Value::Int(5)
        );
        assert_eq!(
            jar.invoke_method("demo.Math", "branch", "()I", None, &mut host)
                .unwrap(),
            Value::Int(5)
        );
        assert_eq!(
            jar.invoke_method("demo.Math", "narrow", "()I", None, &mut host)
                .unwrap(),
            Value::Int(-1)
        );
        assert_eq!(
            jar.invoke_method_with_args(
                "demo.Math",
                "same",
                "(Ljava/lang/Object;Ljava/lang/Object;)I",
                None,
                &[
                    Value::Ref(Some("same".into())),
                    Value::Ref(Some("same".into()))
                ],
                &mut host,
            )
            .unwrap(),
            Value::Int(1)
        );
        assert_eq!(
            jar.invoke_method_with_args(
                "demo.Math",
                "same",
                "(Ljava/lang/Object;Ljava/lang/Object;)I",
                None,
                &[
                    Value::Ref(Some("left".into())),
                    Value::Ref(Some("right".into()))
                ],
                &mut host,
            )
            .unwrap(),
            Value::Int(0)
        );
    }

    #[test]
    fn stack_duplication_and_swap_opcodes_execute() {
        let method = |name: &str, code: Vec<u8>| Method {
            name: name.into(),
            descriptor: "()I".into(),
            access: 9,
            code: Some(code),
            max_locals: 0,
        };
        let class = ClassFile {
            name: "demo/Stack".into(),
            super_name: "java/lang/Object".into(),
            pool: vec![Cp::Empty],
            bootstrap_methods: Vec::new(),
            fields: Vec::new(),
            methods: vec![
                method("pop2", vec![0x04, 0x05, 0x06, 0x58, 0xac]),
                method("dup_x1", vec![0x04, 0x05, 0x5a, 0x57, 0x60, 0xac]),
                method(
                    "dup_x2",
                    vec![0x04, 0x05, 0x06, 0x5b, 0x57, 0x57, 0x60, 0xac],
                ),
                method("dup2", vec![0x04, 0x05, 0x5c, 0x57, 0x57, 0x60, 0xac]),
                method(
                    "dup2_x1",
                    vec![0x04, 0x05, 0x06, 0x5d, 0x57, 0x57, 0x60, 0xac],
                ),
                method(
                    "dup2_x2",
                    vec![0x04, 0x05, 0x06, 0x07, 0x5e, 0x57, 0x57, 0x60, 0xac],
                ),
                method("swap", vec![0x04, 0x05, 0x5f, 0x60, 0xac]),
            ],
        };
        let jar = NativeJar::from_classes([class]);
        let expected = [
            ("pop2", 1),
            ("dup_x1", 3),
            ("dup_x2", 4),
            ("dup2", 3),
            ("dup2_x1", 4),
            ("dup2_x2", 3),
            ("swap", 3),
        ];
        for (name, value) in expected {
            assert_eq!(
                jar.invoke_method("demo.Stack", name, "()I", None, &mut NullHost)
                    .unwrap(),
                Value::Int(value),
                "{name}"
            );
        }
    }

    #[test]
    fn integer_bitwise_and_shift_opcodes_execute() {
        let method = |name: &str, code: Vec<u8>| Method {
            name: name.into(),
            descriptor: "()I".into(),
            access: 9,
            code: Some(code),
            max_locals: 0,
        };
        let class = ClassFile {
            name: "demo/Bitwise".into(),
            super_name: "java/lang/Object".into(),
            pool: vec![Cp::Empty],
            bootstrap_methods: Vec::new(),
            fields: Vec::new(),
            methods: vec![
                method("and", vec![0x10, 0x0f, 0x10, 0x03, 0x7e, 0xac]),
                method("or", vec![0x04, 0x07, 0x80, 0xac]),
                method("xor", vec![0x10, 0x07, 0x06, 0x82, 0xac]),
                method("shl", vec![0x04, 0x06, 0x78, 0xac]),
                method("shr", vec![0x10, 0xf8, 0x04, 0x7a, 0xac]),
                method("ushr", vec![0x10, 0xf8, 0x04, 0x7c, 0xac]),
                method("neg", vec![0x06, 0x74, 0xac]),
            ],
        };
        let jar = NativeJar::from_classes([class]);
        let expected = [
            ("and", 3),
            ("or", 5),
            ("xor", 4),
            ("shl", 8),
            ("shr", -4),
            ("ushr", 2_147_483_644),
            ("neg", -3),
        ];
        for (name, value) in expected {
            assert_eq!(
                jar.invoke_method("demo.Bitwise", name, "()I", None, &mut NullHost)
                    .unwrap(),
                Value::Int(value),
                "{name}"
            );
        }
    }

    #[test]
    fn method_arguments_are_loaded_into_locals() {
        let class = ClassFile {
            name: "demo/Args".into(),
            super_name: "java/lang/Object".into(),
            pool: vec![Cp::Empty],
            bootstrap_methods: Vec::new(),
            fields: Vec::new(),
            methods: vec![Method {
                name: "identity".into(),
                descriptor: "(I)I".into(),
                access: 9,
                code: Some(vec![0x15, 0x00, 0xac]),
                max_locals: 1,
            }],
        };
        let jar = NativeJar::from_classes([class]);
        assert_eq!(
            jar.invoke_method_with_args(
                "demo.Args",
                "identity",
                "(I)I",
                None,
                &[Value::Int(7)],
                &mut NullHost,
            )
            .unwrap(),
            Value::Int(7)
        );
    }

    #[test]
    fn lookup_switch_branches_to_matching_case() {
        let class = ClassFile {
            name: "demo/Switch".into(),
            super_name: "java/lang/Object".into(),
            pool: vec![Cp::Empty],
            bootstrap_methods: Vec::new(),
            fields: Vec::new(),
            methods: vec![Method {
                name: "choose".into(),
                descriptor: "()I".into(),
                access: 9,
                code: Some(vec![
                    0x04, 0xab, 0, 0, // iconst_1; padding
                    0, 0, 0, 19, // default -> iconst_5
                    0, 0, 0, 1, // one pair
                    0, 0, 0, 1, // match key 1
                    0, 0, 0, 19, // pair -> iconst_5
                    0x08, 0xac,
                ]),
                max_locals: 0,
            }],
        };
        let jar = NativeJar::from_classes([class]);
        assert_eq!(
            jar.invoke_method("demo.Switch", "choose", "()I", None, &mut NullHost)
                .unwrap(),
            Value::Int(5)
        );
    }

    #[test]
    fn invalid_branch_is_reported() {
        let class = ClassFile {
            name: "demo/BadBranch".into(),
            super_name: "java/lang/Object".into(),
            pool: vec![Cp::Empty],
            bootstrap_methods: Vec::new(),
            fields: Vec::new(),
            methods: vec![Method {
                name: "run".into(),
                descriptor: "()V".into(),
                access: 9,
                code: Some(vec![0xa7, 0x7f, 0xff]),
                max_locals: 0,
            }],
        };
        let jar = NativeJar::from_classes([class]);
        let error = jar
            .invoke_method("demo.BadBranch", "run", "()V", None, &mut NullHost)
            .unwrap_err();
        assert!(matches!(error, NativeError::InvalidBranch { .. }));
    }

    #[test]
    fn neoforge_marker_is_discoverable_without_fml() {
        let class = ClassFile {
            name: "demo/NeoMod".into(),
            super_name: "java/lang/Object".into(),
            pool: vec![Cp::Empty, Cp::Utf8("Lnet/neoforged/fml/common/Mod;".into())],
            bootstrap_methods: Vec::new(),
            fields: Vec::new(),
            methods: vec![],
        };
        let jar = NativeJar::from_classes([class]);
        assert_eq!(
            jar.discover_entrypoint("neoforge").unwrap().class,
            "demo.NeoMod"
        );
        assert!(jar.discover_entrypoint("main").is_none());
        assert!(jar.discover_entrypoint("client").is_none());
    }

    #[test]
    fn legacy_forge_metadata_and_mod_marker_are_discoverable() {
        let class = ClassFile {
            name: "demo/ForgeMod".into(),
            super_name: "java/lang/Object".into(),
            pool: vec![
                Cp::Empty,
                Cp::Utf8("Lnet/minecraftforge/fml/common/Mod;".into()),
            ],
            bootstrap_methods: Vec::new(),
            fields: Vec::new(),
            methods: vec![],
        };
        let jar = NativeJar::from_classes([class]);
        assert_eq!(
            jar.discover_entrypoint("neoforge").unwrap().class,
            "demo.ForgeMod"
        );

        let text = r#"
[[mods]]
modId = "legacy"
version = "1.0"
[[dependencies.legacy]]
modId = "library"
mandatory = true
"#;
        let (id, version, required, client_side_only) = parse_neoforge_metadata(text).unwrap();
        assert_eq!(id.as_deref(), Some("legacy"));
        assert_eq!(version.as_deref(), Some("1.0"));
        assert_eq!(required, ["library"]);
        assert!(!client_side_only);
    }

    #[test]
    fn discover_entrypoints_keeps_all_fabric_lifecycle_entries() {
        let jar = NativeJar {
            fabric_entrypoints: BTreeMap::from([(
                "main".into(),
                vec![
                    Entrypoint {
                        class: "demo.First".into(),
                        method: None,
                        adapter: None,
                    },
                    Entrypoint {
                        class: "demo.Second".into(),
                        method: Some("boot".into()),
                        adapter: None,
                    },
                ],
            )]),
            ..NativeJar::default()
        };
        assert_eq!(jar.discover_entrypoint("main").unwrap().class, "demo.First");
        assert_eq!(
            jar.discover_entrypoints("main"),
            vec![
                Entrypoint {
                    class: "demo.First".into(),
                    method: None,
                    adapter: None,
                },
                Entrypoint {
                    class: "demo.Second".into(),
                    method: Some("boot".into()),
                    adapter: None,
                },
            ]
        );
    }

    #[test]
    fn seed_host_exposes_jar_identity_and_required_loader_mods() {
        let jar = NativeJar {
            fabric_id: Some("demo".into()),
            fabric_version: Some("1.2.3".into()),
            required_mods: vec!["library".into()],
            ..NativeJar::default()
        };
        let mut host = jar.seed_host(StdHost::new());
        let loader = Value::Ref(Some("fabric-loader".into()));
        let loaded = MethodRef {
            owner: "net/fabricmc/loader/api/FabricLoader".into(),
            name: "isModLoaded".into(),
            descriptor: "(Ljava/lang/String;)Z".into(),
        };
        for id in ["demo", "library"] {
            assert_eq!(
                host.invoke_instance(&loaded, &loader, &[Value::Ref(Some(id.into()))])
                    .unwrap(),
                Value::Int(1),
                "{id} should be visible to the seeded loader"
            );
        }
        let container = MethodRef {
            owner: "net/fabricmc/loader/api/FabricLoader".into(),
            name: "getModContainer".into(),
            descriptor: "(Ljava/lang/String;)Ljava/util/Optional;".into(),
        };
        assert_eq!(
            host.invoke_instance(&container, &loader, &[Value::Ref(Some("demo".into()))])
                .unwrap(),
            Value::Ref(Some("mod-container/demo/1.2.3".into()))
        );
    }

    #[test]
    fn neoforge_constructor_arguments_use_neutral_values() {
        let class = ClassFile {
            name: "demo/NeoMod".into(),
            super_name: "java/lang/Object".into(),
            pool: vec![Cp::Empty, Cp::Utf8("Lnet/neoforged/fml/common/Mod;".into())],
            bootstrap_methods: Vec::new(),
            fields: Vec::new(),
            methods: vec![Method {
                name: "<init>".into(),
                descriptor: "(Lnet/neoforged/bus/api/IEventBus;Lnet/neoforged/fml/ModContainer;)V"
                    .into(),
                access: 1,
                code: Some(vec![0xb1]),
                max_locals: 3,
            }],
        };
        let jar = NativeJar::from_classes([class]);
        let report = jar.execute("demo.NeoMod", None, "neoforge", &mut NullHost);
        assert!(report.is_ok(), "{report:?}");
    }

    #[test]
    fn standard_host_rejects_service_loader_classpath_scanning() {
        let mut host = StdHost::new();
        for (name, descriptor) in [
            ("load", "(Ljava/lang/Class;)Ljava/util/ServiceLoader;"),
            (
                "loadInstalled",
                "(Ljava/lang/Class;)Ljava/util/ServiceLoader;",
            ),
        ] {
            let method = MethodRef {
                owner: "java/util/ServiceLoader".into(),
                name: name.into(),
                descriptor: descriptor.into(),
            };
            let error = host.invoke(&method, &[Value::Ref(Some("demo/Provider".into()))]);
            assert!(matches!(
                error,
                Err(NativeError::Metadata(message))
                    if message.contains("ServiceLoader")
                        && message.contains("host classpath scanning is disabled")
            ));
        }
    }

    #[test]
    fn standard_host_loads_only_explicit_service_providers() {
        let mut host = StdHost::new()
            .with_service_provider("demo/Provider", "demo.FirstProvider")
            .with_service_provider("demo/Provider", "demo.SecondProvider");
        assert_eq!(
            host.service_providers("demo/Provider"),
            Some(
                [
                    "demo.FirstProvider".to_owned(),
                    "demo.SecondProvider".to_owned()
                ]
                .as_slice()
            )
        );
        let load = MethodRef {
            owner: "java/util/ServiceLoader".into(),
            name: "load".into(),
            descriptor: "(Ljava/lang/Class;)Ljava/util/ServiceLoader;".into(),
        };
        let loader = host
            .invoke(&load, &[Value::Ref(Some("demo.Provider".into()))])
            .unwrap();
        assert_eq!(
            loader,
            Value::Ref(Some("service-loader/demo/Provider".into()))
        );
        let iterator = host
            .invoke_instance(
                &MethodRef {
                    owner: "java/util/ServiceLoader".into(),
                    name: "iterator".into(),
                    descriptor: "()Ljava/util/Iterator;".into(),
                },
                &loader,
                &[],
            )
            .unwrap();
        let next = MethodRef {
            owner: "java/util/Iterator".into(),
            name: "next".into(),
            descriptor: "()Ljava/lang/Object;".into(),
        };
        assert_eq!(
            host.invoke_instance(&next, &iterator, &[]).unwrap(),
            Value::Ref(Some("demo.FirstProvider".into()))
        );
        assert_eq!(
            host.invoke_instance(&next, &iterator, &[]).unwrap(),
            Value::Ref(Some("demo.SecondProvider".into()))
        );
    }

    #[test]
    fn standard_host_maps_common_queries_and_records_calls() {
        let mut host = StdHost::new().with_property("demo.mode", "native");
        let property = MethodRef {
            owner: "java/lang/System".into(),
            name: "getProperty".into(),
            descriptor: "(Ljava/lang/String;)Ljava/lang/String;".into(),
        };
        assert_eq!(
            host.invoke(&property, &[Value::Ref(Some("demo.mode".into()))])
                .unwrap(),
            Value::Ref(Some("native".into()))
        );
        let property_with_default = MethodRef {
            owner: "java/lang/System".into(),
            name: "getProperty".into(),
            descriptor: "(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;".into(),
        };
        assert_eq!(
            host.invoke(
                &property_with_default,
                &[
                    Value::Ref(Some("demo.mode".into())),
                    Value::Ref(Some("fallback".into())),
                ],
            )
            .unwrap(),
            Value::Ref(Some("native".into()))
        );
        assert_eq!(
            host.invoke(
                &property_with_default,
                &[
                    Value::Ref(Some("missing.mode".into())),
                    Value::Ref(Some("fallback".into())),
                ],
            )
            .unwrap(),
            Value::Ref(Some("fallback".into()))
        );
        let parse = MethodRef {
            owner: "java/lang/Integer".into(),
            name: "parseInt".into(),
            descriptor: "(Ljava/lang/String;)I".into(),
        };
        assert_eq!(
            host.invoke(&parse, &[Value::Ref(Some("7".into()))])
                .unwrap(),
            Value::Int(7)
        );
        let parse_radix = MethodRef {
            owner: "java/lang/Integer".into(),
            name: "parseInt".into(),
            descriptor: "(Ljava/lang/String;I)I".into(),
        };
        assert_eq!(
            host.invoke(
                &parse_radix,
                &[Value::Ref(Some("ff".into())), Value::Int(16)],
            )
            .unwrap(),
            Value::Int(255)
        );
        let value_of_radix = MethodRef {
            owner: "java/lang/Integer".into(),
            name: "valueOf".into(),
            descriptor: "(Ljava/lang/String;I)Ljava/lang/Integer;".into(),
        };
        assert_eq!(
            host.invoke(
                &value_of_radix,
                &[Value::Ref(Some("101".into())), Value::Int(2)],
            )
            .unwrap(),
            Value::Ref(Some("integer:5".into()))
        );
        let compare = MethodRef {
            owner: "java/lang/Integer".into(),
            name: "compare".into(),
            descriptor: "(II)I".into(),
        };
        assert_eq!(
            host.invoke(&compare, &[Value::Int(-2), Value::Int(7)])
                .unwrap(),
            Value::Int(-1)
        );
        let hex = MethodRef {
            owner: "java/lang/Integer".into(),
            name: "toHexString".into(),
            descriptor: "(I)Ljava/lang/String;".into(),
        };
        assert_eq!(
            host.invoke(&hex, &[Value::Int(255)]).unwrap(),
            Value::Ref(Some("ff".into()))
        );
        let long_compare = MethodRef {
            owner: "java/lang/Long".into(),
            name: "compare".into(),
            descriptor: "(JJ)I".into(),
        };
        assert_eq!(
            host.invoke(&long_compare, &[Value::Int(4), Value::Int(9)])
                .unwrap(),
            Value::Int(-1)
        );
        let long_hex = MethodRef {
            owner: "java/lang/Long".into(),
            name: "toHexString".into(),
            descriptor: "(J)Ljava/lang/String;".into(),
        };
        assert_eq!(
            host.invoke(&long_hex, &[Value::Int(255)]).unwrap(),
            Value::Ref(Some("ff".into()))
        );
        assert_eq!(
            host.invoke(&long_hex, &[Value::Ref(Some("long:-1".into()))])
                .unwrap(),
            Value::Ref(Some("ffffffffffffffff".into()))
        );
        assert_eq!(
            host.calls(),
            &[
                property,
                property_with_default.clone(),
                property_with_default,
                parse,
                parse_radix,
                value_of_radix,
                compare,
                hex,
                long_compare,
                long_hex.clone(),
                long_hex,
            ]
        );
    }

    #[test]
    fn standard_host_classloader_resources_are_explicit_and_host_local() {
        let mut host = StdHost::new().with_resource("assets/demo.txt", "resource/demo");
        assert_eq!(host.resource("assets/demo.txt"), Some("resource/demo"));
        assert_eq!(host.resource("/assets/demo.txt"), Some("resource/demo"));
        assert_eq!(host.resource("missing.txt"), None);

        let loader = Value::Ref(Some("classloader/bootstrap".into()));
        let url = MethodRef {
            owner: "java/lang/ClassLoader".into(),
            name: "getResource".into(),
            descriptor: "(Ljava/lang/String;)Ljava/net/URL;".into(),
        };
        assert_eq!(
            host.invoke_instance(&url, &loader, &[Value::Ref(Some("assets/demo.txt".into()))])
                .unwrap(),
            Value::Ref(Some("resource/demo".into()))
        );
        let stream = MethodRef {
            name: "getResourceAsStream".into(),
            descriptor: "(Ljava/lang/String;)Ljava/io/InputStream;".into(),
            ..url.clone()
        };
        assert_eq!(
            host.invoke_instance(&stream, &loader, &[Value::Ref(Some("missing.txt".into()))])
                .unwrap(),
            Value::Ref(None)
        );
        host = host.with_resource_bytes("assets/bytes.bin", vec![97, 98, 99]);
        let byte_stream = host
            .invoke_instance(
                &stream,
                &loader,
                &[Value::Ref(Some("assets/bytes.bin".into()))],
            )
            .unwrap();
        let read = MethodRef {
            owner: "java/io/InputStream".into(),
            name: "read".into(),
            descriptor: "()I".into(),
        };
        let available = MethodRef {
            owner: "java/io/InputStream".into(),
            name: "available".into(),
            descriptor: "()I".into(),
        };
        let skip = MethodRef {
            owner: "java/io/InputStream".into(),
            name: "skip".into(),
            descriptor: "(J)J".into(),
        };
        assert_eq!(
            host.invoke_instance(&available, &byte_stream, &[]).unwrap(),
            Value::Int(3)
        );
        assert_eq!(
            host.invoke_instance(&read, &byte_stream, &[]).unwrap(),
            Value::Int(97)
        );
        assert_eq!(
            host.invoke_instance(&skip, &byte_stream, &[Value::Int(1)])
                .unwrap(),
            Value::Int(1)
        );
        assert_eq!(
            host.invoke_instance(&read, &byte_stream, &[]).unwrap(),
            Value::Int(99)
        );
        assert_eq!(
            host.invoke_instance(&read, &byte_stream, &[]).unwrap(),
            Value::Int(-1)
        );
        let mark_supported = MethodRef {
            owner: "java/io/InputStream".into(),
            name: "markSupported".into(),
            descriptor: "()Z".into(),
        };
        let mark = MethodRef {
            owner: "java/io/InputStream".into(),
            name: "mark".into(),
            descriptor: "(I)V".into(),
        };
        let reset = MethodRef {
            owner: "java/io/InputStream".into(),
            name: "reset".into(),
            descriptor: "()V".into(),
        };
        assert_eq!(
            host.invoke_instance(&mark_supported, &byte_stream, &[])
                .unwrap(),
            Value::Int(1)
        );
        let marked_stream = host
            .invoke_instance(
                &stream,
                &loader,
                &[Value::Ref(Some("/assets/bytes.bin".into()))],
            )
            .unwrap();
        host.invoke_instance(&mark, &marked_stream, &[Value::Int(0)])
            .unwrap();
        assert_eq!(
            host.invoke_instance(&read, &marked_stream, &[]).unwrap(),
            Value::Int(97)
        );
        host.invoke_instance(&reset, &marked_stream, &[]).unwrap();
        assert_eq!(
            host.invoke_instance(&read, &marked_stream, &[]).unwrap(),
            Value::Int(97)
        );
        let skip_n = MethodRef {
            owner: "java/io/InputStream".into(),
            name: "skipNBytes".into(),
            descriptor: "(J)V".into(),
        };
        assert!(
            host.invoke_instance(&skip_n, &marked_stream, &[Value::Int(3)])
                .is_err()
        );

        let resources = MethodRef {
            owner: "java/lang/ClassLoader".into(),
            name: "getResources".into(),
            descriptor: "(Ljava/lang/String;)Ljava/util/Enumeration;".into(),
        };
        let enumeration = host
            .invoke_instance(
                &resources,
                &loader,
                &[Value::Ref(Some("assets/demo.txt".into()))],
            )
            .unwrap();
        let has_more = MethodRef {
            owner: "java/util/Enumeration".into(),
            name: "hasMoreElements".into(),
            descriptor: "()Z".into(),
        };
        let next = MethodRef {
            owner: "java/util/Enumeration".into(),
            name: "nextElement".into(),
            descriptor: "()Ljava/lang/Object;".into(),
        };
        assert_eq!(
            host.invoke_instance(&has_more, &enumeration, &[]).unwrap(),
            Value::Int(1)
        );
        assert_eq!(
            host.invoke_instance(&next, &enumeration, &[]).unwrap(),
            Value::Ref(Some("resource/demo".into()))
        );
        assert_eq!(
            host.invoke_instance(&has_more, &enumeration, &[]).unwrap(),
            Value::Int(0)
        );

        let missing = host
            .invoke(
                &MethodRef {
                    owner: "java/lang/ClassLoader".into(),
                    name: "getSystemResources".into(),
                    descriptor: "(Ljava/lang/String;)Ljava/util/Enumeration;".into(),
                },
                &[Value::Ref(Some("missing.txt".into()))],
            )
            .unwrap();
        assert_eq!(
            host.invoke_instance(&has_more, &missing, &[]).unwrap(),
            Value::Int(0)
        );
    }

    #[test]
    fn standard_host_classloader_loads_only_explicit_classes() {
        let mut host = StdHost::new().with_class("demo.Provider");
        assert_eq!(host.class_marker("demo/Provider"), Some("demo/Provider"));
        let loader = Value::Ref(Some("classloader/bootstrap".into()));
        let load = MethodRef {
            owner: "java/lang/ClassLoader".into(),
            name: "loadClass".into(),
            descriptor: "(Ljava/lang/String;)Ljava/lang/Class;".into(),
        };
        assert_eq!(
            host.invoke_instance(&load, &loader, &[Value::Ref(Some("demo.Provider".into()))],)
                .unwrap(),
            Value::Ref(Some("demo/Provider".into()))
        );
        let missing = host
            .invoke_instance(
                &load,
                &loader,
                &[Value::Ref(Some("missing.Provider".into()))],
            )
            .unwrap_err()
            .to_string();
        assert!(missing.contains("not explicitly seeded"), "{missing}");
    }

    #[test]
    fn standard_host_maps_integer_floor_math() {
        let mut host = StdHost::new();
        let floor_div = MethodRef {
            owner: "java/lang/Math".into(),
            name: "floorDiv".into(),
            descriptor: "(II)I".into(),
        };
        assert_eq!(
            host.invoke(&floor_div, &[Value::Int(-7), Value::Int(3)])
                .unwrap(),
            Value::Int(-3)
        );
        let floor_mod = MethodRef {
            owner: "java/lang/Math".into(),
            name: "floorMod".into(),
            descriptor: "(II)I".into(),
        };
        assert_eq!(
            host.invoke(&floor_mod, &[Value::Int(-7), Value::Int(3)])
                .unwrap(),
            Value::Int(2)
        );
        assert_eq!(
            host.invoke(&floor_mod, &[Value::Int(7), Value::Int(-3)])
                .unwrap(),
            Value::Int(-2)
        );
        assert_eq!(
            host.invoke(&floor_mod, &[Value::Int(i32::MIN), Value::Int(-1)])
                .unwrap(),
            Value::Int(0)
        );
    }

    #[test]
    fn standard_host_maps_bounded_long_parsing() {
        let mut host = StdHost::new();
        let parse = MethodRef {
            owner: "java/lang/Long".into(),
            name: "parseLong".into(),
            descriptor: "(Ljava/lang/String;)J".into(),
        };
        let value = host
            .invoke(&parse, &[Value::Ref(Some("9223372036854775807".into()))])
            .unwrap();
        assert_eq!(value, Value::Ref(Some("long:9223372036854775807".into())));
        assert_eq!(
            host.invoke(
                &MethodRef {
                    owner: "java/lang/Long".into(),
                    name: "toString".into(),
                    descriptor: "(J)Ljava/lang/String;".into(),
                },
                std::slice::from_ref(&value),
            )
            .unwrap(),
            Value::Ref(Some("9223372036854775807".into()))
        );
        assert_eq!(
            host.invoke(
                &MethodRef {
                    owner: "java/lang/Long".into(),
                    name: "parseLong".into(),
                    descriptor: "(Ljava/lang/String;I)J".into(),
                },
                &[Value::Ref(Some("ff".into())), Value::Int(16)],
            )
            .unwrap(),
            Value::Ref(Some("long:255".into()))
        );
        assert!(
            host.invoke(&parse, &[Value::Ref(Some("oops".into()))])
                .is_err()
        );
    }

    #[test]
    fn standard_host_maps_character_queries_and_boxing() {
        let mut host = StdHost::new();
        let letter = MethodRef {
            owner: "java/lang/Character".into(),
            name: "isLetter".into(),
            descriptor: "(C)Z".into(),
        };
        assert_eq!(
            host.invoke(&letter, &[Value::Int('Æ' as i32)]).unwrap(),
            Value::Int(1)
        );
        let digit = MethodRef {
            name: "isDigit".into(),
            descriptor: "(I)Z".into(),
            ..letter.clone()
        };
        assert_eq!(
            host.invoke(&digit, &[Value::Int('७' as i32)]).unwrap(),
            Value::Int(1)
        );
        let lower = MethodRef {
            name: "toLowerCase".into(),
            descriptor: "(C)C".into(),
            ..letter.clone()
        };
        assert_eq!(
            host.invoke(&lower, &[Value::Int('Q' as i32)]).unwrap(),
            Value::Int('q' as i32)
        );
        let value_of = MethodRef {
            name: "valueOf".into(),
            descriptor: "(C)Ljava/lang/Character;".into(),
            ..letter.clone()
        };
        let boxed = host.invoke(&value_of, &[Value::Int('Z' as i32)]).unwrap();
        assert_eq!(boxed, Value::Ref(Some("character:90".into())));
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    name: "charValue".into(),
                    descriptor: "()C".into(),
                    ..letter.clone()
                },
                &boxed,
                &[]
            )
            .unwrap(),
            Value::Int('Z' as i32)
        );
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    name: "toString".into(),
                    descriptor: "()Ljava/lang/String;".into(),
                    ..letter
                },
                &boxed,
                &[]
            )
            .unwrap(),
            Value::Ref(Some("Z".into()))
        );
    }

    #[test]
    fn standard_host_maps_byte_wrapper_values() {
        let mut host = StdHost::new();
        let parse = MethodRef {
            owner: "java/lang/Byte".into(),
            name: "parseByte".into(),
            descriptor: "(Ljava/lang/String;)B".into(),
        };
        assert_eq!(
            host.invoke(&parse, &[Value::Ref(Some("-12".into()))])
                .unwrap(),
            Value::Int(-12)
        );
        let value_of = MethodRef {
            owner: "java/lang/Byte".into(),
            name: "valueOf".into(),
            descriptor: "(Ljava/lang/String;)Ljava/lang/Byte;".into(),
        };
        let boxed = host
            .invoke(&value_of, &[Value::Ref(Some("127".into()))])
            .unwrap();
        assert_eq!(boxed, Value::Ref(Some("byte:127".into())));
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "java/lang/Byte".into(),
                    name: "byteValue".into(),
                    descriptor: "()B".into(),
                },
                &boxed,
                &[],
            )
            .unwrap(),
            Value::Int(127)
        );
        assert!(
            host.invoke(&parse, &[Value::Ref(Some("128".into()))])
                .is_err()
        );
    }

    #[test]
    fn standard_host_maps_bounded_short_wrapper_values() {
        let mut host = StdHost::new();
        let parse = MethodRef {
            owner: "java/lang/Short".into(),
            name: "parseShort".into(),
            descriptor: "(Ljava/lang/String;)S".into(),
        };
        assert_eq!(
            host.invoke(&parse, &[Value::Ref(Some("-32768".into()))])
                .unwrap(),
            Value::Int(-32768)
        );
        assert_eq!(
            host.invoke(
                &MethodRef {
                    owner: "java/lang/Short".into(),
                    name: "parseShort".into(),
                    descriptor: "(Ljava/lang/String;I)S".into(),
                },
                &[Value::Ref(Some("7fff".into())), Value::Int(16)],
            )
            .unwrap(),
            Value::Int(32767)
        );
        let value_of = MethodRef {
            owner: "java/lang/Short".into(),
            name: "valueOf".into(),
            descriptor: "(Ljava/lang/String;)Ljava/lang/Short;".into(),
        };
        let boxed = host
            .invoke(&value_of, &[Value::Ref(Some("32767".into()))])
            .unwrap();
        assert_eq!(boxed, Value::Ref(Some("short:32767".into())));
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "java/lang/Short".into(),
                    name: "shortValue".into(),
                    descriptor: "()S".into(),
                },
                &boxed,
                &[],
            )
            .unwrap(),
            Value::Int(32767)
        );
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "java/lang/Short".into(),
                    name: "toString".into(),
                    descriptor: "()Ljava/lang/String;".into(),
                },
                &boxed,
                &[],
            )
            .unwrap(),
            Value::Ref(Some("32767".into()))
        );
        assert_eq!(
            host.invoke(
                &MethodRef {
                    owner: "java/lang/Short".into(),
                    name: "toString".into(),
                    descriptor: "(S)Ljava/lang/String;".into(),
                },
                &[Value::Int(-12)],
            )
            .unwrap(),
            Value::Ref(Some("-12".into()))
        );
        assert!(
            host.invoke(&parse, &[Value::Ref(Some("32768".into()))])
                .is_err()
        );
        assert!(
            host.invoke(&parse, &[Value::Ref(Some(" 1 ".into()))])
                .is_err()
        );
    }

    #[test]
    fn standard_host_maps_checked_integer_math() {
        let mut host = StdHost::new();
        let add_exact = MethodRef {
            owner: "java/lang/Math".into(),
            name: "addExact".into(),
            descriptor: "(II)I".into(),
        };
        assert_eq!(
            host.invoke(&add_exact, &[Value::Int(40), Value::Int(2)])
                .unwrap(),
            Value::Int(42)
        );
        assert!(matches!(
            host.invoke(&add_exact, &[Value::Int(i32::MAX), Value::Int(1)]),
            Err(NativeError::Arithmetic { .. })
        ));
        assert!(matches!(
            host.invoke(&add_exact, &[Value::Int(i32::MIN), Value::Int(-1)]),
            Err(NativeError::Arithmetic { .. })
        ));
    }

    #[test]
    fn standard_host_maps_bounded_floating_point_wrappers() {
        let mut host = StdHost::new();
        let parse_double = MethodRef {
            owner: "java/lang/Double".into(),
            name: "parseDouble".into(),
            descriptor: "(Ljava/lang/String;)D".into(),
        };
        let double = host
            .invoke(&parse_double, &[Value::Ref(Some("Infinity".into()))])
            .unwrap();
        assert_eq!(double, Value::Ref(Some("double:inf".into())));
        assert_eq!(
            host.invoke(
                &MethodRef {
                    owner: "java/lang/Double".into(),
                    name: "isInfinite".into(),
                    descriptor: "(D)Z".into(),
                },
                std::slice::from_ref(&double),
            )
            .unwrap(),
            Value::Int(1)
        );
        assert_eq!(
            host.invoke(
                &MethodRef {
                    owner: "java/lang/Double".into(),
                    name: "toString".into(),
                    descriptor: "(D)Ljava/lang/String;".into(),
                },
                std::slice::from_ref(&double),
            )
            .unwrap(),
            Value::Ref(Some("Infinity".into()))
        );
        let float = host
            .invoke(
                &MethodRef {
                    owner: "java/lang/Float".into(),
                    name: "parseFloat".into(),
                    descriptor: "(Ljava/lang/String;)F".into(),
                },
                &[Value::Ref(Some("1.25".into()))],
            )
            .unwrap();
        assert_eq!(float, Value::Ref(Some("float:1.25".into())));
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "java/lang/Float".into(),
                    name: "toString".into(),
                    descriptor: "()Ljava/lang/String;".into(),
                },
                &float,
                &[],
            )
            .unwrap(),
            Value::Ref(Some("1.25".into()))
        );
        assert!(
            host.invoke(&parse_double, &[Value::Ref(Some("oops".into()))])
                .is_err()
        );
    }

    #[test]
    fn standard_host_maps_number_accessors_across_boxed_types() {
        let mut host = StdHost::new();
        let number = |name: &str, descriptor: &str| MethodRef {
            owner: "java/lang/Number".into(),
            name: name.into(),
            descriptor: descriptor.into(),
        };
        let long = Value::Ref(Some("long:4294967297".into()));
        assert_eq!(
            host.invoke_instance(&number("intValue", "()I"), &long, &[])
                .unwrap(),
            Value::Int(1)
        );
        assert_eq!(
            host.invoke_instance(&number("longValue", "()J"), &long, &[])
                .unwrap(),
            long_value(4_294_967_297)
        );
        assert_eq!(
            host.invoke_instance(&number("byteValue", "()B"), &long, &[])
                .unwrap(),
            Value::Int(1)
        );
        let floating = Value::Ref(Some("double:3.75".into()));
        assert_eq!(
            host.invoke_instance(&number("intValue", "()I"), &floating, &[])
                .unwrap(),
            Value::Int(3)
        );
        assert_eq!(
            host.invoke_instance(&number("floatValue", "()F"), &floating, &[])
                .unwrap(),
            float_value(3.75)
        );
        assert_eq!(
            host.invoke_instance(&number("doubleValue", "()D"), &long, &[])
                .unwrap(),
            double_value(4_294_967_297.0)
        );
    }

    #[test]
    fn standard_host_maps_uuid_identity_without_randomness() {
        let mut host = StdHost::new();
        let from_string = MethodRef {
            owner: "java/util/UUID".into(),
            name: "fromString".into(),
            descriptor: "(Ljava/lang/String;)Ljava/util/UUID;".into(),
        };
        let uuid = host
            .invoke(
                &from_string,
                &[Value::Ref(Some(
                    "550E8400-E29B-41D4-A716-446655440000".into(),
                ))],
            )
            .unwrap();
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "java/util/UUID".into(),
                    name: "toString".into(),
                    descriptor: "()Ljava/lang/String;".into(),
                },
                &uuid,
                &[],
            )
            .unwrap(),
            Value::Ref(Some("550e8400-e29b-41d4-a716-446655440000".into()))
        );
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "java/util/UUID".into(),
                    name: "equals".into(),
                    descriptor: "(Ljava/lang/Object;)Z".into(),
                },
                &uuid,
                &[Value::Ref(Some(
                    "uuid/550e8400-e29b-41d4-a716-446655440000".into(),
                ))],
            )
            .unwrap(),
            Value::Int(1)
        );
        let random = host
            .invoke(
                &MethodRef {
                    owner: "java/util/UUID".into(),
                    name: "randomUUID".into(),
                    descriptor: "()Ljava/util/UUID;".into(),
                },
                &[],
            )
            .unwrap();
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "java/util/UUID".into(),
                    name: "toString".into(),
                    descriptor: "()Ljava/lang/String;".into(),
                },
                &random,
                &[],
            )
            .unwrap(),
            Value::Ref(Some("00000000-0000-4000-8000-000000000000".into()))
        );
        assert!(
            host.invoke(&from_string, &[Value::Ref(Some("not-a-uuid".into()))],)
                .is_err()
        );
    }

    #[test]
    fn standard_host_preserves_instance_receivers_and_neutral_unknowns() {
        let mut host = StdHost::new();
        let length = MethodRef {
            owner: "java/lang/String".into(),
            name: "length".into(),
            descriptor: "()I".into(),
        };
        assert_eq!(
            host.invoke_instance(&length, &Value::Ref(Some("hé".into())), &[])
                .unwrap(),
            Value::Int(2)
        );
        let unknown = MethodRef {
            owner: "demo/Unknown".into(),
            name: "run".into(),
            descriptor: "()Ljava/lang/Object;".into(),
        };
        assert_eq!(host.invoke(&unknown, &[]).unwrap(), Value::Ref(None));
        assert_eq!(host.calls(), &[length, unknown]);
        assert_eq!(
            host.invocations()[0].receiver,
            Some(Value::Ref(Some("hé".into())))
        );
        assert_eq!(host.invocations()[1].args, Vec::new());
    }

    #[test]
    fn standard_host_maps_arrays_as_list_to_an_opaque_marker() {
        let mut host = StdHost::new();
        let as_list = MethodRef {
            owner: "java/util/Arrays".into(),
            name: "asList".into(),
            descriptor: "([Ljava/lang/Object;)Ljava/util/List;".into(),
        };
        let array = Value::Ref(Some("array/java/lang/String/args/0".into()));
        assert_eq!(
            host.invoke(&as_list, std::slice::from_ref(&array)).unwrap(),
            Value::Ref(Some("list/rarray/java/lang/String/args/0".into()))
        );
    }

    #[test]
    fn standard_host_maps_component_text_values() {
        let mut host = StdHost::new();
        let literal = host
            .invoke(
                &MethodRef {
                    owner: "net/minecraft/network/chat/Component".into(),
                    name: "literal".into(),
                    descriptor: "(Ljava/lang/String;)Lnet/minecraft/network/chat/MutableComponent;"
                        .into(),
                },
                &[Value::Ref(Some("Hello".into()))],
            )
            .unwrap();
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "net/minecraft/network/chat/MutableComponent".into(),
                    name: "append".into(),
                    descriptor: "(Ljava/lang/String;)Lnet/minecraft/network/chat/MutableComponent;"
                        .into(),
                },
                &literal,
                &[Value::Ref(Some(" world".into()))],
            )
            .unwrap(),
            component_value("literal", "Hello world")
        );
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "net/minecraft/network/chat/Component".into(),
                    name: "getString".into(),
                    descriptor: "()Ljava/lang/String;".into(),
                },
                &component_value("translatable", "item.example.apple"),
                &[],
            )
            .unwrap(),
            Value::Ref(Some("item.example.apple".into()))
        );
    }

    #[test]
    fn standard_host_maps_common_string_operations_with_utf16_indices() {
        let mut host = StdHost::new();
        let receiver = Value::Ref(Some("HélloWorld".into()));
        let starts = MethodRef {
            owner: "java/lang/String".into(),
            name: "startsWith".into(),
            descriptor: "(Ljava/lang/String;)Z".into(),
        };
        assert_eq!(
            host.invoke_instance(&starts, &receiver, &[Value::Ref(Some("Hé".into()))])
                .unwrap(),
            Value::Int(1)
        );
        let repeat = MethodRef {
            owner: "java/lang/String".into(),
            name: "repeat".into(),
            descriptor: "(I)Ljava/lang/String;".into(),
        };
        assert_eq!(
            host.invoke_instance(&repeat, &Value::Ref(Some("ab".into())), &[Value::Int(3)])
                .unwrap(),
            Value::Ref(Some("ababab".into()))
        );
        assert!(
            host.invoke_instance(&repeat, &receiver, &[Value::Int(-1)])
                .is_err()
        );
        let substring = MethodRef {
            owner: "java/lang/String".into(),
            name: "substring".into(),
            descriptor: "(II)Ljava/lang/String;".into(),
        };
        assert_eq!(
            host.invoke_instance(&substring, &receiver, &[Value::Int(1), Value::Int(5)],)
                .unwrap(),
            Value::Ref(Some("éllo".into()))
        );
        let char_at = MethodRef {
            owner: "java/lang/String".into(),
            name: "charAt".into(),
            descriptor: "(I)C".into(),
        };
        assert_eq!(
            host.invoke_instance(&char_at, &receiver, &[Value::Int(1)])
                .unwrap(),
            Value::Int('é' as i32)
        );
        let index_of = MethodRef {
            owner: "java/lang/String".into(),
            name: "indexOf".into(),
            descriptor: "(Ljava/lang/String;)I".into(),
        };
        assert_eq!(
            host.invoke_instance(&index_of, &receiver, &[Value::Ref(Some("World".into()))])
                .unwrap(),
            Value::Int(5)
        );
        let last_index_of = MethodRef {
            owner: "java/lang/String".into(),
            name: "lastIndexOf".into(),
            descriptor: "(Ljava/lang/String;)I".into(),
        };
        assert_eq!(
            host.invoke_instance(
                &last_index_of,
                &Value::Ref(Some("héllo hé".into())),
                &[Value::Ref(Some("hé".into()))],
            )
            .unwrap(),
            Value::Int(6)
        );
        let hash = MethodRef {
            owner: "java/lang/String".into(),
            name: "hashCode".into(),
            descriptor: "()I".into(),
        };
        assert_eq!(
            host.invoke_instance(&hash, &receiver, &[]).unwrap(),
            Value::Int(stable_hash("HélloWorld"))
        );
    }

    #[test]
    fn standard_host_maps_string_normalization_operations() {
        let mut host = StdHost::new();
        let receiver = Value::Ref(Some("  Fabric/NeoForge  ".into()));
        let replace = MethodRef {
            owner: "java/lang/String".into(),
            name: "replace".into(),
            descriptor: "(Ljava/lang/CharSequence;Ljava/lang/CharSequence;)Ljava/lang/String;"
                .into(),
        };
        assert_eq!(
            host.invoke_instance(
                &replace,
                &receiver,
                &[Value::Ref(Some("/".into())), Value::Ref(Some(".".into())),],
            )
            .unwrap(),
            Value::Ref(Some("  Fabric.NeoForge  ".into()))
        );
        let trim = MethodRef {
            owner: "java/lang/String".into(),
            name: "trim".into(),
            descriptor: "()Ljava/lang/String;".into(),
        };
        assert_eq!(
            host.invoke_instance(&trim, &receiver, &[]).unwrap(),
            Value::Ref(Some("Fabric/NeoForge".into()))
        );
        let blank = MethodRef {
            owner: "java/lang/String".into(),
            name: "isBlank".into(),
            descriptor: "()Z".into(),
        };
        assert_eq!(
            host.invoke_instance(&blank, &Value::Ref(Some("\u{2003}\u{2009}".into())), &[],)
                .unwrap(),
            Value::Int(1)
        );
    }

    #[test]
    fn standard_host_maps_string_builder_and_objects() {
        let mut host = StdHost::new();
        let builder = Value::Ref(Some("java/lang/StringBuilder".into()));
        let append_string = MethodRef {
            owner: "java/lang/StringBuilder".into(),
            name: "append".into(),
            descriptor: "(Ljava/lang/String;)Ljava/lang/StringBuilder;".into(),
        };
        let builder = host
            .invoke_instance(&append_string, &builder, &[Value::Ref(Some("id=".into()))])
            .unwrap();
        let append_int = MethodRef {
            owner: "java/lang/StringBuilder".into(),
            name: "append".into(),
            descriptor: "(I)Ljava/lang/StringBuilder;".into(),
        };
        let builder = host
            .invoke_instance(&append_int, &builder, &[Value::Int(42)])
            .unwrap();
        let to_string = MethodRef {
            owner: "java/lang/StringBuilder".into(),
            name: "toString".into(),
            descriptor: "()Ljava/lang/String;".into(),
        };
        assert_eq!(
            host.invoke_instance(&to_string, &builder, &[]).unwrap(),
            Value::Ref(Some("id=42".into()))
        );
        let require = MethodRef {
            owner: "java/util/Objects".into(),
            name: "requireNonNull".into(),
            descriptor: "(Ljava/lang/Object;)Ljava/lang/Object;".into(),
        };
        assert_eq!(
            host.invoke(&require, &[Value::Ref(Some("ok".into()))])
                .unwrap(),
            Value::Ref(Some("ok".into()))
        );
        assert!(host.invoke(&require, &[Value::Ref(None)]).is_err());
        let require_else = MethodRef {
            owner: "java/util/Objects".into(),
            name: "requireNonNullElse".into(),
            descriptor: "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;".into(),
        };
        assert_eq!(
            host.invoke(
                &require_else,
                &[
                    Value::Ref(Some("value".into())),
                    Value::Ref(Some("fallback".into()))
                ],
            )
            .unwrap(),
            Value::Ref(Some("value".into()))
        );
        assert_eq!(
            host.invoke(
                &require_else,
                &[Value::Ref(None), Value::Ref(Some("fallback".into()))],
            )
            .unwrap(),
            Value::Ref(Some("fallback".into()))
        );
        assert!(
            host.invoke(&require_else, &[Value::Ref(None), Value::Ref(None)])
                .is_err()
        );
        let hash_code = MethodRef {
            owner: "java/util/Objects".into(),
            name: "hashCode".into(),
            descriptor: "(Ljava/lang/Object;)I".into(),
        };
        assert_eq!(
            host.invoke(&hash_code, &[Value::Ref(None)]).unwrap(),
            Value::Int(0)
        );
        assert_eq!(
            host.invoke(&hash_code, &[Value::Ref(Some("ok".into()))])
                .unwrap(),
            Value::Int(stable_hash("ok"))
        );
        let value_of = MethodRef {
            owner: "java/lang/String".into(),
            name: "valueOf".into(),
            descriptor: "(I)Ljava/lang/String;".into(),
        };
        assert_eq!(
            host.invoke(&value_of, &[Value::Int(42)]).unwrap(),
            Value::Ref(Some("42".into()))
        );
        let boolean_value_of = MethodRef {
            owner: "java/lang/String".into(),
            name: "valueOf".into(),
            descriptor: "(Z)Ljava/lang/String;".into(),
        };
        assert_eq!(
            host.invoke(&boolean_value_of, &[Value::Int(1)]).unwrap(),
            Value::Ref(Some("true".into()))
        );
    }

    #[test]
    fn standard_host_maps_optional_callback_flow() {
        let mut host = StdHost::new();
        let empty = MethodRef {
            owner: "java/util/Optional".into(),
            name: "empty".into(),
            descriptor: "()Ljava/util/Optional;".into(),
        };
        assert_eq!(host.invoke(&empty, &[]).unwrap(), Value::Ref(None));
        let of = MethodRef {
            owner: "java/util/Optional".into(),
            name: "of".into(),
            descriptor: "(Ljava/lang/Object;)Ljava/util/Optional;".into(),
        };
        assert_eq!(
            host.invoke(&of, &[Value::Ref(Some("demo".into()))])
                .unwrap(),
            Value::Ref(Some("optional/demo".into()))
        );
        assert!(host.invoke(&of, &[Value::Ref(None)]).is_err());
        let of_nullable = MethodRef {
            owner: "java/util/Optional".into(),
            name: "ofNullable".into(),
            descriptor: "(Ljava/lang/Object;)Ljava/util/Optional;".into(),
        };
        assert_eq!(
            host.invoke(&of_nullable, &[Value::Ref(None)]).unwrap(),
            Value::Ref(None)
        );
        let optional_to_string = MethodRef {
            owner: "java/util/Optional".into(),
            name: "toString".into(),
            descriptor: "()Ljava/lang/String;".into(),
        };
        assert_eq!(
            host.invoke_instance(
                &optional_to_string,
                &Value::Ref(Some("optional/demo".into())),
                &[],
            )
            .unwrap(),
            Value::Ref(Some("Optional[demo]".into()))
        );
        let optional_equals = MethodRef {
            owner: "java/util/Optional".into(),
            name: "equals".into(),
            descriptor: "(Ljava/lang/Object;)Z".into(),
        };
        assert_eq!(
            host.invoke_instance(
                &optional_equals,
                &Value::Ref(Some("optional/demo".into())),
                &[Value::Ref(Some("optional/demo".into()))],
            )
            .unwrap(),
            Value::Int(1)
        );
        let map = MethodRef {
            owner: "java/util/Optional".into(),
            name: "map".into(),
            descriptor: "(Ljava/util/function/Function;)Ljava/util/Optional;".into(),
        };
        let mapped = host
            .invoke_instance(
                &map,
                &Value::Ref(Some("optional/demo".into())),
                &[Value::Ref(Some("@indy:identity".into()))],
            )
            .unwrap();
        assert_eq!(mapped, Value::Ref(Some("optional/demo".into())));
        assert_eq!(
            host.invoke_instance(&map, &Value::Ref(None), &[Value::Ref(None)])
                .unwrap(),
            Value::Ref(None)
        );
        let to_string = MethodRef {
            owner: "java/util/Optional".into(),
            name: "toString".into(),
            descriptor: "()Ljava/lang/String;".into(),
        };
        assert_eq!(
            host.invoke_instance(&to_string, &Value::Ref(Some("optional/demo".into())), &[])
                .unwrap(),
            Value::Ref(Some("Optional[demo]".into()))
        );
        assert_eq!(
            host.invoke_instance(&to_string, &Value::Ref(None), &[])
                .unwrap(),
            Value::Ref(Some("Optional.empty".into()))
        );
        let filter = MethodRef {
            owner: "java/util/Optional".into(),
            name: "filter".into(),
            descriptor: "(Ljava/util/function/Predicate;)Ljava/util/Optional;".into(),
        };
        assert_eq!(
            host.invoke_instance(
                &filter,
                &Value::Ref(Some("optional/demo".into())),
                &[Value::Ref(Some("@indy:predicate".into()))],
            )
            .unwrap(),
            Value::Ref(Some("optional/demo".into()))
        );
        assert_eq!(
            host.invoke_instance(
                &filter,
                &Value::Ref(Some("optional/demo".into())),
                &[Value::Ref(None)],
            )
            .unwrap(),
            Value::Ref(None)
        );
        let or = MethodRef {
            owner: "java/util/Optional".into(),
            name: "or".into(),
            descriptor: "(Ljava/util/function/Supplier;)Ljava/util/Optional;".into(),
        };
        assert_eq!(
            host.invoke_instance(
                &or,
                &Value::Ref(Some("optional/demo".into())),
                &[Value::Ref(Some("@indy:unused-supplier".into()))],
            )
            .unwrap(),
            Value::Ref(Some("optional/demo".into()))
        );
        assert_eq!(
            host.invoke_instance(
                &or,
                &Value::Ref(None),
                &[Value::Ref(Some("@indy:empty-supplier".into()))],
            )
            .unwrap(),
            Value::Ref(None)
        );
        let if_present = MethodRef {
            owner: "java/util/Optional".into(),
            name: "ifPresent".into(),
            descriptor: "(Ljava/util/function/Consumer;)V".into(),
        };
        assert_eq!(
            host.invoke_instance(
                &if_present,
                &Value::Ref(Some("optional/demo".into())),
                &[Value::Ref(Some("@indy:consumer".into()))],
            )
            .unwrap(),
            Value::Void
        );
        assert!(
            host.calls().iter().any(|call| {
                call.owner == "java/util/function/Consumer" && call.name == "accept"
            })
        );
        let or_else_throw = MethodRef {
            owner: "java/util/Optional".into(),
            name: "orElseThrow".into(),
            descriptor: "()Ljava/lang/Object;".into(),
        };
        assert_eq!(
            host.invoke_instance(
                &or_else_throw,
                &Value::Ref(Some("optional/demo".into())),
                &[],
            )
            .unwrap(),
            Value::Ref(Some("demo".into()))
        );
        assert!(
            host.invoke_instance(&or_else_throw, &Value::Ref(None), &[])
                .is_err()
        );
        let or_else_throw_supplier = MethodRef {
            owner: "java/util/Optional".into(),
            name: "orElseThrow".into(),
            descriptor: "(Ljava/util/function/Supplier;)Ljava/lang/Object;".into(),
        };
        assert_eq!(
            host.invoke_instance(
                &or_else_throw_supplier,
                &Value::Ref(None),
                &[Value::Ref(Some("@indy:optional-error".into()))],
            )
            .unwrap(),
            Value::Ref(None)
        );
        assert!(
            host.calls()
                .iter()
                .any(|call| { call.owner == "java/util/function/Supplier" && call.name == "get" })
        );
    }

    #[test]
    fn standard_host_maps_optional_int_values() {
        let mut host = StdHost::new();
        let empty = MethodRef {
            owner: "java/util/OptionalInt".into(),
            name: "empty".into(),
            descriptor: "()Ljava/util/OptionalInt;".into(),
        };
        let of = MethodRef {
            owner: "java/util/OptionalInt".into(),
            name: "of".into(),
            descriptor: "(I)Ljava/util/OptionalInt;".into(),
        };
        let empty_value = host.invoke(&empty, &[]).unwrap();
        let value = host.invoke(&of, &[Value::Int(42)]).unwrap();
        assert_eq!(empty_value, Value::Ref(None));
        assert_eq!(value, Value::Ref(Some("optional-int/42".into())));

        let present = MethodRef {
            owner: "java/util/OptionalInt".into(),
            name: "isPresent".into(),
            descriptor: "()Z".into(),
        };
        let get = MethodRef {
            owner: "java/util/OptionalInt".into(),
            name: "getAsInt".into(),
            descriptor: "()I".into(),
        };
        let or_else = MethodRef {
            owner: "java/util/OptionalInt".into(),
            name: "orElse".into(),
            descriptor: "(I)I".into(),
        };
        assert_eq!(
            host.invoke_instance(&present, &value, &[]).unwrap(),
            Value::Int(1)
        );
        assert_eq!(
            host.invoke_instance(&get, &value, &[]).unwrap(),
            Value::Int(42)
        );
        assert_eq!(
            host.invoke_instance(&or_else, &empty_value, &[Value::Int(7)])
                .unwrap(),
            Value::Int(7)
        );

        let to_string = MethodRef {
            owner: "java/util/OptionalInt".into(),
            name: "toString".into(),
            descriptor: "()Ljava/lang/String;".into(),
        };
        assert_eq!(
            host.invoke_instance(&to_string, &value, &[]).unwrap(),
            Value::Ref(Some("OptionalInt[42]".into()))
        );
        assert!(host.invoke_instance(&get, &empty_value, &[]).is_err());
    }

    #[test]
    fn standard_host_maps_optional_long_and_double_values() {
        let mut host = StdHost::new();
        let long_empty = MethodRef {
            owner: "java/util/OptionalLong".into(),
            name: "empty".into(),
            descriptor: "()Ljava/util/OptionalLong;".into(),
        };
        let long_of = MethodRef {
            owner: "java/util/OptionalLong".into(),
            name: "of".into(),
            descriptor: "(J)Ljava/util/OptionalLong;".into(),
        };
        let empty_long = host.invoke(&long_empty, &[]).unwrap();
        let long = host
            .invoke(
                &long_of,
                &[Value::Ref(Some("long:9007199254740991".into()))],
            )
            .unwrap();
        assert_eq!(empty_long, Value::Ref(None));
        assert_eq!(
            long,
            Value::Ref(Some("optional-long/9007199254740991".into()))
        );
        let long_get = MethodRef {
            owner: "java/util/OptionalLong".into(),
            name: "getAsLong".into(),
            descriptor: "()J".into(),
        };
        let long_or_else = MethodRef {
            owner: "java/util/OptionalLong".into(),
            name: "orElse".into(),
            descriptor: "(J)J".into(),
        };
        assert_eq!(
            host.invoke_instance(&long_get, &long, &[]).unwrap(),
            Value::Ref(Some("long:9007199254740991".into()))
        );
        assert_eq!(
            host.invoke_instance(
                &long_or_else,
                &empty_long,
                &[Value::Ref(Some("long:7".into()))]
            )
            .unwrap(),
            Value::Ref(Some("long:7".into()))
        );

        let double_empty = MethodRef {
            owner: "java/util/OptionalDouble".into(),
            name: "empty".into(),
            descriptor: "()Ljava/util/OptionalDouble;".into(),
        };
        let double_of = MethodRef {
            owner: "java/util/OptionalDouble".into(),
            name: "of".into(),
            descriptor: "(D)Ljava/util/OptionalDouble;".into(),
        };
        let empty_double = host.invoke(&double_empty, &[]).unwrap();
        let double = host
            .invoke(&double_of, &[Value::Ref(Some("double:3.5".into()))])
            .unwrap();
        assert_eq!(empty_double, Value::Ref(None));
        assert_eq!(double, Value::Ref(Some("optional-double/3.5".into())));
        let double_get = MethodRef {
            owner: "java/util/OptionalDouble".into(),
            name: "getAsDouble".into(),
            descriptor: "()D".into(),
        };
        let double_or_else = MethodRef {
            owner: "java/util/OptionalDouble".into(),
            name: "orElse".into(),
            descriptor: "(D)D".into(),
        };
        assert_eq!(
            host.invoke_instance(&double_get, &double, &[]).unwrap(),
            Value::Ref(Some("double:3.5".into()))
        );
        assert_eq!(
            host.invoke_instance(
                &double_or_else,
                &empty_double,
                &[Value::Ref(Some("double:2.25".into()))]
            )
            .unwrap(),
            Value::Ref(Some("double:2.25".into()))
        );
        let to_string = MethodRef {
            owner: "java/util/OptionalDouble".into(),
            name: "toString".into(),
            descriptor: "()Ljava/lang/String;".into(),
        };
        assert_eq!(
            host.invoke_instance(&to_string, &double, &[]).unwrap(),
            Value::Ref(Some("OptionalDouble[3.5]".into()))
        );
        assert!(
            host.invoke_instance(&double_get, &empty_double, &[])
                .is_err()
        );
    }

    #[test]
    fn standard_host_maps_common_collections() {
        let mut host = StdHost::new();
        let list_of = MethodRef {
            owner: "java/util/List".into(),
            name: "of".into(),
            descriptor: "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/util/List;".into(),
        };
        let list = host
            .invoke(&list_of, &[Value::Ref(Some("alpha".into())), Value::Int(7)])
            .unwrap();
        let size = MethodRef {
            owner: "java/util/List".into(),
            name: "size".into(),
            descriptor: "()I".into(),
        };
        assert_eq!(
            host.invoke_instance(&size, &list, &[]).unwrap(),
            Value::Int(2)
        );
        let get = MethodRef {
            owner: "java/util/List".into(),
            name: "get".into(),
            descriptor: "(I)Ljava/lang/Object;".into(),
        };
        assert_eq!(
            host.invoke_instance(&get, &list, &[Value::Int(1)]).unwrap(),
            Value::Int(7)
        );
        let contains = MethodRef {
            owner: "java/util/Collection".into(),
            name: "contains".into(),
            descriptor: "(Ljava/lang/Object;)Z".into(),
        };
        assert_eq!(
            host.invoke_instance(&contains, &list, &[Value::Ref(Some("alpha".into()))],)
                .unwrap(),
            Value::Int(1)
        );
        let collection_for_each = MethodRef {
            owner: "java/util/Collection".into(),
            name: "forEach".into(),
            descriptor: "(Ljava/util/function/Consumer;)V".into(),
        };
        assert_eq!(
            host.invoke_instance(
                &collection_for_each,
                &list,
                &[Value::Ref(Some("@indy:list-consumer".into()))],
            )
            .unwrap(),
            Value::Void
        );
        assert_eq!(
            host.calls()
                .iter()
                .filter(|call| call.owner == "java/util/function/Consumer" && call.name == "accept")
                .count(),
            2
        );
        let map_of = MethodRef {
            owner: "java/util/Map".into(),
            name: "of".into(),
            descriptor: "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/util/Map;".into(),
        };
        let map = host
            .invoke(
                &map_of,
                &[
                    Value::Ref(Some("key".into())),
                    Value::Ref(Some("value".into())),
                ],
            )
            .unwrap();
        let map_get = MethodRef {
            owner: "java/util/Map".into(),
            name: "get".into(),
            descriptor: "(Ljava/lang/Object;)Ljava/lang/Object;".into(),
        };
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "java/util/Map".into(),
                    name: "size".into(),
                    descriptor: "()I".into(),
                },
                &map,
                &[],
            )
            .unwrap(),
            Value::Int(1)
        );
        assert_eq!(
            host.invoke_instance(&map_get, &map, &[Value::Ref(Some("key".into()))],)
                .unwrap(),
            Value::Ref(Some("value".into()))
        );
        let hashmap_get = MethodRef {
            owner: "java/util/HashMap".into(),
            name: "get".into(),
            descriptor: "(Ljava/lang/Object;)Ljava/lang/Object;".into(),
        };
        assert_eq!(
            host.invoke_instance(&hashmap_get, &map, &[Value::Ref(Some("key".into()))],)
                .unwrap(),
            Value::Ref(Some("value".into()))
        );
        let hashmap_size = MethodRef {
            owner: "java/util/HashMap".into(),
            name: "size".into(),
            descriptor: "()I".into(),
        };
        assert_eq!(
            host.invoke_instance(&hashmap_size, &map, &[]).unwrap(),
            Value::Int(1)
        );
        let get_or_default = MethodRef {
            owner: "java/util/Map".into(),
            name: "getOrDefault".into(),
            descriptor: "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;".into(),
        };
        assert_eq!(
            host.invoke_instance(
                &get_or_default,
                &map,
                &[
                    Value::Ref(Some("missing".into())),
                    Value::Ref(Some("fallback".into())),
                ],
            )
            .unwrap(),
            Value::Ref(Some("fallback".into()))
        );
        let key_set = MethodRef {
            owner: "java/util/Map".into(),
            name: "keySet".into(),
            descriptor: "()Ljava/util/Set;".into(),
        };
        let keys = host.invoke_instance(&key_set, &map, &[]).unwrap();
        assert_eq!(
            host.invoke_instance(&size, &keys, &[]).unwrap(),
            Value::Int(1)
        );
        assert_eq!(
            host.invoke_instance(&contains, &keys, &[Value::Ref(Some("key".into()))])
                .unwrap(),
            Value::Int(1)
        );
        let values = MethodRef {
            owner: "java/util/Map".into(),
            name: "values".into(),
            descriptor: "()Ljava/util/Collection;".into(),
        };
        let values = host.invoke_instance(&values, &map, &[]).unwrap();
        assert_eq!(
            host.invoke_instance(&contains, &values, &[Value::Ref(Some("value".into()))])
                .unwrap(),
            Value::Int(1)
        );
        let entry_set = MethodRef {
            owner: "java/util/Map".into(),
            name: "entrySet".into(),
            descriptor: "()Ljava/util/Set;".into(),
        };
        let entries = host.invoke_instance(&entry_set, &map, &[]).unwrap();
        assert_eq!(
            host.invoke_instance(&size, &entries, &[]).unwrap(),
            Value::Int(1)
        );
        let iterator = MethodRef {
            owner: "java/util/Collection".into(),
            name: "iterator".into(),
            descriptor: "()Ljava/util/Iterator;".into(),
        };
        let iterator = host.invoke_instance(&iterator, &list, &[]).unwrap();
        let has_next = MethodRef {
            owner: "java/util/Iterator".into(),
            name: "hasNext".into(),
            descriptor: "()Z".into(),
        };
        let next = MethodRef {
            owner: "java/util/Iterator".into(),
            name: "next".into(),
            descriptor: "()Ljava/lang/Object;".into(),
        };
        assert_eq!(
            host.invoke_instance(&has_next, &iterator, &[]).unwrap(),
            Value::Int(1)
        );
        assert_eq!(
            host.invoke_instance(&next, &iterator, &[]).unwrap(),
            Value::Ref(Some("alpha".into()))
        );
        assert_eq!(
            host.invoke_instance(&has_next, &iterator, &[]).unwrap(),
            Value::Int(1)
        );
        assert_eq!(
            host.invoke_instance(&next, &iterator, &[]).unwrap(),
            Value::Int(7)
        );
        assert_eq!(
            host.invoke_instance(&has_next, &iterator, &[]).unwrap(),
            Value::Int(0)
        );
        assert!(host.invoke_instance(&next, &iterator, &[]).is_err());
        let remaining_iterator = host
            .invoke_instance(
                &MethodRef {
                    owner: "java/util/Collection".into(),
                    name: "iterator".into(),
                    descriptor: "()Ljava/util/Iterator;".into(),
                },
                &list,
                &[],
            )
            .unwrap();
        let for_each_remaining = MethodRef {
            owner: "java/util/Iterator".into(),
            name: "forEachRemaining".into(),
            descriptor: "(Ljava/util/function/Consumer;)V".into(),
        };
        assert_eq!(
            host.invoke_instance(
                &for_each_remaining,
                &remaining_iterator,
                &[Value::Ref(Some("@indy:remaining-consumer".into()))],
            )
            .unwrap(),
            Value::Void
        );
        assert_eq!(
            host.invoke_instance(&has_next, &remaining_iterator, &[])
                .unwrap(),
            Value::Int(0)
        );
        assert_eq!(
            host.calls()
                .iter()
                .filter(|call| call.owner == "java/util/function/Consumer" && call.name == "accept")
                .count(),
            4
        );
        let entries_iterator = host
            .invoke_instance(
                &MethodRef {
                    owner: "java/util/Collection".into(),
                    name: "iterator".into(),
                    descriptor: "()Ljava/util/Iterator;".into(),
                },
                &entries,
                &[],
            )
            .unwrap();
        let entry = host.invoke_instance(&next, &entries_iterator, &[]).unwrap();
        let entry_key = MethodRef {
            owner: "java/util/Map$Entry".into(),
            name: "getKey".into(),
            descriptor: "()Ljava/lang/Object;".into(),
        };
        let entry_value = MethodRef {
            owner: "java/util/Map$Entry".into(),
            name: "getValue".into(),
            descriptor: "()Ljava/lang/Object;".into(),
        };
        assert_eq!(
            host.invoke_instance(&entry_key, &entry, &[]).unwrap(),
            Value::Ref(Some("key".into()))
        );
        assert_eq!(
            host.invoke_instance(&entry_value, &entry, &[]).unwrap(),
            Value::Ref(Some("value".into()))
        );
        let entry_equals = MethodRef {
            owner: "java/util/Map$Entry".into(),
            name: "equals".into(),
            descriptor: "(Ljava/lang/Object;)Z".into(),
        };
        assert_eq!(
            host.invoke_instance(&entry_equals, &entry, std::slice::from_ref(&entry))
                .unwrap(),
            Value::Int(1)
        );
        assert_eq!(
            host.invoke_instance(
                &entry_equals,
                &entry,
                &[Value::Ref(Some("map-entry/rkey,rother".into()))],
            )
            .unwrap(),
            Value::Int(0)
        );
        let entry_hash = MethodRef {
            owner: "java/util/Map$Entry".into(),
            name: "hashCode".into(),
            descriptor: "()I".into(),
        };
        assert_eq!(
            host.invoke_instance(&entry_hash, &entry, &[]).unwrap(),
            Value::Int(stable_hash("map-entry/rkey,rvalue"))
        );
        let remove_if = MethodRef {
            owner: "java/util/Collection".into(),
            name: "removeIf".into(),
            descriptor: "(Ljava/util/function/Predicate;)Z".into(),
        };
        assert_eq!(
            host.invoke_instance(
                &remove_if,
                &list,
                &[Value::Ref(Some("@indy:list-predicate".into()))],
            )
            .unwrap(),
            Value::Int(1)
        );
        assert_eq!(
            host.invoke_instance(&size, &list, &[]).unwrap(),
            Value::Int(0)
        );
    }

    #[test]
    fn standard_host_composes_function_markers() {
        let mut host = StdHost::new();
        let predicate = MethodRef {
            owner: "java/util/function/Predicate".into(),
            name: "and".into(),
            descriptor: "(Ljava/util/function/Predicate;)Ljava/util/function/Predicate;".into(),
        };
        let combined = host
            .invoke_instance(
                &predicate,
                &Value::Ref(Some("@indy:left".into())),
                &[Value::Ref(Some("@indy:right".into()))],
            )
            .unwrap();
        let test = MethodRef {
            owner: "java/util/function/Predicate".into(),
            name: "test".into(),
            descriptor: "(Ljava/lang/Object;)Z".into(),
        };
        assert_eq!(
            host.invoke_instance(&test, &combined, &[Value::Ref(Some("x".into()))])
                .unwrap(),
            Value::Int(1)
        );
        let negate = MethodRef {
            owner: "java/util/function/Predicate".into(),
            name: "negate".into(),
            descriptor: "()Ljava/util/function/Predicate;".into(),
        };
        let negated = host.invoke_instance(&negate, &combined, &[]).unwrap();
        assert_eq!(
            host.invoke_instance(&test, &negated, &[Value::Ref(Some("x".into()))])
                .unwrap(),
            Value::Int(0)
        );

        let consumer = MethodRef {
            owner: "java/util/function/Consumer".into(),
            name: "andThen".into(),
            descriptor: "(Ljava/util/function/Consumer;)Ljava/util/function/Consumer;".into(),
        };
        let chained = host
            .invoke_instance(
                &consumer,
                &Value::Ref(Some("@indy:first".into())),
                &[Value::Ref(Some("@indy:second".into()))],
            )
            .unwrap();
        let accept = MethodRef {
            owner: "java/util/function/Consumer".into(),
            name: "accept".into(),
            descriptor: "(Ljava/lang/Object;)V".into(),
        };
        assert_eq!(
            host.invoke_instance(&accept, &chained, &[Value::Ref(Some("x".into()))])
                .unwrap(),
            Value::Void
        );
        assert_eq!(
            host.calls()
                .iter()
                .filter(|call| call.owner == "java/util/function/Consumer" && call.name == "accept")
                .count(),
            3
        );
    }

    #[test]
    fn standard_host_preserves_mutable_list_and_map_updates() {
        let mut host = StdHost::new();
        let list = Value::Ref(Some("java/util/ArrayList".into()));
        let init = MethodRef {
            owner: "java/util/ArrayList".into(),
            name: "<init>".into(),
            descriptor: "()V".into(),
        };
        assert_eq!(
            host.invoke_instance(&init, &list, &[]).unwrap(),
            Value::Void
        );
        let add = MethodRef {
            owner: "java/util/ArrayList".into(),
            name: "add".into(),
            descriptor: "(Ljava/lang/Object;)Z".into(),
        };
        assert_eq!(
            host.invoke_instance(&add, &list, &[Value::Ref(Some("entry".into()))])
                .unwrap(),
            Value::Int(1)
        );
        let list_size = MethodRef {
            owner: "java/util/ArrayList".into(),
            name: "size".into(),
            descriptor: "()I".into(),
        };
        assert_eq!(
            host.invoke_instance(&list_size, &list, &[]).unwrap(),
            Value::Int(1)
        );
        let list_get = MethodRef {
            owner: "java/util/ArrayList".into(),
            name: "get".into(),
            descriptor: "(I)Ljava/lang/Object;".into(),
        };
        assert_eq!(
            host.invoke_instance(&list_get, &list, &[Value::Int(0)])
                .unwrap(),
            Value::Ref(Some("entry".into()))
        );

        let map = Value::Ref(Some("java/util/HashMap".into()));
        let map_init = MethodRef {
            owner: "java/util/HashMap".into(),
            name: "<init>".into(),
            descriptor: "()V".into(),
        };
        host.invoke_instance(&map_init, &map, &[]).unwrap();
        let put = MethodRef {
            owner: "java/util/HashMap".into(),
            name: "put".into(),
            descriptor: "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;".into(),
        };
        assert_eq!(
            host.invoke_instance(
                &put,
                &map,
                &[
                    Value::Ref(Some("key".into())),
                    Value::Ref(Some("value".into())),
                ],
            )
            .unwrap(),
            Value::Ref(None)
        );
        let map_size = MethodRef {
            owner: "java/util/HashMap".into(),
            name: "size".into(),
            descriptor: "()I".into(),
        };
        assert_eq!(
            host.invoke_instance(&map_size, &map, &[]).unwrap(),
            Value::Int(1)
        );
        let map_get = MethodRef {
            owner: "java/util/HashMap".into(),
            name: "get".into(),
            descriptor: "(Ljava/lang/Object;)Ljava/lang/Object;".into(),
        };
        assert_eq!(
            host.invoke_instance(&map_get, &map, &[Value::Ref(Some("key".into()))])
                .unwrap(),
            Value::Ref(Some("value".into()))
        );
        let compute = MethodRef {
            owner: "java/util/Map".into(),
            name: "computeIfAbsent".into(),
            descriptor: "(Ljava/lang/Object;Ljava/util/function/Function;)Ljava/lang/Object;"
                .into(),
        };
        assert_eq!(
            host.invoke_instance(
                &compute,
                &map,
                &[
                    Value::Ref(Some("computed-key".into())),
                    Value::Ref(Some(
                        "@indy:demo#0:0:demo/Factory#make()Ljava/lang/Object;:".into()
                    )),
                ],
            )
            .unwrap(),
            Value::Ref(Some("computed/rcomputed-key".into()))
        );
        assert_eq!(
            host.invoke_instance(&map_get, &map, &[Value::Ref(Some("computed-key".into()))])
                .unwrap(),
            Value::Ref(Some("computed/rcomputed-key".into()))
        );
        let compute_present = MethodRef {
            owner: "java/util/Map".into(),
            name: "computeIfPresent".into(),
            descriptor: "(Ljava/lang/Object;Ljava/util/function/BiFunction;)Ljava/lang/Object;"
                .into(),
        };
        assert_eq!(
            host.invoke_instance(
                &compute_present,
                &map,
                &[
                    Value::Ref(Some("computed-key".into())),
                    Value::Ref(Some("@indy:compute-present".into())),
                ],
            )
            .unwrap(),
            Value::Ref(Some("computed/rcomputed-key".into()))
        );
        let merge = MethodRef {
            owner: "java/util/Map".into(),
            name: "merge".into(),
            descriptor:
                "(Ljava/lang/Object;Ljava/lang/Object;Ljava/util/function/BiFunction;)Ljava/lang/Object;"
                    .into(),
        };
        assert_eq!(
            host.invoke_instance(
                &merge,
                &map,
                &[
                    Value::Ref(Some("merged-key".into())),
                    Value::Ref(Some("merged-value".into())),
                    Value::Ref(Some("@indy:merge".into())),
                ],
            )
            .unwrap(),
            Value::Ref(Some("merged-value".into()))
        );
        assert_eq!(
            host.invoke_instance(&map_get, &map, &[Value::Ref(Some("merged-key".into()))],)
                .unwrap(),
            Value::Ref(Some("merged-value".into()))
        );
        assert_eq!(
            host.invoke_instance(
                &merge,
                &map,
                &[
                    Value::Ref(Some("merged-key".into())),
                    Value::Ref(Some("updated-value".into())),
                    Value::Ref(Some("@indy:merge-existing".into())),
                ],
            )
            .unwrap(),
            Value::Ref(Some("updated-value".into()))
        );
        assert_eq!(
            host.invoke_instance(&map_get, &map, &[Value::Ref(Some("merged-key".into()))],)
                .unwrap(),
            Value::Ref(Some("updated-value".into()))
        );
        let put_if_absent = MethodRef {
            owner: "java/util/Map".into(),
            name: "putIfAbsent".into(),
            descriptor: "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;".into(),
        };
        assert_eq!(
            host.invoke_instance(
                &put_if_absent,
                &map,
                &[
                    Value::Ref(Some("key".into())),
                    Value::Ref(Some("new".into()))
                ],
            )
            .unwrap(),
            Value::Ref(Some("value".into()))
        );
        assert_eq!(
            host.invoke_instance(
                &put_if_absent,
                &map,
                &[
                    Value::Ref(Some("other".into())),
                    Value::Ref(Some("added".into()))
                ],
            )
            .unwrap(),
            Value::Ref(None)
        );
        let replace = MethodRef {
            owner: "java/util/Map".into(),
            name: "replace".into(),
            descriptor: "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;".into(),
        };
        assert_eq!(
            host.invoke_instance(
                &replace,
                &map,
                &[
                    Value::Ref(Some("key".into())),
                    Value::Ref(Some("updated".into()))
                ],
            )
            .unwrap(),
            Value::Ref(Some("value".into()))
        );
        let replace_all = MethodRef {
            owner: "java/util/Map".into(),
            name: "replaceAll".into(),
            descriptor: "(Ljava/util/function/BiFunction;)V".into(),
        };
        assert_eq!(
            host.invoke_instance(
                &replace_all,
                &map,
                &[Value::Ref(Some("@indy:map-replace".into()))],
            )
            .unwrap(),
            Value::Void
        );
        assert_eq!(
            host.invoke_instance(&map_get, &map, &[Value::Ref(Some("key".into()))])
                .unwrap(),
            Value::Ref(Some("updated".into()))
        );
        assert!(
            host.calls().iter().any(|call| {
                call.owner == "java/util/function/BiFunction" && call.name == "apply"
            })
        );
        let remove_if = MethodRef {
            owner: "java/util/Map".into(),
            name: "remove".into(),
            descriptor: "(Ljava/lang/Object;Ljava/lang/Object;)Z".into(),
        };
        assert_eq!(
            host.invoke_instance(
                &remove_if,
                &map,
                &[
                    Value::Ref(Some("other".into())),
                    Value::Ref(Some("added".into()))
                ],
            )
            .unwrap(),
            Value::Int(1)
        );
    }

    #[test]
    fn standard_host_invokes_map_for_each_callbacks() {
        let mut host = StdHost::new();
        let map = Value::Ref(Some("java/util/HashMap".into()));
        host.invoke_instance(
            &MethodRef {
                owner: "java/util/HashMap".into(),
                name: "<init>".into(),
                descriptor: "()V".into(),
            },
            &map,
            &[],
        )
        .unwrap();
        let put = MethodRef {
            owner: "java/util/HashMap".into(),
            name: "put".into(),
            descriptor: "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;".into(),
        };
        for (key, value) in [("first", "one"), ("second", "two")] {
            host.invoke_instance(
                &put,
                &map,
                &[Value::Ref(Some(key.into())), Value::Ref(Some(value.into()))],
            )
            .unwrap();
        }
        let for_each = MethodRef {
            owner: "java/util/Map".into(),
            name: "forEach".into(),
            descriptor: "(Ljava/util/function/BiConsumer;)V".into(),
        };
        assert_eq!(
            host.invoke_instance(
                &for_each,
                &map,
                &[Value::Ref(Some("@indy:map-consumer".into()))],
            )
            .unwrap(),
            Value::Void
        );
        let callbacks = host
            .calls()
            .iter()
            .filter(|call| call.owner == "java/util/function/BiConsumer" && call.name == "accept")
            .count();
        assert_eq!(callbacks, 2);
    }

    #[test]
    fn standard_host_merges_mutable_collection_inputs() {
        let mut host = StdHost::new();
        let list = Value::Ref(Some("java/util/ArrayList".into()));
        let init = MethodRef {
            owner: "java/util/ArrayList".into(),
            name: "<init>".into(),
            descriptor: "()V".into(),
        };
        host.invoke_instance(&init, &list, &[]).unwrap();
        let source = host
            .invoke(
                &MethodRef {
                    owner: "java/util/List".into(),
                    name: "of".into(),
                    descriptor: "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/util/List;".into(),
                },
                &[Value::Ref(Some("a".into())), Value::Ref(Some("b".into()))],
            )
            .unwrap();
        let add_all = MethodRef {
            owner: "java/util/List".into(),
            name: "addAll".into(),
            descriptor: "(Ljava/util/Collection;)Z".into(),
        };
        assert_eq!(
            host.invoke_instance(&add_all, &list, &[source]).unwrap(),
            Value::Int(1)
        );
        let indexed_add_all = MethodRef {
            owner: "java/util/ArrayList".into(),
            name: "addAll".into(),
            descriptor: "(ILjava/util/Collection;)Z".into(),
        };
        assert_eq!(
            host.invoke_instance(
                &indexed_add_all,
                &list,
                &[Value::Int(1), Value::Ref(Some("list/rmarker".into()))],
            )
            .unwrap(),
            Value::Int(1)
        );
        let get = MethodRef {
            owner: "java/util/List".into(),
            name: "get".into(),
            descriptor: "(I)Ljava/lang/Object;".into(),
        };
        assert_eq!(
            host.invoke_instance(&get, &list, &[Value::Int(1)]).unwrap(),
            Value::Ref(Some("marker".into()))
        );

        let map = Value::Ref(Some("java/util/HashMap".into()));
        host.invoke_instance(
            &MethodRef {
                owner: "java/util/HashMap".into(),
                name: "<init>".into(),
                descriptor: "()V".into(),
            },
            &map,
            &[],
        )
        .unwrap();
        let map_source = host
            .invoke(
                &MethodRef {
                    owner: "java/util/Map".into(),
                    name: "of".into(),
                    descriptor: "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/util/Map;".into(),
                },
                &[Value::Ref(Some("k".into())), Value::Ref(Some("v".into()))],
            )
            .unwrap();
        let put_all = MethodRef {
            owner: "java/util/Map".into(),
            name: "putAll".into(),
            descriptor: "(Ljava/util/Map;)V".into(),
        };
        assert_eq!(
            host.invoke_instance(&put_all, &map, &[map_source]).unwrap(),
            Value::Void
        );
        let get = MethodRef {
            owner: "java/util/HashMap".into(),
            name: "get".into(),
            descriptor: "(Ljava/lang/Object;)Ljava/lang/Object;".into(),
        };
        assert_eq!(
            host.invoke_instance(&get, &map, &[Value::Ref(Some("k".into()))])
                .unwrap(),
            Value::Ref(Some("v".into()))
        );
    }

    #[test]
    fn standard_host_removes_and_locates_mutable_list_values() {
        let mut host = StdHost::new();
        let list = Value::Ref(Some("java/util/ArrayList".into()));
        host.invoke_instance(
            &MethodRef {
                owner: "java/util/ArrayList".into(),
                name: "<init>".into(),
                descriptor: "()V".into(),
            },
            &list,
            &[],
        )
        .unwrap();
        let add = MethodRef {
            owner: "java/util/List".into(),
            name: "add".into(),
            descriptor: "(Ljava/lang/Object;)Z".into(),
        };
        for value in ["a", "b", "a"] {
            host.invoke_instance(&add, &list, &[Value::Ref(Some(value.into()))])
                .unwrap();
        }
        let index_of = MethodRef {
            owner: "java/util/List".into(),
            name: "indexOf".into(),
            descriptor: "(Ljava/lang/Object;)I".into(),
        };
        assert_eq!(
            host.invoke_instance(&index_of, &list, &[Value::Ref(Some("a".into()))])
                .unwrap(),
            Value::Int(0)
        );
        let last_index_of = MethodRef {
            owner: "java/util/List".into(),
            name: "lastIndexOf".into(),
            descriptor: "(Ljava/lang/Object;)I".into(),
        };
        assert_eq!(
            host.invoke_instance(&last_index_of, &list, &[Value::Ref(Some("a".into()))])
                .unwrap(),
            Value::Int(2)
        );
        let remove_value = MethodRef {
            owner: "java/util/List".into(),
            name: "remove".into(),
            descriptor: "(Ljava/lang/Object;)Z".into(),
        };
        assert_eq!(
            host.invoke_instance(&remove_value, &list, &[Value::Ref(Some("a".into()))])
                .unwrap(),
            Value::Int(1)
        );
        let remove_index = MethodRef {
            owner: "java/util/ArrayList".into(),
            name: "remove".into(),
            descriptor: "(I)Ljava/lang/Object;".into(),
        };
        assert_eq!(
            host.invoke_instance(&remove_index, &list, &[Value::Int(0)])
                .unwrap(),
            Value::Ref(Some("b".into()))
        );
        assert_eq!(
            host.invoke_instance(&index_of, &list, &[Value::Ref(Some("missing".into()))])
                .unwrap(),
            Value::Int(-1)
        );
    }

    #[test]
    fn standard_host_preserves_mutable_set_uniqueness_and_removal() {
        let mut host = StdHost::new();
        let set = Value::Ref(Some("java/util/HashSet".into()));
        let init = MethodRef {
            owner: "java/util/HashSet".into(),
            name: "<init>".into(),
            descriptor: "()V".into(),
        };
        host.invoke_instance(&init, &set, &[]).unwrap();
        let add = MethodRef {
            owner: "java/util/Set".into(),
            name: "add".into(),
            descriptor: "(Ljava/lang/Object;)Z".into(),
        };
        assert_eq!(
            host.invoke_instance(&add, &set, &[Value::Ref(Some("entry".into()))])
                .unwrap(),
            Value::Int(1)
        );
        assert_eq!(
            host.invoke_instance(&add, &set, &[Value::Ref(Some("entry".into()))])
                .unwrap(),
            Value::Int(0)
        );
        let size = MethodRef {
            owner: "java/util/Set".into(),
            name: "size".into(),
            descriptor: "()I".into(),
        };
        assert_eq!(
            host.invoke_instance(&size, &set, &[]).unwrap(),
            Value::Int(1)
        );
        let remove = MethodRef {
            owner: "java/util/HashSet".into(),
            name: "remove".into(),
            descriptor: "(Ljava/lang/Object;)Z".into(),
        };
        assert_eq!(
            host.invoke_instance(&remove, &set, &[Value::Ref(Some("entry".into()))])
                .unwrap(),
            Value::Int(1)
        );
        assert_eq!(
            host.invoke_instance(&remove, &set, &[Value::Ref(Some("entry".into()))])
                .unwrap(),
            Value::Int(0)
        );
    }

    #[test]
    fn standard_host_checks_collection_containment_as_a_view() {
        let mut host = StdHost::new();
        let set = Value::Ref(Some("java/util/HashSet".into()));
        host.invoke_instance(
            &MethodRef {
                owner: "java/util/HashSet".into(),
                name: "<init>".into(),
                descriptor: "()V".into(),
            },
            &set,
            &[],
        )
        .unwrap();
        let add = MethodRef {
            owner: "java/util/Set".into(),
            name: "add".into(),
            descriptor: "(Ljava/lang/Object;)Z".into(),
        };
        for value in ["alpha", "beta"] {
            host.invoke_instance(&add, &set, &[Value::Ref(Some(value.into()))])
                .unwrap();
        }
        let source = host
            .invoke(
                &MethodRef {
                    owner: "java/util/Set".into(),
                    name: "of".into(),
                    descriptor: "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/util/Set;".into(),
                },
                &[
                    Value::Ref(Some("alpha".into())),
                    Value::Ref(Some("beta".into())),
                ],
            )
            .unwrap();
        let contains_all = MethodRef {
            owner: "java/util/Collection".into(),
            name: "containsAll".into(),
            descriptor: "(Ljava/util/Collection;)Z".into(),
        };
        assert_eq!(
            host.invoke_instance(&contains_all, &set, &[source])
                .unwrap(),
            Value::Int(1)
        );
        let missing = host
            .invoke(
                &MethodRef {
                    owner: "java/util/List".into(),
                    name: "of".into(),
                    descriptor: "(Ljava/lang/Object;)Ljava/util/List;".into(),
                },
                &[Value::Ref(Some("gamma".into()))],
            )
            .unwrap();
        assert_eq!(
            host.invoke_instance(&contains_all, &set, &[missing])
                .unwrap(),
            Value::Int(0)
        );
    }

    #[test]
    fn standard_host_mutates_collection_views_with_remove_and_retain_all() {
        let mut host = StdHost::new();
        let set = Value::Ref(Some("java/util/HashSet".into()));
        host.invoke_instance(
            &MethodRef {
                owner: "java/util/HashSet".into(),
                name: "<init>".into(),
                descriptor: "()V".into(),
            },
            &set,
            &[],
        )
        .unwrap();
        let add = MethodRef {
            owner: "java/util/Set".into(),
            name: "add".into(),
            descriptor: "(Ljava/lang/Object;)Z".into(),
        };
        for value in ["alpha", "beta", "gamma"] {
            host.invoke_instance(&add, &set, &[Value::Ref(Some(value.into()))])
                .unwrap();
        }
        let source = host
            .invoke(
                &MethodRef {
                    owner: "java/util/Set".into(),
                    name: "of".into(),
                    descriptor: "(Ljava/lang/Object;)Ljava/util/Set;".into(),
                },
                &[Value::Ref(Some("beta".into()))],
            )
            .unwrap();
        let remove_all = MethodRef {
            owner: "java/util/Collection".into(),
            name: "removeAll".into(),
            descriptor: "(Ljava/util/Collection;)Z".into(),
        };
        assert_eq!(
            host.invoke_instance(&remove_all, &set, &[source]).unwrap(),
            Value::Int(1)
        );
        let contains = MethodRef {
            owner: "java/util/Set".into(),
            name: "contains".into(),
            descriptor: "(Ljava/lang/Object;)Z".into(),
        };
        assert_eq!(
            host.invoke_instance(&contains, &set, &[Value::Ref(Some("beta".into()))])
                .unwrap(),
            Value::Int(0)
        );
        let retain_source = host
            .invoke(
                &MethodRef {
                    owner: "java/util/List".into(),
                    name: "of".into(),
                    descriptor: "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/util/List;".into(),
                },
                &[
                    Value::Ref(Some("alpha".into())),
                    Value::Ref(Some("delta".into())),
                ],
            )
            .unwrap();
        let retain_all = MethodRef {
            owner: "java/util/Collection".into(),
            name: "retainAll".into(),
            descriptor: "(Ljava/util/Collection;)Z".into(),
        };
        assert_eq!(
            host.invoke_instance(&retain_all, &set, &[retain_source])
                .unwrap(),
            Value::Int(1)
        );
        assert_eq!(
            host.invoke_instance(&contains, &set, &[Value::Ref(Some("alpha".into()))])
                .unwrap(),
            Value::Int(1)
        );
        assert_eq!(
            host.invoke_instance(&contains, &set, &[Value::Ref(Some("gamma".into()))])
                .unwrap(),
            Value::Int(0)
        );
    }

    #[test]
    fn standard_host_exposes_system_properties_as_a_map() {
        let mut host = StdHost::new().with_property("demo.mode", "native");
        let get_properties = MethodRef {
            owner: "java/lang/System".into(),
            name: "getProperties".into(),
            descriptor: "()Ljava/util/Properties;".into(),
        };
        let properties = host.invoke(&get_properties, &[]).unwrap();
        let get = MethodRef {
            owner: "java/util/Properties".into(),
            name: "get".into(),
            descriptor: "(Ljava/lang/Object;)Ljava/lang/Object;".into(),
        };
        assert_eq!(
            host.invoke_instance(&get, &properties, &[Value::Ref(Some("demo.mode".into()))],)
                .unwrap(),
            Value::Ref(Some("native".into()))
        );
        let size = MethodRef {
            owner: "java/util/Properties".into(),
            name: "size".into(),
            descriptor: "()I".into(),
        };
        assert_eq!(
            host.invoke_instance(&size, &properties, &[]).unwrap(),
            Value::Int(2)
        );
    }

    #[test]
    fn standard_host_uses_seeded_current_time_millis() {
        let mut host = StdHost::new().with_current_time_millis(1_700_000_000);
        let current_time_millis = MethodRef {
            owner: "java/lang/System".into(),
            name: "currentTimeMillis".into(),
            descriptor: "()J".into(),
        };
        assert_eq!(
            host.invoke(&current_time_millis, &[]).unwrap(),
            Value::Int(1_700_000_000)
        );

        // The default is deterministic too; no host clock is consulted.
        let mut default_host = StdHost::new();
        assert_eq!(
            default_host.invoke(&current_time_millis, &[]).unwrap(),
            Value::Int(0)
        );
        let identity_hash = MethodRef {
            owner: "java/lang/System".into(),
            name: "identityHashCode".into(),
            descriptor: "(Ljava/lang/Object;)I".into(),
        };
        assert_eq!(
            host.invoke(&identity_hash, &[Value::Ref(None)]).unwrap(),
            Value::Int(0)
        );
        assert_eq!(
            host.invoke(&identity_hash, &[Value::Ref(Some("demo".into()))])
                .unwrap(),
            Value::Int(stable_hash("demo"))
        );
    }

    #[test]
    fn standard_host_maps_main_thread_bootstrap_markers() {
        let mut host = StdHost::new();
        let current_thread = MethodRef {
            owner: "java/lang/Thread".into(),
            name: "currentThread".into(),
            descriptor: "()Ljava/lang/Thread;".into(),
        };
        let thread = host.invoke(&current_thread, &[]).unwrap();
        assert_eq!(thread, Value::Ref(Some("thread/main".into())));

        for (name, descriptor, expected) in [
            (
                "getName",
                "()Ljava/lang/String;",
                Value::Ref(Some("main".into())),
            ),
            ("isDaemon", "()Z", Value::Int(0)),
            (
                "getContextClassLoader",
                "()Ljava/lang/ClassLoader;",
                Value::Ref(Some("classloader/bootstrap".into())),
            ),
        ] {
            assert_eq!(
                host.invoke_instance(
                    &MethodRef {
                        owner: "java/lang/Thread".into(),
                        name: name.into(),
                        descriptor: descriptor.into(),
                    },
                    &thread,
                    &[],
                )
                .unwrap(),
                expected
            );
        }
    }

    #[test]
    fn standard_host_uses_only_seeded_environment_values() {
        let mut host = StdHost::new().with_environment_variable("DEMO_MODE", "native");
        let getenv = MethodRef {
            owner: "java/lang/System".into(),
            name: "getenv".into(),
            descriptor: "(Ljava/lang/String;)Ljava/lang/String;".into(),
        };
        assert_eq!(
            host.invoke(&getenv, &[Value::Ref(Some("DEMO_MODE".into()))])
                .unwrap(),
            Value::Ref(Some("native".into()))
        );
        // Arbitrary process variables are deliberately invisible to StdHost.
        assert_eq!(
            host.invoke(&getenv, &[Value::Ref(Some("PATH".into()))])
                .unwrap(),
            Value::Ref(None)
        );

        let getenv_all = MethodRef {
            owner: "java/lang/System".into(),
            name: "getenv".into(),
            descriptor: "()Ljava/util/Map;".into(),
        };
        let values = host.invoke(&getenv_all, &[]).unwrap();
        let get = MethodRef {
            owner: "java/util/Map".into(),
            name: "get".into(),
            descriptor: "(Ljava/lang/Object;)Ljava/lang/Object;".into(),
        };
        assert_eq!(
            host.invoke_instance(&get, &values, &[Value::Ref(Some("DEMO_MODE".into()))])
                .unwrap(),
            Value::Ref(Some("native".into()))
        );
    }

    #[test]
    fn standard_host_maps_bounded_class_metadata_queries() {
        let mut host = StdHost::new();
        let class = Value::Ref(Some("demo/Bootstrap".into()));
        let get_name = MethodRef {
            owner: "java/lang/Class".into(),
            name: "getName".into(),
            descriptor: "()Ljava/lang/String;".into(),
        };
        assert_eq!(
            host.invoke_instance(&get_name, &class, &[]).unwrap(),
            Value::Ref(Some("demo.Bootstrap".into()))
        );
        let canonical_name = MethodRef {
            owner: "java/lang/Class".into(),
            name: "getCanonicalName".into(),
            descriptor: "()Ljava/lang/String;".into(),
        };
        assert_eq!(
            host.invoke_instance(&canonical_name, &class, &[]).unwrap(),
            Value::Ref(Some("demo.Bootstrap".into()))
        );
        let simple_name = MethodRef {
            owner: "java/lang/Class".into(),
            name: "getSimpleName".into(),
            descriptor: "()Ljava/lang/String;".into(),
        };
        assert_eq!(
            host.invoke_instance(&simple_name, &class, &[]).unwrap(),
            Value::Ref(Some("Bootstrap".into()))
        );
        let is_primitive = MethodRef {
            owner: "java/lang/Class".into(),
            name: "isPrimitive".into(),
            descriptor: "()Z".into(),
        };
        assert_eq!(
            host.invoke_instance(&is_primitive, &Value::Ref(Some("int".into())), &[])
                .unwrap(),
            Value::Int(1)
        );
        let is_array = MethodRef {
            owner: "java/lang/Class".into(),
            name: "isArray".into(),
            descriptor: "()Z".into(),
        };
        assert_eq!(
            host.invoke_instance(
                &is_array,
                &Value::Ref(Some("[Ljava/lang/String;".into())),
                &[],
            )
            .unwrap(),
            Value::Int(1)
        );
        let assignable = MethodRef {
            owner: "java/lang/Class".into(),
            name: "isAssignableFrom".into(),
            descriptor: "(Ljava/lang/Class;)Z".into(),
        };
        assert_eq!(
            host.invoke_instance(
                &assignable,
                &Value::Ref(Some("java/lang/Object".into())),
                &[class],
            )
            .unwrap(),
            Value::Int(1)
        );
    }

    #[test]
    fn standard_host_maps_enum_value_and_identity() {
        let mut host = StdHost::new();
        let value_of = MethodRef {
            owner: "java/lang/Enum".into(),
            name: "valueOf".into(),
            descriptor: "(Ljava/lang/Class;Ljava/lang/String;)Ljava/lang/Enum;".into(),
        };
        let value = host
            .invoke(
                &value_of,
                &[
                    Value::Ref(Some("demo/Mode".into())),
                    Value::Ref(Some("ACTIVE".into())),
                ],
            )
            .unwrap();
        assert_eq!(value, Value::Ref(Some("enum/demo/Mode/ACTIVE".into())));
        let name = MethodRef {
            owner: "java/lang/Enum".into(),
            name: "name".into(),
            descriptor: "()Ljava/lang/String;".into(),
        };
        assert_eq!(
            host.invoke_instance(&name, &value, &[]).unwrap(),
            Value::Ref(Some("ACTIVE".into()))
        );
        let equals = MethodRef {
            owner: "java/lang/Enum".into(),
            name: "equals".into(),
            descriptor: "(Ljava/lang/Object;)Z".into(),
        };
        assert_eq!(
            host.invoke_instance(&equals, &value, std::slice::from_ref(&value))
                .unwrap(),
            Value::Int(1)
        );
    }

    #[test]
    fn standard_host_maps_fabric_loader_metadata_and_paths() {
        let mut host = StdHost::new().with_mod("demo", "1.2.3");
        let loader = Value::Ref(Some("fabric-loader".into()));
        let environment = MethodRef {
            owner: "net/fabricmc/loader/api/FabricLoader".into(),
            name: "getEnvironmentType".into(),
            descriptor: "()Lnet/fabricmc/api/EnvType;".into(),
        };
        assert_eq!(
            host.invoke_instance(&environment, &loader, &[]).unwrap(),
            Value::Ref(Some("CLIENT".into()))
        );
        let resolver_method = MethodRef {
            owner: "net/fabricmc/loader/api/FabricLoader".into(),
            name: "getMappingResolver".into(),
            descriptor: "()Lnet/fabricmc/loader/api/MappingResolver;".into(),
        };
        let resolver = host
            .invoke_instance(&resolver_method, &loader, &[])
            .unwrap();
        assert_eq!(resolver, Value::Ref(Some("fabric-mapping-resolver".into())));
        let map_class = MethodRef {
            owner: "net/fabricmc/loader/api/MappingResolver".into(),
            name: "mapClassName".into(),
            descriptor: "(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;".into(),
        };
        assert_eq!(
            host.invoke_instance(
                &map_class,
                &resolver,
                &[
                    Value::Ref(Some("named".into())),
                    Value::Ref(Some("net.minecraft.Example".into())),
                ],
            )
            .unwrap(),
            Value::Ref(Some("net.minecraft.Example".into()))
        );
        let map_method = MethodRef {
            owner: "net/fabricmc/loader/api/MappingResolver".into(),
            name: "mapMethodName".into(),
            descriptor: "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;".into(),
        };
        assert_eq!(
            host.invoke_instance(
                &map_method,
                &resolver,
                &[
                    Value::Ref(Some("named".into())),
                    Value::Ref(Some("net.minecraft.Example".into())),
                    Value::Ref(Some("tick".into())),
                    Value::Ref(Some("()V".into())),
                ],
            )
            .unwrap(),
            Value::Ref(Some("tick".into()))
        );
        let server_host = StdHost::new().with_environment_type("SERVER");
        assert_eq!(
            server_host
                .clone()
                .invoke_instance(&environment, &loader, &[])
                .unwrap(),
            Value::Ref(Some("SERVER".into()))
        );
        let development = MethodRef {
            owner: "net/fabricmc/loader/api/FabricLoader".into(),
            name: "isDevelopmentEnvironment".into(),
            descriptor: "()Z".into(),
        };
        assert_eq!(
            host.invoke_instance(&development, &loader, &[]).unwrap(),
            Value::Int(0)
        );
        assert_eq!(
            StdHost::new()
                .with_development_environment(true)
                .invoke_instance(&development, &loader, &[])
                .unwrap(),
            Value::Int(1)
        );
        let loaded = MethodRef {
            owner: "net/fabricmc/loader/api/FabricLoader".into(),
            name: "isModLoaded".into(),
            descriptor: "(Ljava/lang/String;)Z".into(),
        };
        assert_eq!(
            host.invoke_instance(&loaded, &loader, &[Value::Ref(Some("demo".into()))])
                .unwrap(),
            Value::Int(1)
        );
        let container = MethodRef {
            owner: "net/fabricmc/loader/api/FabricLoader".into(),
            name: "getModContainer".into(),
            descriptor: "(Ljava/lang/String;)Ljava/util/Optional;".into(),
        };
        let container_value = host
            .invoke_instance(&container, &loader, &[Value::Ref(Some("demo".into()))])
            .unwrap();
        assert_eq!(
            container_value,
            Value::Ref(Some("mod-container/demo/1.2.3".into()))
        );
        let all_mods = MethodRef {
            owner: "net/fabricmc/loader/api/FabricLoader".into(),
            name: "getAllMods".into(),
            descriptor: "()Ljava/util/List;".into(),
        };
        let all_mods_value = host.invoke_instance(&all_mods, &loader, &[]).unwrap();
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "java/util/List".into(),
                    name: "size".into(),
                    descriptor: "()I".into(),
                },
                &all_mods_value,
                &[],
            )
            .unwrap(),
            Value::Int(1)
        );
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "java/util/List".into(),
                    name: "get".into(),
                    descriptor: "(I)Ljava/lang/Object;".into(),
                },
                &all_mods_value,
                &[Value::Int(0)],
            )
            .unwrap(),
            container_value
        );
        let present = MethodRef {
            owner: "java/util/Optional".into(),
            name: "isPresent".into(),
            descriptor: "()Z".into(),
        };
        assert_eq!(
            host.invoke_instance(&present, &container_value, &[])
                .unwrap(),
            Value::Int(1)
        );
        let fallback = MethodRef {
            owner: "java/util/Optional".into(),
            name: "orElse".into(),
            descriptor: "(Ljava/lang/Object;)Ljava/lang/Object;".into(),
        };
        assert_eq!(
            host.invoke_instance(
                &fallback,
                &Value::Ref(None),
                &[Value::Ref(Some("fallback".into()))],
            )
            .unwrap(),
            Value::Ref(Some("fallback".into()))
        );
        let metadata = MethodRef {
            owner: "net/fabricmc/loader/api/ModContainer".into(),
            name: "getMetadata".into(),
            descriptor: "()Lnet/fabricmc/loader/api/metadata/ModMetadata;".into(),
        };
        assert_eq!(
            host.invoke_instance(&metadata, &container_value, &[])
                .unwrap(),
            Value::Ref(Some("metadata/demo/1.2.3".into()))
        );
        let root_paths = MethodRef {
            owner: "net/fabricmc/loader/api/ModContainer".into(),
            name: "getRootPaths".into(),
            descriptor: "()Ljava/util/Collection;".into(),
        };
        let roots = host
            .invoke_instance(&root_paths, &container_value, &[])
            .unwrap();
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "java/util/Collection".into(),
                    name: "size".into(),
                    descriptor: "()I".into(),
                },
                &roots,
                &[],
            )
            .unwrap(),
            Value::Int(1)
        );
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "java/util/Collection".into(),
                    name: "contains".into(),
                    descriptor: "(Ljava/lang/Object;)Z".into(),
                },
                &roots,
                &[Value::Ref(Some("path:/horizon/mods/demo".into()))],
            )
            .unwrap(),
            Value::Int(1)
        );
        let name = MethodRef {
            owner: "net/fabricmc/loader/api/metadata/ModMetadata".into(),
            name: "getName".into(),
            descriptor: "()Ljava/lang/String;".into(),
        };
        assert_eq!(
            host.invoke_instance(&name, &Value::Ref(Some("metadata/demo/1.2.3".into())), &[])
                .unwrap(),
            Value::Ref(Some("demo".into()))
        );
        let metadata_type = MethodRef {
            owner: "net/fabricmc/loader/api/metadata/ModMetadata".into(),
            name: "getType".into(),
            descriptor: "()Ljava/lang/String;".into(),
        };
        assert_eq!(
            host.invoke_instance(
                &metadata_type,
                &Value::Ref(Some("metadata/demo/1.2.3".into())),
                &[],
            )
            .unwrap(),
            Value::Ref(Some("release".into()))
        );
        let provides = MethodRef {
            owner: "net/fabricmc/loader/api/metadata/ModMetadata".into(),
            name: "getProvides".into(),
            descriptor: "()Ljava/util/Collection;".into(),
        };
        let provides_value = host
            .invoke_instance(
                &provides,
                &Value::Ref(Some("metadata/demo/1.2.3".into())),
                &[],
            )
            .unwrap();
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "java/util/Collection".into(),
                    name: "size".into(),
                    descriptor: "()I".into(),
                },
                &provides_value,
                &[],
            )
            .unwrap(),
            Value::Int(0)
        );
        for method_name in ["getAuthors", "getLicense"] {
            let values = host
                .invoke_instance(
                    &MethodRef {
                        owner: "net/fabricmc/loader/api/metadata/ModMetadata".into(),
                        name: method_name.into(),
                        descriptor: "()Ljava/util/Collection;".into(),
                    },
                    &Value::Ref(Some("metadata/demo/1.2.3".into())),
                    &[],
                )
                .unwrap();
            assert_eq!(
                host.invoke_instance(
                    &MethodRef {
                        owner: "java/util/Collection".into(),
                        name: "size".into(),
                        descriptor: "()I".into(),
                    },
                    &values,
                    &[],
                )
                .unwrap(),
                Value::Int(0),
                "{method_name} should be a bounded empty collection"
            );
        }
        let icon = host
            .invoke_instance(
                &MethodRef {
                    owner: "net/fabricmc/loader/api/metadata/ModMetadata".into(),
                    name: "getIconPath".into(),
                    descriptor: "(I)Ljava/util/Optional;".into(),
                },
                &Value::Ref(Some("metadata/demo/1.2.3".into())),
                &[Value::Int(64)],
            )
            .unwrap();
        assert_eq!(icon, Value::Ref(None));
        let dependencies = MethodRef {
            owner: "net/fabricmc/loader/api/metadata/ModMetadata".into(),
            name: "getDependencies".into(),
            descriptor: "()Ljava/util/Map;".into(),
        };
        let dependencies_value = host
            .invoke_instance(
                &dependencies,
                &Value::Ref(Some("metadata/demo/1.2.3".into())),
                &[],
            )
            .unwrap();
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "java/util/Map".into(),
                    name: "size".into(),
                    descriptor: "()I".into(),
                },
                &dependencies_value,
                &[],
            )
            .unwrap(),
            Value::Int(0)
        );
        let version = MethodRef {
            owner: "net/fabricmc/loader/api/metadata/ModMetadata".into(),
            name: "getVersion".into(),
            descriptor: "()Lnet/fabricmc/loader/api/Version;".into(),
        };
        let version_value = host
            .invoke_instance(
                &version,
                &Value::Ref(Some("metadata/demo/1.2.3".into())),
                &[],
            )
            .unwrap();
        let friendly = MethodRef {
            owner: "net/fabricmc/loader/api/Version".into(),
            name: "getFriendlyString".into(),
            descriptor: "()Ljava/lang/String;".into(),
        };
        assert_eq!(
            host.invoke_instance(&friendly, &version_value, &[])
                .unwrap(),
            Value::Ref(Some("1.2.3".into()))
        );
        let game_dir = MethodRef {
            owner: "net/fabricmc/loader/api/FabricLoader".into(),
            name: "getGameDir".into(),
            descriptor: "()Ljava/nio/file/Path;".into(),
        };
        assert_eq!(
            host.invoke_instance(&game_dir, &loader, &[]).unwrap(),
            Value::Ref(Some("path:/horizon/game".into()))
        );
        let resolve = MethodRef {
            owner: "java/nio/file/Path".into(),
            name: "resolve".into(),
            descriptor: "(Ljava/lang/String;)Ljava/nio/file/Path;".into(),
        };
        let config = host
            .invoke_instance(
                &resolve,
                &Value::Ref(Some("path:/horizon/game".into())),
                &[Value::Ref(Some("config".into()))],
            )
            .unwrap();
        assert_eq!(config, Value::Ref(Some("path:/horizon/game/config".into())));
        let to_string = MethodRef {
            owner: "java/nio/file/Path".into(),
            name: "toString".into(),
            descriptor: "()Ljava/lang/String;".into(),
        };
        assert_eq!(
            host.invoke_instance(&to_string, &config, &[]).unwrap(),
            Value::Ref(Some("/horizon/game/config".into()))
        );
    }

    #[test]
    fn standard_host_maps_fabric_environment_enum_values() {
        let mut host = StdHost::new();
        let client = MethodRef {
            owner: "net/fabricmc/api/EnvType".into(),
            name: "CLIENT".into(),
            descriptor: "Lnet/fabricmc/api/EnvType;".into(),
        };
        let client = host.get_static(&client).unwrap();
        assert_eq!(client, Value::Ref(Some("CLIENT".into())));
        let is_client = MethodRef {
            owner: "net/fabricmc/api/EnvType".into(),
            name: "isClient".into(),
            descriptor: "()Z".into(),
        };
        assert_eq!(
            host.invoke_instance(&is_client, &client, &[]).unwrap(),
            Value::Int(1)
        );
        let name = MethodRef {
            owner: "java/lang/Enum".into(),
            name: "name".into(),
            descriptor: "()Ljava/lang/String;".into(),
        };
        assert_eq!(
            host.invoke_instance(&name, &client, &[]).unwrap(),
            Value::Ref(Some("CLIENT".into()))
        );
        let ordinal = MethodRef {
            owner: "java/lang/Enum".into(),
            name: "ordinal".into(),
            descriptor: "()I".into(),
        };
        assert_eq!(
            host.invoke_instance(&ordinal, &Value::Ref(Some("SERVER".into())), &[])
                .unwrap(),
            Value::Int(1)
        );
    }

    #[test]
    fn standard_host_maps_neoforge_mod_list_queries() {
        let mut host = StdHost::new().with_mod("demo", "1.2.3");
        let get = MethodRef {
            owner: "net/neoforged/fml/ModList".into(),
            name: "get".into(),
            descriptor: "()Lnet/neoforged/fml/ModList;".into(),
        };
        let list = host.get_static(&get).unwrap();
        assert_eq!(list, Value::Ref(Some("neoforge-mod-list".into())));
        let loaded = MethodRef {
            owner: "net/neoforged/fml/ModList".into(),
            name: "isLoaded".into(),
            descriptor: "(Ljava/lang/String;)Z".into(),
        };
        assert_eq!(
            host.invoke_instance(&loaded, &list, &[Value::Ref(Some("demo".into()))])
                .unwrap(),
            Value::Int(1)
        );
        assert_eq!(
            host.invoke_instance(&loaded, &list, &[Value::Ref(Some("missing".into()))])
                .unwrap(),
            Value::Int(0)
        );
        let container = MethodRef {
            owner: "net/neoforged/fml/ModList".into(),
            name: "getModContainerById".into(),
            descriptor: "(Ljava/lang/String;)Ljava/util/Optional;".into(),
        };
        assert_eq!(
            host.invoke_instance(&container, &list, &[Value::Ref(Some("demo".into()))])
                .unwrap(),
            Value::Ref(Some("neoforge-mod-container/demo/1.2.3".into()))
        );
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "net/neoforged/fml/ModContainer".into(),
                    name: "getModId".into(),
                    descriptor: "()Ljava/lang/String;".into(),
                },
                &Value::Ref(Some("neoforge-mod-container/demo/1.2.3".into())),
                &[],
            )
            .unwrap(),
            Value::Ref(Some("demo".into()))
        );
        let mods = MethodRef {
            owner: "net/neoforged/fml/ModList".into(),
            name: "getMods".into(),
            descriptor: "()Ljava/util/List;".into(),
        };
        let mods_value = host.invoke_instance(&mods, &list, &[]).unwrap();
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "java/util/List".into(),
                    name: "size".into(),
                    descriptor: "()I".into(),
                },
                &mods_value,
                &[],
            )
            .unwrap(),
            Value::Int(1)
        );
    }

    #[test]
    fn standard_host_maps_legacy_forge_loader_aliases() {
        let mut host = StdHost::new().with_mod("legacy", "2.0");
        let list = host
            .get_static(&MethodRef {
                owner: "net/minecraftforge/fml/ModList".into(),
                name: "get".into(),
                descriptor: "()Lnet/minecraftforge/fml/ModList;".into(),
            })
            .unwrap();
        assert_eq!(list, Value::Ref(Some("neoforge-mod-list".into())));
        let container = host
            .invoke_instance(
                &MethodRef {
                    owner: "net/minecraftforge/fml/ModList".into(),
                    name: "getModContainerById".into(),
                    descriptor: "(Ljava/lang/String;)Ljava/util/Optional;".into(),
                },
                &list,
                &[Value::Ref(Some("legacy".into()))],
            )
            .unwrap();
        assert_eq!(
            container,
            Value::Ref(Some("neoforge-mod-container/legacy/2.0".into()))
        );
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "net/minecraftforge/fml/ModContainer".into(),
                    name: "getModId".into(),
                    descriptor: "()Ljava/lang/String;".into(),
                },
                &container,
                &[],
            )
            .unwrap(),
            Value::Ref(Some("legacy".into()))
        );
        let context = host
            .get_static(&MethodRef {
                owner: "net/minecraftforge/fml/javafmlmod/FMLJavaModLoadingContext".into(),
                name: "get".into(),
                descriptor: "()Lnet/minecraftforge/fml/javafmlmod/FMLJavaModLoadingContext;".into(),
            })
            .unwrap();
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "net/minecraftforge/fml/javafmlmod/FMLJavaModLoadingContext".into(),
                    name: "getModEventBus".into(),
                    descriptor: "()Lnet/minecraftforge/eventbus/api/IEventBus;".into(),
                },
                &context,
                &[],
            )
            .unwrap(),
            Value::Ref(Some("net/minecraftforge/eventbus/api/IEventBus".into()))
        );
    }

    #[test]
    fn standard_host_maps_identifier_and_resource_key_values() {
        let mut host = StdHost::new();
        let parse = MethodRef {
            owner: "net/minecraft/resources/Identifier".into(),
            name: "parse".into(),
            descriptor: "(Ljava/lang/String;)Lnet/minecraft/resources/Identifier;".into(),
        };
        let id = host
            .invoke(&parse, &[Value::Ref(Some("ruby:lamp".into()))])
            .unwrap();
        assert_eq!(id, Value::Ref(Some("identifier/ruby:lamp".into())));
        let modern_parse = MethodRef {
            owner: "net/minecraft/resources/ResourceLocation".into(),
            name: "parse".into(),
            descriptor: "(Ljava/lang/String;)Lnet/minecraft/resources/ResourceLocation;".into(),
        };
        let modern_id = host
            .invoke(&modern_parse, &[Value::Ref(Some("ruby:lamp".into()))])
            .unwrap();
        assert_eq!(modern_id, id);
        let modern_string = MethodRef {
            owner: "net/minecraft/resources/ResourceLocation".into(),
            name: "toString".into(),
            descriptor: "()Ljava/lang/String;".into(),
        };
        assert_eq!(
            host.invoke_instance(&modern_string, &modern_id, &[])
                .unwrap(),
            Value::Ref(Some("ruby:lamp".into()))
        );
        let path = MethodRef {
            owner: "net/minecraft/resources/Identifier".into(),
            name: "getPath".into(),
            descriptor: "()Ljava/lang/String;".into(),
        };
        assert_eq!(
            host.invoke_instance(&path, &id, &[]).unwrap(),
            Value::Ref(Some("lamp".into()))
        );
        let short = MethodRef {
            owner: "net/minecraft/resources/Identifier".into(),
            name: "toShortString".into(),
            descriptor: "()Ljava/lang/String;".into(),
        };
        assert_eq!(
            host.invoke_instance(&short, &id, &[]).unwrap(),
            Value::Ref(Some("ruby/lamp".into()))
        );
        let prefixed = MethodRef {
            owner: "net/minecraft/resources/Identifier".into(),
            name: "withPrefix".into(),
            descriptor: "(Ljava/lang/String;)Lnet/minecraft/resources/Identifier;".into(),
        };
        assert_eq!(
            host.invoke_instance(&prefixed, &id, &[Value::Ref(Some("block/".into()))],)
                .unwrap(),
            Value::Ref(Some("identifier/ruby:block/lamp".into()))
        );
        let language_key = MethodRef {
            owner: "net/minecraft/resources/Identifier".into(),
            name: "toLanguageKey".into(),
            descriptor: "(Ljava/lang/String;)Ljava/lang/String;".into(),
        };
        assert_eq!(
            host.invoke_instance(&language_key, &id, &[Value::Ref(Some("item".into()))],)
                .unwrap(),
            Value::Ref(Some("item.ruby.lamp".into()))
        );
        let with_path = MethodRef {
            owner: "net/minecraft/resources/Identifier".into(),
            name: "withPath".into(),
            descriptor: "(Ljava/util/function/UnaryOperator;)Lnet/minecraft/resources/Identifier;"
                .into(),
        };
        let transformed = host
            .invoke_instance(
                &with_path,
                &id,
                &[Value::Ref(Some("@indy:path-transform".into()))],
            )
            .unwrap();
        assert_eq!(transformed, Value::Ref(Some("identifier/ruby:lamp".into())));
        let key_create = MethodRef {
            owner: "net/minecraft/resources/ResourceKey".into(),
            name: "create".into(),
            descriptor: "(Lnet/minecraft/resources/ResourceKey;Lnet/minecraft/resources/Identifier;)Lnet/minecraft/resources/ResourceKey;".into(),
        };
        let key = host
            .invoke(
                &key_create,
                &[Value::Ref(Some("registry".into())), id.clone()],
            )
            .unwrap();
        let key_id = MethodRef {
            owner: "net/minecraft/resources/ResourceKey".into(),
            name: "identifier".into(),
            descriptor: "()Lnet/minecraft/resources/Identifier;".into(),
        };
        assert!(matches!(
            host.invoke_instance(&key_id, &key, &[]).unwrap(),
            Value::Ref(Some(_))
        ));
        let key_id_modern = MethodRef {
            owner: "net/minecraft/resources/ResourceKey".into(),
            name: "identifier".into(),
            descriptor: "()Lnet/minecraft/resources/ResourceLocation;".into(),
        };
        assert_eq!(
            host.invoke_instance(&key_id_modern, &key, &[]).unwrap(),
            host.invoke_instance(&key_id, &key, &[]).unwrap()
        );
        let key_location = MethodRef {
            owner: "net/minecraft/resources/ResourceKey".into(),
            name: "location".into(),
            descriptor: "()Lnet/minecraft/resources/Identifier;".into(),
        };
        assert_eq!(
            host.invoke_instance(&key_location, &key, &[]).unwrap(),
            host.invoke_instance(&key_id, &key, &[]).unwrap()
        );
        let tag_create = MethodRef {
            owner: "net/minecraft/tags/TagKey".into(),
            name: "create".into(),
            descriptor: "(Lnet/minecraft/resources/ResourceKey;Lnet/minecraft/resources/Identifier;)Lnet/minecraft/tags/TagKey;".into(),
        };
        let tag = host
            .invoke(&tag_create, &[key.clone(), id.clone()])
            .unwrap();
        let tag_location = MethodRef {
            owner: "net/minecraft/tags/TagKey".into(),
            name: "location".into(),
            descriptor: "()Lnet/minecraft/resources/Identifier;".into(),
        };
        assert_eq!(host.invoke_instance(&tag_location, &tag, &[]).unwrap(), id);
        let tag_registry = MethodRef {
            owner: "net/minecraft/tags/TagKey".into(),
            name: "registry".into(),
            descriptor: "()Lnet/minecraft/resources/ResourceKey;".into(),
        };
        assert_eq!(host.invoke_instance(&tag_registry, &tag, &[]).unwrap(), key);
        let tag_is_for = MethodRef {
            owner: "net/minecraft/tags/TagKey".into(),
            name: "isFor".into(),
            descriptor: "(Lnet/minecraft/resources/ResourceKey;)Z".into(),
        };
        assert_eq!(
            host.invoke_instance(&tag_is_for, &tag, std::slice::from_ref(&key))
                .unwrap(),
            Value::Int(1)
        );
    }

    #[test]
    fn standard_host_keeps_neoforge_registration_chain_alive() {
        let mut host = StdHost::new();
        let context_get = MethodRef {
            owner: "net/neoforged/fml/javafmlmod/FMLJavaModLoadingContext".into(),
            name: "get".into(),
            descriptor: "()Lnet/neoforged/fml/javafmlmod/FMLJavaModLoadingContext;".into(),
        };
        let context = host.invoke(&context_get, &[]).unwrap();
        let bus_get = MethodRef {
            owner: "net/neoforged/fml/javafmlmod/FMLJavaModLoadingContext".into(),
            name: "getModEventBus".into(),
            descriptor: "()Lnet/neoforged/bus/api/IEventBus;".into(),
        };
        let bus = host.invoke_instance(&bus_get, &context, &[]).unwrap();
        let add_listener = MethodRef {
            owner: "net/neoforged/bus/api/IEventBus".into(),
            name: "addListener".into(),
            descriptor: "(Ljava/util/function/Consumer;)Lnet/neoforged/bus/api/IEventBus;".into(),
        };
        assert_eq!(
            host.invoke_instance(
                &add_listener,
                &bus,
                &[Value::Ref(Some("@indy:listener".into()))],
            )
            .unwrap(),
            bus
        );
        let deferred = MethodRef {
            owner: "net/neoforged/neoforge/registries/DeferredRegister".into(),
            name: "create".into(),
            descriptor: "(Ljava/lang/Object;Ljava/lang/String;)Lnet/neoforged/neoforge/registries/DeferredRegister;".into(),
        };
        let register = host
            .invoke(
                &deferred,
                &[
                    Value::Ref(Some("registry".into())),
                    Value::Ref(Some("demo".into())),
                ],
            )
            .unwrap();
        let holder = MethodRef {
            owner: "net/neoforged/neoforge/registries/DeferredRegister".into(),
            name: "register".into(),
            descriptor: "(Ljava/lang/String;Ljava/util/function/Supplier;)Lnet/neoforged/neoforge/registries/DeferredHolder;".into(),
        };
        assert_eq!(
            host.invoke_instance(
                &holder,
                &register,
                &[
                    Value::Ref(Some("entry".into())),
                    Value::Ref(Some("@indy:supplier".into())),
                ],
            )
            .unwrap(),
            Value::Ref(Some("holder/deferred-register/registry/demo/entry".into()))
        );
        let entries = host
            .invoke_instance(
                &MethodRef {
                    owner: "net/neoforged/neoforge/registries/DeferredRegister".into(),
                    name: "getEntries".into(),
                    descriptor: "()Ljava/util/Collection;".into(),
                },
                &register,
                &[],
            )
            .unwrap();
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "java/util/Collection".into(),
                    name: "size".into(),
                    descriptor: "()I".into(),
                },
                &entries,
                &[],
            )
            .unwrap(),
            Value::Int(1)
        );
        let holder_get = MethodRef {
            owner: "net/neoforged/neoforge/registries/DeferredHolder".into(),
            name: "get".into(),
            descriptor: "()Ljava/lang/Object;".into(),
        };
        assert_eq!(
            host.invoke_instance(
                &holder_get,
                &Value::Ref(Some("holder/deferred-register/registry/demo/entry".into())),
                &[],
            )
            .unwrap(),
            Value::Ref(Some(
                "registered/deferred-register/registry/demo/entry".into(),
            ))
        );
        for name in ["value", "getOptional", "getKey", "getId"] {
            let descriptor = match name {
                "value" => "()Ljava/lang/Object;",
                "getOptional" => "()Ljava/util/Optional;",
                "getKey" => "()Lnet/minecraft/resources/ResourceKey;",
                "getId" => "()Lnet/minecraft/resources/Identifier;",
                _ => unreachable!(),
            };
            let method = MethodRef {
                owner: "net/neoforged/neoforge/registries/DeferredHolder".into(),
                name: name.into(),
                descriptor: descriptor.into(),
            };
            let value = host
                .invoke_instance(
                    &method,
                    &Value::Ref(Some("holder/deferred-register/registry/demo/entry".into())),
                    &[],
                )
                .unwrap();
            assert!(
                matches!(value, Value::Ref(Some(_))),
                "{name} returned {value:?}"
            );
        }
        let holder_value = host
            .invoke_instance(
                &MethodRef {
                    owner: "net/minecraft/core/Holder".into(),
                    name: "value".into(),
                    descriptor: "()Ljava/lang/Object;".into(),
                },
                &Value::Ref(Some("holder/deferred-register/registry/demo/entry".into())),
                &[],
            )
            .unwrap();
        assert_eq!(
            holder_value,
            Value::Ref(Some(
                "registered/deferred-register/registry/demo/entry".into()
            ))
        );
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "net/minecraft/core/Holder".into(),
                    name: "isBound".into(),
                    descriptor: "()Z".into(),
                },
                &Value::Ref(Some("holder/deferred-register/registry/demo/entry".into())),
                &[],
            )
            .unwrap(),
            Value::Int(1)
        );
        let holder_key = host
            .invoke_instance(
                &MethodRef {
                    owner: "net/minecraft/core/Holder$Reference".into(),
                    name: "unwrapKey".into(),
                    descriptor: "()Ljava/util/Optional;".into(),
                },
                &Value::Ref(Some("holder/deferred-register/registry/demo/entry".into())),
                &[],
            )
            .unwrap();
        assert_eq!(
            holder_key,
            Value::Ref(Some(
                "optional/resource-key/deferred-register/registry/demo/entry".into()
            ))
        );
        let modern_id = host
            .invoke_instance(
                &MethodRef {
                    owner: "net/neoforged/neoforge/registries/DeferredHolder".into(),
                    name: "getId".into(),
                    descriptor: "()Lnet/minecraft/resources/ResourceLocation;".into(),
                },
                &Value::Ref(Some("holder/deferred-register/registry/demo/entry".into())),
                &[],
            )
            .unwrap();
        assert_eq!(
            modern_id,
            Value::Ref(Some(
                "identifier/holder/deferred-register/registry/demo/entry".into()
            ))
        );
        let optional = host
            .invoke_instance(
                &MethodRef {
                    owner: "net/neoforged/neoforge/registries/DeferredHolder".into(),
                    name: "getOptional".into(),
                    descriptor: "()Ljava/util/Optional;".into(),
                },
                &Value::Ref(Some("holder/deferred-register/registry/demo/entry".into())),
                &[],
            )
            .unwrap();
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "java/util/Optional".into(),
                    name: "get".into(),
                    descriptor: "()Ljava/lang/Object;".into(),
                },
                &optional,
                &[],
            )
            .unwrap(),
            Value::Ref(Some(
                "registered/deferred-register/registry/demo/entry".into(),
            ))
        );
        assert_eq!(host.registrations().len(), 1);
        assert_eq!(host.invocations().len(), 18);
        assert_eq!(
            host.invocations()[2].args[0],
            Value::Ref(Some("@indy:listener".into()))
        );
        let dispatches = host.dispatch_event(Value::Ref(Some("RegisterEvent".into())));
        assert_eq!(dispatches.len(), 1);
        assert_eq!(
            dispatches[0].listener.args.last(),
            Some(&Value::Ref(Some("@indy:listener".into())))
        );
        assert_eq!(
            dispatches[0].event,
            Value::Ref(Some("RegisterEvent".into()))
        );
        #[derive(Default)]
        struct RecordingSink(Vec<String>);
        impl RegistrySink for RecordingSink {
            fn register(&mut self, entry: &RegistryEntry) -> Result<(), String> {
                self.0.push(entry.key());
                Ok(())
            }
        }
        let mut sink = RecordingSink::default();
        assert_eq!(host.apply_registrations(&mut sink).unwrap(), 1);
        assert_eq!(
            sink.0,
            vec!["deferred-register/registry/demo/entry".to_owned()]
        );

        let typed = RegistryEntry::typed(
            "minecraft:block",
            "demo:entry",
            RegistryDeclaration::Block {
                template: "minecraft:stone".into(),
                properties: BTreeMap::from([(String::from("hardness"), String::from("2.0"))]),
                item: true,
            },
        );
        assert_eq!(typed.key(), "minecraft:block/demo:entry");
        assert!(typed.value == Value::Ref(Some("typed/minecraft:block/demo:entry".into())));
        let (registry, id) = {
            let entry = &host.registrations()[0];
            (entry.registry.clone(), entry.id.clone())
        };
        host.attach_registration_declaration(
            &registry,
            &id,
            typed.declaration.clone().expect("typed declaration"),
        )
        .unwrap();
        #[derive(Default)]
        struct TypedSink(Vec<String>);
        impl TypedRegistrySink for TypedSink {
            fn register_typed(&mut self, entry: &RegistryEntry) -> Result<(), String> {
                self.0.push(entry.key());
                Ok(())
            }
        }
        let mut typed_sink = TypedSink::default();
        assert_eq!(host.apply_typed_registrations(&mut typed_sink).unwrap(), 1);
        assert_eq!(typed_sink.0, vec!["deferred-register/registry/demo/entry"]);
    }

    #[test]
    fn standard_host_aliases_legacy_forge_registry_objects() {
        let mut host = StdHost::new();
        let register = host
            .invoke(
                &MethodRef {
                    owner: "net/minecraftforge/registries/DeferredRegister".into(),
                    name: "create".into(),
                    descriptor: "(Ljava/lang/Object;Ljava/lang/String;)Lnet/minecraftforge/registries/DeferredRegister;".into(),
                },
                &[Value::Ref(Some("registry".into())), Value::Ref(Some("legacy".into()))],
            )
            .unwrap();
        let object = host
            .invoke_instance(
                &MethodRef {
                    owner: "net/minecraftforge/registries/DeferredRegister".into(),
                    name: "register".into(),
                    descriptor: "(Ljava/lang/String;Ljava/util/function/Supplier;)Lnet/minecraftforge/registries/RegistryObject;".into(),
                },
                &register,
                &[Value::Ref(Some("entry".into())), Value::Ref(Some("@indy:supplier".into()))],
            )
            .unwrap();
        assert_eq!(
            object,
            Value::Ref(Some(
                "holder/deferred-register/registry/legacy/entry".into()
            ))
        );
        let value = host
            .invoke_instance(
                &MethodRef {
                    owner: "net/minecraftforge/registries/RegistryObject".into(),
                    name: "get".into(),
                    descriptor: "()Ljava/lang/Object;".into(),
                },
                &object,
                &[],
            )
            .unwrap();
        assert_eq!(
            value,
            Value::Ref(Some(
                "registered/deferred-register/registry/legacy/entry".into()
            ))
        );
        let name = host
            .invoke_instance(
                &MethodRef {
                    owner: "net/minecraftforge/registries/IForgeRegistryEntry".into(),
                    name: "getRegistryName".into(),
                    descriptor: "()Lnet/minecraft/resources/ResourceLocation;".into(),
                },
                &value,
                &[],
            )
            .unwrap();
        assert!(matches!(name, Value::Ref(Some(marker)) if marker.starts_with("identifier/")));
        assert_eq!(host.registrations().len(), 1);
    }

    #[test]
    fn standard_host_records_neoforge_register_event_supplier() {
        let mut host = StdHost::new();
        let event = Value::Ref(Some("register-event".into()));
        let register = MethodRef {
            owner: "net/neoforged/neoforge/registries/RegisterEvent".into(),
            name: "register".into(),
            descriptor: "(Lnet/minecraft/resources/ResourceKey;Lnet/minecraft/resources/Identifier;Ljava/util/function/Supplier;)V".into(),
        };
        host.invoke_instance(
            &register,
            &event,
            &[
                Value::Ref(Some("resource-key/minecraft:item".into())),
                Value::Ref(Some("identifier/ruby:wand".into())),
                Value::Ref(Some("@indy:ruby-wand-supplier".into())),
            ],
        )
        .unwrap();
        assert_eq!(host.registrations().len(), 1);
        let entry = &host.registrations()[0];
        assert_eq!(entry.key(), "minecraft:item/ruby:wand");
        assert_eq!(
            entry.value,
            Value::Ref(Some("@indy:ruby-wand-supplier".into()))
        );
    }

    #[test]
    fn invokedynamic_preserves_captures_as_a_host_marker() {
        let class = ClassFile {
            name: "demo/Lambda".into(),
            super_name: "java/lang/Object".into(),
            pool: vec![
                Cp::Empty,
                Cp::InvokeDynamic {
                    bootstrap: 0,
                    nt: 2,
                },
                Cp::NameType {
                    name: 3,
                    descriptor: 4,
                },
                Cp::Utf8("accept".into()),
                Cp::Utf8("(Ljava/lang/Object;)Ljava/lang/Object;".into()),
                Cp::MethodHandle { reference_index: 6 },
                Cp::MethodRef { class: 7, nt: 8 },
                Cp::Class(9),
                Cp::NameType {
                    name: 10,
                    descriptor: 11,
                },
                Cp::Utf8("demo/Target".into()),
                Cp::Utf8("callback".into()),
                Cp::Utf8("(Ljava/lang/Object;)V".into()),
            ],
            bootstrap_methods: vec![BootstrapMethod {
                method_ref: 5,
                arguments: Vec::new(),
            }],
            fields: Vec::new(),
            methods: vec![Method {
                name: "make".into(),
                descriptor: "(Ljava/lang/Object;)Ljava/lang/Object;".into(),
                access: 9,
                code: Some(vec![0x2a, 0xba, 0, 1, 0, 0, 0xb0]),
                max_locals: 1,
            }],
        };
        let jar = NativeJar::from_classes([class]);
        let value = jar
            .invoke_method_with_args(
                "demo.Lambda",
                "make",
                "(Ljava/lang/Object;)Ljava/lang/Object;",
                None,
                &[Value::Ref(Some("capture".into()))],
                &mut NullHost,
            )
            .unwrap();
        assert_eq!(
            value,
            Value::Ref(Some(
                "@indy:demo/Lambda#make:1:demo/Target#callback(Ljava/lang/Object;)V:rcapture"
                    .into(),
            ))
        );
    }

    #[test]
    fn invokedynamic_resolves_lambda_metafactory_implementation_handle() {
        let class = ClassFile {
            name: "demo/Lambda".into(),
            super_name: "java/lang/Object".into(),
            pool: vec![
                Cp::Empty,
                Cp::InvokeDynamic {
                    bootstrap: 0,
                    nt: 2,
                },
                Cp::NameType {
                    name: 3,
                    descriptor: 4,
                },
                Cp::Utf8("apply".into()),
                Cp::Utf8("(Ljava/lang/Object;)Ljava/lang/Object;".into()),
                Cp::MethodHandle { reference_index: 6 },
                Cp::MethodRef { class: 7, nt: 8 },
                Cp::Class(9),
                Cp::NameType {
                    name: 10,
                    descriptor: 11,
                },
                Cp::Utf8("java/lang/invoke/LambdaMetafactory".into()),
                Cp::Utf8("metafactory".into()),
                Cp::Utf8("(Ljava/lang/invoke/MethodHandles$Lookup;Ljava/lang/String;Ljava/lang/invoke/MethodType;Ljava/lang/invoke/MethodType;Ljava/lang/invoke/MethodHandle;Ljava/lang/invoke/MethodType;)Ljava/lang/invoke/CallSite;".into()),
                Cp::MethodHandle { reference_index: 13 },
                Cp::MethodRef { class: 14, nt: 15 },
                Cp::Class(16),
                Cp::NameType {
                    name: 17,
                    descriptor: 18,
                },
                Cp::Utf8("demo/Target".into()),
                Cp::Utf8("callback".into()),
                Cp::Utf8("(Ljava/lang/Object;)Ljava/lang/Object;".into()),
            ],
            bootstrap_methods: vec![BootstrapMethod {
                method_ref: 5,
                arguments: vec![0, 12, 0],
            }],
            fields: Vec::new(),
            methods: Vec::new(),
        };
        assert_eq!(
            class.invoke_dynamic_target(1).unwrap(),
            Some(MethodRef {
                owner: "demo/Target".into(),
                name: "callback".into(),
                descriptor: "(Ljava/lang/Object;)Ljava/lang/Object;".into(),
            })
        );
    }

    #[test]
    fn instance_fields_round_trip_for_bounded_objects() {
        let class = ClassFile {
            name: "demo/Fields".into(),
            super_name: "java/lang/Object".into(),
            pool: vec![
                Cp::Empty,
                Cp::FieldRef { class: 2, nt: 3 },
                Cp::Class(4),
                Cp::NameType {
                    name: 5,
                    descriptor: 6,
                },
                Cp::Utf8("demo/Fields".into()),
                Cp::Utf8("value".into()),
                Cp::Utf8("I".into()),
            ],
            bootstrap_methods: Vec::new(),
            fields: Vec::new(),
            methods: vec![Method {
                name: "roundTrip".into(),
                descriptor: "(Ljava/lang/Object;)I".into(),
                access: 9,
                code: Some(vec![0x2a, 0x08, 0xb5, 0, 1, 0x2a, 0xb4, 0, 1, 0xac]),
                max_locals: 1,
            }],
        };
        let jar = NativeJar::from_classes([class]);
        assert_eq!(
            jar.invoke_method_with_args(
                "demo.Fields",
                "roundTrip",
                "(Ljava/lang/Object;)I",
                None,
                &[Value::Ref(Some("object/1".into()))],
                &mut NullHost,
            )
            .unwrap(),
            Value::Int(5)
        );
    }

    #[test]
    fn jar_owned_new_allocations_have_distinct_instance_identity() {
        let classes = BTreeMap::new();
        let mut host = NullHost;
        let mut runtime = Runtime {
            classes: &classes,
            host: &mut host,
            calls: 0,
            fields: BTreeMap::new(),
            static_fields: BTreeMap::new(),
            initialized_classes: std::collections::BTreeSet::new(),
            arrays: BTreeMap::new(),
            next_array: 0,
            next_object: 0,
            streams: BTreeMap::new(),
            next_stream: 0,
        };
        let first = runtime.new_object("demo/Fields");
        let second = runtime.new_object("demo/Fields");
        assert_ne!(first, second);
        let field = MethodRef {
            owner: "demo/Fields".into(),
            name: "value".into(),
            descriptor: "I".into(),
        };
        runtime
            .fields
            .insert(instance_field_key(&first, &field).unwrap(), Value::Int(7));
        assert_eq!(
            runtime
                .fields
                .get(&instance_field_key(&first, &field).unwrap()),
            Some(&Value::Int(7))
        );
        assert!(
            !runtime
                .fields
                .contains_key(&instance_field_key(&second, &field).unwrap())
        );
    }

    #[test]
    fn class_initializer_populates_jar_owned_static_fields() {
        let class = ClassFile {
            name: "demo/Static".into(),
            super_name: "java/lang/Object".into(),
            pool: vec![
                Cp::Empty,
                Cp::FieldRef { class: 2, nt: 3 },
                Cp::Class(4),
                Cp::NameType {
                    name: 5,
                    descriptor: 6,
                },
                Cp::Utf8("demo/Static".into()),
                Cp::Utf8("VALUE".into()),
                Cp::Utf8("I".into()),
            ],
            bootstrap_methods: Vec::new(),
            fields: Vec::new(),
            methods: vec![
                Method {
                    name: "<clinit>".into(),
                    descriptor: "()V".into(),
                    access: 8,
                    code: Some(vec![0x08, 0xb3, 0, 1, 0xb1]),
                    max_locals: 0,
                },
                Method {
                    name: "read".into(),
                    descriptor: "()I".into(),
                    access: 9,
                    code: Some(vec![0xb2, 0, 1, 0xac]),
                    max_locals: 0,
                },
            ],
        };
        let jar = NativeJar::from_classes([class]);
        assert_eq!(
            jar.invoke_method("demo.Static", "read", "()I", None, &mut NullHost)
                .unwrap(),
            Value::Int(5)
        );
    }

    #[test]
    fn bounded_arrays_allocate_load_store_and_report_length() {
        let method = |name: &str, code: Vec<u8>| Method {
            name: name.into(),
            descriptor: "()I".into(),
            access: 9,
            code: Some(code),
            max_locals: 0,
        };
        let class = ClassFile {
            name: "demo/Arrays".into(),
            super_name: "java/lang/Object".into(),
            pool: vec![Cp::Empty],
            bootstrap_methods: Vec::new(),
            fields: Vec::new(),
            methods: vec![
                method(
                    "read",
                    vec![
                        0x05, 0xbc, 0x0a, 0x59, 0x03, 0x10, 0x07, 0x4f, 0x03, 0x2e, 0xac,
                    ],
                ),
                method("length", vec![0x06, 0xbc, 0x0a, 0xbe, 0xac]),
            ],
        };
        let jar = NativeJar::from_classes([class]);
        assert_eq!(
            jar.invoke_method("demo.Arrays", "read", "()I", None, &mut NullHost)
                .unwrap(),
            Value::Int(7)
        );
        assert_eq!(
            jar.invoke_method("demo.Arrays", "length", "()I", None, &mut NullHost)
                .unwrap(),
            Value::Int(3)
        );
    }

    #[test]
    fn reflective_array_surface_uses_runtime_owned_arrays() {
        let classes = BTreeMap::new();
        let mut host = NullHost;
        let mut runtime = Runtime {
            classes: &classes,
            host: &mut host,
            calls: 0,
            fields: BTreeMap::new(),
            static_fields: BTreeMap::new(),
            initialized_classes: std::collections::BTreeSet::new(),
            arrays: BTreeMap::new(),
            next_array: 0,
            next_object: 0,
            streams: BTreeMap::new(),
            next_stream: 0,
        };
        let new_instance = MethodRef {
            owner: "java/lang/reflect/Array".into(),
            name: "newInstance".into(),
            descriptor: "(Ljava/lang/Class;I)Ljava/lang/Object;".into(),
        };
        let array = runtime
            .invoke_reflect_array(
                &new_instance,
                &[Value::Ref(Some("java/lang/String".into())), Value::Int(2)],
                "demo/Reflective",
                "probe",
            )
            .unwrap()
            .unwrap();
        let set = MethodRef {
            owner: "java/lang/reflect/Array".into(),
            name: "set".into(),
            descriptor: "(Ljava/lang/Object;ILjava/lang/Object;)V".into(),
        };
        assert_eq!(
            runtime
                .invoke_reflect_array(
                    &set,
                    &[
                        array.clone(),
                        Value::Int(1),
                        Value::Ref(Some("beta".into()))
                    ],
                    "demo/Reflective",
                    "probe",
                )
                .unwrap(),
            Some(Value::Void)
        );
        let length = MethodRef {
            owner: "java/lang/reflect/Array".into(),
            name: "getLength".into(),
            descriptor: "(Ljava/lang/Object;)I".into(),
        };
        assert_eq!(
            runtime
                .invoke_reflect_array(
                    &length,
                    std::slice::from_ref(&array),
                    "demo/Reflective",
                    "probe",
                )
                .unwrap(),
            Some(Value::Int(2))
        );
        let get = MethodRef {
            owner: "java/lang/reflect/Array".into(),
            name: "get".into(),
            descriptor: "(Ljava/lang/Object;I)Ljava/lang/Object;".into(),
        };
        assert_eq!(
            runtime
                .invoke_reflect_array(
                    &get,
                    &[array.clone(), Value::Int(1)],
                    "demo/Reflective",
                    "probe",
                )
                .unwrap(),
            Some(Value::Ref(Some("beta".into())))
        );
        let set_boolean = MethodRef {
            owner: "java/lang/reflect/Array".into(),
            name: "setBoolean".into(),
            descriptor: "(Ljava/lang/Object;IZ)V".into(),
        };
        assert_eq!(
            runtime
                .invoke_reflect_array(
                    &set_boolean,
                    &[array.clone(), Value::Int(0), Value::Int(1),],
                    "demo/Reflective",
                    "probe",
                )
                .unwrap(),
            Some(Value::Void)
        );
        let get_boolean = MethodRef {
            owner: "java/lang/reflect/Array".into(),
            name: "getBoolean".into(),
            descriptor: "(Ljava/lang/Object;I)Z".into(),
        };
        assert_eq!(
            runtime
                .invoke_reflect_array(
                    &get_boolean,
                    &[array, Value::Int(0)],
                    "demo/Reflective",
                    "probe",
                )
                .unwrap(),
            Some(Value::Int(1))
        );
        let long_array = runtime
            .invoke_reflect_array(
                &new_instance,
                &[Value::Ref(Some("long".into())), Value::Int(1)],
                "demo/Reflective",
                "probe",
            )
            .unwrap()
            .unwrap();
        let set_long = MethodRef {
            owner: "java/lang/reflect/Array".into(),
            name: "setLong".into(),
            descriptor: "(Ljava/lang/Object;IJ)V".into(),
        };
        runtime
            .invoke_reflect_array(
                &set_long,
                &[long_array.clone(), Value::Int(0), Value::Int(42)],
                "demo/Reflective",
                "probe",
            )
            .unwrap();
        let get_long = MethodRef {
            owner: "java/lang/reflect/Array".into(),
            name: "getLong".into(),
            descriptor: "(Ljava/lang/Object;I)J".into(),
        };
        assert_eq!(
            runtime
                .invoke_reflect_array(
                    &get_long,
                    &[long_array, Value::Int(0)],
                    "demo/Reflective",
                    "probe",
                )
                .unwrap(),
            Some(Value::Int(42))
        );
    }

    #[test]
    fn stream_surface_keeps_collection_values_bounded() {
        let classes = BTreeMap::new();
        let mut host = StdHost::new();
        let list = host
            .invoke(
                &MethodRef {
                    owner: "java/util/List".into(),
                    name: "of".into(),
                    descriptor:
                        "(Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;)Ljava/util/List;"
                            .into(),
                },
                &[
                    Value::Ref(Some("alpha".into())),
                    Value::Int(2),
                    Value::Ref(Some("alpha".into())),
                ],
            )
            .unwrap();
        let optional = host
            .invoke(
                &MethodRef {
                    owner: "java/util/Optional".into(),
                    name: "of".into(),
                    descriptor: "(Ljava/lang/Object;)Ljava/util/Optional;".into(),
                },
                &[Value::Ref(Some("alpha".into()))],
            )
            .unwrap();
        let empty = host
            .invoke(
                &MethodRef {
                    owner: "java/util/Optional".into(),
                    name: "empty".into(),
                    descriptor: "()Ljava/util/Optional;".into(),
                },
                &[],
            )
            .unwrap();
        let mut runtime = Runtime {
            classes: &classes,
            host: &mut host,
            calls: 0,
            fields: BTreeMap::new(),
            static_fields: BTreeMap::new(),
            initialized_classes: std::collections::BTreeSet::new(),
            arrays: BTreeMap::new(),
            next_array: 0,
            next_object: 0,
            streams: BTreeMap::new(),
            next_stream: 0,
        };
        let stream = runtime
            .invoke_stream(
                &MethodRef {
                    owner: "java/util/List".into(),
                    name: "stream".into(),
                    descriptor: "()Ljava/util/stream/Stream;".into(),
                },
                Some(&list),
                &[],
                "demo/Stream",
                "probe",
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            runtime
                .invoke_stream(
                    &MethodRef {
                        owner: "java/util/stream/Stream".into(),
                        name: "count".into(),
                        descriptor: "()J".into(),
                    },
                    Some(&stream),
                    &[],
                    "demo/Stream",
                    "probe",
                )
                .unwrap(),
            Some(Value::Int(3))
        );
        assert_eq!(
            runtime
                .invoke_stream(
                    &MethodRef {
                        owner: "java/util/stream/Stream".into(),
                        name: "anyMatch".into(),
                        descriptor: "(Ljava/util/function/Predicate;)Z".into(),
                    },
                    Some(&stream),
                    &[Value::Ref(Some("@indy:stream-predicate".into()))],
                    "demo/Stream",
                    "probe",
                )
                .unwrap(),
            Some(Value::Int(1))
        );
        let optional_stream = runtime
            .invoke_stream(
                &MethodRef {
                    owner: "java/util/Optional".into(),
                    name: "stream".into(),
                    descriptor: "()Ljava/util/stream/Stream;".into(),
                },
                Some(&optional),
                &[],
                "demo/Stream",
                "probe",
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            runtime
                .invoke_stream(
                    &MethodRef {
                        owner: "java/util/stream/Stream".into(),
                        name: "count".into(),
                        descriptor: "()J".into(),
                    },
                    Some(&optional_stream),
                    &[],
                    "demo/Stream",
                    "probe",
                )
                .unwrap(),
            Some(Value::Int(1))
        );
        let empty_stream = runtime
            .invoke_stream(
                &MethodRef {
                    owner: "java/util/Optional".into(),
                    name: "stream".into(),
                    descriptor: "()Ljava/util/stream/Stream;".into(),
                },
                Some(&empty),
                &[],
                "demo/Stream",
                "probe",
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            runtime
                .invoke_stream(
                    &MethodRef {
                        owner: "java/util/stream/Stream".into(),
                        name: "count".into(),
                        descriptor: "()J".into(),
                    },
                    Some(&empty_stream),
                    &[],
                    "demo/Stream",
                    "probe",
                )
                .unwrap(),
            Some(Value::Int(0))
        );
        assert_eq!(
            runtime
                .invoke_stream(
                    &MethodRef {
                        owner: "java/util/stream/Stream".into(),
                        name: "noneMatch".into(),
                        descriptor: "(Ljava/util/function/Predicate;)Z".into(),
                    },
                    Some(&stream),
                    &[Value::Ref(Some("@indy:stream-predicate".into()))],
                    "demo/Stream",
                    "probe",
                )
                .unwrap(),
            Some(Value::Int(0))
        );
        assert_eq!(
            runtime
                .invoke_stream(
                    &MethodRef {
                        owner: "java/util/stream/Stream".into(),
                        name: "allMatch".into(),
                        descriptor: "(Ljava/util/function/Predicate;)Z".into(),
                    },
                    Some(&stream),
                    &[Value::Ref(Some("@indy:stream-predicate".into()))],
                    "demo/Stream",
                    "probe",
                )
                .unwrap(),
            Some(Value::Int(1))
        );
        assert_eq!(
            runtime
                .invoke_stream(
                    &MethodRef {
                        owner: "java/util/stream/Stream".into(),
                        name: "findFirst".into(),
                        descriptor: "()Ljava/util/Optional;".into(),
                    },
                    Some(&stream),
                    &[],
                    "demo/Stream",
                    "probe",
                )
                .unwrap(),
            Some(Value::Ref(Some("optional/alpha".into())))
        );
        let distinct = runtime
            .invoke_stream(
                &MethodRef {
                    owner: "java/util/stream/Stream".into(),
                    name: "distinct".into(),
                    descriptor: "()Ljava/util/stream/Stream;".into(),
                },
                Some(&stream),
                &[],
                "demo/Stream",
                "probe",
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            runtime
                .invoke_stream(
                    &MethodRef {
                        owner: "java/util/stream/Stream".into(),
                        name: "toList".into(),
                        descriptor: "()Ljava/util/List;".into(),
                    },
                    Some(&distinct),
                    &[],
                    "demo/Stream",
                    "probe",
                )
                .unwrap(),
            Some(collection_value(
                "list",
                &[Value::Ref(Some("alpha".into())), Value::Int(2)],
            ))
        );
        let collector = Value::Ref(Some("collector/list".into()));
        assert_eq!(
            runtime
                .invoke_stream(
                    &MethodRef {
                        owner: "java/util/stream/Stream".into(),
                        name: "collect".into(),
                        descriptor: "(Ljava/util/stream/Collector;)Ljava/lang/Object;".into(),
                    },
                    Some(&distinct),
                    std::slice::from_ref(&collector),
                    "demo/Stream",
                    "probe",
                )
                .unwrap(),
            Some(collection_value(
                "list",
                &[Value::Ref(Some("alpha".into())), Value::Int(2)],
            ))
        );
        let array = runtime
            .invoke_stream(
                &MethodRef {
                    owner: "java/util/stream/Stream".into(),
                    name: "toArray".into(),
                    descriptor: "()[Ljava/lang/Object;".into(),
                },
                Some(&distinct),
                &[],
                "demo/Stream",
                "probe",
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            runtime
                .invoke_reflect_array(
                    &MethodRef {
                        owner: "java/lang/reflect/Array".into(),
                        name: "getLength".into(),
                        descriptor: "(Ljava/lang/Object;)I".into(),
                    },
                    std::slice::from_ref(&array),
                    "demo/Stream",
                    "probe",
                )
                .unwrap(),
            Some(Value::Int(2))
        );
    }

    #[test]
    fn reflective_method_lookup_invokes_loaded_static_method() {
        let target = ClassFile {
            name: "demo/Reflective".into(),
            super_name: "java/lang/Object".into(),
            pool: vec![Cp::Empty],
            bootstrap_methods: Vec::new(),
            fields: Vec::new(),
            methods: vec![
                Method {
                    name: "answer".into(),
                    descriptor: "()I".into(),
                    access: 9,
                    code: Some(vec![0x10, 42, 0xac]),
                    max_locals: 0,
                },
                Method {
                    name: "answer".into(),
                    descriptor: "(I)I".into(),
                    access: 9,
                    code: Some(vec![0x1a, 0xac]),
                    max_locals: 1,
                },
            ],
        };
        let classes = BTreeMap::from([(target.name.clone(), target)]);
        let mut host = NullHost;
        let mut runtime = Runtime {
            classes: &classes,
            host: &mut host,
            calls: 0,
            fields: BTreeMap::new(),
            static_fields: BTreeMap::new(),
            initialized_classes: std::collections::BTreeSet::new(),
            arrays: BTreeMap::from([
                ("array/params".into(), Vec::new()),
                ("array/args".into(), Vec::new()),
                (
                    "array/int-param".into(),
                    vec![Value::Ref(Some("int".into()))],
                ),
                ("array/int-arg".into(), vec![Value::Int(7)]),
            ]),
            next_array: 0,
            next_object: 0,
            streams: BTreeMap::new(),
            next_stream: 0,
        };
        let lookup = MethodRef {
            owner: "java/lang/Class".into(),
            name: "getDeclaredMethod".into(),
            descriptor: "(Ljava/lang/String;[Ljava/lang/Class;)Ljava/lang/reflect/Method;".into(),
        };
        let method = runtime
            .invoke_reflect_method(
                &lookup,
                Some(&Value::Ref(Some("demo/Reflective".into()))),
                &[
                    Value::Ref(Some("answer".into())),
                    Value::Ref(Some("array/params".into())),
                ],
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            method,
            Value::Ref(Some("reflect-method/demo/Reflective#answer()I".into()))
        );
        let methods_lookup = MethodRef {
            owner: "java/lang/Class".into(),
            name: "getDeclaredMethods".into(),
            descriptor: "()[Ljava/lang/reflect/Method;".into(),
        };
        let methods = runtime
            .invoke_reflect_method(
                &methods_lookup,
                Some(&Value::Ref(Some("demo/Reflective".into()))),
                &[],
            )
            .unwrap()
            .unwrap();
        let method_count = runtime
            .invoke_reflect_array(
                &MethodRef {
                    owner: "java/lang/reflect/Array".into(),
                    name: "getLength".into(),
                    descriptor: "(Ljava/lang/Object;)I".into(),
                },
                std::slice::from_ref(&methods),
                "demo/Reflective",
                "probe",
            )
            .unwrap();
        assert_eq!(method_count, Some(Value::Int(2)));
        let mut std_host = StdHost::new();
        let method_name = MethodRef {
            owner: "java/lang/reflect/Method".into(),
            name: "getName".into(),
            descriptor: "()Ljava/lang/String;".into(),
        };
        assert_eq!(
            std_host
                .invoke_instance(&method_name, &method, &[])
                .unwrap(),
            Value::Ref(Some("answer".into()))
        );
        let parameter_count = MethodRef {
            owner: "java/lang/reflect/Method".into(),
            name: "getParameterCount".into(),
            descriptor: "()I".into(),
        };
        assert_eq!(
            std_host
                .invoke_instance(&parameter_count, &method, &[])
                .unwrap(),
            Value::Int(0)
        );
        let method_string = MethodRef {
            owner: "java/lang/reflect/Method".into(),
            name: "toString".into(),
            descriptor: "()Ljava/lang/String;".into(),
        };
        assert_eq!(
            std_host
                .invoke_instance(&method_string, &method, &[])
                .unwrap(),
            Value::Ref(Some("reflect-method/demo/Reflective#answer()I".into()))
        );
        let invoke = MethodRef {
            owner: "java/lang/reflect/Method".into(),
            name: "invoke".into(),
            descriptor: "(Ljava/lang/Object;[Ljava/lang/Object;)Ljava/lang/Object;".into(),
        };
        assert_eq!(
            runtime
                .invoke_reflect_method(
                    &invoke,
                    Some(&method),
                    &[Value::Ref(None), Value::Ref(Some("array/args".into()))],
                )
                .unwrap(),
            Some(Value::Int(42))
        );
        assert_eq!(
            runtime
                .invoke_reflect_method(
                    &invoke,
                    Some(&method),
                    &[Value::Ref(None), Value::Ref(None)],
                )
                .unwrap(),
            Some(Value::Int(42))
        );
        let overloaded = runtime
            .invoke_reflect_method(
                &lookup,
                Some(&Value::Ref(Some("demo/Reflective".into()))),
                &[
                    Value::Ref(Some("answer".into())),
                    Value::Ref(Some("array/int-param".into())),
                ],
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            overloaded,
            Value::Ref(Some("reflect-method/demo/Reflective#answer(I)I".into()))
        );
        assert_eq!(
            std_host
                .invoke_instance(&parameter_count, &overloaded, &[])
                .unwrap(),
            Value::Int(1)
        );
        assert_eq!(
            runtime
                .invoke_reflect_method(
                    &invoke,
                    Some(&overloaded),
                    &[Value::Ref(None), Value::Ref(Some("array/int-arg".into()))],
                )
                .unwrap(),
            Some(Value::Int(7))
        );
        assert_eq!(
            runtime
                .class_for_name(&[Value::Ref(Some("demo.Reflective".into()))])
                .unwrap(),
            Value::Ref(Some("demo/Reflective".into()))
        );
        assert_eq!(
            runtime
                .class_for_name(&[Value::Ref(Some("demo.Missing".into()))])
                .unwrap(),
            Value::Ref(None)
        );
    }

    #[test]
    fn parsed_fields_are_available_through_bounded_reflection() {
        fn u2(bytes: &mut Vec<u8>, value: u16) {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        fn utf8(bytes: &mut Vec<u8>, value: &str) {
            bytes.push(1);
            u2(bytes, value.len() as u16);
            bytes.extend_from_slice(value.as_bytes());
        }

        // A minimal class file with one public and one private field.  Keeping
        // this fixture in bytes exercises the same field-table parser used by
        // NativeJar::from_jar rather than only testing hand-built metadata.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0xCAFE_BABEu32.to_be_bytes());
        u2(&mut bytes, 0); // minor
        u2(&mut bytes, 52); // major
        u2(&mut bytes, 9); // constant-pool count
        utf8(&mut bytes, "demo/ReflectiveFields"); // 1
        bytes.extend_from_slice(&[7, 0, 1]); // 2: Class
        utf8(&mut bytes, "java/lang/Object"); // 3
        bytes.extend_from_slice(&[7, 0, 3]); // 4: Class
        utf8(&mut bytes, "PUBLIC"); // 5
        utf8(&mut bytes, "I"); // 6
        utf8(&mut bytes, "privateValue"); // 7
        utf8(&mut bytes, "Ljava/lang/String;"); // 8
        u2(&mut bytes, 0x0021); // access
        u2(&mut bytes, 2); // this_class
        u2(&mut bytes, 4); // super_class
        u2(&mut bytes, 0); // interfaces
        u2(&mut bytes, 2); // fields
        u2(&mut bytes, 0x0009); // public static
        u2(&mut bytes, 5);
        u2(&mut bytes, 6);
        u2(&mut bytes, 0); // attributes
        u2(&mut bytes, 0x0002); // private
        u2(&mut bytes, 7);
        u2(&mut bytes, 8);
        u2(&mut bytes, 0); // attributes
        u2(&mut bytes, 0); // methods
        u2(&mut bytes, 0); // class attributes

        let target = ClassFile::parse(&bytes).unwrap();
        assert_eq!(target.name, "demo/ReflectiveFields");
        assert_eq!(target.fields.len(), 2);
        assert_eq!(target.fields[0].name, "PUBLIC");
        assert_eq!(target.fields[0].descriptor, "I");
        assert!(target.fields[0].is_static());

        let classes = BTreeMap::from([(target.name.clone(), target)]);
        let mut host = NullHost;
        let mut runtime = Runtime {
            classes: &classes,
            host: &mut host,
            calls: 0,
            fields: BTreeMap::new(),
            static_fields: BTreeMap::new(),
            initialized_classes: std::collections::BTreeSet::new(),
            arrays: BTreeMap::new(),
            next_array: 0,
            next_object: 0,
            streams: BTreeMap::new(),
            next_stream: 0,
        };
        let class = Value::Ref(Some("demo/ReflectiveFields".into()));
        let declared_fields = runtime
            .invoke_reflect_method(
                &MethodRef {
                    owner: "java/lang/Class".into(),
                    name: "getDeclaredFields".into(),
                    descriptor: "()[Ljava/lang/reflect/Field;".into(),
                },
                Some(&class),
                &[],
            )
            .unwrap()
            .unwrap();
        let declared_marker = expect_ref(&declared_fields).unwrap();
        let declared = runtime.arrays.get(declared_marker).unwrap().clone();
        assert_eq!(declared.len(), 2);

        let public_fields = runtime
            .invoke_reflect_method(
                &MethodRef {
                    owner: "java/lang/Class".into(),
                    name: "getFields".into(),
                    descriptor: "()[Ljava/lang/reflect/Field;".into(),
                },
                Some(&class),
                &[],
            )
            .unwrap()
            .unwrap();
        let public_marker = expect_ref(&public_fields).unwrap();
        let public = runtime.arrays.get(public_marker).unwrap();
        assert_eq!(public, &[declared[0].clone()]);

        let mut std_host = StdHost::new();
        let field_name = MethodRef {
            owner: "java/lang/reflect/Field".into(),
            name: "getName".into(),
            descriptor: "()Ljava/lang/String;".into(),
        };
        assert_eq!(
            std_host
                .invoke_instance(&field_name, &declared[0], &[])
                .unwrap(),
            Value::Ref(Some("PUBLIC".into()))
        );
        let field_modifiers = MethodRef {
            owner: "java/lang/reflect/Field".into(),
            name: "getModifiers".into(),
            descriptor: "()I".into(),
        };
        assert_eq!(
            std_host
                .invoke_instance(&field_modifiers, &declared[0], &[])
                .unwrap(),
            Value::Int(9)
        );
        let field_type = MethodRef {
            owner: "java/lang/reflect/Field".into(),
            name: "getType".into(),
            descriptor: "()Ljava/lang/Class;".into(),
        };
        assert_eq!(
            std_host
                .invoke_instance(&field_type, &declared[0], &[])
                .unwrap(),
            Value::Ref(Some("I".into()))
        );
    }

    #[test]
    fn functional_markers_execute_targets_in_the_loaded_jar() {
        let target = ClassFile {
            name: "demo/Target".into(),
            super_name: "java/lang/Object".into(),
            pool: vec![Cp::Empty],
            bootstrap_methods: Vec::new(),
            fields: Vec::new(),
            methods: vec![Method {
                name: "callback".into(),
                descriptor: "(Ljava/lang/Object;)Ljava/lang/Object;".into(),
                access: 9,
                code: Some(vec![0x2a, 0xb0]),
                max_locals: 1,
            }],
        };
        let caller = ClassFile {
            name: "demo/Caller".into(),
            super_name: "java/lang/Object".into(),
            pool: vec![
                Cp::Empty,
                Cp::InterfaceMethodRef { class: 2, nt: 3 },
                Cp::Class(4),
                Cp::NameType {
                    name: 5,
                    descriptor: 6,
                },
                Cp::Utf8("java/util/function/Function".into()),
                Cp::Utf8("apply".into()),
                Cp::Utf8("(Ljava/lang/Object;)Ljava/lang/Object;".into()),
                Cp::InterfaceMethodRef { class: 8, nt: 9 },
                Cp::Class(10),
                Cp::NameType {
                    name: 11,
                    descriptor: 12,
                },
                Cp::Utf8("java/util/Optional".into()),
                Cp::Utf8("map".into()),
                Cp::Utf8("(Ljava/util/function/Function;)Ljava/util/Optional;".into()),
            ],
            bootstrap_methods: Vec::new(),
            fields: Vec::new(),
            methods: vec![
                Method {
                    name: "call".into(),
                    descriptor: "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;".into(),
                    access: 9,
                    code: Some(vec![0x2a, 0x2b, 0xb9, 0, 1, 2, 0, 0xb0]),
                    max_locals: 2,
                },
                Method {
                    name: "map".into(),
                    descriptor: "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/util/Optional;".into(),
                    access: 9,
                    code: Some(vec![0x2a, 0x2b, 0xb9, 0, 7, 2, 0, 0xb0]),
                    max_locals: 2,
                },
            ],
        };
        let jar = NativeJar::from_classes([caller, target]);
        let marker = Value::Ref(Some(
            "@indy:demo/Caller#make:0:demo/Target#callback(Ljava/lang/Object;)Ljava/lang/Object;:"
                .into(),
        ));
        assert_eq!(
            jar.invoke_method_with_args(
                "demo/Caller",
                "call",
                "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
                None,
                &[marker, Value::Ref(Some("demo".into()))],
                &mut NullHost,
            )
            .unwrap(),
            Value::Ref(Some("demo".into()))
        );
        assert_eq!(
            jar.invoke_method_with_args(
                "demo/Caller",
                "map",
                "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/util/Optional;",
                None,
                &[
                    Value::Ref(Some("optional/input".into())),
                    Value::Ref(Some(
                        "@indy:demo/Caller#make:0:demo/Target#callback(Ljava/lang/Object;)Ljava/lang/Object;:"
                            .into(),
                    )),
                ],
                &mut NullHost,
            )
            .unwrap(),
            Value::Ref(Some("optional/input".into()))
        );
    }

    #[test]
    fn callback_marker_round_trips_delimited_capture_values() {
        let marker = format!(
            "@indy:demo/Mod#make:1:demo/Target#callback(Ljava/lang/Object;)V:{}",
            value_marker(&Value::Ref(Some("a,b\\c".into())))
        );
        let parsed = parse_callback_marker(&marker).unwrap();
        assert_eq!(parsed.captures, vec![Value::Ref(Some("a,b\\c".into()))]);
    }

    #[test]
    fn dispatch_executes_supported_instance_callback_target() {
        let target = ClassFile {
            name: "demo/Target".into(),
            super_name: "java/lang/Object".into(),
            pool: vec![Cp::Empty],
            bootstrap_methods: Vec::new(),
            fields: Vec::new(),
            methods: vec![Method {
                name: "callback".into(),
                descriptor: "(Ljava/lang/Object;)V".into(),
                access: 1,
                code: Some(vec![0xb1]),
                max_locals: 2,
            }],
        };
        let jar = NativeJar::from_classes([target]);
        let dispatch = EventDispatch {
            listener: HostInvocation {
                method: MethodRef {
                    owner: "net/neoforged/bus/api/IEventBus".into(),
                    name: "addListener".into(),
                    descriptor: "(Ljava/util/function/Consumer;)Lnet/neoforged/bus/api/IEventBus;"
                        .into(),
                },
                receiver: Some(Value::Ref(Some("event-bus".into()))),
                args: vec![Value::Ref(Some(
                    "@indy:demo/Mod#wire:1:demo/Target#callback(Ljava/lang/Object;)V:rTarget"
                        .into(),
                ))],
            },
            event: Value::Ref(Some("register-event".into())),
        };
        let result = jar.execute_dispatch(&dispatch, &mut NullHost).unwrap();
        assert_eq!(result, Value::Void);
    }

    #[test]
    fn standard_host_records_payload_registrations() {
        let mut host = StdHost::new();
        let registrar = Value::Ref(Some("payload-registrar".into()));
        let method = MethodRef {
            owner: "net/neoforged/neoforge/network/registration/PayloadRegistrar".into(),
            name: "playToServer".into(),
            descriptor: "(Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;)Lnet/neoforged/neoforge/network/registration/PayloadRegistrar;".into(),
        };
        assert_eq!(
            host.invoke_instance(
                &method,
                &registrar,
                &[
                    Value::Ref(Some("payload-type".into())),
                    Value::Ref(Some("stream-codec".into())),
                    Value::Ref(Some("handler".into())),
                ],
            )
            .unwrap(),
            registrar
        );
        assert_eq!(
            host.payload_registrations(),
            &[PayloadRegistration {
                direction: "playToServer".into(),
                payload_type: Value::Ref(Some("payload-type".into())),
                codec: Value::Ref(Some("stream-codec".into())),
                handler: Value::Ref(Some("handler".into())),
            }]
        );

        let configuration = MethodRef {
            owner: "net/neoforged/neoforge/network/registration/PayloadRegistrar".into(),
            name: "configurationToClient".into(),
            descriptor: "(Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;)Lnet/neoforged/neoforge/network/registration/PayloadRegistrar;".into(),
        };
        host.invoke_instance(
            &configuration,
            &registrar,
            &[
                Value::Ref(Some("config-type".into())),
                Value::Ref(Some("config-codec".into())),
                Value::Ref(Some("config-handler".into())),
            ],
        )
        .unwrap();
        assert_eq!(
            host.payload_registrations().last().unwrap().direction,
            "configurationToClient"
        );
    }

    #[test]
    fn standard_host_retains_custom_packet_payload_type_identity() {
        let mut host = StdHost::new();
        let type_marker = Value::Ref(Some("packet-type#1".into()));
        let constructor = MethodRef {
            owner: "net/minecraft/network/protocol/common/custom/CustomPacketPayload$Type".into(),
            name: "<init>".into(),
            descriptor:
                "(Lnet/minecraft/resources/Identifier;Lnet/minecraft/network/codec/StreamCodec;)V"
                    .into(),
        };
        host.invoke_instance(
            &constructor,
            &type_marker,
            &[
                Value::Ref(Some("identifier/demo:ping".into())),
                Value::Ref(Some("stream-codec".into())),
            ],
        )
        .unwrap();
        let id = MethodRef {
            owner: "net/minecraft/network/protocol/common/custom/CustomPacketPayload$Type".into(),
            name: "id".into(),
            descriptor: "()Lnet/minecraft/resources/Identifier;".into(),
        };
        assert_eq!(
            host.invoke_instance(&id, &type_marker, &[]).unwrap(),
            Value::Ref(Some("identifier/demo:ping".into()))
        );
        let to_string = MethodRef {
            owner: id.owner.clone(),
            name: "toString".into(),
            descriptor: "()Ljava/lang/String;".into(),
        };
        assert_eq!(
            host.invoke_instance(&to_string, &type_marker, &[]).unwrap(),
            Value::Ref(Some("demo:ping".into()))
        );
    }

    #[test]
    fn standard_host_aliases_legacy_forge_simple_channel_registration() {
        let mut host = StdHost::new();
        let channel = Value::Ref(Some("forge-channel".into()));
        let method = MethodRef {
            owner: "net/minecraftforge/network/simple/SimpleChannel".into(),
            name: "registerMessage".into(),
            descriptor: "(ILjava/lang/Class;Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;)Lnet/minecraftforge/network/simple/SimpleChannel;".into(),
        };
        assert_eq!(
            host.invoke_instance(
                &method,
                &channel,
                &[
                    Value::Int(7),
                    Value::Ref(Some("message-class".into())),
                    Value::Ref(Some("encoder".into())),
                    Value::Ref(Some("decoder".into())),
                    Value::Ref(Some("handler".into())),
                ],
            )
            .unwrap(),
            channel
        );
        assert_eq!(
            host.payload_registrations(),
            &[PayloadRegistration {
                direction: "playBidirectional".into(),
                payload_type: Value::Ref(Some("message-class".into())),
                codec: Value::Ref(Some("encoder".into())),
                handler: Value::Ref(Some("handler".into())),
            }]
        );
    }

    #[test]
    fn standard_host_bridges_seeded_block_capability_requests() {
        let mut host = StdHost::new();
        host.seed_capability(
            "net/minecraft/world/level/Level",
            "capability/item",
            Some("block-pos/1,2,3"),
            Some("direction/north"),
            Value::Ref(Some("handler/items".into())),
        );
        let query = MethodRef {
            owner: "net/minecraft/world/level/Level".into(),
            name: "getCapability".into(),
            descriptor: "(Lnet/neoforged/neoforge/capabilities/Capability;Lnet/minecraft/core/BlockPos;Lnet/minecraft/core/Direction;)Lnet/neoforged/neoforge/common/util/LazyOptional;".into(),
        };
        let optional = host
            .invoke_instance(
                &query,
                &Value::Ref(Some("level/main".into())),
                &[
                    Value::Ref(Some("capability/item".into())),
                    Value::Ref(Some("block-pos/1,2,3".into())),
                    Value::Ref(Some("direction/north".into())),
                ],
            )
            .unwrap();
        assert_eq!(host.capability_requests().len(), 1);
        assert_eq!(
            host.capability_requests()[0],
            CapabilityRequest {
                owner: "net/minecraft/world/level/Level".into(),
                capability: "capability/item".into(),
                position: Some("block-pos/1,2,3".into()),
                side: Some("direction/north".into()),
            }
        );
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "net/neoforged/neoforge/common/util/LazyOptional".into(),
                    name: "resolve".into(),
                    descriptor: "()Ljava/lang/Object;".into(),
                },
                &optional,
                &[],
            )
            .unwrap(),
            Value::Ref(Some("handler/items".into()))
        );
    }

    #[test]
    fn standard_host_tracks_capability_and_lazy_optional_values() {
        let mut host = StdHost::new();
        let capability = host
            .invoke(
                &MethodRef {
                    owner: "net/neoforged/neoforge/capabilities/CapabilityManager".into(),
                    name: "get".into(),
                    descriptor: "(Lnet/neoforged/neoforge/capabilities/CapabilityToken;)Lnet/neoforged/neoforge/capabilities/Capability;".into(),
                },
                &[Value::Ref(Some("token/items".into()))],
            )
            .unwrap();
        assert!(
            matches!(capability, Value::Ref(Some(ref marker)) if marker.starts_with("capability/"))
        );
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "net/neoforged/neoforge/capabilities/Capability".into(),
                    name: "isRegistered".into(),
                    descriptor: "()Z".into(),
                },
                &capability,
                &[],
            )
            .unwrap(),
            Value::Int(1)
        );

        let or_empty = MethodRef {
            owner: "net/neoforged/neoforge/capabilities/Capability".into(),
            name: "orEmpty".into(),
            descriptor:
                "(Ljava/lang/Object;Ljava/lang/Object;)Lnet/neoforged/neoforge/common/util/LazyOptional;"
                    .into(),
        };
        let optional = host
            .invoke_instance(
                &or_empty,
                &capability,
                &[Value::Ref(Some("handler/items".into())), capability.clone()],
            )
            .unwrap();
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "net/neoforged/neoforge/common/util/LazyOptional".into(),
                    name: "resolve".into(),
                    descriptor: "()Ljava/lang/Object;".into(),
                },
                &optional,
                &[],
            )
            .unwrap(),
            Value::Ref(Some("handler/items".into()))
        );

        let optional = host
            .invoke(
                &MethodRef {
                    owner: "net/neoforged/neoforge/common/util/LazyOptional".into(),
                    name: "ofNullable".into(),
                    descriptor:
                        "(Ljava/lang/Object;)Lnet/neoforged/neoforge/common/util/LazyOptional;"
                            .into(),
                },
                &[Value::Ref(Some("handler/items".into()))],
            )
            .unwrap();
        let present = MethodRef {
            owner: "net/neoforged/neoforge/common/util/LazyOptional".into(),
            name: "isPresent".into(),
            descriptor: "()Z".into(),
        };
        assert_eq!(
            host.invoke_instance(&present, &optional, &[]).unwrap(),
            Value::Int(1)
        );
        let or_else = MethodRef {
            owner: "net/neoforged/neoforge/common/util/LazyOptional".into(),
            name: "orElse".into(),
            descriptor: "(Ljava/lang/Object;)Ljava/lang/Object;".into(),
        };
        assert_eq!(
            host.invoke_instance(&or_else, &optional, &[Value::Ref(Some("fallback".into()))],)
                .unwrap(),
            Value::Ref(Some("handler/items".into()))
        );
        let mapped = host
            .invoke_instance(
                &MethodRef {
                    owner: "net/neoforged/neoforge/common/util/LazyOptional".into(),
                    name: "map".into(),
                    descriptor: "(Ljava/util/function/Function;)Lnet/neoforged/neoforge/common/util/LazyOptional;".into(),
                },
                &optional,
                &[Value::Ref(Some("@indy:identity-map".into()))],
            )
            .unwrap();
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "net/neoforged/neoforge/common/util/LazyOptional".into(),
                    name: "resolve".into(),
                    descriptor: "()Ljava/lang/Object;".into(),
                },
                &mapped,
                &[],
            )
            .unwrap(),
            Value::Ref(Some("handler/items".into()))
        );
        host.invoke_instance(
            &MethodRef {
                owner: "net/neoforged/neoforge/common/util/LazyOptional".into(),
                name: "ifPresent".into(),
                descriptor: "(Ljava/util/function/Consumer;)V".into(),
            },
            &mapped,
            &[Value::Ref(Some("@indy:capability-consumer".into()))],
        )
        .unwrap();
        assert!(host.invocations().iter().any(|invocation| {
            invocation.method.owner == "java/util/function/Consumer"
                && invocation.method.name == "accept"
                && invocation.args == [Value::Ref(Some("handler/items".into()))]
        }));
        host.invoke_instance(
            &MethodRef {
                owner: "net/neoforged/neoforge/common/util/LazyOptional".into(),
                name: "invalidate".into(),
                descriptor: "()V".into(),
            },
            &optional,
            &[],
        )
        .unwrap();
        assert_eq!(
            host.invoke_instance(&present, &optional, &[]).unwrap(),
            Value::Int(0)
        );
        assert_eq!(
            host.invoke_instance(&or_else, &optional, &[Value::Ref(Some("fallback".into()))],)
                .unwrap(),
            Value::Ref(Some("fallback".into()))
        );
    }

    #[test]
    fn standard_host_maps_legacy_forge_lazy_optional_alias() {
        let mut host = StdHost::new();
        let owner = "net/minecraftforge/common/util/LazyOptional";
        let optional = host
            .invoke(
                &MethodRef {
                    owner: owner.into(),
                    name: "ofNullable".into(),
                    descriptor: "(Ljava/lang/Object;)Lnet/minecraftforge/common/util/LazyOptional;"
                        .into(),
                },
                &[Value::Ref(Some("forge-value".into()))],
            )
            .unwrap();
        let resolve = MethodRef {
            owner: owner.into(),
            name: "resolve".into(),
            descriptor: "()Ljava/lang/Object;".into(),
        };
        assert_eq!(
            host.invoke_instance(&resolve, &optional, &[]).unwrap(),
            Value::Ref(Some("forge-value".into()))
        );
        host.invoke_instance(
            &MethodRef {
                owner: owner.into(),
                name: "invalidate".into(),
                descriptor: "()V".into(),
            },
            &optional,
            &[],
        )
        .unwrap();
        assert_eq!(
            host.invoke_instance(&resolve, &optional, &[]).unwrap(),
            Value::Ref(None)
        );
    }

    #[test]
    fn standard_host_tracks_fabric_array_backed_event_listeners() {
        let mut host = StdHost::new();
        let create = MethodRef {
            owner: "net/fabricmc/fabric/api/event/EventFactory".into(),
            name: "createArrayBacked".into(),
            descriptor: "(Ljava/lang/Class;Ljava/util/function/Function;)Lnet/fabricmc/fabric/api/event/Event;".into(),
        };
        let event = host
            .invoke(
                &create,
                &[
                    Value::Ref(Some("demo/Event".into())),
                    Value::Ref(Some(
                        "@indy:demo/Mod#event:0:demo/Mod#combine()Ljava/lang/Object;:".into(),
                    )),
                ],
            )
            .unwrap();
        assert_eq!(event, Value::Ref(Some("fabric-event/0".into())));
        let register = MethodRef {
            owner: "net/fabricmc/fabric/api/event/Event".into(),
            name: "register".into(),
            descriptor: "(Ljava/lang/Object;)V".into(),
        };
        let listener = Value::Ref(Some(
            "@indy:demo/Mod#wire:1:demo/Mod#onEvent(Ljava/lang/Object;)V:".into(),
        ));
        assert!(matches!(
            host.invoke_instance(&register, &event, &[listener]),
            Ok(Value::Void)
        ));
        let dispatches = host.dispatch_event(Value::Ref(Some("demo-event".into())));
        assert_eq!(dispatches.len(), 1);
        assert_eq!(
            dispatches[0].listener.method.owner,
            "net/fabricmc/fabric/api/event/Event"
        );
        assert_eq!(dispatches[0].event, Value::Ref(Some("demo-event".into())));
        let cancelled_dispatches =
            host.dispatch_event(Value::Ref(Some("event/cancelled/demo".into())));
        assert_eq!(cancelled_dispatches.len(), 1);
        assert_eq!(
            cancelled_dispatches[0].listener.method.owner,
            "net/fabricmc/fabric/api/event/Event"
        );
        let invoker = MethodRef {
            owner: "net/fabricmc/fabric/api/event/Event".into(),
            name: "invoker".into(),
            descriptor: "()Ljava/lang/Object;".into(),
        };
        assert_eq!(
            host.invoke_instance(&invoker, &event, &[]).unwrap(),
            Value::Ref(Some("fabric-event/0/invoker".into()))
        );
        let is_canceled = MethodRef {
            owner: "demo/Event".into(),
            name: "isCanceled".into(),
            descriptor: "()Z".into(),
        };
        assert_eq!(
            host.invoke_instance(&is_canceled, &event, &[]).unwrap(),
            Value::Int(0)
        );
        assert_eq!(
            host.invoke_instance(
                &is_canceled,
                &Value::Ref(Some("event/cancelled/demo".into())),
                &[],
            )
            .unwrap(),
            Value::Int(1)
        );
    }

    #[test]
    fn standard_host_tracks_forge_register_object_listeners() {
        let mut host = StdHost::new();
        let bus = Value::Ref(Some("forge-event-bus".into()));
        let register = MethodRef {
            owner: "net/minecraftforge/eventbus/api/IEventBus".into(),
            name: "register".into(),
            descriptor: "(Ljava/lang/Object;)Lnet/minecraftforge/eventbus/api/IEventBus;".into(),
        };
        host.invoke_instance(
            &register,
            &bus,
            &[Value::Ref(Some("@indy:forge-static-handler".into()))],
        )
        .unwrap();
        let dispatches = host.dispatch_event(Value::Ref(Some("forge-event".into())));
        assert_eq!(dispatches.len(), 1);
        assert_eq!(
            dispatches[0].listener.method.owner,
            "net/minecraftforge/eventbus/api/IEventBus"
        );
    }

    #[test]
    fn dispatch_event_honors_neoforge_receive_cancelled_flag() {
        let mut host = StdHost::new();
        let bus = Value::Ref(Some("event-bus".into()));
        let add_listener = MethodRef {
            owner: "net/neoforged/bus/api/IEventBus".into(),
            name: "addListener".into(),
            descriptor: "(Ljava/util/function/Consumer;)Lnet/neoforged/bus/api/IEventBus;".into(),
        };
        host.invoke_instance(
            &add_listener,
            &bus,
            &[Value::Ref(Some("@indy:default-listener".into()))],
        )
        .unwrap();
        host.invoke_instance(
            &add_listener,
            &bus,
            &[
                Value::Ref(Some("@indy:cancelled-listener".into())),
                Value::Int(1),
            ],
        )
        .unwrap();
        let dispatches = host.dispatch_event(Value::Ref(Some("event/cancelled/demo".into())));
        assert_eq!(dispatches.len(), 1);
        assert_eq!(
            dispatches[0].listener.args[0],
            Value::Ref(Some("@indy:cancelled-listener".into()))
        );
    }

    #[test]
    fn standard_host_keeps_neoforge_typed_listener_overload_fluent() {
        let mut host = StdHost::new();
        let bus = Value::Ref(Some("event-bus".into()));
        let add_listener = MethodRef {
            owner: "net/neoforged/bus/api/IEventBus".into(),
            name: "addListener".into(),
            descriptor: "(Lnet/neoforged/bus/api/EventPriority;ZLjava/lang/Class;Ljava/util/function/Consumer;)Lnet/neoforged/bus/api/IEventBus;".into(),
        };
        let result = host
            .invoke_instance(
                &add_listener,
                &bus,
                &[
                    Value::Ref(Some("NORMAL".into())),
                    Value::Int(1),
                    Value::Ref(Some("demo/Event".into())),
                    Value::Ref(Some("@indy:typed-listener".into())),
                ],
            )
            .unwrap();
        assert_eq!(result, bus);
        let dispatches = host.dispatch_event(Value::Ref(Some("event/cancelled/demo".into())));
        assert_eq!(dispatches.len(), 1);
        assert_eq!(
            dispatches[0].listener.args.last(),
            Some(&Value::Ref(Some("@indy:typed-listener".into())))
        );
    }

    #[test]
    fn standard_host_unregistration_removes_neoforge_listener() {
        let mut host = StdHost::new();
        let bus = Value::Ref(Some("event-bus".into()));
        let add_listener = MethodRef {
            owner: "net/neoforged/bus/api/IEventBus".into(),
            name: "addListener".into(),
            descriptor: "(Ljava/util/function/Consumer;)Lnet/neoforged/bus/api/IEventBus;".into(),
        };
        let listener = Value::Ref(Some("@indy:remove-me".into()));
        host.invoke_instance(&add_listener, &bus, std::slice::from_ref(&listener))
            .unwrap();
        let unregister = MethodRef {
            owner: "net/neoforged/bus/api/IEventBus".into(),
            name: "unregister".into(),
            descriptor: "(Ljava/lang/Object;)V".into(),
        };
        assert_eq!(
            host.invoke_instance(&unregister, &bus, std::slice::from_ref(&listener))
                .unwrap(),
            Value::Void
        );
        assert!(
            host.dispatch_event(Value::Ref(Some("event/demo".into())))
                .is_empty()
        );
    }

    #[test]
    fn standard_host_records_neoforge_posts_and_reports_bounded_cancellation() {
        let mut host = StdHost::new();
        let post = MethodRef {
            owner: "net/neoforged/bus/api/IEventBus".into(),
            name: "post".into(),
            descriptor: "(Lnet/neoforged/bus/api/Event;)Z".into(),
        };
        let bus = Value::Ref(Some("net/neoforged/bus/api/IEventBus".into()));

        assert_eq!(
            host.invoke_instance(&post, &bus, &[Value::Ref(Some("event/demo".into()))],)
                .unwrap(),
            Value::Int(0)
        );
        assert_eq!(
            host.invoke_instance(
                &post,
                &bus,
                &[Value::Ref(Some("event/cancelled/demo".into()))],
            )
            .unwrap(),
            Value::Int(1)
        );
        assert_eq!(
            host.posted_events(),
            &[
                Value::Ref(Some("event/demo".into())),
                Value::Ref(Some("event/cancelled/demo".into())),
            ]
        );
    }

    #[test]
    fn standard_host_aliases_legacy_forge_event_bus() {
        let mut host = StdHost::new();
        let context = host
            .get_static(&MethodRef {
                owner: "net/minecraftforge/fml/javafmlmod/FMLJavaModLoadingContext".into(),
                name: "get".into(),
                descriptor: "()Lnet/minecraftforge/fml/javafmlmod/FMLJavaModLoadingContext;".into(),
            })
            .unwrap();
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "net/minecraftforge/fml/javafmlmod/FMLJavaModLoadingContext".into(),
                    name: "getModEventBus".into(),
                    descriptor: "()Lnet/minecraftforge/eventbus/api/IEventBus;".into(),
                },
                &context,
                &[],
            )
            .unwrap(),
            Value::Ref(Some("net/minecraftforge/eventbus/api/IEventBus".into()))
        );
        let bus = Value::Ref(Some("net/minecraftforge/eventbus/api/IEventBus".into()));
        let add = MethodRef {
            owner: "net/minecraftforge/eventbus/api/IEventBus".into(),
            name: "addListener".into(),
            descriptor:
                "(Ljava/util/function/Consumer;)Lnet/minecraftforge/eventbus/api/IEventBus;".into(),
        };
        let listener = Value::Ref(Some("@indy:forge-listener".into()));
        assert_eq!(
            host.invoke_instance(&add, &bus, std::slice::from_ref(&listener))
                .unwrap(),
            bus
        );
        assert_eq!(
            host.dispatch_event(Value::Ref(Some("forge-event".into())))
                .len(),
            1
        );
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "net/minecraftforge/eventbus/api/IEventBus".into(),
                    name: "unregister".into(),
                    descriptor: "(Ljava/lang/Object;)V".into(),
                },
                &bus,
                std::slice::from_ref(&listener),
            )
            .unwrap(),
            Value::Void
        );
        assert!(
            host.dispatch_event(Value::Ref(Some("forge-event-after-remove".into())))
                .is_empty()
        );

        let post = MethodRef {
            owner: "net/minecraftforge/eventbus/api/IEventBus".into(),
            name: "post".into(),
            descriptor: "(Lnet/minecraftforge/eventbus/api/Event;)Z".into(),
        };
        assert_eq!(
            host.invoke_instance(
                &post,
                &bus,
                &[Value::Ref(Some("event/cancelled/forge".into()))]
            )
            .unwrap(),
            Value::Int(1)
        );
        assert_eq!(
            host.posted_events(),
            &[Value::Ref(Some("event/cancelled/forge".into()))]
        );
    }

    #[test]
    fn parses_explicit_typed_native_declarations() {
        let (id, entries) = parse_native_metadata(
            r#"{
                "id":"ruby",
                "registrations":[
                  {"registry":"minecraft:block","id":"ruby:lamp","kind":"block","template":"minecraft:stone","item":true,"properties":{"hardness":2.0}},
                  {"registry":"minecraft:mob_effect","id":"ruby:glow","kind":"mob_effect","category":"beneficial","color":16711680},
                  {"registry":"minecraft:ingredient","id":"ruby:stone","kind":"ingredient","ingredient":{"items":["minecraft:stone"]}}
                ]
            }"#,
        )
        .unwrap();
        assert_eq!(id.as_deref(), Some("ruby"));
        assert_eq!(entries.len(), 3);
        assert!(matches!(
            entries[0].declaration,
            Some(RegistryDeclaration::Block { item: true, .. })
        ));
        assert!(matches!(
            entries[1].declaration,
            Some(RegistryDeclaration::MobEffect {
                color: 0xff0000,
                ..
            })
        ));
        assert!(matches!(
            entries[2].declaration,
            Some(RegistryDeclaration::Ingredient { .. })
        ));
    }

    #[test]
    fn standard_host_maps_locale_us_integer_number_format() {
        let mut host = StdHost::new();
        let locale = host
            .get_static(&MethodRef {
                owner: "java/util/Locale".into(),
                name: "US".into(),
                descriptor: "Ljava/util/Locale;".into(),
            })
            .unwrap();
        assert_eq!(locale, Value::Ref(Some("locale/US".into())));
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "java/util/Locale".into(),
                    name: "toLanguageTag".into(),
                    descriptor: "()Ljava/lang/String;".into()
                },
                &locale,
                &[]
            )
            .unwrap(),
            Value::Ref(Some("en-US".into()))
        );
        let format = host
            .invoke(
                &MethodRef {
                    owner: "java/text/NumberFormat".into(),
                    name: "getIntegerInstance".into(),
                    descriptor: "(Ljava/util/Locale;)Ljava/text/NumberFormat;".into(),
                },
                &[locale],
            )
            .unwrap();
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "java/text/NumberFormat".into(),
                    name: "format".into(),
                    descriptor: "(I)Ljava/lang/String;".into()
                },
                &format,
                &[Value::Int(-2_147_483_648)]
            )
            .unwrap(),
            Value::Ref(Some("-2,147,483,648".into()))
        );
    }

    #[test]
    fn standard_host_rejects_non_us_number_format_locale() {
        let mut host = StdHost::new();
        let error = host
            .invoke(
                &MethodRef {
                    owner: "java/text/NumberFormat".into(),
                    name: "getIntegerInstance".into(),
                    descriptor: "(Ljava/util/Locale;)Ljava/text/NumberFormat;".into(),
                },
                &[Value::Ref(Some("locale/FRANCE".into()))],
            )
            .unwrap_err();
        assert!(error.to_string().contains("only Locale.US"));
    }

    #[test]
    fn rejects_malformed_native_metadata_shapes() {
        for text in [
            "[]",
            r#"{"id":false}"#,
            r#"{"registrations":{}}"#,
            r#"{"registrations":[{"registry":"minecraft:item","id":"ruby:wand","kind":"item","template":"minecraft:stick","properties":null}]}"#,
            r#"{"registrations":[{"registry":"minecraft:item","id":"ruby:wand","kind":"item","template":"minecraft:stick","properties":{"x":[]}}]}"#,
            r#"{"registrations":[{"registry":"minecraft:block","id":"ruby:lamp","kind":"block","template":"minecraft:stone","item":"yes"}]}"#,
        ] {
            assert!(parse_native_metadata(text).is_err(), "accepted {text}");
        }
    }

    #[test]
    fn standard_host_preserves_bounded_throwable_metadata() {
        let mut host = StdHost::new();
        let error = Value::Ref(Some("java/lang/IllegalStateException#0".into()));
        let cause = Value::Ref(Some("java/lang/IllegalArgumentException#1".into()));
        let init = MethodRef {
            owner: "java/lang/IllegalStateException".into(),
            name: "<init>".into(),
            descriptor: "(Ljava/lang/String;Ljava/lang/Throwable;)V".into(),
        };
        assert_eq!(
            host.invoke_instance(
                &init,
                &error,
                &[Value::Ref(Some("bad state".into())), cause.clone()],
            )
            .unwrap(),
            Value::Void
        );
        let get_message = MethodRef {
            owner: "java/lang/Throwable".into(),
            name: "getMessage".into(),
            descriptor: "()Ljava/lang/String;".into(),
        };
        assert_eq!(
            host.invoke_instance(&get_message, &error, &[]).unwrap(),
            Value::Ref(Some("bad state".into()))
        );
        let get_cause = MethodRef {
            owner: "java/lang/Throwable".into(),
            name: "getCause".into(),
            descriptor: "()Ljava/lang/Throwable;".into(),
        };
        assert_eq!(
            host.invoke_instance(&get_cause, &error, &[]).unwrap(),
            cause
        );
        let to_string = MethodRef {
            owner: "java/lang/Throwable".into(),
            name: "toString".into(),
            descriptor: "()Ljava/lang/String;".into(),
        };
        assert_eq!(
            host.invoke_instance(&to_string, &error, &[]).unwrap(),
            Value::Ref(Some("IllegalStateException: bad state".into()))
        );
    }

    #[test]
    fn executes_bounded_mixin_injection_at_head_before_target() {
        let target = ClassFile {
            name: "demo/Target".into(),
            super_name: "java/lang/Object".into(),
            pool: vec![Cp::Empty],
            bootstrap_methods: Vec::new(),
            fields: Vec::new(),
            methods: vec![Method {
                name: "tick".into(),
                descriptor: "()I".into(),
                access: 9,
                code: Some(vec![0x08, 0xac]), // iconst_5; ireturn
                max_locals: 0,
            }],
        };
        let callback = ClassFile {
            name: "demo/Mixin".into(),
            super_name: "java/lang/Object".into(),
            pool: vec![Cp::Empty],
            bootstrap_methods: Vec::new(),
            fields: Vec::new(),
            methods: vec![Method {
                name: "onTick".into(),
                descriptor: "(Lorg/spongepowered/asm/mixin/injection/callback/CallbackInfo;)V"
                    .into(),
                access: 9,
                code: Some(vec![0xb1]),
                max_locals: 1,
            }],
        };
        let jar = NativeJar::from_classes([target, callback]);
        let plan = MixinInjectionPlan::inject_at_head(
            MethodRef {
                owner: "demo/Target".into(),
                name: "tick".into(),
                descriptor: "()I".into(),
            },
            MethodRef {
                owner: "demo/Mixin".into(),
                name: "onTick".into(),
                descriptor: "(Lorg/spongepowered/asm/mixin/injection/callback/CallbackInfo;)V"
                    .into(),
            },
        );
        let report = jar
            .execute_mixin_at_head(&plan, None, &[], &mut NullHost)
            .unwrap();
        assert_eq!(report.callback_result, Value::Void);
        assert_eq!(report.target_result, Value::Int(5));
    }

    #[test]
    fn standard_host_maps_fabric_entrypoint_containers() {
        let mut host = StdHost::new()
            .with_mod("demo", "1.2.3")
            .with_fabric_entrypoint("main", "demo.Main", "demo")
            .with_fabric_entrypoint("client", "demo.Client", "demo");
        let loader = Value::Ref(Some("fabric-loader".into()));
        let entries = MethodRef {
            owner: "net/fabricmc/loader/api/FabricLoader".into(),
            name: "getEntrypoints".into(),
            descriptor: "(Ljava/lang/String;)Ljava/util/List;".into(),
        };
        let main = host
            .invoke_instance(&entries, &loader, &[Value::Ref(Some("main".into()))])
            .unwrap();
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "java/util/List".into(),
                    name: "size".into(),
                    descriptor: "()I".into(),
                },
                &main,
                &[],
            )
            .unwrap(),
            Value::Int(1)
        );
        let container = host
            .invoke_instance(
                &MethodRef {
                    owner: "java/util/List".into(),
                    name: "get".into(),
                    descriptor: "(I)Ljava/lang/Object;".into(),
                },
                &main,
                &[Value::Int(0)],
            )
            .unwrap();
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "net/fabricmc/loader/api/EntrypointContainer".into(),
                    name: "getEntrypointClass".into(),
                    descriptor: "()Ljava/lang/Class;".into(),
                },
                &container,
                &[],
            )
            .unwrap(),
            Value::Ref(Some("demo.Main".into()))
        );
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "net/fabricmc/loader/api/EntrypointContainer".into(),
                    name: "getProvider".into(),
                    descriptor: "()Lnet/fabricmc/loader/api/ModContainer;".into(),
                },
                &container,
                &[],
            )
            .unwrap(),
            Value::Ref(Some("mod-container/demo/1.2.3".into()))
        );
        let client = host
            .invoke_instance(&entries, &loader, &[Value::Ref(Some("client".into()))])
            .unwrap();
        assert_eq!(
            host.invoke_instance(
                &MethodRef {
                    owner: "java/util/List".into(),
                    name: "size".into(),
                    descriptor: "()I".into(),
                },
                &client,
                &[],
            )
            .unwrap(),
            Value::Int(1)
        );
        assert!(host.fabric_entrypoint_containers().iter().any(|entry| {
            entry.kind == "client" && entry.class == "demo.Client" && entry.provider == "demo"
        }));
    }

    #[test]
    fn native_jar_seeds_fabric_entrypoint_containers() {
        let jar = NativeJar {
            fabric_id: Some("demo".into()),
            fabric_version: Some("1.2.3".into()),
            fabric_entrypoints: BTreeMap::from([(
                "main".into(),
                vec![Entrypoint {
                    class: "demo.Main".into(),
                    method: None,
                    adapter: None,
                }],
            )]),
            ..NativeJar::default()
        };
        let host = jar.seed_host(StdHost::new());
        assert_eq!(
            host.fabric_entrypoint_containers(),
            &[FabricEntrypointContainer {
                kind: "main".into(),
                class: "demo.Main".into(),
                provider: "demo".into(),
            }]
        );
    }

    #[test]
    fn rejects_unsupported_mixin_translation_contracts() {
        let jar = NativeJar::default();
        let mut plan = MixinInjectionPlan::inject_at_head(
            MethodRef {
                owner: "demo/Target".into(),
                name: "tick".into(),
                descriptor: "()V".into(),
            },
            MethodRef {
                owner: "demo/Mixin".into(),
                name: "onTick".into(),
                descriptor: "()V".into(),
            },
        );
        plan.at = "RETURN".into();
        let error = jar
            .execute_mixin_at_head(&plan, None, &[], &mut NullHost)
            .unwrap_err();
        assert!(error.to_string().contains("only @Inject(at = HEAD)"));
    }
}
