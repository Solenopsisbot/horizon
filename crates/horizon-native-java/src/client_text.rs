//! Styled chat components on the client surface.
//!
//! A component is a text marker (`component/literal/<text>`,
//! `component/translatable/<key>`); the interpreter's own component model
//! keeps text only and concatenates on `append`. The client keeps a style
//! inside that text as vanilla's legacy formatting codes, which every drawing
//! path already reads (`Font.width` skips them, the HUD and chat format by
//! them): `Component.literal("Day").withStyle(ChatFormatting.GOLD)` is
//! `component/literal/§6Day`, a translatable one keeps its codes before the
//! key (`component/translatable/§6key.example`), and an RGB colour
//! (`withColor(0x33ebff)`, `TextColor.fromRgb`) is the client-only code
//! `§#33ebff`, which the game reads as that colour.
//!
//! What this cannot say: a sibling appended after a styled child inherits the
//! child's codes instead of the parent's style (vanilla gives it the
//! parent's), and `getString()` returns the codes with the text. Both only
//! touch text a mod builds from several differently styled parts.
//!
//! The objects:
//!
//! - `ChatFormatting.<NAME>` is its enum marker; `toString()` is its code
//!   (`§6`), so `ChatFormatting.GOLD + "Day"` is the same styled string;
//! - `Style.EMPTY` and every derived style is `client/style/<codes>`;
//! - `TextColor.fromRgb(rgb)` / `fromLegacyFormat(f)` is
//!   `client/text-color/<code>`.

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

/// A component with `codes` put before its text (a translatable's before
/// its key).
fn styled(component: &str, codes: &str) -> Option<Value> {
    let (kind, text) = component
        .strip_prefix("component/")?
        .split_once('/')
        .unwrap_or(("literal", ""));
    Some(marker(format!("component/{kind}/{codes}{text}")))
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
        return match (name, returns) {
            ("getVisualOrderText", _) => Some(marker(object)),
            ("withStyle" | "withColor" | "setStyle", _) => {
                // `withStyle(ChatFormatting...)` hands an array: its codes
                // are not readable here, so the text keeps its own style.
                let codes = args.first().and_then(codes_of).unwrap_or_default();
                styled(object, &codes)
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

/// A translatable payload's key without the style codes before it, and the
/// codes.
pub fn split_codes(text: &str) -> (&str, &str) {
    let mut end = 0;
    while let Some(after) = text[end..].strip_prefix('§') {
        let len = if after.starts_with('#')
            && after.is_char_boundary(7.min(after.len()))
            && after.len() >= 7
        {
            7
        } else {
            match after.chars().next() {
                Some(c) => c.len_utf8(),
                None => break,
            }
        };
        end += '§'.len_utf8() + len;
    }
    (&text[..end], &text[end..])
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
    fn styles_become_codes_before_the_text() {
        let gold = text_static(&method(
            FORMATTING,
            "GOLD",
            "Lnet/minecraft/ChatFormatting;",
        ))
        .unwrap();
        let with_style = method(
            "net/minecraft/network/chat/MutableComponent",
            "withStyle",
            "(Lnet/minecraft/ChatFormatting;)Lnet/minecraft/network/chat/MutableComponent;",
        );
        assert_eq!(
            text_call(
                "component/literal/Day 3",
                &with_style,
                std::slice::from_ref(&gold)
            ),
            Some(marker("component/literal/§6Day 3"))
        );
        // A translatable keeps its key after the codes.
        assert_eq!(
            text_call(
                "component/translatable/key.a",
                &with_style,
                std::slice::from_ref(&gold)
            ),
            Some(marker("component/translatable/§6key.a"))
        );
        assert_eq!(split_codes("§6§lkey.a"), ("§6§l", "key.a"));
        assert_eq!(split_codes("§#33ebffkey.a"), ("§#33ebff", "key.a"));
        // `ChatFormatting.GOLD + "x"` concatenates its code.
        let to_string = method(FORMATTING, "toString", "()Ljava/lang/String;");
        let gold_name = expect_ref(&gold).unwrap().clone();
        assert_eq!(text_call(&gold_name, &to_string, &[]), Some(marker("§6")));
        // An RGB colour through a style.
        let with_color = method(
            "net/minecraft/network/chat/Style",
            "withColor",
            "(I)Lnet/minecraft/network/chat/Style;",
        );
        assert_eq!(
            text_call(STYLE, &with_color, &[Value::Int(0x33ebff)]),
            Some(marker("client/style/§#33ebff"))
        );
    }
}
