//! A bounded class-file writer and injection weaver.
//!
//! The interpreter in [`crate`] never needed to write a class: it reads a
//! `ClassFile` and executes a bytecode subset.  This module adds the other
//! direction, for the shapes a native Mixin translation can prepare before an
//! embedded JVM sees them: a method that gets a `HEAD` callback at offset 0, or
//! one that gets a callback immediately before each `return`.
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
//! [`weave_return`] inserts the same sequence immediately before every
//! return-family instruction (`return`, `ireturn`, `areturn`, `freturn`,
//! `dreturn`, `lreturn`) for [`ReturnMode::Return`], or before only the last one
//! for [`ReturnMode::Tail`] (Mixin's `TAIL`).  Only `void` targets are woven;
//! the callback carries the target's arguments, plus the receiver when it
//! declares it.
//!
//! Because an insert moves every byte after it, both anchors relocate everything
//! in the method that names a bytecode offset, each by the delta that applies at
//! its own offset (a `RETURN` insert only moves the bytes after that `return`):
//!
//! - 16-bit branch offsets (`if*`, `goto`, `jsr`, `ifnull`, `ifnonnull`) and the
//!   32-bit `goto_w`/`jsr_w`, keeping each target on the instruction it named
//!   (or on the insert in front of it, per the `RETURN` rule below);
//! - the exception table's `start_pc`, `end_pc` and `handler_pc`: the inserted
//!   callback is never caught by the target's own handler, because an insert at
//!   a range start lands before it and an insert at the exclusive end lands
//!   after it;
//! - `LineNumberTable`, `LocalVariableTable` and `LocalVariableTypeTable`
//!   `start_pc` (and the local range's end, so its length grows with the code);
//! - `StackMapTable` frame offsets, re-derived from the shifted absolute
//!   offsets as cumulative `offset_delta`s, plus the `uninitialized` offsets
//!   inside frames.  A frame whose delta no longer fits its compact form is
//!   widened (`same_frame` to `same_frame_extended`, and so on).
//! - the `default` and case offsets of a `tableswitch`/`lookupswitch`, and the
//!   alignment padding its own new offset forces.  The padding is 0..=3 bytes,
//!   so relocating a switch can move the bytes after it a second time, on top
//!   of the insert.
//!
//! A relocation is therefore a layout pass, not a single splice: the pass walks
//! the original instructions left to right, places the inserted bytes at the
//! instruction they precede, and gives each switch the padding its *new* offset
//! asks for.  Padding depends only on that offset, which the pass has already
//! fixed, so one pass is self-consistent.  The pass repeats - bounded by
//! `MAX_RELOCATION_PASSES` - only when a `goto`/`jsr` has to grow to
//! `goto_w`/`jsr_w` because an insert pushed its target past the 16-bit range;
//! growing an instruction moves everything after it, so the next pass has to
//! agree.  A layout that does not settle inside the bound is rejected.
//!
//! A conditional branch (`if*`, `ifnull`, `ifnonnull`) has no wide encoding.
//! Splitting one into an inverted branch over a `goto_w` would add a branch and
//! a frame at a site the weaver cannot derive, so a conditional whose relocated
//! target no longer fits 16 bits is rejected with the distance instead of
//! written wrong.
//!
//! An insert sits in front of the instruction it precedes, and two details
//! follow from that.  A `RETURN`/`TAIL` insert is *entered*: a branch or switch
//! case that named the `return` now lands on the callback, so a `return` that is
//! also a jump target - the body of a `switch` case, an `if` arm - still calls
//! back.  A `HEAD` insert is *skipped*: a backward branch to the method's first
//! instruction lands after the callback, so the callback runs once at entry.
//! When an insert lands on a `StackMapTable` frame position the frame is emitted
//! twice, once before the callback and once at the instruction, because the
//! bytes after an unconditional transfer start a basic block whether or not
//! anything branches to them and the callback runs in the same state.
//!
//! `wide`-prefixed local instructions (`wide` load/store, `iinc`, `ret`) are
//! walked at their real length and copied verbatim, so a method whose local
//! slots exceed 255 weaves exactly like one below the limit.
//!
//! `max_stack` is raised conservatively (HEAD: `original + argument slots + 1`;
//! RETURN/TAIL: `original + argument slots`), `max_locals` is raised to the
//! argument slots when the class declares fewer, and the `code_length` and
//! `Code` attribute length follow the rewritten code.  Attributes the weaver
//! does not read (`Exceptions`, type annotations, ...) are still copied
//! byte-for-byte.
//!
//! The callback descriptor must consume exactly the target's arguments and
//! return `void`.  An instance target's callback may instead take the target
//! instance as an extra first parameter, which the inserted `aload_0` supplies.
//!
//! [`weave_class`] applies every matching `HEAD` plan first, then every
//! `RETURN`/`TAIL` plan, re-parsing the class between edits and skipping plans
//! for other classes.  [`weave_classes`] groups plans by target owner, weaves
//! each affected class with [`weave_class`], and re-encodes every other class
//! unchanged.
//!
//! # What it does not support
//!
//! [`weave_plans`] applies only `HEAD` plans, in plan order, and skips plans
//! that target another class or ask for cancellation; [`weave_class`] instead
//! rejects a cancellable or `INVOKE` plan that targets its class with a reason.
//!
//! Non-void `RETURN`/`TAIL` callbacks, cancellation and `@At("INVOKE")` stay on
//! the interpreter path.  A non-void `RETURN` callback needs a
//! `CallbackInfoReturnable` object carrying the target's boxed result,
//! cancellation needs a runtime-owned `CallbackInfo`, and `INVOKE` moves or
//! guards a call site - all three need object construction or call-site
//! rewriting a plain insert cannot express.
//!
//! A non-void `RETURN` is rejected rather than half-woven, and the reason is a
//! hard one: Mixin's callback is static and void, so it must receive a live
//! `CallbackInfoReturnable`.  Emitting that means `new`/`dup`/`<init>` plus a
//! boxing conversion of the value already on the stack; reading an override
//! back means `getReturnValue` plus `checkcast`/unboxing and a branch that
//! either keeps the original value or replaces it.  Every one of those branches
//! needs a `StackMapTable` frame, and this module relocates frames but cannot
//! synthesise one.  The crate also has no JVM-side helper class to delegate to:
//! `CallbackInfos` is interpreter state, not a class in the woven jar.  So the
//! target is rejected with that reason and the interpreter handles it.
//!
//! A class whose constant pool uses `CONSTANT_Dynamic` (condy) is rejected
//! because the writer cannot re-encode its bootstrap arguments.  Non-ASCII
//! constant pool strings and code that would exceed the class-file
//! `code_length` limit are rejected with a reason rather than written.

use std::collections::{BTreeMap, BTreeSet};

use crate::{ClassFile, Cp, MethodRef, MixinInjectionPlan};

const ACC_STATIC: u16 = 0x0008;
const ACC_ABSTRACT: u16 = 0x0400;
const ACC_NATIVE: u16 = 0x0100;
const MAX_CODE_LENGTH: usize = 65_535;
/// Relocation passes before a layout has to be stable.  A pass only ever grows
/// an instruction (`goto`/`jsr` to `goto_w`/`jsr_w`), never shrinks one, so the
/// layout is monotone and settles in at most one pass per widening round plus
/// one to confirm; the bound keeps a pathological method bounded instead of
/// looping, and a method that hits it is rejected rather than written.
const MAX_RELOCATION_PASSES: usize = 16;
/// Plans [`weave_class`] applies to one class.  A jar with more matching plans
/// than this is outside the bounded weaver instead of growing the edit loop.
const MAX_WEAVE_PLANS_PER_CLASS: usize = 32;

/// Which return instruction a [`weave_return`] insert lands before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReturnMode {
    /// Before every return-family instruction (`RETURN`).
    Return,
    /// Before the last return-family instruction only (`TAIL`).
    Tail,
}

impl ReturnMode {
    fn anchor(self) -> &'static str {
        match self {
            Self::Return => "RETURN",
            Self::Tail => "TAIL",
        }
    }
}

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

/// One entry of a `Code` attribute's exception table.
#[derive(Debug, Clone, Copy)]
struct ExceptionHandler {
    start_pc: u16,
    end_pc: u16,
    handler_pc: u16,
    catch_type: u16,
}

/// One `Code` attribute, split at the bytes a rewrite touches.
struct CodeLayout<'a> {
    max_stack: u16,
    max_locals: u16,
    code: &'a [u8],
    exception_table: Vec<ExceptionHandler>,
    /// The Code attribute's own attributes, in file order.  Tables that name a
    /// bytecode offset are parsed so a rewrite can shift them; every other
    /// attribute keeps its bytes.
    attributes: Vec<CodeAttribute<'a>>,
}

/// One attribute of a `Code` attribute.
enum CodeAttribute<'a> {
    /// An attribute the weaver does not read: copied verbatim.
    Raw { name_index: u16, body: &'a [u8] },
    /// `LineNumberTable`.
    LineNumbers {
        name_index: u16,
        lines: Vec<LineNumber>,
    },
    /// `LocalVariableTable` or `LocalVariableTypeTable`, whose rows share a
    /// layout.
    LocalVariables {
        name_index: u16,
        entries: Vec<LocalVariable>,
    },
    /// `StackMapTable`, the verifier's frames.
    StackMap {
        name_index: u16,
        frames: Vec<StackMapFrame>,
    },
}

impl CodeAttribute<'_> {
    fn name_index(&self) -> u16 {
        match self {
            Self::Raw { name_index, .. }
            | Self::LineNumbers { name_index, .. }
            | Self::LocalVariables { name_index, .. }
            | Self::StackMap { name_index, .. } => *name_index,
        }
    }
}

struct LineNumber {
    start_pc: u16,
    line_number: u16,
}

struct LocalVariable {
    start_pc: u16,
    length: u16,
    name_index: u16,
    descriptor_index: u16,
    index: u16,
}

/// One `StackMapTable` frame with its absolute bytecode offset.
struct StackMapFrame {
    /// The parsed `frame_type`, kept so an unshifted table re-encodes
    /// byte-exactly; a shifted table widens the type only when the new
    /// `offset_delta` no longer fits it.
    frame_type: u8,
    offset: usize,
    locals: Vec<VerificationType>,
    stack: Vec<VerificationType>,
}

/// One `verification_type_info`.
#[derive(Debug, Clone, Copy)]
enum VerificationType {
    /// Tags 0..=6 (`Top`, `Integer`, ... `Null`).
    Simple(u8),
    Object(u16),
    /// A `new` offset, which a rewrite shifts like a branch target.
    Uninitialized(u16),
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
                    Some(parse_code(body, &pool_utf8)?)
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

fn parse_code<'a>(body: &'a [u8], pool: &[Option<&'a str>]) -> Result<CodeLayout<'a>, String> {
    let mut r = Bytes::new(body);
    let max_stack = r.u2()?;
    let max_locals = r.u2()?;
    let code_length = r.u4()? as usize;
    let code = r.bytes(code_length)?;
    let handlers = r.u2()? as usize;
    let mut exception_table = Vec::with_capacity(handlers);
    for _ in 0..handlers {
        exception_table.push(ExceptionHandler {
            start_pc: r.u2()?,
            end_pc: r.u2()?,
            handler_pc: r.u2()?,
            catch_type: r.u2()?,
        });
    }
    let attribute_count = r.u2()? as usize;
    let mut attributes = Vec::with_capacity(attribute_count);
    for _ in 0..attribute_count {
        let name_index = r.u2()?;
        let length = r.u4()? as usize;
        let attribute = r.bytes(length)?;
        attributes.push(parse_code_attribute(pool, name_index, attribute)?);
    }
    Ok(CodeLayout {
        max_stack,
        max_locals,
        code,
        exception_table,
        attributes,
    })
}

fn parse_code_attribute<'a>(
    pool: &[Option<&'a str>],
    name_index: u16,
    body: &'a [u8],
) -> Result<CodeAttribute<'a>, String> {
    let name = pool.get(name_index as usize).copied().flatten();
    Ok(match name {
        Some("LineNumberTable") => CodeAttribute::LineNumbers {
            name_index,
            lines: parse_line_numbers(body)?,
        },
        Some("LocalVariableTable" | "LocalVariableTypeTable") => CodeAttribute::LocalVariables {
            name_index,
            entries: parse_local_variables(body)?,
        },
        Some("StackMapTable") => CodeAttribute::StackMap {
            name_index,
            frames: parse_stack_map(body)?,
        },
        _ => CodeAttribute::Raw { name_index, body },
    })
}

fn parse_line_numbers(body: &[u8]) -> Result<Vec<LineNumber>, String> {
    let mut r = Bytes::new(body);
    let count = r.u2()? as usize;
    let mut lines = Vec::with_capacity(count);
    for _ in 0..count {
        lines.push(LineNumber {
            start_pc: r.u2()?,
            line_number: r.u2()?,
        });
    }
    Ok(lines)
}

fn parse_local_variables(body: &[u8]) -> Result<Vec<LocalVariable>, String> {
    let mut r = Bytes::new(body);
    let count = r.u2()? as usize;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        entries.push(LocalVariable {
            start_pc: r.u2()?,
            length: r.u2()?,
            name_index: r.u2()?,
            descriptor_index: r.u2()?,
            index: r.u2()?,
        });
    }
    Ok(entries)
}

fn parse_verification(r: &mut Bytes<'_>) -> Result<VerificationType, String> {
    let tag = r.u1()?;
    Ok(match tag {
        0..=6 => VerificationType::Simple(tag),
        7 => VerificationType::Object(r.u2()?),
        8 => VerificationType::Uninitialized(r.u2()?),
        other => return Err(format!("stack-map verification type {other} is reserved")),
    })
}

fn parse_stack_map(body: &[u8]) -> Result<Vec<StackMapFrame>, String> {
    let mut r = Bytes::new(body);
    let count = r.u2()? as usize;
    let mut frames: Vec<StackMapFrame> = Vec::with_capacity(count);
    for _ in 0..count {
        let frame_type = r.u1()?;
        let (delta, locals, stack) = match frame_type {
            0..=63 => (frame_type as usize, Vec::new(), Vec::new()),
            64..=127 => (
                frame_type as usize - 64,
                Vec::new(),
                vec![parse_verification(&mut r)?],
            ),
            128..=246 => {
                return Err(format!(
                    "StackMapTable frame type {frame_type} is reserved; the bounded weaver cannot re-encode it"
                ));
            }
            247 => (
                r.u2()? as usize,
                Vec::new(),
                vec![parse_verification(&mut r)?],
            ),
            248..=251 => (r.u2()? as usize, Vec::new(), Vec::new()),
            252..=254 => {
                let delta = r.u2()? as usize;
                let locals = parse_verifications(&mut r, frame_type as usize - 251)?;
                (delta, locals, Vec::new())
            }
            _ => {
                let delta = r.u2()? as usize;
                let local_count = r.u2()? as usize;
                let locals = parse_verifications(&mut r, local_count)?;
                let stack_count = r.u2()? as usize;
                let stack = parse_verifications(&mut r, stack_count)?;
                (delta, locals, stack)
            }
        };
        let offset = match frames.last() {
            Some(previous) => previous.offset + delta + 1,
            None => delta,
        };
        frames.push(StackMapFrame {
            frame_type,
            offset,
            locals,
            stack,
        });
    }
    Ok(frames)
}

fn parse_verifications(r: &mut Bytes<'_>, count: usize) -> Result<Vec<VerificationType>, String> {
    let mut types = Vec::with_capacity(count);
    for _ in 0..count {
        types.push(parse_verification(r)?);
    }
    Ok(types)
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

/// The rewritten `Code` attribute of one method.
struct CodePatch<'a> {
    code: Vec<u8>,
    max_stack: u16,
    max_locals: u16,
    exception_table: Vec<ExceptionHandler>,
    attributes: Vec<CodeAttribute<'a>>,
}

struct Patch<'a> {
    method: usize,
    attribute: usize,
    code: CodePatch<'a>,
}

fn emit<'a>(
    layout: &Layout<'a>,
    patch: Option<Patch<'a>>,
    added: &[Cp],
) -> Result<Vec<u8>, String> {
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
            if let Some(patch) = patch.as_ref()
                && patch.method == method_index
                && patch.attribute == attribute_index
            {
                let body = encode_code(&patch.code)?;
                let length = u32::try_from(body.len())
                    .map_err(|_| "Code attribute is longer than 4 GiB".to_owned())?;
                out.extend_from_slice(&length.to_be_bytes());
                out.extend_from_slice(&body);
                continue;
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

fn encode_code(patch: &CodePatch<'_>) -> Result<Vec<u8>, String> {
    if patch.code.len() > MAX_CODE_LENGTH {
        return Err(format!(
            "method code is longer than the class-file limit of {MAX_CODE_LENGTH} bytes"
        ));
    }
    let length = u32::try_from(patch.code.len())
        .map_err(|_| "method code is longer than 4 GiB".to_owned())?;
    let mut body = Vec::with_capacity(patch.code.len() + patch.exception_table.len() * 8 + 64);
    body.extend_from_slice(&patch.max_stack.to_be_bytes());
    body.extend_from_slice(&patch.max_locals.to_be_bytes());
    body.extend_from_slice(&length.to_be_bytes());
    body.extend_from_slice(&patch.code);
    let handlers = u16::try_from(patch.exception_table.len())
        .map_err(|_| "exception table has too many entries".to_owned())?;
    body.extend_from_slice(&handlers.to_be_bytes());
    for handler in &patch.exception_table {
        body.extend_from_slice(&handler.start_pc.to_be_bytes());
        body.extend_from_slice(&handler.end_pc.to_be_bytes());
        body.extend_from_slice(&handler.handler_pc.to_be_bytes());
        body.extend_from_slice(&handler.catch_type.to_be_bytes());
    }
    let attributes = u16::try_from(patch.attributes.len())
        .map_err(|_| "Code attribute has too many attributes".to_owned())?;
    body.extend_from_slice(&attributes.to_be_bytes());
    for attribute in &patch.attributes {
        body.extend_from_slice(&attribute.name_index().to_be_bytes());
        let encoded = encode_code_attribute(attribute)?;
        let length = u32::try_from(encoded.len())
            .map_err(|_| "Code attribute is longer than 4 GiB".to_owned())?;
        body.extend_from_slice(&length.to_be_bytes());
        body.extend_from_slice(&encoded);
    }
    Ok(body)
}

fn encode_code_attribute(attribute: &CodeAttribute<'_>) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    match attribute {
        CodeAttribute::Raw { body, .. } => out.extend_from_slice(body),
        CodeAttribute::LineNumbers { lines, .. } => {
            let count = u16::try_from(lines.len())
                .map_err(|_| "LineNumberTable has too many entries".to_owned())?;
            out.extend_from_slice(&count.to_be_bytes());
            for line in lines {
                out.extend_from_slice(&line.start_pc.to_be_bytes());
                out.extend_from_slice(&line.line_number.to_be_bytes());
            }
        }
        CodeAttribute::LocalVariables { entries, .. } => {
            let count = u16::try_from(entries.len())
                .map_err(|_| "local-variable table has too many entries".to_owned())?;
            out.extend_from_slice(&count.to_be_bytes());
            for entry in entries {
                out.extend_from_slice(&entry.start_pc.to_be_bytes());
                out.extend_from_slice(&entry.length.to_be_bytes());
                out.extend_from_slice(&entry.name_index.to_be_bytes());
                out.extend_from_slice(&entry.descriptor_index.to_be_bytes());
                out.extend_from_slice(&entry.index.to_be_bytes());
            }
        }
        CodeAttribute::StackMap { frames, .. } => encode_frames(&mut out, frames)?,
    }
    Ok(out)
}

fn encode_frames(out: &mut Vec<u8>, frames: &[StackMapFrame]) -> Result<(), String> {
    let count =
        u16::try_from(frames.len()).map_err(|_| "StackMapTable has too many frames".to_owned())?;
    out.extend_from_slice(&count.to_be_bytes());
    let mut previous: Option<usize> = None;
    for frame in frames {
        let delta = match previous {
            None => frame.offset,
            Some(previous) => frame
                .offset
                .checked_sub(previous + 1)
                .ok_or_else(|| "stack-map frames are out of order".to_owned())?,
        };
        let delta = u16::try_from(delta).map_err(|_| {
            format!(
                "a stack-map frame would move past the class-file offset limit of {MAX_CODE_LENGTH}"
            )
        })?;
        previous = Some(frame.offset);
        match frame.frame_type {
            0..=63 => {
                if delta <= 63 {
                    out.push(delta as u8);
                } else {
                    out.push(251); // same_frame_extended
                    out.extend_from_slice(&delta.to_be_bytes());
                }
            }
            64..=127 => {
                if delta <= 63 {
                    out.push(64 + delta as u8);
                } else {
                    out.push(247); // same_locals_1_stack_item_frame_extended
                    out.extend_from_slice(&delta.to_be_bytes());
                }
                encode_verifications(out, &frame.stack);
            }
            247 => {
                out.push(247);
                out.extend_from_slice(&delta.to_be_bytes());
                encode_verifications(out, &frame.stack);
            }
            248..=251 => {
                out.push(frame.frame_type);
                out.extend_from_slice(&delta.to_be_bytes());
            }
            252..=254 => {
                out.push(frame.frame_type);
                out.extend_from_slice(&delta.to_be_bytes());
                encode_verifications(out, &frame.locals);
            }
            255 => {
                out.push(255);
                out.extend_from_slice(&delta.to_be_bytes());
                let locals = u16::try_from(frame.locals.len())
                    .map_err(|_| "stack-map frame has too many locals".to_owned())?;
                out.extend_from_slice(&locals.to_be_bytes());
                encode_verifications(out, &frame.locals);
                let stack = u16::try_from(frame.stack.len())
                    .map_err(|_| "stack-map frame has too many stack items".to_owned())?;
                out.extend_from_slice(&stack.to_be_bytes());
                encode_verifications(out, &frame.stack);
            }
            other => {
                return Err(format!(
                    "stack-map frame type {other} is reserved; the bounded weaver cannot re-encode it"
                ));
            }
        }
    }
    Ok(())
}

fn encode_verifications(out: &mut Vec<u8>, types: &[VerificationType]) {
    for verification in types {
        match verification {
            VerificationType::Simple(tag) => out.push(*tag),
            VerificationType::Object(index) => {
                out.push(7);
                out.extend_from_slice(&index.to_be_bytes());
            }
            VerificationType::Uninitialized(offset) => {
                out.push(8);
                out.extend_from_slice(&offset.to_be_bytes());
            }
        }
    }
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

/// A branch instruction that names a bytecode offset, and therefore has to be
/// rewritten when an insert moves the code after it.
struct BranchSite {
    /// Offset of the opcode in the original code.
    at: usize,
    /// The branch opcode, kept so a `goto`/`jsr` can be widened and an `if*`
    /// can be rejected by name.
    opcode: u8,
    /// Whether the offset field is 32 bits (`goto_w`/`jsr_w`).
    wide: bool,
    /// Absolute target offset in the original code.
    target: usize,
}

/// One `tableswitch`/`lookupswitch`, with every target as an absolute offset in
/// the original code.
struct SwitchSite {
    /// Offset of the opcode in the original code.
    at: usize,
    kind: SwitchKind,
}

enum SwitchKind {
    /// `tableswitch`: `targets[i]` answers `low + i`.
    Table {
        default: usize,
        low: i32,
        high: i32,
        targets: Vec<usize>,
    },
    /// `lookupswitch`: `pairs[i]` is `(match, target)`.
    Lookup {
        default: usize,
        pairs: Vec<(i32, usize)>,
    },
}

impl SwitchKind {
    fn name(&self) -> &'static str {
        match self {
            Self::Table { .. } => "tableswitch",
            Self::Lookup { .. } => "lookupswitch",
        }
    }

    fn opcode(&self) -> u8 {
        match self {
            Self::Table { .. } => 0xaa,
            Self::Lookup { .. } => 0xab,
        }
    }

    fn default(&self) -> usize {
        match self {
            Self::Table { default, .. } | Self::Lookup { default, .. } => *default,
        }
    }

    /// The `default` target followed by every case target.
    fn all_targets(&self) -> Vec<usize> {
        let mut targets = vec![self.default()];
        match self {
            Self::Table { targets: cases, .. } => targets.extend_from_slice(cases),
            Self::Lookup { pairs, .. } => {
                targets.extend(pairs.iter().map(|(_, target)| *target));
            }
        }
        targets
    }

    /// The encoded bytes after the opcode and its alignment padding.
    fn operand_len(&self) -> usize {
        match self {
            Self::Table { targets, .. } => 12 + 4 * targets.len(),
            Self::Lookup { pairs, .. } => 8 + 8 * pairs.len(),
        }
    }

    /// The instruction's length when its opcode sits at `at`, which is only
    /// known once the layout has placed it.
    fn len_at(&self, at: usize) -> usize {
        1 + switch_padding(at) + self.operand_len()
    }
}

/// The alignment padding a switch at `at` needs so its first operand starts on
/// a 4-byte boundary counted from the start of the `code` array.
fn switch_padding(at: usize) -> usize {
    (4 - (at + 1) % 4) % 4
}

/// One instruction of the original code.
struct Instruction {
    at: usize,
    size: usize,
    /// Index into [`CodeScan::switches`] when this instruction is a switch.
    switch: Option<usize>,
}

/// One pass over a method's code: its instruction boundaries, its branches, its
/// switches and its return instructions.
struct CodeScan {
    /// Instructions in file order, covering the code without gaps.
    instructions: Vec<Instruction>,
    branches: Vec<BranchSite>,
    switches: Vec<SwitchSite>,
    returns: Vec<usize>,
}

impl CodeScan {
    fn starts(&self) -> BTreeSet<usize> {
        self.instructions
            .iter()
            .map(|instruction| instruction.at)
            .collect()
    }
}

/// The byte size of one fixed-length instruction, or `None` for an opcode whose
/// size the caller has to work out (`tableswitch`, `lookupswitch`, `wide`).
fn instruction_size(opcode: u8) -> Option<usize> {
    Some(match opcode {
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
        0x99..=0xa8 => 3,
        0xa9 => 2,
        0xaa | 0xab | 0xc4 => return None,
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
        0xc6..=0xc9 => 3,
        _ => return None,
    })
}

/// The length of a `wide`-prefixed instruction at `at`: `wide` plus a two-byte
/// local index plus, for `iinc`, its two-byte constant.
fn wide_instruction_size(label: &str, code: &[u8], at: usize) -> Result<usize, String> {
    let sub = code
        .get(at + 1)
        .copied()
        .ok_or_else(|| format!("{label} has a truncated wide instruction at offset {at}"))?;
    match sub {
        // wide iload/lload/fload/dload/aload, the stores, and ret.
        0x15..=0x19 | 0x36..=0x3a | 0xa9 => Ok(4),
        // wide iinc <index> <const>.
        0x84 => Ok(6),
        other => Err(format!(
            "{label} uses wide opcode 0x{other:02x} at offset {at}; wide only prefixes a local-variable instruction"
        )),
    }
}

/// Parse one switch.  Bounds are checked against the code the switch lives in,
/// so a truncated table is a reason, not an allocation.
fn parse_switch(label: &str, code: &[u8], at: usize) -> Result<(SwitchSite, usize), String> {
    let opcode = code[at];
    let mut r = Bytes::new(code);
    r.at = at + 1 + switch_padding(at);
    let relative = |r: &mut Bytes<'_>| -> Result<usize, String> {
        let delta = r.u4()? as i32;
        let target = at as i64 + i64::from(delta);
        if target < 0 || target as usize >= code.len() {
            return Err(format!(
                "{} at offset {at} jumps to {target}, which is outside the {}-byte code",
                opcode_name(opcode),
                code.len()
            ));
        }
        Ok(target as usize)
    };
    let kind = match opcode {
        0xaa => {
            let default = relative(&mut r)?;
            let low = r.u4()? as i32;
            let high = r.u4()? as i32;
            let count = if i64::from(high) < i64::from(low) {
                if i64::from(high) == i64::from(low) - 1 {
                    0
                } else {
                    return Err(format!(
                        "tableswitch at offset {at} declares low {low} above high {high}"
                    ));
                }
            } else {
                let count = (i64::from(high) - i64::from(low) + 1) as u64;
                if count > (code.len() - r.at) as u64 / 4 {
                    return Err(format!(
                        "{label} has a tableswitch at offset {at} declaring {count} jump targets, more than the code holds"
                    ));
                }
                count as usize
            };
            let mut targets = Vec::with_capacity(count);
            for _ in 0..count {
                targets.push(relative(&mut r)?);
            }
            SwitchKind::Table {
                default,
                low,
                high,
                targets,
            }
        }
        0xab => {
            let default = relative(&mut r)?;
            let pairs = r.u4()? as i32;
            if pairs < 0 {
                return Err(format!(
                    "lookupswitch at offset {at} declares {pairs} match-offset pairs"
                ));
            }
            let pairs = pairs as usize;
            if pairs as u64 > (code.len() - r.at) as u64 / 8 {
                return Err(format!(
                    "{label} has a lookupswitch at offset {at} declaring {pairs} match-offset pairs, more than the code holds"
                ));
            }
            let mut entries = Vec::with_capacity(pairs);
            for _ in 0..pairs {
                let key = r.u4()? as i32;
                let target = relative(&mut r)?;
                entries.push((key, target));
            }
            SwitchKind::Lookup {
                default,
                pairs: entries,
            }
        }
        other => {
            return Err(format!(
                "opcode 0x{other:02x} at offset {at} is not a switch"
            ));
        }
    };
    let size = r.at - at;
    Ok((SwitchSite { at, kind }, size))
}

/// Walk a method's code, recording every instruction boundary, branch target,
/// switch target and return offset.
///
/// Every target has to name an instruction boundary: a relocation maps targets
/// through the layout, so a target inside an instruction would otherwise become
/// a branch into the middle of one.
fn scan_code(label: &str, code: &[u8]) -> Result<CodeScan, String> {
    let mut scan = CodeScan {
        instructions: Vec::new(),
        branches: Vec::new(),
        switches: Vec::new(),
        returns: Vec::new(),
    };
    let mut at = 0usize;
    while at < code.len() {
        let opcode = code[at];
        let (size, switch) = match opcode {
            0xaa | 0xab => {
                let (site, size) = parse_switch(label, code, at)?;
                scan.switches.push(site);
                (size, Some(scan.switches.len() - 1))
            }
            0xc4 => (wide_instruction_size(label, code, at)?, None),
            _ => {
                let size = instruction_size(opcode).ok_or_else(|| {
                    format!("{label} uses unknown or reserved opcode 0x{opcode:02x} at offset {at}")
                })?;
                (size, None)
            }
        };
        let end = at
            .checked_add(size)
            .ok_or_else(|| "code offset overflow".to_owned())?;
        if end > code.len() {
            return Err(format!(
                "{label} has a truncated instruction at offset {at}"
            ));
        }
        if matches!(opcode, 0xac..=0xb1) {
            scan.returns.push(at);
        }
        if let Some((wide, target)) = branch_target(opcode, code, at)? {
            scan.branches.push(BranchSite {
                at,
                opcode,
                wide,
                target,
            });
        }
        scan.instructions.push(Instruction { at, size, switch });
        at = end;
    }

    let starts = scan.starts();
    for branch in &scan.branches {
        if !starts.contains(&branch.target) {
            return Err(format!(
                "{label}: {} at offset {} targets {}, which is not an instruction boundary",
                opcode_name(branch.opcode),
                branch.at,
                branch.target
            ));
        }
    }
    for site in &scan.switches {
        for target in site.kind.all_targets() {
            if !starts.contains(&target) {
                return Err(format!(
                    "{label}: {} at offset {} targets {target}, which is not an instruction boundary",
                    site.kind.name(),
                    site.at
                ));
            }
        }
    }
    Ok(scan)
}

/// The signed relative offset of one branch instruction, or `None` when the
/// opcode has no bytecode offset operand.
fn branch_target(opcode: u8, code: &[u8], at: usize) -> Result<Option<(bool, usize)>, String> {
    let wide = match opcode {
        0x99..=0xa8 | 0xc6 | 0xc7 => false,
        0xc8 | 0xc9 => true,
        _ => return Ok(None),
    };
    let field = at + 1;
    let relative = if wide {
        let bytes = code
            .get(field..field + 4)
            .ok_or_else(|| format!("branch at offset {at} is truncated"))?;
        i64::from(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    } else {
        let bytes = code
            .get(field..field + 2)
            .ok_or_else(|| format!("branch at offset {at} is truncated"))?;
        i64::from(i16::from_be_bytes([bytes[0], bytes[1]]))
    };
    let target = at as i64 + relative;
    if target < 0 || target as usize >= code.len() {
        return Err(format!(
            "{} at offset {at} jumps to {target}, which is outside the {}-byte code",
            opcode_name(opcode),
            code.len()
        ));
    }
    Ok(Some((wide, target as usize)))
}

/// One relocation pass's answer: where every original instruction start, every
/// insert and the end of the code now sit.
struct CodePlan {
    /// New offset of each instruction start, keyed by its original offset.
    offsets: BTreeMap<usize, usize>,
    /// New offset where the bytes inserted before an instruction begin, i.e.
    /// the position an exclusive range end keeps the insert outside of.
    boundaries: BTreeMap<usize, usize>,
    /// Instructions that grew from `goto`/`jsr` to `goto_w`/`jsr_w`.
    widened: BTreeSet<usize>,
    /// New offset of the exclusive end of the code.
    end: usize,
    /// The original code's length, so an offset at the (exclusive) end maps
    /// without pretending it is an instruction.
    original_len: usize,
}

/// The length of the insert at `offset`, or `None`.  `insertions` is sorted by
/// offset, which [`relocate`] guarantees.
fn inserted_at(insertions: &[(usize, usize)], offset: usize) -> Option<usize> {
    insertions
        .binary_search_by_key(&offset, |(at, _)| *at)
        .ok()
        .map(|index| insertions[index].1)
}

/// Place every instruction of the original code in the rewritten buffer.
///
/// A switch's size is computed from the offset this pass gives it, so its
/// alignment padding is exact rather than carried over from the original
/// layout; an instruction in `widened` is five bytes instead of three.
fn layout_pass(
    label: &str,
    scan: &CodeScan,
    insertions: &[(usize, usize)],
    widened: &BTreeSet<usize>,
) -> Result<CodePlan, String> {
    let mut plan = CodePlan {
        offsets: BTreeMap::new(),
        boundaries: BTreeMap::new(),
        widened: widened.clone(),
        end: 0,
        original_len: scan
            .instructions
            .last()
            .map_or(0, |instruction| instruction.at + instruction.size),
    };
    let mut out = 0usize;
    for instruction in &scan.instructions {
        plan.boundaries.insert(instruction.at, out);
        out = out
            .checked_add(inserted_at(insertions, instruction.at).unwrap_or(0))
            .ok_or_else(|| format!("{label}: woven code length overflow"))?;
        plan.offsets.insert(instruction.at, out);
        let size = match instruction.switch {
            Some(index) => scan.switches[index].kind.len_at(out),
            None if widened.contains(&instruction.at) => 5,
            None => instruction.size,
        };
        out = out
            .checked_add(size)
            .ok_or_else(|| format!("{label}: woven code length overflow"))?;
    }
    plan.end = out;
    Ok(plan)
}

/// The opcode a wide branch has: `goto_w`/`jsr_w`.
fn widened_opcode(opcode: u8) -> Option<u8> {
    match opcode {
        0xa7 => Some(0xc8),
        0xa8 => Some(0xc9),
        _ => None,
    }
}

/// Relocate until the layout stops changing, bounded by
/// [`MAX_RELOCATION_PASSES`].
///
/// The only thing that can move between passes is a `goto`/`jsr` that had to
/// grow, because a grown instruction shifts every offset after it.  Widening is
/// monotone - a branch that fits stays narrow - so the loop cannot oscillate;
/// the bound exists to reject a method whose relocation does not settle instead
/// of spinning.
fn stable_plan(
    label: &str,
    scan: &CodeScan,
    insertions: &[(usize, usize)],
    entry: TargetEntry,
) -> Result<CodePlan, String> {
    let mut widened = BTreeSet::new();
    for _ in 0..MAX_RELOCATION_PASSES {
        let plan = layout_pass(label, scan, insertions, &widened)?;
        let offset = |at: usize| {
            plan.offsets
                .get(&at)
                .copied()
                .ok_or_else(|| format!("{label}: offset {at} is not an instruction boundary"))
        };
        let mut grew = false;
        for branch in &scan.branches {
            let opcode_at = offset(branch.at)?;
            let delta = map_target(label, &plan, branch.target, entry)? as i64 - opcode_at as i64;
            if branch.wide {
                if i32::try_from(delta).is_err() {
                    return Err(format!(
                        "{label}: relocating the {} at offset {} would exceed the 32-bit branch range",
                        opcode_name(branch.opcode),
                        branch.at
                    ));
                }
                continue;
            }
            if i16::try_from(delta).is_ok() {
                continue;
            }
            if widened_opcode(branch.opcode).is_none() {
                return Err(format!(
                    "{label}: {} at offset {} would sit {delta} bytes from its target after weaving, past the 16-bit branch range; the weaver only widens goto/jsr, so this conditional branch stays on the interpreter path",
                    opcode_name(branch.opcode),
                    branch.at
                ));
            }
            if widened.insert(branch.at) {
                grew = true;
            }
        }
        for site in &scan.switches {
            let opcode_at = plan.offsets.get(&site.at).copied().ok_or_else(|| {
                format!("{label}: offset {} is not an instruction boundary", site.at)
            })?;
            for target in site.kind.all_targets() {
                let target = map_target(label, &plan, target, entry)?;
                if i32::try_from(target as i64 - opcode_at as i64).is_err() {
                    return Err(format!(
                        "{label}: relocating the {} at offset {} would exceed the 32-bit switch range",
                        site.kind.name(),
                        site.at
                    ));
                }
            }
        }
        if !grew {
            return Ok(plan);
        }
    }
    Err(format!(
        "{label}: relocating switches and branches did not converge in {MAX_RELOCATION_PASSES} passes"
    ))
}

/// Where an original code offset now sits.
///
/// `inclusive` selects the two cases a rewrite distinguishes.  With `true` the
/// result is where the original byte at `offset` now sits: an insert exactly at
/// that offset goes *before* it, which is what an instruction, a branch target,
/// an exception handler, a stack-map frame, an `uninitialized` offset and a
/// debug-table start all name.  With `false` the result is the start of the
/// inserted block at `offset`, which is where an exclusive range end belongs so
/// the inserted bytes stay outside the range.
fn map_offset(
    label: &str,
    plan: &CodePlan,
    offset: usize,
    inclusive: bool,
) -> Result<usize, String> {
    if offset == plan.original_len {
        return Ok(plan.end);
    }
    let mapped = if inclusive {
        plan.offsets.get(&offset)
    } else {
        plan.boundaries.get(&offset)
    };
    mapped
        .copied()
        .ok_or_else(|| format!("{label}: offset {offset} is not an instruction boundary"))
}

fn shifted_u2(label: &str, value: usize, what: &str) -> Result<u16, String> {
    u16::try_from(value).map_err(|_| format!("{label}: {what} moves past the 65535-byte limit"))
}

/// Where a branch or switch target lands when it names an instruction that has
/// an insert in front of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetEntry {
    /// Keep the target on the original instruction, so the insert runs only
    /// when control falls into it.  `HEAD`'s insert sits at offset 0 and uses
    /// this: a backward branch to the method's first instruction must not run
    /// the callback a second time.
    Skip,
    /// Point the target at the inserted bytes, so every path that reaches the
    /// instruction runs the callback first.  `RETURN`/`TAIL` need this: a
    /// `return` that is also a branch or switch target - the body of a
    /// `switch` case, `try` handler tail, `if` arm - must still call back.
    Enter,
}

/// The new offset a branch or switch target maps to.
fn map_target(
    label: &str,
    plan: &CodePlan,
    offset: usize,
    entry: TargetEntry,
) -> Result<usize, String> {
    map_offset(label, plan, offset, entry == TargetEntry::Skip)
}

/// Write one switch at its planned offset, re-deriving its padding, its
/// relative `default`/case offsets and (for `tableswitch`) its `low`/`high`.
fn write_switch(
    label: &str,
    out: &mut Vec<u8>,
    site: &SwitchSite,
    plan: &CodePlan,
    entry: TargetEntry,
) -> Result<(), String> {
    let opcode_at = plan
        .offsets
        .get(&site.at)
        .copied()
        .ok_or_else(|| "a switch is missing from the layout".to_owned())?;
    if out.len() != opcode_at {
        return Err("a switch does not sit at its planned offset".to_owned());
    }
    let relative = |target: usize| -> Result<i32, String> {
        let target = map_target(label, plan, target, entry)?;
        i32::try_from(target as i64 - opcode_at as i64)
            .map_err(|_| "a relocated switch offset exceeds its 32-bit operand".to_owned())
    };
    out.push(site.kind.opcode());
    for _ in 0..switch_padding(opcode_at) {
        out.push(0);
    }
    match &site.kind {
        SwitchKind::Table {
            default,
            low,
            high,
            targets,
        } => {
            out.extend_from_slice(&relative(*default)?.to_be_bytes());
            out.extend_from_slice(&low.to_be_bytes());
            out.extend_from_slice(&high.to_be_bytes());
            for target in targets {
                out.extend_from_slice(&relative(*target)?.to_be_bytes());
            }
        }
        SwitchKind::Lookup { default, pairs } => {
            out.extend_from_slice(&relative(*default)?.to_be_bytes());
            let count = i32::try_from(pairs.len())
                .map_err(|_| "lookupswitch has too many match-offset pairs".to_owned())?;
            out.extend_from_slice(&count.to_be_bytes());
            for (key, target) in pairs {
                out.extend_from_slice(&key.to_be_bytes());
                out.extend_from_slice(&relative(*target)?.to_be_bytes());
            }
        }
    }
    Ok(())
}

/// Build the rewritten code from the stable plan: insert each callback at its
/// instruction, re-encode switches and branches, copy everything else.
fn emit_woven(
    label: &str,
    code: &[u8],
    scan: &CodeScan,
    plan: &CodePlan,
    insertions: &[(usize, usize)],
    bytes: &[u8],
    entry: TargetEntry,
) -> Result<Vec<u8>, String> {
    let branches = scan
        .branches
        .iter()
        .map(|branch| (branch.at, branch))
        .collect::<BTreeMap<usize, &BranchSite>>();
    let mut out = Vec::with_capacity(plan.end);
    for instruction in &scan.instructions {
        if inserted_at(insertions, instruction.at).is_some() {
            out.extend_from_slice(bytes);
        }
        if let Some(index) = instruction.switch {
            write_switch(label, &mut out, &scan.switches[index], plan, entry)?;
            continue;
        }
        let at = plan.offsets.get(&instruction.at).copied().ok_or_else(|| {
            format!(
                "{label}: instruction at {} is missing from the layout",
                instruction.at
            )
        })?;
        if out.len() != at {
            return Err(format!(
                "{label}: the woven layout disagrees at offset {}",
                instruction.at
            ));
        }
        let Some(branch) = branches.get(&instruction.at) else {
            out.extend_from_slice(&code[instruction.at..instruction.at + instruction.size]);
            continue;
        };
        let target = map_target(label, plan, branch.target, entry)?;
        let delta = target as i64 - at as i64;
        match widened_opcode(branch.opcode).filter(|_| plan.widened.contains(&branch.at)) {
            Some(opcode) => {
                out.push(opcode);
                let delta = i32::try_from(delta).map_err(|_| {
                    format!("{label}: a widened branch exceeds the 32-bit branch range")
                })?;
                out.extend_from_slice(&delta.to_be_bytes());
            }
            None if branch.wide => {
                out.push(branch.opcode);
                let delta = i32::try_from(delta)
                    .map_err(|_| format!("{label}: a branch exceeds the 32-bit branch range"))?;
                out.extend_from_slice(&delta.to_be_bytes());
            }
            None => {
                out.push(branch.opcode);
                let delta = i16::try_from(delta).map_err(|_| {
                    format!(
                        "{label}: {} at offset {} exceeds the 16-bit branch range after weaving",
                        opcode_name(branch.opcode),
                        branch.at
                    )
                })?;
                out.extend_from_slice(&delta.to_be_bytes());
            }
        }
    }
    if out.len() != plan.end {
        return Err(format!(
            "{label}: the woven code is {} bytes, planned {}",
            out.len(),
            plan.end
        ));
    }
    Ok(out)
}

/// Check that a rewritten buffer is still a well-formed instruction stream.
///
/// The layout is the authority on where things moved; re-scanning is the cheap
/// independent check that every branch and switch target still lands on an
/// instruction boundary and every switch's padding agrees with its offset.
fn verify_woven(label: &str, code: &[u8]) -> Result<(), String> {
    scan_code(label, code)
        .map(|_| ())
        .map_err(|error| format!("{label}: the woven code does not re-scan: {error}"))
}

/// The insert sites, checked and sorted: every one carries the same bytes and
/// lands on an instruction boundary.
fn validate_insertions(
    label: &str,
    scan: &CodeScan,
    code_len: usize,
    insertions: &[(usize, usize)],
    length: usize,
) -> Result<Vec<(usize, usize)>, String> {
    let mut sorted = insertions.to_vec();
    sorted.sort_unstable_by_key(|(at, _)| *at);
    let starts = scan.starts();
    for (index, (at, insert)) in sorted.iter().enumerate() {
        if *insert != length {
            return Err(format!("{label}: inconsistent insert sites"));
        }
        if index > 0 && sorted[index - 1].0 == *at {
            return Err(format!("{label}: two inserts land at offset {at}"));
        }
        if *at >= code_len || !starts.contains(at) {
            return Err(format!(
                "{label}: an insert lands at offset {at}, which is not an instruction boundary"
            ));
        }
    }
    Ok(sorted)
}

/// Rewrite one method's code around a set of same-size inserts.
///
/// The layout pass places every instruction (and re-pads every switch) in the
/// new buffer; then every structure that names a bytecode offset is mapped
/// through it: branch operands, switch tables, exception table ranges,
/// `LineNumberTable`/`LocalVariableTable`/`LocalVariableTypeTable` `start_pc`,
/// `StackMapTable` frame deltas and `uninitialized` offsets.
fn relocate<'a>(
    label: &str,
    code: &CodeLayout<'a>,
    scan: &CodeScan,
    insertions: &[(usize, usize)],
    bytes: &[u8],
    max_stack: u16,
    max_locals: u16,
    entry: TargetEntry,
) -> Result<CodePatch<'a>, String> {
    let insertions = validate_insertions(label, scan, code.code.len(), insertions, bytes.len())?;
    let plan = stable_plan(label, scan, &insertions, entry)?;
    let woven = emit_woven(label, code.code, scan, &plan, &insertions, bytes, entry)?;
    verify_woven(label, &woven)?;

    let mut exception_table = Vec::with_capacity(code.exception_table.len());
    for handler in &code.exception_table {
        // The range keeps the original instructions: an insert exactly at the
        // start lands before it, an insert exactly at the exclusive end lands
        // after it, so the callback is never caught by the target's own handler.
        let (start, end) = if handler.start_pc == handler.end_pc {
            (
                map_offset(label, &plan, handler.start_pc as usize, false)?,
                map_offset(label, &plan, handler.end_pc as usize, false)?,
            )
        } else {
            (
                map_offset(label, &plan, handler.start_pc as usize, true)?,
                map_offset(label, &plan, handler.end_pc as usize, false)?,
            )
        };
        let handler_pc = map_target(label, &plan, handler.handler_pc as usize, entry)?;
        if start > end || handler_pc >= woven.len() {
            return Err(format!(
                "{label}: relocating the exception table produced an invalid range"
            ));
        }
        exception_table.push(ExceptionHandler {
            start_pc: shifted_u2(label, start, "an exception range")?,
            end_pc: shifted_u2(label, end, "an exception range")?,
            handler_pc: shifted_u2(label, handler_pc, "an exception handler")?,
            catch_type: handler.catch_type,
        });
    }

    let attributes = code
        .attributes
        .iter()
        .map(|attribute| relocate_attribute(label, attribute, &plan, &insertions))
        .collect::<Result<Vec<_>, String>>()?;

    Ok(CodePatch {
        code: woven,
        max_stack,
        max_locals,
        exception_table,
        attributes,
    })
}

fn relocate_attribute<'a>(
    label: &str,
    attribute: &CodeAttribute<'a>,
    plan: &CodePlan,
    insertions: &[(usize, usize)],
) -> Result<CodeAttribute<'a>, String> {
    Ok(match attribute {
        CodeAttribute::Raw { name_index, body } => CodeAttribute::Raw {
            name_index: *name_index,
            body,
        },
        CodeAttribute::LineNumbers { name_index, lines } => CodeAttribute::LineNumbers {
            name_index: *name_index,
            lines: lines
                .iter()
                .map(|line| {
                    Ok(LineNumber {
                        start_pc: shifted_u2(
                            label,
                            map_offset(label, plan, line.start_pc as usize, true)?,
                            "a line number",
                        )?,
                        line_number: line.line_number,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?,
        },
        CodeAttribute::LocalVariables {
            name_index,
            entries,
        } => CodeAttribute::LocalVariables {
            name_index: *name_index,
            entries: entries
                .iter()
                .map(|entry| relocate_local(label, entry, plan))
                .collect::<Result<Vec<_>, String>>()?,
        },
        CodeAttribute::StackMap { name_index, frames } => CodeAttribute::StackMap {
            name_index: *name_index,
            frames: relocate_frames(label, frames, plan, insertions)?,
        },
    })
}

fn relocate_local(
    label: &str,
    entry: &LocalVariable,
    plan: &CodePlan,
) -> Result<LocalVariable, String> {
    let start = map_offset(label, plan, entry.start_pc as usize, true)?;
    let end = map_offset(
        label,
        plan,
        entry.start_pc as usize + entry.length as usize,
        false,
    )?;
    if end < start {
        return Err(format!(
            "{label}: relocating a local-variable range produced an invalid range"
        ));
    }
    Ok(LocalVariable {
        start_pc: shifted_u2(label, start, "a local-variable range")?,
        length: shifted_u2(label, end - start, "a local-variable range")?,
        name_index: entry.name_index,
        descriptor_index: entry.descriptor_index,
        index: entry.index,
    })
}

fn relocate_frames(
    label: &str,
    frames: &[StackMapFrame],
    plan: &CodePlan,
    insertions: &[(usize, usize)],
) -> Result<Vec<StackMapFrame>, String> {
    let mut out = Vec::with_capacity(frames.len() * 2);
    for frame in frames {
        // An insert at a frame position puts the callback in front of the
        // instruction that frame describes, which leaves the callback where the
        // verifier still wants a frame: the instruction after an unconditional
        // transfer (the `goto`/switch that named this position) starts a basic
        // block even when nothing branches to the inserted bytes.  The callback
        // runs in exactly the frame's state - it only pushes its arguments and
        // calls `invokestatic` - so the same frame is valid at both offsets.
        if frame.offset > 0 && inserted_at(insertions, frame.offset).is_some() {
            let boundary = map_offset(label, plan, frame.offset, false)?;
            push_frame(label, &mut out, frame, boundary, plan)?;
        }
        let offset = map_offset(label, plan, frame.offset, true)?;
        push_frame(label, &mut out, frame, offset, plan)?;
    }
    Ok(out)
}

/// Append one relocated frame, keeping the table strictly increasing.
fn push_frame(
    label: &str,
    out: &mut Vec<StackMapFrame>,
    frame: &StackMapFrame,
    offset: usize,
    plan: &CodePlan,
) -> Result<(), String> {
    if let Some(previous) = out.last() {
        let previous: &StackMapFrame = previous;
        if offset <= previous.offset {
            return Err(format!(
                "{label}: relocating stack-map frames produced a duplicate"
            ));
        }
    }
    out.push(StackMapFrame {
        frame_type: frame.frame_type,
        offset,
        locals: frame
            .locals
            .iter()
            .map(|verification| relocate_verification(label, verification, plan))
            .collect::<Result<Vec<_>, String>>()?,
        stack: frame
            .stack
            .iter()
            .map(|verification| relocate_verification(label, verification, plan))
            .collect::<Result<Vec<_>, String>>()?,
    });
    Ok(())
}

fn relocate_verification(
    label: &str,
    verification: &VerificationType,
    plan: &CodePlan,
) -> Result<VerificationType, String> {
    Ok(match verification {
        VerificationType::Simple(tag) => VerificationType::Simple(*tag),
        VerificationType::Object(index) => VerificationType::Object(*index),
        VerificationType::Uninitialized(offset) => VerificationType::Uninitialized(shifted_u2(
            label,
            map_offset(label, plan, *offset as usize, true)?,
            "an uninitialized offset",
        )?),
    })
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
    anchor: &str,
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
                "callback {callback_descriptor} takes a trailing CallbackInfo; {anchor} weaving with CallbackInfo needs the interpreter path"
            ));
        }
    }
    if crate::return_desc(callback_descriptor) != "V" {
        return Err(format!(
            "callback {callback_descriptor} must return void for {anchor} weaving"
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

/// A target method prepared for weaving: its parsed layout, the index of the
/// `Code` attribute every anchor rewrites, and the descriptor facts the
/// callback checks need.
struct Target<'a> {
    layout: Layout<'a>,
    class_name: String,
    method_index: usize,
    attribute_index: usize,
    name: String,
    descriptor: String,
    is_static: bool,
}

impl<'a> Target<'a> {
    fn label(&self) -> String {
        format!("{}.{}{}", self.class_name, self.name, self.descriptor)
    }

    fn code(&self) -> Result<&CodeLayout<'a>, String> {
        self.layout.methods[self.method_index]
            .attributes
            .get(self.attribute_index)
            .and_then(|attribute| attribute.code.as_ref())
            .ok_or_else(|| format!("{}: method has no Code attribute", self.label()))
    }
}

/// Shared validation for every anchor: owner match, condy, the target method,
/// its `Code` attribute, and the opcodes no insert can keep valid.
fn prepare_target<'a>(class: &'a ClassFile, target: &MethodRef) -> Result<Target<'a>, String> {
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

    // Reject the opcodes no insert can keep valid before any plan runs.
    let label = format!("{}.{name}{descriptor}", class.name);
    scan_code(&label, code.code)?;

    let is_static = method.access & ACC_STATIC != 0;
    Ok(Target {
        layout,
        class_name: class.name.clone(),
        method_index,
        attribute_index,
        name,
        descriptor,
        is_static,
    })
}

/// The instruction bytes for one callback call, and the stack/local slots they
/// need.
struct CallbackCall {
    bytes: Vec<u8>,
    /// Operand-stack slots the loads push at their peak.
    stack_slots: u16,
    /// Local slots the target's arguments (and receiver) occupy.
    local_slots: u16,
}

/// Emit the loads for one callback and its `invokestatic`.
///
/// `local_slots` is the highest local slot the loads read plus one, so the
/// caller can raise `max_locals` when the class declares fewer; `stack_slots`
/// is the number of operand-stack slots they push at their peak, so the caller
/// can raise `max_stack` conservatively.
fn callback_call(
    target: &Target<'_>,
    callback: &MethodRef,
    takes_receiver: bool,
    pool: &mut PoolBuilder<'_>,
) -> Result<CallbackCall, String> {
    let arguments = crate::descriptor_arguments(&target.descriptor);
    let mut inserted = Vec::new();
    let mut slot = if target.is_static { 0 } else { 1 };
    let mut slots = 0u16;
    if takes_receiver {
        inserted.push(0x2a); // aload_0
        slots += 1;
    }
    for argument in &arguments {
        let (opcode, width) = load_opcode(argument)?;
        // A slot above 255 needs the `wide` prefix; the class-file format caps a
        // method's parameter slots well below that, so this only runs for a
        // hand-built descriptor the verifier would reject anyway, but emitting
        // the wide form keeps the insert faithful to the slots it is told to
        // read instead of silently truncating the index.
        if slot > u8::MAX as u16 {
            inserted.push(0xc4); // wide
            inserted.push(opcode);
            inserted.extend_from_slice(&slot.to_be_bytes());
        } else {
            inserted.push(opcode);
            inserted.push(slot as u8);
        }
        slot = slot.checked_add(width).ok_or("local slot overflow")?;
        slots = slots.checked_add(width).ok_or("argument slot overflow")?;
    }
    let callback_index = pool.method_ref(callback)?;
    inserted.push(0xb8); // invokestatic
    inserted.extend_from_slice(&callback_index.to_be_bytes());
    Ok(CallbackCall {
        bytes: inserted,
        stack_slots: slots,
        local_slots: slot,
    })
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
/// The insert moves every branch target, switch table, exception range,
/// stack-map frame and debug-table offset after it; see [`crate::weaver`] for
/// the rewrite.
///
/// Errors name the reason a method is outside the subset instead of guessing:
/// condy, abstract/native methods, a mismatched callback descriptor,
/// `CallbackInfo` parameters, a `wide` prefix that is not a local-variable
/// instruction, or a conditional branch whose target no longer fits 16 bits.
pub fn weave_head(
    class: &ClassFile,
    target: &MethodRef,
    callback: &MethodRef,
) -> Result<Vec<u8>, String> {
    let target = prepare_target(class, target)?;
    let label = target.label();
    let takes_receiver = callback_arguments(
        &target.class_name,
        target.is_static,
        &target.descriptor,
        &callback.descriptor,
        "HEAD",
    )?;

    let mut pool = PoolBuilder::new(&class.pool);
    let call = callback_call(&target, callback, takes_receiver, &mut pool)?;
    let code = target.code()?;
    let scan = scan_code(&label, code.code)?;
    if code.code.len() + call.bytes.len() > MAX_CODE_LENGTH {
        return Err(format!(
            "{label}: woven code would exceed the class-file limit of {MAX_CODE_LENGTH} bytes"
        ));
    }
    let max_stack = u32::from(code.max_stack)
        .checked_add(u32::from(call.stack_slots))
        .and_then(|value| value.checked_add(1))
        .and_then(|value| u16::try_from(value).ok())
        .ok_or_else(|| format!("{label}: woven max_stack overflows u16"))?;
    let max_locals = code.max_locals.max(call.local_slots);
    let patch = relocate(
        &label,
        code,
        &scan,
        &[(0, call.bytes.len())],
        &call.bytes,
        max_stack,
        max_locals,
        TargetEntry::Skip,
    )?;

    emit(
        &target.layout,
        Some(Patch {
            method: target.method_index,
            attribute: target.attribute_index,
            code: patch,
        }),
        &pool.added,
    )
}

/// Insert a void callback immediately before the target's `return`
/// instructions.
///
/// [`ReturnMode::Return`] inserts before every return-family instruction in
/// the method; [`ReturnMode::Tail`] inserts before the last one only, matching
/// Mixin's `RETURN` and `TAIL` anchors.
///
/// Only `void` targets are woven.  A non-void target would need a
/// `CallbackInfoReturnable` carrying the target's boxed result plus an override
/// branch that reloads or replaces it, which needs object construction and
/// synthesised stack-map frames this bounded insert cannot emit; that target is
/// rejected with a reason and stays on the interpreter path.
///
/// The callback is invoked with `invokestatic`.  A callback takes the target's
/// arguments in order and returns `void`.  An instance target's callback may
/// take the target instance first, which the inserted `aload_0` supplies.
/// Each insert relocates the offsets after it exactly as [`weave_head`] does,
/// so a branchy method, a loop, a `tableswitch`/`lookupswitch` (whose padding is
/// re-derived from its new offset), a `wide` local and an exception handler
/// keep their targets and frames.  Unlike a `HEAD` insert, a `RETURN` insert is
/// entered by the branches and switch cases that named the `return`.
/// Every other rejection matches
/// [`weave_head`]: condy, abstract/native methods, a mismatched callback
/// descriptor, or `CallbackInfo`/`CallbackInfoReturnable` parameters.
pub fn weave_return(
    class: &ClassFile,
    target: &MethodRef,
    callback: &MethodRef,
    mode: ReturnMode,
) -> Result<Vec<u8>, String> {
    let target = prepare_target(class, target)?;
    let label = target.label();
    let anchor = mode.anchor();
    if crate::return_desc(&target.descriptor) != "V" {
        return Err(format!(
            "{label}: {anchor} weaving a non-void target needs a CallbackInfoReturnable carrying the target's boxed result and an override branch that reloads or replaces it; the bounded weaver cannot construct the object or synthesise the StackMapTable frames such a branch needs, so only void targets are woven (the interpreter handles this one)"
        ));
    }
    let takes_receiver = callback_arguments(
        &target.class_name,
        target.is_static,
        &target.descriptor,
        &callback.descriptor,
        anchor,
    )?;

    let code = target.code()?;
    let scan = scan_code(&label, code.code)?;
    let sites: Vec<usize> = match mode {
        ReturnMode::Return => scan.returns.clone(),
        ReturnMode::Tail => scan.returns.last().copied().into_iter().collect(),
    };
    if sites.is_empty() {
        return Err(format!(
            "{label}: {anchor} weaving found no return instruction"
        ));
    }

    let mut pool = PoolBuilder::new(&class.pool);
    let call = callback_call(&target, callback, takes_receiver, &mut pool)?;
    if code.code.len() + call.bytes.len() * sites.len() > MAX_CODE_LENGTH {
        return Err(format!(
            "{label}: woven code would exceed the class-file limit of {MAX_CODE_LENGTH} bytes"
        ));
    }
    let max_stack = u32::from(code.max_stack)
        .checked_add(u32::from(call.stack_slots))
        .and_then(|value| u16::try_from(value).ok())
        .ok_or_else(|| format!("{label}: woven max_stack overflows u16"))?;
    let max_locals = code.max_locals.max(call.local_slots);
    let insertions = sites
        .iter()
        .map(|site| (*site, call.bytes.len()))
        .collect::<Vec<_>>();
    let patch = relocate(
        &label,
        code,
        &scan,
        &insertions,
        &call.bytes,
        max_stack,
        max_locals,
        TargetEntry::Enter,
    )?;

    emit(
        &target.layout,
        Some(Patch {
            method: target.method_index,
            attribute: target.attribute_index,
            code: patch,
        }),
        &pool.added,
    )
}

/// Apply every plan that targets `class`: `HEAD` plans first, then
/// `RETURN`/`TAIL` plans, each group in declaration order.
///
/// Plans for other classes are skipped.  A matching plan that asks for an
/// anchor or object this file cannot produce is an error rather than a silent
/// skip: `INVOKE` call-site rewriting, cancellation (`CallbackInfo`), an
/// unknown anchor, and more than `MAX_WEAVE_PLANS_PER_CLASS` plans for one
/// class.
pub fn weave_class(class: &ClassFile, plans: &[MixinInjectionPlan]) -> Result<Vec<u8>, String> {
    let matching = plans
        .iter()
        .filter(|plan| plan.target.owner == class.name)
        .collect::<Vec<_>>();
    if matching.len() > MAX_WEAVE_PLANS_PER_CLASS {
        return Err(format!(
            "class {} has {} matching weave plans; the bounded weaver applies at most {MAX_WEAVE_PLANS_PER_CLASS}",
            class.name,
            matching.len()
        ));
    }
    for plan in &matching {
        if plan.cancellable {
            return Err(format!(
                "{}.{}: cancellable {} weaving needs a runtime-owned CallbackInfo, which the bounded weaver cannot construct",
                plan.target.owner, plan.target.name, plan.at
            ));
        }
        if !matches!(plan.at.as_str(), "HEAD" | "RETURN" | "TAIL") {
            return Err(format!(
                "{}.{}: anchor {} needs the interpreter path; the bounded weaver applies HEAD, RETURN and TAIL",
                plan.target.owner, plan.target.name, plan.at
            ));
        }
    }

    let mut current = class.write()?;
    for plan in matching.iter().filter(|plan| plan.at == "HEAD") {
        let next = ClassFile::parse(&current)
            .map_err(|error| format!("rewritten class did not parse again: {error}"))?;
        current = weave_head(&next, &plan.target, &plan.callback)?;
    }
    for plan in matching
        .iter()
        .filter(|plan| plan.at == "RETURN" || plan.at == "TAIL")
    {
        let mode = if plan.at == "TAIL" {
            ReturnMode::Tail
        } else {
            ReturnMode::Return
        };
        let next = ClassFile::parse(&current)
            .map_err(|error| format!("rewritten class did not parse again: {error}"))?;
        current = weave_return(&next, &plan.target, &plan.callback, mode)?;
    }
    Ok(current)
}

/// Apply the `HEAD` plans that target `class`, in plan order.
///
/// Only plans whose [`MixinInjectionPlan::at`] is `"HEAD"`, whose target owner
/// is this class and whose target method exists are applied; plans for other
/// classes and every other anchor are left to the interpreter.  Cancellable
/// plans are skipped because their callback takes a `CallbackInfo`, which this
/// bytecode insert cannot construct.
///
/// [`weave_class`] is the whole-class entry point: it also applies `RETURN`
/// and `TAIL` plans, and rejects anchors and cancellable plans it cannot weave
/// instead of skipping them.
///
/// Not supported here, and left on the interpreter path: `RETURN`, `TAIL` and
/// `INVOKE` anchors; cancellation; and `CallbackInfo`/`CallbackInfoReturnable`
/// parameters.  Errors from a matching plan (ambiguous target descriptor,
/// callback mismatch, a condy pool, a `wide` prefix that is not a
/// local-variable instruction, a conditional branch past the 16-bit range)
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

/// Weave a set of parsed classes, grouped by the class each plan targets.
///
/// Classes with matching plans go through [`weave_class`]; every other class is
/// re-encoded unchanged by [`ClassFile::write`], so the result is a complete
/// replacement set keyed like `classes`.  A plan that targets a class absent
/// from the set is an error rather than a silent skip, and the per-class plan
/// bound is `MAX_WEAVE_PLANS_PER_CLASS`.
pub fn weave_classes(
    classes: &BTreeMap<String, ClassFile>,
    plans: &[MixinInjectionPlan],
) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let mut by_owner: BTreeMap<&str, Vec<&MixinInjectionPlan>> = BTreeMap::new();
    for plan in plans {
        by_owner
            .entry(plan.target.owner.as_str())
            .or_default()
            .push(plan);
    }
    for owner in by_owner.keys() {
        if !classes.values().any(|class| class.name == *owner) {
            return Err(format!(
                "weave plan targets class {owner}, which is not in the class set"
            ));
        }
    }

    let mut woven = BTreeMap::new();
    for (key, class) in classes {
        let bytes = match by_owner.get(class.name.as_str()) {
            Some(class_plans) => {
                let owned = class_plans
                    .iter()
                    .map(|plan| (*plan).clone())
                    .collect::<Vec<_>>();
                weave_class(class, &owned)?
            }
            None => class.write()?,
        };
        woven.insert(key.clone(), bytes);
    }
    Ok(woven)
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

    /// A `demo/T.m(II)I` whose `Code` carries a `LineNumberTable`, a
    /// `LocalVariableTable` and a one-frame `StackMapTable`:
    ///
    /// ```text
    /// 0: iload_1
    /// 1: ifne +4      -> 5
    /// 4: ireturn
    /// 5: iload_2
    /// 6: ireturn
    /// ```
    ///
    /// Pool: 1 `demo/T`, 2 Class 1, 3 `java/lang/Object`, 4 Class 3, 5 `m`,
    /// 6 `(II)I`, 7 `Code`, 8 `LineNumberTable`, 9 `LocalVariableTable`,
    /// 10 `StackMapTable`.
    fn frames_fixture() -> Vec<u8> {
        let mut out = Vec::new();
        u4(&mut out, 0xCAFE_BABE);
        u2(&mut out, 0);
        u2(&mut out, 52);
        u2(&mut out, 11);
        utf8(&mut out, "demo/T");
        out.extend_from_slice(&[7, 0, 1]);
        utf8(&mut out, "java/lang/Object");
        out.extend_from_slice(&[7, 0, 3]);
        utf8(&mut out, "m");
        utf8(&mut out, "(II)I");
        utf8(&mut out, "Code");
        utf8(&mut out, "LineNumberTable");
        utf8(&mut out, "LocalVariableTable");
        utf8(&mut out, "StackMapTable");
        u2(&mut out, 0x0021); // access
        u2(&mut out, 2); // this
        u2(&mut out, 4); // super
        u2(&mut out, 0); // interfaces
        u2(&mut out, 0); // fields
        u2(&mut out, 1); // methods
        u2(&mut out, 0x0001); // public
        u2(&mut out, 5);
        u2(&mut out, 6);
        u2(&mut out, 1); // attributes
        u2(&mut out, 7);

        let mut code_attributes = Vec::new();
        // LineNumberTable: (0, 1) and (5, 2).
        u2(&mut code_attributes, 8);
        let mut attribute = Vec::new();
        u2(&mut attribute, 2);
        for (start_pc, line) in [(0u16, 1u16), (5, 2)] {
            u2(&mut attribute, start_pc);
            u2(&mut attribute, line);
        }
        u4(&mut code_attributes, attribute.len() as u32);
        code_attributes.extend_from_slice(&attribute);
        // LocalVariableTable: slot 1 over [0, 7).
        u2(&mut code_attributes, 9);
        let mut attribute = Vec::new();
        u2(&mut attribute, 1);
        u2(&mut attribute, 0); // start_pc
        u2(&mut attribute, 7); // length
        u2(&mut attribute, 5); // name
        u2(&mut attribute, 6); // descriptor
        u2(&mut attribute, 1); // slot
        u4(&mut code_attributes, attribute.len() as u32);
        code_attributes.extend_from_slice(&attribute);
        // StackMapTable: one full frame at offset 5.
        u2(&mut code_attributes, 10);
        let mut attribute = Vec::new();
        u2(&mut attribute, 1);
        attribute.push(255);
        u2(&mut attribute, 5); // offset_delta
        u2(&mut attribute, 3); // locals
        attribute.push(7);
        u2(&mut attribute, 2); // demo/T
        attribute.push(1); // Integer
        attribute.push(1); // Integer
        u2(&mut attribute, 0); // stack
        u4(&mut code_attributes, attribute.len() as u32);
        code_attributes.extend_from_slice(&attribute);

        let mut body = Vec::new();
        u2(&mut body, 1); // max_stack
        u2(&mut body, 3); // max_locals
        u4(&mut body, 7);
        body.extend_from_slice(&[0x1b, 0x9a, 0x00, 0x04, 0xac, 0x1c, 0xac]);
        u2(&mut body, 0); // exception table
        u2(&mut body, 3); // Code attributes
        body.extend_from_slice(&code_attributes);
        u4(&mut out, body.len() as u32);
        out.extend_from_slice(&body);
        u2(&mut out, 0); // class attributes
        out
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

    /// Parse `Code` out of a woven class so the assertions read the written
    /// bytes, not the in-memory patch.
    fn woven_code(woven: &[u8], name: &str, descriptor: &str) -> (Vec<u8>, u16, u16) {
        let class = ClassFile::parse(woven).unwrap();
        assert_eq!(class.write().unwrap(), woven, "woven class round-trips");
        let method = class.method(name, descriptor).unwrap();
        (
            method.code.clone().unwrap(),
            method.max_stack,
            method.max_locals,
        )
    }

    #[test]
    fn head_relocates_a_branch_target() {
        // `ifne +3; return` — the branch jumps over the return to offset 3.
        let bytes = fixture(0x0001, "(I)I", 2, &[0x99, 0x00, 0x03, 0xb1], &[]);
        let class = ClassFile::parse(&bytes).unwrap();
        let branchy = MethodRef {
            descriptor: "(I)I".into(),
            ..target()
        };
        let woven = weave_head(&class, &branchy, &callback("(Ldemo/T;I)V")).unwrap();
        let (code, max_stack, max_locals) = woven_code(&woven, "m", "(I)I");
        // aload_0 (1) + iload_1 (2) + invokestatic (3) = 6 bytes.
        assert_eq!(&code[..6], &[0x2a, 0x15, 0x01, 0xb8, 0x00, 0x0d]);
        assert_eq!(code[6], 0x99);
        // The return moved to 9, and the branch now sits at 6.  JVM offsets are
        // relative to the opcode, so the operand is 9 - 6 = 3.
        assert_eq!(i16::from_be_bytes([code[7], code[8]]), 3);
        assert_eq!(code[9], 0xb1);
        assert_eq!(max_stack, 2 + 2 + 1);
        assert_eq!(max_locals, 2);
    }

    #[test]
    fn head_relocates_an_exception_table() {
        // `ifne +4; return; ireturn` with a handler covering [3, 5) that starts
        // at offset 4, so every column has to move by the insert.
        let mut handler = Vec::new();
        u2(&mut handler, 3); // start_pc
        u2(&mut handler, 5); // end_pc
        u2(&mut handler, 4); // handler_pc
        u2(&mut handler, 0); // catch_type: catch-all
        let bytes = fixture(
            0x0001,
            "(II)I",
            3,
            &[0x99, 0x00, 0x04, 0xb1, 0xac],
            &handler,
        );
        let class = ClassFile::parse(&bytes).unwrap();
        let woven = weave_head(&class, &target(), &callback("(Ldemo/T;II)V")).unwrap();
        let (code, _, _) = woven_code(&woven, "m", "(II)I");
        // aload_0 + iload_1 + iload_2 + invokestatic = 8 bytes.
        assert_eq!(code.len(), 5 + 8);
        // The table sits between the code and the two (empty) attribute counts.
        let table = &woven[woven.len() - 12..woven.len() - 4];
        assert_eq!(
            table,
            &[
                0, 11, // start_pc: 3 + 8
                0, 13, // end_pc: 5 + 8
                0, 12, // handler_pc: 4 + 8
                0, 0, // catch-all
            ]
        );
    }

    #[test]
    fn head_relocates_debug_tables_and_frames() {
        let bytes = frames_fixture();
        let class = ClassFile::parse(&bytes).unwrap();
        let woven = weave_head(&class, &target(), &callback("(Ldemo/T;II)V")).unwrap();
        let (_, _, _) = woven_code(&woven, "m", "(II)I");
        let layout = Layout::parse(&woven).unwrap();
        let method = &layout.methods[0];
        let code = method.attributes[0].code.as_ref().unwrap();
        let mut frame_lines = Vec::new();
        for attribute in &code.attributes {
            match attribute {
                CodeAttribute::LineNumbers { lines, .. } => {
                    for line in lines {
                        frame_lines.push(("line", line.start_pc));
                    }
                }
                CodeAttribute::LocalVariables { entries, .. } => {
                    for entry in entries {
                        frame_lines.push(("local", entry.start_pc));
                        frame_lines.push(("length", entry.length));
                    }
                }
                CodeAttribute::StackMap { frames, .. } => {
                    for frame in frames {
                        frame_lines.push(("frame", frame.offset as u16));
                    }
                }
                CodeAttribute::Raw { .. } => {}
            }
        }
        // The fixture inserts 8 bytes at offset 0; every recorded offset moves
        // with it and the local-variable range keeps its own length.
        assert_eq!(
            frame_lines,
            vec![
                ("line", 8),
                ("line", 13),
                ("local", 8),
                ("length", 7),
                ("frame", 13),
            ]
        );
    }

    #[test]
    fn head_relocates_a_tableswitch() {
        // 0: tableswitch (three padding bytes), default -> 24, low 0, high 1,
        // case 0 -> 25, case 1 -> 26; then three `ireturn`.
        let code = [
            0xaa, 0, 0, 0, // opcode and alignment padding
            0, 0, 0, 24, // default
            0, 0, 0, 0, // low
            0, 0, 0, 1, // high
            0, 0, 0, 25, // case 0
            0, 0, 0, 26, // case 1
            0xac, 0xac, 0xac,
        ];
        assert_eq!(code.len(), 27);
        let bytes = fixture(0x0001, "(II)I", 3, &code, &[]);
        let class = ClassFile::parse(&bytes).unwrap();
        // `iload_1; iload_2; invokestatic` is 7 bytes, which is not a multiple
        // of four: the switch's padding has to be re-derived, so it shrinks from
        // 24 bytes to 21 and everything after it moves a second time.
        let woven = weave_head(&class, &target(), &callback("(II)V")).unwrap();
        let (woven_code, _, _) = woven_code(&woven, "m", "(II)I");
        assert_eq!(woven_code.len(), 27 + 7 - 3);
        assert_eq!(woven_code[7], 0xaa);
        assert_eq!(switch_header(&woven_code, 7), (21, 0, 1, vec![22, 23]));
        assert_eq!(&woven_code[28..], &[0xac, 0xac, 0xac]);
    }

    #[test]
    fn head_relocates_a_lookupswitch() {
        // 0: lookupswitch (three padding bytes), default -> 28, two pairs
        // (1 -> 29, 100 -> 30); then three `ireturn`.
        let code = [
            0xab, 0, 0, 0, // opcode and alignment padding
            0, 0, 0, 28, // default
            0, 0, 0, 2, // npairs
            0, 0, 0, 1, 0, 0, 0, 29, // key 1 -> 29
            0, 0, 0, 100, 0, 0, 0, 30, // key 100 -> 30
            0xac, 0xac, 0xac,
        ];
        assert_eq!(code.len(), 31);
        let bytes = fixture(0x0001, "(II)I", 3, &code, &[]);
        let class = ClassFile::parse(&bytes).unwrap();
        let woven = weave_head(&class, &target(), &callback("(II)V")).unwrap();
        let (woven_code, _, _) = woven_code(&woven, "m", "(II)I");
        // Padding drops from three bytes to none, so the switch shrinks 3 bytes
        // and the three returns land at 32, 33 and 34.
        assert_eq!(woven_code.len(), 31 + 7 - 3);
        assert_eq!(woven_code[7], 0xab);
        assert_eq!(woven_code[8..12], 25i32.to_be_bytes());
        assert_eq!(woven_code[12..16], 2i32.to_be_bytes());
        assert_eq!(woven_code[16..20], 1i32.to_be_bytes());
        assert_eq!(woven_code[20..24], 26i32.to_be_bytes());
        assert_eq!(woven_code[24..28], 100i32.to_be_bytes());
        assert_eq!(woven_code[28..32], 27i32.to_be_bytes());
        assert_eq!(&woven_code[32..], &[0xac, 0xac, 0xac]);
    }

    #[test]
    fn switch_padding_grows_when_an_insert_moves_a_switch_off_the_grid() {
        // The same jump table behind one `nop`, so the switch starts at offset 1
        // and needs two padding bytes.  A 7-byte insert puts it at 8, where it
        // needs three: the instruction grows by one on top of the insert.
        let code = [
            0x00, // nop
            0xaa, 0, 0, // opcode and two padding bytes
            0, 0, 0, 20, // default -> 21
            0, 0, 0, 0, // low
            0, 0, 0, 0, // high
            0, 0, 0, 19, // case 0 -> 20
            0xac, 0xac, // ireturn, ireturn
        ];
        assert_eq!(code.len(), 22);
        let bytes = fixture(0x0001, "(II)I", 3, &code, &[]);
        let class = ClassFile::parse(&bytes).unwrap();
        let woven = weave_head(&class, &target(), &callback("(II)V")).unwrap();
        let (woven_code, _, _) = woven_code(&woven, "m", "(II)I");
        assert_eq!(woven_code.len(), code.len() + 7 + 1);
        assert_eq!(&woven_code[..4], &[0x15, 0x01, 0x15, 0x02]);
        assert_eq!(woven_code[4], 0xb8); // invokestatic
        assert_eq!(woven_code[7], 0x00, "the nop moved to 7");
        assert_eq!(woven_code[8], 0xaa);
        // default -> 29, the case -> 28, both relative to the switch at 8.
        assert_eq!(switch_header(&woven_code, 8), (21, 0, 0, vec![20]));
        assert_eq!(&woven_code[28..], &[0xac, 0xac]);
    }

    #[test]
    fn head_weaves_a_method_with_wide_locals() {
        // `wide iload 256; wide iinc 257 by 3; ireturn`.  `javac` cannot emit
        // this (a source method's slots stop below 256), so it is assembled by
        // hand: the weaver has to walk the 4- and 6-byte wide forms instead of
        // treating `wide` as a one-byte opcode.
        let code = [
            0xc4, 0x15, 0x01, 0x00, // wide iload 256
            0xc4, 0x84, 0x01, 0x01, 0x00, 0x03, // wide iinc 257, 3
            0xac, // ireturn
        ];
        let bytes = fixture(0x0001, "()I", 260, &code, &[]);
        let class = ClassFile::parse(&bytes).unwrap();
        let target = MethodRef {
            owner: "demo/T".into(),
            name: "m".into(),
            descriptor: "()I".into(),
        };
        let woven = weave_head(&class, &target, &callback("(Ldemo/T;)V")).unwrap();
        let (woven_code, _, max_locals) = woven_code(&woven, "m", "()I");
        assert_eq!(woven_code.len(), code.len() + 4); // aload_0 + invokestatic
        assert_eq!(&woven_code[4..], &code);
        assert_eq!(max_locals, 260, "the wide slots raise nothing");
    }

    #[test]
    fn head_grows_a_goto_that_would_leave_the_16_bit_range() {
        // `goto +32767; return; 32763 nops; return`: the goto sits exactly at
        // the edge of its 16-bit operand.  A RETURN insert in front of the first
        // return pushes the target past it, so the goto has to become `goto_w`.
        let mut code = vec![0xa7, 0x7f, 0xff, 0xb1];
        code.resize(4 + 32_763, 0x00);
        code.push(0xb1);
        assert_eq!(code.len(), 32_768);
        let bytes = fixture(0x0009, "()V", 0, &code, &[]);
        let class = ClassFile::parse(&bytes).unwrap();
        let target = MethodRef {
            owner: "demo/T".into(),
            name: "m".into(),
            descriptor: "()V".into(),
        };
        let woven = weave_return(&class, &target, &callback("()V"), ReturnMode::Return).unwrap();
        let (woven_code, _, _) = woven_code(&woven, "m", "()V");
        // Two 3-byte inserts, plus the two bytes the widening adds.
        assert_eq!(woven_code.len(), code.len() + 6 + 2);
        assert_eq!(woven_code[0], 0xc8, "the goto grew to goto_w");
        // A RETURN target enters the callback: 32767 + 3 (the first insert) +
        // 2 (the wider goto), the inserted call itself starting there.
        assert_eq!(woven_code[1..5], 32_772i32.to_be_bytes());
        assert_eq!(woven_code[5], 0xb8, "the first callback sits before it");
        assert_eq!(woven_code[8], 0xb1);
        assert_eq!(*woven_code.last().unwrap(), 0xb1);
    }

    #[test]
    fn head_rejects_a_conditional_branch_past_the_16_bit_range() {
        // The same shape with `ifeq`, which has no wide encoding: splitting it
        // would add a branch and a frame the weaver cannot derive, so the method
        // is rejected with the distance rather than written invalid.
        let mut code = vec![0x99, 0x7f, 0xff, 0xb1];
        code.resize(4 + 32_763, 0x00);
        code.push(0xb1);
        let bytes = fixture(0x0009, "()V", 0, &code, &[]);
        let class = ClassFile::parse(&bytes).unwrap();
        let target = MethodRef {
            owner: "demo/T".into(),
            name: "m".into(),
            descriptor: "()V".into(),
        };
        let error =
            weave_return(&class, &target, &callback("()V"), ReturnMode::Return).unwrap_err();
        assert!(error.contains("ifeq"), "{error}");
        assert!(error.contains("16-bit"), "{error}");
        assert!(error.contains("32770"), "{error}");
    }

    /// `(default, low, high, cases)` of the `tableswitch` at `at`, with the
    /// relative offsets kept relative to the opcode.
    fn switch_header(code: &[u8], at: usize) -> (i32, i32, i32, Vec<i32>) {
        assert_eq!(code[at], 0xaa);
        let mut field = at + 1 + super::switch_padding(at);
        let mut read = || {
            let value = i32::from_be_bytes([
                code[field],
                code[field + 1],
                code[field + 2],
                code[field + 3],
            ]);
            field += 4;
            value
        };
        let default = read();
        let low = read();
        let high = read();
        let cases = (low..=high).map(|_| read()).collect();
        (default, low, high, cases)
    }
}
