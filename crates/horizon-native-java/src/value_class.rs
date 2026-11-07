//! The runtime class of the two values the interpreter keeps untyped: a
//! plain string and an array.
//!
//! A `String` is a bare `Value::Ref(Some(text))`, the same shape every runtime
//! and host marker has, so `"x" instanceof String` used to answer the neutral
//! `false`. That stayed invisible until a library model acted on it: Guava's
//! `Preconditions.checkState(validator.test(defaultValue))` with
//! `validator = o -> o instanceof String` refused FarmersDelight's own string
//! defaults ("Config defaults are invalid").
//!
//! An array marker (`array/<class>/<method>/<n>`) names the frame that built
//! it, not its element type, so `array.getClass()` was null and
//! `getComponentType()` of it too. Architectury's
//! `EventFactory.createLoop(T... empty)` reads its event interface exactly
//! that way, and the null reached `Proxy.newProxyInstance` as an NPE. The
//! component a creating instruction names is now kept beside the array, as a
//! field of the array marker, so the resident heap keeps and drops it with
//! the array.

use super::*;

/// Field key suffix (after the array marker) holding an array's component
/// class marker: `java/lang/String`, `int`, `[I`.
const ARRAY_COMPONENT_FIELD: &str = "|[#component";

/// `java.lang.String` and every type it extends or implements.
const STRING_TYPES: &[&str] = &[
    "java/lang/String",
    "java/lang/CharSequence",
    "java/lang/Comparable",
    "java/io/Serializable",
    "java/lang/constant/Constable",
    "java/lang/constant/ConstantDesc",
];

/// Runtime markers with no `/`, `#`, `@` or `:` in them, which the shape rule
/// of [`is_plain_string`] cannot tell from text.
const SEPARATOR_FREE_MARKERS: &[&str] = &[
    "method-handles-lookup",
    "guava-cache-builder",
    "mixin-environment",
    "stack-walker",
];

/// Whether a reference `marker` is a plain `java.lang.String` and `wanted`
/// is one of the types a string is an instance of.
pub(crate) fn string_instance_of(marker: &str, wanted: &str) -> bool {
    STRING_TYPES.contains(&wanted) && is_plain_string(marker)
}

/// Whether a reference's text is a string rather than a runtime or host
/// marker.
///
/// The interpreter cannot know for certain, so this answers from the marker
/// shapes the runtime builds, and leaves a text that looks like one of them
/// a non-string (the previous answer for every text):
/// - a box, a builder, a collection, a path or an enum constant: whatever
///   `Object.getClass` names (`class_of_value`), including `true`/`false`;
/// - an object `<class>#<n>`: a `#` after a non-empty, separator-free class;
/// - a callback, comparator or array class: a leading `@` or `[`;
/// - a marker kind or class name: a `/` whose first segment has no `:`, `.`
///   or space (`list/3`, `registry/minecraft:item`, `demo/Main`), which a
///   resource id like `minecraft:block/stone` does not match.
fn is_plain_string(marker: &str) -> bool {
    if marker.starts_with(['@', '[']) || SEPARATOR_FREE_MARKERS.contains(&marker) {
        return false;
    }
    if let Some((class, _)) = marker.split_once('#')
        && !class.is_empty()
        && !class.contains([':', ' '])
    {
        return false;
    }
    if !marker.contains('#') && class_of_value(&Value::Ref(Some(marker.to_owned()))).is_some() {
        return false;
    }
    if let Some((head, _)) = marker.split_once('/')
        && !head.is_empty()
        && !head.contains([':', '.', ' '])
    {
        return false;
    }
    true
}

/// The class marker of an array whose elements are `component`: the JVM's
/// descriptor form (`[Ljava/lang/String;`, `[I`, `[[I`), which the class
/// surface already reads as an array class.
fn array_class_marker(component: &str) -> String {
    let element = match component {
        "boolean" => "Z".to_owned(),
        "byte" => "B".to_owned(),
        "char" => "C".to_owned(),
        "short" => "S".to_owned(),
        "int" => "I".to_owned(),
        "long" => "J".to_owned(),
        "float" => "F".to_owned(),
        "double" => "D".to_owned(),
        array if array.starts_with('[') => array.to_owned(),
        other => format!("L{};", other.replace('.', "/")),
    };
    format!("[{element}")
}

/// The component class marker of an array class marker, or `None` when the
/// marker is not an array class.
pub(crate) fn component_of_array_class(marker: &str) -> Option<String> {
    let element = marker.strip_prefix('[')?;
    Some(match element {
        "Z" => "boolean".to_owned(),
        "B" => "byte".to_owned(),
        "C" => "char".to_owned(),
        "S" => "short".to_owned(),
        "I" => "int".to_owned(),
        "J" => "long".to_owned(),
        "F" => "float".to_owned(),
        "D" => "double".to_owned(),
        nested if nested.starts_with('[') => nested.to_owned(),
        object => object
            .strip_prefix('L')
            .and_then(|name| name.strip_suffix(';'))?
            .replace('.', "/"),
    })
}

/// The component class marker of a `newarray` element type code.
pub(crate) fn primitive_array_component(atype: u8) -> Option<&'static str> {
    Some(match atype {
        4 => "boolean",
        5 => "char",
        6 => "float",
        7 => "double",
        8 => "byte",
        9 => "short",
        10 => "int",
        11 => "long",
        _ => return None,
    })
}

impl<H: Host> Runtime<'_, H> {
    /// Remember the component class the instruction or call that built
    /// `array` named.
    pub(super) fn record_array_component(&mut self, array: &str, component: &str) {
        self.fields.insert(
            format!("{array}{ARRAY_COMPONENT_FIELD}"),
            Value::Ref(Some(component.to_owned())),
        );
    }

    /// `array.getClass()` for an array whose component is known, and
    /// `Class.getComponentType()` of an array class (null for any other
    /// class, as on the JVM).
    pub(super) fn invoke_array_class(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
    ) -> Option<Value> {
        let marker = receiver.and_then(expect_ref)?;
        match (r.owner.as_str(), r.name.as_str(), r.descriptor.as_str()) {
            ("java/lang/Object", "getClass", "()Ljava/lang/Class;") => {
                let Some(Value::Ref(Some(component))) =
                    self.fields.get(&format!("{marker}{ARRAY_COMPONENT_FIELD}"))
                else {
                    return None;
                };
                Some(Value::Ref(Some(array_class_marker(component))))
            }
            ("java/lang/Class", "getComponentType", "()Ljava/lang/Class;") => {
                Some(Value::Ref(component_of_array_class(marker)))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_a_string_and_runtime_markers_are_not() {
        for text in [
            "farmersdelight:rope",
            "",
            "hello world",
            "#ff0000",
            "minecraft:block/stone",
            "config.json",
        ] {
            assert!(is_plain_string(text), "{text:?} should be a string");
        }
        for marker in [
            "demo/Main#3",
            "list/4",
            "registry/minecraft:item",
            "integer:5",
            "true",
            "path:/horizon/game",
            "string-builder/2",
            "@indy:demo/Main:0:demo/Main#lambda$0()V:",
            "@cmp:natural",
            "[Ljava/lang/String;",
            "method-handles-lookup",
            "java/lang/String",
        ] {
            assert!(
                !is_plain_string(marker),
                "{marker:?} should not be a string"
            );
        }
    }
}
