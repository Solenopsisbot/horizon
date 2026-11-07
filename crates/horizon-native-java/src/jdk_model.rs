//! Bounded models of the JDK and logging-library values a mod's config
//! bootstrap reaches before it ever touches the game.
//!
//! Each answer here replaces a `null` the unmodelled-call fallback used to
//! return. A null charset or logger is not neutral: config libraries check
//! their builder state with `Objects.requireNonNull(charset)` or
//! `if (logger == null) throw new IllegalStateException()`, so the jar stopped
//! on a value the JVM would never have produced.
//!
//! The values are plain markers in the runtime's string vocabulary:
//!
//! * `charset/<canonical name>`: a `java.nio.charset.Charset`. Only the six
//!   charsets every JVM must ship (`StandardCharsets`) are known; any other
//!   name raises `UnsupportedCharsetException`, as a JVM without that charset
//!   would.
//! * `logger/<name>`: a log4j, slf4j, `java.util.logging` or Mojang
//!   `LogUtils` logger. Nothing is written anywhere: every level reports
//!   disabled and every logging call returns without effect, which is what a
//!   logger configured to drop the mod's output does.
//!
//! The module also answers `Path.resolveSibling`, which the host's path model
//! lacked. Paths stay virtual (`path:<text>`): no call in this file reads or
//! writes the host filesystem.

use crate::{MethodRef, NativeError, Value, expect_ref, expect_string};

/// The canonical names of the charsets `java.nio.charset.StandardCharsets`
/// declares, keyed by the field name. Every JVM ships these six.
const STANDARD_CHARSETS: &[(&str, &str)] = &[
    ("UTF_8", "UTF-8"),
    ("US_ASCII", "US-ASCII"),
    ("ISO_8859_1", "ISO-8859-1"),
    ("UTF_16", "UTF-16"),
    ("UTF_16BE", "UTF-16BE"),
    ("UTF_16LE", "UTF-16LE"),
];

/// Prefix of a modelled `Charset` marker.
const CHARSET_PREFIX: &str = "charset/";
/// Prefix of a modelled logger marker.
const LOGGER_PREFIX: &str = "logger/";

fn charset_value(canonical: &str) -> Value {
    Value::Ref(Some(format!("{CHARSET_PREFIX}{canonical}")))
}

fn logger_value(name: &str) -> Value {
    Value::Ref(Some(format!("{LOGGER_PREFIX}{name}")))
}

/// The canonical name `Charset.forName` resolves `name` to, matching the
/// JDK's case-insensitive lookup over the standard charsets and their common
/// aliases.
fn canonical_charset(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    Some(match lower.as_str() {
        "utf-8" | "utf8" | "unicode-1-1-utf-8" => "UTF-8",
        "us-ascii" | "ascii" | "iso646-us" | "us" | "default" | "ascii7" | "646" => "US-ASCII",
        "iso-8859-1" | "iso8859_1" | "iso8859-1" | "latin1" | "l1" | "iso_8859-1" | "cp819" => {
            "ISO-8859-1"
        }
        "utf-16" | "utf16" | "utf_16" => "UTF-16",
        "utf-16be" | "utf_16be" | "x-utf-16be" => "UTF-16BE",
        "utf-16le" | "utf_16le" | "x-utf-16le" => "UTF-16LE",
        _ => return None,
    })
}

/// Whether `owner` is one of the logger interfaces this module answers for.
fn is_logger_owner(owner: &str) -> bool {
    matches!(
        owner,
        "org/apache/logging/log4j/Logger"
            | "org/apache/logging/log4j/spi/ExtendedLogger"
            | "org/apache/logging/log4j/spi/AbstractLogger"
            | "org/slf4j/Logger"
            | "java/util/logging/Logger"
            | "java/lang/System$Logger"
    )
}

/// The logger name a factory argument names: a `String` as is, a `Class`
/// marker (an internal name) as its binary name, and anything else as the
/// root logger's empty name.
fn logger_name(args: &[Value]) -> String {
    match args.first() {
        Some(Value::Ref(Some(text))) => text.replace('/', "."),
        _ => String::new(),
    }
}

/// A `GETSTATIC` the model answers, or `None` to leave it to the host.
pub(crate) fn static_field(owner: &str, name: &str) -> Option<Value> {
    match owner {
        "java/nio/charset/StandardCharsets" => STANDARD_CHARSETS
            .iter()
            .find(|(field, _)| *field == name)
            .map(|(_, canonical)| charset_value(canonical)),
        _ => None,
    }
}

fn int_arg(args: &[Value], index: usize) -> Option<i32> {
    args.get(index).and_then(super::boxed_integer)
}

fn long_arg(args: &[Value], index: usize) -> Option<i64> {
    args.get(index).and_then(super::boxed_long)
}

/// A static call the model answers, or `None` to leave it to the host.
pub(crate) fn static_call(r: &MethodRef, args: &[Value]) -> Option<Result<Value, NativeError>> {
    if let Some(collector) = to_map_collector(r, args) {
        return Some(Ok(collector));
    }
    if let Some(comparator) = crate::jdk_stream::comparator_static(r, args) {
        return Some(Ok(comparator));
    }
    match (r.owner.as_str(), r.name.as_str(), r.descriptor.as_str()) {
        // `Arrays.asList(new T[0])`: the shared empty array has no elements,
        // so the list is empty rather than a one-element list holding it.
        ("java/util/Arrays", "asList", "([Ljava/lang/Object;)Ljava/util/List;") if matches!(args.first(), Some(Value::Ref(Some(marker))) if marker == "array/empty") => {
            Some(Ok(Value::Ref(Some("list".into()))))
        }
        // `Modifier`'s predicates over the access flags reflection answers
        // (`Field.getModifiers`): config frameworks check `isPublic`/`isStatic`
        // before they read a field, and the neutral `false` made every field
        // look private.
        ("java/lang/reflect/Modifier", name, "(I)Z") => {
            let bit = match name {
                "isPublic" => 0x0001,
                "isPrivate" => 0x0002,
                "isProtected" => 0x0004,
                "isStatic" => 0x0008,
                "isFinal" => 0x0010,
                "isSynchronized" => 0x0020,
                "isVolatile" => 0x0040,
                "isTransient" => 0x0080,
                "isNative" => 0x0100,
                "isInterface" => 0x0200,
                "isAbstract" => 0x0400,
                "isStrict" => 0x0800,
                _ => return None,
            };
            let flags = int_arg(args, 0)?;
            Some(Ok(Value::Int(i32::from(flags & bit != 0))))
        }
        // The wrappers' static reducers, which streams reach as method
        // references (`Integer::sum` as a `toMap` merge or `reduce` operator).
        ("java/lang/Integer", "sum" | "max" | "min", "(II)I") => {
            let (a, b) = (int_arg(args, 0)?, int_arg(args, 1)?);
            Some(Ok(Value::Int(match r.name.as_str() {
                "sum" => a.wrapping_add(b),
                "max" => a.max(b),
                _ => a.min(b),
            })))
        }
        ("java/lang/Long", "sum" | "max" | "min", "(JJ)J") => {
            let (a, b) = (long_arg(args, 0)?, long_arg(args, 1)?);
            Some(Ok(Value::Long(match r.name.as_str() {
                "sum" => a.wrapping_add(b),
                "max" => a.max(b),
                _ => a.min(b),
            })))
        }
        // Since JDK 18 the default charset is UTF-8 on every platform.
        ("java/nio/charset/Charset", "defaultCharset", "()Ljava/nio/charset/Charset;") => {
            Some(Ok(charset_value("UTF-8")))
        }
        (
            "java/nio/charset/Charset",
            "forName",
            "(Ljava/lang/String;)Ljava/nio/charset/Charset;",
        ) => {
            let name = args.first().map(expect_string).unwrap_or_default();
            Some(match canonical_charset(name) {
                Some(canonical) => Ok(charset_value(canonical)),
                None => Err(NativeError::UncaughtException {
                    class: "java/nio/charset/UnsupportedCharsetException".into(),
                    message: Some(name.to_owned()),
                    frame: None,
                }),
            })
        }
        ("java/nio/charset/Charset", "isSupported", "(Ljava/lang/String;)Z") => {
            let name = args.first().map(expect_string).unwrap_or_default();
            Some(Ok(Value::Int(i32::from(canonical_charset(name).is_some()))))
        }
        // Logger factories. A descriptor that answers a logger type is enough:
        // log4j's overloads take a `String`, a `Class`, an `Object` and an
        // optional `MessageFactory`, and all of them name the logger by their
        // first argument.
        ("org/apache/logging/log4j/LogManager", "getLogger" | "getFormatterLogger", descriptor)
            if descriptor.ends_with(")Lorg/apache/logging/log4j/Logger;") =>
        {
            Some(Ok(logger_value(&logger_name(args))))
        }
        ("org/apache/logging/log4j/LogManager", "getRootLogger", _) => Some(Ok(logger_value(""))),
        ("org/slf4j/LoggerFactory", "getLogger", descriptor)
            if descriptor.ends_with(")Lorg/slf4j/Logger;") =>
        {
            Some(Ok(logger_value(&logger_name(args))))
        }
        // Mojang's `LogUtils.getLogger()` names the caller's class through a
        // stack walk; the bounded runtime has no stack to walk, so the logger
        // is anonymous. Its name is never observable through a disabled logger.
        ("com/mojang/logging/LogUtils", "getLogger", "()Lorg/slf4j/Logger;") => {
            Some(Ok(logger_value("")))
        }
        ("java/util/logging/Logger", "getLogger", descriptor)
            if descriptor.ends_with(")Ljava/util/logging/Logger;") =>
        {
            Some(Ok(logger_value(&logger_name(args))))
        }
        ("java/util/logging/Logger", "getGlobal" | "getAnonymousLogger", _) => {
            Some(Ok(logger_value(if r.name == "getGlobal" {
                "global"
            } else {
                ""
            })))
        }
        _ => None,
    }
}

/// An instance call on a modelled value, or `None` to leave it to the host.
pub(crate) fn instance_call(
    r: &MethodRef,
    receiver: &Value,
    args: &[Value],
) -> Option<Result<Value, NativeError>> {
    if let Some(comparator) = crate::jdk_stream::comparator_instance(r, receiver, args) {
        return Some(Ok(comparator));
    }
    if r.owner == "java/nio/file/Path" && r.name == "resolveSibling" {
        return resolve_sibling(r, receiver, args).map(Ok);
    }
    let marker = expect_ref(receiver)?;
    if let Some(canonical) = marker.strip_prefix(CHARSET_PREFIX) {
        return charset_call(r, canonical, args);
    }
    if let Some(name) = marker.strip_prefix(LOGGER_PREFIX)
        && (is_logger_owner(&r.owner) || r.owner == "java/lang/Object")
    {
        return logger_call(r, name);
    }
    None
}

fn charset_call(
    r: &MethodRef,
    canonical: &str,
    args: &[Value],
) -> Option<Result<Value, NativeError>> {
    let text = || Value::Ref(Some(canonical.to_owned()));
    Some(Ok(match (r.name.as_str(), r.descriptor.as_str()) {
        ("name" | "displayName" | "toString", "()Ljava/lang/String;") => text(),
        ("canEncode" | "isRegistered", "()Z") => Value::Int(1),
        ("equals", "(Ljava/lang/Object;)Z") => Value::Int(i32::from(matches!(
            args.first(),
            Some(Value::Ref(Some(other)))
                if other.strip_prefix(CHARSET_PREFIX) == Some(canonical)
        ))),
        ("hashCode", "()I") => Value::Int(java_string_hash(canonical)),
        _ => return None,
    }))
}

/// A disabled logger: no level is enabled and no call has an effect.
fn logger_call(r: &MethodRef, name: &str) -> Option<Result<Value, NativeError>> {
    let returns = r.descriptor.rsplit_once(')').map_or("", |(_, ret)| ret);
    Some(Ok(match (r.name.as_str(), returns) {
        ("getName", "Ljava/lang/String;") => Value::Ref(Some(name.to_owned())),
        (method, "Z") if method.starts_with("is") => Value::Int(0),
        (_, "V") => Value::Void,
        _ => return None,
    }))
}

/// `Path.resolveSibling(other)`: `other` against the receiver's parent, or
/// `other` itself when the receiver has no parent or `other` is absolute.
fn resolve_sibling(r: &MethodRef, receiver: &Value, args: &[Value]) -> Option<Value> {
    if !matches!(
        r.descriptor.as_str(),
        "(Ljava/lang/String;)Ljava/nio/file/Path;" | "(Ljava/nio/file/Path;)Ljava/nio/file/Path;"
    ) {
        return None;
    }
    let path = expect_ref(receiver)?;
    let path = path.strip_prefix("path:").unwrap_or(path);
    let other = args.first().map(expect_string).unwrap_or_default();
    let other = other.strip_prefix("path:").unwrap_or(other);
    let parent = path
        .trim_end_matches('/')
        .rsplit_once('/')
        .map(|(parent, _)| if parent.is_empty() { "/" } else { parent });
    let resolved = match parent {
        Some(parent) if !other.starts_with('/') => {
            format!("{}/{other}", parent.trim_end_matches('/'))
        }
        _ => other.to_owned(),
    };
    Some(Value::Ref(Some(format!("path:{resolved}"))))
}

/// Elements one `StreamSupport.stream` source may yield before the runtime
/// stops it. Config and service code streams a handful of providers or
/// entries; a spliterator that keeps advancing past this is a generator the
/// bounded runtime refuses rather than spinning on.
pub(crate) const MAX_SPLITERATOR_ELEMENTS: usize = 4096;

/// Prefix of the consumer `StreamSupport.stream` hands a jar spliterator's
/// `tryAdvance`: `accept(value)` appends to the stream marker that follows.
pub(crate) const COLLECTING_CONSUMER_PREFIX: &str = "@fn:collect:";

/// Prefix of a `Collectors.toMap` collector marker: the key and value
/// functions (and the optional merge function) as escaped capture markers.
const TO_MAP_COLLECTOR_PREFIX: &str = "collector/to-map/";

/// The leading operand that marks a `to-map` collector as Guava's.
const GUAVA_COLLECTOR_FLAG: &str = "@guava";

/// The `Collectors.toMap` overloads the host answers with a collector marker.
pub(crate) fn to_map_collector(r: &MethodRef, args: &[Value]) -> Option<Value> {
    // Guava's immutable-map collectors (`ImmutableMap.toImmutableMap`,
    // `Maps.toImmutableEnumMap`, WorldEdit's platform table) collect the same
    // way; only a repeated key raises Guava's `IllegalArgumentException`.
    let guava = match (r.owner.as_str(), r.name.as_str()) {
        ("java/util/stream/Collectors", "toMap") => false,
        ("com/google/common/collect/ImmutableMap", "toImmutableMap")
        | ("com/google/common/collect/Maps", "toImmutableEnumMap") => true,
        _ => return None,
    };
    let functions = match r.descriptor.as_str() {
        "(Ljava/util/function/Function;Ljava/util/function/Function;)Ljava/util/stream/Collector;" => {
            2
        }
        // The merge overload, and the one that also names the map supplier:
        // the bounded map keeps insertion order whichever map type was asked
        // for, so the supplier is not called.
        "(Ljava/util/function/Function;Ljava/util/function/Function;Ljava/util/function/BinaryOperator;)Ljava/util/stream/Collector;"
        | "(Ljava/util/function/Function;Ljava/util/function/Function;Ljava/util/function/BinaryOperator;Ljava/util/function/Supplier;)Ljava/util/stream/Collector;" => {
            3
        }
        _ => return None,
    };
    let mut parts = args
        .iter()
        .take(functions)
        .map(super::value_marker)
        .collect::<Vec<_>>();
    if guava {
        parts.insert(0, GUAVA_COLLECTOR_FLAG.to_owned());
    }
    let parts = parts.join(",");
    Some(Value::Ref(Some(format!(
        "{TO_MAP_COLLECTOR_PREFIX}{parts}"
    ))))
}

impl crate::StdHost {
    /// `List.copyOf`, `Set.copyOf` and `Map.copyOf`: an immutable copy of a
    /// collection the host can read, with the JDK's rules (a `null` element
    /// throws `NullPointerException`, a set keeps the first of equal
    /// elements). Without this the call answered an empty neutral list, and
    /// icy-better-horses found every breed's coat list empty.
    pub(crate) fn collection_copy_of(
        &mut self,
        r: &MethodRef,
        args: &[Value],
    ) -> Option<Result<Value, NativeError>> {
        let kind = match (r.owner.as_str(), r.name.as_str(), r.descriptor.as_str()) {
            ("java/util/List", "copyOf", "(Ljava/util/Collection;)Ljava/util/List;") => "list",
            ("java/util/Set", "copyOf", "(Ljava/util/Collection;)Ljava/util/Set;") => "set",
            ("java/util/Map", "copyOf", "(Ljava/util/Map;)Ljava/util/Map;") => "map",
            _ => return None,
        };
        let source = args.first()?;
        let null_pointer = || NativeError::UncaughtException {
            class: "java/lang/NullPointerException".into(),
            message: None,
            frame: None,
        };
        if matches!(source, Value::Ref(None)) {
            return Some(Err(null_pointer()));
        }
        let values = match super::host_collection_values(self, source).and_then(|items| {
            items
                .iter()
                .map(|item| super::parse_collection_value(item))
                .collect::<Result<Vec<_>, _>>()
        }) {
            Ok(values) => values,
            Err(error) => return Some(Err(error)),
        };
        if values.iter().any(|value| matches!(value, Value::Ref(None))) {
            return Some(Err(null_pointer()));
        }
        let values = if kind == "set" {
            let mut seen = std::collections::BTreeSet::new();
            values
                .into_iter()
                .filter(|value| seen.insert(super::value_marker(value)))
                .collect()
        } else {
            values
        };
        Some(Ok(super::collection_value(kind, &values)))
    }

    /// `Map.Entry.setValue` on an entry `entrySet()` handed out.
    ///
    /// An entry marker is a value (`map-entry/k,v`), not a view, so the write
    /// goes to the one host map that holds exactly this key and value: the
    /// map the entry came from (`for (e : map.entrySet()) e.setValue(...)`,
    /// icy-better-horses freezing its coat lists). The earlier value is the
    /// answer, as on the JVM. An entry no mutable map holds is an immutable
    /// `Map.entry`, which raises `UnsupportedOperationException`; one that
    /// several maps hold is refused rather than written to a guess. The
    /// entry marker itself keeps its old value: a later `getValue()` on the
    /// same entry reads the value it was created with.
    pub(crate) fn entry_set_value(
        &mut self,
        key: &Value,
        old: &Value,
        replacement: Value,
    ) -> Result<Value, NativeError> {
        let holders = self
            .mutable_collections
            .iter()
            .filter(|(marker, _)| {
                super::is_map_owner(
                    marker
                        .split_once('#')
                        .map_or(marker.as_str(), |(class, _)| class),
                )
            })
            .filter_map(|(marker, values)| {
                values
                    .chunks_exact(2)
                    .position(|pair| &pair[0] == key && &pair[1] == old)
                    .map(|pair| (marker.clone(), pair * 2 + 1))
            })
            .collect::<Vec<_>>();
        match holders.as_slice() {
            [] => Err(NativeError::UncaughtException {
                class: "java/lang/UnsupportedOperationException".into(),
                message: None,
                frame: None,
            }),
            [(marker, index)] => {
                let values = self
                    .mutable_collections
                    .get_mut(marker)
                    .expect("the holder was just found");
                Ok(std::mem::replace(&mut values[*index], replacement))
            }
            _ => Err(NativeError::Metadata(
                "Map.Entry.setValue: several maps hold this entry".into(),
            )),
        }
    }

    /// `List.subList(from, to)` on a list the host holds.
    ///
    /// The JDK answers a view; the bounded host answers a copy of the range
    /// as a new mutable list. Reads (what config path code does: NightConfig
    /// walks `path.subList(0, n - 1)` to find a parent table) see the same
    /// elements; a write through the sub-list does not reach the parent. The
    /// range checks raise the JDK's `IndexOutOfBoundsException` /
    /// `IllegalArgumentException` with its messages.
    pub(crate) fn sub_list(
        &mut self,
        r: &MethodRef,
        marker: &str,
        args: &[Value],
    ) -> Result<Value, NativeError> {
        let from = super::expect_int(r, Self::argument(r, args, 0)?)?;
        let to = super::expect_int(r, Self::argument(r, args, 1)?)?;
        let values = self
            .mutable_collections
            .get(marker)
            .cloned()
            .unwrap_or_default();
        let failure = if from < 0 {
            Some((
                "java/lang/IndexOutOfBoundsException",
                format!("fromIndex = {from}"),
            ))
        } else if to as i64 > values.len() as i64 {
            Some((
                "java/lang/IndexOutOfBoundsException",
                format!("toIndex = {to}"),
            ))
        } else if from > to {
            Some((
                "java/lang/IllegalArgumentException",
                format!("fromIndex({from}) > toIndex({to})"),
            ))
        } else {
            None
        };
        if let Some((class, message)) = failure {
            return Err(NativeError::UncaughtException {
                class: class.into(),
                message: Some(message),
                frame: None,
            });
        }
        let range = values[from as usize..to as usize].to_vec();
        let list = self.empty_collection("list");
        match expect_ref(&list).and_then(|marker| self.mutable_collections.get_mut(marker)) {
            Some(stored) => {
                *stored = range;
                Ok(list)
            }
            // Past the neutral-collection bound the host has no fresh mutable
            // list to give; an immutable one still carries the elements.
            None => Ok(super::collection_value("list", &range)),
        }
    }
}

/// The supertypes `instanceof` must see for a boxed value or a JDK
/// collection object, beyond the class itself.
///
/// The interpreter knows the jar's hierarchy from its class files; the JDK's
/// is not in any jar, so `new ArrayList<>() instanceof List` and
/// `Integer.valueOf(3) instanceof Number` answered `false`, and library code
/// that dispatches on value type (NightConfig's TOML writer: "Unsupported
/// value type: java/lang/Integer") refused values the JVM accepts. Only the
/// public types a library tests for are listed.
fn platform_supertypes(class: &str) -> &'static [&'static str] {
    const NUMBER: &[&str] = &[
        "java/lang/Number",
        "java/lang/Comparable",
        "java/io/Serializable",
        "java/lang/constant/Constable",
    ];
    const COMPARABLE_VALUE: &[&str] = &[
        "java/lang/Comparable",
        "java/io/Serializable",
        "java/lang/constant/Constable",
    ];
    const LIST: &[&str] = &[
        "java/util/List",
        "java/util/SequencedCollection",
        "java/util/Collection",
        "java/lang/Iterable",
        "java/util/AbstractList",
        "java/util/AbstractCollection",
        "java/util/RandomAccess",
        "java/lang/Cloneable",
        "java/io/Serializable",
    ];
    const LINKED_LIST: &[&str] = &[
        "java/util/List",
        "java/util/Deque",
        "java/util/Queue",
        "java/util/SequencedCollection",
        "java/util/Collection",
        "java/lang/Iterable",
        "java/util/AbstractSequentialList",
        "java/util/AbstractList",
        "java/util/AbstractCollection",
        "java/lang/Cloneable",
        "java/io/Serializable",
    ];
    const SET: &[&str] = &[
        "java/util/Set",
        "java/util/Collection",
        "java/lang/Iterable",
        "java/util/AbstractSet",
        "java/util/AbstractCollection",
        "java/lang/Cloneable",
        "java/io/Serializable",
    ];
    const SEQUENCED_SET: &[&str] = &[
        "java/util/Set",
        "java/util/SequencedSet",
        "java/util/SequencedCollection",
        "java/util/HashSet",
        "java/util/Collection",
        "java/lang/Iterable",
        "java/util/AbstractSet",
        "java/util/AbstractCollection",
        "java/lang/Cloneable",
        "java/io/Serializable",
    ];
    const SORTED_SET: &[&str] = &[
        "java/util/Set",
        "java/util/SortedSet",
        "java/util/NavigableSet",
        "java/util/SequencedSet",
        "java/util/SequencedCollection",
        "java/util/Collection",
        "java/lang/Iterable",
        "java/util/AbstractSet",
        "java/util/AbstractCollection",
        "java/lang/Cloneable",
        "java/io/Serializable",
    ];
    const MAP: &[&str] = &[
        "java/util/Map",
        "java/util/AbstractMap",
        "java/lang/Cloneable",
        "java/io/Serializable",
    ];
    const SEQUENCED_MAP: &[&str] = &[
        "java/util/Map",
        "java/util/SequencedMap",
        "java/util/HashMap",
        "java/util/AbstractMap",
        "java/lang/Cloneable",
        "java/io/Serializable",
    ];
    const SORTED_MAP: &[&str] = &[
        "java/util/Map",
        "java/util/SortedMap",
        "java/util/NavigableMap",
        "java/util/SequencedMap",
        "java/util/AbstractMap",
        "java/lang/Cloneable",
        "java/io/Serializable",
    ];
    const CONCURRENT_MAP: &[&str] = &[
        "java/util/Map",
        "java/util/concurrent/ConcurrentMap",
        "java/util/AbstractMap",
        "java/io/Serializable",
    ];
    const DEQUE: &[&str] = &[
        "java/util/Deque",
        "java/util/Queue",
        "java/util/SequencedCollection",
        "java/util/Collection",
        "java/lang/Iterable",
        "java/util/AbstractCollection",
        "java/lang/Cloneable",
        "java/io/Serializable",
    ];
    const QUEUE: &[&str] = &[
        "java/util/Queue",
        "java/util/Collection",
        "java/lang/Iterable",
        "java/util/AbstractQueue",
        "java/util/AbstractCollection",
        "java/io/Serializable",
    ];
    match class {
        "java/lang/Integer" | "java/lang/Long" | "java/lang/Short" | "java/lang/Byte"
        | "java/lang/Float" | "java/lang/Double" => NUMBER,
        "java/lang/Boolean" | "java/lang/Character" => COMPARABLE_VALUE,
        "java/util/ArrayList"
        | "java/util/Vector"
        | "java/util/Stack"
        | "java/util/concurrent/CopyOnWriteArrayList" => LIST,
        "java/util/LinkedList" => LINKED_LIST,
        "java/util/HashSet" | "java/util/concurrent/CopyOnWriteArraySet" => SET,
        "java/util/LinkedHashSet" => SEQUENCED_SET,
        "java/util/TreeSet" | "java/util/concurrent/ConcurrentSkipListSet" => SORTED_SET,
        "java/util/HashMap"
        | "java/util/IdentityHashMap"
        | "java/util/WeakHashMap"
        | "java/util/EnumMap"
        | "java/util/Hashtable" => MAP,
        "java/util/LinkedHashMap" => SEQUENCED_MAP,
        "java/util/TreeMap" | "java/util/concurrent/ConcurrentSkipListMap" => SORTED_MAP,
        "java/util/concurrent/ConcurrentHashMap" => CONCURRENT_MAP,
        "java/util/ArrayDeque" | "java/util/concurrent/ConcurrentLinkedDeque" => DEQUE,
        "java/util/PriorityQueue" | "java/util/concurrent/ConcurrentLinkedQueue" => QUEUE,
        _ => &[],
    }
}

/// `marker instanceof wanted` for a boxed value or a JDK collection object
/// (`java/util/ArrayList#3`, `java/util/LinkedHashMap#neutral0`).
pub(crate) fn platform_instance_of(marker: &str, wanted: &str) -> bool {
    if crate::reflect_generic::annotated_type_instance_of(marker, wanted) {
        return true;
    }
    let class = if let Some((class, _)) = marker.split_once('#') {
        class
    } else {
        match marker.split_once(':').map(|(prefix, _)| prefix) {
            Some("integer") => "java/lang/Integer",
            Some("long") => "java/lang/Long",
            Some("short") => "java/lang/Short",
            Some("byte") => "java/lang/Byte",
            Some("float") => "java/lang/Float",
            Some("double") => "java/lang/Double",
            Some("character") => "java/lang/Character",
            Some("boolean") => "java/lang/Boolean",
            _ => return false,
        }
    };
    let known = platform_supertypes(class);
    if known.is_empty() {
        return crate::library_dispatch::fastutil_instance_of(class, wanted);
    }
    class == wanted || known.contains(&wanted)
}

/// Unbox the arguments a method reference to a JDK method receives, the way
/// the lambda metafactory adapts `BinaryOperator<Integer>` to `Integer::sum`:
/// a boxed marker in a primitive parameter position becomes the primitive.
/// `args` are the target's declared parameters in order (no receiver); a value
/// that is already primitive, or not a box, is left as it is.
pub(crate) fn unbox_for_descriptor(descriptor: &str, args: &mut [Value]) {
    for (parameter, value) in super::descriptor_arguments(descriptor)
        .iter()
        .zip(args.iter_mut())
    {
        if !matches!(value, Value::Ref(Some(_))) {
            continue;
        }
        let unboxed = match parameter.as_str() {
            "I" | "S" | "B" => super::boxed_integer(value)
                .or_else(|| super::boxed_short(value).map(i32::from))
                .or_else(|| super::boxed_byte(value).map(i32::from))
                .map(Value::Int),
            "C" => super::boxed_character(value).map(Value::Int),
            "Z" => super::boxed_boolean(value).map(|flag| Value::Int(i32::from(flag))),
            "J" => super::boxed_long(value).map(Value::Long),
            "F" | "D" => super::boxed_number(value).map(Value::Float),
            _ => None,
        };
        if let Some(unboxed) = unboxed {
            *value = unboxed;
        }
    }
}

/// A parsed class's `access_flags`, read back from its bytes (the parser
/// keeps the raw class file and skips the field for every other use).
pub(crate) fn class_access_flags(file: &crate::ClassFile) -> Option<u16> {
    let mut reader = crate::Reader::new(&file.raw);
    if reader.u4().ok()? != 0xCAFE_BABE {
        return None;
    }
    reader.skip(4).ok()?;
    crate::read_pool(&mut reader).ok()?;
    reader.u2().ok()
}

/// The text `String.valueOf` gives a boxed value, for a concatenation
/// operand typed `desc`.
///
/// `"" + Boolean.TRUE` is `"true"`; the runtime's box marker is
/// `boolean:true`, and pasting the marker made Xaero's config codec reject a
/// 12-character boolean against its 5-character bound. Only an operand typed
/// as a box, `Number`, `Comparable`, `Serializable` or `Object` is read as a
/// box: a `String` operand is text, whatever it spells.
pub(crate) fn boxed_text(marker: &str, desc: &str) -> Option<String> {
    if !matches!(
        desc,
        "Ljava/lang/Object;"
            | "Ljava/lang/Number;"
            | "Ljava/lang/Comparable;"
            | "Ljava/io/Serializable;"
            | "Ljava/lang/Boolean;"
            | "Ljava/lang/Integer;"
            | "Ljava/lang/Long;"
            | "Ljava/lang/Short;"
            | "Ljava/lang/Byte;"
            | "Ljava/lang/Character;"
            | "Ljava/lang/Float;"
            | "Ljava/lang/Double;"
    ) {
        return None;
    }
    let (prefix, rest) = marker.split_once(':')?;
    match prefix {
        "boolean" => matches!(rest, "true" | "false").then(|| rest.to_owned()),
        "integer" | "long" | "short" | "byte" => {
            rest.parse::<i64>().ok().map(|value| value.to_string())
        }
        "character" => rest
            .parse::<u32>()
            .ok()
            .and_then(char::from_u32)
            .map(String::from),
        "double" => rest.parse::<f64>().ok().map(super::format_java_float),
        "float" => rest.parse::<f64>().ok().map(super::format_java_float32),
        _ => None,
    }
}

/// The `[flags][width][.precision]` part of one `String.format` specifier.
///
/// Only the `-` (left-justify) and `0` (zero-pad) flags are read; any other
/// flag, an argument index (`%1$s`) or a combination the JDK rejects
/// (`%-d` with no width, `%05s`) is refused with a named reason rather than
/// formatted differently from the JVM.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FormatSpec {
    left: bool,
    zero: bool,
    width: Option<usize>,
    pub(crate) precision: Option<usize>,
}

impl FormatSpec {
    /// Read the specifier starting at `at` (just past `%`), leaving `at` on
    /// the conversion character.
    pub(crate) fn parse(bytes: &[u8], at: &mut usize) -> Result<Self, NativeError> {
        let mut spec = Self::default();
        while let Some(flag) = bytes.get(*at) {
            match flag {
                b'-' => spec.left = true,
                b'0' => spec.zero = true,
                b'+' | b' ' | b',' | b'#' | b'(' => {
                    return Err(NativeError::Metadata(format!(
                        "unsupported String.format flag {}",
                        *flag as char
                    )));
                }
                _ => break,
            }
            *at += 1;
        }
        let digits = |at: &mut usize| {
            let start = *at;
            while bytes.get(*at).is_some_and(u8::is_ascii_digit) {
                *at += 1;
            }
            std::str::from_utf8(&bytes[start..*at])
                .ok()
                .filter(|text| !text.is_empty())
                .and_then(|text| text.parse::<usize>().ok())
        };
        spec.width = digits(at);
        if bytes.get(*at) == Some(&b'$') {
            return Err(NativeError::Metadata(
                "String.format argument indexes are not modelled".into(),
            ));
        }
        if bytes.get(*at) == Some(&b'.') {
            *at += 1;
            spec.precision = Some(digits(at).ok_or_else(|| {
                NativeError::Metadata("String.format precision has no digits".into())
            })?);
        }
        Ok(spec)
    }

    /// Apply the width, flags and (for `%s`) precision to one converted
    /// piece.
    pub(crate) fn pad(&self, piece: String, conversion: char) -> Result<String, NativeError> {
        if matches!(conversion, '%' | 'n') {
            return Ok(piece);
        }
        let numeric = matches!(conversion, 'd' | 'x' | 'X' | 'f');
        if (self.left || self.zero) && self.width.is_none() {
            return Err(NativeError::Metadata(format!(
                "String.format %{conversion} flag needs a width"
            )));
        }
        if self.zero && (!numeric || self.left) {
            return Err(NativeError::Metadata(format!(
                "String.format %{conversion} does not take the 0 flag here"
            )));
        }
        let piece = match (self.precision, conversion) {
            (Some(precision), 's') => piece.chars().take(precision).collect(),
            (Some(_), 'f') | (None, _) => piece,
            (Some(_), other) => {
                return Err(NativeError::Metadata(format!(
                    "String.format %{other} does not take a precision"
                )));
            }
        };
        let Some(width) = self.width else {
            return Ok(piece);
        };
        let length = piece.chars().count();
        if length >= width {
            return Ok(piece);
        }
        let fill = width - length;
        Ok(if self.left {
            format!("{piece}{}", " ".repeat(fill))
        } else if self.zero {
            match piece.strip_prefix('-') {
                Some(digits) => format!("-{}{digits}", "0".repeat(fill)),
                None => format!("{}{piece}", "0".repeat(fill)),
            }
        } else {
            format!("{}{piece}", " ".repeat(fill))
        })
    }
}

/// `%.<precision>f` as the JDK's `Formatter` prints it.
///
/// The JDK rounds `RoundingMode.HALF_UP` on the double's shortest decimal
/// digits (what `Double.toString` shows), not on its exact binary value:
/// `String.format("%.2f", 2.675)` is `2.68` and `9.995` is `10.00`, where a
/// correctly rounded binary value would give `2.67` and `9.99`.
pub(crate) fn format_half_up(value: f64, precision: usize) -> String {
    let scientific = format!("{:e}", value.abs());
    let Some((mantissa, exponent)) = scientific.split_once('e') else {
        return format!("{value:.precision$}");
    };
    let Ok(exponent) = exponent.parse::<i64>() else {
        return format!("{value:.precision$}");
    };
    let significant = mantissa.replace('.', "").into_bytes();
    // The digits as `int.frac`: `point` digits sit before the decimal point.
    let point = exponent + 1;
    let mut integer = Vec::new();
    let mut fraction = Vec::new();
    if point <= 0 {
        integer.push(b'0');
        fraction.extend(std::iter::repeat_n(b'0', (-point) as usize));
        fraction.extend_from_slice(&significant);
    } else {
        let point = point as usize;
        for index in 0..point.max(significant.len()) {
            let digit = significant.get(index).copied().unwrap_or(b'0');
            if index < point {
                integer.push(digit);
            } else {
                fraction.push(digit);
            }
        }
    }
    let round_up = fraction.get(precision).is_some_and(|digit| *digit >= b'5');
    fraction.resize(precision, b'0');
    let mut digits = integer;
    let split = digits.len();
    digits.extend_from_slice(&fraction);
    if round_up {
        let mut index = digits.len();
        loop {
            if index == 0 {
                digits.insert(0, b'1');
                break;
            }
            index -= 1;
            if digits[index] == b'9' {
                digits[index] = b'0';
            } else {
                digits[index] += 1;
                break;
            }
        }
    }
    let split = split + (digits.len() - split - precision);
    let mut text = String::from_utf8_lossy(&digits[..split]).into_owned();
    if precision > 0 {
        text.push('.');
        text.push_str(&String::from_utf8_lossy(&digits[split..]));
    }
    if value.is_sign_negative() {
        format!("-{text}")
    } else {
        text
    }
}

/// Whether `marker` names an object this runtime or the host built
/// (`owner/Class#n`), as opposed to string text.
fn is_object_marker(marker: &str) -> bool {
    marker.rsplit_once('#').is_some_and(|(class, number)| {
        class.contains('/') && !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit())
    })
}

impl<H: crate::Host> crate::Runtime<'_, H> {
    /// The JDK calls the interpreter answers itself, in one dispatch slot:
    /// `CharSequence` on text, `Class` kind queries on jar classes, and list
    /// sorts that need a comparator to run.
    pub(crate) fn invoke_jdk_runtime(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
        args: &[Value],
    ) -> Result<Option<Value>, NativeError> {
        if let Some(value) = self.invoke_class_kind(r, receiver) {
            return Ok(Some(value));
        }
        if let Some(value) = self.invoke_list_sort(r, receiver, args)? {
            return Ok(Some(value));
        }
        self.invoke_char_sequence(r, receiver, args)
    }

    /// `Class.isInterface`/`isEnum`/`isAnnotation`/`isRecord` for a class a
    /// jar declares, from its class-file access flags and superclass.
    ///
    /// The host only knows class names, so it answered `false` for every
    /// `isInterface()`, and a config library that proxies an interface
    /// (servercore's DazzleConf: "OptimizationConfig is not an interface")
    /// refused its own config type. A platform class keeps the host's answer.
    fn invoke_class_kind(&self, r: &MethodRef, receiver: Option<&Value>) -> Option<Value> {
        if r.owner != "java/lang/Class" || r.descriptor != "()Z" {
            return None;
        }
        let name = expect_ref(receiver?)?;
        let file = self.class(name)?;
        let flags = class_access_flags(file)?;
        let answer = match r.name.as_str() {
            "isInterface" => flags & 0x0200 != 0,
            "isAnnotation" => flags & 0x2000 != 0,
            "isEnum" => flags & 0x4000 != 0 && file.super_name == "java/lang/Enum",
            "isRecord" => file.super_name == "java/lang/Record" && flags & 0x0010 != 0,
            _ => return None,
        };
        Some(Value::Int(i32::from(answer)))
    }

    /// A `java.lang.CharSequence` call on a `String` or a modelled
    /// `StringBuilder`/`StringBuffer`.
    ///
    /// Library code types text as `CharSequence` (NightConfig's TOML writer
    /// measures every key with `CharSequence.length()`); the host's string
    /// surface is keyed by `java/lang/String`, so the call is answered as the
    /// `String` method on the receiver's text. A jar class implementing
    /// `CharSequence`, or any other object, keeps the normal dispatch.
    fn invoke_char_sequence(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
        args: &[Value],
    ) -> Result<Option<Value>, NativeError> {
        if r.owner != "java/lang/CharSequence" {
            return Ok(None);
        }
        let Some(receiver) = receiver else {
            return Ok(None);
        };
        let Some(marker) = expect_ref(receiver) else {
            return Ok(None);
        };
        let text = match self.string_builders.get(marker) {
            Some(text) => text.clone(),
            None if is_object_marker(marker) => return Ok(None),
            None => marker.clone(),
        };
        let (name, descriptor) = match (r.name.as_str(), r.descriptor.as_str()) {
            ("subSequence", "(II)Ljava/lang/CharSequence;") => {
                ("substring", "(II)Ljava/lang/String;")
            }
            (
                name @ ("length" | "charAt" | "isEmpty" | "toString" | "chars" | "codePoints"),
                descriptor,
            ) => (name, descriptor),
            _ => return Ok(None),
        };
        let as_string = MethodRef {
            owner: "java/lang/String".into(),
            name: name.into(),
            descriptor: descriptor.into(),
        };
        self.host
            .invoke_instance(&as_string, &Value::Ref(Some(text)), args)
            .map(Some)
    }

    /// The class whose body an instance method reference runs for
    /// `receiver`: the receiver class's own override or inherited body when
    /// the receiver is a jar object, else `None` to keep the symbolic owner.
    ///
    /// The JVM invokes a `REF_invokeVirtual`/`REF_invokeInterface` handle
    /// virtually. The marker does not record the handle kind, so a concrete
    /// symbolic body is only replaced by a jar override found on the
    /// receiver's chain; an abstract one is always resolved there.
    pub(crate) fn callback_dispatch_owner(
        &self,
        target: &MethodRef,
        receiver: Option<&Value>,
    ) -> Option<String> {
        let class = self.receiver_class(receiver?)?.to_owned();
        if class == target.owner {
            return None;
        }
        let (owner, true) = self.member_owner(&class, &target.name, &target.descriptor)? else {
            return None;
        };
        let method = self
            .class(&owner)?
            .method(&target.name, &target.descriptor)?;
        (method.code.is_some() && !method.is_static() && method.access & 0x0002 == 0)
            .then_some(owner)
    }

    /// The jar class whose override a platform-typed virtual call selects.
    ///
    /// `invokevirtual java/io/Reader.read()` on a `FastStringReader#10` the
    /// jar constructed must run `FastStringReader.read()`: the JVM dispatches
    /// on the receiver, and the symbolic owner only fixes the signature. Only
    /// call sites whose symbolic resolution finds no jar body are considered,
    /// only objects this run built (`class#n`), and never a constructor, a
    /// static or a private method, none of which overrides.
    pub(crate) fn platform_override_owner(
        &self,
        r: &MethodRef,
        opcode: u8,
        receiver: Option<&Value>,
    ) -> Option<String> {
        if !matches!(opcode, 0xb6 | 0xb9) || r.name.starts_with('<') {
            return None;
        }
        // A symbolic owner that resolves to a jar body keeps the
        // interpreter's own dispatch rules. What is left is a site whose
        // resolution finds no body in any jar: a platform owner, or a jar
        // class that only inherits the method abstractly (`AbstractConfig`
        // calling `createSubConfig()`, which only `Config` declares).
        if let Some((owner, true)) = self.member_owner(&r.owner, &r.name, &r.descriptor)
            && self
                .class(&owner)
                .and_then(|file| file.method(&r.name, &r.descriptor))
                .is_some_and(|method| method.code.is_some())
        {
            return None;
        }
        let (class, _) = expect_ref(receiver?)?.split_once('#')?;
        let Some((owner, true)) = self.member_owner(class, &r.name, &r.descriptor) else {
            return None;
        };
        let method = self.class(&owner)?.method(&r.name, &r.descriptor)?;
        (method.code.is_some() && !method.is_static() && method.access & 0x0002 == 0)
            .then_some(owner)
    }

    /// `Files.walk(path)` / `Files.list(path)` over the invocation's virtual
    /// file table.
    ///
    /// A path exists when the jar wrote it, created it as a directory, or
    /// wrote something beneath it; anything else raises the JVM's
    /// `NoSuchFileException`, which is what a fresh game directory answers for
    /// a folder the mod never made (emotecraft walks `emotes/`). `walk` yields
    /// the start path, then every descendant (intermediate directories
    /// included) in sorted order; `list` yields the direct children. Both are
    /// bounded by [`MAX_SPLITERATOR_ELEMENTS`]. The host filesystem is never
    /// read.
    pub(crate) fn files_tree(
        &mut self,
        r: &MethodRef,
        path: &str,
        class: &str,
        method: &str,
    ) -> Result<Value, NativeError> {
        let root = path.trim_end_matches('/');
        let prefix = format!("{root}/");
        let mut found = std::collections::BTreeSet::new();
        for key in self.io_files.keys() {
            let Some(rest) = key.strip_prefix(&prefix) else {
                continue;
            };
            let parts = rest
                .split('/')
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>();
            let depth = if r.name == "list" { 1 } else { parts.len() };
            for end in 1..=depth.min(parts.len()) {
                found.insert(format!("{prefix}{}", parts[..end].join("/")));
            }
        }
        if found.is_empty()
            && !self.io_files.contains_key(root)
            && !self.io_files.contains_key(path)
        {
            return Err(NativeError::UncaughtException {
                class: "java/nio/file/NoSuchFileException".into(),
                message: Some(path.to_owned()),
                frame: None,
            });
        }
        if found.len() >= MAX_SPLITERATOR_ELEMENTS {
            return Err(NativeError::Metadata(format!(
                "Files.{} under {path} holds more than {MAX_SPLITERATOR_ELEMENTS} entries",
                r.name
            )));
        }
        let mut values = Vec::with_capacity(found.len() + 1);
        if r.name == "walk" {
            values.push(Value::Ref(Some(format!("path:{path}"))));
        }
        values.extend(
            found
                .into_iter()
                .map(|entry| Value::Ref(Some(format!("path:{entry}")))),
        );
        Ok(self.new_stream(class, method, values))
    }

    /// `accept(value)` on the consumer [`Self::stream_support_stream`] passes
    /// to a spliterator: append the value to the stream being built.
    pub(crate) fn collecting_consumer_accept(
        &mut self,
        marker: &str,
        args: &[Value],
    ) -> Result<Option<Value>, NativeError> {
        let Some(stream) = marker.strip_prefix(COLLECTING_CONSUMER_PREFIX) else {
            return Ok(None);
        };
        let values = self.streams.get_mut(stream).ok_or_else(|| {
            NativeError::Metadata("collecting consumer outlived its stream".into())
        })?;
        if values.len() >= MAX_SPLITERATOR_ELEMENTS {
            return Err(NativeError::Metadata(format!(
                "StreamSupport.stream source yielded more than {MAX_SPLITERATOR_ELEMENTS} elements"
            )));
        }
        values.push(args.first().cloned().unwrap_or(Value::Ref(None)));
        Ok(Some(Value::Void))
    }

    /// `StreamSupport.stream(spliterator, parallel)` over a spliterator the
    /// jar implements (typically a `Spliterators.AbstractSpliterator`
    /// subclass wrapping a `ServiceLoader` iterator).
    ///
    /// The JVM's stream pulls elements through `tryAdvance`; the bounded
    /// stream is eager, so the jar's `tryAdvance` runs until it answers
    /// `false`, each element landing in the new stream through a collecting
    /// consumer. `parallel` changes nothing about which elements a sequential
    /// pull sees. A spliterator the runtime did not build (a host collection's)
    /// is left to the host.
    pub(crate) fn stream_support_stream(
        &mut self,
        r: &MethodRef,
        args: &[Value],
        class: &str,
        method: &str,
    ) -> Result<Option<Value>, NativeError> {
        if r.owner != "java/util/stream/StreamSupport"
            || r.name != "stream"
            || r.descriptor != "(Ljava/util/Spliterator;Z)Ljava/util/stream/Stream;"
        {
            return Ok(None);
        }
        let Some(spliterator) = args.first() else {
            return Ok(None);
        };
        if matches!(spliterator, Value::Ref(None)) {
            return Err(NativeError::UncaughtException {
                class: "java/lang/NullPointerException".into(),
                message: None,
                frame: None,
            });
        }
        let Some(receiver_class) = self.receiver_class(spliterator).map(str::to_owned) else {
            return Ok(None);
        };
        const TRY_ADVANCE: &str = "(Ljava/util/function/Consumer;)Z";
        let Some((owner, true)) = self.member_owner(&receiver_class, "tryAdvance", TRY_ADVANCE)
        else {
            return Ok(None);
        };
        let stream = self.new_stream(class, method, Vec::new());
        let marker = expect_ref(&stream).cloned().unwrap_or_default();
        let consumer = Value::Ref(Some(format!("{COLLECTING_CONSUMER_PREFIX}{marker}")));
        // Each advance yields at most one element, so the element bound in
        // `collecting_consumer_accept` also bounds this loop; the extra step
        // covers an advance that answers `true` without yielding.
        for _ in 0..=MAX_SPLITERATOR_ELEMENTS {
            let advanced = self.invoke(
                &owner,
                "tryAdvance",
                TRY_ADVANCE,
                Some(spliterator.clone()),
                std::slice::from_ref(&consumer),
            )?;
            if !matches!(advanced, Value::Int(value) if value != 0) {
                return Ok(Some(stream));
            }
        }
        Err(NativeError::Metadata(format!(
            "StreamSupport.stream source advanced more than {MAX_SPLITERATOR_ELEMENTS} times"
        )))
    }

    /// `stream.collect(Collectors.toMap(key, value[, merge]))`.
    ///
    /// Keys keep the stream's order. A repeated key without a merge function
    /// raises the JVM's `IllegalStateException` ("Duplicate key ..."); with
    /// one, the merged value replaces the earlier one in place. A `null`
    /// value raises `NullPointerException`, as `HashMap.merge` does.
    pub(crate) fn collect_to_map(
        &mut self,
        collector: &str,
        values: &[Value],
    ) -> Result<Option<Value>, NativeError> {
        let Some(payload) = collector.strip_prefix(TO_MAP_COLLECTOR_PREFIX) else {
            return Ok(None);
        };
        let mut markers = super::split_capture_markers(payload)?;
        let guava = markers
            .first()
            .is_some_and(|marker| marker == GUAVA_COLLECTOR_FLAG);
        if guava {
            markers.remove(0);
        }
        let functions = markers
            .into_iter()
            .map(|marker| super::parse_collection_value(&marker))
            .collect::<Result<Vec<_>, _>>()?;
        let (key_fn, value_fn, merge_fn) = match functions.as_slice() {
            [key, value] => (key.clone(), value.clone(), None),
            [key, value, merge] => (key.clone(), value.clone(), Some(merge.clone())),
            _ => return Ok(None),
        };
        let mut entries: Vec<(Value, Value)> = Vec::new();
        for element in values {
            let key = self.apply_function(&key_fn, element)?;
            let value = self.apply_function(&value_fn, element)?;
            if matches!(value, Value::Ref(None)) {
                return Err(NativeError::UncaughtException {
                    class: "java/lang/NullPointerException".into(),
                    message: None,
                    frame: None,
                });
            }
            let existing = entries
                .iter()
                .position(|(other, _)| super::value_marker(other) == super::value_marker(&key));
            match (existing, &merge_fn) {
                (None, _) => entries.push((key, value)),
                (Some(index), Some(merge)) => {
                    let earlier = entries[index].1.clone();
                    let merged = self.apply_callback(merge, &[earlier, value])?;
                    entries[index].1 = merged;
                }
                (Some(index), None) if guava => {
                    return Err(NativeError::UncaughtException {
                        class: "java/lang/IllegalArgumentException".into(),
                        message: Some(format!(
                            "Multiple entries with same key: {key}={} and {key}={}",
                            super::value_text(&entries[index].1),
                            super::value_text(&value),
                            key = super::value_text(&key),
                        )),
                        frame: None,
                    });
                }
                (Some(index), None) => {
                    return Err(NativeError::UncaughtException {
                        class: "java/lang/IllegalStateException".into(),
                        message: Some(format!(
                            "Duplicate key {} (attempted merging values {} and {})",
                            super::value_text(&key),
                            super::value_text(&entries[index].1),
                            super::value_text(&value)
                        )),
                        frame: None,
                    });
                }
            }
        }
        let flat = entries
            .into_iter()
            .flat_map(|(key, value)| [key, value])
            .collect::<Vec<_>>();
        Ok(Some(super::collection_value("map", &flat)))
    }

    fn apply_function(&mut self, function: &Value, element: &Value) -> Result<Value, NativeError> {
        self.apply_callback(function, std::slice::from_ref(element))
    }

    /// Run a functional-interface value the way a stream stage does: a jar
    /// lambda or method reference through the callback path, anything else
    /// through the host's `Function.apply`.
    pub(crate) fn apply_callback(
        &mut self,
        function: &Value,
        args: &[Value],
    ) -> Result<Value, NativeError> {
        if let Some(value) = self.invoke_callback(function, args)? {
            return Ok(value);
        }
        let descriptor = if args.len() == 2 {
            (
                "java/util/function/BiFunction",
                "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
            )
        } else {
            (
                "java/util/function/Function",
                "(Ljava/lang/Object;)Ljava/lang/Object;",
            )
        };
        self.host.invoke_instance(
            &MethodRef {
                owner: descriptor.0.into(),
                name: "apply".into(),
                descriptor: descriptor.1.into(),
            },
            function,
            args,
        )
    }
}

/// `String#hashCode` over UTF-16 code units.
fn java_string_hash(text: &str) -> i32 {
    text.encode_utf16().fold(0i32, |hash, unit| {
        hash.wrapping_mul(31).wrapping_add(i32::from(unit))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn method(owner: &str, name: &str, descriptor: &str) -> MethodRef {
        MethodRef {
            owner: owner.into(),
            name: name.into(),
            descriptor: descriptor.into(),
        }
    }

    fn text(value: &str) -> Value {
        Value::Ref(Some(value.into()))
    }

    #[test]
    fn charset_for_name_resolves_aliases_and_rejects_unknown_names() {
        let for_name = method(
            "java/nio/charset/Charset",
            "forName",
            "(Ljava/lang/String;)Ljava/nio/charset/Charset;",
        );
        let utf8 = static_call(&for_name, &[text("utf8")]).unwrap().unwrap();
        let name = method("java/nio/charset/Charset", "name", "()Ljava/lang/String;");
        assert_eq!(
            instance_call(&name, &utf8, &[]).unwrap().unwrap(),
            text("UTF-8")
        );
        let latin = static_call(&for_name, &[text("Latin1")]).unwrap().unwrap();
        assert_eq!(
            instance_call(&name, &latin, &[]).unwrap().unwrap(),
            text("ISO-8859-1")
        );
        assert!(matches!(
            static_call(&for_name, &[text("KOI8-R")]),
            Some(Err(NativeError::UncaughtException { class, .. }))
                if class == "java/nio/charset/UnsupportedCharsetException"
        ));
    }

    #[test]
    fn standard_charsets_equal_their_for_name_lookup() {
        let field = static_field("java/nio/charset/StandardCharsets", "UTF_8").unwrap();
        let for_name = method(
            "java/nio/charset/Charset",
            "forName",
            "(Ljava/lang/String;)Ljava/nio/charset/Charset;",
        );
        let looked_up = static_call(&for_name, &[text("UTF-8")]).unwrap().unwrap();
        let equals = method("java/lang/Object", "equals", "(Ljava/lang/Object;)Z");
        assert_eq!(
            instance_call(&equals, &field, &[looked_up])
                .unwrap()
                .unwrap(),
            Value::Int(1)
        );
        let ascii = static_field("java/nio/charset/StandardCharsets", "US_ASCII").unwrap();
        assert_eq!(
            instance_call(&equals, &field, &[ascii]).unwrap().unwrap(),
            Value::Int(0)
        );
        // "UTF-8".hashCode() on a JVM.
        let hash = method("java/nio/charset/Charset", "hashCode", "()I");
        assert_eq!(
            instance_call(&hash, &field, &[]).unwrap().unwrap(),
            Value::Int(81_070_450)
        );
    }

    #[test]
    fn logger_factories_answer_a_named_disabled_logger() {
        let by_class = method(
            "org/apache/logging/log4j/LogManager",
            "getLogger",
            "(Ljava/lang/Class;)Lorg/apache/logging/log4j/Logger;",
        );
        let logger = static_call(&by_class, &[text("xaero/lib/XaeroLib")])
            .unwrap()
            .unwrap();
        assert_ne!(logger, Value::Ref(None));
        let get_name = method(
            "org/apache/logging/log4j/Logger",
            "getName",
            "()Ljava/lang/String;",
        );
        assert_eq!(
            instance_call(&get_name, &logger, &[]).unwrap().unwrap(),
            text("xaero.lib.XaeroLib")
        );
        let enabled = method("org/slf4j/Logger", "isDebugEnabled", "()Z");
        let slf4j = static_call(
            &method(
                "org/slf4j/LoggerFactory",
                "getLogger",
                "(Ljava/lang/String;)Lorg/slf4j/Logger;",
            ),
            &[text("demo")],
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            instance_call(&enabled, &slf4j, &[]).unwrap().unwrap(),
            Value::Int(0)
        );
        let info = method(
            "org/slf4j/Logger",
            "info",
            "(Ljava/lang/String;Ljava/lang/Object;)V",
        );
        assert_eq!(
            instance_call(&info, &slf4j, &[text("{}"), text("x")])
                .unwrap()
                .unwrap(),
            Value::Void
        );
    }

    #[test]
    fn arrays_as_list_of_the_empty_array_is_an_empty_list() {
        let as_list = method(
            "java/util/Arrays",
            "asList",
            "([Ljava/lang/Object;)Ljava/util/List;",
        );
        assert_eq!(
            static_call(&as_list, &[text("array/empty")])
                .unwrap()
                .unwrap(),
            text("list")
        );
        // A runtime-owned array is the interpreter's to expand.
        assert!(static_call(&as_list, &[text("array/demo/Probe/run/0")]).is_none());
    }

    #[test]
    fn modifier_predicates_read_their_flag_bits() {
        // `public static final` is 0x19 on the JVM.
        let flags = Value::Int(0x19);
        for (name, expected) in [
            ("isPublic", 1),
            ("isStatic", 1),
            ("isFinal", 1),
            ("isPrivate", 0),
            ("isAbstract", 0),
        ] {
            let predicate = method("java/lang/reflect/Modifier", name, "(I)Z");
            assert_eq!(
                static_call(&predicate, std::slice::from_ref(&flags))
                    .unwrap()
                    .unwrap(),
                Value::Int(expected),
                "{name}"
            );
        }
    }

    #[test]
    fn format_half_up_rounds_the_shortest_digits_like_the_jdk() {
        // `String.format("%.<p>f", x)` on a JVM (java 21).
        for (value, precision, expected) in [
            (0.125, 2, "0.13"),
            (-0.125, 2, "-0.13"),
            (2.675, 2, "2.68"),
            (9.995, 2, "10.00"),
            (0.5, 0, "1"),
            (1.0 / 3.0, 3, "0.333"),
            (1.005, 2, "1.01"),
            (0.045, 2, "0.05"),
            (123456.785, 2, "123456.79"),
            (1e-7, 3, "0.000"),
            (2.5, 0, "3"),
        ] {
            assert_eq!(
                format_half_up(value, precision),
                expected,
                "{value} {precision}"
            );
        }
    }

    #[test]
    fn resolve_sibling_replaces_the_last_name() {
        let sibling = method(
            "java/nio/file/Path",
            "resolveSibling",
            "(Ljava/lang/String;)Ljava/nio/file/Path;",
        );
        let cases = [
            (
                "path:/horizon/config",
                "defaultconfigs",
                "path:/horizon/defaultconfigs",
            ),
            (
                "path:/horizon/config/a.json",
                "b.json",
                "path:/horizon/config/b.json",
            ),
            ("path:config", "other", "path:other"),
            ("path:/top", "x", "path:/x"),
            ("path:/horizon/config", "/abs", "path:/abs"),
        ];
        for (path, other, expected) in cases {
            assert_eq!(
                instance_call(&sibling, &text(path), &[text(other)])
                    .unwrap()
                    .unwrap(),
                text(expected),
                "{path} sibling {other}"
            );
        }
    }
}
