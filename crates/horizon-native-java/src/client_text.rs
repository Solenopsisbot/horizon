//! Styles on the client surface: `Style`, `TextColor` and the
//! `ChatFormatting` constants a client callback styles text with.
//!
//! A component's own style lives in the interpreter's component model
//! (`facade_text.rs`): `withStyle(ChatFormatting)`, `withColor(int)` and
//! `append` build vanilla's component JSON there, `component/json/<json>`.
//! What that model leaves to the host is the style *objects*, and those are
//! answered here: `Style.EMPTY` and every derived style is
//! `client/style/<codes>`, its formatting kept as legacy codes (an RGB
//! colour as the client-only `§#rrggbb`); `TextColor.fromRgb(rgb)` /
//! `fromLegacyFormat(f)` is `client/text-color/<code>`. A component styled
//! with one (`withStyle(style)`, `setStyle(style)`) becomes the same JSON
//! component the façade builds, with the style's keys set.
//!
//! `ChatFormatting.<NAME>.toString()` is its code (`§6`), so
//! `ChatFormatting.GOLD + "Day"` is a legacy-coded string, which every client
//! drawing path formats by.

use crate::{MethodRef, Value, expect_ref, return_desc};

use super::{flag, int_arg, marker};

const FORMATTING: &str = "net/minecraft/ChatFormatting";
const FORMATTING_MARKER: &str = "enum/net/minecraft/ChatFormatting/";
const STYLE: &str = "client/style/";
const TEXT_COLOR: &str = "client/text-color/";

/// `ChatFormatting`: name, code, colour (RGB) for the colours.
const FORMATS: &[(&str, char, Option<u32>)] = &[
    ("BLACK", '0', Some(0x000000)),
    ("DARK_BLUE", '1', Some(0x0000aa)),
    ("DARK_GREEN", '2', Some(0x00aa00)),
    ("DARK_AQUA", '3', Some(0x00aaaa)),
    ("DARK_RED", '4', Some(0xaa0000)),
    ("DARK_PURPLE", '5', Some(0xaa00aa)),
    ("GOLD", '6', Some(0xffaa00)),
    ("GRAY", '7', Some(0xaaaaaa)),
    ("DARK_GRAY", '8', Some(0x555555)),
    ("BLUE", '9', Some(0x5555ff)),
    ("GREEN", 'a', Some(0x55ff55)),
    ("AQUA", 'b', Some(0x55ffff)),
    ("RED", 'c', Some(0xff5555)),
    ("LIGHT_PURPLE", 'd', Some(0xff55ff)),
    ("YELLOW", 'e', Some(0xffff55)),
    ("WHITE", 'f', Some(0xffffff)),
    ("OBFUSCATED", 'k', None),
    ("BOLD", 'l', None),
    ("STRIKETHROUGH", 'm', None),
    ("UNDERLINE", 'n', None),
    ("ITALIC", 'o', None),
    ("RESET", 'r', None),
];

fn format_named(name: &str) -> Option<&'static (&'static str, char, Option<u32>)> {
    FORMATS.iter().find(|(n, ..)| *n == name)
}

/// The legacy code of a `ChatFormatting` marker (`§6`).
fn formatting_code(value: &Value) -> Option<String> {
    let name = expect_ref(value)?.strip_prefix(FORMATTING_MARKER)?;
    format_named(name).map(|(_, code, _)| format!("§{code}"))
}

/// The code an RGB colour is written as.
fn rgb_code(rgb: i32) -> String {
    format!("§#{:06x}", rgb as u32 & 0xff_ffff)
}

/// The codes a style argument stands for: a `ChatFormatting`, a style, a
/// text colour or an `int` colour.
fn codes_of(value: &Value) -> Option<String> {
    if let Value::Int(rgb) = value {
        return Some(rgb_code(*rgb));
    }
    let text = expect_ref(value)?;
    if let Some(codes) = text.strip_prefix(STYLE) {
        return Some(codes.to_owned());
    }
    if let Some(code) = text.strip_prefix(TEXT_COLOR) {
        return Some(code.to_owned());
    }
    formatting_code(value)
}

/// A boxed `Boolean` argument (`withBold(true)` boxes it).
fn boxed_true(value: Option<&Value>) -> Option<bool> {
    match value? {
        Value::Int(v) => Some(*v != 0),
        Value::Ref(Some(text)) => text.strip_prefix("boolean:").map(|v| v == "true"),
        _ => None,
    }
}

/// A component with a style's `codes` applied over its own style, as
/// `MutableComponent.withStyle(Style)` merges them (`Style.applyTo`): the
/// codes' keys win.
fn styled(component: &Value, codes: &str) -> Option<Value> {
    use serde_json::{Map, Value as Json, json};
    let mut object = match crate::facade_text::to_json(component)? {
        Json::Object(object) => object,
        Json::String(text) => Map::from_iter([("text".to_owned(), Json::String(text))]),
        _ => return None,
    };
    let mut chars = codes.chars();
    while let Some(c) = chars.next() {
        if c != '§' {
            continue;
        }
        let Some(code) = chars.next() else { break };
        if code == '#' {
            let hex: String = chars.by_ref().take(6).collect();
            object.insert(
                "color".into(),
                json!(format!("#{}", hex.to_ascii_uppercase())),
            );
            continue;
        }
        let key = match code {
            'l' => "bold",
            'o' => "italic",
            'n' => "underlined",
            'm' => "strikethrough",
            'k' => "obfuscated",
            'r' => {
                for key in [
                    "color",
                    "bold",
                    "italic",
                    "underlined",
                    "strikethrough",
                    "obfuscated",
                ] {
                    object.remove(key);
                }
                continue;
            }
            colour => {
                if let Some((name, ..)) = FORMATS
                    .iter()
                    .find(|(_, c, rgb)| *c == colour && rgb.is_some())
                {
                    object.insert("color".into(), json!(name.to_ascii_lowercase()));
                }
                continue;
            }
        };
        object.insert(key.into(), json!(true));
    }
    Some(crate::facade_text::from_json(&Json::Object(object)))
}

/// Static fields: `ChatFormatting.<NAME>` and `Style.EMPTY`.
pub(super) fn text_static(r: &MethodRef) -> Option<Value> {
    match (r.owner.as_str(), r.name.as_str()) {
        (FORMATTING, name) if format_named(name).is_some() => {
            Some(marker(format!("{FORMATTING_MARKER}{name}")))
        }
        ("net/minecraft/network/chat/Style", "EMPTY") => Some(marker(STYLE)),
        _ => None,
    }
}

/// Static calls: `TextColor.fromRgb`, `TextColor.fromLegacyFormat`,
/// `ChatFormatting.getByName`/`getByCode`.
pub(super) fn text_invoke(r: &MethodRef, args: &[Value]) -> Option<Value> {
    match (r.owner.as_str(), r.name.as_str()) {
        ("net/minecraft/network/chat/TextColor", "fromRgb") => Some(marker(format!(
            "{TEXT_COLOR}{}",
            rgb_code(int_arg(args, 0)?)
        ))),
        ("net/minecraft/network/chat/TextColor", "fromLegacyFormat") => Some(marker(format!(
            "{TEXT_COLOR}{}",
            formatting_code(args.first()?)?
        ))),
        (FORMATTING, "getByName") => {
            let name = expect_ref(args.first()?)?.to_ascii_uppercase();
            Some(match format_named(&name) {
                Some((name, ..)) => marker(format!("{FORMATTING_MARKER}{name}")),
                None => Value::Ref(None),
            })
        }
        (FORMATTING, "getByCode") => {
            let code = char::from_u32(int_arg(args, 0)? as u32)?.to_ascii_lowercase();
            Some(match FORMATS.iter().find(|(_, c, _)| *c == code) {
                Some((name, ..)) => marker(format!("{FORMATTING_MARKER}{name}")),
                None => Value::Ref(None),
            })
        }
        _ => None,
    }
}

/// Instance calls on components, styles, text colours and formatting
/// constants; `None` when the receiver is none of them or the member is not
/// listed.
pub(super) fn text_call(object: &str, r: &MethodRef, args: &[Value]) -> Option<Value> {
    let name = r.name.as_str();
    let returns = return_desc(&r.descriptor);
    if object.starts_with("component/") {
        // `withStyle(ChatFormatting)` and `withColor(int)` are the
        // interpreter's (`facade_text.rs`); a style object is ours.
        return match (name, returns) {
            ("withStyle" | "setStyle", _) => {
                let codes = expect_ref(args.first()?)?.strip_prefix(STYLE)?;
                styled(&marker(object), codes)
            }
            ("getStyle", _) => Some(marker(STYLE)),
            _ => None,
        };
    }
    if let Some(codes) = object.strip_prefix(STYLE) {
        let add = |extra: Option<String>| Some(marker(format!("{STYLE}{codes}{}", extra?)));
        return match (name, returns) {
            ("withColor", _) => add(codes_of(args.first()?)),
            ("applyFormat" | "applyLegacyFormat", _) => add(formatting_code(args.first()?)),
            ("withBold", _) => add(Some(
                if boxed_true(args.first())? { "§l" } else { "" }.into(),
            )),
            ("withItalic", _) => add(Some(
                if boxed_true(args.first())? { "§o" } else { "" }.into(),
            )),
            ("withUnderlined", _) => add(Some(
                if boxed_true(args.first())? { "§n" } else { "" }.into(),
            )),
            ("withStrikethrough", _) => add(Some(
                if boxed_true(args.first())? { "§m" } else { "" }.into(),
            )),
            ("withObfuscated", _) => add(Some(
                if boxed_true(args.first())? { "§k" } else { "" }.into(),
            )),
            ("isEmpty", "Z") => Some(flag(codes.is_empty())),
            _ => None,
        };
    }
    if object.starts_with(TEXT_COLOR) {
        return match (name, returns) {
            ("getValue", "I") => {
                let code = object.strip_prefix(TEXT_COLOR)?;
                let rgb = match code.strip_prefix("§#") {
                    Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                    None => {
                        let c = code.chars().nth(1)?;
                        FORMATS.iter().find(|(_, k, _)| *k == c)?.2?
                    }
                };
                Some(Value::Int(rgb as i32))
            }
            _ => None,
        };
    }
    let format = object
        .strip_prefix(FORMATTING_MARKER)
        .and_then(format_named)?;
    let (fname, code, colour) = *format;
    match (name, returns) {
        ("toString", _) => Some(marker(format!("§{code}"))),
        ("getChar", "C") => Some(Value::Int(code as i32)),
        ("getName" | "getSerializedName", _) => Some(marker(fname.to_ascii_lowercase())),
        ("isColor", "Z") => Some(flag(colour.is_some())),
        ("isFormat", "Z") => Some(flag(colour.is_none() && code != 'r')),
        // A boxed `Integer`, null for a format.
        ("getColor", _) => Some(match colour {
            Some(rgb) => marker(format!("integer:{rgb}")),
            None => Value::Ref(None),
        }),
        _ => None,
    }
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

    #[test]
    fn a_style_object_styles_a_component_as_vanilla_json() {
        let with_color = method(
            "net/minecraft/network/chat/Style",
            "withColor",
            "(I)Lnet/minecraft/network/chat/Style;",
        );
        let style = text_call(STYLE, &with_color, &[Value::Int(0x33ebff)]).unwrap();
        assert_eq!(style, marker("client/style/§#33ebff"));
        let bold = text_call(
            expect_ref(&style).unwrap(),
            &method(
                "net/minecraft/network/chat/Style",
                "withBold",
                "(Ljava/lang/Boolean;)Lnet/minecraft/network/chat/Style;",
            ),
            &[marker("boolean:true")],
        )
        .unwrap();
        let with_style = method(
            "net/minecraft/network/chat/MutableComponent",
            "withStyle",
            "(Lnet/minecraft/network/chat/Style;)Lnet/minecraft/network/chat/MutableComponent;",
        );
        let styled = text_call("component/literal/Day 3", &with_style, &[bold]).unwrap();
        let json: serde_json::Value = serde_json::from_str(
            expect_ref(&styled)
                .unwrap()
                .strip_prefix("component/json/")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({"text": "Day 3", "color": "#33EBFF", "bold": true})
        );
        // `ChatFormatting.GOLD + "x"` concatenates its code.
        let gold = text_static(&method(
            FORMATTING,
            "GOLD",
            "Lnet/minecraft/ChatFormatting;",
        ))
        .unwrap();
        let to_string = method(FORMATTING, "toString", "()Ljava/lang/String;");
        assert_eq!(
            text_call(expect_ref(&gold).unwrap(), &to_string, &[]),
            Some(marker("§6"))
        );
    }
}
