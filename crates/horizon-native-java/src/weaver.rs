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
//! [`weave_return`] inserts a callback immediately before every return-family
//! instruction (`return`, `ireturn`, `areturn`, `freturn`, `dreturn`,
//! `lreturn`) for [`ReturnMode::Return`], or before only the last one for
//! [`ReturnMode::Tail`] (Mixin's `TAIL`).  The callback carries the target's
//! arguments, plus the receiver when it declares it.
//!
//! A `void` target weaves the plain `invokestatic`.  A non-`void` target also
//! carries its result: the callback's last argument is a
//! `CallbackInfoReturnable`, which the insert builds around the target's boxed
//! value - the same class [`callback_info_returnable_class`] adds to the jar:
//!
//! ```text
//! invokestatic  java/lang/Integer.valueOf                (primitive returns only)
//! astore        <fresh local>
//! new           org/spongepowered/asm/mixin/injection/callback/CallbackInfoReturnable
//! dup
//! aload         <fresh local>
//! invokespecial CallbackInfoReturnable.<init>(Ljava/lang/Object;)V
//! astore        <fresh local>
//! <load every callback argument: the receiver, then the target's arguments>
//! aload         <fresh local>
//! invokestatic  callback
//! aload         <fresh local>
//! invokevirtual CallbackInfoReturnable.getReturnValue()Ljava/lang/Object;
//! checkcast     <wrapper for a primitive, the return type otherwise>
//! invokevirtual <wrapper>.<type>Value                    (primitive returns only)
//! ```
//!
//! The object is built before the arguments because the callback consumes the
//! topmost one as its last argument, and the second `aload` keeps the woven
//! method's own reference for `getReturnValue`.
//!
//! The object starts with the target's own boxed result, so a callback that
//! never calls `setReturnValue` changes nothing and the last `setReturnValue`
//! wins.  A `cancel()` keeps the override, because the method returns
//! `getReturnValue()` either way - which is what Mixin's own `RETURN` handler
//! does for a cancellable non-void injection.  The insert therefore adds no
//! branch, and the fresh local is the only verifier state it touches.
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
//! A conditional branch (`if*`, `ifnull`, `ifnonnull`) has no wide encoding, so
//! one whose relocated target no longer fits 16 bits is split in place:
//!
//! ```text
//! if<inverse> +8       (skips the wide jump when the original condition is false)
//! goto_w target
//! <the original fallthrough instruction>
//! ```
//!
//! The fallthrough instruction becomes a branch target and therefore needs a
//! `StackMapTable` frame.  The frame's locals are derived by following the
//! straight-line region from the nearest preceding frame (or the method entry)
//! with a bounded type model - local loads and stores, constants, fields,
//! calls, arrays, `checkcast` and the stack shuffles - so the frame is exact
//! rather than guessed; an instruction the model cannot follow is a reason, not
//! a guessed frame.  When such a frame is added, every frame in the method is
//! re-encoded as a `full_frame`, because a later `append`/`chop` frame's delta
//! is relative to the frame in front of it.
//!
//! An insert sits in front of the instruction it precedes, and two details
//! follow from that.  A `RETURN`/`TAIL` insert is *entered*: a branch or switch
//! case that named the `return` now lands on the callback, so a `return` that is
//! also a jump target - the body of a `switch` case, an `if` arm - still calls
//! back.  A `HEAD` insert is *skipped*: a backward branch to the method's first
//! instruction lands after the callback, so the callback runs once at entry.
//! When an insert lands on a `StackMapTable` frame position the frame is emitted
//! before the callback, because the bytes after an unconditional transfer start
//! a basic block whether or not anything branches to them and a `HEAD` or `void`
//! callback runs in the same state.  Such an insert leaves stack and locals
//! untouched, so the frame is emitted at the instruction too; a non-void
//! `RETURN` insert writes a fresh local, so only the insert keeps the frame and
//! the `return` behind it - which now starts no basic block - carries none.
//!
//! `wide`-prefixed local instructions (`wide` load/store, `iinc`, `ret`) are
//! walked at their real length and copied verbatim, so a method whose local
//! slots exceed 255 weaves exactly like one below the limit.
//!
//! `max_stack` is raised conservatively (HEAD: `original + argument slots + 1`;
//! RETURN/TAIL: `original + argument slots`, plus three slots for the
//! `CallbackInfoReturnable` construction of a non-void target), `max_locals` is
//! raised to the argument slots when the class declares fewer (and by one for
//! the fresh local of a non-void insert), and the `code_length` and `Code`
//! attribute length follow the rewritten code.  Attributes the weaver does not
//! read (`Exceptions`, type annotations, ...) are still copied byte-for-byte.
//!
//! The callback descriptor must consume exactly the target's arguments and
//! return `void`.  An instance target's callback may instead take the target
//! instance as an extra first parameter, which the inserted `aload_0` supplies.
//! A non-void `RETURN`/`TAIL` callback must end with a
//! `CallbackInfoReturnable`, which the insert supplies.  A cancellable `void`
//! `HEAD` callback ends with a `CallbackInfo` instead: the insert constructs
//! [`callback_info_class`], passes it, and emits
//! `if (!ci.isCancelled()) goto body; return`, so a cancelled callback skips the
//! target body and the method answers with the `void` default.
//!
//! [`weave_class`] applies every matching `HEAD` plan first, then every
//! `RETURN`/`TAIL` plan, re-parsing the class between edits and skipping plans
//! for other classes.  [`weave_classes`] groups plans by target owner, weaves
//! each affected class with [`weave_class`], and re-encodes every other class
//! unchanged.  [`weave_classes_with_extras`] additionally reports the synthetic
//! classes a rewrite references ([`WovenClasses::added`]), which is what a jar
//! writer has to add beside the rewritten class.
//!
//! # What it does not support
//!
//! [`weave_plans`] applies only plain `HEAD` plans, in plan order, and skips
//! plans that target another class or ask for cancellation; [`weave_class`]
//! instead rejects an anchor it cannot weave and a per-class plan overflow with
//! a reason.
//!
//! `@At("INVOKE")` is applied by [`weave_invoke`] (and [`weave_invoke_plan`]),
//! not by [`weave_class`]: a call-site hook is a different edit from an
//! around-the-body one, and keeping them apart means a plan set that names a
//! call site cannot be mistaken for one that names the method.  The bounded
//! shape is a `void` or value-returning `invokevirtual`/`invokestatic`/
//! `invokeinterface` for a non-cancellable callback; cancellation also needs a
//! `void` call and exactly one matching instruction, because a value would have
//! to be replaced by the callee descriptor's default.  `invokespecial` call
//! sites stay on the interpreter path.  A cancellable `RETURN`/`TAIL` on a
//! `void` target, and a cancellable `HEAD` on a non-void one, stay there too:
//! the first has no result to carry, the second needs the descriptor's default
//! value in the target's shape.  A non-void `RETURN`/`TAIL` callback that does
//! not declare the trailing `CallbackInfoReturnable` cannot observe the result
//! and stays there as well.
//!
//! The synthetic `CallbackInfoReturnable` is the smallest class that carries the
//! result, so it does not extend Mixin's `CallbackInfo`: a callback that casts
//! it to `CallbackInfo`, or that asks either synthetic class for `isCancellable`,
//! is outside the bounded subset.  A jar that already declares the class keeps
//! its own copy.
//!
//! # `CONSTANT_Dynamic`
//!
//! A class whose constant pool already uses `CONSTANT_Dynamic` (condy) is
//! preserved, not rejected - including a class that also carries javac's
//! `invokedynamic` string-concatenation bootstrap.  The writer appends new
//! constants after the original entries, so no existing index moves, and it
//! re-encodes the pool and `BootstrapMethods` through an index map so the same
//! guarantee holds if a caller ever compacts the pool.  Before weaving, every
//! condy entry is validated as a coherent shape: the name/type is a field
//! descriptor, the bootstrap index names a `BootstrapMethods` row, that row's
//! handle is a legal `CONSTANT_MethodHandle`, and every static argument is a
//! loadable constant, recursively for another condy.  The bootstrap method is
//! never evaluated; only its references are preserved.  A malformed entry that
//! names the reserved index 0 keeps the historical blanket rejection, because
//! there is no coherent condy shape to relocate.
//!
//! Non-ASCII constant pool strings and code that would exceed the class-file
//! `code_length` limit are rejected with a reason rather than written.

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    CALLBACK_INFO_CLASS, CALLBACK_INFO_DESC, CALLBACK_INFO_RETURNABLE_CLASS,
    CALLBACK_INFO_RETURNABLE_DESC, ClassFile, Cp, MethodRef, MixinInjectionPlan,
};

const ACC_STATIC: u16 = 0x0008;
const ACC_ABSTRACT: u16 = 0x0400;
const ACC_NATIVE: u16 = 0x0100;
const ACC_PUBLIC: u16 = 0x0001;
const ACC_PRIVATE: u16 = 0x0002;
/// `ACC_SUPER`, which every class file since Java 1.0.2 sets so `invokespecial`
/// dispatches through the superclass.
const ACC_SUPER: u16 = 0x0020;
/// The class-file version of the synthetic classes this module writes: Java 8,
/// which every JVM that can load a mod's classes accepts.
const SYNTHETIC_CLASS_MAJOR: u16 = 52;
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
/// Conditional branches one method may split.  Each split walks the straight-line
/// region to derive a frame, so a method that needs more is outside the bounded
/// weaver rather than a quadratic derivation.
const MAX_EXPANDED_BRANCHES: usize = 64;

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

/// One actual constant-pool entry, in file order.
///
/// The writer reconstructs the pool from these entries so a constant-pool
/// relocation can rewrite every index-bearing tag, `CONSTANT_Dynamic` and
/// `CONSTANT_InvokeDynamic` included.  The payload is kept verbatim so an
/// untouched entry still re-encodes byte-for-byte.
struct PoolEntry<'a> {
    /// The index this entry occupies.  A `long`/`double` leaves the next slot
    /// empty, which is why this is not the vector position.
    index: u16,
    tag: u8,
    /// The bytes after the tag.
    payload: &'a [u8],
}

/// One `BootstrapMethods` row: the bootstrap method handle and its static
/// arguments, both constant-pool indices.
struct BootstrapMethod {
    method_ref: u16,
    arguments: Vec<u16>,
}

/// One class attribute.  `BootstrapMethods` is parsed because a relocated pool
/// has to keep every `CONSTANT_Dynamic`/`CONSTANT_InvokeDynamic` row pointing at
/// the same constants; every other attribute keeps its bytes.
enum ClassAttribute<'a> {
    Raw {
        name_index: u16,
        body: &'a [u8],
    },
    BootstrapMethods {
        name_index: u16,
        methods: Vec<BootstrapMethod>,
    },
}

impl ClassAttribute<'_> {
    fn name_index(&self) -> u16 {
        match self {
            Self::Raw { name_index, .. } | Self::BootstrapMethods { name_index, .. } => *name_index,
        }
    }
}

/// The class file as ordered pieces, so the writer can copy everything it does
/// not mean to change.
struct Layout<'a> {
    /// `magic`, `minor_version`, `major_version`.
    version: &'a [u8],
    pool_count: u16,
    /// Every actual pool entry, in file order.
    pool_entries: Vec<PoolEntry<'a>>,
    /// `Option<&str>` per constant pool index; `None` for non-`Utf8` entries.
    pool_utf8: Vec<Option<&'a str>>,
    /// Constant-pool tag per index (0 for the reserved slot and the second slot
    /// of a `long`/`double`), used to validate `CONSTANT_Dynamic` references.
    pool_tags: Vec<u8>,
    /// `access_flags`, `this_class`, `super_class` and the interfaces table.
    after_pool: &'a [u8],
    /// The whole field table, count included.
    fields: &'a [u8],
    methods: Vec<MethodLayout<'a>>,
    /// The class attribute table, parsed far enough to relocate
    /// `BootstrapMethods`.
    class_attributes: Vec<ClassAttribute<'a>>,
    /// Bytes after the class attributes.  Kept so `write` is byte-exact even
    /// for a class file that carries trailing data.
    tail: &'a [u8],
}

/// An old-to-new constant-pool index map.
///
/// Appending constants never moves an existing entry, so the writer normally
/// uses the identity map.  A caller that compacts the pool can pass any
/// one-to-one map; [`Layout::write_pool`] and [`Layout::write_class_attributes`]
/// apply it to every index the entries and `BootstrapMethods` rows name,
/// `CONSTANT_Dynamic` included.
struct PoolMap(Vec<u16>);

impl PoolMap {
    fn identity(count: u16) -> Self {
        Self((0..count).collect())
    }

    fn get(&self, index: u16) -> Result<u16, String> {
        self.0.get(index as usize).copied().ok_or_else(|| {
            format!(
                "constant pool index {index} is outside the {}-entry pool being written",
                self.0.len()
            )
        })
    }
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
        let (pool_utf8, pool_entries) = parse_pool(&mut r, pool_count)?;
        let mut pool_tags = vec![0u8; pool_count as usize];
        for entry in &pool_entries {
            pool_tags[entry.index as usize] = entry.tag;
        }

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

        let class_attribute_count = r.u2()? as usize;
        let mut class_attributes = Vec::with_capacity(class_attribute_count);
        for _ in 0..class_attribute_count {
            let name_index = r.u2()?;
            let length = r.u4()? as usize;
            let body = r.bytes(length)?;
            class_attributes.push(
                if pool_utf8.get(name_index as usize).copied().flatten() == Some("BootstrapMethods")
                {
                    ClassAttribute::BootstrapMethods {
                        name_index,
                        methods: parse_bootstrap_methods(body)?,
                    }
                } else {
                    ClassAttribute::Raw { name_index, body }
                },
            );
        }
        let tail = &bytes[r.at..];

        Ok(Self {
            version,
            pool_count,
            pool_entries,
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

    /// The one `BootstrapMethods` row list, if the class declares the attribute.
    fn bootstrap_methods(&self) -> Option<&[BootstrapMethod]> {
        self.class_attributes
            .iter()
            .find_map(|attribute| match attribute {
                ClassAttribute::BootstrapMethods { methods, .. } => Some(methods.as_slice()),
                ClassAttribute::Raw { .. } => None,
            })
    }

    fn pool_entry(&self, index: u16) -> Option<&PoolEntry<'a>> {
        self.pool_entries.iter().find(|entry| entry.index == index)
    }

    fn pool_tag(&self, index: u16) -> Option<u8> {
        self.pool_tags
            .get(index as usize)
            .copied()
            .filter(|tag| *tag != 0)
    }

    /// The name and descriptor one `CONSTANT_NameAndType` names.
    fn name_type(&self, index: u16) -> Result<(&'a str, &'a str), String> {
        let entry = self
            .pool_entry(index)
            .filter(|entry| entry.tag == 12)
            .ok_or_else(|| format!("constant pool index {index} is not a CONSTANT_NameAndType"))?;
        let name_index = payload_u2(entry.payload, 0)?;
        let descriptor_index = payload_u2(entry.payload, 2)?;
        let name = self
            .utf8(name_index)
            .ok_or_else(|| format!("constant pool index {name_index} is not a Utf8 name"))?;
        let descriptor = self.utf8(descriptor_index).ok_or_else(|| {
            format!("constant pool index {descriptor_index} is not a Utf8 descriptor")
        })?;
        Ok((name, descriptor))
    }

    /// Check every `CONSTANT_Dynamic` entry and its bootstrap chain.
    ///
    /// Appending constants after the original entries cannot move an index, so
    /// the writer does not have to renumber anything to preserve a condy pool.
    /// It does have to be sure the class it writes is coherent, which is what
    /// this validation establishes: the name/type is a field descriptor, the
    /// bootstrap index names a `BootstrapMethods` row, that row's handle is a
    /// legal `CONSTANT_MethodHandle`, and every static argument is a loadable
    /// constant (recursively, for another `CONSTANT_Dynamic`).
    ///
    /// A malformed entry that names the reserved index 0 keeps the historical
    /// blanket rejection: that is not a condy shape the writer can relocate,
    /// and a class built by splicing an empty tag is outside the subset.
    fn validate_constant_pool(&self) -> Result<(), String> {
        if !self.pool_tags.contains(&17) {
            return Ok(());
        }
        let bootstrap_attributes = self
            .class_attributes
            .iter()
            .filter(|attribute| matches!(attribute, ClassAttribute::BootstrapMethods { .. }))
            .count();
        if bootstrap_attributes > 1 {
            return Err("class declares more than one BootstrapMethods attribute".to_owned());
        }
        let mut visiting = BTreeSet::new();
        for entry in &self.pool_entries {
            if entry.tag == 17 {
                self.validate_dynamic(entry.index, &mut visiting)?;
            }
        }
        Ok(())
    }

    fn validate_dynamic(&self, index: u16, visiting: &mut BTreeSet<u16>) -> Result<(), String> {
        if !visiting.insert(index) {
            return Err(format!(
                "CONSTANT_Dynamic {index} is part of a bootstrap cycle"
            ));
        }
        let entry = self
            .pool_entry(index)
            .filter(|entry| entry.tag == 17)
            .ok_or_else(|| format!("constant pool index {index} is not a CONSTANT_Dynamic"))?;
        let bootstrap = payload_u2(entry.payload, 0)?;
        let nt = payload_u2(entry.payload, 2)?;
        if nt == 0 {
            // The reserved index 0 is not a shape this writer can relocate.
            return Err(
                "class constant pool uses CONSTANT_Dynamic (condy), which the bounded weaver does not rewrite"
                    .to_owned(),
            );
        }
        let (name, descriptor) = self.name_type(nt)?;
        if name.is_empty() || !crate::valid_field_descriptor(descriptor) {
            return Err(format!(
                "CONSTANT_Dynamic {index} names {name:?}{descriptor}, which is not a field descriptor"
            ));
        }
        let methods = self.bootstrap_methods().ok_or_else(|| {
            format!("CONSTANT_Dynamic {index} has no BootstrapMethods attribute to name")
        })?;
        let method = methods.get(bootstrap as usize).ok_or_else(|| {
            format!(
                "CONSTANT_Dynamic {index} names bootstrap method {bootstrap}, but the class declares {}",
                methods.len()
            )
        })?;
        self.validate_bootstrap_handle(method.method_ref, index)?;
        for (position, argument) in method.arguments.iter().enumerate() {
            match self.pool_tag(*argument) {
                Some(17) => self.validate_dynamic(*argument, visiting)?,
                Some(3 | 4 | 5 | 6 | 7 | 8 | 15 | 16) => {}
                Some(18) => {
                    return Err(format!(
                        "CONSTANT_Dynamic {index}'s static argument {position} is a CONSTANT_InvokeDynamic, which is not a loadable constant"
                    ));
                }
                Some(tag) => {
                    return Err(format!(
                        "CONSTANT_Dynamic {index}'s static argument {position} (index {argument}) has tag {tag}, which is not a loadable constant"
                    ));
                }
                None => {
                    return Err(format!(
                        "CONSTANT_Dynamic {index}'s static argument {position} names constant pool index {argument}, which the class does not declare"
                    ));
                }
            }
        }
        visiting.remove(&index);
        Ok(())
    }

    fn validate_bootstrap_handle(&self, handle_index: u16, dynamic: u16) -> Result<(), String> {
        let entry = self
            .pool_entry(handle_index)
            .filter(|entry| entry.tag == 15)
            .ok_or_else(|| {
                format!(
                    "CONSTANT_Dynamic {dynamic} names bootstrap method {handle_index}, which is not a CONSTANT_MethodHandle"
                )
            })?;
        let kind = entry
            .payload
            .first()
            .copied()
            .ok_or_else(|| "a CONSTANT_MethodHandle has no reference kind".to_owned())?;
        let reference = payload_u2(entry.payload, 1)?;
        let reference_tag = self.pool_tag(reference).unwrap_or(0);
        let legal = match kind {
            1..=4 => reference_tag == 9,
            5 | 8 => reference_tag == 10,
            6 | 7 => matches!(reference_tag, 10 | 11),
            9 => reference_tag == 11,
            _ => false,
        };
        if !legal {
            return Err(format!(
                "CONSTANT_Dynamic {dynamic}'s bootstrap method handle has kind {kind} and references constant-pool tag {reference_tag}, which is not a legal handle target"
            ));
        }
        Ok(())
    }

    /// Re-encode the constant pool through an index map.
    ///
    /// The identity map reproduces the input bytes exactly; a compaction map
    /// rewrites every index-bearing entry, including `CONSTANT_Dynamic`'s
    /// `bootstrap_method_attr_index` and `name_and_type_index`.
    fn write_pool(&self, out: &mut Vec<u8>, map: &PoolMap) -> Result<(), String> {
        // Entries are written in their *new* index order, so a compaction that
        // moves an entry also moves its bytes; every index a payload names is
        // rewritten through the same map.
        let mut ordered = self
            .pool_entries
            .iter()
            .map(|entry| Ok((map.get(entry.index)?, entry)))
            .collect::<Result<Vec<_>, String>>()?;
        ordered.sort_by_key(|(index, _)| *index);
        let mut expected = 1u16;
        for (index, entry) in &ordered {
            if *index != expected {
                return Err(format!(
                    "constant-pool relocation is not contiguous at index {index} (expected {expected})"
                ));
            }
            expected = expected
                .checked_add(if matches!(entry.tag, 5 | 6) { 2 } else { 1 })
                .ok_or("constant pool index overflow")?;
        }
        for (_, entry) in ordered {
            out.push(entry.tag);
            match entry.tag {
                1 | 3 | 4 | 5 | 6 => out.extend_from_slice(entry.payload),
                7 | 8 | 16 | 19 | 20 => push_u2(out, map.get(payload_u2(entry.payload, 0)?)?),
                9 | 10 | 11 | 12 | 17 | 18 => {
                    push_u2(out, map.get(payload_u2(entry.payload, 0)?)?);
                    push_u2(out, map.get(payload_u2(entry.payload, 2)?)?);
                }
                15 => {
                    let kind = entry.payload.first().copied().ok_or_else(|| {
                        "a CONSTANT_MethodHandle has no reference kind".to_owned()
                    })?;
                    out.push(kind);
                    push_u2(out, map.get(payload_u2(entry.payload, 1)?)?);
                }
                other => {
                    return Err(format!(
                        "constant pool tag {other} is not supported by the class writer"
                    ));
                }
            }
        }
        Ok(())
    }

    fn write_class_attributes(&self, out: &mut Vec<u8>, map: &PoolMap) -> Result<(), String> {
        let count = u16::try_from(self.class_attributes.len())
            .map_err(|_| "class file has too many attributes".to_owned())?;
        out.extend_from_slice(&count.to_be_bytes());
        for attribute in &self.class_attributes {
            out.extend_from_slice(&attribute.name_index().to_be_bytes());
            let body = match attribute {
                ClassAttribute::Raw { body, .. } => body.to_vec(),
                ClassAttribute::BootstrapMethods { methods, .. } => {
                    encode_bootstrap_methods(methods, map)?
                }
            };
            let length = u32::try_from(body.len())
                .map_err(|_| "attribute is longer than 4 GiB".to_owned())?;
            out.extend_from_slice(&length.to_be_bytes());
            out.extend_from_slice(&body);
        }
        Ok(())
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
) -> Result<(Vec<Option<&'a str>>, Vec<PoolEntry<'a>>), String> {
    if count == 0 {
        return Err("class file constant pool count is zero".to_owned());
    }
    let mut utf8 = vec![None; count as usize];
    let mut entries = Vec::new();
    let mut index = 1usize;
    while index < count as usize {
        let offset = r.at;
        let entry_index = u16::try_from(index).map_err(|_| "constant pool index overflow")?;
        let tag = r.u1()?;
        let payload_start = r.at;
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
        entries.push(PoolEntry {
            index: entry_index,
            tag,
            payload: &r.bytes[payload_start..r.at],
        });
        index += 1;
    }
    Ok((utf8, entries))
}

fn parse_bootstrap_methods(body: &[u8]) -> Result<Vec<BootstrapMethod>, String> {
    let mut r = Bytes::new(body);
    let count = r.u2()? as usize;
    let mut methods = Vec::with_capacity(count);
    for _ in 0..count {
        let method_ref = r.u2()?;
        let argument_count = r.u2()? as usize;
        let mut arguments = Vec::with_capacity(argument_count);
        for _ in 0..argument_count {
            arguments.push(r.u2()?);
        }
        methods.push(BootstrapMethod {
            method_ref,
            arguments,
        });
    }
    Ok(methods)
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
        Cp::FieldRef { class, nt } => {
            out.push(9);
            out.extend_from_slice(&class.to_be_bytes());
            out.extend_from_slice(&nt.to_be_bytes());
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
                "the class writer can only append Utf8/Class/NameAndType/FieldRef/MethodRef constants, not {other:?}"
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

fn push_u2(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_be_bytes());
}

/// Read a `u2` from a constant-pool entry payload.
fn payload_u2(payload: &[u8], at: usize) -> Result<u16, String> {
    let bytes = payload
        .get(at..at + 2)
        .ok_or_else(|| format!("constant pool entry is truncated at payload offset {at}"))?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

/// Re-encode a `BootstrapMethods` table through an index map.
fn encode_bootstrap_methods(methods: &[BootstrapMethod], map: &PoolMap) -> Result<Vec<u8>, String> {
    let count = u16::try_from(methods.len())
        .map_err(|_| "BootstrapMethods has too many entries".to_owned())?;
    let mut out = Vec::new();
    push_u2(&mut out, count);
    for method in methods {
        push_u2(&mut out, map.get(method.method_ref)?);
        let arguments = u16::try_from(method.arguments.len())
            .map_err(|_| "a BootstrapMethods row has too many arguments".to_owned())?;
        push_u2(&mut out, arguments);
        for argument in &method.arguments {
            push_u2(&mut out, map.get(*argument)?);
        }
    }
    Ok(out)
}

fn push_u4(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

/// Append one constant to a pool under construction and return its index.
///
/// Like a parsed pool, index 0 is the reserved `Cp::Empty` slot, so the count a
/// caller writes is `pool.len()` and the entries are `pool[1..]`.
fn pool_push(pool: &mut Vec<Cp>, constant: Cp) -> u16 {
    pool.push(constant);
    // A synthetic class stays far below the 65535-entry constant pool limit, so
    // the index of the entry just pushed always fits.
    (pool.len() - 1) as u16
}

fn pool_utf8(pool: &mut Vec<Cp>, value: &str) -> u16 {
    pool_push(pool, Cp::Utf8(value.to_owned()))
}

fn pool_class(pool: &mut Vec<Cp>, name: &str) -> u16 {
    let name = pool_utf8(pool, name);
    pool_push(pool, Cp::Class(name))
}

fn pool_name_type(pool: &mut Vec<Cp>, name: u16, descriptor: u16) -> u16 {
    pool_push(pool, Cp::NameType { name, descriptor })
}

fn pool_field_ref(pool: &mut Vec<Cp>, owner: &str, name: u16, descriptor: u16) -> u16 {
    let class = pool_class(pool, owner);
    let nt = pool_name_type(pool, name, descriptor);
    pool_push(pool, Cp::FieldRef { class, nt })
}

fn pool_method_ref(pool: &mut Vec<Cp>, owner: &str, name: u16, descriptor: u16) -> u16 {
    let class = pool_class(pool, owner);
    let nt = pool_name_type(pool, name, descriptor);
    pool_push(pool, Cp::MethodRef { class, nt })
}

/// One method body of a synthetic class.
struct SyntheticCode<'a> {
    max_stack: u16,
    max_locals: u16,
    bytes: &'a [u8],
}

fn push_synthetic_field(out: &mut Vec<u8>, access: u16, name: u16, descriptor: u16) {
    push_u2(out, access);
    push_u2(out, name);
    push_u2(out, descriptor);
    push_u2(out, 0); // no field attributes
}

fn push_synthetic_method(
    out: &mut Vec<u8>,
    code_name: u16,
    access: u16,
    name: u16,
    descriptor: u16,
    body: SyntheticCode<'_>,
) {
    push_u2(out, access);
    push_u2(out, name);
    push_u2(out, descriptor);
    push_u2(out, 1); // one attribute: Code
    push_u2(out, code_name);
    let mut attribute = Vec::new();
    push_u2(&mut attribute, body.max_stack);
    push_u2(&mut attribute, body.max_locals);
    push_u4(&mut attribute, body.bytes.len() as u32);
    attribute.extend_from_slice(body.bytes);
    push_u2(&mut attribute, 0); // exception table
    push_u2(&mut attribute, 0); // Code attributes
    push_u4(out, attribute.len() as u32);
    out.extend_from_slice(&attribute);
}

/// The bounded JVM-side `CallbackInfoReturnable` a woven non-void
/// `RETURN`/`TAIL` insert constructs.
///
/// Mixin's own class lives in the Mixin jar, which a jar woven without Mixin on
/// the classpath does not have, so the weaver writes the smallest class that
/// answers what a return callback uses: `returnValue`/`cancelled` fields, the
/// `()V` and `(Ljava/lang/Object;)V` constructors, `setReturnValue`,
/// `getReturnValue`, `cancel` and `isCancelled`.  No member branches, so the
/// class needs no `StackMapTable`, and it parses with [`crate::ClassFile::parse`]
/// exactly like a compiled class.
///
/// The class stands on its own rather than extending Mixin's `CallbackInfo`:
/// the woven jar may not carry that class, and a callback that casts this object
/// to `CallbackInfo` is outside the bounded subset.
pub fn callback_info_returnable_class() -> Result<Vec<u8>, String> {
    const NAME: &str = CALLBACK_INFO_RETURNABLE_CLASS;
    let mut pool: Vec<Cp> = vec![Cp::Empty];
    let this_class = pool_class(&mut pool, NAME);
    let super_class = pool_class(&mut pool, "java/lang/Object");
    let code_name = pool_utf8(&mut pool, "Code");
    // Members are named and described once, so the pool has no duplicate Utf8.
    let name_return_value = pool_utf8(&mut pool, "returnValue");
    let name_cancelled = pool_utf8(&mut pool, "cancelled");
    let name_init = pool_utf8(&mut pool, "<init>");
    let name_set_return = pool_utf8(&mut pool, "setReturnValue");
    let name_get_return = pool_utf8(&mut pool, "getReturnValue");
    let name_cancel = pool_utf8(&mut pool, "cancel");
    let name_is_cancelled = pool_utf8(&mut pool, "isCancelled");
    let desc_object = pool_utf8(&mut pool, "Ljava/lang/Object;");
    let desc_boolean = pool_utf8(&mut pool, "Z");
    let desc_void = pool_utf8(&mut pool, "()V");
    let desc_value = pool_utf8(&mut pool, "(Ljava/lang/Object;)V");
    let desc_get_return = pool_utf8(&mut pool, "()Ljava/lang/Object;");
    let desc_is_cancelled = pool_utf8(&mut pool, "()Z");
    let return_value = pool_field_ref(&mut pool, NAME, name_return_value, desc_object);
    let cancelled = pool_field_ref(&mut pool, NAME, name_cancelled, desc_boolean);
    let object_init = pool_method_ref(&mut pool, "java/lang/Object", name_init, desc_void);

    // `<init>()V`: `aload_0; invokespecial Object.<init>; return`.
    let mut init_bytes = vec![0x2a, 0xb7];
    init_bytes.extend_from_slice(&object_init.to_be_bytes());
    init_bytes.push(0xb1);
    // `<init>(Object)V`: the same, then `putfield returnValue` from `aload_1`.
    let mut init_value_bytes = vec![0x2a, 0xb7];
    init_value_bytes.extend_from_slice(&object_init.to_be_bytes());
    init_value_bytes.extend_from_slice(&[0x2a, 0x2b, 0xb5]);
    init_value_bytes.extend_from_slice(&return_value.to_be_bytes());
    init_value_bytes.push(0xb1);
    // `setReturnValue(Object)V`: `aload_0; aload_1; putfield returnValue`.
    let mut set_return_bytes = vec![0x2a, 0x2b, 0xb5];
    set_return_bytes.extend_from_slice(&return_value.to_be_bytes());
    set_return_bytes.push(0xb1);
    // `getReturnValue()Object`: `aload_0; getfield returnValue; areturn`.
    let mut get_return_bytes = vec![0x2a, 0xb4];
    get_return_bytes.extend_from_slice(&return_value.to_be_bytes());
    get_return_bytes.push(0xb0);
    // `cancel()V`: `aload_0; iconst_1; putfield cancelled; return`.
    let mut cancel_bytes = vec![0x2a, 0x04, 0xb5];
    cancel_bytes.extend_from_slice(&cancelled.to_be_bytes());
    cancel_bytes.push(0xb1);
    // `isCancelled()Z`: `aload_0; getfield cancelled; ireturn`.
    let mut is_cancelled_bytes = vec![0x2a, 0xb4];
    is_cancelled_bytes.extend_from_slice(&cancelled.to_be_bytes());
    is_cancelled_bytes.push(0xac);

    let mut out = Vec::new();
    push_u4(&mut out, 0xCAFE_BABE);
    push_u2(&mut out, 0); // minor_version
    push_u2(&mut out, SYNTHETIC_CLASS_MAJOR);
    push_u2(&mut out, pool.len() as u16);
    // Index 0 is the reserved slot, so the entries are `pool[1..]`.
    for constant in &pool[1..] {
        encode_constant(&mut out, constant)?;
    }
    push_u2(&mut out, ACC_PUBLIC | ACC_SUPER);
    push_u2(&mut out, this_class);
    push_u2(&mut out, super_class);
    push_u2(&mut out, 0); // interfaces
    push_u2(&mut out, 2); // fields
    push_synthetic_field(&mut out, ACC_PRIVATE, name_return_value, desc_object);
    push_synthetic_field(&mut out, ACC_PRIVATE, name_cancelled, desc_boolean);
    push_u2(&mut out, 6); // methods
    push_synthetic_method(
        &mut out,
        code_name,
        ACC_PUBLIC,
        name_init,
        desc_void,
        SyntheticCode {
            max_stack: 1,
            max_locals: 1,
            bytes: &init_bytes,
        },
    );
    push_synthetic_method(
        &mut out,
        code_name,
        ACC_PUBLIC,
        name_init,
        desc_value,
        SyntheticCode {
            max_stack: 2,
            max_locals: 2,
            bytes: &init_value_bytes,
        },
    );
    push_synthetic_method(
        &mut out,
        code_name,
        ACC_PUBLIC,
        name_set_return,
        desc_value,
        SyntheticCode {
            max_stack: 2,
            max_locals: 2,
            bytes: &set_return_bytes,
        },
    );
    push_synthetic_method(
        &mut out,
        code_name,
        ACC_PUBLIC,
        name_get_return,
        desc_get_return,
        SyntheticCode {
            max_stack: 1,
            max_locals: 1,
            bytes: &get_return_bytes,
        },
    );
    push_synthetic_method(
        &mut out,
        code_name,
        ACC_PUBLIC,
        name_cancel,
        desc_void,
        SyntheticCode {
            max_stack: 2,
            max_locals: 1,
            bytes: &cancel_bytes,
        },
    );
    push_synthetic_method(
        &mut out,
        code_name,
        ACC_PUBLIC,
        name_is_cancelled,
        desc_is_cancelled,
        SyntheticCode {
            max_stack: 1,
            max_locals: 1,
            bytes: &is_cancelled_bytes,
        },
    );
    push_u2(&mut out, 0); // class attributes
    Ok(out)
}

/// The bounded JVM-side `CallbackInfo` a woven cancellable `void` `HEAD`
/// insert constructs.
///
/// Like [`callback_info_returnable_class`], this is the smallest class a callback
/// needs rather than Mixin's own: a `cancelled` field, the `()V` constructor,
/// `cancel` and `isCancelled`.  No member branches, so it needs no
/// `StackMapTable`.
pub fn callback_info_class() -> Result<Vec<u8>, String> {
    const NAME: &str = CALLBACK_INFO_CLASS;
    let mut pool: Vec<Cp> = vec![Cp::Empty];
    let this_class = pool_class(&mut pool, NAME);
    let super_class = pool_class(&mut pool, "java/lang/Object");
    let code_name = pool_utf8(&mut pool, "Code");
    let name_cancelled = pool_utf8(&mut pool, "cancelled");
    let name_init = pool_utf8(&mut pool, "<init>");
    let name_cancel = pool_utf8(&mut pool, "cancel");
    let name_is_cancelled = pool_utf8(&mut pool, "isCancelled");
    let desc_boolean = pool_utf8(&mut pool, "Z");
    let desc_void = pool_utf8(&mut pool, "()V");
    let desc_is_cancelled = pool_utf8(&mut pool, "()Z");
    let cancelled = pool_field_ref(&mut pool, NAME, name_cancelled, desc_boolean);
    let object_init = pool_method_ref(&mut pool, "java/lang/Object", name_init, desc_void);

    // `<init>()V`: `aload_0; invokespecial Object.<init>; return`.
    let mut init_bytes = vec![0x2a, 0xb7];
    init_bytes.extend_from_slice(&object_init.to_be_bytes());
    init_bytes.push(0xb1);
    // `cancel()V`: `aload_0; iconst_1; putfield cancelled; return`.
    let mut cancel_bytes = vec![0x2a, 0x04, 0xb5];
    cancel_bytes.extend_from_slice(&cancelled.to_be_bytes());
    cancel_bytes.push(0xb1);
    // `isCancelled()Z`: `aload_0; getfield cancelled; ireturn`.
    let mut is_cancelled_bytes = vec![0x2a, 0xb4];
    is_cancelled_bytes.extend_from_slice(&cancelled.to_be_bytes());
    is_cancelled_bytes.push(0xac);

    let mut out = Vec::new();
    push_u4(&mut out, 0xCAFE_BABE);
    push_u2(&mut out, 0); // minor_version
    push_u2(&mut out, SYNTHETIC_CLASS_MAJOR);
    push_u2(&mut out, pool.len() as u16);
    for constant in &pool[1..] {
        encode_constant(&mut out, constant)?;
    }
    push_u2(&mut out, ACC_PUBLIC | ACC_SUPER);
    push_u2(&mut out, this_class);
    push_u2(&mut out, super_class);
    push_u2(&mut out, 0); // interfaces
    push_u2(&mut out, 1); // fields
    push_synthetic_field(&mut out, ACC_PRIVATE, name_cancelled, desc_boolean);
    push_u2(&mut out, 3); // methods
    push_synthetic_method(
        &mut out,
        code_name,
        ACC_PUBLIC,
        name_init,
        desc_void,
        SyntheticCode {
            max_stack: 1,
            max_locals: 1,
            bytes: &init_bytes,
        },
    );
    push_synthetic_method(
        &mut out,
        code_name,
        ACC_PUBLIC,
        name_cancel,
        desc_void,
        SyntheticCode {
            max_stack: 2,
            max_locals: 1,
            bytes: &cancel_bytes,
        },
    );
    push_synthetic_method(
        &mut out,
        code_name,
        ACC_PUBLIC,
        name_is_cancelled,
        desc_is_cancelled,
        SyntheticCode {
            max_stack: 1,
            max_locals: 1,
            bytes: &is_cancelled_bytes,
        },
    );
    push_u2(&mut out, 0); // class attributes
    Ok(out)
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
    let map = PoolMap::identity(layout.pool_count);
    let mut out = Vec::with_capacity(layout.pool_entries.len() * 4 + 64);
    out.extend_from_slice(layout.version);
    out.extend_from_slice(&pool_count.to_be_bytes());
    layout.write_pool(&mut out, &map)?;
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
    layout.write_class_attributes(&mut out, &map)?;
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
    /// Conditional branches that no longer reach their target in 16 bits and
    /// were split into an inverted branch over a `goto_w`.
    expanded: BTreeSet<usize>,
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
    expanded: &BTreeSet<usize>,
) -> Result<CodePlan, String> {
    let mut plan = CodePlan {
        offsets: BTreeMap::new(),
        boundaries: BTreeMap::new(),
        widened: widened.clone(),
        expanded: expanded.clone(),
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
            None if expanded.contains(&instruction.at) => 8,
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
    let mut expanded = BTreeSet::new();
    for _ in 0..MAX_RELOCATION_PASSES {
        let plan = layout_pass(label, scan, insertions, &widened, &expanded)?;
        let offset = |at: usize| {
            plan.offsets
                .get(&at)
                .copied()
                .ok_or_else(|| format!("{label}: offset {at} is not an instruction boundary"))
        };
        let mut grew = false;
        for branch in &scan.branches {
            let opcode_at = offset(branch.at)?;
            if expanded.contains(&branch.at) {
                // The branch is now `if<inverse> +8; goto_w target`; only the
                // wide jump's 32-bit operand has to fit.
                let target = map_target(label, &plan, branch.target, entry)?;
                if i32::try_from(target as i64 - (opcode_at as i64 + 3)).is_err() {
                    return Err(format!(
                        "{label}: the widened {} at offset {} would exceed the 32-bit branch range",
                        opcode_name(branch.opcode),
                        branch.at
                    ));
                }
                continue;
            }
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
            if widened_opcode(branch.opcode).is_some() {
                if widened.insert(branch.at) {
                    grew = true;
                }
                continue;
            }
            if inverse_conditional(branch.opcode).is_none() {
                return Err(format!(
                    "{label}: {} at offset {} would sit {delta} bytes from its target after weaving, past the 16-bit branch range; the weaver only widens goto/jsr and splits the if* family",
                    opcode_name(branch.opcode),
                    branch.at
                ));
            }
            // `if*` has no wide encoding: overwrite it with the inverse
            // condition branching over a `goto_w`, which is 8 bytes instead of
            // 3.  The fallthrough instruction becomes the inverse branch's
            // target, so it needs a frame the relocation emits below.
            if !scan.starts().contains(&(branch.at + 3)) {
                return Err(format!(
                    "{label}: {} at offset {} has no fallthrough instruction to split around",
                    opcode_name(branch.opcode),
                    branch.at
                ));
            }
            if expanded.len() >= MAX_EXPANDED_BRANCHES {
                return Err(format!(
                    "{label}: more than {MAX_EXPANDED_BRANCHES} conditional branches would need splitting after weaving; the bounded weaver stops here"
                ));
            }
            if expanded.insert(branch.at) {
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
        if plan.expanded.contains(&branch.at) {
            // `if<inverse> +8; goto_w target`: the inverse condition jumps over
            // the wide branch to the original fallthrough instruction, which
            // sits at `at + 8` (any insert in front of it begins there too).
            let inverse = inverse_conditional(branch.opcode).ok_or_else(|| {
                format!(
                    "{label}: {} at offset {} has no inverting conditional",
                    opcode_name(branch.opcode),
                    branch.at
                )
            })?;
            let fallthrough = map_target(label, plan, branch.at + 3, entry)?;
            let skip = i16::try_from(fallthrough as i64 - at as i64).map_err(|_| {
                format!(
                    "{label}: the split {} at offset {} cannot reach its fallthrough",
                    opcode_name(branch.opcode),
                    branch.at
                )
            })?;
            out.push(inverse);
            out.extend_from_slice(&skip.to_be_bytes());
            let wide_delta = i32::try_from(target as i64 - (at as i64 + 3)).map_err(|_| {
                format!("{label}: a widened branch exceeds the 32-bit branch range")
            })?;
            out.push(0xc8); // goto_w
            out.extend_from_slice(&wide_delta.to_be_bytes());
            continue;
        }
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

/// Where the frames of a `Code` attribute whose offset carries an insert are
/// re-emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameSites {
    /// At the insert and at the instruction.  A `HEAD` insert and a `void`
    /// `RETURN` insert leave the operand stack and every local exactly as the
    /// verifier expects at the instruction, so its frame still describes the
    /// state after the insert and stays valid at both offsets.
    Both,
    /// At the insert only.  A non-void `RETURN` insert stores the target's
    /// result in a fresh local, so the state the verifier sees at the
    /// instruction differs by one live local: re-emitting the frame there would
    /// claim a `Top` slot that actually holds the boxed result.  The insert
    /// itself receives that frame, and every branch or switch that named the
    /// instruction now lands on the insert, so the instruction starts no basic
    /// block and needs none.
    InsertOnly,
}

/// A frame the rewrite synthesises where the original class had none.
///
/// A cancellable insert branches, so the instruction it jumps to becomes a
/// basic block and needs a verifier frame; a split conditional needs one for
/// its fallthrough.  The locals are slot-based (a wide value fills two slots)
/// and the stack is value-based, like the derivation that produces them.
#[derive(Clone)]
struct SyntheticFrame {
    offset: usize,
    locals: Vec<ValueType>,
    stack: Vec<ValueType>,
    /// `true` maps the offset to the instruction itself (after any insert at
    /// that offset); `false` maps it to the start of an insert there.
    at_instruction: bool,
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
    target: &Target<'a>,
    code: &CodeLayout<'a>,
    scan: &CodeScan,
    insertions: &[(usize, usize)],
    bytes: &[u8],
    max_stack: u16,
    max_locals: u16,
    entry: TargetEntry,
    sites: FrameSites,
    extra_frames: &[SyntheticFrame],
    pool: &mut PoolBuilder<'_>,
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

    // Frames the rewrite introduces that no original frame describes: the
    // callers' (a cancellable `HEAD` body, an INVOKE call and its
    // continuation) plus the fallthrough of every conditional the layout had
    // to split.  A class file below version 50 ignores `StackMapTable` and is
    // verified by type inference, so it needs none of them.
    let mut extras = extra_frames.to_vec();
    for at in &plan.expanded {
        let fallthrough = at + 3;
        if code_frames_have(code, fallthrough) {
            continue;
        }
        let state = derive_state_at(label, target, code, scan, fallthrough)?;
        extras.push(SyntheticFrame {
            offset: fallthrough,
            locals: state.locals,
            stack: Vec::new(),
            at_instruction: entry == TargetEntry::Skip,
        });
    }
    if target.major() < 50 {
        extras.clear();
    }
    let mut seen = BTreeSet::new();
    extras.retain(|frame| !code_frames_have(code, frame.offset) && seen.insert(frame.offset));

    let mut attributes = Vec::with_capacity(code.attributes.len() + 1);
    let mut has_stack_map = false;
    for attribute in &code.attributes {
        match attribute {
            CodeAttribute::StackMap { name_index, frames } => {
                has_stack_map = true;
                let frames = if extras.is_empty() {
                    relocate_frames(label, frames, &plan, &insertions, sites)?
                } else {
                    relocate_frames_full(
                        label,
                        target,
                        frames,
                        &plan,
                        &insertions,
                        sites,
                        &extras,
                        pool,
                    )?
                };
                attributes.push(CodeAttribute::StackMap {
                    name_index: *name_index,
                    frames,
                });
            }
            other => {
                attributes.push(relocate_attribute(label, other, &plan)?);
            }
        }
    }
    if !has_stack_map && !extras.is_empty() {
        let name_index = pool.utf8("StackMapTable")?;
        attributes.push(CodeAttribute::StackMap {
            name_index,
            frames: synthesize_frames(label, &extras, &plan, pool)?,
        });
    }

    Ok(CodePatch {
        code: woven,
        max_stack,
        max_locals,
        exception_table,
        attributes,
    })
}

/// Whether the method already carries a stack-map frame at `offset`.
fn code_frames_have(code: &CodeLayout<'_>, offset: usize) -> bool {
    code.attributes.iter().any(|attribute| match attribute {
        CodeAttribute::StackMap { frames, .. } => frames.iter().any(|frame| frame.offset == offset),
        _ => false,
    })
}

fn relocate_attribute<'a>(
    label: &str,
    attribute: &CodeAttribute<'a>,
    plan: &CodePlan,
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
        CodeAttribute::StackMap { .. } => {
            return Err(format!(
                "{label}: a stack map is relocated by the frame pass, not by itself"
            ));
        }
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
    sites: FrameSites,
) -> Result<Vec<StackMapFrame>, String> {
    let mut out = Vec::with_capacity(frames.len() * 2);
    for frame in frames {
        // An insert at a frame position puts the callback in front of the
        // instruction that frame describes, which leaves the callback where the
        // verifier still wants a frame: the instruction after an unconditional
        // transfer (the `goto`/switch that named this position) starts a basic
        // block even when nothing branches to the inserted bytes.  A `HEAD` or
        // void `RETURN` callback runs in exactly the frame's state - it only
        // pushes its arguments and calls `invokestatic` - so the same frame is
        // valid at both offsets.  A non-void `RETURN` insert leaves one more live
        // local behind, so only the insert keeps the frame.
        if inserted_at(insertions, frame.offset).is_some()
            && (frame.offset > 0 || sites == FrameSites::InsertOnly)
        {
            let boundary = map_offset(label, plan, frame.offset, false)?;
            push_frame(label, &mut out, frame, boundary, plan)?;
            if sites == FrameSites::InsertOnly {
                continue;
            }
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

/// Relocate a stack map as a table of full frames, so a frame this rewrite
/// synthesises can be inserted without changing what a later delta frame means.
///
/// Compressed frames (`same`, `append`, `chop`) are deltas from the previous
/// frame.  Inserting a frame in the middle would make a following `append`
/// append to the new frame instead of the original one, so every frame in this
/// path is written as a `full_frame` with its absolute locals and stack.
#[allow(clippy::too_many_arguments)]
fn relocate_frames_full(
    label: &str,
    target: &Target<'_>,
    frames: &[StackMapFrame],
    plan: &CodePlan,
    insertions: &[(usize, usize)],
    sites: FrameSites,
    extras: &[SyntheticFrame],
    pool: &mut PoolBuilder<'_>,
) -> Result<Vec<StackMapFrame>, String> {
    let full = full_frames(target, frames)?;
    let mut out = Vec::with_capacity(full.len() * 2 + extras.len());
    for (offset, locals, stack) in &full {
        if inserted_at(insertions, *offset).is_some()
            && (*offset > 0 || sites == FrameSites::InsertOnly)
        {
            let boundary = map_offset(label, plan, *offset, false)?;
            out.push(full_frame(label, boundary, locals, stack, plan, pool)?);
            if sites == FrameSites::InsertOnly {
                continue;
            }
        }
        let mapped = map_offset(label, plan, *offset, true)?;
        out.push(full_frame(label, mapped, locals, stack, plan, pool)?);
    }
    for extra in extras {
        let mapped = if extra.at_instruction {
            map_offset(label, plan, extra.offset, true)?
        } else {
            map_offset(label, plan, extra.offset, false)?
        };
        let locals = values_from_slots(&extra.locals);
        out.push(full_frame(
            label,
            mapped,
            &locals,
            &extra.stack,
            plan,
            pool,
        )?);
    }
    sort_frames(label, out)
}

/// The frames for a method that had no `StackMapTable`, built only from the
/// extras this rewrite introduces.
fn synthesize_frames(
    label: &str,
    extras: &[SyntheticFrame],
    plan: &CodePlan,
    pool: &mut PoolBuilder<'_>,
) -> Result<Vec<StackMapFrame>, String> {
    let mut out = Vec::with_capacity(extras.len());
    for extra in extras {
        let mapped = if extra.at_instruction {
            map_offset(label, plan, extra.offset, true)?
        } else {
            map_offset(label, plan, extra.offset, false)?
        };
        let locals = values_from_slots(&extra.locals);
        out.push(full_frame(
            label,
            mapped,
            &locals,
            &extra.stack,
            plan,
            pool,
        )?);
    }
    sort_frames(label, out)
}

fn full_frame(
    label: &str,
    offset: usize,
    locals: &[ValueType],
    stack: &[ValueType],
    plan: &CodePlan,
    pool: &mut PoolBuilder<'_>,
) -> Result<StackMapFrame, String> {
    Ok(StackMapFrame {
        frame_type: 255,
        offset,
        locals: locals
            .iter()
            .map(|value| verification_from_value(label, value, plan, pool))
            .collect::<Result<Vec<_>, _>>()?,
        stack: stack
            .iter()
            .map(|value| verification_from_value(label, value, plan, pool))
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn sort_frames(label: &str, mut frames: Vec<StackMapFrame>) -> Result<Vec<StackMapFrame>, String> {
    frames.sort_by_key(|frame| frame.offset);
    for pair in frames.windows(2) {
        if pair[0].offset == pair[1].offset {
            return Err(format!(
                "{label}: two stack-map frames land at offset {}",
                pair[0].offset
            ));
        }
    }
    Ok(frames)
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

/// The object one weave shape appends as the callback's last argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Injected {
    /// No injected object: a `HEAD` or `void` `RETURN`/`TAIL` callback.
    None,
    /// The `CallbackInfoReturnable` a non-void `RETURN`/`TAIL` insert builds.
    Returnable,
    /// The `CallbackInfo` a cancellable `void` `HEAD` insert builds.
    Cancellable,
}

/// Check a callback descriptor against the weave shape.
///
/// Returns whether the callback takes the target instance as an extra first
/// parameter (which the inserted `aload_0` supplies).  A static target's
/// callback must take exactly the target's arguments; an instance target's
/// callback may take them with or without the receiver.
///
/// `injected` names the object the callback must end with, if any: the
/// `CallbackInfoReturnable` a non-void return insert builds, or the
/// `CallbackInfo` a cancellable `void` `HEAD` insert builds.
fn callback_arguments(
    class_name: &str,
    is_static: bool,
    target_descriptor: &str,
    callback_descriptor: &str,
    anchor: &str,
    injected: Injected,
) -> Result<bool, String> {
    if !crate::valid_method_descriptor(callback_descriptor) {
        return Err(format!(
            "callback descriptor {callback_descriptor:?} is not a method descriptor"
        ));
    }
    let callback_arguments = crate::descriptor_arguments(callback_descriptor);
    let last = callback_arguments.last().map(String::as_str);
    let trailing_desc = match injected {
        Injected::None => {
            if let Some(last) = last
                && crate::is_callback_info_descriptor(last)
            {
                return Err(format!(
                    "callback {callback_descriptor} takes a trailing CallbackInfo; {anchor} weaving with CallbackInfo needs the interpreter path"
                ));
            }
            None
        }
        Injected::Returnable => {
            if last != Some(CALLBACK_INFO_RETURNABLE_DESC) {
                let detail = if last.is_some_and(crate::is_callback_info_descriptor) {
                    "a plain CallbackInfo cannot carry the result"
                } else {
                    "the callback declares no such argument"
                };
                return Err(format!(
                    "{anchor} weaving a non-void target needs the callback's last argument to be {CALLBACK_INFO_RETURNABLE_DESC}: the insert boxes the target's result into that object and returns getReturnValue(), so no override branch and no synthesised StackMapTable frame is needed; {detail}, so {callback_descriptor} cannot observe or replace the result and stays on the interpreter path"
                ));
            }
            Some(CALLBACK_INFO_RETURNABLE_DESC)
        }
        Injected::Cancellable => {
            if last != Some(CALLBACK_INFO_DESC) {
                let detail = if last == Some(CALLBACK_INFO_RETURNABLE_DESC) {
                    "a CallbackInfoReturnable belongs on a non-void RETURN/TAIL anchor"
                } else {
                    "the callback declares no CallbackInfo argument"
                };
                return Err(format!(
                    "{anchor} weaving a cancellable void target needs the callback's last argument to be {CALLBACK_INFO_DESC}, which the insert constructs and cancels with; {detail}, so {callback_descriptor} cannot cancel and stays on the interpreter path"
                ));
            }
            Some(CALLBACK_INFO_DESC)
        }
    };
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
        if let Some(trailing) = trailing_desc {
            shape.push_str(trailing);
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
    /// The parsed constant pool, so the verifier-state derivation can resolve
    /// field and method descriptors the code names.
    pool: &'a [Cp],
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

    /// The class-file major version, which decides whether a `StackMapTable`
    /// is authoritative (50 and up) or ignored (49 and below).
    fn major(&self) -> u16 {
        u16::from_be_bytes([self.layout.version[6], self.layout.version[7]])
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
    layout
        .validate_constant_pool()
        .map_err(|error| format!("{}.{}: {error}", class.name, target.name))?;
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
        pool: &class.pool,
        method_index,
        attribute_index,
        name,
        descriptor,
        is_static,
    })
}

/// The JVM type model this module needs to re-derive one `StackMapTable`
/// frame: enough to follow the local-variable types along a straight-line
/// region, not a full verifier.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ValueType {
    Top,
    Int,
    Float,
    Long,
    Double,
    Null,
    UninitializedThis,
    /// An initialized reference, by internal name or array descriptor.
    Object(String),
    /// An object whose `new` sits at this original bytecode offset and whose
    /// constructor has not run yet.
    Uninitialized(usize),
}

impl ValueType {
    fn width(&self) -> usize {
        match self {
            Self::Long | Self::Double => 2,
            _ => 1,
        }
    }

    /// The type one field or argument descriptor names.
    fn from_descriptor(descriptor: &str) -> Result<Self, String> {
        match descriptor.as_bytes().first().copied() {
            Some(b'I' | b'Z' | b'B' | b'C' | b'S') => Ok(Self::Int),
            Some(b'J') => Ok(Self::Long),
            Some(b'F') => Ok(Self::Float),
            Some(b'D') => Ok(Self::Double),
            Some(b'L') if descriptor.ends_with(';') && descriptor.len() > 2 => {
                Ok(Self::Object(descriptor[1..descriptor.len() - 1].to_owned()))
            }
            Some(b'[') => Ok(Self::Object(descriptor.to_owned())),
            _ => Err(format!("descriptor {descriptor:?} is not a value type")),
        }
    }
}

fn pool_utf8_at(pool: &[Cp], index: u16) -> Option<&str> {
    match pool.get(index as usize) {
        Some(Cp::Utf8(value)) => Some(value.as_str()),
        _ => None,
    }
}

fn pool_class_at(pool: &[Cp], index: u16) -> Option<&str> {
    match pool.get(index as usize) {
        Some(Cp::Class(name)) => pool_utf8_at(pool, *name),
        _ => None,
    }
}

fn pool_name_type_at(pool: &[Cp], index: u16) -> Option<(&str, &str)> {
    match pool.get(index as usize) {
        Some(Cp::NameType { name, descriptor }) => {
            Some((pool_utf8_at(pool, *name)?, pool_utf8_at(pool, *descriptor)?))
        }
        _ => None,
    }
}

fn pool_field_type(pool: &[Cp], index: u16) -> Option<&str> {
    match pool.get(index as usize) {
        Some(Cp::FieldRef { nt, .. }) => {
            pool_name_type_at(pool, *nt).map(|(_, descriptor)| descriptor)
        }
        _ => None,
    }
}

fn pool_method(pool: &[Cp], index: u16) -> Option<(&str, &str)> {
    match pool.get(index as usize) {
        Some(Cp::MethodRef { nt, .. } | Cp::InterfaceMethodRef { nt, .. }) => {
            pool_name_type_at(pool, *nt)
        }
        _ => None,
    }
}

/// The owner, name and descriptor one `Methodref`/`InterfaceMethodref` names.
fn invoke_method_ref(pool: &[Cp], index: u16) -> Option<(&str, &str, &str)> {
    match pool.get(index as usize) {
        Some(Cp::MethodRef { class, nt } | Cp::InterfaceMethodRef { class, nt }) => {
            let owner = pool_class_at(pool, *class)?;
            let (name, descriptor) = pool_name_type_at(pool, *nt)?;
            Some((owner, name, descriptor))
        }
        _ => None,
    }
}

fn pool_invoke_dynamic_descriptor(pool: &[Cp], index: u16) -> Option<&str> {
    match pool.get(index as usize) {
        Some(Cp::InvokeDynamic { nt, .. }) => {
            pool_name_type_at(pool, *nt).map(|(_, descriptor)| descriptor)
        }
        _ => None,
    }
}

/// The local-variable types a method starts with, as a `StackMapTable` locals
/// list (wide values take one entry).
fn initial_local_values(target: &Target<'_>) -> Result<Vec<ValueType>, String> {
    let mut values = Vec::new();
    if !target.is_static {
        values.push(if target.name == "<init>" {
            ValueType::UninitializedThis
        } else {
            ValueType::Object(target.class_name.clone())
        });
    }
    for argument in crate::descriptor_arguments(&target.descriptor) {
        values.push(ValueType::from_descriptor(&argument)?);
    }
    Ok(values)
}

/// Expand a `StackMapTable` locals list (one entry per value) to one entry per
/// local slot, which is how the derivation stores and reads it.
fn slots_from_values(values: &[ValueType]) -> Vec<ValueType> {
    let mut slots = Vec::new();
    for value in values {
        slots.push(value.clone());
        if value.width() == 2 {
            slots.push(ValueType::Top);
        }
    }
    slots
}

/// Collapse one entry per local slot back to a `StackMapTable` locals list,
/// dropping the trailing `Top`s the table leaves implicit.
fn values_from_slots(slots: &[ValueType]) -> Vec<ValueType> {
    let mut values = Vec::new();
    let mut at = 0;
    while at < slots.len() {
        let value = slots[at].clone();
        let width = value.width();
        values.push(value);
        at += width;
    }
    while values.last() == Some(&ValueType::Top) {
        values.pop();
    }
    values
}

fn value_from_verification(
    pool: &[Cp],
    verification: &VerificationType,
) -> Result<ValueType, String> {
    Ok(match verification {
        VerificationType::Simple(0) => ValueType::Top,
        VerificationType::Simple(1) => ValueType::Int,
        VerificationType::Simple(2) => ValueType::Float,
        VerificationType::Simple(3) => ValueType::Double,
        VerificationType::Simple(4) => ValueType::Long,
        VerificationType::Simple(5) => ValueType::Null,
        VerificationType::Simple(6) => ValueType::UninitializedThis,
        VerificationType::Simple(other) => {
            return Err(format!("stack-map type {other} is reserved"));
        }
        VerificationType::Object(index) => ValueType::Object(
            pool_class_at(pool, *index)
                .ok_or_else(|| format!("stack-map object index {index} is not a class"))?
                .to_owned(),
        ),
        VerificationType::Uninitialized(offset) => ValueType::Uninitialized(*offset as usize),
    })
}

fn verification_from_value(
    label: &str,
    value: &ValueType,
    plan: &CodePlan,
    pool: &mut PoolBuilder<'_>,
) -> Result<VerificationType, String> {
    Ok(match value {
        ValueType::Top => VerificationType::Simple(0),
        ValueType::Int => VerificationType::Simple(1),
        ValueType::Float => VerificationType::Simple(2),
        ValueType::Double => VerificationType::Simple(3),
        ValueType::Long => VerificationType::Simple(4),
        ValueType::Null => VerificationType::Simple(5),
        ValueType::UninitializedThis => VerificationType::Simple(6),
        ValueType::Object(name) => VerificationType::Object(pool.class(name)?),
        ValueType::Uninitialized(offset) => VerificationType::Uninitialized(shifted_u2(
            label,
            map_offset(label, plan, *offset, true)?,
            "an uninitialized offset",
        )?),
    })
}

/// One `StackMapTable` frame with its locals and stack expanded to absolute,
/// delta-free lists.
fn full_frames(
    target: &Target<'_>,
    frames: &[StackMapFrame],
) -> Result<Vec<(usize, Vec<ValueType>, Vec<ValueType>)>, String> {
    let mut locals = initial_local_values(target)?;
    let mut out = Vec::with_capacity(frames.len());
    for frame in frames {
        match frame.frame_type {
            0..=63 | 64..=127 | 247 => {}
            248..=251 => {
                let chop = 251 - frame.frame_type as usize;
                if chop > locals.len() {
                    return Err(format!(
                        "{}: a stack-map chop frame removes more locals than the method declares",
                        target.label()
                    ));
                }
                locals.truncate(locals.len() - chop);
            }
            252..=254 => {
                for verification in &frame.locals {
                    locals.push(value_from_verification(target.pool, verification)?);
                }
            }
            255 => {
                locals = frame
                    .locals
                    .iter()
                    .map(|verification| value_from_verification(target.pool, verification))
                    .collect::<Result<Vec<_>, _>>()?;
            }
            other => {
                return Err(format!(
                    "{}: stack-map frame type {other} is reserved",
                    target.label()
                ));
            }
        }
        let stack = frame
            .stack
            .iter()
            .map(|verification| value_from_verification(target.pool, verification))
            .collect::<Result<Vec<_>, _>>()?;
        out.push((frame.offset, locals.clone(), stack));
    }
    Ok(out)
}

/// The verifier state this module derives at one bytecode offset.
struct SimState {
    locals: Vec<ValueType>,
    stack: Vec<ValueType>,
}

fn pop_stack(stack: &mut Vec<ValueType>, count: usize) -> Result<(), String> {
    for _ in 0..count {
        stack
            .pop()
            .ok_or_else(|| "the operand stack underflows while deriving a frame".to_owned())?;
    }
    Ok(())
}

fn read_local(locals: &[ValueType], slot: usize) -> Result<ValueType, String> {
    if slot > 0
        && matches!(
            locals.get(slot - 1),
            Some(ValueType::Long | ValueType::Double)
        )
    {
        return Err(format!(
            "local slot {slot} is the second half of a wide local"
        ));
    }
    match locals.get(slot) {
        Some(ValueType::Top) | None => Err(format!("local slot {slot} holds no value")),
        Some(value) => Ok(value.clone()),
    }
}

fn write_local(locals: &mut Vec<ValueType>, slot: usize, value: ValueType) {
    // A store over either half of an old wide value leaves the other half
    // unusable, exactly as a `Top` slot.
    if slot > 0
        && matches!(
            locals.get(slot - 1),
            Some(ValueType::Long | ValueType::Double)
        )
    {
        locals[slot - 1] = ValueType::Top;
    }
    if matches!(locals.get(slot), Some(ValueType::Long | ValueType::Double))
        && let Some(next) = locals.get_mut(slot + 1)
    {
        *next = ValueType::Top;
    }
    if locals.len() <= slot {
        locals.resize(slot + 1, ValueType::Top);
    }
    let wide = value.width() == 2;
    locals[slot] = value;
    if wide {
        if locals.len() <= slot + 1 {
            locals.resize(slot + 2, ValueType::Top);
        }
        locals[slot + 1] = ValueType::Top;
    }
}

fn code_u1(code: &[u8], at: usize) -> Result<u8, String> {
    code.get(at)
        .copied()
        .ok_or_else(|| format!("bytecode is truncated at offset {at}"))
}

fn code_u2(code: &[u8], at: usize) -> Result<u16, String> {
    let bytes = code
        .get(at..at + 2)
        .ok_or_else(|| format!("bytecode is truncated at offset {at}"))?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

/// The top one or two values that span two local/operand slots: a `long` or
/// `double`, or two category-1 values.
fn pop_two_slots(stack: &mut Vec<ValueType>) -> Result<Vec<ValueType>, String> {
    let top = stack
        .pop()
        .ok_or_else(|| "the operand stack underflows while deriving a frame".to_owned())?;
    if top.width() == 2 {
        return Ok(vec![top]);
    }
    let under = stack
        .pop()
        .ok_or_else(|| "the operand stack underflows while deriving a frame".to_owned())?;
    if under.width() != 1 {
        return Err("a category-2 value sits where a category-1 value is required".to_owned());
    }
    Ok(vec![under, top])
}

fn pop_category_one(stack: &mut Vec<ValueType>) -> Result<ValueType, String> {
    let value = stack
        .pop()
        .ok_or_else(|| "the operand stack underflows while deriving a frame".to_owned())?;
    if value.width() != 1 {
        return Err("a category-2 value sits where a category-1 value is required".to_owned());
    }
    Ok(value)
}

fn push_values(stack: &mut Vec<ValueType>, values: Vec<ValueType>) {
    stack.extend(values);
}

/// The component type an `aaload` reads from an array descriptor.
fn array_component(descriptor: &str) -> Result<ValueType, String> {
    let rest = descriptor
        .strip_prefix('[')
        .ok_or_else(|| format!("{descriptor:?} is not an array descriptor"))?;
    if rest.starts_with('[') {
        Ok(ValueType::Object(rest.to_owned()))
    } else {
        ValueType::from_descriptor(rest)
    }
}

/// The type one `ldc`/`ldc_w` constant pushes.
fn constant_type(pool: &[Cp], index: u16) -> Result<ValueType, String> {
    Ok(match pool.get(index as usize) {
        Some(Cp::Integer(_)) => ValueType::Int,
        Some(Cp::Float(_)) => ValueType::Float,
        Some(Cp::String(_)) => ValueType::Object("java/lang/String".to_owned()),
        Some(Cp::Class(_)) => ValueType::Object("java/lang/Class".to_owned()),
        Some(Cp::MethodType) => ValueType::Object("java/lang/invoke/MethodType".to_owned()),
        Some(Cp::MethodHandle { .. }) => {
            ValueType::Object("java/lang/invoke/MethodHandle".to_owned())
        }
        other => {
            return Err(format!(
                "ldc constant {other:?} has no operand-stack type in the bounded derivation"
            ));
        }
    })
}

/// Apply one instruction to the derived state.  `Ok(true)` means control falls
/// through to the next instruction, `Ok(false)` that it transfers elsewhere
/// (or ends the method).
fn simulate_instruction(
    target: &Target<'_>,
    code: &CodeLayout<'_>,
    at: usize,
    state: &mut SimState,
) -> Result<bool, String> {
    let bytes = code.code;
    let opcode = code_u1(bytes, at)?;
    match opcode {
        0x00 => {}
        0x01 => state.stack.push(ValueType::Null),
        0x02..=0x08 => state.stack.push(ValueType::Int),
        0x09..=0x0a => state.stack.push(ValueType::Long),
        0x0b..=0x0d => state.stack.push(ValueType::Float),
        0x0e..=0x0f => state.stack.push(ValueType::Double),
        0x10 => state.stack.push(ValueType::Int),
        0x11 => state.stack.push(ValueType::Int),
        0x12 => {
            let index = code_u1(bytes, at + 1)? as u16;
            state.stack.push(constant_type(target.pool, index)?);
        }
        0x13 => {
            let index = code_u2(bytes, at + 1)?;
            state.stack.push(constant_type(target.pool, index)?);
        }
        0x14 => {
            let index = code_u2(bytes, at + 1)?;
            let value = match target.pool.get(index as usize) {
                Some(Cp::Long(_)) => ValueType::Long,
                Some(Cp::Double(_)) => ValueType::Double,
                other => {
                    return Err(format!("ldc2_w constant {other:?} is not a long or double"));
                }
            };
            state.stack.push(value);
        }
        0x15..=0x19 => {
            let slot = code_u1(bytes, at + 1)? as usize;
            state.stack.push(read_local(&state.locals, slot)?);
        }
        0x1a..=0x1d => {
            let slot = (opcode - 0x1a) as usize;
            state.stack.push(read_local(&state.locals, slot)?);
        }
        0x1e..=0x21 => {
            let slot = (opcode - 0x1e) as usize;
            state.stack.push(read_local(&state.locals, slot)?);
        }
        0x22..=0x25 => {
            let slot = (opcode - 0x22) as usize;
            state.stack.push(read_local(&state.locals, slot)?);
        }
        0x26..=0x29 => {
            let slot = (opcode - 0x26) as usize;
            state.stack.push(read_local(&state.locals, slot)?);
        }
        0x2a..=0x2d => {
            let slot = (opcode - 0x2a) as usize;
            state.stack.push(read_local(&state.locals, slot)?);
        }
        0x2e..=0x35 => {
            pop_stack(&mut state.stack, 1)?; // index
            let array = state
                .stack
                .pop()
                .ok_or_else(|| "an array load has no array on the stack".to_owned())?;
            let loaded = match opcode {
                0x2e | 0x33 | 0x34 | 0x35 => ValueType::Int,
                0x2f => ValueType::Long,
                0x30 => ValueType::Float,
                0x31 => ValueType::Double,
                _ => match array {
                    ValueType::Object(descriptor) => array_component(&descriptor)?,
                    _ => ValueType::Object("java/lang/Object".to_owned()),
                },
            };
            state.stack.push(loaded);
        }
        0x36..=0x3a => {
            let slot = code_u1(bytes, at + 1)? as usize;
            let value = state
                .stack
                .pop()
                .ok_or_else(|| "the operand stack underflows while deriving a frame".to_owned())?;
            write_local(&mut state.locals, slot, value);
        }
        0x3b..=0x3e => {
            let slot = (opcode - 0x3b) as usize;
            let value = state
                .stack
                .pop()
                .ok_or_else(|| "the operand stack underflows while deriving a frame".to_owned())?;
            write_local(&mut state.locals, slot, value);
        }
        0x3f..=0x42 => {
            let slot = (opcode - 0x3f) as usize;
            let value = state
                .stack
                .pop()
                .ok_or_else(|| "the operand stack underflows while deriving a frame".to_owned())?;
            write_local(&mut state.locals, slot, value);
        }
        0x43..=0x46 => {
            let slot = (opcode - 0x43) as usize;
            let value = state
                .stack
                .pop()
                .ok_or_else(|| "the operand stack underflows while deriving a frame".to_owned())?;
            write_local(&mut state.locals, slot, value);
        }
        0x47..=0x4a => {
            let slot = (opcode - 0x47) as usize;
            let value = state
                .stack
                .pop()
                .ok_or_else(|| "the operand stack underflows while deriving a frame".to_owned())?;
            write_local(&mut state.locals, slot, value);
        }
        0x4b..=0x4e => {
            let slot = (opcode - 0x4b) as usize;
            let value = state
                .stack
                .pop()
                .ok_or_else(|| "the operand stack underflows while deriving a frame".to_owned())?;
            write_local(&mut state.locals, slot, value);
        }
        0x4f..=0x56 => pop_stack(&mut state.stack, 3)?,
        0x57 => pop_stack(&mut state.stack, 1)?,
        0x58 => {
            let _ = pop_two_slots(&mut state.stack)?;
        }
        0x59 => {
            let value = pop_category_one(&mut state.stack)?;
            state.stack.push(value.clone());
            state.stack.push(value);
        }
        0x5a => {
            let top = pop_category_one(&mut state.stack)?;
            let under = pop_category_one(&mut state.stack)?;
            state.stack.push(top.clone());
            state.stack.push(under);
            state.stack.push(top);
        }
        0x5b => {
            let top = pop_category_one(&mut state.stack)?;
            let under = pop_two_slots(&mut state.stack)?;
            state.stack.push(top.clone());
            push_values(&mut state.stack, under);
            state.stack.push(top);
        }
        0x5c => {
            let group = pop_two_slots(&mut state.stack)?;
            push_values(&mut state.stack, group.clone());
            push_values(&mut state.stack, group);
        }
        0x5d => {
            let group = pop_two_slots(&mut state.stack)?;
            let under = pop_category_one(&mut state.stack)?;
            push_values(&mut state.stack, group.clone());
            state.stack.push(under);
            push_values(&mut state.stack, group);
        }
        0x5e => {
            let group = pop_two_slots(&mut state.stack)?;
            let under = pop_two_slots(&mut state.stack)?;
            push_values(&mut state.stack, group.clone());
            push_values(&mut state.stack, under);
            push_values(&mut state.stack, group);
        }
        0x5f => {
            let top = pop_category_one(&mut state.stack)?;
            let under = pop_category_one(&mut state.stack)?;
            state.stack.push(top);
            state.stack.push(under);
        }
        0x60..=0x63 => {
            pop_stack(&mut state.stack, 2)?;
            state.stack.push(match opcode {
                0x61 => ValueType::Long,
                0x62 => ValueType::Float,
                0x63 => ValueType::Double,
                _ => ValueType::Int,
            });
        }
        0x64..=0x67 => {
            pop_stack(&mut state.stack, 2)?;
            state.stack.push(match opcode {
                0x65 => ValueType::Long,
                0x66 => ValueType::Float,
                0x67 => ValueType::Double,
                _ => ValueType::Int,
            });
        }
        0x68..=0x6b => {
            pop_stack(&mut state.stack, 2)?;
            state.stack.push(match opcode {
                0x69 => ValueType::Long,
                0x6a => ValueType::Float,
                0x6b => ValueType::Double,
                _ => ValueType::Int,
            });
        }
        0x6c..=0x6f => {
            pop_stack(&mut state.stack, 2)?;
            state.stack.push(match opcode {
                0x6d => ValueType::Long,
                0x6e => ValueType::Float,
                0x6f => ValueType::Double,
                _ => ValueType::Int,
            });
        }
        0x70..=0x73 => {
            pop_stack(&mut state.stack, 2)?;
            state.stack.push(match opcode {
                0x71 => ValueType::Long,
                0x72 => ValueType::Float,
                0x73 => ValueType::Double,
                _ => ValueType::Int,
            });
        }
        0x74..=0x77 => {
            pop_stack(&mut state.stack, 1)?;
            state.stack.push(match opcode {
                0x75 => ValueType::Long,
                0x76 => ValueType::Float,
                0x77 => ValueType::Double,
                _ => ValueType::Int,
            });
        }
        0x78 | 0x7a | 0x7c => {
            pop_stack(&mut state.stack, 2)?;
            state.stack.push(ValueType::Int);
        }
        0x79 | 0x7b | 0x7d => {
            pop_stack(&mut state.stack, 2)?;
            state.stack.push(ValueType::Long);
        }
        0x7e | 0x80 | 0x82 => {
            pop_stack(&mut state.stack, 2)?;
            state.stack.push(ValueType::Int);
        }
        0x7f | 0x81 | 0x83 => {
            pop_stack(&mut state.stack, 2)?;
            state.stack.push(ValueType::Long);
        }
        0x84 => {
            // `iinc` keeps the local's `int` type.
        }
        0x85..=0x93 => {
            let converted = match opcode {
                0x85 => ValueType::Long,
                0x86 => ValueType::Float,
                0x87 => ValueType::Double,
                0x88 | 0x8b | 0x8e | 0x91 | 0x92 | 0x93 => ValueType::Int,
                0x89 | 0x8c | 0x8f => ValueType::Float,
                0x8a | 0x8d | 0x90 => ValueType::Double,
                _ => unreachable!(),
            };
            pop_stack(&mut state.stack, 1)?;
            state.stack.push(converted);
        }
        0x94..=0x98 => {
            pop_stack(&mut state.stack, 2)?;
            state.stack.push(ValueType::Int);
        }
        0x99..=0x9e | 0xc6 | 0xc7 => pop_stack(&mut state.stack, 1)?,
        0x9f..=0xa6 => pop_stack(&mut state.stack, 2)?,
        0xa7 => return Ok(false),
        0xa8 => {
            return Err(
                "the derived state cannot follow a jsr; the bounded weaver does not rewrite subroutines"
                    .to_owned(),
            );
        }
        0xa9 => return Ok(false),
        0xac..=0xb1 => return Ok(false),
        0xb2 | 0xb4 => {
            let index = code_u2(bytes, at + 1)?;
            let descriptor = pool_field_type(target.pool, index)
                .ok_or_else(|| format!("field reference {index} is missing from the pool"))?;
            if opcode == 0xb4 {
                pop_stack(&mut state.stack, 1)?;
            }
            state.stack.push(ValueType::from_descriptor(descriptor)?);
        }
        0xb3 | 0xb5 => {
            let index = code_u2(bytes, at + 1)?;
            if pool_field_type(target.pool, index).is_none() {
                return Err(format!("field reference {index} is missing from the pool"));
            }
            pop_stack(&mut state.stack, 1)?;
            if opcode == 0xb5 {
                pop_stack(&mut state.stack, 1)?;
            }
        }
        0xb6..=0xb9 => {
            let index = code_u2(bytes, at + 1)?;
            let (name, descriptor) = pool_method(target.pool, index)
                .ok_or_else(|| format!("method reference {index} is missing from the pool"))?;
            let arguments = crate::descriptor_arguments(descriptor);
            pop_stack(&mut state.stack, arguments.len())?;
            let receiver = if opcode == 0xb8 {
                None
            } else {
                Some(
                    state
                        .stack
                        .pop()
                        .ok_or_else(|| "a method call has no receiver on the stack".to_owned())?,
                )
            };
            if let Some(receiver) = receiver
                && opcode == 0xb7
                && name == "<init>"
            {
                let uninitialized = receiver.clone();
                let initialized = match receiver {
                    ValueType::Uninitialized(offset) => {
                        let new_at = offset;
                        let class_index = code_u2(bytes, new_at + 1)?;
                        ValueType::Object(
                            pool_class_at(target.pool, class_index)
                                .ok_or_else(|| {
                                    format!("new at offset {new_at} names an unknown class")
                                })?
                                .to_owned(),
                        )
                    }
                    ValueType::UninitializedThis => ValueType::Object(target.class_name.clone()),
                    other => other,
                };
                for value in state.stack.iter_mut().chain(state.locals.iter_mut()) {
                    if *value == uninitialized {
                        *value = initialized.clone();
                    }
                }
            }
            let returned = crate::return_desc(descriptor);
            if returned != "V" {
                state.stack.push(ValueType::from_descriptor(returned)?);
            }
        }
        0xba => {
            let index = code_u2(bytes, at + 1)?;
            let descriptor =
                pool_invoke_dynamic_descriptor(target.pool, index).ok_or_else(|| {
                    format!("invokedynamic reference {index} is missing from the pool")
                })?;
            pop_stack(
                &mut state.stack,
                crate::descriptor_arguments(descriptor).len(),
            )?;
            let returned = crate::return_desc(descriptor);
            if returned != "V" {
                state.stack.push(ValueType::from_descriptor(returned)?);
            }
        }
        0xbb => {
            let index = code_u2(bytes, at + 1)?;
            let name = pool_class_at(target.pool, index)
                .ok_or_else(|| format!("new names class {index}, which is not a class"))?;
            state.stack.push(ValueType::Uninitialized(at));
            let _ = name;
        }
        0xbc => {
            pop_stack(&mut state.stack, 1)?;
            let array = match code_u1(bytes, at + 1)? {
                4 => "[Z",
                5 => "[C",
                6 => "[F",
                7 => "[D",
                8 => "[B",
                9 => "[S",
                10 => "[I",
                11 => "[J",
                other => {
                    return Err(format!(
                        "newarray type {other} is not a primitive array type"
                    ));
                }
            };
            state.stack.push(ValueType::Object(array.to_owned()));
        }
        0xbd => {
            pop_stack(&mut state.stack, 1)?;
            let index = code_u2(bytes, at + 1)?;
            let name = pool_class_at(target.pool, index)
                .ok_or_else(|| format!("anewarray names class {index}, which is not a class"))?;
            state.stack.push(ValueType::Object(format!("[L{name};")));
        }
        0xbe => {
            pop_stack(&mut state.stack, 1)?;
            state.stack.push(ValueType::Int);
        }
        0xbf => {
            pop_stack(&mut state.stack, 1)?;
            return Ok(false);
        }
        0xc0 => {
            pop_stack(&mut state.stack, 1)?;
            let index = code_u2(bytes, at + 1)?;
            let name = pool_class_at(target.pool, index)
                .ok_or_else(|| format!("checkcast names class {index}, which is not a class"))?;
            state.stack.push(ValueType::Object(name.to_owned()));
        }
        0xc1 => {
            pop_stack(&mut state.stack, 1)?;
            state.stack.push(ValueType::Int);
        }
        0xc2 | 0xc3 => pop_stack(&mut state.stack, 1)?,
        0xc4 => {
            let sub = code_u1(bytes, at + 1)?;
            match sub {
                0x15..=0x19 => {
                    let slot = code_u2(bytes, at + 2)? as usize;
                    state.stack.push(read_local(&state.locals, slot)?);
                }
                0x36..=0x3a => {
                    let slot = code_u2(bytes, at + 2)? as usize;
                    let value = state.stack.pop().ok_or_else(|| {
                        "the operand stack underflows while deriving a frame".to_owned()
                    })?;
                    write_local(&mut state.locals, slot, value);
                }
                0x84 => {}
                0xa9 => return Ok(false),
                other => {
                    return Err(format!(
                        "wide prefixes opcode 0x{other:02x}, which the bounded derivation cannot follow"
                    ));
                }
            }
        }
        0xc5 => {
            let dimensions = code_u1(bytes, at + 3)? as usize;
            pop_stack(&mut state.stack, dimensions)?;
            let index = code_u2(bytes, at + 1)?;
            let name = pool_class_at(target.pool, index).ok_or_else(|| {
                format!("multianewarray names class {index}, which is not a class")
            })?;
            state.stack.push(ValueType::Object(name.to_owned()));
        }
        0xc8 => return Ok(false),
        0xc9 => {
            return Err(
                "the derived state cannot follow a jsr_w; the bounded weaver does not rewrite subroutines"
                    .to_owned(),
            );
        }
        other => {
            return Err(format!(
                "the bounded frame derivation does not model opcode 0x{other:02x}"
            ));
        }
    }
    Ok(true)
}

/// Derive the verifier state just before the instruction at `stop` by following
/// the straight-line region from the nearest preceding `StackMapTable` frame
/// (or the method entry), and applying every instruction's effect on the local
/// variables and the operand stack.
///
/// The region between two frames cannot have a second entry - an instruction a
/// branch could reach would carry its own frame - so a straight-line walk is
/// exact.  An instruction the bounded model cannot follow, or control that does
/// not fall through, is a reason rather than a guessed frame.
fn derive_state_at(
    label: &str,
    target: &Target<'_>,
    code: &CodeLayout<'_>,
    scan: &CodeScan,
    stop: usize,
) -> Result<SimState, String> {
    let frames = match code
        .attributes
        .iter()
        .find_map(|attribute| match attribute {
            CodeAttribute::StackMap { frames, .. } => Some(frames),
            _ => None,
        }) {
        Some(frames) => full_frames(target, frames)?,
        None => Vec::new(),
    };
    let (start, mut state) = match frames.iter().rev().find(|(offset, _, _)| *offset <= stop) {
        Some((offset, locals, stack)) => (
            *offset,
            SimState {
                locals: slots_from_values(locals),
                stack: stack.clone(),
            },
        ),
        None => (
            0,
            SimState {
                locals: slots_from_values(&initial_local_values(target)?),
                stack: Vec::new(),
            },
        ),
    };
    if start == stop {
        return Ok(state);
    }
    let instructions = scan
        .instructions
        .iter()
        .map(|instruction| (instruction.at, instruction))
        .collect::<BTreeMap<_, _>>();
    let mut at = start;
    while at < stop {
        let instruction = instructions.get(&at).ok_or_else(|| {
            format!(
                "{label}: cannot derive the verifier state at {stop}: offset {at} is not an instruction boundary"
            )
        })?;
        if instruction.switch.is_some() {
            pop_stack(&mut state.stack, 1)?;
        } else if !simulate_instruction(target, code, at, &mut state)? {
            return Err(format!(
                "{label}: cannot derive the verifier state at {stop}: control does not fall through from offset {at}"
            ));
        }
        at = instruction.at + instruction.size;
    }
    Ok(state)
}

/// The opcode of the branch that skips the wide jump of an expanded
/// conditional, or `None` when the branch cannot be split this way.
fn inverse_conditional(opcode: u8) -> Option<u8> {
    Some(match opcode {
        0x99 => 0x9a, // ifeq <-> ifne
        0x9a => 0x99,
        0x9b => 0x9c, // iflt <-> ifge
        0x9c => 0x9b,
        0x9d => 0x9e, // ifgt <-> ifle
        0x9e => 0x9d,
        0x9f => 0xa0, // if_icmpeq <-> if_icmpne
        0xa0 => 0x9f,
        0xa1 => 0xa2, // if_icmplt <-> if_icmpge
        0xa2 => 0xa1,
        0xa3 => 0xa4, // if_icmpgt <-> if_icmple
        0xa4 => 0xa3,
        0xa5 => 0xa6, // if_acmpeq <-> if_acmpne
        0xa6 => 0xa5,
        0xc6 => 0xc7, // ifnull <-> ifnonnull
        0xc7 => 0xc6,
        _ => return None,
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

/// Append a local-variable instruction naming `slot`.
///
/// A slot above 255 needs the `wide` prefix; the class-file format caps a
/// method's parameter slots well below that, so this only runs for a hand-built
/// descriptor or a method that already declares 255+ locals, but emitting the
/// wide form keeps the insert faithful to the slot it names instead of silently
/// truncating the index.
fn local_opcode(out: &mut Vec<u8>, opcode: u8, slot: u16) {
    if slot > u8::MAX as u16 {
        out.push(0xc4); // wide
        out.push(opcode);
        out.extend_from_slice(&slot.to_be_bytes());
    } else {
        out.push(opcode);
        out.push(slot as u8);
    }
}

/// One local load of a callback argument.
#[derive(Debug, Clone, Copy)]
struct LocalLoad {
    /// The opcode: the compressed one-byte form (`aload_0`) for the receiver,
    /// the indexed form (`iload 1`) for an argument.
    opcode: u8,
    /// The slot an indexed opcode names; `None` for the compressed form.
    slot: Option<u16>,
    /// Local slots the value spans.
    width: u16,
}

impl LocalLoad {
    /// `aload_0`, which supplies an instance callback's receiver.
    fn receiver() -> Self {
        Self {
            opcode: 0x2a,
            slot: None,
            width: 1,
        }
    }

    fn argument(opcode: u8, slot: u16, width: u16) -> Self {
        Self {
            opcode,
            slot: Some(slot),
            width,
        }
    }

    fn emit(&self, out: &mut Vec<u8>) {
        match self.slot {
            Some(slot) => local_opcode(out, self.opcode, slot),
            None => out.push(self.opcode),
        }
    }

    /// The local slot the load reads, and the slots it spans.
    fn span(&self) -> (u16, u16) {
        (self.slot.unwrap_or(0), self.width)
    }
}

/// The local loads a callback's own arguments need, in order: the optional
/// receiver first, then the target's arguments, each with the local slot it
/// occupies and the slots it spans.
fn argument_loads(target: &Target<'_>, takes_receiver: bool) -> Result<Vec<LocalLoad>, String> {
    let arguments = crate::descriptor_arguments(&target.descriptor);
    let mut loads = Vec::with_capacity(arguments.len() + 1);
    let mut slot: u16 = if target.is_static { 0 } else { 1 };
    if takes_receiver {
        loads.push(LocalLoad::receiver());
    }
    for argument in &arguments {
        let (opcode, width) = load_opcode(argument)?;
        loads.push(LocalLoad::argument(opcode, slot, width));
        slot = slot.checked_add(width).ok_or("local slot overflow")?;
    }
    Ok(loads)
}

/// The local slots the target's arguments occupy, receiver included.
fn argument_slots(target: &Target<'_>) -> Result<u16, String> {
    let mut slots: u16 = if target.is_static { 0 } else { 1 };
    for argument in crate::descriptor_arguments(&target.descriptor) {
        let (_, width) = load_opcode(&argument)?;
        slots = slots.checked_add(width).ok_or("local slot overflow")?;
    }
    Ok(slots)
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
    let mut inserted = Vec::new();
    let mut slots = 0u16;
    let mut local_slots = if target.is_static { 0 } else { 1 };
    for load in argument_loads(target, takes_receiver)? {
        load.emit(&mut inserted);
        let (slot, width) = load.span();
        local_slots = local_slots.max(slot.checked_add(width).ok_or("local slot overflow")?);
        slots = slots.checked_add(width).ok_or("argument slot overflow")?;
    }
    let callback_index = pool.method_ref(callback)?;
    inserted.push(0xb8); // invokestatic
    inserted.extend_from_slice(&callback_index.to_be_bytes());
    Ok(CallbackCall {
        bytes: inserted,
        stack_slots: slots,
        local_slots,
    })
}

/// The wrapper class, `valueOf` descriptor and unboxing method that move one
/// primitive return descriptor through a `CallbackInfoReturnable`.
fn primitive_wrapper(descriptor: &str) -> Option<(&'static str, &'static str, &'static str)> {
    Some(match descriptor {
        "Z" => (
            "java/lang/Boolean",
            "(Z)Ljava/lang/Boolean;",
            "booleanValue",
        ),
        "B" => ("java/lang/Byte", "(B)Ljava/lang/Byte;", "byteValue"),
        "C" => (
            "java/lang/Character",
            "(C)Ljava/lang/Character;",
            "charValue",
        ),
        "S" => ("java/lang/Short", "(S)Ljava/lang/Short;", "shortValue"),
        "I" => ("java/lang/Integer", "(I)Ljava/lang/Integer;", "intValue"),
        "J" => ("java/lang/Long", "(J)Ljava/lang/Long;", "longValue"),
        "F" => ("java/lang/Float", "(F)Ljava/lang/Float;", "floatValue"),
        "D" => ("java/lang/Double", "(D)Ljava/lang/Double;", "doubleValue"),
        _ => return None,
    })
}

/// The `CONSTANT_Class` name an `areturn` type's `checkcast` names: the class
/// name inside `L...;`, or the array descriptor itself.
fn reference_class_name(descriptor: &str) -> Result<&str, String> {
    if let Some(name) = descriptor
        .strip_prefix('L')
        .and_then(|rest| rest.strip_suffix(';'))
        && !name.is_empty()
        && !name.contains(';')
    {
        return Ok(name);
    }
    if descriptor.starts_with('[') {
        return Ok(descriptor);
    }
    Err(format!(
        "return descriptor {descriptor:?} is neither a primitive nor a reference type"
    ))
}

/// Emit one non-void `RETURN`/`TAIL` insert.
///
/// The target's result is on the operand stack when the insert runs.  The insert
/// boxes it into the fresh local `slot`, builds a `CallbackInfoReturnable`
/// around it, calls the callback with the woven method's arguments and that
/// object, then replaces the result with `getReturnValue()` in the target's own
/// shape.  No branch is needed: the object starts with the target's value, so
/// "no override" and "override" are the same read.
///
/// `slot` holds the boxed value and then the object itself - the value is dead
/// once `<init>` ran - so one fresh local is enough.  It sits at the callback's
/// own argument position, which is why the object is built before the arguments
/// are pushed: the arguments consume the topmost object and the second `aload`
/// keeps the woven method's own reference for `getReturnValue`.
fn returnable_call(
    target: &Target<'_>,
    callback: &MethodRef,
    takes_receiver: bool,
    slot: u16,
    pool: &mut PoolBuilder<'_>,
) -> Result<CallbackCall, String> {
    let returns = crate::return_desc(&target.descriptor);
    let wrapper = primitive_wrapper(returns);
    let mut bytes = Vec::new();
    if let Some((wrapper, box_descriptor, _)) = wrapper {
        let value_of = pool.method_ref(&MethodRef {
            owner: wrapper.to_owned(),
            name: "valueOf".to_owned(),
            descriptor: box_descriptor.to_owned(),
        })?;
        bytes.push(0xb8); // invokestatic <wrapper>.valueOf
        bytes.extend_from_slice(&value_of.to_be_bytes());
    }
    local_opcode(&mut bytes, 0x3a, slot); // astore <slot>
    let returnable = pool.class(CALLBACK_INFO_RETURNABLE_CLASS)?;
    bytes.push(0xbb); // new CallbackInfoReturnable
    bytes.extend_from_slice(&returnable.to_be_bytes());
    bytes.push(0x59); // dup
    local_opcode(&mut bytes, 0x19, slot); // aload <slot>
    let init = pool.method_ref(&MethodRef {
        owner: CALLBACK_INFO_RETURNABLE_CLASS.to_owned(),
        name: "<init>".to_owned(),
        descriptor: "(Ljava/lang/Object;)V".to_owned(),
    })?;
    bytes.push(0xb7); // invokespecial CallbackInfoReturnable.<init>(Object)
    bytes.extend_from_slice(&init.to_be_bytes());
    local_opcode(&mut bytes, 0x3a, slot); // astore <slot>: the value is dead
    let mut slots = 0u16;
    let mut local_slots = slot
        .checked_add(1)
        .ok_or("the fresh local does not fit the class-file local-slot limit")?;
    for load in argument_loads(target, takes_receiver)? {
        load.emit(&mut bytes);
        let (index, width) = load.span();
        local_slots = local_slots.max(index.saturating_add(width));
        slots = slots.saturating_add(width);
    }
    local_opcode(&mut bytes, 0x19, slot); // aload <slot>
    let callback_index = pool.method_ref(callback)?;
    bytes.push(0xb8); // invokestatic callback
    bytes.extend_from_slice(&callback_index.to_be_bytes());
    local_opcode(&mut bytes, 0x19, slot); // aload <slot>
    let get_return = pool.method_ref(&MethodRef {
        owner: CALLBACK_INFO_RETURNABLE_CLASS.to_owned(),
        name: "getReturnValue".to_owned(),
        descriptor: "()Ljava/lang/Object;".to_owned(),
    })?;
    bytes.push(0xb6); // invokevirtual getReturnValue
    bytes.extend_from_slice(&get_return.to_be_bytes());
    match wrapper {
        Some((wrapper, _, unbox)) => {
            let cast = pool.class(wrapper)?;
            bytes.push(0xc0); // checkcast <wrapper>
            bytes.extend_from_slice(&cast.to_be_bytes());
            let unbox = pool.method_ref(&MethodRef {
                owner: wrapper.to_owned(),
                name: unbox.to_owned(),
                descriptor: format!("(){returns}"),
            })?;
            bytes.push(0xb6); // invokevirtual <wrapper>.<unbox>
            bytes.extend_from_slice(&unbox.to_be_bytes());
        }
        None => {
            let cast = pool.class(reference_class_name(returns)?)?;
            bytes.push(0xc0); // checkcast <return type>
            bytes.extend_from_slice(&cast.to_be_bytes());
        }
    }
    Ok(CallbackCall {
        bytes,
        // `new` + `dup` + the boxed value sit above the callback's arguments.
        stack_slots: slots.saturating_add(3),
        local_slots,
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
/// malformed condy, abstract/native methods, a mismatched callback descriptor,
/// `CallbackInfo` parameters, a `wide` prefix that is not a local-variable
/// instruction, or a conditional branch the split cannot express.
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
        Injected::None,
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
        &target,
        code,
        &scan,
        &[(0, call.bytes.len())],
        &call.bytes,
        max_stack,
        max_locals,
        TargetEntry::Skip,
        FrameSites::Both,
        &[],
        &mut pool,
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

/// Insert a callback immediately before the target's `return` instructions.
///
/// [`ReturnMode::Return`] inserts before every return-family instruction in
/// the method; [`ReturnMode::Tail`] inserts before the last one only, matching
/// Mixin's `RETURN` and `TAIL` anchors.
///
/// A `void` target weaves the plain `invokestatic`.  A non-void target also
/// carries its result: the callback must end with a `CallbackInfoReturnable`,
/// which the insert builds from the target's boxed value, and the method then
/// returns `getReturnValue()` in its own shape, so a `setReturnValue` override
/// wins and the target's value survives when none is set.  A `cancel()` keeps
/// the override rather than discarding it - the method returns
/// `getReturnValue()` either way, which is what Mixin's own cancellable
/// `RETURN` handler does.  The object [`callback_info_returnable_class`] writes
/// is the class such a jar needs.
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
/// [`weave_head`]: malformed condy, abstract/native methods, a mismatched
/// callback descriptor, or a `void` target whose callback asks for a
/// `CallbackInfo`.
pub fn weave_return(
    class: &ClassFile,
    target: &MethodRef,
    callback: &MethodRef,
    mode: ReturnMode,
) -> Result<Vec<u8>, String> {
    let target = prepare_target(class, target)?;
    let label = target.label();
    let anchor = mode.anchor();
    let returns_value = crate::return_desc(&target.descriptor) != "V";
    let takes_receiver = callback_arguments(
        &target.class_name,
        target.is_static,
        &target.descriptor,
        &callback.descriptor,
        anchor,
        if returns_value {
            Injected::Returnable
        } else {
            Injected::None
        },
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
    // A non-void insert keeps the boxed result in a fresh local past every one
    // the method declares, so it can never shadow an argument or a local.
    let (call, frame_sites) = if returns_value {
        let slot = code.max_locals.max(argument_slots(&target)?);
        let call = returnable_call(&target, callback, takes_receiver, slot, &mut pool)?;
        let sites = FrameSites::InsertOnly;
        (call, sites)
    } else {
        let call = callback_call(&target, callback, takes_receiver, &mut pool)?;
        (call, FrameSites::Both)
    };
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
        &target,
        code,
        &scan,
        &insertions,
        &call.bytes,
        max_stack,
        max_locals,
        TargetEntry::Enter,
        frame_sites,
        &[],
        &mut pool,
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

/// Emit the shared prefix of a cancellable insert: construct one
/// `CallbackInfo`, call the callback with the target's arguments (and receiver,
/// when it declares it) plus that object, and leave the object on the stack.
///
/// Returns the bytes, the peak number of operand-stack slots they need, and
/// the highest local slot they read plus one.
fn callback_info_call(
    target: &Target<'_>,
    callback: &MethodRef,
    takes_receiver: bool,
    slot: u16,
    pool: &mut PoolBuilder<'_>,
) -> Result<(Vec<u8>, u16, u16), String> {
    let mut bytes = Vec::new();
    let info = pool.class(CALLBACK_INFO_CLASS)?;
    bytes.push(0xbb); // new CallbackInfo
    bytes.extend_from_slice(&info.to_be_bytes());
    bytes.push(0x59); // dup
    let init = pool.method_ref(&MethodRef {
        owner: CALLBACK_INFO_CLASS.to_owned(),
        name: "<init>".to_owned(),
        descriptor: "()V".to_owned(),
    })?;
    bytes.push(0xb7); // invokespecial CallbackInfo.<init>()V
    bytes.extend_from_slice(&init.to_be_bytes());
    local_opcode(&mut bytes, 0x3a, slot); // astore <slot>
    let mut slots = 0u16;
    let mut local_slots = slot
        .checked_add(1)
        .ok_or("the fresh local does not fit the class-file local-slot limit")?;
    for load in argument_loads(target, takes_receiver)? {
        load.emit(&mut bytes);
        let (index, width) = load.span();
        local_slots = local_slots.max(index.saturating_add(width));
        slots = slots.saturating_add(width);
    }
    local_opcode(&mut bytes, 0x19, slot); // aload <slot>
    let callback_index = pool.method_ref(callback)?;
    bytes.push(0xb8); // invokestatic callback
    bytes.extend_from_slice(&callback_index.to_be_bytes());
    local_opcode(&mut bytes, 0x19, slot); // aload <slot>
    Ok((bytes, slots.saturating_add(1).max(2), local_slots))
}

/// Emit one cancellable `void` `HEAD` insert.
///
/// The insert builds a `CallbackInfo`, calls the callback with the target's
/// arguments plus that object, and branches around the target body when the
/// callback cancelled.  A cancelled `void` method answers with its only
/// possible default: a plain `return`.
fn cancellable_head_call(
    target: &Target<'_>,
    callback: &MethodRef,
    takes_receiver: bool,
    slot: u16,
    pool: &mut PoolBuilder<'_>,
) -> Result<CallbackCall, String> {
    let (mut bytes, stack_slots, local_slots) =
        callback_info_call(target, callback, takes_receiver, slot, pool)?;
    let is_cancelled = pool.method_ref(&MethodRef {
        owner: CALLBACK_INFO_CLASS.to_owned(),
        name: "isCancelled".to_owned(),
        descriptor: "()Z".to_owned(),
    })?;
    bytes.push(0xb6); // invokevirtual isCancelled
    bytes.extend_from_slice(&is_cancelled.to_be_bytes());
    // `ifeq body; return`: the branch target is the first original instruction,
    // which the insert places right after these bytes, so the operand is the
    // fixed distance over the `return`.
    let ifeq_at = bytes.len();
    bytes.push(0x99); // ifeq
    let operand = bytes.len();
    bytes.extend_from_slice(&[0, 0]);
    bytes.push(0xb1); // return
    let body = bytes.len();
    let delta = i16::try_from(body as i64 - ifeq_at as i64)
        .map_err(|_| "the cancellable insert cannot reach its body".to_owned())?;
    bytes[operand..operand + 2].copy_from_slice(&delta.to_be_bytes());
    Ok(CallbackCall {
        bytes,
        // `new` + `dup` while the stack is empty, then the arguments and the
        // `CallbackInfo` at the call.
        stack_slots,
        local_slots,
    })
}

/// The `void` target's entry locals, extended with the fresh local the
/// cancellable insert stores the `CallbackInfo` in.
fn cancellable_entry_locals(target: &Target<'_>, slot: u16) -> Result<Vec<ValueType>, String> {
    let mut slots = slots_from_values(&initial_local_values(target)?);
    if slots.len() < slot as usize {
        slots.resize(slot as usize, ValueType::Top);
    }
    write_local(
        &mut slots,
        slot as usize,
        ValueType::Object(CALLBACK_INFO_CLASS.to_owned()),
    );
    Ok(slots)
}

/// Insert a cancellable `CallbackInfo` callback at offset 0 of a `void`
/// `target`.
///
/// The insert constructs [`callback_info_class`], calls the callback with the
/// target's arguments (and receiver, when it declares it) plus that object, and
/// emits `if (!ci.isCancelled()) goto body; return`, so a cancelled callback
/// skips the target body and the method answers with the `void` default.  A
/// non-cancellable `HEAD` plan keeps the plain [`weave_head`] shape byte for
/// byte.
///
/// Only a `void` target is covered: a non-void one would also need a default
/// value in the target's shape, which the interpreter path supplies.
pub fn weave_head_cancellable(
    class: &ClassFile,
    target: &MethodRef,
    callback: &MethodRef,
) -> Result<Vec<u8>, String> {
    let target = prepare_target(class, target)?;
    let label = target.label();
    if crate::return_desc(&target.descriptor) != "V" {
        return Err(format!(
            "{label}: cancellable HEAD weaving is bounded to void targets; a non-void target needs a CallbackInfoReturnable and stays on the interpreter path"
        ));
    }
    let takes_receiver = callback_arguments(
        &target.class_name,
        target.is_static,
        &target.descriptor,
        &callback.descriptor,
        "HEAD",
        Injected::Cancellable,
    )?;

    let code = target.code()?;
    let scan = scan_code(&label, code.code)?;
    let slot = code.max_locals.max(argument_slots(&target)?);
    let mut pool = PoolBuilder::new(&class.pool);
    let call = cancellable_head_call(&target, callback, takes_receiver, slot, &mut pool)?;
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
    let entry = cancellable_entry_locals(&target, slot)?;
    let patch = relocate(
        &label,
        &target,
        code,
        &scan,
        &[(0, call.bytes.len())],
        &call.bytes,
        max_stack,
        max_locals,
        TargetEntry::Skip,
        FrameSites::Both,
        &[SyntheticFrame {
            offset: 0,
            locals: entry,
            stack: Vec::new(),
            at_instruction: true,
        }],
        &mut pool,
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

/// One invocation instruction a plan selects: `invokevirtual`, `invokestatic`
/// or `invokeinterface`.
struct InvokeSite {
    at: usize,
    size: usize,
    descriptor: String,
    /// Whether the call pushes a receiver (`invokevirtual`/`invokeinterface`).
    receiver: bool,
}

/// The call sites of `call_site` in one method.
///
/// A selector with an empty descriptor matches every overload of the name.
/// `invokespecial` is reported rather than matched: rewriting a constructor or
/// private call is outside the bounded shape.
fn invoke_sites(
    label: &str,
    target: &Target<'_>,
    code: &CodeLayout<'_>,
    scan: &CodeScan,
    call_site: &MethodRef,
) -> Result<Vec<InvokeSite>, String> {
    let mut sites = Vec::new();
    for instruction in &scan.instructions {
        let opcode = code_u1(code.code, instruction.at)?;
        if !matches!(opcode, 0xb6..=0xb9) {
            continue;
        }
        let index = code_u2(code.code, instruction.at + 1)?;
        let Some((owner, name, descriptor)) = invoke_method_ref(target.pool, index) else {
            continue;
        };
        if owner != call_site.owner
            || name != call_site.name
            || (!call_site.descriptor.is_empty() && descriptor != call_site.descriptor)
        {
            continue;
        }
        if opcode == 0xb7 {
            return Err(format!(
                "{label}: INVOKE weaving cannot rewrite the invokespecial {owner}.{name}{descriptor} call site at {}; only invokevirtual, invokestatic and invokeinterface are bounded",
                instruction.at
            ));
        }
        sites.push(InvokeSite {
            at: instruction.at,
            size: instruction.size,
            descriptor: descriptor.to_owned(),
            receiver: opcode != 0xb8,
        });
    }
    Ok(sites)
}

/// The `pop`/`pop2` opcode that removes one call argument of this descriptor.
fn pop_opcode(descriptor: &str) -> Result<u8, String> {
    let first = descriptor
        .as_bytes()
        .first()
        .copied()
        .ok_or_else(|| "empty call argument descriptor".to_owned())?;
    Ok(match first {
        b'J' | b'D' => 0x58, // pop2
        b'V' => {
            return Err(format!("call argument descriptor {descriptor:?} is void"));
        }
        _ => 0x57, // pop
    })
}

/// One cancellable call-site insert with the positions of the two branch
/// operands the layout has to patch.
struct CancellableInvoke {
    bytes: Vec<u8>,
    ifeq: usize,
    ifeq_operand: usize,
    goto: usize,
    goto_operand: usize,
    stack_slots: u16,
    local_slots: u16,
}

/// Emit the insert that runs one callback before a `void` call and, when the
/// callback cancelled, pops the call's receiver and arguments and jumps to the
/// instruction after the call.
///
/// The inserted shape is:
///
/// ```text
/// <CallbackInfo setup and callback call>
/// aload <slot>; invokevirtual CallbackInfo.isCancelled()Z
/// ifeq <the call>          (not cancelled)
/// pop/pop2 ...             (the call's arguments, then the receiver)
/// goto <after the call>
/// <the call itself>        (the original instruction, at insert end)
/// <after the call>         (the original next instruction)
/// ```
///
/// Both branch operands are patched from the stable layout, because the `goto`
/// has to land on the start of any insert in front of the original next
/// instruction, not on the instruction itself.
fn cancellable_invoke_call(
    target: &Target<'_>,
    callback: &MethodRef,
    takes_receiver: bool,
    site: &InvokeSite,
    slot: u16,
    pool: &mut PoolBuilder<'_>,
) -> Result<CancellableInvoke, String> {
    let (mut bytes, stack_slots, local_slots) =
        callback_info_call(target, callback, takes_receiver, slot, pool)?;
    let is_cancelled = pool.method_ref(&MethodRef {
        owner: CALLBACK_INFO_CLASS.to_owned(),
        name: "isCancelled".to_owned(),
        descriptor: "()Z".to_owned(),
    })?;
    bytes.push(0xb6); // invokevirtual isCancelled
    bytes.extend_from_slice(&is_cancelled.to_be_bytes());
    push_ifeq(&mut bytes);
    let ifeq = bytes.len() - 3;
    let ifeq_operand = ifeq + 1;
    for argument in crate::descriptor_arguments(&site.descriptor).iter().rev() {
        bytes.push(pop_opcode(argument)?);
    }
    if site.receiver {
        bytes.push(0x57); // pop the receiver
    }
    push_goto(&mut bytes);
    let goto = bytes.len() - 3;
    let goto_operand = goto + 1;
    Ok(CancellableInvoke {
        bytes,
        ifeq,
        ifeq_operand,
        goto,
        goto_operand,
        stack_slots,
        local_slots,
    })
}

fn push_ifeq(out: &mut Vec<u8>) {
    out.push(0x99); // ifeq
    out.extend_from_slice(&[0, 0]);
}

fn push_goto(out: &mut Vec<u8>) {
    out.push(0xa7); // goto
    out.extend_from_slice(&[0, 0]);
}

/// Insert a callback immediately before every call site a plan selects.
///
/// `target` is the woven method (the plan's `target`); `call_site` is the
/// `@At(INVOKE)` selector.  Only `invokevirtual`, `invokestatic` and
/// `invokeinterface` call sites are bounded: those are the instructions whose
/// operand stack the insert can leave untouched (non-cancellable) or pop with
/// known types (cancellable).  A selector with an empty descriptor matches
/// every overload of the name.
///
/// The callback shape is the one the interpreter validates for an INVOKE plan:
/// it mirrors the woven method's arguments, may take the woven method's
/// receiver first, may end with a `CallbackInfo` when `cancellable`, and
/// returns `void`.  The callback therefore sees the method the call site lives
/// in, not the callee's arguments.
///
/// A non-cancellable insert is stack-neutral: it is the same load-arguments and
/// `invokestatic` sequence a `HEAD` insert emits, placed before each matching
/// call.  A cancellable insert constructs [`callback_info_class`], calls the
/// callback, and - when `ci.cancel()` ran - pops the call's arguments with
/// their own widths and branches around the call.  Cancellation is bounded to
/// a `void` call site with exactly one matching instruction, because a
/// non-void call would also have to push the callee descriptor's default value.
///
/// Both shapes relocate branches, switches, the exception table and
/// `StackMapTable` exactly as [`weave_head`] does.  A cancellable insert adds
/// the two verifier frames its branches need (the call and the instruction
/// after it), derived from the method's own frames instead of guessed.
pub fn weave_invoke(
    class: &ClassFile,
    target: &MethodRef,
    callback: &MethodRef,
    call_site: &MethodRef,
    cancellable: bool,
) -> Result<Vec<u8>, String> {
    let target = prepare_target(class, target)?;
    let label = target.label();
    let takes_receiver = callback_arguments(
        &target.class_name,
        target.is_static,
        &target.descriptor,
        &callback.descriptor,
        "INVOKE",
        if cancellable {
            Injected::Cancellable
        } else {
            Injected::None
        },
    )?;
    let code = target.code()?;
    let scan = scan_code(&label, code.code)?;
    let sites = invoke_sites(&label, &target, code, &scan, call_site)?;
    if sites.is_empty() {
        return Err(format!(
            "{label}: INVOKE weaving found no {}{} call site; the selector matches invokevirtual, invokestatic and invokeinterface instructions by owner, name and descriptor",
            call_site.owner, call_site.name
        ));
    }
    if cancellable {
        let [site] = sites.as_slice() else {
            return Err(format!(
                "{label}: cancellable INVOKE weaving needs exactly one matching call site, but the selector matches {}",
                sites.len()
            ));
        };
        return weave_cancellable_invoke(&target, callback, takes_receiver, site, code, &scan);
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
        .and_then(|value| value.checked_add(1))
        .and_then(|value| u16::try_from(value).ok())
        .ok_or_else(|| format!("{label}: woven max_stack overflows u16"))?;
    let max_locals = code.max_locals.max(call.local_slots);
    let insertions = sites
        .iter()
        .map(|site| (site.at, call.bytes.len()))
        .collect::<Vec<_>>();
    let patch = relocate(
        &label,
        &target,
        code,
        &scan,
        &insertions,
        &call.bytes,
        max_stack,
        max_locals,
        TargetEntry::Enter,
        FrameSites::Both,
        &[],
        &mut pool,
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

fn weave_cancellable_invoke(
    target: &Target<'_>,
    callback: &MethodRef,
    takes_receiver: bool,
    site: &InvokeSite,
    code: &CodeLayout<'_>,
    scan: &CodeScan,
) -> Result<Vec<u8>, String> {
    let label = target.label();
    if crate::return_desc(&site.descriptor) != "V" {
        return Err(format!(
            "{label}: cancellable INVOKE weaving is bounded to void call sites; the selected call site's descriptor {} answers a value, which would also need the callee descriptor's default pushed on cancellation",
            site.descriptor
        ));
    }
    if site.at + site.size >= code.code.len() {
        return Err(format!(
            "{label}: the call site at {} has no instruction after it for the cancellation branch to reach",
            site.at
        ));
    }
    let slot = code.max_locals.max(argument_slots(target)?);
    let mut pool = PoolBuilder::new(target.pool);
    let mut call =
        cancellable_invoke_call(target, callback, takes_receiver, site, slot, &mut pool)?;
    if code.code.len() + call.bytes.len() > MAX_CODE_LENGTH {
        return Err(format!(
            "{label}: woven code would exceed the class-file limit of {MAX_CODE_LENGTH} bytes"
        ));
    }

    // The insert's length is fixed before its branch operands are known, so the
    // stable layout can be computed first and the operands patched into the
    // bytes the relocation will place at that same offset.
    let insertions = [(site.at, call.bytes.len())];
    let insertions =
        validate_insertions(&label, scan, code.code.len(), &insertions, call.bytes.len())?;
    let plan = stable_plan(&label, scan, &insertions, TargetEntry::Enter)?;
    let call_at = plan.offsets.get(&site.at).copied().ok_or_else(|| {
        format!(
            "{label}: the call site at {} is not an instruction boundary",
            site.at
        )
    })?;
    let insert_start = call_at
        .checked_sub(call.bytes.len())
        .ok_or_else(|| format!("{label}: the insert at {} does not fit the layout", site.at))?;
    let after = map_offset(&label, &plan, site.at + site.size, false)?;
    let ifeq_delta = i16::try_from(call_at as i64 - (insert_start + call.ifeq) as i64)
        .map_err(|_| format!("{label}: the cancellation branch cannot reach the call"))?;
    let goto_delta = i16::try_from(after as i64 - (insert_start + call.goto) as i64)
        .map_err(|_| format!("{label}: the cancellation branch cannot reach the continuation"))?;
    call.bytes[call.ifeq_operand..call.ifeq_operand + 2].copy_from_slice(&ifeq_delta.to_be_bytes());
    call.bytes[call.goto_operand..call.goto_operand + 2].copy_from_slice(&goto_delta.to_be_bytes());

    let before = derive_state_at(&label, target, code, scan, site.at)?;
    let after_state = derive_state_at(&label, target, code, scan, site.at + site.size)?;
    let extras = [
        SyntheticFrame {
            offset: site.at,
            locals: before.locals,
            stack: before.stack,
            at_instruction: true,
        },
        SyntheticFrame {
            offset: site.at + site.size,
            locals: after_state.locals,
            stack: after_state.stack,
            at_instruction: false,
        },
    ];
    let max_stack = u32::from(code.max_stack)
        .checked_add(u32::from(call.stack_slots))
        .and_then(|value| value.checked_add(1))
        .and_then(|value| u16::try_from(value).ok())
        .ok_or_else(|| format!("{label}: woven max_stack overflows u16"))?;
    let max_locals = code.max_locals.max(call.local_slots);
    let patch = relocate(
        &label,
        target,
        code,
        scan,
        &[(site.at, call.bytes.len())],
        &call.bytes,
        max_stack,
        max_locals,
        TargetEntry::Enter,
        FrameSites::Both,
        &extras,
        &mut pool,
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

/// Apply one `@At("INVOKE")` plan: [`weave_invoke`] with the plan's callback,
/// target and selected call site.
pub fn weave_invoke_plan(class: &ClassFile, plan: &MixinInjectionPlan) -> Result<Vec<u8>, String> {
    if plan.at != "INVOKE" {
        return Err(format!(
            "{}.{}: plan anchor is {}, not INVOKE",
            plan.target.owner, plan.target.name, plan.at
        ));
    }
    let site = plan.invoke_target.as_ref().ok_or_else(|| {
        format!(
            "{}.{}: the @At(INVOKE) plan has no resolved call site",
            plan.target.owner, plan.target.name
        )
    })?;
    weave_invoke(class, &plan.target, &plan.callback, site, plan.cancellable)
}

/// Apply every plan that targets `class`: `HEAD` plans first, then
/// `RETURN`/`TAIL` plans, each group in declaration order.
///
/// Plans for other classes are skipped.  A matching plan that asks for an
/// anchor or object this file cannot produce is an error rather than a silent
/// skip: `INVOKE` call-site rewriting (use [`weave_invoke_plan`] for that
/// anchor), cancelling a `void` target, an unknown anchor, and more than
/// `MAX_WEAVE_PLANS_PER_CLASS` plans for one class.
///
/// A `RETURN`/`TAIL` plan on a non-void target references the synthetic
/// `CallbackInfoReturnable`; [`weave_class_with_extras`] reports that class
/// alongside the rewritten bytes so a jar writer can add it.
pub fn weave_class(class: &ClassFile, plans: &[MixinInjectionPlan]) -> Result<Vec<u8>, String> {
    weave_class_with_extras(class, plans).map(|(bytes, _)| bytes)
}

/// [`weave_class`], plus the synthetic classes the rewrite references.
///
/// `added` is keyed by internal class name and holds
/// [`callback_info_returnable_class`] when a non-void `RETURN`/`TAIL` insert
/// landed and [`callback_info_class`] when a cancellable `void` `HEAD` insert
/// landed; it is empty otherwise.  The weaver cannot tell whether the jar
/// already declares a class, so [`weave_classes_with_extras`] drops the
/// duplicates.
pub fn weave_class_with_extras(
    class: &ClassFile,
    plans: &[MixinInjectionPlan],
) -> Result<(Vec<u8>, BTreeMap<String, Vec<u8>>), String> {
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
        if !matches!(plan.at.as_str(), "HEAD" | "RETURN" | "TAIL") {
            return Err(format!(
                "{}.{}: anchor {} needs the interpreter path; the bounded weaver applies HEAD, RETURN and TAIL",
                plan.target.owner, plan.target.name, plan.at
            ));
        }
        // A non-void `RETURN`/`TAIL` builds the `CallbackInfoReturnable` the
        // callback cancels with; a `void` `HEAD` builds the `CallbackInfo`.
        // Every other cancellable shape still needs a runtime-owned object the
        // bounded weaver does not construct.
        if plan.cancellable && plan.at != "HEAD" && !plan_returns_value(class, plan) {
            return Err(format!(
                "{}.{}: cancellable {} weaving needs a runtime-owned CallbackInfo, which the bounded weaver cannot construct",
                plan.target.owner, plan.target.name, plan.at
            ));
        }
        if plan.cancellable && plan.at == "HEAD" && plan_returns_value_for_head(class, plan) {
            return Err(format!(
                "{}.{}: cancellable HEAD weaving of a non-void target needs the descriptor's default value in the target's shape; only void targets are bounded",
                plan.target.owner, plan.target.name
            ));
        }
    }

    let mut current = class.write()?;
    let mut needs_returnable = false;
    let mut needs_callback_info = false;
    for plan in matching.iter().filter(|plan| plan.at == "HEAD") {
        let next = ClassFile::parse(&current)
            .map_err(|error| format!("rewritten class did not parse again: {error}"))?;
        current = if plan.cancellable {
            needs_callback_info = true;
            weave_head_cancellable(&next, &plan.target, &plan.callback)?
        } else {
            weave_head(&next, &plan.target, &plan.callback)?
        };
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
        needs_returnable |= plan_returns_value(&next, plan);
        current = weave_return(&next, &plan.target, &plan.callback, mode)?;
    }
    let mut added = BTreeMap::new();
    if needs_returnable {
        added.insert(
            CALLBACK_INFO_RETURNABLE_CLASS.to_owned(),
            callback_info_returnable_class()?,
        );
    }
    if needs_callback_info {
        added.insert(CALLBACK_INFO_CLASS.to_owned(), callback_info_class()?);
    }
    Ok((current, added))
}

/// Whether a `HEAD` plan targets a method whose descriptor returns a value.
fn plan_returns_value_for_head(class: &ClassFile, plan: &MixinInjectionPlan) -> bool {
    class
        .method(&plan.target.name, &plan.target.descriptor)
        .is_some_and(|method| crate::return_desc(&method.descriptor) != "V")
}

/// Whether one `RETURN`/`TAIL` plan targets a method with a value to carry.
///
/// A descriptor-less plan never resolves here; the weaver reports that
/// ambiguity itself, so the answer is `false` and the plan keeps the `void`
/// shape for the cancellable check.
fn plan_returns_value(class: &ClassFile, plan: &MixinInjectionPlan) -> bool {
    matches!(plan.at.as_str(), "RETURN" | "TAIL")
        && class
            .method(&plan.target.name, &plan.target.descriptor)
            .is_some_and(|method| crate::return_desc(&method.descriptor) != "V")
}

/// Apply the `HEAD` plans that target `class`, in plan order.
///
/// Only plans whose [`MixinInjectionPlan::at`] is `"HEAD"`, whose target owner
/// is this class and whose target method exists are applied; plans for other
/// classes and every other anchor are left to the interpreter.  Cancellable
/// plans are skipped: this entry point returns only bytes, so it cannot report
/// the synthetic `CallbackInfo` they reference - [`weave_class_with_extras`]
/// can.
///
/// [`weave_class`] is the whole-class entry point: it also applies `RETURN`
/// and `TAIL` plans, and rejects anchors and cancellable plans it cannot weave
/// instead of skipping them.
///
/// Not supported here, and left to the interpreter: `RETURN`, `TAIL` and
/// `INVOKE` anchors (the last is [`weave_invoke_plan`]); cancellation; and
/// `CallbackInfo`/`CallbackInfoReturnable` parameters.  Errors from a matching
/// plan (ambiguous target descriptor, callback mismatch, a malformed condy
/// pool, a `wide` prefix that is not a local-variable instruction, a
/// conditional branch the split cannot express) propagate rather than being
/// silently skipped.
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

/// A woven class set: the rewritten input classes plus the synthetic classes
/// their bytecode references.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WovenClasses {
    /// Every input class, rewritten or re-encoded unchanged, keyed like the
    /// input set.
    pub classes: BTreeMap<String, Vec<u8>>,
    /// Synthetic classes to add beside them, keyed by internal class name.
    /// Empty unless a non-void `RETURN`/`TAIL` insert landed and the input set
    /// does not already declare the class.
    pub added: BTreeMap<String, Vec<u8>>,
}

/// Weave a set of parsed classes, grouped by the class each plan targets.
///
/// Classes with matching plans go through [`weave_class`]; every other class is
/// re-encoded unchanged by [`ClassFile::write`], so the result is a complete
/// replacement set keyed like `classes`.  A plan that targets a class absent
/// from the set is an error rather than a silent skip, and the per-class plan
/// bound is `MAX_WEAVE_PLANS_PER_CLASS`.
///
/// The result drops the class set, so a caller that also writes a jar wants
/// [`weave_classes_with_extras`].
pub fn weave_classes(
    classes: &BTreeMap<String, ClassFile>,
    plans: &[MixinInjectionPlan],
) -> Result<BTreeMap<String, Vec<u8>>, String> {
    weave_classes_with_extras(classes, plans).map(|woven| woven.classes)
}

/// [`weave_classes`], plus the synthetic classes the rewrites reference.
///
/// [`WovenClasses::added`] holds `CallbackInfoReturnable` when a non-void
/// `RETURN`/`TAIL` insert landed, unless the input already declares it - a jar
/// that ships its own copy keeps it.
pub fn weave_classes_with_extras(
    classes: &BTreeMap<String, ClassFile>,
    plans: &[MixinInjectionPlan],
) -> Result<WovenClasses, String> {
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

    let mut woven = WovenClasses::default();
    for (key, class) in classes {
        match by_owner.get(class.name.as_str()) {
            Some(class_plans) => {
                let owned = class_plans
                    .iter()
                    .map(|plan| (*plan).clone())
                    .collect::<Vec<_>>();
                let (bytes, added) = weave_class_with_extras(class, &owned)?;
                for (name, bytes) in added {
                    woven.added.entry(name).or_insert(bytes);
                }
                woven.classes.insert(key.clone(), bytes);
            }
            None => {
                woven.classes.insert(key.clone(), class.write()?);
            }
        }
    }
    let declared = classes
        .values()
        .map(|class| class.name.clone())
        .collect::<BTreeSet<_>>();
    woven.added.retain(|name, _| !declared.contains(name));
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
    fn head_expands_a_conditional_branch_past_the_16_bit_range() {
        // `iconst_0; ifeq +32766; return; 32762 nops; return`: the branch sits
        // at 1 and names 32767.  Two RETURN inserts (at 4 and at 32767) push the
        // target to 32770, past the 16-bit range, so the `ifeq` is split into
        // `ifne +8; goto_w target` and the fallthrough - offset 4, now a branch
        // target - gets a synthesised frame.
        let mut code = vec![0x03, 0x99, 0x7f, 0xfe, 0xb1];
        code.resize(32_767, 0x00);
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
        assert_eq!(woven_code[0], 0x03, "iconst_0 stays first");
        assert_eq!(woven_code[1], 0x9a, "ifeq became ifne");
        assert_eq!(
            i16::from_be_bytes([woven_code[2], woven_code[3]]),
            8,
            "the inverse branch skips the wide jump"
        );
        assert_eq!(woven_code[4], 0xc8, "the far target uses goto_w");
        assert_eq!(
            i32::from_be_bytes([woven_code[5], woven_code[6], woven_code[7], woven_code[8]]),
            32_771,
            "goto_w names the relocated target"
        );
        // The fallthrough return moved to 9 and starts with its callback.
        assert_eq!(woven_code[9], 0xb8);

        // The method had no StackMapTable, so the split adds one with a single
        // full frame at the fallthrough.
        let layout = Layout::parse(&woven).unwrap();
        let method = &layout.methods[0];
        let code = method.attributes[0].code.as_ref().unwrap();
        let frames = code
            .attributes
            .iter()
            .find_map(|attribute| match attribute {
                CodeAttribute::StackMap { frames, .. } => Some(frames),
                _ => None,
            })
            .expect("a synthesised StackMapTable");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].offset, 9);
        assert_eq!(frames[0].frame_type, 255);
        assert!(frames[0].locals.is_empty());
        assert!(frames[0].stack.is_empty());
    }

    /// Cancellable `RETURN`/`TAIL` on a non-void target.
    ///
    /// Mixin's cancellable handler returns `getReturnValue()` whether or not the
    /// callback cancelled, so `cancel()` alone keeps the target's own result and
    /// a `setReturnValue` still wins - exactly what the branch-free insert does.
    /// Only a `void` target still needs the interpreter's runtime-owned
    /// `CallbackInfo`.
    #[test]
    fn a_cancellable_plan_weaves_a_non_void_target() {
        let bytes = fixture(0x0009, "(I)I", 1, &[0x1a, 0xac], &[]);
        let class = ClassFile::parse(&bytes).unwrap();
        let target = MethodRef {
            owner: "demo/T".into(),
            name: "m".into(),
            descriptor: "(I)I".into(),
        };
        let mut plan = MixinInjectionPlan::inject_at_return(
            target.clone(),
            callback(&format!("(I{CALLBACK_INFO_RETURNABLE_DESC})V")),
        );
        plan.cancellable = true;
        let woven = weave_class(&class, &[plan]).unwrap();
        let (code, _, _) = woven_code(&woven, "m", "(I)I");
        assert_eq!(
            code.iter().filter(|opcode| **opcode == 0xbb).count(),
            1,
            "one CallbackInfoReturnable for the single return"
        );
        assert_eq!(code.last(), Some(&0xac), "the plain return stays last");
        assert_eq!(
            code.iter().filter(|opcode| **opcode == 0xb6).count(),
            2,
            "getReturnValue plus the unboxing call"
        );
    }

    /// The synthetic class is a normal class file the crate's own parser reads,
    /// so a jar that carries it loads on the native path too.
    #[test]
    fn the_synthetic_returnable_parses_and_round_trips() {
        let bytes = callback_info_returnable_class().unwrap();
        let class = ClassFile::parse(&bytes).unwrap();
        assert_eq!(class.name, CALLBACK_INFO_RETURNABLE_CLASS);
        assert_eq!(class.write().unwrap(), bytes);
        assert_eq!(class.methods.len(), 6);
        assert_eq!(class.fields.len(), 2);
        assert!(
            class
                .method("getReturnValue", "()Ljava/lang/Object;")
                .is_some()
        );
        assert!(
            class
                .method("setReturnValue", "(Ljava/lang/Object;)V")
                .is_some()
        );
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

    /// A minimal class with one `CONSTANT_Dynamic`: pool index 19 is the condy,
    /// its name/type (index 10) is `condyName`/`I`, bootstrap row 0 is a
    /// `ConstantBootstraps`-shaped handle (index 18) with one `String` argument
    /// (index 20, whose `Utf8` is index 11).
    fn condy_fixture() -> Vec<u8> {
        let mut out = Vec::new();
        u4(&mut out, 0xCAFE_BABE);
        u2(&mut out, 0);
        u2(&mut out, 52);
        u2(&mut out, 22);
        utf8(&mut out, "demo/C"); // 1
        out.extend_from_slice(&[7, 0, 1]); // 2
        utf8(&mut out, "java/lang/Object"); // 3
        out.extend_from_slice(&[7, 0, 3]); // 4
        utf8(&mut out, "m"); // 5
        utf8(&mut out, "()V"); // 6
        utf8(&mut out, "Code"); // 7
        utf8(&mut out, "condyName"); // 8
        utf8(&mut out, "I"); // 9
        out.extend_from_slice(&[12, 0, 8, 0, 9]); // 10 NameAndType
        utf8(&mut out, "extra"); // 11
        utf8(&mut out, "java/lang/invoke/ConstantBootstraps"); // 12
        utf8(&mut out, "bootstrap"); // 13
        utf8(&mut out, "()Ljava/lang/Object;"); // 14
        out.extend_from_slice(&[12, 0, 13, 0, 14]); // 15 NameAndType
        out.extend_from_slice(&[7, 0, 12]); // 16 Class
        out.extend_from_slice(&[10, 0, 16, 0, 15]); // 17 Methodref
        out.extend_from_slice(&[15, 6, 0, 17]); // 18 MethodHandle
        out.extend_from_slice(&[17, 0, 0, 0, 10]); // 19 Dynamic
        out.extend_from_slice(&[8, 0, 11]); // 20 String
        utf8(&mut out, "BootstrapMethods"); // 21

        u2(&mut out, 0x0021); // access
        u2(&mut out, 2); // this
        u2(&mut out, 4); // super
        u2(&mut out, 0); // interfaces
        u2(&mut out, 0); // fields
        u2(&mut out, 1); // methods
        u2(&mut out, 0x0001);
        u2(&mut out, 5);
        u2(&mut out, 6);
        u2(&mut out, 1); // attributes
        u2(&mut out, 7); // Code
        let mut body = Vec::new();
        u2(&mut body, 0); // max_stack
        u2(&mut body, 0); // max_locals
        u4(&mut body, 1); // code_length
        body.push(0xb1); // return
        u2(&mut body, 0); // exception table
        u2(&mut body, 0); // Code attributes
        u4(&mut out, body.len() as u32);
        out.extend_from_slice(&body);

        u2(&mut out, 1); // class attributes
        u2(&mut out, 21);
        let mut bootstrap = Vec::new();
        u2(&mut bootstrap, 1); // rows
        u2(&mut bootstrap, 18); // method handle
        u2(&mut bootstrap, 1); // static arguments
        u2(&mut bootstrap, 20);
        u4(&mut out, bootstrap.len() as u32);
        out.extend_from_slice(&bootstrap);
        out
    }

    #[test]
    fn condy_pools_are_accepted_and_round_trip() {
        let bytes = condy_fixture();
        let class = ClassFile::parse(&bytes).unwrap();
        assert_eq!(class.write().unwrap(), bytes, "identity round-trip");
        let layout = Layout::parse(&bytes).unwrap();
        layout
            .validate_constant_pool()
            .expect("a well-formed condy is inside the writer's subset");
    }

    /// A relocated pool rewrites `CONSTANT_Dynamic`'s name/type and the
    /// `BootstrapMethods` static argument, so a future compaction cannot leave
    /// the condy pointing at the wrong constant.
    #[test]
    fn pool_relocation_rewrites_dynamic_and_bootstrap_references() {
        let bytes = condy_fixture();
        let layout = Layout::parse(&bytes).unwrap();
        let identity = PoolMap::identity(layout.pool_count);

        let mut pool = Vec::new();
        layout.write_pool(&mut pool, &identity).unwrap();
        assert_eq!(
            &bytes[10..10 + pool.len()],
            pool.as_slice(),
            "the identity map reproduces the pool bytes"
        );
        let mut attributes = Vec::new();
        layout
            .write_class_attributes(&mut attributes, &identity)
            .unwrap();
        assert_eq!(
            &bytes[bytes.len() - attributes.len()..],
            attributes.as_slice(),
            "the identity map reproduces the BootstrapMethods bytes"
        );

        let mut map = PoolMap::identity(layout.pool_count);
        map.0.swap(8, 11);
        let mut relocated = Vec::new();
        layout.write_pool(&mut relocated, &map).unwrap();
        let mut r = Bytes::new(&relocated);
        let (utf8, entries) = parse_pool(&mut r, layout.pool_count).unwrap();
        assert_eq!(utf8[8], Some("extra"));
        assert_eq!(utf8[11], Some("condyName"));
        let entry = |index: u16| {
            entries
                .iter()
                .find(|entry| entry.index == index)
                .expect("relocated entry")
        };
        assert_eq!(entry(10).tag, 12);
        assert_eq!(payload_u2(entry(10).payload, 0).unwrap(), 11);
        assert_eq!(payload_u2(entry(10).payload, 2).unwrap(), 9);
        assert_eq!(entry(19).tag, 17);
        assert_eq!(payload_u2(entry(19).payload, 0).unwrap(), 0);
        assert_eq!(payload_u2(entry(19).payload, 2).unwrap(), 10);
        assert_eq!(entry(20).tag, 8);
        assert_eq!(payload_u2(entry(20).payload, 0).unwrap(), 8);

        let mut relocated_attributes = Vec::new();
        layout
            .write_class_attributes(&mut relocated_attributes, &map)
            .unwrap();
        assert_eq!(
            relocated_attributes, attributes,
            "the bootstrap row keeps its handle and argument indices"
        );
    }
}
