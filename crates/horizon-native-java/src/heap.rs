//! The objects a jar's Java code built, kept with the jar's host between calls.
//!
//! The interpreter names every object by a marker (`demo/Listener#3`, an
//! `array/...` or `iterator/...` marker) and keeps its instance fields, array
//! elements, `StringBuilder` text and enum identity in per-invocation tables
//! on [`Runtime`]. Before this module those tables died with the invocation:
//! a later callback (a game hook, an event listener, a payload handler) saw
//! the jar's static fields but none of the objects they pointed at, so a stop
//! listener walking the listener list its entrypoint filled walked a list of
//! fieldless markers.
//!
//! A [`JavaHeap`] is those tables plus the object and array counters, moved
//! into the host when an invocation ends ([`Runtime::persist_resident_heap`])
//! and back out when the next one starts ([`Runtime::adopt_resident_heap`]).
//! [`StdHost`] is the resident host; a host that keeps no jar instance answers
//! `None` and every invocation starts empty, as before.
//!
//! What stays is what a later call can still reach. When a heap is stored the
//! host collects it ([`collect`]) once it has grown well past what the last
//! collection kept: everything reachable from the jar's static fields and from
//! the values the host itself holds (its call log, registrations, payload
//! handlers, posted events, holders...) is kept, the rest is dropped. A tick
//! listener that allocates every tick therefore does not grow the heap for the
//! life of the server. The collection is deterministic: it depends only on the
//! tables' contents, never on hashing order or time.
//!
//! Markers are text, and composite values embed other markers in their own
//! text (a lambda's `@indy:...` marker carries its captures, a `map-entry/k,v`
//! both halves, a map view its map). Rather than parse every composite format,
//! the collector looks for any collectable marker that ends where a run of
//! digits ends inside a value's text: every collectable marker ends in its
//! allocation number, so this finds an embedded one wherever it sits. A string
//! that merely spells a live marker keeps that object alive; that is the only
//! imprecision, and it errs on the side of keeping.
//!
//! The heap is bounded after collection ([`MAX_RESIDENT_FIELDS`],
//! [`MAX_RESIDENT_ARRAYS`], [`MAX_RESIDENT_BYTES`]); a store over a bound is
//! refused with an error that names it, so the callback that grew it fails
//! instead of the server running out of memory.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::{Host, NativeError, Runtime, StdHost, Value, game_hooks::GAME_OBJECT_PREFIX};

/// Instance fields one jar's resident heap may keep after collection.
pub(crate) const MAX_RESIDENT_FIELDS: usize = 65_536;
/// Arrays one jar's resident heap may keep after collection.
pub(crate) const MAX_RESIDENT_ARRAYS: usize = 8_192;
/// Estimated bytes (marker and value text, eight per primitive) the kept
/// fields, arrays, builders and host collections may hold after collection.
pub(crate) const MAX_RESIDENT_BYTES: usize = 32 << 20;
/// Entries a heap may grow past twice what the last collection kept before
/// the next store collects it. Small heaps collect rarely; a large one at most
/// every time it doubles, so a store costs amortised O(1) per entry.
const COLLECT_SLACK: usize = 1024;

/// The instance state a jar's invocations built, kept between them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JavaHeap {
    /// `receiver|owner#name descriptor` → value, the interpreter's own keys.
    pub(crate) fields: BTreeMap<String, Value>,
    pub(crate) arrays: BTreeMap<String, Vec<Value>>,
    pub(crate) string_builders: BTreeMap<String, String>,
    /// Enum constants by object marker: declared name and ordinal. A later
    /// call never reruns `<clinit>`, so without these `name()` on a constant a
    /// static holds would lose its answer.
    pub(crate) enum_constants: BTreeMap<String, (String, i32)>,
    /// Next object and array numbers, so a later call never reuses a marker
    /// an earlier one handed out (a reused `java/util/ArrayList#0` would
    /// replace the entrypoint's list in the host's collection table).
    pub(crate) next_object: usize,
    pub(crate) next_array: usize,
    /// Entries the last collection kept, the baseline of the next trigger.
    live: usize,
    /// Texts the storing caller holds outside the host (the game-hook tier's
    /// fields on game objects), roots of this store's collection only.
    roots: Vec<String>,
}

impl JavaHeap {
    /// Instance fields kept, across every object.
    pub fn instance_fields(&self) -> usize {
        self.fields.len()
    }

    /// Arrays kept.
    pub fn arrays(&self) -> usize {
        self.arrays.len()
    }

    /// The instance field `name` (+ `descriptor`) of `owner` on `object`, as
    /// the last stored invocation left it.
    pub fn instance_field(
        &self,
        object: &str,
        owner: &str,
        name_descriptor: &str,
    ) -> Option<&Value> {
        self.fields
            .get(&format!("{object}|{owner}#{name_descriptor}"))
    }
}

/// A position in a host's call log ([`StdHost::invocations`] and its method
/// list, posted events), to roll back what one callback logged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallLogMark {
    calls: usize,
    invocations: usize,
    posted_events: usize,
}

impl StdHost {
    /// The objects this host's jar keeps between invocations.
    pub fn resident_heap(&self) -> &JavaHeap {
        &self.heap
    }

    /// Where the call log ends now; see [`Self::rewind_call_log`].
    pub fn call_log_mark(&self) -> CallLogMark {
        CallLogMark {
            calls: self.calls.len(),
            invocations: self.invocations.len(),
            posted_events: self.posted_events.len(),
        }
    }

    /// Drop what was logged after `mark`. A host kept for a server's life runs
    /// callbacks every tick; their calls are not registrations, and keeping
    /// them would grow the log (and the heap's roots) without bound.
    pub fn rewind_call_log(&mut self, mark: CallLogMark) {
        self.calls.truncate(mark.calls);
        self.invocations.truncate(mark.invocations);
        self.posted_events.truncate(mark.posted_events);
    }

    pub(crate) fn take_resident_heap(&mut self) -> JavaHeap {
        let live = self.heap.live;
        let heap = std::mem::take(&mut self.heap);
        self.heap.live = live;
        heap
    }

    /// Keep `heap` as this host's resident heap, collecting it first when it
    /// grew past the trigger or a bound. The heap is kept even when it is
    /// refused, so a jar's other callbacks still see their objects; the error
    /// fails the callback that grew it.
    pub(crate) fn store_resident_heap(&mut self, mut heap: JavaHeap) -> Result<(), NativeError> {
        heap.live = self.heap.live;
        let roots = std::mem::take(&mut heap.roots);
        let entries = heap_entries(&heap, self);
        if entries > heap.live.saturating_mul(2).saturating_add(COLLECT_SLACK)
            || over_count_bound(&heap)
        {
            collect(&mut heap, self, &roots);
            heap.live = heap_entries(&heap, self);
            let bytes = heap_bytes(&heap, self);
            self.heap = heap;
            if bytes > MAX_RESIDENT_BYTES {
                return Err(NativeError::Metadata(format!(
                    "the jar's Java objects hold about {bytes} bytes after collection, over the resident bound of {MAX_RESIDENT_BYTES}"
                )));
            }
        } else {
            self.heap = heap;
        }
        if over_count_bound(&self.heap) {
            return Err(NativeError::Metadata(format!(
                "the jar's Java objects keep {} fields and {} arrays after collection, over the resident bound of {MAX_RESIDENT_FIELDS} fields or {MAX_RESIDENT_ARRAYS} arrays",
                self.heap.fields.len(),
                self.heap.arrays.len()
            )));
        }
        Ok(())
    }

    /// Every text the host holds that may name a jar object: the roots of a
    /// collection, besides the static fields. Conservative by design: a table
    /// that is not listed here would let the collector drop an object the host
    /// still names, so every host table that stores a `Value` or a marker key
    /// is listed, including bootstrap-only ones.
    fn root_texts<'a>(&'a self, out: &mut Vec<&'a str>) {
        fn value<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
            if let Value::Ref(Some(text)) = value {
                out.push(text);
            }
        }
        for field in self.persistent_static_fields.values() {
            value(field, out);
        }
        for invocation in &self.invocations {
            if let Some(receiver) = &invocation.receiver {
                value(receiver, out);
            }
            invocation.args.iter().for_each(|arg| value(arg, out));
        }
        for entry in &self.registrations {
            value(&entry.value, out);
        }
        self.posted_events
            .iter()
            .for_each(|event| value(event, out));
        for registration in &self.payload_registrations {
            value(&registration.payload_type, out);
            value(&registration.codec, out);
            value(&registration.handler, out);
        }
        for (key, held) in &self.holders {
            out.push(key);
            value(held, out);
        }
        for (key, held) in &self.capability_values {
            out.push(key);
            value(held, out);
        }
        for (key, held) in &self.lazy_optionals {
            out.push(key);
            if let Some(held) = held {
                value(held, out);
            }
        }
        for (key, slots) in &self.item_handlers {
            out.push(key);
            slots.iter().for_each(|slot| value(slot, out));
        }
        for (key, cause) in &self.throwable_causes {
            out.push(key);
            if let Some(cause) = cause {
                value(cause, out);
            }
        }
        out.extend(self.throwable_messages.keys().map(String::as_str));
        out.extend(self.payload_bytes.keys().map(String::as_str));
        out.extend(self.packet_types.keys().map(String::as_str));
        out.extend(self.item_property_builders.keys().map(String::as_str));
        out.extend(self.block_property_builders.keys().map(String::as_str));
        out.extend(self.resource_streams.keys().map(String::as_str));
        // The command tree's callbacks and requirement lambdas (`brigadier.rs`).
        self.brigadier_root_texts(out);
    }
}

/// Whether a marker names an object the interpreter or host allocated, which
/// may be dropped once nothing reaches it. Anything else that holds state (an
/// entrypoint instance named by its class, a game object of the hook tier, a
/// host marker of another shape) is kept for the host's life, and its contents
/// are roots.
fn collectable(marker: &str) -> bool {
    if marker.starts_with(GAME_OBJECT_PREFIX) {
        return false;
    }
    if let Some(rest) = marker
        .strip_prefix("array/")
        .or_else(|| marker.strip_prefix("iterator/"))
        .or_else(|| marker.strip_prefix("resource-enumeration/"))
    {
        return rest.ends_with(|c: char| c.is_ascii_digit());
    }
    // `new` names an object `<class>#<n>` (`Runtime::new_object`).
    marker.rsplit_once('#').is_some_and(|(class, number)| {
        !class.is_empty() && !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit())
    })
}

/// The receiver of an instance-field key (`receiver|owner#name desc`).
fn field_receiver(key: &str) -> &str {
    key.rsplit_once('|').map_or(key, |(receiver, _)| receiver)
}

fn heap_entries(heap: &JavaHeap, host: &StdHost) -> usize {
    heap.fields.len()
        + heap.arrays.len()
        + heap.string_builders.len()
        + heap.enum_constants.len()
        + host.mutable_collections.len()
        + host.iterators.len()
}

fn over_count_bound(heap: &JavaHeap) -> bool {
    heap.fields.len() > MAX_RESIDENT_FIELDS || heap.arrays.len() > MAX_RESIDENT_ARRAYS
}

fn value_bytes(value: &Value) -> usize {
    match value {
        Value::Ref(Some(text)) => text.len(),
        _ => 8,
    }
}

fn heap_bytes(heap: &JavaHeap, host: &StdHost) -> usize {
    let values = |values: &[Value]| values.iter().map(value_bytes).sum::<usize>();
    heap.fields
        .iter()
        .map(|(key, value)| key.len() + value_bytes(value))
        .chain(
            heap.arrays
                .iter()
                .map(|(key, elements)| key.len() + values(elements)),
        )
        .chain(
            heap.string_builders
                .iter()
                .map(|(key, text)| key.len() + text.len()),
        )
        .chain(
            host.mutable_collections
                .iter()
                .map(|(key, elements)| key.len() + values(elements)),
        )
        .chain(
            host.iterators
                .iter()
                .map(|(key, (elements, _))| key.len() + values(elements)),
        )
        .sum()
}

/// The collectable markers of a heap and the distinct lengths they come in,
/// which is what [`scan`] tries at each digit-run end.
struct Collectable {
    markers: HashSet<String>,
    lengths: Vec<usize>,
}

impl Collectable {
    fn new(heap: &JavaHeap, host: &StdHost) -> Self {
        let markers: HashSet<String> = heap
            .fields
            .keys()
            .map(|key| field_receiver(key))
            .chain(heap.arrays.keys().map(String::as_str))
            .chain(heap.string_builders.keys().map(String::as_str))
            .chain(heap.enum_constants.keys().map(String::as_str))
            .chain(host.mutable_collections.keys().map(String::as_str))
            .chain(host.iterators.keys().map(String::as_str))
            .filter(|marker| collectable(marker))
            .map(str::to_owned)
            .collect();
        let lengths: BTreeSet<usize> = markers.iter().map(String::len).collect();
        Self {
            markers,
            lengths: lengths.into_iter().collect(),
        }
    }

    /// Every collectable marker `text` names, itself or embedded: each one
    /// ends where a run of digits ends.
    fn scan(&self, text: &str, out: &mut Vec<String>) {
        let bytes = text.as_bytes();
        for end in 1..=bytes.len() {
            if !bytes[end - 1].is_ascii_digit() || bytes.get(end).is_some_and(u8::is_ascii_digit) {
                continue;
            }
            for &length in &self.lengths {
                let Some(start) = end.checked_sub(length) else {
                    break;
                };
                if let Some(candidate) = text.get(start..end)
                    && self.markers.contains(candidate)
                {
                    out.push(candidate.to_owned());
                }
            }
        }
    }
}

/// Drop every collectable object of `heap` (and every collectable collection
/// or iterator of `host`) that nothing reaches from the roots.
///
/// Roots: the static fields and host tables ([`StdHost::root_texts`]), the
/// caller's `extra` texts, and the contents of every object that is not
/// collectable. Edges: an object's instance fields, array elements,
/// collection elements and iterator snapshot.
pub(crate) fn collect(heap: &mut JavaHeap, host: &mut StdHost, extra: &[String]) {
    let collectable = Collectable::new(heap, host);
    if collectable.markers.is_empty() {
        return;
    }
    let mut pending = Vec::new();
    {
        let mut roots: Vec<&str> = extra.iter().map(String::as_str).collect();
        host.root_texts(&mut roots);
        let kept_values = heap
            .fields
            .iter()
            .filter(|(key, _)| !collectable.markers.contains(field_receiver(key)))
            .map(|(_, value)| value)
            .chain(
                host.mutable_collections
                    .iter()
                    .filter(|(key, _)| !collectable.markers.contains(key.as_str()))
                    .flat_map(|(_, elements)| elements),
            )
            .chain(
                host.iterators
                    .iter()
                    .filter(|(key, _)| !collectable.markers.contains(key.as_str()))
                    .flat_map(|(_, (elements, _))| elements),
            );
        for value in kept_values {
            if let Value::Ref(Some(text)) = value {
                roots.push(text);
            }
        }
        for text in roots {
            collectable.scan(text, &mut pending);
        }
    }
    let mut reached = BTreeSet::new();
    let mut found = Vec::new();
    while let Some(marker) = pending.pop() {
        if !reached.insert(marker.clone()) {
            continue;
        }
        let prefix = format!("{marker}|");
        let fields = heap
            .fields
            .range(prefix.clone()..)
            .take_while(|(key, _)| key.starts_with(&prefix))
            .map(|(_, value)| value);
        let arrays = heap.arrays.get(&marker).into_iter().flatten();
        let collections = host.mutable_collections.get(&marker).into_iter().flatten();
        let iterators = host
            .iterators
            .get(&marker)
            .into_iter()
            .flat_map(|(elements, _)| elements);
        for value in fields.chain(arrays).chain(collections).chain(iterators) {
            if let Value::Ref(Some(text)) = value {
                collectable.scan(text, &mut found);
            }
        }
        pending.extend(found.drain(..).filter(|next| !reached.contains(next)));
    }
    let keep = |marker: &str| !collectable.markers.contains(marker) || reached.contains(marker);
    heap.fields.retain(|key, _| keep(field_receiver(key)));
    heap.arrays.retain(|key, _| keep(key));
    heap.string_builders.retain(|key, _| keep(key));
    heap.enum_constants.retain(|key, _| keep(key));
    host.mutable_collections.retain(|key, _| keep(key));
    host.iterators.retain(|key, _| keep(key));
}

impl<H: Host> Runtime<'_, H> {
    /// Continue from the objects earlier invocations of this jar left on its
    /// host. Answers whether the host keeps a resident heap; when it does not,
    /// the runtime keeps whatever it was built with, as before.
    ///
    /// Entries the runtime was built with win over the host's: a caller that
    /// seeds instance state (the game-hook tier's state from before the
    /// resident heap) knows it better.
    pub(crate) fn adopt_resident_heap(&mut self) -> bool {
        let Some(mut heap) = self.host.take_persistent_heap() else {
            return false;
        };
        heap.fields.append(&mut self.fields);
        heap.arrays.append(&mut self.arrays);
        heap.string_builders.append(&mut self.string_builders);
        heap.enum_constants.append(&mut self.enum_constants);
        self.fields = heap.fields;
        self.arrays = heap.arrays;
        self.string_builders = heap.string_builders;
        self.enum_constants = heap.enum_constants;
        self.next_object = self.next_object.max(heap.next_object);
        self.next_array = self.next_array.max(heap.next_array);
        true
    }

    /// Hand this invocation's objects back to the host for the next one.
    /// Call it once, after [`Self::persist_class_state`] (the statics are the
    /// collection's roots), and only after [`Self::adopt_resident_heap`]
    /// answered `true`.
    pub(crate) fn persist_resident_heap(&mut self) -> Result<(), NativeError> {
        self.persist_resident_heap_with(Vec::new())
    }

    /// [`Self::persist_resident_heap`] with texts the caller keeps outside the
    /// host, which keep the objects they name alive through this store.
    pub(crate) fn persist_resident_heap_with(
        &mut self,
        roots: Vec<String>,
    ) -> Result<(), NativeError> {
        let heap = JavaHeap {
            fields: std::mem::take(&mut self.fields),
            arrays: std::mem::take(&mut self.arrays),
            string_builders: std::mem::take(&mut self.string_builders),
            enum_constants: std::mem::take(&mut self.enum_constants),
            next_object: self.next_object,
            next_array: self.next_array,
            live: 0,
            roots,
        };
        self.host.store_persistent_heap(heap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> Value {
        Value::Ref(Some(value.to_owned()))
    }

    fn field(object: &str, name: &str) -> String {
        format!("{object}|demo/Node#{name}Ljava/lang/Object;")
    }

    /// A host whose static `ROOT` names `root`, and a heap with a chain
    /// `root → a → b`, an orphan `c → d` and an array only `b` names.
    fn graph(root: &str) -> (StdHost, JavaHeap) {
        let mut host = StdHost::new();
        host.persistent_static_fields
            .insert("demo/Mod#ROOTLjava/lang/Object;".into(), text(root));
        let mut heap = JavaHeap::default();
        heap.fields
            .insert(field("demo/Node#1", "next"), text("demo/Node#2"));
        heap.fields.insert(
            field("demo/Node#2", "next"),
            text("array/Ldemo/Node;/run/0"),
        );
        heap.fields
            .insert(field("demo/Node#3", "next"), text("demo/Node#4"));
        heap.fields
            .insert(field("demo/Node#4", "next"), Value::Int(4));
        heap.arrays
            .insert("array/Ldemo/Node;/run/0".into(), vec![Value::Int(1)]);
        heap.arrays
            .insert("array/Ldemo/Node;/run/1".into(), vec![Value::Int(2)]);
        (host, heap)
    }

    fn objects(heap: &JavaHeap) -> Vec<&str> {
        let mut out: Vec<&str> = heap.fields.keys().map(|key| field_receiver(key)).collect();
        out.extend(heap.arrays.keys().map(String::as_str));
        out.dedup();
        out
    }

    #[test]
    fn collection_keeps_what_the_statics_reach_and_drops_the_rest() {
        let (mut host, mut heap) = graph("demo/Node#1");
        collect(&mut heap, &mut host, &[]);
        assert_eq!(
            objects(&heap),
            ["demo/Node#1", "demo/Node#2", "array/Ldemo/Node;/run/0"]
        );
    }

    #[test]
    fn markers_embedded_in_a_lambda_or_an_entry_keep_their_objects() {
        // A static lambda capturing node 3, and node 1 only as a map entry's
        // value inside a list the host keeps for a static.
        let (mut host, mut heap) =
            graph("@indy:demo/Mod#init:4:demo/Mod#lambda$0(Ldemo/Node;)V:rdemo/Node#3");
        host.persistent_static_fields.insert(
            "demo/Mod#MAPLjava/util/List;".into(),
            text("java/util/ArrayList#7"),
        );
        host.mutable_collections.insert(
            "java/util/ArrayList#7".into(),
            vec![text("map-entry/rkey,rdemo/Node#1")],
        );
        host.mutable_collections
            .insert("java/util/ArrayList#8".into(), vec![text("demo/Node#3")]);
        collect(&mut heap, &mut host, &[]);
        assert_eq!(
            objects(&heap),
            [
                "demo/Node#1",
                "demo/Node#2",
                "demo/Node#3",
                "demo/Node#4",
                "array/Ldemo/Node;/run/0"
            ]
        );
        // The unreachable list is dropped; the static's is kept.
        assert_eq!(
            host.mutable_collections.keys().collect::<Vec<_>>(),
            ["java/util/ArrayList#7"]
        );
    }

    #[test]
    fn a_marker_that_only_shares_a_prefix_is_not_a_reference() {
        // `demo/Node#12` is not `demo/Node#1`.
        let (mut host, mut heap) = graph("demo/Node#12");
        collect(&mut heap, &mut host, &[]);
        assert!(objects(&heap).is_empty(), "{:?}", objects(&heap));
    }

    #[test]
    fn entrypoint_and_game_object_fields_are_roots() {
        let (mut host, mut heap) = graph("nothing");
        // The entrypoint instance is named by its class, a game object by
        // the hook tier's prefix: neither is collectable, both keep what
        // their fields name.
        heap.fields
            .insert(field("demo/Mod", "first"), text("demo/Node#1"));
        heap.fields
            .insert(field("game/entity/7", "tag"), text("demo/Node#3"));
        collect(&mut heap, &mut host, &[]);
        assert_eq!(heap.fields.len(), 6);
        assert_eq!(heap.arrays.len(), 1);
    }

    #[test]
    fn a_store_collects_once_the_heap_outgrows_its_trigger_and_refuses_past_its_bound() {
        let mut host = StdHost::new();
        let mut heap = JavaHeap::default();
        for index in 0..COLLECT_SLACK {
            heap.fields
                .insert(field(&format!("demo/Node#{index}"), "x"), Value::Int(1));
        }
        // At the trigger, nothing is collected yet.
        host.store_resident_heap(heap.clone()).unwrap();
        assert_eq!(host.resident_heap().instance_fields(), COLLECT_SLACK);
        // One past it, the unreachable nodes go.
        heap.fields
            .insert(field("demo/Node#extra1", "x"), Value::Int(1));
        heap.fields
            .insert(field("demo/Node#9999", "x"), Value::Int(1));
        host.store_resident_heap(heap).unwrap();
        assert_eq!(
            host.resident_heap().fields.keys().collect::<Vec<_>>(),
            [&field("demo/Node#extra1", "x")]
        );

        // Reachable state past the bound is refused, and still kept.
        let mut big = JavaHeap::default();
        for index in 0..=MAX_RESIDENT_FIELDS {
            big.fields
                .insert(field("demo/Mod", &format!("f{index}")), Value::Int(1));
        }
        let error = host.store_resident_heap(big).unwrap_err().to_string();
        assert!(error.contains("over the resident bound"), "{error}");
        assert_eq!(
            host.resident_heap().instance_fields(),
            MAX_RESIDENT_FIELDS + 1
        );
    }

    #[test]
    fn a_rewound_call_log_drops_only_what_came_after_the_mark() {
        let mut host = StdHost::new();
        host.posted_events.push(Value::Int(1));
        let mark = host.call_log_mark();
        host.posted_events.push(Value::Int(2));
        host.rewind_call_log(mark);
        assert_eq!(host.posted_events, [Value::Int(1)]);
    }
}
