//! `java.lang.reflect.Proxy` over interfaces the running jar (or a library
//! linked beside it) declares, and the `MethodHandle` subset a proxy's
//! handler uses to run an interface's default methods.
//!
//! Config libraries hand out their settings as proxies: DazzleConf builds one
//! `Proxy.newProxyInstance(loader, new Class[] {MainConfig.class}, handler)`
//! per config section, and every `config.someKey()` the mod calls later is a
//! call on the proxy. The JVM routes such a call to
//! `handler.invoke(proxy, method, args)` with a `Method` for the interface
//! method and the arguments boxed into an `Object[]` (null when there are
//! none); the handler's answer is unboxed to the method's return type. That
//! is what happens here: the `Method` is the same `reflect-method/` marker the
//! reflection surface hands out, so `getName`, `getDeclaringClass`,
//! `getReturnType` and `isDefault` answer from the class file.
//!
//! `Object`'s `equals`, `hashCode` and `toString` on a proxy also go to the
//! handler, with `java.lang.Object` as the declaring class, as on the JVM.
//!
//! Method handles: `MethodHandles.lookup()`/`privateLookupIn`,
//! `Lookup.unreflect`/`unreflectSpecial`, `MethodHandle.bindTo` and
//! `invokeWithArguments`. A special handle runs the named class's own body
//! (a default method on the proxy), never a virtual override. Other lookups
//! (`findVirtual` and friends) are not modelled.

use super::*;

/// The class of every proxy object; markers are `jdk/proxy/$Proxy#<n>`.
const PROXY_CLASS: &str = "jdk/proxy/$Proxy";
/// Interfaces one proxy may implement; `newProxyInstance` itself refuses
/// more than 65535, a real config section names one.
const MAX_PROXY_INTERFACES: usize = 64;
/// The `Lookup` marker `MethodHandles.lookup()` and `privateLookupIn` answer.
const LOOKUP_MARKER: &str = "method-handles-lookup";
/// Prefix of a method handle marker:
/// `method-handle/<special|virtual>/<bound receiver or ->/<method marker>`.
const HANDLE_PREFIX: &str = "method-handle/";

fn proxy_field(marker: &str, name: &str) -> String {
    format!("{marker}|{PROXY_CLASS}#{name}")
}

fn is_proxy(marker: &str) -> bool {
    marker
        .strip_prefix(PROXY_CLASS)
        .is_some_and(|rest| rest.starts_with('#'))
}

fn npe(message: Option<&str>) -> NativeError {
    NativeError::UncaughtException {
        class: "java/lang/NullPointerException".into(),
        message: message.map(str::to_owned),
        frame: None,
    }
}

impl<H: Host> Runtime<'_, H> {
    /// The proxy and method-handle calls this module answers, or `None`.
    pub(super) fn invoke_reflect_proxy(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
        args: &[Value],
    ) -> Result<Option<Value>, NativeError> {
        if let Some(marker) = receiver.and_then(expect_ref)
            && is_proxy(marker)
            && r.name != "<init>"
        {
            let marker = marker.clone();
            return self.invoke_on_proxy(&marker, r, args).map(Some);
        }
        match (r.owner.as_str(), r.name.as_str(), r.descriptor.as_str()) {
            (
                "java/lang/reflect/Proxy",
                "newProxyInstance",
                "(Ljava/lang/ClassLoader;[Ljava/lang/Class;Ljava/lang/reflect/InvocationHandler;)Ljava/lang/Object;",
            ) => self.new_proxy(args).map(Some),
            (
                "java/lang/reflect/Proxy",
                "getInvocationHandler",
                "(Ljava/lang/Object;)Ljava/lang/reflect/InvocationHandler;",
            ) => {
                let target = args.first().and_then(expect_ref).cloned();
                match target {
                    Some(marker) if is_proxy(&marker) => Ok(Some(
                        self.fields
                            .get(&proxy_field(&marker, "h"))
                            .cloned()
                            .unwrap_or(Value::Ref(None)),
                    )),
                    _ => Err(NativeError::UncaughtException {
                        class: "java/lang/IllegalArgumentException".into(),
                        message: Some("not a proxy instance".into()),
                        frame: None,
                    }),
                }
            }
            ("java/lang/reflect/Proxy", "isProxyClass", "(Ljava/lang/Class;)Z") => {
                Ok(Some(Value::Int(i32::from(
                    args.first()
                        .and_then(expect_ref)
                        .is_some_and(|class| class == PROXY_CLASS),
                ))))
            }
            _ => self.invoke_method_handles(r, receiver, args),
        }
    }

    /// `Proxy.newProxyInstance`: a fresh proxy object remembering its
    /// interfaces and handler. Every interface must be one the class model
    /// holds, since only those can say which methods the proxy has.
    fn new_proxy(&mut self, args: &[Value]) -> Result<Value, NativeError> {
        let interfaces = match args.get(1) {
            Some(value @ Value::Ref(Some(_))) => self.array(value)?.clone(),
            _ => return Err(npe(Some("interfaces"))),
        };
        let handler = args.get(2).cloned().unwrap_or(Value::Ref(None));
        if matches!(handler, Value::Ref(None)) {
            return Err(npe(Some("h")));
        }
        if interfaces.len() > MAX_PROXY_INTERFACES {
            return Err(NativeError::Metadata(format!(
                "Proxy.newProxyInstance names {} interfaces; the bounded proxy takes at most {MAX_PROXY_INTERFACES}",
                interfaces.len()
            )));
        }
        let mut names = Vec::with_capacity(interfaces.len());
        for interface in &interfaces {
            let Some(name) = expect_ref(interface) else {
                return Err(npe(None));
            };
            let name = name.replace('.', "/");
            let is_interface = self
                .class(&name)
                .and_then(crate::jdk_model::class_access_flags)
                .is_some_and(|flags| flags & 0x0200 != 0);
            if !is_interface {
                // The JVM refuses a class here; an interface outside the class
                // model (a game or JDK interface) has no method table to proxy.
                return Err(if self.class(&name).is_some() {
                    NativeError::UncaughtException {
                        class: "java/lang/IllegalArgumentException".into(),
                        message: Some(format!("{} is not an interface", name.replace('/', "."))),
                        frame: None,
                    }
                } else {
                    NativeError::Metadata(format!(
                        "Proxy.newProxyInstance over {name}, an interface outside the class model"
                    ))
                });
            }
            names.push(Value::Ref(Some(name)));
        }
        let proxy = self.new_object(PROXY_CLASS);
        let marker = expect_ref(&proxy).cloned().unwrap_or_default();
        let list = self.new_array("java/lang/Class", "newProxyInstance");
        self.arrays.insert(list.clone(), names);
        self.fields
            .insert(proxy_field(&marker, "interfaces"), Value::Ref(Some(list)));
        self.fields.insert(proxy_field(&marker, "h"), handler);
        Ok(proxy)
    }

    /// The interfaces a proxy implements, in the order it was built with.
    fn proxy_interfaces(&self, marker: &str) -> Vec<String> {
        let Some(Value::Ref(Some(list))) = self.fields.get(&proxy_field(marker, "interfaces"))
        else {
            return Vec::new();
        };
        self.arrays
            .get(list)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| expect_ref(value).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// `proxy instanceof wanted`: one of its interfaces is (or extends)
    /// `wanted`, or `wanted` is `Proxy` or `Object`.
    pub(super) fn proxy_instance_of(&self, marker: &str, wanted: &str) -> bool {
        if !is_proxy(marker) {
            return false;
        }
        matches!(
            wanted,
            "java/lang/Object" | "java/lang/reflect/Proxy" | "java/io/Serializable"
        ) || self
            .proxy_interfaces(marker)
            .iter()
            .any(|interface| interface == wanted || self.interface_extends(interface, wanted))
    }

    /// The interface that declares `name descriptor` for a proxy: the first
    /// of its interfaces (or their superinterfaces) with that method.
    fn proxy_declaring_interface(
        &self,
        marker: &str,
        name: &str,
        descriptor: &str,
    ) -> Option<String> {
        let mut queue = std::collections::VecDeque::from(self.proxy_interfaces(marker));
        let mut seen = std::collections::BTreeSet::new();
        while let Some(interface) = queue.pop_front() {
            if seen.len() >= MAX_SUPERCLASS_WALK || !seen.insert(interface.clone()) {
                continue;
            }
            let Some(file) = self.class(&interface) else {
                continue;
            };
            if file
                .method(name, descriptor)
                .is_some_and(|method| !method.is_static())
            {
                return Some(interface);
            }
            queue.extend(file.interface_names());
        }
        None
    }

    /// A call on a proxy: `handler.invoke(proxy, method, args)`.
    fn invoke_on_proxy(
        &mut self,
        marker: &str,
        r: &MethodRef,
        args: &[Value],
    ) -> Result<Value, NativeError> {
        let object_method = matches!(
            (r.name.as_str(), r.descriptor.as_str()),
            ("equals", "(Ljava/lang/Object;)Z")
                | ("hashCode", "()I")
                | ("toString", "()Ljava/lang/String;")
        );
        let declaring = if object_method {
            "java/lang/Object".to_owned()
        } else {
            match self.proxy_declaring_interface(marker, &r.name, &r.descriptor) {
                Some(interface) => interface,
                None if r.owner == "java/lang/Object" && r.name == "getClass" => {
                    return Ok(Value::Ref(Some(PROXY_CLASS.into())));
                }
                None => {
                    return Err(NativeError::MissingMethod(format!(
                        "proxy method {}#{}{}",
                        r.owner, r.name, r.descriptor
                    )));
                }
            }
        };
        let method = Value::Ref(Some(format!(
            "reflect-method/{declaring}#{}{}",
            r.name, r.descriptor
        )));
        // The JVM passes null, not an empty array, for a no-argument method.
        let parameters = descriptor_arguments(&r.descriptor);
        let arguments = if parameters.is_empty() {
            Value::Ref(None)
        } else {
            let mut boxed = Vec::with_capacity(args.len());
            for (parameter, value) in parameters.iter().zip(args) {
                boxed.push(self.box_primitive(parameter, value)?);
            }
            let array = self.new_array("java/lang/Object", "proxy");
            self.arrays.insert(array.clone(), boxed);
            Value::Ref(Some(array))
        };
        let handler = self
            .fields
            .get(&proxy_field(marker, "h"))
            .cloned()
            .unwrap_or(Value::Ref(None));
        let call = [Value::Ref(Some(marker.to_owned())), method, arguments];
        let answer = self.call_invocation_handler(&handler, &call)?;
        let result = return_desc(&r.descriptor);
        Ok(match result {
            "V" => Value::Void,
            "Z" | "B" | "C" | "S" | "I" | "J" | "F" | "D" => {
                if matches!(answer, Value::Ref(None)) {
                    // Unboxing the handler's null for a primitive return.
                    return Err(npe(None));
                }
                unbox_to(result, &answer)
            }
            _ => answer,
        })
    }

    /// Run `InvocationHandler.invoke` on a handler: a lambda through the
    /// callback path, a jar object through its own (or inherited) body.
    fn call_invocation_handler(
        &mut self,
        handler: &Value,
        call: &[Value],
    ) -> Result<Value, NativeError> {
        const INVOKE: &str =
            "(Ljava/lang/Object;Ljava/lang/reflect/Method;[Ljava/lang/Object;)Ljava/lang/Object;";
        let class = expect_ref(handler)
            .and_then(|marker| marker.split_once('#'))
            .map(|(class, _)| class.to_owned());
        if let Some(class) = class
            && let Some((owner, true)) = self.member_owner(&class, "invoke", INVOKE)
        {
            return self.invoke(&owner, "invoke", INVOKE, Some(handler.clone()), call);
        }
        if let Some(value) = self.invoke_callback(handler, call)? {
            return Ok(value);
        }
        self.host.invoke_instance(
            &MethodRef {
                owner: "java/lang/reflect/InvocationHandler".into(),
                name: "invoke".into(),
                descriptor: INVOKE.into(),
            },
            handler,
            call,
        )
    }

    /// `MethodHandles.lookup()`, `privateLookupIn`, `Lookup.unreflect`/
    /// `unreflectSpecial`, `MethodHandle.bindTo`/`invokeWithArguments`, and
    /// `Module.addReads` (which the JDK 11 default-method path calls first).
    fn invoke_method_handles(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
        args: &[Value],
    ) -> Result<Option<Value>, NativeError> {
        match (r.owner.as_str(), r.name.as_str(), r.descriptor.as_str()) {
            ("java/lang/invoke/MethodHandles", "lookup" | "publicLookup", _) => {
                Ok(Some(Value::Ref(Some(LOOKUP_MARKER.into()))))
            }
            (
                "java/lang/invoke/MethodHandles",
                "privateLookupIn",
                "(Ljava/lang/Class;Ljava/lang/invoke/MethodHandles$Lookup;)Ljava/lang/invoke/MethodHandles$Lookup;",
            ) => Ok(Some(Value::Ref(Some(LOOKUP_MARKER.into())))),
            ("java/lang/invoke/MethodHandles$Lookup", "in", _) => {
                Ok(Some(Value::Ref(Some(LOOKUP_MARKER.into()))))
            }
            ("java/lang/invoke/MethodHandles$Lookup", "unreflect" | "unreflectSpecial", _) => {
                let Some(method) = args.first().and_then(expect_ref) else {
                    return Err(npe(None));
                };
                if !method.starts_with("reflect-method/") {
                    return Ok(None);
                }
                let kind = if r.name == "unreflectSpecial" {
                    "special"
                } else {
                    "virtual"
                };
                Ok(Some(Value::Ref(Some(format!(
                    "{HANDLE_PREFIX}{kind}/-/{method}"
                )))))
            }
            ("java/lang/Module", "addReads", "(Ljava/lang/Module;)Ljava/lang/Module;") => {
                Ok(receiver.cloned())
            }
            ("java/lang/invoke/MethodHandle", "bindTo", _) => {
                let Some((kind, bound, method)) = receiver.and_then(parse_handle) else {
                    return Ok(None);
                };
                if bound != "-" {
                    return Ok(None);
                }
                let Some(target) = args.first().and_then(expect_ref) else {
                    return Err(npe(None));
                };
                // The bound receiver is an object marker; one with a `/` would
                // make the handle ambiguous to parse, so it is not modelled.
                if target.contains('/') && !is_proxy(target) {
                    return Ok(None);
                }
                Ok(Some(Value::Ref(Some(format!(
                    "{HANDLE_PREFIX}{kind}/{}/{method}",
                    target.replace('/', "\u{1}")
                )))))
            }
            (
                "java/lang/invoke/MethodHandle",
                "invokeWithArguments",
                "([Ljava/lang/Object;)Ljava/lang/Object;",
            ) => {
                let Some((kind, bound, method)) = receiver.and_then(parse_handle) else {
                    return Ok(None);
                };
                let mut values = match args.first() {
                    Some(value @ Value::Ref(Some(_))) => self.array(value)?.clone(),
                    _ => Vec::new(),
                };
                if bound != "-" {
                    values.insert(0, Value::Ref(Some(bound.replace('\u{1}', "/"))));
                }
                self.invoke_handle(&kind, &method, values).map(Some)
            }
            _ => Ok(None),
        }
    }

    /// Run the method a handle names on `values` (the receiver first for an
    /// instance method), unboxed to its parameter types; the answer is boxed
    /// back, as `invokeWithArguments` returns `Object`.
    fn invoke_handle(
        &mut self,
        kind: &str,
        method: &str,
        mut values: Vec<Value>,
    ) -> Result<Value, NativeError> {
        let marker = method.strip_prefix("reflect-method/").unwrap_or(method);
        let Some((class, member)) = marker.split_once('#') else {
            return Err(NativeError::Metadata("malformed method handle".into()));
        };
        let Some(at) = member.find('(') else {
            return Err(NativeError::Metadata(
                "method handle without a descriptor".into(),
            ));
        };
        let (name, descriptor) = (&member[..at], &member[at..]);
        let is_static = self
            .class(class)
            .and_then(|file| file.method(name, descriptor))
            .is_some_and(Method::is_static);
        let receiver = if is_static || values.is_empty() {
            None
        } else {
            Some(values.remove(0))
        };
        let parameters = descriptor_arguments(descriptor);
        if parameters.len() != values.len() {
            return Err(NativeError::UncaughtException {
                class: "java/lang/invoke/WrongMethodTypeException".into(),
                message: Some(format!(
                    "{name}{descriptor} takes {} arguments, got {}",
                    parameters.len(),
                    values.len()
                )),
                frame: None,
            });
        }
        let values = coerce_arguments(descriptor, &values);
        // A special handle runs exactly the named class's body; a virtual one
        // selects the receiver's override, as `invokevirtual` would.
        let owner = if kind == "virtual"
            && let Some(class) = receiver
                .as_ref()
                .and_then(expect_ref)
                .and_then(|marker| marker.split_once('#'))
                .map(|(class, _)| class.to_owned())
            && let Some((owner, true)) = self.member_owner(&class, name, descriptor)
        {
            owner
        } else {
            class.to_owned()
        };
        if receiver
            .as_ref()
            .and_then(expect_ref)
            .is_some_and(|marker| is_proxy(marker))
            && kind == "virtual"
        {
            let marker = receiver
                .as_ref()
                .and_then(expect_ref)
                .cloned()
                .unwrap_or_default();
            let r = MethodRef {
                owner: class.to_owned(),
                name: name.to_owned(),
                descriptor: descriptor.to_owned(),
            };
            let answer = self.invoke_on_proxy(&marker, &r, &values)?;
            return self.box_primitive(return_desc(descriptor), &answer);
        }
        let answer = self.invoke(&owner, name, descriptor, receiver, &values)?;
        let answer = if answer == Value::Void {
            Value::Ref(None)
        } else {
            answer
        };
        self.box_primitive(return_desc(descriptor), &answer)
    }
}

/// `(kind, bound receiver or "-", method marker)` of a handle marker.
fn parse_handle(value: &Value) -> Option<(String, String, String)> {
    let rest = expect_ref(value)?.strip_prefix(HANDLE_PREFIX)?;
    let (kind, rest) = rest.split_once('/')?;
    let (bound, method) = rest.split_once('/')?;
    Some((kind.to_owned(), bound.to_owned(), method.to_owned()))
}

/// One boxed handler answer as the primitive `descriptor` names.
fn unbox_to(descriptor: &str, value: &Value) -> Value {
    match descriptor {
        "J" => boxed_long(value).map_or_else(|| value.clone(), Value::Long),
        "F" | "D" => boxed_float(value).map_or_else(|| value.clone(), Value::Float),
        "Z" => {
            boxed_boolean(value).map_or_else(|| value.clone(), |flag| Value::Int(i32::from(flag)))
        }
        _ => boxed_integer(value).map_or_else(|| value.clone(), Value::Int),
    }
}
