//! Dispatch and JDK shapes the config and serialization libraries a mod
//! bundles reach, kept beside the interpreter instead of inside its 40k-line
//! root.
//!
//! Each answer is what the JVM does for the same call; none of them is keyed
//! by a mod's name.

use super::*;

impl<H: Host> Runtime<'_, H> {
    /// The library and JDK calls this module answers, or `None` to leave the
    /// call to the rest of the dispatch chain.
    pub(super) fn invoke_library_model(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
        args: &[Value],
        class: &str,
        method: &str,
    ) -> Result<Option<Value>, NativeError> {
        // Runtime-built method handles (`method_handles.rs`) before the
        // proxy module's lookups, which refuse every JDK member.
        if let Some(value) = self.invoke_method_handle_model(r, receiver, args)? {
            return Ok(Some(value));
        }
        if let Some(value) = self.invoke_reflect_proxy(r, receiver, args)? {
            return Ok(Some(value));
        }
        if let Some(value) = self.invoke_array_class(r, receiver) {
            return Ok(Some(value));
        }
        if let Some(value) = self.invoke_guava(r, receiver, args)? {
            return Ok(Some(value));
        }
        if let Some(value) = self.invoke_filter_stream(r, receiver, args, class, method)? {
            return Ok(Some(value));
        }
        if let Some(value) = self.invoke_byte_streams(r, receiver, args, class, method)? {
            return Ok(Some(value));
        }
        if let Some(value) = self.invoke_reflect_generic(r, receiver)? {
            return Ok(Some(value));
        }
        if let Some(value) = self.invoke_varargs_path(r, args)? {
            return Ok(Some(value));
        }
        if let Some(value) = self.invoke_stack_trace(r, receiver) {
            return Ok(Some(value));
        }
        if let Some(value) = self.invoke_array_views(r, receiver, args)? {
            return Ok(Some(value));
        }
        if let Some(value) = self.invoke_module(r, receiver, args) {
            return Ok(Some(value));
        }
        if let Some(value) = unboxed_to_string(r, receiver) {
            return Ok(Some(value));
        }
        if let Some(value) = self.invoke_class_is_instance(r, receiver, args)? {
            return Ok(Some(value));
        }
        if let Some(value) = self.invoke_fastutil_primitive(r, receiver, args)? {
            return Ok(Some(value));
        }
        if let Some(value) = self.invoke_simple_entry(r, receiver, args)? {
            return Ok(Some(value));
        }
        if let Some(value) = self.invoke_mapped_registry(r, receiver, args) {
            return Ok(Some(value));
        }
        Ok(None)
    }

    /// A registry a mod constructs itself (`new MappedRegistry<>(key, ...)`,
    /// `new DefaultedMappedRegistry<>(default, key, ...)`) keeps its key, so
    /// `FabricRegistryBuilder.from(registry)` names the same `registry/<id>`
    /// marker `createSimple(key)` would (moderner_beta's registry helper).
    fn invoke_mapped_registry(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
        args: &[Value],
    ) -> Option<Value> {
        const KEY_FIELD: &str = "|net/minecraft/core/MappedRegistry#key";
        match (r.owner.as_str(), r.name.as_str()) {
            (
                "net/minecraft/core/MappedRegistry" | "net/minecraft/core/DefaultedMappedRegistry",
                "<init>",
            ) => {
                let marker = expect_ref(receiver?)?.clone();
                // The default id comes first in `DefaultedMappedRegistry`.
                let key = args
                    .iter()
                    .find(|value| {
                        expect_ref(value).is_some_and(|key| key.starts_with("registry/"))
                    })?
                    .clone();
                self.fields.insert(format!("{marker}{KEY_FIELD}"), key);
                Some(Value::Void)
            }
            ("net/fabricmc/fabric/api/event/registry/FabricRegistryBuilder", "from") => {
                let registry = expect_ref(args.first()?)?;
                let Some(Value::Ref(Some(key))) =
                    self.fields.get(&format!("{registry}{KEY_FIELD}"))
                else {
                    return None;
                };
                let id = key.strip_prefix("registry/")?;
                Some(Value::Ref(Some(format!("fabric-registry-builder/{id}"))))
            }
            _ => None,
        }
    }

    /// `AbstractMap.SimpleEntry` and `SimpleImmutableEntry`, the entries
    /// library code builds itself (DazzleConf's `mapEntryOf`). The key and
    /// value live in the object's own `key`/`value` fields; `getKey`,
    /// `getValue`, `setValue` (the immutable one throws) and `toString`
    /// answer from them. A call typed as `Map.Entry` on one reaches here too,
    /// before the host's `map-entry/` marker model refuses the object.
    fn invoke_simple_entry(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
        args: &[Value],
    ) -> Result<Option<Value>, NativeError> {
        const SIMPLE: &str = "java/util/AbstractMap$SimpleEntry";
        const IMMUTABLE: &str = "java/util/AbstractMap$SimpleImmutableEntry";
        let Some(marker) = receiver.and_then(expect_ref).cloned() else {
            return Ok(None);
        };
        let Some((class, _)) = marker.split_once('#') else {
            return Ok(None);
        };
        if class != SIMPLE && class != IMMUTABLE {
            return Ok(None);
        }
        let immutable = class == IMMUTABLE;
        let key = format!("{marker}|{class}#keyLjava/lang/Object;");
        let value_key = format!("{marker}|{class}#valueLjava/lang/Object;");
        if r.name == "<init>" && (r.owner == SIMPLE || r.owner == IMMUTABLE) {
            let (entry_key, entry_value) = match r.descriptor.as_str() {
                "(Ljava/lang/Object;Ljava/lang/Object;)V" => (
                    args.first().cloned().unwrap_or(Value::Ref(None)),
                    args.get(1).cloned().unwrap_or(Value::Ref(None)),
                ),
                "(Ljava/util/Map$Entry;)V" => {
                    let entry = args.first().cloned().unwrap_or(Value::Ref(None));
                    let get = |name: &str| MethodRef {
                        owner: "java/util/Map$Entry".into(),
                        name: name.into(),
                        descriptor: "()Ljava/lang/Object;".into(),
                    };
                    let k = match self.invoke_simple_entry(&get("getKey"), Some(&entry), &[])? {
                        Some(k) => k,
                        None => self.host.invoke_instance(&get("getKey"), &entry, &[])?,
                    };
                    let v = match self.invoke_simple_entry(&get("getValue"), Some(&entry), &[])? {
                        Some(v) => v,
                        None => self.host.invoke_instance(&get("getValue"), &entry, &[])?,
                    };
                    (k, v)
                }
                _ => return Ok(None),
            };
            self.fields.insert(key, entry_key);
            self.fields.insert(value_key, entry_value);
            return Ok(Some(Value::Void));
        }
        let stored =
            |this: &Self, field: &str| this.fields.get(field).cloned().unwrap_or(Value::Ref(None));
        Ok(Some(match (r.name.as_str(), r.descriptor.as_str()) {
            ("getKey", "()Ljava/lang/Object;") => stored(self, &key),
            ("getValue", "()Ljava/lang/Object;") => stored(self, &value_key),
            ("setValue", "(Ljava/lang/Object;)Ljava/lang/Object;") => {
                if immutable {
                    return Err(NativeError::UncaughtException {
                        class: "java/lang/UnsupportedOperationException".into(),
                        message: None,
                        frame: None,
                    });
                }
                let old = stored(self, &value_key);
                self.fields
                    .insert(value_key, args.first().cloned().unwrap_or(Value::Ref(None)));
                old
            }
            _ => return Ok(None),
        }))
    }

    /// A fastutil collection's primitive overloads, answered through the
    /// `java.util` method they specialise.
    ///
    /// `Object2IntOpenHashMap.put(Object, int)`, `getInt(Object)`,
    /// `IntArrayList.add(int)` and their kin box the primitive, call the
    /// boxed `Map`/`List`/`Set` method the host models, and unbox the answer,
    /// with fastutil's default return value (`0`/`false`) for a missing
    /// entry. `addTo` is `put(k, get(k) + n)` answering the old value. The
    /// collection is the host's ordinary mutable one, so `entrySet()`,
    /// iteration and `Map.get` see the same entries.
    fn invoke_fastutil_primitive(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
        args: &[Value],
    ) -> Result<Option<Value>, NativeError> {
        let Some(kind) = fastutil_kind(&r.owner) else {
            return Ok(None);
        };
        let Some(receiver) = receiver.cloned() else {
            return Ok(None);
        };
        let parameters = descriptor_arguments(&r.descriptor);
        let result = return_desc(&r.descriptor).to_owned();
        let primitive = |descriptor: &str| descriptor.len() == 1 && descriptor != "V";
        if !parameters.iter().any(|parameter| primitive(parameter)) && !primitive(&result) {
            return Ok(None);
        }
        let java = |name: &str, descriptor: &str| MethodRef {
            owner: format!(
                "java/util/{}",
                match kind {
                    "map" => "Map",
                    "list" => "List",
                    _ => "Set",
                }
            ),
            name: name.into(),
            descriptor: descriptor.into(),
        };
        // Box every argument the java.util method takes as `Object`; a `List`
        // index stays an `int`.
        let boxed = |this: &mut Self, keep_index: bool| -> Result<Vec<Value>, NativeError> {
            parameters
                .iter()
                .zip(args)
                .enumerate()
                .map(|(index, (parameter, value))| {
                    if keep_index && index == 0 {
                        Ok(value.clone())
                    } else {
                        this.box_primitive(parameter, value)
                    }
                })
                .collect()
        };
        let name = r.name.as_str();
        let base = name
            .strip_suffix("Int")
            .or_else(|| name.strip_suffix("Long"))
            .or_else(|| name.strip_suffix("Double"))
            .or_else(|| name.strip_suffix("Float"))
            .or_else(|| name.strip_suffix("Boolean"))
            .or_else(|| name.strip_suffix("Short"))
            .or_else(|| name.strip_suffix("Byte"))
            .or_else(|| name.strip_suffix("Char"))
            .filter(|base| matches!(*base, "get" | "remove" | "put" | "add" | "set"))
            .unwrap_or(name);
        let answer = match (kind, base, parameters.len()) {
            ("map", "put", 2) => {
                let call = boxed(self, false)?;
                self.host.invoke_instance(
                    &java(
                        "put",
                        "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
                    ),
                    &receiver,
                    &call,
                )?
            }
            ("map", "get" | "remove" | "containsKey" | "containsValue", 1) => {
                let call = boxed(self, false)?;
                let descriptor = if base.starts_with("contains") {
                    "(Ljava/lang/Object;)Z"
                } else {
                    "(Ljava/lang/Object;)Ljava/lang/Object;"
                };
                self.host
                    .invoke_instance(&java(base, descriptor), &receiver, &call)?
            }
            ("map", "getOrDefault", 2) => {
                let call = boxed(self, false)?;
                self.host.invoke_instance(
                    &java(
                        "getOrDefault",
                        "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
                    ),
                    &receiver,
                    &call,
                )?
            }
            ("map", "addTo", 2) => {
                let call = boxed(self, false)?;
                let old = self.host.invoke_instance(
                    &java("get", "(Ljava/lang/Object;)Ljava/lang/Object;"),
                    &receiver,
                    &call[..1],
                )?;
                let old = unbox_primitive(&result, &old);
                let sum = match (&old, args.get(1)) {
                    (Value::Int(old), Some(Value::Int(add))) => Value::Int(old.wrapping_add(*add)),
                    (Value::Long(old), Some(Value::Long(add))) => {
                        Value::Long(old.wrapping_add(*add))
                    }
                    (Value::Float(old), Some(Value::Float(add))) => Value::Float(old + add),
                    _ => return Ok(None),
                };
                let sum = self.box_primitive(&result, &sum)?;
                self.host.invoke_instance(
                    &java(
                        "put",
                        "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
                    ),
                    &receiver,
                    &[call[0].clone(), sum],
                )?;
                return Ok(Some(old));
            }
            ("list", "add", 1) | ("set", "add" | "contains" | "remove", 1) => {
                let call = boxed(self, false)?;
                self.host
                    .invoke_instance(&java(base, "(Ljava/lang/Object;)Z"), &receiver, &call)?
            }
            ("list", "contains", 1) => {
                let call = boxed(self, false)?;
                self.host.invoke_instance(
                    &java("contains", "(Ljava/lang/Object;)Z"),
                    &receiver,
                    &call,
                )?
            }
            ("list", "indexOf" | "lastIndexOf", 1) => {
                let call = boxed(self, false)?;
                self.host
                    .invoke_instance(&java(base, "(Ljava/lang/Object;)I"), &receiver, &call)?
            }
            ("list", "add", 2) => {
                let call = boxed(self, true)?;
                self.host.invoke_instance(
                    &java("add", "(ILjava/lang/Object;)V"),
                    &receiver,
                    &call,
                )?
            }
            ("list", "get" | "remove", 1) if parameters[0] == "I" => {
                self.host
                    .invoke_instance(&java(base, "(I)Ljava/lang/Object;"), &receiver, args)?
            }
            ("list", "set", 2) => {
                let call = boxed(self, true)?;
                self.host.invoke_instance(
                    &java("set", "(ILjava/lang/Object;)Ljava/lang/Object;"),
                    &receiver,
                    &call,
                )?
            }
            _ => return Ok(None),
        };
        Ok(Some(if primitive(&result) {
            unbox_primitive(&result, &answer)
        } else {
            answer
        }))
    }

    /// The boxed value `Integer.valueOf`/`Long.valueOf`/... answers for one
    /// primitive of `descriptor`; a reference passes through.
    pub(super) fn box_primitive(
        &mut self,
        descriptor: &str,
        value: &Value,
    ) -> Result<Value, NativeError> {
        let (owner, call) = match descriptor {
            "I" => ("java/lang/Integer", "(I)Ljava/lang/Integer;"),
            "J" => ("java/lang/Long", "(J)Ljava/lang/Long;"),
            "D" => ("java/lang/Double", "(D)Ljava/lang/Double;"),
            "F" => ("java/lang/Float", "(F)Ljava/lang/Float;"),
            "Z" => ("java/lang/Boolean", "(Z)Ljava/lang/Boolean;"),
            "S" => ("java/lang/Short", "(S)Ljava/lang/Short;"),
            "B" => ("java/lang/Byte", "(B)Ljava/lang/Byte;"),
            "C" => ("java/lang/Character", "(C)Ljava/lang/Character;"),
            _ => return Ok(value.clone()),
        };
        self.host.invoke(
            &MethodRef {
                owner: owner.into(),
                name: "valueOf".into(),
                descriptor: call.into(),
            },
            std::slice::from_ref(value),
        )
    }

    /// The text `String.valueOf(value)` gives for an object one of the
    /// library models answers `toString` for; `None` for anything else
    /// (strings, boxes and plain markers keep the concatenation's own text).
    pub(super) fn library_to_string(
        &mut self,
        value: &Value,
        class: &str,
        method: &str,
    ) -> Result<Option<String>, NativeError> {
        let Some(marker) = expect_ref(value) else {
            return Ok(None);
        };
        if !marker.contains('#')
            && !marker.starts_with("typetoken/")
            && !marker.starts_with("method-type/")
        {
            return Ok(None);
        }
        let to_string = MethodRef {
            owner: "java/lang/Object".into(),
            name: "toString".into(),
            descriptor: "()Ljava/lang/String;".into(),
        };
        Ok(
            match self.invoke_library_model(&to_string, Some(value), &[], class, method)? {
                Some(Value::Ref(Some(text))) => Some(text),
                _ => None,
            },
        )
    }

    /// The `instanceof` answers this module and the proxy model add to the
    /// opcode's own: unboxed boxes and proxies.
    pub(super) fn library_instance_of(&self, value: &Value, wanted: &str) -> bool {
        unboxed_instance_of(value, wanted)
            || expect_ref(value).is_some_and(|marker| {
                self.proxy_instance_of(marker, wanted)
                    || crate::value_class::string_instance_of(marker, wanted)
            })
    }

    /// `value instanceof wanted`, by the same rules the `instanceof` opcode
    /// applies.
    pub(super) fn value_instance_of(&self, value: &Value, wanted: &str) -> bool {
        self.library_instance_of(value, wanted)
            || expect_ref(value).is_some_and(|marker| {
                self.object_is_instance_of(marker, wanted)
                    || self.host.host_instance_of(marker, wanted)
                    || crate::jdk_model::platform_instance_of(marker, wanted)
                    || generic_type_instance_of(marker, wanted)
                    || (wanted == "java/lang/Class"
                        && class_marker_like(marker, self.classes, &self.linked))
            })
    }

    /// `Class.isInstance(value)` and `Class.cast(value)` with the
    /// `instanceof` opcode's rules. The host had no model for either, so
    /// `Map.class.isInstance(map)` was always false and a config library
    /// refused its own default ("must be an instance of the return type").
    fn invoke_class_is_instance(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
        args: &[Value],
    ) -> Result<Option<Value>, NativeError> {
        if r.owner != "java/lang/Class" {
            return Ok(None);
        }
        let wanted = match (r.name.as_str(), r.descriptor.as_str()) {
            ("isInstance", "(Ljava/lang/Object;)Z")
            | ("cast", "(Ljava/lang/Object;)Ljava/lang/Object;") => {
                reflection_class_name(receiver, r)?.replace('.', "/")
            }
            _ => return Ok(None),
        };
        let value = args.first().cloned().unwrap_or(Value::Ref(None));
        let is_null = matches!(value, Value::Ref(None));
        // A primitive class has no instances; `int.class.isInstance(1)` is
        // false on the JVM too.
        let instance = !is_null
            && !PRIMITIVE_CLASSES.contains(&wanted.as_str())
            && self.value_instance_of(&value, &wanted);
        if r.name == "isInstance" {
            return Ok(Some(Value::Int(i32::from(instance))));
        }
        if is_null || instance {
            return Ok(Some(value));
        }
        // Only a class the runtime can judge throws; anything else keeps the
        // value, as the opcode-level `checkcast` does.
        if self.class(&wanted).is_some() {
            return Err(NativeError::UncaughtException {
                class: "java/lang/ClassCastException".into(),
                message: Some(format!(
                    "Cannot cast {} to {}",
                    describe_value(&value),
                    wanted.replace('/', ".")
                )),
                frame: None,
            });
        }
        Ok(Some(value))
    }

    /// `Class.getModule()` and the `java.lang.Module` queries access checks
    /// make.
    ///
    /// A mod's classes (and a library it bundles) load in the class loader's
    /// unnamed module, which exports and opens every package to everyone and
    /// sits in no layer; the JDK's `java.*` classes are in the named
    /// `java.base`, which exports its `java.*` packages. DazzleConf checks
    /// `type.getModule().isExported(type.getPackageName(), self)` on every
    /// config return type once it finds `java.lang.Module`, and refused the
    /// config ("inaccessible return type boolean") on the null module the host
    /// answered. A primitive or array type is reported in `java.base`, as the
    /// JVM does.
    fn invoke_module(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
        args: &[Value],
    ) -> Option<Value> {
        if r.owner == "java/lang/Class"
            && r.name == "getModule"
            && r.descriptor == "()Ljava/lang/Module;"
        {
            let class = expect_ref(receiver?)?;
            let java_base = class.starts_with("java/")
                || class.starts_with('[')
                || PRIMITIVE_CLASSES.contains(&class.as_str());
            return Some(Value::Ref(Some(
                if java_base && !self.has_class(class) {
                    JAVA_BASE_MODULE
                } else {
                    UNNAMED_MODULE
                }
                .into(),
            )));
        }
        // A primitive's package is `java.lang` and an array's is its
        // element's, as `Class.getPackageName` documents; the host's name
        // split answered "" for both.
        if r.owner == "java/lang/Class"
            && r.name == "getPackageName"
            && r.descriptor == "()Ljava/lang/String;"
        {
            let class = expect_ref(receiver?)?;
            let element = class.trim_start_matches('[');
            let element = if class.starts_with('[') {
                erased_field_type(element)
            } else {
                element.to_owned()
            };
            if PRIMITIVE_CLASSES.contains(&element.as_str()) {
                return Some(Value::Ref(Some("java.lang".into())));
            }
            if class.starts_with('[') {
                let package = element.rsplit_once('/').map_or("", |(package, _)| package);
                return Some(Value::Ref(Some(package.replace('/', "."))));
            }
            return None;
        }
        if r.owner != "java/lang/Module" {
            return None;
        }
        let module = expect_ref(receiver?)?.clone();
        let named = module == JAVA_BASE_MODULE;
        if !named && module != UNNAMED_MODULE {
            return None;
        }
        Some(match (r.name.as_str(), r.descriptor.as_str()) {
            ("isNamed", "()Z") => Value::Int(i32::from(named)),
            ("getName", "()Ljava/lang/String;") => {
                Value::Ref(named.then(|| "java.base".to_owned()))
            }
            ("getLayer", "()Ljava/lang/ModuleLayer;") => {
                Value::Ref(named.then(|| "module-layer/boot".to_owned()))
            }
            ("getDescriptor", "()Ljava/lang/module/ModuleDescriptor;") => Value::Ref(None),
            ("getClassLoader", "()Ljava/lang/ClassLoader;") => {
                Value::Ref((!named).then(|| "classloader/bootstrap".to_owned()))
            }
            (
                "isExported" | "isOpen",
                "(Ljava/lang/String;)Z" | "(Ljava/lang/String;Ljava/lang/Module;)Z",
            ) => {
                // `java.base` exports `java.*`; it opens none of them to an
                // unnamed module.
                let package = args.first().and_then(expect_ref).map(String::as_str);
                Value::Int(i32::from(if named {
                    r.name == "isExported"
                        && package.is_some_and(|package| package.starts_with("java."))
                } else {
                    true
                }))
            }
            ("canRead", "(Ljava/lang/Module;)Z") => Value::Int(1),
            ("toString", "()Ljava/lang/String;") => Value::Ref(Some(
                if named {
                    "module java.base"
                } else {
                    "unnamed module"
                }
                .into(),
            )),
            _ => return None,
        })
    }

    /// A jar subclass of `java.io.FilterInputStream` reads through the
    /// stream it wraps.
    ///
    /// The super constructor stores the wrapped stream in the protected `in`
    /// field, and every `read`/`skip`/`available`/`close` the subclass does
    /// not override forwards to it, as the JDK's `FilterInputStream` does.
    /// The host answered those calls with a neutral `0`, so a subclass's
    /// `readFully` loop (emotecraft's `LittleEndianInputStream`) never saw
    /// end of stream and ran until its budget.
    fn invoke_filter_stream(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
        args: &[Value],
        class: &str,
        method: &str,
    ) -> Result<Option<Value>, NativeError> {
        const FILTER: &str = "java/io/FilterInputStream";
        const IN_FIELD: &str = "#inLjava/io/InputStream;";
        let Some(marker) = receiver.and_then(expect_ref).cloned() else {
            return Ok(None);
        };
        let Some((receiver_class, _)) = marker.split_once('#') else {
            return Ok(None);
        };
        let receiver_class = receiver_class.to_owned();
        // The JDK's own filter streams forward the same way; a buffer size
        // changes nothing a bounded read can observe.
        let jdk_filter = matches!(
            r.owner.as_str(),
            "java/io/BufferedInputStream"
                | "java/io/DataInputStream"
                | "java/io/PushbackInputStream"
        );
        if (r.owner == FILTER || jdk_filter)
            && r.name == "<init>"
            && matches!(
                r.descriptor.as_str(),
                "(Ljava/io/InputStream;)V" | "(Ljava/io/InputStream;I)V"
            )
        {
            // `new BufferedInputStream(in)` builds the JDK class itself; a
            // `FilterInputStream` super call comes from a jar subclass.
            if !(jdk_filter && receiver_class == r.owner) && !self.has_class(&receiver_class) {
                return Ok(None);
            }
            let wrapped = args.first().cloned().unwrap_or(Value::Ref(None));
            // The JVM's field is `FilterInputStream.in`; a subclass's own
            // `getfield in` resolves under its own name here, so both keys
            // hold the stream.
            self.fields
                .insert(format!("{marker}|{FILTER}{IN_FIELD}"), wrapped.clone());
            self.fields
                .insert(format!("{marker}|{receiver_class}{IN_FIELD}"), wrapped);
            return Ok(Some(Value::Void));
        }
        let forwarded = matches!(
            (r.name.as_str(), r.descriptor.as_str()),
            ("read", "()I" | "([B)I" | "([BII)I")
                | ("skip", "(J)J")
                | ("available", "()I")
                | ("close" | "mark" | "reset", "()V" | "(I)V")
                | ("markSupported", "()Z")
                | ("readAllBytes" | "readNBytes", "()[B")
        );
        if !forwarded {
            return Ok(None);
        }
        let Some(mut wrapped) = self
            .fields
            .get(&format!("{marker}|{FILTER}{IN_FIELD}"))
            .cloned()
        else {
            return Ok(None);
        };
        // A body the subclass declares (or inherits from a jar class) runs
        // instead; only the JDK's own forwarding is modelled here.
        if self
            .member_owner(&receiver_class, &r.name, &r.descriptor)
            .is_some_and(|(_, jar_owned)| jar_owned)
        {
            return Ok(None);
        }
        // Filters stack (`new Reader(new BufferedInputStream(resource))`):
        // forward through every JDK filter until a stream that reads itself.
        for _ in 0..MAX_FILTER_CHAIN {
            let Some(next) = expect_ref(&wrapped)
                .and_then(|inner| self.fields.get(&format!("{inner}|{FILTER}{IN_FIELD}")))
                .cloned()
            else {
                break;
            };
            let wrapped_class = expect_ref(&wrapped)
                .and_then(|inner| inner.split_once('#'))
                .map(|(class, _)| class.to_owned())
                .unwrap_or_default();
            if self
                .member_owner(&wrapped_class, &r.name, &r.descriptor)
                .is_some_and(|(_, jar_owned)| jar_owned)
            {
                break;
            }
            wrapped = next;
        }
        let Value::Ref(Some(_)) = wrapped else {
            return Err(NativeError::UncaughtException {
                class: "java/lang/NullPointerException".into(),
                message: None,
                frame: None,
            });
        };
        let inner = MethodRef {
            owner: "java/io/InputStream".into(),
            name: r.name.clone(),
            descriptor: r.descriptor.clone(),
        };
        if let Some(value) = self.invoke_io(&inner, Some(&wrapped), args, class, method)? {
            return Ok(Some(value));
        }
        if let Some(value) =
            self.invoke_byte_streams(&inner, Some(&wrapped), args, class, method)?
        {
            return Ok(Some(value));
        }
        // A wrapped jar stream runs its own body.
        if let Some(wrapped_class) = expect_ref(&wrapped)
            .and_then(|marker| marker.split_once('#'))
            .map(|(class, _)| class.to_owned())
            && let Some((owner, true)) = self.member_owner(&wrapped_class, &r.name, &r.descriptor)
        {
            return self
                .invoke(&owner, &r.name, &r.descriptor, Some(wrapped), args)
                .map(Some);
        }
        self.host.invoke_instance(&inner, &wrapped, args).map(Some)
    }

    /// Byte-exact reads into runtime-owned arrays: a host resource stream
    /// (`getResourceAsStream`) through `readAllBytes`/`readNBytes`/
    /// `read(byte[]...)`, and a `ByteArrayInputStream` over a runtime array.
    ///
    /// The host's stream table holds a resource's bytes but cannot write a
    /// runtime array, so these reads used to fail as missing methods; a
    /// `ByteArrayInputStream` built over a jar's own `byte[]` was opaque to the
    /// host. The latter keeps the JDK's own fields (`buf`, `pos`, `count`) on
    /// the object, so a jar subclass reading them sees the same state.
    fn invoke_byte_streams(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
        args: &[Value],
        class: &str,
        method: &str,
    ) -> Result<Option<Value>, NativeError> {
        const BAIS: &str = "java/io/ByteArrayInputStream";
        if !matches!(
            r.owner.as_str(),
            "java/io/InputStream" | BAIS | "java/io/FilterInputStream"
        ) {
            return Ok(None);
        }
        let Some(marker) = receiver.and_then(expect_ref).cloned() else {
            return Ok(None);
        };
        let field = |name: &str, descriptor: &str| format!("{marker}|{BAIS}#{name}{descriptor}");
        if r.owner == BAIS && r.name == "<init>" {
            let Some(array) = args.first().and_then(expect_ref).cloned() else {
                return Ok(None);
            };
            let Some(length) = self.arrays.get(&array).map(Vec::len) else {
                return Ok(None);
            };
            let (offset, count) = match r.descriptor.as_str() {
                "([B)V" => (0, length),
                "([BII)V" => {
                    let offset = args.get(1).and_then(boxed_integer).unwrap_or(0).max(0) as usize;
                    let count = args.get(2).and_then(boxed_integer).unwrap_or(0).max(0) as usize;
                    (offset.min(length), offset.saturating_add(count).min(length))
                }
                _ => return Ok(None),
            };
            self.fields
                .insert(field("buf", "[B"), Value::Ref(Some(array)));
            self.fields
                .insert(field("pos", "I"), Value::Int(offset as i32));
            self.fields
                .insert(field("mark", "I"), Value::Int(offset as i32));
            self.fields
                .insert(field("count", "I"), Value::Int(count as i32));
            return Ok(Some(Value::Void));
        }
        // What the stream yields next: the array-backed stream's slice, or the
        // host resource stream's bytes.
        let array_stream = match self.fields.get(&field("buf", "[B")) {
            Some(Value::Ref(Some(array))) => Some(array.clone()),
            _ => None,
        };
        let take = |this: &mut Self, limit: Option<usize>| -> Option<Vec<u8>> {
            match &array_stream {
                Some(array) => {
                    let pos = match this.fields.get(&field("pos", "I")) {
                        Some(Value::Int(pos)) => (*pos).max(0) as usize,
                        _ => 0,
                    };
                    let count = match this.fields.get(&field("count", "I")) {
                        Some(Value::Int(count)) => (*count).max(0) as usize,
                        _ => 0,
                    };
                    let values = this.arrays.get(array)?;
                    let end = limit.map_or(count, |limit| pos.saturating_add(limit).min(count));
                    let bytes = values
                        .get(pos.min(end)..end)
                        .unwrap_or_default()
                        .iter()
                        .map(|value| match value {
                            Value::Int(byte) => *byte as u8,
                            _ => 0,
                        })
                        .collect::<Vec<_>>();
                    this.fields
                        .insert(field("pos", "I"), Value::Int(end.max(pos) as i32));
                    Some(bytes)
                }
                None => this.host.resource_stream_bytes(&marker, limit),
            }
        };
        let value = match (r.name.as_str(), r.descriptor.as_str()) {
            ("readAllBytes", "()[B") | ("readNBytes", "()[B") => {
                let Some(bytes) = take(self, None) else {
                    return Ok(None);
                };
                self.io_bytes_array(bytes, class, method)
            }
            ("readNBytes", "(I)[B") => {
                let wanted = args.first().and_then(boxed_integer).unwrap_or(0);
                if wanted < 0 {
                    return Err(NativeError::UncaughtException {
                        class: "java/lang/IllegalArgumentException".into(),
                        message: Some("len < 0".into()),
                        frame: None,
                    });
                }
                let Some(bytes) = take(self, Some(wanted as usize)) else {
                    return Ok(None);
                };
                self.io_bytes_array(bytes, class, method)
            }
            ("read", "()I") if array_stream.is_some() => {
                let bytes = take(self, Some(1)).unwrap_or_default();
                Value::Int(bytes.first().map_or(-1, |byte| i32::from(*byte)))
            }
            ("available", "()I") if array_stream.is_some() => {
                let pos = match self.fields.get(&field("pos", "I")) {
                    Some(Value::Int(pos)) => *pos,
                    _ => 0,
                };
                let count = match self.fields.get(&field("count", "I")) {
                    Some(Value::Int(count)) => *count,
                    _ => 0,
                };
                Value::Int((count - pos).max(0))
            }
            ("read", "([B)I" | "([BII)I") => {
                let Some(target) = args.first().and_then(expect_ref).cloned() else {
                    return Err(NativeError::UncaughtException {
                        class: "java/lang/NullPointerException".into(),
                        message: None,
                        frame: None,
                    });
                };
                let Some(capacity) = self.arrays.get(&target).map(Vec::len) else {
                    return Ok(None);
                };
                let (offset, length) = if r.descriptor == "([B)I" {
                    (0, capacity)
                } else {
                    let offset = args.get(1).and_then(boxed_integer).unwrap_or(0);
                    let length = args.get(2).and_then(boxed_integer).unwrap_or(0);
                    if offset < 0 || length < 0 || offset as usize + length as usize > capacity {
                        return Err(NativeError::UncaughtException {
                            class: "java/lang/IndexOutOfBoundsException".into(),
                            message: None,
                            frame: None,
                        });
                    }
                    (offset as usize, length as usize)
                };
                if length == 0 {
                    Value::Int(0)
                } else {
                    let Some(bytes) = take(self, Some(length)) else {
                        return Ok(None);
                    };
                    if bytes.is_empty() {
                        Value::Int(-1)
                    } else {
                        let read = bytes.len();
                        if let Some(slots) = self.arrays.get_mut(&target) {
                            for (slot, byte) in slots[offset..offset + read].iter_mut().zip(bytes) {
                                *slot = Value::Int(i32::from(byte as i8));
                            }
                        }
                        Value::Int(read as i32)
                    }
                }
            }
            ("close", "()V") if array_stream.is_some() => Value::Void,
            _ => return Ok(None),
        };
        Ok(Some(value))
    }

    /// `Collection.toArray` into a runtime-owned array, and
    /// `Arrays.asList` over one.
    ///
    /// The two meet in every "copy this collection" helper (DazzleConf's
    /// `ImmutableCollections.listOf(Collection)` is
    /// `unmodifiableList(asList(c.toArray().clone()))`). The host held no
    /// `toArray` and kept the whole array as `asList`'s single element, so
    /// the copy lost every element. `asList` answers a list holding the
    /// array's elements at the time of the call; a later write through the
    /// view does not reach the array (the list is a copy, not the JVM's
    /// write-through view).
    fn invoke_array_views(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
        args: &[Value],
    ) -> Result<Option<Value>, NativeError> {
        if r.owner == "java/util/Arrays"
            && r.name == "asList"
            && r.descriptor == "([Ljava/lang/Object;)Ljava/util/List;"
        {
            let Some(array) = args.first().filter(|value| {
                expect_ref(value).is_some_and(|marker| self.arrays.contains_key(marker))
            }) else {
                return Ok(None);
            };
            let values = self.array(array)?.clone();
            return Ok(Some(collection_value("list", &values)));
        }
        if r.name != "toArray"
            || !is_collection_owner(&r.owner)
            || is_map_owner(&r.owner)
            || !matches!(
                r.descriptor.as_str(),
                "()[Ljava/lang/Object;"
                    | "([Ljava/lang/Object;)[Ljava/lang/Object;"
                    | "(Ljava/util/function/IntFunction;)[Ljava/lang/Object;"
            )
        {
            return Ok(None);
        }
        let Some(receiver) = receiver else {
            return Ok(None);
        };
        let Ok(items) = runtime_collection_values(&*self.host, receiver) else {
            return Ok(None);
        };
        let values = items
            .iter()
            .map(|item| parse_collection_value(item))
            .collect::<Result<Vec<_>, _>>()?;
        // `toArray(T[] a)` fills `a` when it is large enough and nulls the
        // slot after the last element, as the JDK's `AbstractCollection` does.
        if r.descriptor == "([Ljava/lang/Object;)[Ljava/lang/Object;"
            && let Some(target) = args.first().and_then(expect_ref).cloned()
            && let Some(slots) = self.arrays.get_mut(&target)
            && slots.len() >= values.len()
        {
            let length = values.len();
            for (slot, value) in slots.iter_mut().zip(values) {
                *slot = value;
            }
            if let Some(after) = slots.get_mut(length) {
                *after = Value::Ref(None);
            }
            return Ok(Some(Value::Ref(Some(target))));
        }
        let marker = self.new_array("java/lang/Object", "toArray");
        self.arrays.insert(marker.clone(), values);
        Ok(Some(Value::Ref(Some(marker))))
    }

    /// `Throwable.getStackTrace()` / `Thread.getStackTrace()` and the
    /// `StackTraceElement`s they hand out, from the interpreter's own jar
    /// frames, innermost first.
    ///
    /// Loggers name their caller from a fixed index (voxy's
    /// `new Throwable().getStackTrace()[2]`); the empty array the host
    /// answered made that an `ArrayIndexOutOfBoundsException`. A native run
    /// has no Java frames below the entrypoint, so frames past the jar's own
    /// are not invented. `Thread.getStackTrace` lists its own JDK frame
    /// first, as the JVM does. Line numbers are not tracked (`-1`, the JVM's
    /// "unknown"), and the file name is null.
    fn invoke_stack_trace(&mut self, r: &MethodRef, receiver: Option<&Value>) -> Option<Value> {
        if r.owner == "java/lang/StackTraceElement" {
            let marker = receiver.and_then(expect_ref)?;
            let rest = marker.strip_prefix(STACK_TRACE_ELEMENT_PREFIX)?;
            let (method, class) = rest.split_once('@')?;
            let binary = class.replace('/', ".");
            return Some(match (r.name.as_str(), r.descriptor.as_str()) {
                ("getClassName", "()Ljava/lang/String;") => Value::Ref(Some(binary)),
                ("getMethodName", "()Ljava/lang/String;") => Value::Ref(Some(method.to_owned())),
                ("getLineNumber", "()I") => Value::Int(-1),
                ("getFileName" | "getModuleName" | "getModuleVersion", "()Ljava/lang/String;") => {
                    Value::Ref(None)
                }
                ("isNativeMethod", "()Z") => Value::Int(0),
                ("toString", "()Ljava/lang/String;") => {
                    Value::Ref(Some(format!("{binary}.{method}(Unknown Source)")))
                }
                _ => return None,
            });
        }
        if r.name != "getStackTrace" || r.descriptor != "()[Ljava/lang/StackTraceElement;" {
            return None;
        }
        let thread = r.owner == "java/lang/Thread";
        // A jar class that declares its own `getStackTrace` keeps its body.
        if !thread
            && self
                .class(&r.owner)
                .is_some_and(|file| file.method(&r.name, &r.descriptor).is_some())
        {
            return None;
        }
        if !thread && !r.owner.starts_with("java/lang/") && self.class(&r.owner).is_none() {
            return None;
        }
        let mut frames = Vec::new();
        if thread {
            frames.push(stack_trace_element("java/lang/Thread", "getStackTrace"));
        }
        frames.extend(
            crate::stack_walk::snapshot()
                .iter()
                .map(|(class, method)| stack_trace_element(class, method)),
        );
        let array = self.new_array("java/lang/StackTraceElement", "getStackTrace");
        self.arrays.insert(array.clone(), frames);
        Some(Value::Ref(Some(array)))
    }

    /// `Paths.get(first, more...)` / `Path.of(first, more...)`: the parts
    /// joined with the separator, skipping empty ones, which is what the
    /// default filesystem's `getPath` does. The `String[]` is a runtime-owned
    /// array, so the host's single-argument model could not answer it and a
    /// null path reached the mod (emotecraft resolves its config folder from
    /// `Paths.get("")`).
    fn invoke_varargs_path(
        &mut self,
        r: &MethodRef,
        args: &[Value],
    ) -> Result<Option<Value>, NativeError> {
        if !matches!(
            (r.owner.as_str(), r.name.as_str()),
            ("java/nio/file/Paths", "get") | ("java/nio/file/Path", "of")
        ) || r.descriptor != "(Ljava/lang/String;[Ljava/lang/String;)Ljava/nio/file/Path;"
        {
            return Ok(None);
        }
        let first = match args.first() {
            Some(Value::Ref(Some(text))) => text.clone(),
            _ => {
                return Err(NativeError::UncaughtException {
                    class: "java/lang/NullPointerException".into(),
                    message: None,
                    frame: None,
                });
            }
        };
        let more = match args.get(1) {
            Some(value @ Value::Ref(Some(_))) => self.array(value)?.clone(),
            _ => Vec::new(),
        };
        let mut parts = vec![first];
        for part in more {
            match part {
                Value::Ref(Some(text)) => parts.push(text),
                _ => {
                    return Err(NativeError::UncaughtException {
                        class: "java/lang/NullPointerException".into(),
                        message: None,
                        frame: None,
                    });
                }
            }
        }
        let joined = parts
            .iter()
            .filter(|part| !part.is_empty())
            .map(|part| part.trim_end_matches('/'))
            .collect::<Vec<_>>()
            .join("/");
        Ok(Some(path_value(&joined)))
    }

    /// Whether `owner` (a jar class or interface) inherits an abstract
    /// declaration of `name descriptor` from one of its superinterfaces.
    ///
    /// `invokeinterface IWriter.getExtension()` names an interface that only
    /// extends `ISerializer`, which declares the method abstract. The JVM's
    /// method resolution searches the superinterfaces, so the call is a
    /// virtual dispatch on the receiver; without this the walk stopped at
    /// `Object` and the host answered a neutral null for a body the receiver
    /// ships. The search is breadth first and bounded by
    /// [`MAX_SUPERCLASS_WALK`] interfaces.
    pub(super) fn inherits_abstract(&self, owner: &str, name: &str, descriptor: &str) -> bool {
        let mut queue = std::collections::VecDeque::new();
        let mut current = owner.to_owned();
        for _ in 0..MAX_SUPERCLASS_WALK {
            let Some(file) = self.class(&current) else {
                break;
            };
            queue.extend(file.interface_names());
            if file.super_name.is_empty() || file.super_name == current {
                break;
            }
            current = file.super_name.clone();
        }
        let mut seen = std::collections::BTreeSet::new();
        while let Some(interface) = queue.pop_front() {
            if seen.len() >= MAX_SUPERCLASS_WALK {
                break;
            }
            if !seen.insert(interface.clone()) {
                continue;
            }
            let Some(file) = self.class(&interface) else {
                continue;
            };
            if let Some(method) = file.method(name, descriptor) {
                return method.code.is_none() && !method.is_static();
            }
            queue.extend(file.interface_names());
        }
        false
    }

    /// `Files.exists`/`isDirectory`/`isRegularFile`/`list`/`walk` under a
    /// mod container's root (`ModContainer.getRootPaths()`), answered from the
    /// running jar's own class entries.
    ///
    /// A real Fabric root is the jar's zip filesystem, and config scanners
    /// walk it to find their own classes (voxy lists `me/cortex/voxy/**` for
    /// config types). The native runtime never opens the jar file again, so
    /// the class table it already parsed is the listing. The root names the
    /// container by id only; a path under another mod's root that also matches
    /// the running jar's packages is answered from the running jar, which no
    /// real mod relies on. A path no own class sits under falls through to the
    /// virtual file table unchanged.
    pub(super) fn jar_root_files(
        &mut self,
        r: &MethodRef,
        path: &str,
        class: &str,
        method: &str,
    ) -> Result<Option<Value>, NativeError> {
        let Some(after) = path.strip_prefix(MOD_ROOT_PREFIX) else {
            return Ok(None);
        };
        let (root_id, rest) = after.split_once('/').unwrap_or((after, ""));
        if root_id.is_empty() {
            return Ok(None);
        }
        let rest = rest.trim_matches('/');
        // The root itself is also a name under the mods folder
        // (`getModsDir().resolve("x.jar")`), so only a path inside it is
        // answered here.
        if rest.is_empty() {
            return Ok(None);
        }
        let entries = self
            .classes
            .keys()
            .map(|name| format!("{name}.class"))
            .collect::<Vec<_>>();
        let under = |entry: &str| entry.starts_with(&format!("{rest}/"));
        let is_file = entries.iter().any(|entry| entry == rest);
        let is_directory = entries.iter().any(|entry| under(entry));
        if !is_file && !is_directory {
            return Ok(None);
        }
        let root = format!("{MOD_ROOT_PREFIX}{root_id}");
        let full = |relative: &str| format!("{root}/{relative}");
        let descriptor = r.descriptor.as_str();
        if !descriptor.starts_with("(Ljava/nio/file/Path;") {
            return Ok(None);
        }
        Ok(Some(match r.name.as_str() {
            "exists" => Value::Int(1),
            "notExists" => Value::Int(0),
            "isDirectory" => Value::Int(i32::from(is_directory)),
            "isRegularFile" => Value::Int(i32::from(is_file)),
            "isReadable" => Value::Int(1),
            "list" | "walk" if descriptor.ends_with(")Ljava/util/stream/Stream;") => {
                // `list` yields the direct children, `walk` the start path
                // and every descendant, directories included, in sorted order.
                // The root itself is never answered here (see above).
                let mut found = std::collections::BTreeSet::new();
                for entry in entries.iter().filter(|entry| under(entry)) {
                    let relative = &entry[rest.len() + 1..];
                    let parts = relative.split('/').collect::<Vec<_>>();
                    let depth = if r.name == "list" { 1 } else { parts.len() };
                    for end in 1..=depth.min(parts.len()) {
                        let child = parts[..end].join("/");
                        found.insert(format!("{rest}/{child}"));
                    }
                    if found.len() > crate::jdk_model::MAX_SPLITERATOR_ELEMENTS {
                        return Err(NativeError::Metadata(format!(
                            "Files.{} under {path} holds more than {} entries",
                            r.name,
                            crate::jdk_model::MAX_SPLITERATOR_ELEMENTS
                        )));
                    }
                }
                let mut values = Vec::with_capacity(found.len() + 1);
                if r.name == "walk" {
                    values.push(path_value(&full(rest)));
                }
                values.extend(found.iter().map(|relative| path_value(&full(relative))));
                self.new_stream(class, method, values)
            }
            _ => return Ok(None),
        }))
    }
}

/// The text of the jar resource the host's virtual filesystem answers for
/// `path` (`Files.exists` checks the same table), opened the way
/// `ClassLoader.getResourceAsStream` opens it; `None` when there is none.
pub(super) fn resource_text<H: Host + ?Sized>(host: &mut H, path: &str) -> Option<String> {
    let name = path.trim_start_matches('/');
    let stream = host
        .invoke_instance(
            &MethodRef {
                owner: "java/lang/ClassLoader".into(),
                name: "getResourceAsStream".into(),
                descriptor: "(Ljava/lang/String;)Ljava/io/InputStream;".into(),
            },
            &Value::Ref(Some("classloader/bootstrap".into())),
            &[Value::Ref(Some(name.to_owned()))],
        )
        .ok()?;
    host.resource_stream_text(expect_ref(&stream)?)
}

/// Whether `Class.forName` finds `internal` in the JDK the game runs on.
///
/// Feature probes load a JDK class by name and branch on the
/// `ClassNotFoundException` (DazzleConf asks for `java.lang.Module` to decide
/// whether the module system exists). The game requires Java 25, so the
/// `java.*` and `javax.*` packages are present; a probe for a JDK-internal
/// package (`sun.*`, `jdk.internal.*`) keeps the not-found answer, as a
/// mod's class loader would see it without `--add-opens`.
pub(super) fn jdk_class_exists(internal: &str) -> bool {
    (internal.starts_with("java/") || internal.starts_with("javax/"))
        && !internal.is_empty()
        && internal.split('/').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '$' || c == '_')
        })
}

/// `toString()` on a box the runtime keeps as a primitive value, typed as
/// `Object` or `Number` at the call site: `Double.toString`/`Long.toString`/
/// `Integer.toString` of the value. The host answered the text `null`.
fn unboxed_to_string(r: &MethodRef, receiver: Option<&Value>) -> Option<Value> {
    if r.name != "toString"
        || r.descriptor != "()Ljava/lang/String;"
        || !matches!(
            r.owner.as_str(),
            "java/lang/Object" | "java/lang/Number" | "java/lang/Comparable"
        )
    {
        return None;
    }
    Some(Value::Ref(Some(match receiver? {
        Value::Float(number) => format_java_float(*number),
        Value::Long(number) => number.to_string(),
        Value::Int(number) => number.to_string(),
        _ => return None,
    })))
}

/// One boxed answer unboxed to `descriptor`, with fastutil's default return
/// value (zero, `false`) for null.
fn unbox_primitive(descriptor: &str, value: &Value) -> Value {
    match descriptor {
        "J" => Value::Long(boxed_long(value).unwrap_or(0)),
        "D" | "F" => Value::Float(boxed_float(value).unwrap_or(0.0)),
        "Z" => Value::Int(i32::from(boxed_boolean(value).unwrap_or(false))),
        "I" | "S" | "B" | "C" => Value::Int(boxed_integer(value).unwrap_or(0)),
        _ => value.clone(),
    }
}

/// The `java.util` collection kind (`map`, `list`, `set`) a fastutil
/// collection class implements, from its name; a fastutil iterator, function
/// or comparator is none of them.
pub(crate) fn fastutil_kind(class: &str) -> Option<&'static str> {
    let name = class.strip_prefix("it/unimi/dsi/fastutil/")?;
    let simple = name.rsplit('/').next().unwrap_or(name);
    // Nested entry, view and iterator types (`Object2IntMap$Entry`) are not
    // collections themselves.
    if simple.contains('$') {
        return None;
    }
    if simple.ends_with("Map") {
        Some("map")
    } else if simple.ends_with("List") {
        Some("list")
    } else if simple.ends_with("Set") {
        Some("set")
    } else {
        None
    }
}

/// `instanceof` for the fastutil collections the game ships
/// (`Object2IntOpenHashMap`, `IntArrayList`, `ObjectOpenHashSet`...). Every
/// fastutil map implements `java.util.Map`, every list `java.util.List` and
/// every set `java.util.Set`, boxed for primitive keys; the kind is in the
/// class name.
pub(crate) fn fastutil_instance_of(class: &str, wanted: &str) -> bool {
    let Some(kind) = fastutil_kind(class) else {
        return false;
    };
    let supertypes: &[&str] = match kind {
        "map" => &[
            "java/util/Map",
            "java/util/AbstractMap",
            "java/io/Serializable",
            "java/lang/Cloneable",
        ],
        "list" => &[
            "java/util/List",
            "java/util/Collection",
            "java/lang/Iterable",
            "java/util/SequencedCollection",
            "java/util/RandomAccess",
            "java/io/Serializable",
            "java/lang/Cloneable",
        ],
        _ => &[
            "java/util/Set",
            "java/util/Collection",
            "java/lang/Iterable",
            "java/io/Serializable",
            "java/lang/Cloneable",
        ],
    };
    class == wanted || supertypes.contains(&wanted)
}

/// `instanceof` on a box the runtime keeps as a primitive value.
///
/// The operand of `instanceof` is always a reference, so a `Float` value there
/// is a boxed `Double` (or `Float`), a `Long` a boxed `Long` and an `Int` a
/// boxed `Integer`; the boxing calls answer these unboxed (`Double.valueOf`).
/// A `float` box stores an already-rounded value, so `Float` is claimed only
/// for a value a `float` holds exactly. DazzleConf's number serialiser asks
/// `value instanceof Number` on a `@DefaultDouble` and reported the default
/// as null.
pub(super) fn unboxed_instance_of(value: &Value, wanted: &str) -> bool {
    let shared = matches!(
        wanted,
        "java/lang/Object"
            | "java/lang/Number"
            | "java/lang/Comparable"
            | "java/io/Serializable"
            | "java/lang/constant/Constable"
            | "java/lang/constant/ConstantDesc"
    );
    match value {
        Value::Float(number) => {
            shared
                || wanted == "java/lang/Double"
                || (wanted == "java/lang/Float"
                    && (number.is_nan() || f64::from(*number as f32) == *number))
        }
        Value::Long(_) => shared || wanted == "java/lang/Long",
        Value::Int(_) => shared || wanted == "java/lang/Integer",
        _ => false,
    }
}

/// Filter streams one read forwards through before the chain is treated as
/// malformed; a real stack of wrappers is two or three deep.
const MAX_FILTER_CHAIN: usize = 16;

/// The class markers of the primitive types and `void`.
const PRIMITIVE_CLASSES: &[&str] = &[
    "boolean", "byte", "char", "short", "int", "long", "float", "double", "void",
];

/// The class loader's unnamed module every jar class is in.
const UNNAMED_MODULE: &str = "module/unnamed";
/// The JDK's `java.base` module.
const JAVA_BASE_MODULE: &str = "module/java.base";

/// Prefix of one `StackTraceElement`: `stack-trace-element/<method>@<class>`.
const STACK_TRACE_ELEMENT_PREFIX: &str = "stack-trace-element/";

fn stack_trace_element(class: &str, method: &str) -> Value {
    Value::Ref(Some(format!(
        "{STACK_TRACE_ELEMENT_PREFIX}{method}@{class}"
    )))
}

/// Where `ModContainer.getRootPaths()` puts each mod's root in the virtual
/// filesystem.
pub(super) const MOD_ROOT_PREFIX: &str = "/horizon/mods/";
