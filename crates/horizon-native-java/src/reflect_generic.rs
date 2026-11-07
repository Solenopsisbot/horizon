//! Generic and annotated reflection over a class's and a method's
//! `Signature` attributes: `Class.getGenericSuperclass`/`getGenericInterfaces`,
//! `Method.getReturnType`/`getGenericReturnType`/`getParameterTypes`/
//! `getGenericParameterTypes`/`getAnnotatedReturnType`, and the
//! `AnnotatedType` nodes the last one answers.
//!
//! Config libraries build their whole model from these: DazzleConf reads each
//! config interface method's annotated return type to pick a serializer, and
//! Guava's `TypeToken` subclass idiom (`new TypeToken<List<String>>() {}`)
//! reads the anonymous class's generic superclass. The answers reuse the
//! `java.lang.reflect.Type` markers `Field.getGenericType` already builds, so a
//! raw type is its class marker and a parameterized one decodes through the
//! same surface.
//!
//! Type-use annotations are not read: an `AnnotatedType` answers an empty
//! annotation array, which is what the JVM answers for a type nobody
//! annotated. A method or class with no `Signature` attribute answers the
//! erased type, exactly as the JVM does.

use super::*;
use crate::jdk_model::class_access_flags;

/// Prefix of an `AnnotatedType` marker; the rest is the `Type` marker it
/// wraps (a class name or a `type/` node).
pub(crate) const ANNOTATED_TYPE_PREFIX: &str = "annotated-type/";

impl ClassFile {
    /// The class's own generic `Signature` attribute, when it carries one.
    pub(crate) fn class_signature(&self) -> Option<String> {
        self.read_signatures(None)
    }

    /// One method's generic `Signature` attribute, when it carries one.
    pub(crate) fn method_signature(&self, name: &str, descriptor: &str) -> Option<String> {
        self.read_signatures(Some((name, descriptor)))
    }

    /// Walk the raw class file to the class attributes (`method == None`) or
    /// to one method's attributes, and read its `Signature`. A malformed file
    /// answers `None`, so the caller falls back to the erased type.
    fn read_signatures(&self, method: Option<(&str, &str)>) -> Option<String> {
        let mut r = Reader::new(&self.raw);
        if r.u4().ok()? != 0xCAFE_BABE {
            return None;
        }
        r.skip(4).ok()?;
        let pool = read_pool(&mut r).ok()?;
        r.skip(6).ok()?; // access, this, super
        let interfaces = r.u2().ok()? as usize;
        r.skip(interfaces.checked_mul(2)?).ok()?;
        let fields = r.u2().ok()? as usize;
        for _ in 0..fields {
            r.skip(6).ok()?;
            skip_attributes(&mut r)?;
        }
        let methods = r.u2().ok()? as usize;
        for _ in 0..methods {
            r.skip(2).ok()?;
            let name = utf8(&pool, r.u2().ok()?).ok()?;
            let descriptor = utf8(&pool, r.u2().ok()?).ok()?;
            let matching = method.is_some_and(|(wanted, wanted_descriptor)| {
                name == wanted && descriptor == wanted_descriptor
            });
            if matching {
                return read_signature_attribute(&mut r, &pool);
            }
            skip_attributes(&mut r)?;
        }
        if method.is_some() {
            return None;
        }
        read_signature_attribute(&mut r, &pool)
    }
}

fn skip_attributes(r: &mut Reader<'_>) -> Option<()> {
    let count = r.u2().ok()? as usize;
    for _ in 0..count {
        r.skip(2).ok()?;
        let length = r.u4().ok()? as usize;
        r.skip(length).ok()?;
    }
    Some(())
}

fn read_signature_attribute(r: &mut Reader<'_>, pool: &[Cp]) -> Option<String> {
    let count = r.u2().ok()? as usize;
    let mut found = None;
    for _ in 0..count {
        let attribute = utf8(pool, r.u2().ok()?).ok()?;
        let length = r.u4().ok()? as usize;
        if attribute == "Signature" && length == 2 && found.is_none() {
            found = utf8(pool, r.u2().ok()?).ok();
        } else {
            r.skip(length).ok()?;
        }
    }
    found
}

/// Skip a `<T:Bound;U::Iface;>` formal type parameter list, if one starts at
/// the parser's position.
fn skip_type_parameters(parser: &mut GenericSignatureParser<'_>) -> Option<()> {
    if parser.peek() != Some(b'<') {
        return Some(());
    }
    parser.pos += 1;
    while parser.peek()? != b'>' {
        // Identifier, then the class bound (may be empty) and interface bounds.
        while parser.peek()? != b':' {
            parser.pos += 1;
        }
        parser.pos += 1;
        // An empty class bound (`T::Ljava/lang/Comparable;`) goes straight
        // to the interface bounds.
        if parser.peek()? != b':' {
            parser.parse_reference()?;
        }
        while parser.peek()? == b':' {
            parser.pos += 1;
            parser.parse_reference()?;
        }
    }
    parser.pos += 1;
    Some(())
}

fn signature_parser(text: &str) -> Option<GenericSignatureParser<'_>> {
    (!text.is_empty() && text.len() <= MAX_GENERIC_SIGNATURE_BYTES).then_some(
        GenericSignatureParser {
            bytes: text.as_bytes(),
            pos: 0,
            depth: 0,
        },
    )
}

/// A class `Signature`: the generic superclass and interfaces.
pub(crate) fn parse_class_signature(text: &str) -> Option<(GenericType, Vec<GenericType>)> {
    let mut parser = signature_parser(text)?;
    skip_type_parameters(&mut parser)?;
    let superclass = parser.parse_reference()?;
    let mut interfaces = Vec::new();
    while parser.pos < parser.bytes.len() {
        if interfaces.len() >= MAX_GENERIC_TYPE_ARGUMENTS {
            return None;
        }
        interfaces.push(parser.parse_reference()?);
    }
    Some((superclass, interfaces))
}

/// A method `Signature`: the generic parameter types and return type
/// (`void` is the class `void`). Thrown types are ignored.
pub(crate) fn parse_method_signature(text: &str) -> Option<(Vec<GenericType>, GenericType)> {
    let mut parser = signature_parser(text)?;
    skip_type_parameters(&mut parser)?;
    if parser.peek()? != b'(' {
        return None;
    }
    parser.pos += 1;
    let mut parameters = Vec::new();
    while parser.peek()? != b')' {
        if parameters.len() >= MAX_GENERIC_TYPE_ARGUMENTS {
            return None;
        }
        parameters.push(parser.parse_java_type()?);
    }
    parser.pos += 1;
    let result = if parser.peek()? == b'V' {
        GenericType::Class("void".into())
    } else {
        parser.parse_java_type()?
    };
    Some((parameters, result))
}

/// The erased class marker of one descriptor (`I` is `int`, `[I` stays the
/// array descriptor, `Ljava/util/List;` is `java/util/List`).
fn erased(descriptor: &str) -> Value {
    Value::Ref(Some(erased_field_type(descriptor)))
}

/// The `(class, name, descriptor)` a `reflect-method/` marker names, when it
/// carries its descriptor.
fn method_marker(value: Option<&Value>) -> Option<(String, String, String)> {
    let marker = value
        .and_then(expect_ref)?
        .strip_prefix("reflect-method/")?;
    let (class, member) = marker.split_once('#')?;
    let at = member.find('(')?;
    Some((
        class.to_owned(),
        member[..at].to_owned(),
        member[at..].to_owned(),
    ))
}

/// The `Type` a type marker (a class name or a `type/` node) names.
fn type_of_marker(marker: &str) -> GenericType {
    decode_generic_type(marker).unwrap_or_else(|| GenericType::Class(marker.to_owned()))
}

fn annotated(value: &GenericType) -> Value {
    let inner = match generic_type_value(value) {
        Value::Ref(Some(marker)) => marker,
        _ => String::new(),
    };
    Value::Ref(Some(format!("{ANNOTATED_TYPE_PREFIX}{inner}")))
}

/// `instanceof` for the `AnnotatedType` markers this module builds.
pub(crate) fn annotated_type_instance_of(marker: &str, wanted: &str) -> bool {
    let Some(inner) = marker.strip_prefix(ANNOTATED_TYPE_PREFIX) else {
        return false;
    };
    match wanted {
        "java/lang/reflect/AnnotatedType" | "java/lang/reflect/AnnotatedElement" => true,
        "java/lang/reflect/AnnotatedParameterizedType" => {
            matches!(type_of_marker(inner), GenericType::Parameterized { .. })
        }
        "java/lang/reflect/AnnotatedWildcardType" => {
            matches!(type_of_marker(inner), GenericType::Wildcard { .. })
        }
        "java/lang/reflect/AnnotatedTypeVariable" => {
            matches!(type_of_marker(inner), GenericType::Variable { .. })
        }
        "java/lang/reflect/AnnotatedArrayType" => inner.starts_with('['),
        _ => false,
    }
}

impl<H: Host> Runtime<'_, H> {
    /// The generic reflection calls this module answers, or `None`.
    pub(super) fn invoke_reflect_generic(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
    ) -> Result<Option<Value>, NativeError> {
        match r.owner.as_str() {
            "java/lang/Class" => self.class_generic(r, receiver),
            "java/lang/reflect/Method" => Ok(self.method_generic(r, receiver)),
            owner if owner.starts_with("java/lang/reflect/Annotated") => {
                Ok(self.annotated_type_member(r, receiver))
            }
            _ => Ok(None),
        }
    }

    fn class_generic(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
    ) -> Result<Option<Value>, NativeError> {
        if r.name == "getModifiers" && r.descriptor == "()I" {
            let class = reflection_class_name(receiver, r)?.replace('.', "/");
            return Ok(self.class_modifiers(&class).map(Value::Int));
        }
        let wants_super =
            r.name == "getGenericSuperclass" && r.descriptor == "()Ljava/lang/reflect/Type;";
        let wants_interfaces =
            r.name == "getGenericInterfaces" && r.descriptor == "()[Ljava/lang/reflect/Type;";
        if !wants_super && !wants_interfaces {
            return Ok(None);
        }
        let class = reflection_class_name(receiver, r)?.replace('.', "/");
        let Some(file) = self.class(&class) else {
            // A class the runtime does not hold keeps the host's answer.
            return Ok(None);
        };
        let interface = class_access_flags(file).is_some_and(|flags| flags & 0x0200 != 0);
        let parsed = file
            .class_signature()
            .as_deref()
            .and_then(parse_class_signature);
        let super_name = file.super_name.clone();
        let interface_names = file.interface_names();
        if wants_super {
            // An interface (and `Object`) has no superclass.
            if interface || super_name.is_empty() {
                return Ok(Some(Value::Ref(None)));
            }
            return Ok(Some(match parsed {
                Some((superclass, _)) => generic_type_value(&superclass),
                None => Value::Ref(Some(super_name)),
            }));
        }
        let types = match parsed {
            Some((_, interfaces)) => interfaces,
            None => interface_names
                .into_iter()
                .map(GenericType::Class)
                .collect(),
        };
        Ok(Some(
            self.generic_type_array("getGenericInterfaces", &types),
        ))
    }

    /// `Class.getDeclaredMethods()` (`declared`) or `Class.getMethods()` as
    /// `Method` markers.
    ///
    /// `getDeclaredMethods` is every method the class declares except its
    /// constructors and static initializer. `getMethods` is the public ones,
    /// plus the public methods its jar superclasses and superinterfaces
    /// declare (a static interface method is not inherited), first declaration
    /// of a signature winning, which is what config libraries walking an
    /// interface hierarchy expect. A private interface helper
    /// (`BreedingCap.getAreaAt`) was listed before and read as a config key.
    /// `Object`'s own methods are not listed, as the class model does not hold
    /// it.
    pub(super) fn reflected_methods(&self, class: &str, declared: bool) -> Vec<Value> {
        let marker = |owner: &str, method: &Method| {
            Value::Ref(Some(format!(
                "reflect-method/{owner}#{}{}",
                method.name, method.descriptor
            )))
        };
        let Some(file) = self.class(class) else {
            return Vec::new();
        };
        let is_member = |method: &Method| method.name != "<init>" && method.name != "<clinit>";
        if declared {
            return file
                .methods
                .iter()
                .filter(|method| is_member(method))
                .map(|method| marker(class, method))
                .collect();
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut out = Vec::new();
        let mut queue = std::collections::VecDeque::from([(class.to_owned(), true)]);
        let mut visited = std::collections::BTreeSet::new();
        while let Some((current, own)) = queue.pop_front() {
            if visited.len() >= MAX_SUPERCLASS_WALK || !visited.insert(current.clone()) {
                continue;
            }
            let Some(file) = self.class(&current) else {
                continue;
            };
            let interface = class_access_flags(file).is_some_and(|flags| flags & 0x0200 != 0);
            for method in &file.methods {
                let public = method.access & 0x0001 != 0;
                // A superinterface's static method is not a member of the
                // implementing class or sub-interface.
                if !is_member(method) || !public || (interface && !own && method.is_static()) {
                    continue;
                }
                if seen.insert(format!("{}{}", method.name, method.descriptor)) {
                    out.push(marker(&current, method));
                }
            }
            if !file.super_name.is_empty() {
                queue.push_back((file.super_name.clone(), false));
            }
            for parent in file.interface_names() {
                queue.push_back((parent, false));
            }
        }
        out
    }

    /// `Class.getModifiers()`: a jar class's access flags without
    /// `ACC_SUPER`, which is what the JVM reports for a top-level class (a
    /// member class's `InnerClasses` flags, such as `static` or `private`, are
    /// not read). A primitive or `void` is `public abstract final`, an array
    /// takes its element's visibility plus `abstract final`, and the common
    /// JDK types answer their declared modifiers. Any other class keeps the
    /// host's answer.
    fn class_modifiers(&self, class: &str) -> Option<i32> {
        const PUBLIC: i32 = 0x0001;
        const FINAL: i32 = 0x0010;
        const INTERFACE: i32 = 0x0200;
        const ABSTRACT: i32 = 0x0400;
        if let Some(element) = class.strip_prefix('[') {
            let element = erased_field_type(element.trim_start_matches('['));
            let visibility = self.class_modifiers(&element).unwrap_or(PUBLIC) & 0x0007;
            return Some(visibility | ABSTRACT | FINAL);
        }
        if matches!(
            class,
            "boolean" | "byte" | "char" | "short" | "int" | "long" | "float" | "double" | "void"
        ) {
            return Some(PUBLIC | ABSTRACT | FINAL);
        }
        if let Some(file) = self.class(class) {
            return class_access_flags(file).map(|flags| i32::from(flags) & !0x0020);
        }
        Some(match class {
            "java/lang/String"
            | "java/lang/Integer"
            | "java/lang/Long"
            | "java/lang/Short"
            | "java/lang/Byte"
            | "java/lang/Character"
            | "java/lang/Boolean"
            | "java/lang/Float"
            | "java/lang/Double"
            | "java/lang/Class"
            | "java/util/UUID"
            | "java/util/Optional" => PUBLIC | FINAL,
            "java/lang/Object" => PUBLIC,
            "java/lang/Number" | "java/lang/Enum" | "java/lang/Record" => PUBLIC | ABSTRACT,
            "java/util/List"
            | "java/util/Map"
            | "java/util/Set"
            | "java/util/Collection"
            | "java/lang/Iterable"
            | "java/lang/CharSequence"
            | "java/lang/Comparable"
            | "java/lang/Runnable" => PUBLIC | INTERFACE | ABSTRACT,
            other if other.starts_with("java/util/function/") => PUBLIC | INTERFACE | ABSTRACT,
            // The game's API a mod's config names (`TextColor`, `EntityType`)
            // is public; whether one is an interface is not known here.
            other if other.starts_with("net/minecraft/") || other.starts_with("com/mojang/") => {
                PUBLIC
            }
            _ => return None,
        })
    }

    fn method_generic(&mut self, r: &MethodRef, receiver: Option<&Value>) -> Option<Value> {
        let (class, name, descriptor) = method_marker(receiver)?;
        let generic = || {
            self.class(&class)
                .and_then(|file| file.method_signature(&name, &descriptor))
                .as_deref()
                .and_then(parse_method_signature)
        };
        let erased_parameters = || {
            descriptor_arguments(&descriptor)
                .iter()
                .map(|parameter| GenericType::Class(erased_field_type(parameter)))
                .collect::<Vec<_>>()
        };
        let erased_return = return_desc(&descriptor).to_owned();
        Some(match (r.name.as_str(), r.descriptor.as_str()) {
            ("getReturnType", "()Ljava/lang/Class;") => erased(&erased_return),
            ("getGenericReturnType", "()Ljava/lang/reflect/Type;") => match generic() {
                Some((_, result)) => generic_type_value(&result),
                None => erased(&erased_return),
            },
            ("getAnnotatedReturnType", "()Ljava/lang/reflect/AnnotatedType;") => match generic() {
                Some((_, result)) => annotated(&result),
                None => annotated(&GenericType::Class(erased_field_type(&erased_return))),
            },
            ("getParameterTypes", "()[Ljava/lang/Class;") => {
                let types = erased_parameters();
                self.generic_type_array("getParameterTypes", &types)
            }
            ("getGenericParameterTypes", "()[Ljava/lang/reflect/Type;") => {
                let types = generic()
                    .map(|(parameters, _)| parameters)
                    .filter(|parameters| {
                        parameters.len() == descriptor_arguments(&descriptor).len()
                    })
                    .unwrap_or_else(erased_parameters);
                self.generic_type_array("getGenericParameterTypes", &types)
            }
            ("getDeclaringClass", "()Ljava/lang/Class;") => Value::Ref(Some(class)),
            ("getModifiers", "()I") => {
                let file = self.class(&class)?;
                let method = file.method(&name, &descriptor)?;
                Value::Int(i32::from(method.access) & 0x0fff)
            }
            ("isDefault", "()Z") => {
                let file = self.class(&class)?;
                let interface = class_access_flags(file).is_some_and(|flags| flags & 0x0200 != 0);
                let method = file.method(&name, &descriptor)?;
                // A default method is a public, non-abstract, non-static
                // instance method of an interface.
                Value::Int(i32::from(
                    interface
                        && method.code.is_some()
                        && !method.is_static()
                        && method.access & 0x0001 != 0,
                ))
            }
            ("isVarArgs", "()Z") => {
                let method = self.class(&class)?.method(&name, &descriptor)?;
                Value::Int(i32::from(method.access & 0x0080 != 0))
            }
            ("isBridge", "()Z") => {
                let method = self.class(&class)?.method(&name, &descriptor)?;
                Value::Int(i32::from(method.access & 0x0040 != 0))
            }
            ("isSynthetic", "()Z") => {
                let method = self.class(&class)?.method(&name, &descriptor)?;
                Value::Int(i32::from(method.access & 0x1000 != 0))
            }
            _ => return None,
        })
    }

    fn annotated_type_member(&mut self, r: &MethodRef, receiver: Option<&Value>) -> Option<Value> {
        let marker = receiver.and_then(expect_ref)?;
        let inner = marker.strip_prefix(ANNOTATED_TYPE_PREFIX)?.to_owned();
        let node = type_of_marker(&inner);
        Some(match (r.name.as_str(), r.descriptor.as_str()) {
            ("getType", "()Ljava/lang/reflect/Type;") => Value::Ref(Some(inner)),
            (
                "getAnnotations" | "getDeclaredAnnotations",
                "()[Ljava/lang/annotation/Annotation;",
            ) => {
                let array = self.new_array("java/lang/annotation/Annotation", "getAnnotations");
                self.arrays.insert(array.clone(), Vec::new());
                Value::Ref(Some(array))
            }
            (
                "getAnnotation" | "getDeclaredAnnotation",
                "(Ljava/lang/Class;)Ljava/lang/annotation/Annotation;",
            ) => Value::Ref(None),
            ("isAnnotationPresent", "(Ljava/lang/Class;)Z") => Value::Int(0),
            ("getAnnotatedActualTypeArguments", "()[Ljava/lang/reflect/AnnotatedType;") => {
                let GenericType::Parameterized { args, .. } = node else {
                    return None;
                };
                let values = args.iter().map(annotated).collect::<Vec<_>>();
                let array = self.new_array(
                    "java/lang/reflect/AnnotatedType",
                    "getAnnotatedActualTypeArguments",
                );
                self.arrays.insert(array.clone(), values);
                Value::Ref(Some(array))
            }
            ("getAnnotatedUpperBounds", "()[Ljava/lang/reflect/AnnotatedType;") => {
                let GenericType::Wildcard { upper, .. } = node else {
                    return None;
                };
                let values = generic_type_bounds(&upper)
                    .iter()
                    .map(annotated)
                    .collect::<Vec<_>>();
                let array =
                    self.new_array("java/lang/reflect/AnnotatedType", "getAnnotatedUpperBounds");
                self.arrays.insert(array.clone(), values);
                Value::Ref(Some(array))
            }
            ("getAnnotatedLowerBounds", "()[Ljava/lang/reflect/AnnotatedType;") => {
                let GenericType::Wildcard { lower, .. } = node else {
                    return None;
                };
                let values = lower.iter().map(annotated).collect::<Vec<_>>();
                let array =
                    self.new_array("java/lang/reflect/AnnotatedType", "getAnnotatedLowerBounds");
                self.arrays.insert(array.clone(), values);
                Value::Ref(Some(array))
            }
            ("toString", "()Ljava/lang/String;") => Value::Ref(Some(generic_type_display(&node))),
            _ => return None,
        })
    }
}
