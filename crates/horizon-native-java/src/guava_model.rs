//! The Guava surface a mod's bootstrap reaches through the copy the game
//! ships: `Preconditions`, `TypeToken`, `CacheBuilder`/`LoadingCache`,
//! `HashMultimap` and friends, and `Sets.immutableEnumSet`.
//!
//! Guava is a platform library on the native path (the game provides it, a
//! mod does not bundle it), so its calls reached the host, which answered
//! null: `checkNotNull(x)` lost `x`, a cache builder was null, and WorldEdit's
//! `Key.of(TypeToken.of(String.class))` failed `Null typeToken`. The answers
//! here are Guava's documented behaviour over the host's own collections.
//!
//! * A `TypeToken` is `typetoken/<Type marker>` (the class name or `type/`
//!   node the reflection surface uses), or a jar subclass object
//!   (`new TypeToken<List<String>>() {}`) whose captured type is read from
//!   its class `Signature` when the super constructor runs. Equality and the
//!   hash are the type's.
//! * A cache is a host map; a `LoadingCache` also keeps its loader, and a miss
//!   runs `CacheLoader.load` (or the function `CacheLoader.from` wrapped) and
//!   stores the answer. Eviction settings are accepted and have no effect, as
//!   nothing in a bootstrap fills a cache past them.
//! * A multimap is a host map from each key to a host set (`HashMultimap`,
//!   `LinkedHashMultimap`) or list (`ArrayListMultimap`); `get` hands out that
//!   collection, so adding to it writes through as Guava's view does.

use super::*;

const TYPE_TOKEN: &str = "com/google/common/reflect/TypeToken";
const TOKEN_PREFIX: &str = "typetoken/";
const CACHE_BUILDER: &str = "guava-cache-builder";
const LOADER_PREFIX: &str = "guava-cache-loader/";
/// The captured type a `TypeToken` subclass object keeps, under its marker.
const RUNTIME_TYPE: &str = "#runtimeTypeLjava/lang/reflect/Type;";

fn map_ref(name: &str, descriptor: &str) -> MethodRef {
    MethodRef {
        owner: "java/util/Map".into(),
        name: name.into(),
        descriptor: descriptor.into(),
    }
}

fn collection_ref(owner: &str, name: &str, descriptor: &str) -> MethodRef {
    MethodRef {
        owner: owner.into(),
        name: name.into(),
        descriptor: descriptor.into(),
    }
}

fn thrown(class: &str, message: Option<String>) -> NativeError {
    NativeError::UncaughtException {
        class: class.into(),
        message,
        frame: None,
    }
}

/// The box class of a primitive class marker, for `TypeToken.wrap`.
fn boxed_class(primitive: &str) -> Option<&'static str> {
    Some(match primitive {
        "boolean" => "java/lang/Boolean",
        "byte" => "java/lang/Byte",
        "char" => "java/lang/Character",
        "short" => "java/lang/Short",
        "int" => "java/lang/Integer",
        "long" => "java/lang/Long",
        "float" => "java/lang/Float",
        "double" => "java/lang/Double",
        "void" => "java/lang/Void",
        _ => return None,
    })
}

/// Java's `String.hashCode` of `text`, so a token's hash is stable and the
/// same for equal types.
fn java_string_hash(text: &str) -> i32 {
    text.encode_utf16().fold(0i32, |hash, unit| {
        hash.wrapping_mul(31).wrapping_add(i32::from(unit))
    })
}

impl<H: Host> Runtime<'_, H> {
    /// The Guava calls this module answers, or `None`.
    pub(super) fn invoke_guava(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
        args: &[Value],
    ) -> Result<Option<Value>, NativeError> {
        if let Some(value) = self.invoke_memoized_supplier(r, receiver, args)? {
            return Ok(Some(value));
        }
        if !r.owner.starts_with("com/google/common/")
            && !receiver
                .and_then(expect_ref)
                .is_some_and(|marker| marker.starts_with(TOKEN_PREFIX))
        {
            // A jar `TypeToken` subclass object answers through its type even
            // when the call site names the subclass.
            // The receiver's own class decides: a member the subclass does
            // not declare is `TypeToken`'s (`toString` through `Object`).
            if let Some(marker) = receiver.and_then(expect_ref)
                && self
                    .fields
                    .contains_key(&format!("{marker}|{TYPE_TOKEN}{RUNTIME_TYPE}"))
                && let Some((class, _)) = marker.split_once('#')
                && self
                    .member_owner(class, &r.name, &r.descriptor)
                    .is_some_and(|(owner, jar_owned)| !jar_owned && owner == TYPE_TOKEN)
            {
                return self.invoke_type_token(r, receiver, args);
            }
            return Ok(None);
        }
        match r.owner.as_str() {
            "com/google/common/base/Preconditions" => self.invoke_preconditions(r, args),
            "com/google/common/collect/Sets" | "com/google/common/collect/ImmutableSet"
                if r.name == "immutableEnumSet" =>
            {
                let mut values = Vec::new();
                for (index, value) in args.iter().enumerate() {
                    if index == 0 && !r.descriptor.starts_with("(Ljava/lang/Iterable;") {
                        values.push(value.clone());
                        continue;
                    }
                    match value {
                        Value::Ref(Some(marker)) if self.arrays.contains_key(marker) => {
                            values.extend(self.array(value)?.clone());
                        }
                        other => {
                            let items = runtime_collection_values(&*self.host, other)?;
                            for item in items {
                                values.push(parse_collection_value(&item)?);
                            }
                        }
                    }
                }
                if values.iter().any(|value| matches!(value, Value::Ref(None))) {
                    return Err(thrown("java/lang/NullPointerException", None));
                }
                let mut unique: Vec<Value> = Vec::with_capacity(values.len());
                for value in values {
                    if !unique.contains(&value) {
                        unique.push(value);
                    }
                }
                Ok(Some(collection_value("set", &unique)))
            }
            "com/google/common/collect/ImmutableSet"
            | "com/google/common/collect/ImmutableMap"
            | "com/google/common/collect/ImmutableBiMap"
                if r.name == "of" =>
            {
                self.invoke_immutable_of(r, args)
            }
            TYPE_TOKEN => self.invoke_type_token(r, receiver, args),
            "com/google/common/cache/CacheBuilder" => self.invoke_cache_builder(r, receiver, args),
            "com/google/common/cache/CacheLoader" => match (r.name.as_str(), r.descriptor.as_str())
            {
                (
                    "from",
                    "(Lcom/google/common/base/Function;)Lcom/google/common/cache/CacheLoader;"
                    | "(Lcom/google/common/base/Supplier;)Lcom/google/common/cache/CacheLoader;",
                ) => {
                    let Some(function) = args.first().and_then(expect_ref) else {
                        return Err(thrown("java/lang/NullPointerException", None));
                    };
                    Ok(Some(Value::Ref(Some(format!("{LOADER_PREFIX}{function}")))))
                }
                // A jar `CacheLoader` subclass runs its own constructor body.
                ("<init>", "()V") => Ok(Some(Value::Void)),
                _ => Ok(None),
            },
            "com/google/common/cache/LoadingCache" | "com/google/common/cache/Cache" => {
                self.invoke_cache(r, receiver, args)
            }
            "com/google/common/collect/HashMultimap"
            | "com/google/common/collect/LinkedHashMultimap"
            | "com/google/common/collect/ArrayListMultimap"
            | "com/google/common/collect/Multimap"
            | "com/google/common/collect/SetMultimap"
            | "com/google/common/collect/ListMultimap" => self.invoke_multimap(r, receiver, args),
            _ => Ok(None),
        }
    }

    /// `ImmutableSet.of(...)` and `ImmutableMap.of(...)`/`ImmutableBiMap.of`,
    /// Guava's documented answers over the host's own collections.
    ///
    /// A set keeps the first of equal elements, in argument order, and its
    /// `of(e1, ..., e6, E... others)` overload spreads the array. A map keeps
    /// its pairs in order and refuses a repeated key with Guava's
    /// `IllegalArgumentException` (a bi-map also a repeated value). A `null`
    /// element, key or value is Guava's `NullPointerException`.
    fn invoke_immutable_of(
        &mut self,
        r: &MethodRef,
        args: &[Value],
    ) -> Result<Option<Value>, NativeError> {
        let mut values = args.to_vec();
        if r.descriptor.contains("[Ljava/lang/Object;)")
            && let Some(rest) = values.pop()
        {
            if matches!(rest, Value::Ref(None)) {
                return Err(thrown("java/lang/NullPointerException", None));
            }
            values.extend(self.array(&rest)?.clone());
        }
        if values.iter().any(|value| matches!(value, Value::Ref(None))) {
            return Err(thrown("java/lang/NullPointerException", None));
        }
        if r.owner == "com/google/common/collect/ImmutableSet" {
            let mut unique: Vec<Value> = Vec::with_capacity(values.len());
            for value in values {
                if !unique.contains(&value) {
                    unique.push(value);
                }
            }
            return Ok(Some(collection_value("set", &unique)));
        }
        if values.len() % 2 != 0 {
            return Ok(None);
        }
        let bimap = r.owner == "com/google/common/collect/ImmutableBiMap";
        for (index, pair) in values.chunks(2).enumerate() {
            for earlier in values.chunks(2).take(index) {
                let (kind, clash) = if earlier[0] == pair[0] {
                    ("key", true)
                } else if bimap && earlier[1] == pair[1] {
                    ("value", true)
                } else {
                    ("", false)
                };
                if clash {
                    let text = |value: &Value| expect_string(value).to_owned();
                    return Err(thrown(
                        "java/lang/IllegalArgumentException",
                        Some(format!(
                            "Multiple entries with same {kind}: {}={} and {}={}",
                            text(&pair[0]),
                            text(&pair[1]),
                            text(&earlier[0]),
                            text(&earlier[1])
                        )),
                    ));
                }
            }
        }
        Ok(Some(collection_value("map", &values)))
    }

    /// `Suppliers.memoize(s)` (and `memoizeWithExpiration`, whose expiry a
    /// bootstrap never outlives) answers a supplier that runs `s` once and
    /// keeps the answer; `Suppliers.ofInstance(v)` always answers `v`. The
    /// memo is the supplier object's own state, so every `get` through
    /// `com.google.common.base.Supplier` or `java.util.function.Supplier`
    /// sees it.
    fn invoke_memoized_supplier(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
        args: &[Value],
    ) -> Result<Option<Value>, NativeError> {
        const MEMO: &str = "com/google/common/base/Suppliers$Memoizing";
        if r.owner == "com/google/common/base/Suppliers" {
            let (delegate, done) = match r.name.as_str() {
                "memoize" | "memoizeWithExpiration" => (args.first().cloned(), false),
                "ofInstance" => (args.first().cloned(), true),
                _ => return Ok(None),
            };
            let Some(delegate) = delegate else {
                return Ok(None);
            };
            if r.name != "ofInstance" && matches!(delegate, Value::Ref(None)) {
                return Err(thrown("java/lang/NullPointerException", None));
            }
            let supplier = self.new_object(MEMO);
            let marker = expect_ref(&supplier).cloned().unwrap_or_default();
            let field = if done { "value" } else { "delegate" };
            self.fields
                .insert(format!("{marker}|{MEMO}#{field}"), delegate);
            if done {
                self.fields
                    .insert(format!("{marker}|{MEMO}#done"), Value::Int(1));
            }
            return Ok(Some(supplier));
        }
        let Some(marker) = receiver.and_then(expect_ref).cloned() else {
            return Ok(None);
        };
        if !marker.starts_with(&format!("{MEMO}#")) {
            return Ok(None);
        }
        if r.name == "toString" {
            return Ok(Some(Value::Ref(Some("Suppliers.memoize(...)".into()))));
        }
        if r.name != "get" || r.descriptor != "()Ljava/lang/Object;" {
            return Ok(None);
        }
        if self.fields.contains_key(&format!("{marker}|{MEMO}#done")) {
            return Ok(Some(
                self.fields
                    .get(&format!("{marker}|{MEMO}#value"))
                    .cloned()
                    .unwrap_or(Value::Ref(None)),
            ));
        }
        let delegate = self
            .fields
            .get(&format!("{marker}|{MEMO}#delegate"))
            .cloned()
            .unwrap_or(Value::Ref(None));
        let value = match self.invoke_callback(&delegate, &[])? {
            Some(value) => value,
            None => {
                // A jar `Supplier` implementation runs its own `get`.
                let class = expect_ref(&delegate)
                    .and_then(|marker| marker.split_once('#'))
                    .map(|(class, _)| class.to_owned())
                    .unwrap_or_default();
                const GET: &str = "()Ljava/lang/Object;";
                match self.member_owner(&class, "get", GET) {
                    Some((owner, true)) => self.invoke(&owner, "get", GET, Some(delegate), &[])?,
                    _ => self.host.invoke_instance(r, &delegate, &[])?,
                }
            }
        };
        self.fields
            .insert(format!("{marker}|{MEMO}#value"), value.clone());
        self.fields
            .insert(format!("{marker}|{MEMO}#done"), Value::Int(1));
        Ok(Some(value))
    }

    fn invoke_preconditions(
        &mut self,
        r: &MethodRef,
        args: &[Value],
    ) -> Result<Option<Value>, NativeError> {
        let message = |this: &Self| -> Option<String> {
            // Guava fills `%s` placeholders; the template alone is enough to
            // name the failure.
            args.get(1).map(|value| match value {
                Value::Ref(Some(text)) => this
                    .string_builders
                    .get(text)
                    .cloned()
                    .unwrap_or_else(|| text.clone()),
                other => value_text(other),
            })
        };
        match r.name.as_str() {
            "checkNotNull" => match args.first() {
                Some(Value::Ref(None)) | None => {
                    Err(thrown("java/lang/NullPointerException", message(self)))
                }
                Some(value) => Ok(Some(value.clone())),
            },
            "checkArgument" | "checkState" => {
                let holds = args.first().and_then(boxed_boolean).unwrap_or(false)
                    || matches!(args.first(), Some(Value::Int(flag)) if *flag != 0);
                if holds {
                    Ok(Some(Value::Void))
                } else {
                    let class = if r.name == "checkArgument" {
                        "java/lang/IllegalArgumentException"
                    } else {
                        "java/lang/IllegalStateException"
                    };
                    Err(thrown(class, message(self)))
                }
            }
            "checkElementIndex" | "checkPositionIndex" => {
                let index = args.first().and_then(boxed_integer).unwrap_or(-1);
                let size = args.get(1).and_then(boxed_integer).unwrap_or(0);
                let fits = if r.name == "checkElementIndex" {
                    index >= 0 && index < size
                } else {
                    index >= 0 && index <= size
                };
                if fits {
                    Ok(Some(Value::Int(index)))
                } else {
                    Err(thrown(
                        "java/lang/IndexOutOfBoundsException",
                        Some(format!("index ({index}) must be less than size ({size})")),
                    ))
                }
            }
            _ => Ok(None),
        }
    }

    /// The `Type` marker a token names: a `typetoken/` marker's suffix, or
    /// the type a jar subclass object captured.
    fn token_type(&self, token: &Value) -> Option<String> {
        let marker = expect_ref(token)?;
        if let Some(inner) = marker.strip_prefix(TOKEN_PREFIX) {
            return Some(inner.to_owned());
        }
        match self
            .fields
            .get(&format!("{marker}|{TYPE_TOKEN}{RUNTIME_TYPE}"))
        {
            Some(Value::Ref(Some(inner))) => Some(inner.clone()),
            _ => None,
        }
    }

    fn token_value(inner: &str) -> Value {
        Value::Ref(Some(format!("{TOKEN_PREFIX}{inner}")))
    }

    /// The raw class a type marker erases to.
    fn raw_class(inner: &str) -> String {
        match decode_generic_type(inner) {
            Some(GenericType::Parameterized { raw, .. }) => raw,
            Some(GenericType::Wildcard { upper, .. }) => upper.first().map_or_else(
                || "java/lang/Object".to_owned(),
                |bound| match bound {
                    GenericType::Class(name) => name.clone(),
                    GenericType::Parameterized { raw, .. } => raw.clone(),
                    _ => "java/lang/Object".to_owned(),
                },
            ),
            Some(GenericType::Variable { bounds, .. }) => bounds.first().map_or_else(
                || "java/lang/Object".to_owned(),
                |bound| match bound {
                    GenericType::Class(name) => name.clone(),
                    GenericType::Parameterized { raw, .. } => raw.clone(),
                    _ => "java/lang/Object".to_owned(),
                },
            ),
            Some(GenericType::Class(name)) => name,
            None => inner.to_owned(),
        }
    }

    fn invoke_type_token(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
        args: &[Value],
    ) -> Result<Option<Value>, NativeError> {
        match (r.name.as_str(), r.descriptor.as_str()) {
            (
                "of",
                "(Ljava/lang/Class;)Lcom/google/common/reflect/TypeToken;"
                | "(Ljava/lang/reflect/Type;)Lcom/google/common/reflect/TypeToken;",
            ) => {
                let Some(inner) = args.first().and_then(expect_ref) else {
                    return Err(thrown("java/lang/NullPointerException", None));
                };
                return Ok(Some(Self::token_value(inner)));
            }
            // `new TypeToken<T>() {}`: the anonymous subclass's generic
            // superclass names `T`. A subclass that does not parameterize
            // `TypeToken` is Guava's "must be parameterized" error.
            ("<init>", "()V") => {
                let Some(marker) = receiver.and_then(expect_ref).cloned() else {
                    return Ok(None);
                };
                let Some((class, _)) = marker.split_once('#') else {
                    return Ok(None);
                };
                let captured = self
                    .class(class)
                    .and_then(ClassFile::class_signature)
                    .as_deref()
                    .and_then(crate::reflect_generic::parse_class_signature)
                    .and_then(|(superclass, _)| match superclass {
                        GenericType::Parameterized { raw, args, .. } if raw == TYPE_TOKEN => {
                            args.into_iter().next()
                        }
                        _ => None,
                    });
                let Some(captured) = captured else {
                    return Err(thrown(
                        "java/lang/IllegalStateException",
                        Some(format!("{} isn't parameterized", class.replace('/', "."))),
                    ));
                };
                self.fields.insert(
                    format!("{marker}|{TYPE_TOKEN}{RUNTIME_TYPE}"),
                    generic_type_value(&captured),
                );
                return Ok(Some(Value::Void));
            }
            _ => {}
        }
        let Some(inner) = receiver.and_then(|token| self.token_type(token)) else {
            return Ok(None);
        };
        Ok(Some(match (r.name.as_str(), r.descriptor.as_str()) {
            ("getType", "()Ljava/lang/reflect/Type;") => Value::Ref(Some(inner)),
            ("getRawType", "()Ljava/lang/Class;") => Value::Ref(Some(Self::raw_class(&inner))),
            ("wrap", "()Lcom/google/common/reflect/TypeToken;") => {
                Self::token_value(boxed_class(&inner).unwrap_or(&inner))
            }
            ("unwrap", "()Lcom/google/common/reflect/TypeToken;") => {
                let primitive = [
                    "boolean", "byte", "char", "short", "int", "long", "float", "double", "void",
                ]
                .into_iter()
                .find(|primitive| boxed_class(primitive) == Some(inner.as_str()));
                Self::token_value(primitive.unwrap_or(&inner))
            }
            ("isPrimitive", "()Z") => Value::Int(i32::from(boxed_class(&inner).is_some())),
            ("isArray", "()Z") => Value::Int(i32::from(inner.starts_with('['))),
            ("getComponentType", "()Lcom/google/common/reflect/TypeToken;") => {
                match inner.strip_prefix('[') {
                    Some(element) => Self::token_value(&erased_field_type(element)),
                    None => Value::Ref(None),
                }
            }
            ("equals", "(Ljava/lang/Object;)Z") => {
                let other = args.first().and_then(|value| self.token_type(value));
                Value::Int(i32::from(other.as_deref() == Some(inner.as_str())))
            }
            ("hashCode", "()I") => Value::Int(java_string_hash(&inner)),
            ("toString", "()Ljava/lang/String;") => {
                Value::Ref(Some(decode_generic_type(&inner).map_or_else(
                    || class_type_display(&inner),
                    |node| generic_type_display(&node),
                )))
            }
            (
                "isSubtypeOf" | "isSupertypeOf",
                "(Lcom/google/common/reflect/TypeToken;)Z" | "(Ljava/lang/reflect/Type;)Z",
            ) => {
                let other = args
                    .first()
                    .and_then(|value| {
                        self.token_type(value)
                            .or_else(|| expect_ref(value).cloned())
                    })
                    .ok_or_else(|| thrown("java/lang/NullPointerException", None))?;
                let (sub, sup) = if r.name == "isSubtypeOf" {
                    (Self::raw_class(&inner), Self::raw_class(&other))
                } else {
                    (Self::raw_class(&other), Self::raw_class(&inner))
                };
                // Raw assignability; type arguments are not compared.
                Value::Int(i32::from(
                    sub == sup || sup == "java/lang/Object" || self.class_is_instance(&sub, &sup),
                ))
            }
            _ => return Ok(None),
        }))
    }

    fn invoke_cache_builder(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
        args: &[Value],
    ) -> Result<Option<Value>, NativeError> {
        if r.name == "newBuilder" || r.name == "from" {
            return Ok(Some(Value::Ref(Some(CACHE_BUILDER.into()))));
        }
        if receiver.and_then(expect_ref).map(String::as_str) != Some(CACHE_BUILDER) {
            return Ok(None);
        }
        if r.name == "build" {
            let Some(cache) = self.host.empty_mutable_collection("map") else {
                return Ok(None);
            };
            let marker = expect_ref(&cache).cloned().unwrap_or_default();
            if let Some(loader) = args.first() {
                if matches!(loader, Value::Ref(None)) {
                    return Err(thrown("java/lang/NullPointerException", None));
                }
                self.fields
                    .insert(format!("{marker}|guava#loader"), loader.clone());
            }
            return Ok(Some(cache));
        }
        // Size, expiry, reference strength, listeners and stats settings keep
        // the builder.
        Ok(r.descriptor
            .ends_with(")Lcom/google/common/cache/CacheBuilder;")
            .then(|| Value::Ref(Some(CACHE_BUILDER.into()))))
    }

    /// Run a cache's loader for `key`.
    fn cache_load(&mut self, loader: &Value, key: &Value) -> Result<Value, NativeError> {
        if let Some(function) =
            expect_ref(loader).and_then(|marker| marker.strip_prefix(LOADER_PREFIX))
        {
            let function = Value::Ref(Some(function.to_owned()));
            return Ok(self
                .invoke_callback(&function, std::slice::from_ref(key))?
                .unwrap_or(Value::Ref(None)));
        }
        let class = expect_ref(loader)
            .and_then(|marker| marker.split_once('#'))
            .map(|(class, _)| class.to_owned())
            .unwrap_or_default();
        const LOAD: &str = "(Ljava/lang/Object;)Ljava/lang/Object;";
        match self.member_owner(&class, "load", LOAD) {
            Some((owner, true)) => self.invoke(
                &owner,
                "load",
                LOAD,
                Some(loader.clone()),
                std::slice::from_ref(key),
            ),
            _ => Err(NativeError::Metadata(format!(
                "LoadingCache loader {} is not a modelled CacheLoader",
                describe_value(loader)
            ))),
        }
    }

    fn invoke_cache(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
        args: &[Value],
    ) -> Result<Option<Value>, NativeError> {
        let Some(cache) = receiver.cloned() else {
            return Ok(None);
        };
        let Some(marker) = expect_ref(&cache).cloned() else {
            return Ok(None);
        };
        let get = map_ref("get", "(Ljava/lang/Object;)Ljava/lang/Object;");
        let put = map_ref(
            "put",
            "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
        );
        Ok(Some(match r.name.as_str() {
            "get" | "getUnchecked" | "apply" if args.len() == 1 => {
                let key = args[0].clone();
                if matches!(key, Value::Ref(None)) {
                    return Err(thrown("java/lang/NullPointerException", None));
                }
                let cached = self
                    .host
                    .invoke_instance(&get, &cache, std::slice::from_ref(&key))?;
                if !matches!(cached, Value::Ref(None)) {
                    cached
                } else {
                    let Some(loader) = self.fields.get(&format!("{marker}|guava#loader")).cloned()
                    else {
                        return Ok(None);
                    };
                    let loaded = self.cache_load(&loader, &key)?;
                    if matches!(loaded, Value::Ref(None)) {
                        // Guava refuses a null load.
                        return Err(thrown(
                            "com/google/common/cache/CacheLoader$InvalidCacheLoadException",
                            Some(format!(
                                "CacheLoader returned null for key {}.",
                                value_text(&key)
                            )),
                        ));
                    }
                    self.host
                        .invoke_instance(&put, &cache, &[key, loaded.clone()])?;
                    loaded
                }
            }
            // `get(key, Callable)` on a plain cache.
            "get" if args.len() == 2 => {
                let key = args[0].clone();
                let cached = self
                    .host
                    .invoke_instance(&get, &cache, std::slice::from_ref(&key))?;
                if !matches!(cached, Value::Ref(None)) {
                    cached
                } else {
                    let loaded = self
                        .invoke_callback(&args[1], &[])?
                        .unwrap_or(Value::Ref(None));
                    self.host
                        .invoke_instance(&put, &cache, &[key, loaded.clone()])?;
                    loaded
                }
            }
            "getIfPresent" => self.host.invoke_instance(&get, &cache, args)?,
            "put" => {
                self.host.invoke_instance(&put, &cache, args)?;
                Value::Void
            }
            "invalidate" => {
                self.host.invoke_instance(
                    &map_ref("remove", "(Ljava/lang/Object;)Ljava/lang/Object;"),
                    &cache,
                    args,
                )?;
                Value::Void
            }
            "invalidateAll" if args.is_empty() => {
                self.host
                    .invoke_instance(&map_ref("clear", "()V"), &cache, &[])?;
                Value::Void
            }
            "size" => {
                let size = self
                    .host
                    .invoke_instance(&map_ref("size", "()I"), &cache, &[])?;
                Value::Long(i64::from(boxed_integer(&size).unwrap_or(0)))
            }
            "asMap" => cache,
            "cleanUp" | "refresh" => Value::Void,
            _ => return Ok(None),
        }))
    }

    fn invoke_multimap(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
        args: &[Value],
    ) -> Result<Option<Value>, NativeError> {
        if r.name == "create" {
            let Some(map) = self.host.empty_mutable_collection("map") else {
                return Ok(None);
            };
            let marker = expect_ref(&map).cloned().unwrap_or_default();
            let values = if r.owner.ends_with("ArrayListMultimap") {
                "list"
            } else {
                "set"
            };
            self.fields.insert(
                format!("{marker}|guava#multimap"),
                Value::Ref(Some(values.into())),
            );
            return Ok(Some(map));
        }
        let Some(map) = receiver.cloned() else {
            return Ok(None);
        };
        let Some(marker) = expect_ref(&map).cloned() else {
            return Ok(None);
        };
        let Some(Value::Ref(Some(kind))) = self
            .fields
            .get(&format!("{marker}|guava#multimap"))
            .cloned()
        else {
            return Ok(None);
        };
        let owner = if kind == "list" {
            "java/util/List"
        } else {
            "java/util/Set"
        };
        let get = map_ref("get", "(Ljava/lang/Object;)Ljava/lang/Object;");
        // The collection a key maps to, made (and stored) on first use.
        let values_of = |this: &mut Self, key: &Value| -> Result<Value, NativeError> {
            let existing = this
                .host
                .invoke_instance(&get, &map, std::slice::from_ref(key))?;
            if !matches!(existing, Value::Ref(None)) {
                return Ok(existing);
            }
            let fresh = this.host.empty_mutable_collection(&kind).ok_or_else(|| {
                NativeError::Metadata("the host models no mutable collection".into())
            })?;
            this.host.invoke_instance(
                &map_ref(
                    "put",
                    "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
                ),
                &map,
                &[key.clone(), fresh.clone()],
            )?;
            Ok(fresh)
        };
        let size_of = |this: &mut Self, collection: &Value| -> Result<i32, NativeError> {
            let size = this.host.invoke_instance(
                &collection_ref(owner, "size", "()I"),
                collection,
                &[],
            )?;
            Ok(boxed_integer(&size).unwrap_or(0))
        };
        let keys = |this: &mut Self| -> Result<Vec<Value>, NativeError> {
            let set =
                this.host
                    .invoke_instance(&map_ref("keySet", "()Ljava/util/Set;"), &map, &[])?;
            runtime_collection_values(&*this.host, &set)?
                .iter()
                .map(|item| parse_collection_value(item))
                .collect()
        };
        Ok(Some(match (r.name.as_str(), r.descriptor.as_str()) {
            ("put", "(Ljava/lang/Object;Ljava/lang/Object;)Z") => {
                let collection = values_of(self, &args[0])?;
                self.host.invoke_instance(
                    &collection_ref(owner, "add", "(Ljava/lang/Object;)Z"),
                    &collection,
                    &args[1..2],
                )?
            }
            ("get", _) => values_of(self, &args[0])?,
            ("containsKey", "(Ljava/lang/Object;)Z") => {
                let existing = self.host.invoke_instance(&get, &map, &args[..1])?;
                let present =
                    !matches!(existing, Value::Ref(None)) && size_of(self, &existing)? > 0;
                Value::Int(i32::from(present))
            }
            ("containsEntry", "(Ljava/lang/Object;Ljava/lang/Object;)Z") => {
                let existing = self.host.invoke_instance(&get, &map, &args[..1])?;
                if matches!(existing, Value::Ref(None)) {
                    Value::Int(0)
                } else {
                    self.host.invoke_instance(
                        &collection_ref(owner, "contains", "(Ljava/lang/Object;)Z"),
                        &existing,
                        &args[1..2],
                    )?
                }
            }
            ("remove", "(Ljava/lang/Object;Ljava/lang/Object;)Z") => {
                let existing = self.host.invoke_instance(&get, &map, &args[..1])?;
                if matches!(existing, Value::Ref(None)) {
                    Value::Int(0)
                } else {
                    self.host.invoke_instance(
                        &collection_ref(owner, "remove", "(Ljava/lang/Object;)Z"),
                        &existing,
                        &args[1..2],
                    )?
                }
            }
            ("size", "()I") | ("isEmpty", "()Z") => {
                let mut total = 0;
                for key in keys(self)? {
                    let collection = self.host.invoke_instance(&get, &map, &[key])?;
                    total += size_of(self, &collection)?;
                }
                if r.name == "size" {
                    Value::Int(total)
                } else {
                    Value::Int(i32::from(total == 0))
                }
            }
            ("keySet", "()Ljava/util/Set;") => {
                let mut present = Vec::new();
                for key in keys(self)? {
                    let collection =
                        self.host
                            .invoke_instance(&get, &map, std::slice::from_ref(&key))?;
                    if size_of(self, &collection)? > 0 {
                        present.push(key);
                    }
                }
                collection_value("set", &present)
            }
            ("values", "()Ljava/util/Collection;") => {
                let mut all = Vec::new();
                for key in keys(self)? {
                    let collection = self.host.invoke_instance(&get, &map, &[key])?;
                    for item in runtime_collection_values(&*self.host, &collection)? {
                        all.push(parse_collection_value(&item)?);
                    }
                }
                collection_value("list", &all)
            }
            ("asMap", "()Ljava/util/Map;") => map,
            ("clear", "()V") => {
                self.host
                    .invoke_instance(&map_ref("clear", "()V"), &map, &[])?;
                Value::Void
            }
            _ => return Ok(None),
        }))
    }
}
