//! A bounded class-file writer and `HEAD`-injection weaver.
//!
//! The interpreter in [`crate`] never needed to write a class: it reads a
//! `ClassFile` and executes a bytecode subset.  This module adds the other
//! direction, for the one shape a native Mixin translation can prepare before
//! an embedded JVM sees it: a straight-line method that gets a `HEAD` callback
//! inserted at offset 0.
//!
//! [`ClassFile::write`] re-encodes a parsed class from the bytes it was parsed from.
//! Every byte range the parser does not need to understand (the constant pool
//! payloads, fields, unknown attributes, trailing bytes) is copied verbatim, so
//! an untouched class round-trips byte-for-byte.  Constant pool entries the
//! writer appends are the only bytes it invents.
//!
//! # What the weaver supports
//!
//! [`weave_head`] inserts, at offset 0 of the target method:
//!
//! ```text
//! aload_0                         (only when the callback takes the receiver)
//! <load every argument from its local slot>
//! invokestatic callback
//! ```
//!
//! It updates `max_stack` conservatively to `original + argument slots + 1`,
//! the `code_length`, and the `Code` attribute length.  Every other attribute,
//! including a `Code` attribute's own `LineNumberTable`, is copied byte-for-byte
//! (their `start_pc` values are not shifted; they are debug data only).
//!
//! The target method must be straight-line: no `if*`, `goto`, `tableswitch`,
//! `lookupswitch`, `jsr`/`ret`, no exception table, no `StackMapTable`, no
//! `wide`, and the class's constant pool must not use `CONSTANT_Dynamic`
//! (condy).  The callback descriptor must consume exactly the target's
//! arguments and return `void`.  An instance target's callback may instead take
//! the target instance as an extra first parameter, which the inserted
//! `aload_0` supplies.
//!
//! # What it does not support
//!
//! [`weave_plans`] applies only `HEAD` plans, in plan order, and skips plans
//! that target another class or ask for cancellation.  `RETURN`/`TAIL`/
//! `INVOKE` plans, branches (so no frame or local-variable rewriting),
//! exception handlers and `CallbackInfo`/`CallbackInfoReturnable` parameters
//! stay on the interpreter path: HEAD-with-`CallbackInfo` needs the interpreter
//! to own the cancellation object, which a bytecode insert cannot create.
//! Non-ASCII constant pool strings, local slots above 255 (`wide`) and code
//! that would exceed the class-file `code_length` limit are rejected with a
//! reason rather than written.

use crate::{ClassFile, Cp, MethodRef, MixinInjectionPlan};

const ACC_STATIC: u16 = 0x0008;
const ACC_ABSTRACT: u16 = 0x0400;
const ACC_NATIVE: u16 = 0x0100;
const MAX_CODE_LENGTH: usize = 65_535;

#[derive(Debug, Clone, Copy)]
struct Bytes<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Bytes<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self
            .at
            .checked_add(n)
            .ok_or_else(|| "class file offset overflow".to_owned())?;
        let slice = self
            .bytes
            .get(self.at..end)
            .ok_or_else(|| format!("class file is truncated at offset {}", self.at))?;
        self.at = end;
        Ok(slice)
    }

    fn skip(&mut self, n: usize) -> Result<(), String> {
        self.bytes(n).map(|_| ())
    }

    fn u1(&mut self) -> Result<u8, String> {
        Ok(self.bytes(1)?[0])
    }

    fn u2(&mut self) -> Result<u16, String> {
        let bytes = self.bytes(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn u4(&mut self) -> Result<u32, String> {
        let bytes = self.bytes(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }
}

/// One `Code` attribute, split at the bytes a rewrite touches.
struct CodeLayout<'a> {
    max_stack: u16,
    max_locals: u16,
    code: &'a [u8],
    /// `exception_table_length` and its entries, copied verbatim.
    exception_table: &'a [u8],
    /// The Code attribute's own attributes (count included), copied verbatim.
    attributes: &'a [u8],
}

struct AttributeLayout<'a> {
    name_index: u16,
    body: &'a [u8],
    code: Option<CodeLayout<'a>>,
}

struct MethodLayout<'a> {
    access: u16,
    name_index: u16,
    descriptor_index: u16,
    attributes: Vec<AttributeLayout<'a>>,
}

/// The class file as ordered byte ranges, so the writer can copy everything it
/// does not mean to change.
struct Layout<'a> {
    /// `magic`, `minor_version`, `major_version`.
    version: &'a [u8],
    pool_count: u16,
    pool: &'a [u8],
    /// `Option<&str>` per constant pool index; `None` for non-`Utf8` entries.
    pool_utf8: Vec<Option<&'a str>>,
    /// Constant pool tags in file order, used to reject tags the weaver cannot
    /// preserve (condy) even though the writer copies them verbatim.
    pool_tags: Vec<u8>,
    /// `access_flags`, `this_class`, `super_class` and the interfaces table.
    after_pool: &'a [u8],
    /// The whole field table, count included.
    fields: &'a [u8],
    methods: Vec<MethodLayout<'a>>,
    /// The whole class attribute table, count included.
    class_attributes: &'a [u8],
    /// Bytes after the class attributes.  Kept so `write` is byte-exact even
    /// for a class file that carries trailing data.
    tail: &'a [u8],
}

impl<'a> Layout<'a> {
    fn parse(bytes: &'a [u8]) -> Result<Self, String> {
        let mut r = Bytes::new(bytes);
        if r.u4()? != 0xCAFE_BABE {
            return Err("class file has bad magic".to_owned());
        }
        r.skip(4)?; // minor_version, major_version
        let version = &bytes[..8];

        let pool_count = r.u2()?;
        let pool_start = r.at;
        let (pool_utf8, pool_tags) = parse_pool(&mut r, pool_count)?;
        let pool = &bytes[pool_start..r.at];

        let after_pool_start = r.at;
        let _access = r.u2()?;
        let _this_class = r.u2()?;
        let _super_class = r.u2()?;
        let interfaces = r.u2()? as usize;
        r.skip(
            interfaces
                .checked_mul(2)
                .ok_or("interfaces count overflow")?,
        )?;
        let after_pool = &bytes[after_pool_start..r.at];

        let fields_start = r.at;
        let field_count = r.u2()? as usize;
        for _ in 0..field_count {
            r.skip(6)?; // access_flags, name_index, descriptor_index
            skip_attributes(&mut r)?;
        }
        let fields = &bytes[fields_start..r.at];

        let method_count = r.u2()? as usize;
        let mut methods = Vec::with_capacity(method_count);
        for _ in 0..method_count {
            let access = r.u2()?;
            let name_index = r.u2()?;
            let descriptor_index = r.u2()?;
            let attribute_count = r.u2()? as usize;
            let mut attributes = Vec::with_capacity(attribute_count);
            for _ in 0..attribute_count {
                let name_index = r.u2()?;
                let length = r.u4()? as usize;
                let body = r.bytes(length)?;
                let code = if pool_utf8.get(name_index as usize).copied().flatten() == Some("Code")
                {
                    Some(parse_code(body)?)
                } else {
                    None
                };
                attributes.push(AttributeLayout {
                    name_index,
                    body,
                    code,
                });
            }
            methods.push(MethodLayout {
                access,
                name_index,
                descriptor_index,
                attributes,
            });
        }

        let class_attribute_start = r.at;
        let class_attribute_count = r.u2()? as usize;
        for _ in 0..class_attribute_count {
            r.skip(2)?; // attribute_name_index
            let length = r.u4()? as usize;
            r.skip(length)?;
        }
        let class_attributes = &bytes[class_attribute_start..r.at];
        let tail = &bytes[r.at..];

        Ok(Self {
            version,
            pool_count,
            pool,
            pool_utf8,
            pool_tags,
            after_pool,
            fields,
            methods,
            class_attributes,
            tail,
        })
    }

    fn utf8(&self, index: u16) -> Option<&'a str> {
        self.pool_utf8.get(index as usize).copied().flatten()
    }

    fn method_name(&self, method: &MethodLayout<'a>) -> Option<&'a str> {
        self.utf8(method.name_index)
    }

    fn method_descriptor(&self, method: &MethodLayout<'a>) -> Option<&'a str> {
        self.utf8(method.descriptor_index)
    }
}

fn skip_attributes(r: &mut Bytes<'_>) -> Result<(), String> {
    let count = r.u2()? as usize;
    for _ in 0..count {
        r.skip(2)?; // attribute_name_index
        let length = r.u4()? as usize;
        r.skip(length)?;
    }
    Ok(())
}

fn parse_pool<'a>(
    r: &mut Bytes<'a>,
    count: u16,
) -> Result<(Vec<Option<&'a str>>, Vec<u8>), String> {
    if count == 0 {
        return Err("class file constant pool count is zero".to_owned());
    }
    let mut utf8 = vec![None; count as usize];
    let mut tags = Vec::new();
    let mut index = 1usize;
    while index < count as usize {
        let offset = r.at;
        let tag = r.u1()?;
        tags.push(tag);
        match tag {
            1 => {
                let length = r.u2()? as usize;
                if let Ok(value) = std::str::from_utf8(r.bytes(length)?) {
                    utf8[index] = Some(value);
                }
            }
            3 | 4 => r.skip(4)?,
            5 | 6 => {
                r.skip(8)?;
                // Long and double take two constant pool slots.
                index += 1;
            }
            7 | 8 | 16 | 19 | 20 => r.skip(2)?,
            9 | 10 | 11 | 12 | 17 | 18 => r.skip(4)?,
            15 => r.skip(3)?,
            other => {
                return Err(format!(
                    "constant pool tag {other} at offset {offset} is not supported by the class writer"
                ));
            }
        }
        index += 1;
    }
    Ok((utf8, tags))
}

fn parse_code(body: &[u8]) -> Result<CodeLayout<'_>, String> {
    let mut r = Bytes::new(body);
    let max_stack = r.u2()?;
    let max_locals = r.u2()?;
    let code_length = r.u4()? as usize;
    let code = r.bytes(code_length)?;
    let exception_start = r.at;
    let handlers = r.u2()? as usize;
    r.skip(
        handlers
            .checked_mul(8)
            .ok_or("exception table length overflow")?,
    )?;
    let exception_table = &body[exception_start..r.at];
    let attributes = &body[r.at..];
    Ok(CodeLayout {
        max_stack,
        max_locals,
        code,
        exception_table,
        attributes,
    })
}

/// One appended constant: only the shapes the weaver interns.
fn encode_constant(out: &mut Vec<u8>, constant: &Cp) -> Result<(), String> {
    match constant {
        Cp::Utf8(value) => {
            if !value.is_ascii() {
                return Err(format!(
                    "constant pool string {value:?} is not ASCII; the bounded writer cannot encode modified UTF-8"
                ));
            }
            let length = u16::try_from(value.len())
                .map_err(|_| "constant pool string is longer than 65535 bytes".to_owned())?;
            out.push(1);
            out.extend_from_slice(&length.to_be_bytes());
            out.extend_from_slice(value.as_bytes());
        }
        Cp::Class(name) => {
            out.push(7);
            out.extend_from_slice(&name.to_be_bytes());
        }
        Cp::NameType { name, descriptor } => {
            out.push(12);
            out.extend_from_slice(&name.to_be_bytes());
            out.extend_from_slice(&descriptor.to_be_bytes());
        }
        Cp::MethodRef { class, nt } => {
            out.push(10);
            out.extend_from_slice(&class.to_be_bytes());
            out.extend_from_slice(&nt.to_be_bytes());
        }
        Cp::InterfaceMethodRef { class, nt } => {
            out.push(11);
            out.extend_from_slice(&class.to_be_bytes());
            out.extend_from_slice(&nt.to_be_bytes());
        }
        other => {
            return Err(format!(
                "the class writer can only append Utf8/Class/NameAndType/MethodRef constants, not {other:?}"
            ));
        }
    }
    Ok(())
}

/// Interns the constants a callback reference needs, reusing the class's own
/// pool when it already holds the same entry.
struct PoolBuilder<'a> {
    pool: &'a [Cp],
    added: Vec<Cp>,
}

impl<'a> PoolBuilder<'a> {
    fn new(pool: &'a [Cp]) -> Self {
        Self {
            pool,
            added: Vec::new(),
        }
    }

    fn iter(&self) -> impl Iterator<Item = (usize, &Cp)> {
        self.pool.iter().enumerate().chain(
            self.added
                .iter()
                .enumerate()
                .map(|(i, cp)| (i + self.pool.len(), cp)),
        )
    }

    fn position(&self, mut matches: impl FnMut(&Cp) -> bool) -> Option<u16> {
        self.iter()
            .find(|(_, cp)| matches(cp))
            .and_then(|(index, _)| u16::try_from(index).ok())
    }

    fn push(&mut self, constant: Cp) -> Result<u16, String> {
        let index = self.pool.len() + self.added.len();
        let index =
            u16::try_from(index).map_err(|_| "constant pool has no free entries".to_owned())?;
        self.added.push(constant);
        Ok(index)
    }

    fn utf8(&mut self, value: &str) -> Result<u16, String> {
        if let Some(index) = self.position(|cp| matches!(cp, Cp::Utf8(v) if v == value)) {
            return Ok(index);
        }
        self.push(Cp::Utf8(value.to_owned()))
    }

    fn class(&mut self, name: &str) -> Result<u16, String> {
        let name_index = self.utf8(name)?;
        if let Some(index) = self.position(|cp| matches!(cp, Cp::Class(n) if *n == name_index)) {
            return Ok(index);
        }
        self.push(Cp::Class(name_index))
    }

    fn name_type(&mut self, name: &str, descriptor: &str) -> Result<u16, String> {
        let name_index = self.utf8(name)?;
        let descriptor_index = self.utf8(descriptor)?;
        if let Some(index) = self.position(|cp| {
            matches!(
                cp,
                Cp::NameType { name, descriptor }
                    if *name == name_index && *descriptor == descriptor_index
            )
        }) {
            return Ok(index);
        }
        self.push(Cp::NameType {
            name: name_index,
            descriptor: descriptor_index,
        })
    }

    fn method_ref(&mut self, method: &MethodRef) -> Result<u16, String> {
        let class = self.class(&method.owner)?;
        let nt = self.name_type(&method.name, &method.descriptor)?;
        if let Some(index) = self.position(|cp| match cp {
            Cp::MethodRef { class: c, nt: n } | Cp::InterfaceMethodRef { class: c, nt: n } => {
                *c == class && *n == nt
            }
            _ => false,
        }) {
            return Ok(index);
        }
        self.push(Cp::MethodRef { class, nt })
    }
}

struct Patch {
    method: usize,
    attribute: usize,
    code: Vec<u8>,
    max_stack: u16,
}

fn emit(layout: &Layout<'_>, patch: Option<Patch>, added: &[Cp]) -> Result<Vec<u8>, String> {
    let pool_count = layout.pool_count as usize + added.len();
    let pool_count =
        u16::try_from(pool_count).map_err(|_| "constant pool has no free entries".to_owned())?;
    let mut out = Vec::with_capacity(layout.version.len() + layout.pool.len() + 64);
    out.extend_from_slice(layout.version);
    out.extend_from_slice(&pool_count.to_be_bytes());
    out.extend_from_slice(layout.pool);
    for constant in added {
        encode_constant(&mut out, constant)?;
    }
    out.extend_from_slice(layout.after_pool);
    out.extend_from_slice(layout.fields);
    let method_count = u16::try_from(layout.methods.len())
        .map_err(|_| "class file has too many methods".to_owned())?;
    out.extend_from_slice(&method_count.to_be_bytes());
    for (method_index, method) in layout.methods.iter().enumerate() {
        out.extend_from_slice(&method.access.to_be_bytes());
        out.extend_from_slice(&method.name_index.to_be_bytes());
        out.extend_from_slice(&method.descriptor_index.to_be_bytes());
        let attribute_count = u16::try_from(method.attributes.len())
            .map_err(|_| "method has too many attributes".to_owned())?;
        out.extend_from_slice(&attribute_count.to_be_bytes());
        for (attribute_index, attribute) in method.attributes.iter().enumerate() {
            out.extend_from_slice(&attribute.name_index.to_be_bytes());
            if let (Some(patch), Some(code)) = (patch.as_ref(), attribute.code.as_ref()) {
                if patch.method == method_index && patch.attribute == attribute_index {
                    let body = encode_code(code, &patch.code, patch.max_stack)?;
                    let length = u32::try_from(body.len())
                        .map_err(|_| "Code attribute is longer than 4 GiB".to_owned())?;
                    out.extend_from_slice(&length.to_be_bytes());
                    out.extend_from_slice(&body);
                    continue;
                }
            }
            let length = u32::try_from(attribute.body.len())
                .map_err(|_| "attribute is longer than 4 GiB".to_owned())?;
            out.extend_from_slice(&length.to_be_bytes());
            out.extend_from_slice(attribute.body);
        }
    }
    out.extend_from_slice(layout.class_attributes);
    out.extend_from_slice(layout.tail);
    Ok(out)
}

fn encode_code(code: &CodeLayout<'_>, woven: &[u8], max_stack: u16) -> Result<Vec<u8>, String> {
    if woven.len() > MAX_CODE_LENGTH {
        return Err(format!(
            "method code is longer than the class-file limit of {MAX_CODE_LENGTH} bytes"
        ));
    }
    let length =
        u32::try_from(woven.len()).map_err(|_| "method code is longer than 4 GiB".to_owned())?;
    let mut body =
        Vec::with_capacity(woven.len() + code.exception_table.len() + code.attributes.len() + 12);
    body.extend_from_slice(&max_stack.to_be_bytes());
    body.extend_from_slice(&code.max_locals.to_be_bytes());
    body.extend_from_slice(&length.to_be_bytes());
    body.extend_from_slice(woven);
    body.extend_from_slice(code.exception_table);
    body.extend_from_slice(code.attributes);
    Ok(body)
}

/// Re-encode a class file from its parsed bytes.
///
/// Byte ranges the writer does not rewrite are copied verbatim, so an
/// unmodified class round-trips exactly.  A constant pool tag the writer does
/// not know is an error rather than an emitted byte stream that would not load.
pub(crate) fn write_class(raw: &[u8]) -> Result<Vec<u8>, String> {
    if raw.is_empty() {
        return Err("class file bytes are empty".to_owned());
    }
    let layout = Layout::parse(raw)?;
    emit(&layout, None, &[])
}

/// The load opcode and local-slot width for one argument descriptor.
fn load_opcode(descriptor: &str) -> Result<(u8, u16), String> {
    let Some(first) = descriptor.as_bytes().first() else {
        return Err("empty argument descriptor".to_owned());
    };
    Ok(match first {
        b'Z' | b'B' | b'C' | b'S' | b'I' => (0x15, 1), // iload
        b'J' => (0x16, 2),                             // lload
        b'F' => (0x17, 1),                             // fload
        b'D' => (0x18, 2),                             // dload
        b'L' | b'[' => (0x19, 1),                      // aload
        other => {
            return Err(format!(
                "argument descriptor {descriptor:?} starts with unsupported type {}",
                *other as char
            ));
        }
    })
}

fn opcode_name(opcode: u8) -> &'static str {
    match opcode {
        0x99 => "ifeq",
        0x9a => "ifne",
        0x9b => "iflt",
        0x9c => "ifge",
        0x9d => "ifgt",
        0x9e => "ifle",
        0x9f => "if_icmpeq",
        0xa0 => "if_icmpne",
        0xa1 => "if_icmplt",
        0xa2 => "if_icmpge",
        0xa3 => "if_icmpgt",
        0xa4 => "if_icmple",
        0xa5 => "if_acmpeq",
        0xa6 => "if_acmpne",
        0xa7 => "goto",
        0xa8 => "jsr",
        0xa9 => "ret",
        0xaa => "tableswitch",
        0xab => "lookupswitch",
        0xc4 => "wide",
        0xc6 => "ifnull",
        0xc7 => "ifnonnull",
        0xc8 => "goto_w",
        0xc9 => "jsr_w",
        _ => "unknown",
    }
}

/// Reject every opcode whose control flow a plain insert at offset 0 cannot
/// preserve.
fn scan_straight_line(label: &str, code: &[u8]) -> Result<(), String> {
    let mut at = 0usize;
    while at < code.len() {
        let opcode = code[at];
        let size = match opcode {
            0x00..=0x0f => 1,
            0x10 => 2,
            0x11 => 3,
            0x12 => 2,
            0x13 | 0x14 => 3,
            0x15..=0x19 => 2,
            0x1a..=0x35 => 1,
            0x36..=0x3a => 2,
            0x3b..=0x83 => 1,
            0x84 => 3,
            0x85..=0x98 => 1,
            0x99..=0xa8 | 0xa9..=0xab | 0xc4 | 0xc6..=0xc9 => {
                return Err(format!(
                    "{label} contains {} at offset {at}; HEAD weaving needs a straight-line method (no branches, switches, jsr/ret or wide)",
                    opcode_name(opcode)
                ));
            }
            0xac..=0xb1 => 1,
            0xb2..=0xb8 => 3,
            0xb9 | 0xba => 5,
            0xbb => 3,
            0xbc => 2,
            0xbd => 3,
            0xbe | 0xbf => 1,
            0xc0 | 0xc1 => 3,
            0xc2 | 0xc3 => 1,
            0xc5 => 4,
            other => {
                return Err(format!(
                    "{label} uses unknown or reserved opcode 0x{other:02x} at offset {at}"
                ));
            }
        };
        at = at
            .checked_add(size)
            .ok_or_else(|| "code offset overflow".to_owned())?;
        if at > code.len() {
            return Err(format!(
                "{label} has a truncated instruction at offset {}",
                at - size
            ));
        }
    }
    Ok(())
}

/// Reject a `StackMapTable`, which would describe the code before the insert.
fn reject_frames(layout: &Layout<'_>, label: &str, code: &CodeLayout<'_>) -> Result<(), String> {
    let mut attributes = Bytes::new(code.attributes);
    let count = attributes
        .u2()
        .map_err(|_| format!("{label} has a truncated Code attribute"))?;
    for _ in 0..count {
        let name_index = attributes.u2()?;
        let length = attributes.u4()? as usize;
        attributes.skip(length)?;
        if layout.utf8(name_index) == Some("StackMapTable") {
            return Err(format!(
                "{label} carries a StackMapTable; the bounded weaver does not rewrite stack-map frames"
            ));
        }
    }
    Ok(())
}

fn find_target(class_name: &str, layout: &Layout<'_>, target: &MethodRef) -> Result<usize, String> {
    let matches = layout
        .methods
        .iter()
        .enumerate()
        .filter(|(_, method)| {
            layout.method_name(method) == Some(target.name.as_str())
                && (target.descriptor.is_empty()
                    || layout.method_descriptor(method) == Some(target.descriptor.as_str()))
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Err(format!(
            "class {class_name} has no method {}{}",
            target.name, target.descriptor
        )),
        [index] => Ok(*index),
        many => {
            let mut names = many
                .iter()
                .filter_map(|index| layout.method_descriptor(&layout.methods[*index]))
                .collect::<Vec<_>>();
            names.sort_unstable();
            Err(format!(
                "class {class_name} target {}{} is ambiguous: it declares {}",
                target.name,
                target.descriptor,
                names.join(", ")
            ))
        }
    }
}

/// Check a callback descriptor against the weave shape.
///
/// Returns whether the callback takes the target instance as an extra first
/// parameter (which the inserted `aload_0` supplies).  A static target's
/// callback must take exactly the target's arguments; an instance target's
/// callback may take them with or without the receiver.
fn callback_arguments(
    class_name: &str,
    is_static: bool,
    target_descriptor: &str,
    callback_descriptor: &str,
) -> Result<bool, String> {
    if !crate::valid_method_descriptor(callback_descriptor) {
        return Err(format!(
            "callback descriptor {callback_descriptor:?} is not a method descriptor"
        ));
    }
    let callback_arguments = crate::descriptor_arguments(callback_descriptor);
    if let Some(last) = callback_arguments.last() {
        if crate::is_callback_info_descriptor(last) {
            return Err(format!(
                "callback {callback_descriptor} takes a trailing CallbackInfo; HEAD weaving with CallbackInfo needs the interpreter path"
            ));
        }
    }
    if crate::return_desc(callback_descriptor) != "V" {
        return Err(format!(
            "callback {callback_descriptor} must return void for HEAD weaving"
        ));
    }
    let target_arguments = crate::descriptor_arguments(target_descriptor);
    let descriptor = |receiver: Option<&str>| {
        let mut shape = String::from("(");
        if let Some(receiver) = receiver {
            shape.push('L');
            shape.push_str(receiver);
            shape.push(';');
        }
        for argument in &target_arguments {
            shape.push_str(argument);
        }
        shape.push_str(")V");
        shape
    };
    let plain = descriptor(None);
    if callback_descriptor == plain {
        return Ok(false);
    }
    if is_static {
        return Err(format!(
            "static target expects callback {plain}, got {callback_descriptor}"
        ));
    }
    let with_receiver = descriptor(Some(class_name));
    if callback_descriptor == with_receiver {
        return Ok(true);
    }
    Err(format!(
        "callback descriptor {callback_descriptor} does not match the woven arguments {plain} or {with_receiver}"
    ))
}

/// Insert a `HEAD` callback at offset 0 of `target`.
///
/// `class` must be the class that declares `target` (`target.owner` must equal
/// [`ClassFile::name`]).  The returned bytes are the rewritten class; the input
/// is untouched.
///
/// The callback is invoked with `invokestatic`, so it must be static.  A
/// callback takes the target's arguments in order and returns `void`.  An
/// instance target's callback may take the target instance first, which is
/// what the inserted `aload_0` supplies; a callback that does not declare it
/// gets the arguments only, and no receiver is pushed.
///
/// Errors name the reason a method is outside the subset instead of guessing:
/// branches, switches, `jsr`/`ret`, `wide`, an exception table, a
/// `StackMapTable`, condy, abstract/native methods, a local slot above 255, a
/// mismatched callback descriptor, or `CallbackInfo` parameters.
pub fn weave_head(
    class: &ClassFile,
    target: &MethodRef,
    callback: &MethodRef,
) -> Result<Vec<u8>, String> {
    if target.owner != class.name {
        return Err(format!(
            "target owner {} does not match class {}",
            target.owner, class.name
        ));
    }
    let layout = Layout::parse(&class.raw)?;
    if layout.pool_tags.contains(&17) {
        return Err(format!(
            "{}.{}: class constant pool uses CONSTANT_Dynamic (condy), which the bounded weaver does not rewrite",
            class.name, target.name
        ));
    }
    let method_index = find_target(&class.name, &layout, target)?;
    let method = &layout.methods[method_index];
    let descriptor = layout
        .method_descriptor(method)
        .ok_or_else(|| format!("{}: target has no readable descriptor", class.name))?
        .to_owned();
    let name = layout
        .method_name(method)
        .ok_or_else(|| format!("{}: target has no readable name", class.name))?
        .to_owned();
    if !crate::valid_method_descriptor(&descriptor) {
        return Err(format!(
            "{}.{name}: target descriptor {descriptor:?} is not a method descriptor",
            class.name
        ));
    }
    if method.access & (ACC_ABSTRACT | ACC_NATIVE) != 0 {
        return Err(format!(
            "{}.{name}{descriptor}: abstract or native method has no Code to weave",
            class.name
        ));
    }
    let attribute_index = method
        .attributes
        .iter()
        .position(|attribute| attribute.code.is_some())
        .ok_or_else(|| {
            format!(
                "{}.{name}{descriptor}: method has no Code attribute",
                class.name
            )
        })?;
    let code = method.attributes[attribute_index]
        .code
        .as_ref()
        .ok_or_else(|| "missing Code attribute".to_owned())?;

    let label = format!("{}.{name}{descriptor}", class.name);
    let handlers = u16::from_be_bytes([code.exception_table[0], code.exception_table[1]]);
    if handlers != 0 {
        return Err(format!(
            "{label} declares {handlers} exception handler(s); the bounded weaver needs a method with no exception table"
        ));
    }
    reject_frames(&layout, &label, code)?;
    scan_straight_line(&label, code.code)?;

    let is_static = method.access & ACC_STATIC != 0;
    let takes_receiver =
        callback_arguments(&class.name, is_static, &descriptor, &callback.descriptor)?;

    let arguments = crate::descriptor_arguments(&descriptor);
    let mut inserted = Vec::new();
    let mut slot = if is_static { 0 } else { 1 };
    let mut argument_slots = 0u32;
    if takes_receiver {
        inserted.push(0x2a); // aload_0
        argument_slots += 1;
    }
    for argument in &arguments {
        let (opcode, width) = load_opcode(argument)?;
        if slot > u8::MAX as u16 {
            return Err(format!(
                "{label}: argument local slot {slot} needs wide, which the bounded weaver does not emit"
            ));
        }
        inserted.push(opcode);
        inserted.push(slot as u8);
        slot = slot.checked_add(width).ok_or("local slot overflow")?;
        argument_slots += u32::from(width);
    }
    if slot > code.max_locals {
        return Err(format!(
            "{label} declares max_locals {} but its arguments occupy {slot} slots",
            code.max_locals
        ));
    }

    let mut pool = PoolBuilder::new(&class.pool);
    let callback_index = pool.method_ref(callback)?;
    inserted.push(0xb8); // invokestatic
    inserted.extend_from_slice(&callback_index.to_be_bytes());

    let mut woven = inserted;
    woven.extend_from_slice(code.code);
    if woven.len() > MAX_CODE_LENGTH {
        return Err(format!(
            "{label}: woven code would exceed the class-file limit of {MAX_CODE_LENGTH} bytes"
        ));
    }
    let max_stack = u32::from(code.max_stack) + argument_slots + 1;
    let max_stack =
        u16::try_from(max_stack).map_err(|_| format!("{label}: woven max_stack overflows u16"))?;

    emit(
        &layout,
        Some(Patch {
            method: method_index,
            attribute: attribute_index,
            code: woven,
            max_stack,
        }),
        &pool.added,
    )
}

/// Apply the `HEAD` plans that target `class`, in plan order.
///
/// Only plans whose [`MixinInjectionPlan::at`] is `"HEAD"`, whose target owner
/// is this class and whose target method exists are applied; plans for other
/// classes and every other anchor are left to the interpreter.  Cancellable
/// plans are skipped because their callback takes a `CallbackInfo`, which this
/// bytecode insert cannot construct.
///
/// Not supported here, and left on the interpreter path: `RETURN`, `TAIL` and
/// `INVOKE` anchors; targets with branches or stack-map frames (so no
/// `RETURN`/`TAIL` rewriting); exception handlers; cancellation; and
/// `CallbackInfo`/`CallbackInfoReturnable` parameters.  Errors from a matching
/// plan (ambiguous target descriptor, callback mismatch, unsupported target)
/// propagate rather than being silently skipped.
///
/// Every insert lands at offset 0, so when several plans match one method the
/// last plan applied is the first callback to run.  Callers that need Mixin's
/// declaration order should pass the plans in reverse.
pub fn weave_plans(class: &ClassFile, plans: &[MixinInjectionPlan]) -> Result<Vec<u8>, String> {
    let mut current = class.write()?;
    for plan in plans {
        if plan.at != "HEAD" || plan.cancellable || plan.target.owner != class.name {
            continue;
        }
        let next = ClassFile::parse(&current)
            .map_err(|error| format!("rewritten class did not parse again: {error}"))?;
        current = weave_head(&next, &plan.target, &plan.callback)?;
    }
    Ok(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u2(out: &mut Vec<u8>, value: u16) {
        out.extend_from_slice(&value.to_be_bytes());
    }

    fn u4(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&value.to_be_bytes());
    }

    fn utf8(out: &mut Vec<u8>, value: &str) {
        out.push(1);
        u2(out, value.len() as u16);
        out.extend_from_slice(value.as_bytes());
    }

    /// A minimal class with one method.  Pool: 1 `demo/T`, 2 Class 1,
    /// 3 `java/lang/Object`, 4 Class 3, 5 `m`, 6 descriptor, 7 `Code`.
    fn fixture(
        access: u16,
        descriptor: &str,
        max_locals: u16,
        code: &[u8],
        exception_table: &[u8],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        u4(&mut out, 0xCAFE_BABE);
        u2(&mut out, 0);
        u2(&mut out, 52);
        u2(&mut out, 8);
        utf8(&mut out, "demo/T");
        out.extend_from_slice(&[7, 0, 1]);
        utf8(&mut out, "java/lang/Object");
        out.extend_from_slice(&[7, 0, 3]);
        utf8(&mut out, "m");
        utf8(&mut out, descriptor);
        utf8(&mut out, "Code");
        u2(&mut out, 0x0021); // access
        u2(&mut out, 2); // this
        u2(&mut out, 4); // super
        u2(&mut out, 0); // interfaces
        u2(&mut out, 0); // fields
        u2(&mut out, 1); // methods
        u2(&mut out, access);
        u2(&mut out, 5);
        u2(&mut out, 6);
        u2(&mut out, 1); // attributes
        u2(&mut out, 7);
        let mut body = Vec::new();
        u2(&mut body, 2); // max_stack
        u2(&mut body, max_locals);
        u4(&mut body, code.len() as u32);
        body.extend_from_slice(code);
        u2(&mut body, (exception_table.len() / 8) as u16);
        body.extend_from_slice(exception_table);
        u2(&mut body, 0); // Code attributes
        u4(&mut out, body.len() as u32);
        out.extend_from_slice(&body);
        u2(&mut out, 0); // class attributes
        out
    }

    fn callback(descriptor: &str) -> MethodRef {
        MethodRef {
            owner: "demo/Cb".into(),
            name: "cb".into(),
            descriptor: descriptor.into(),
        }
    }

    fn target() -> MethodRef {
        MethodRef {
            owner: "demo/T".into(),
            name: "m".into(),
            descriptor: "(II)I".into(),
        }
    }

    #[test]
    fn write_round_trips_a_synthetic_class() {
        let bytes = fixture(0x0001, "(II)I", 3, &[0x1a, 0x1b, 0x60, 0xac], &[]);
        let class = ClassFile::parse(&bytes).unwrap();
        assert_eq!(class.write().unwrap(), bytes);
    }

    #[test]
    fn write_preserves_long_double_and_unknown_attributes() {
        let mut bytes = Vec::new();
        u4(&mut bytes, 0xCAFE_BABE);
        u2(&mut bytes, 0);
        u2(&mut bytes, 52);
        u2(&mut bytes, 10); // constant pool count (two wide slots included)
        utf8(&mut bytes, "demo/LD"); // 1
        bytes.extend_from_slice(&[7, 0, 1]); // 2
        utf8(&mut bytes, "java/lang/Object"); // 3
        bytes.extend_from_slice(&[7, 0, 3]); // 4
        bytes.extend_from_slice(&[5, 0, 0, 0, 0, 0, 0, 0, 7]); // 5: Long, slot 6
        bytes.extend_from_slice(&[6, 0x40, 0x09, 0x21, 0xfb, 0x54, 0x44, 0x2d, 0x18]); // 7: Double, slot 8
        utf8(&mut bytes, "Signature"); // 9
        u2(&mut bytes, 0x0021);
        u2(&mut bytes, 2);
        u2(&mut bytes, 4);
        u2(&mut bytes, 0); // interfaces
        u2(&mut bytes, 0); // fields
        u2(&mut bytes, 0); // methods
        u2(&mut bytes, 1); // class attributes
        u2(&mut bytes, 9);
        u4(&mut bytes, 4);
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        let class = ClassFile::parse(&bytes).unwrap();
        assert_eq!(class.write().unwrap(), bytes);
    }

    #[test]
    fn writer_rejects_an_unknown_constant_tag() {
        let mut bytes = fixture(0x0001, "(II)I", 3, &[0xac], &[]);
        // Turn the `Code` Utf8 (last entry) into tag 2, which the class writer
        // does not know.  `write_class` must not emit the bytes back.
        let code_name = bytes.windows(4).position(|w| w == b"Code").unwrap();
        bytes[code_name - 3] = 2;
        assert!(write_class(&bytes).is_err());
    }

    #[test]
    fn head_weaves_an_instance_method() {
        let bytes = fixture(0x0001, "(II)I", 3, &[0x1a, 0x1b, 0x60, 0xac], &[]);
        let class = ClassFile::parse(&bytes).unwrap();
        let woven = weave_head(&class, &target(), &callback("(Ldemo/T;II)V")).unwrap();
        let reparsed = ClassFile::parse(&woven).unwrap();
        assert_eq!(reparsed.write().unwrap(), woven);
        let method = reparsed.method("m", "(II)I").unwrap();
        let code = method.code.as_ref().unwrap();
        assert_eq!(&code[..5], &[0x2a, 0x15, 0x01, 0x15, 0x02]);
        assert_eq!(code[5], 0xb8);
        assert_eq!(&code[8..], &[0x1a, 0x1b, 0x60, 0xac]);
        assert_eq!(method.max_stack, 2 + 3 + 1);
    }

    #[test]
    fn head_weaves_an_instance_target_without_the_receiver() {
        let bytes = fixture(0x0001, "(II)I", 3, &[0x1a, 0x1b, 0x60, 0xac], &[]);
        let class = ClassFile::parse(&bytes).unwrap();
        let woven = weave_head(&class, &target(), &callback("(II)V")).unwrap();
        let reparsed = ClassFile::parse(&woven).unwrap();
        let method = reparsed.method("m", "(II)I").unwrap();
        let code = method.code.as_ref().unwrap();
        assert_eq!(&code[..4], &[0x15, 0x01, 0x15, 0x02]);
        assert_eq!(&code[7..], &[0x1a, 0x1b, 0x60, 0xac]);
        assert_eq!(method.max_stack, 2 + 2 + 1);
    }

    #[test]
    fn head_rejects_a_receiver_for_a_static_target() {
        let bytes = fixture(0x0009, "(II)I", 2, &[0x1a, 0x1b, 0x60, 0xac], &[]);
        let class = ClassFile::parse(&bytes).unwrap();
        let error = weave_head(&class, &target(), &callback("(Ldemo/T;II)V")).unwrap_err();
        assert!(error.contains("static target expects"), "{error}");
    }

    #[test]
    fn head_rejects_a_wrong_callback_descriptor() {
        let bytes = fixture(0x0001, "(II)I", 3, &[0x1a, 0x1b, 0x60, 0xac], &[]);
        let class = ClassFile::parse(&bytes).unwrap();
        let error = weave_head(&class, &target(), &callback("(Ldemo/T;I)V")).unwrap_err();
        assert!(error.contains("does not match"), "{error}");
    }

    #[test]
    fn head_rejects_a_callback_info_parameter() {
        let bytes = fixture(0x0001, "(II)I", 3, &[0x1a, 0x1b, 0x60, 0xac], &[]);
        let class = ClassFile::parse(&bytes).unwrap();
        let error = weave_head(
            &class,
            &target(),
            &callback("(Ldemo/T;IILorg/spongepowered/asm/mixin/injection/callback/CallbackInfo;)V"),
        )
        .unwrap_err();
        assert!(error.contains("CallbackInfo"), "{error}");
    }

    #[test]
    fn head_rejects_a_branch() {
        let bytes = fixture(0x0001, "(I)I", 2, &[0x99, 0x00, 0x03, 0xb1], &[]);
        let class = ClassFile::parse(&bytes).unwrap();
        let branchy = MethodRef {
            descriptor: "(I)I".into(),
            ..target()
        };
        let error = weave_head(&class, &branchy, &callback("(Ldemo/T;I)V")).unwrap_err();
        assert!(error.contains("ifeq"), "{error}");
    }

    #[test]
    fn head_rejects_an_exception_table() {
        let handler = [0u8; 8];
        let bytes = fixture(0x0001, "(II)I", 3, &[0xac], &handler);
        let class = ClassFile::parse(&bytes).unwrap();
        let error = weave_head(&class, &target(), &callback("(Ldemo/T;II)V")).unwrap_err();
        assert!(error.contains("exception"), "{error}");
    }
}
