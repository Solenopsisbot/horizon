//! The order a game event's listeners run in, the way the loaders order them.
//!
//! - **Fabric** (`fabric-api-base`'s `ArrayBackedEvent`): a listener registers
//!   on a phase (`register(Identifier phase, listener)`; plain `register` is
//!   [`DEFAULT_PHASE`], `fabric:default`), `addPhaseOrdering(first, second)`
//!   says one phase runs before another, and listeners run phase by phase, in
//!   registration order within a phase. Phases are sorted the way
//!   `NodeSorting.sort` does: strongly connected components (a cycle of
//!   orderings) are collapsed, then a topological sort that, whenever it has
//!   a choice, takes the component whose smallest phase id is smallest by
//!   `Identifier.compareTo` (path first, then namespace). A phase with no
//!   ordering therefore sorts by its id: `fabric:default` before
//!   `mymod:late`, but after `mymod:aaa`.
//! - **NeoForge/Forge** (`EventPriority`): `HIGHEST`, `HIGH`, `NORMAL`, `LOW`,
//!   `LOWEST`, registration order within a priority. The priority comes from
//!   `addListener(EventPriority, ...)` or `@SubscribeEvent(priority = ...)`.
//!
//! All jars share one `Event` (one bus) in a real game, so the order is global
//! across jars: a phase ordering one jar declares orders every jar's listeners
//! of that event, and "registration order" is the jars' load order (the
//! batch's path order) then each jar's own order.
//!
//! Each Fabric `Event` field orders its own phases: two fields the game fires
//! as one [`GameEvent`] (`ServerPlayConnectionEvents.JOIN` and
//! `ServerPlayerEvents.JOIN`) run field by field, in the order their first
//! listener registered. A bus listener of the same [`GameEvent`] (a jar that
//! uses both APIs) runs after every Fabric listener: the two loaders never
//! share an event in a real game, so no order between them is observable.

use std::{
    cmp::{Ordering, Reverse},
    collections::{BTreeMap, BTreeSet, BinaryHeap},
};

use super::GameListener;

/// Fabric's `Event.DEFAULT_PHASE`.
pub const DEFAULT_PHASE: &str = "fabric:default";

/// NeoForge's (and Forge's) `EventPriority` constants, first to last.
pub const PRIORITIES: [&str; 5] = ["HIGHEST", "HIGH", "NORMAL", "LOW", "LOWEST"];

/// The `EventPriority` enum classes a bus listener's priority comes from.
pub(crate) const PRIORITY_CLASSES: [&str; 2] = [
    "net/neoforged/bus/api/EventPriority",
    "net/minecraftforge/eventbus/api/EventPriority",
];

/// Where one listener sits in its event's order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListenerOrder {
    /// A Fabric listener's phase id (`fabric:default`, `mymod:late`).
    Phase(String),
    /// A NeoForge/Forge listener's priority, an index into [`PRIORITIES`]
    /// (`NORMAL` is 2).
    Priority(u8),
}

impl Default for ListenerOrder {
    fn default() -> Self {
        ListenerOrder::Priority(2)
    }
}

impl ListenerOrder {
    /// The default Fabric phase.
    pub fn default_phase() -> Self {
        ListenerOrder::Phase(DEFAULT_PHASE.into())
    }

    /// For diagnostics: the phase id, or the priority's name.
    pub fn label(&self) -> &str {
        match self {
            ListenerOrder::Phase(id) => id,
            ListenerOrder::Priority(at) => PRIORITIES.get(usize::from(*at)).copied().unwrap_or("?"),
        }
    }
}

/// One `addPhaseOrdering(first, second)` a jar made on a Fabric `Event` field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhaseOrdering {
    /// The field, as a listener's `source` names it (`UseBlockCallback.EVENT`).
    pub source: String,
    pub first: String,
    pub second: String,
}

/// The priority an `EventPriority` constant marker (`enum/<class>/HIGH`) or an
/// annotation's enum value (`L<class>;.HIGH`) names.
pub(crate) fn priority_of(text: &str) -> Option<u8> {
    let name = PRIORITY_CLASSES.iter().find_map(|class| {
        text.strip_prefix("enum/")
            .and_then(|rest| rest.strip_prefix(class))
            .and_then(|rest| rest.strip_prefix('/'))
            .or_else(|| {
                text.strip_prefix('L')
                    .and_then(|rest| rest.strip_prefix(class))
                    .and_then(|rest| rest.strip_prefix(";."))
            })
    })?;
    PRIORITIES
        .iter()
        .position(|p| *p == name)
        .and_then(|at| u8::try_from(at).ok())
}

/// `Identifier.compareTo`: the path first, then the namespace. An id without
/// a namespace is `minecraft:`'s, as `Identifier.parse` reads it.
pub fn identifier_cmp(a: &str, b: &str) -> Ordering {
    let split = |id: &'_ str| -> (String, String) {
        match id.split_once(':') {
            Some((namespace, path)) => (namespace.to_owned(), path.to_owned()),
            None => ("minecraft".to_owned(), id.to_owned()),
        }
    };
    let (an, ap) = split(a);
    let (bn, bp) = split(b);
    ap.cmp(&bp).then_with(|| an.cmp(&bn))
}

/// A phase id ordered by [`identifier_cmp`], for the sort's heap.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PhaseId(String);

impl Ord for PhaseId {
    fn cmp(&self, other: &Self) -> Ordering {
        identifier_cmp(&self.0, &other.0).then_with(|| self.0.cmp(&other.0))
    }
}

impl PartialOrd for PhaseId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Fabric's phase order for `phases` under the `(first, second)` orderings:
/// `NodeSorting.sort` over the phases with their links. Every phase an
/// ordering names takes part, as `addPhaseOrdering` creates both phases.
pub fn sort_phases<'a>(
    phases: impl IntoIterator<Item = &'a str>,
    orderings: &[(&'a str, &'a str)],
) -> Vec<String> {
    let mut nodes: BTreeSet<&str> = phases.into_iter().collect();
    for (first, second) in orderings {
        nodes.insert(first);
        nodes.insert(second);
    }
    let nodes: Vec<&str> = nodes.into_iter().collect();
    let index: BTreeMap<&str, usize> = nodes.iter().enumerate().map(|(i, n)| (*n, i)).collect();
    let mut edges = vec![BTreeSet::new(); nodes.len()];
    for (first, second) in orderings {
        // `addPhaseOrdering` refuses a phase ordered against itself.
        if first != second {
            edges[index[first]].insert(index[second]);
        }
    }
    // Reachability (phases are a handful per event), then the components:
    // two phases share one when each reaches the other.
    let n = nodes.len();
    let mut reach = vec![vec![false; n]; n];
    for (start, row) in reach.iter_mut().enumerate() {
        let mut stack = vec![start];
        while let Some(at) = stack.pop() {
            for &next in &edges[at] {
                if !row[next] {
                    row[next] = true;
                    stack.push(next);
                }
            }
        }
    }
    let mut component = vec![usize::MAX; n];
    let mut members: Vec<Vec<usize>> = Vec::new();
    for a in 0..n {
        if component[a] != usize::MAX {
            continue;
        }
        let id = members.len();
        let mut group = vec![a];
        component[a] = id;
        for b in a + 1..n {
            if component[b] == usize::MAX && reach[a][b] && reach[b][a] {
                component[b] = id;
                group.push(b);
            }
        }
        group.sort_by(|x, y| PhaseId(nodes[*x].into()).cmp(&PhaseId(nodes[*y].into())));
        members.push(group);
    }
    let mut in_degree = vec![0usize; members.len()];
    let mut next: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); members.len()];
    for (from, targets) in edges.iter().enumerate() {
        for &to in targets {
            let (a, b) = (component[from], component[to]);
            if a != b && next[a].insert(b) {
                in_degree[b] += 1;
            }
        }
    }
    // Kahn's sort, taking the component whose first (smallest) phase is
    // smallest whenever several are ready.
    let mut ready: BinaryHeap<Reverse<(PhaseId, usize)>> = BinaryHeap::new();
    for (id, group) in members.iter().enumerate() {
        if in_degree[id] == 0 {
            ready.push(Reverse((PhaseId(nodes[group[0]].into()), id)));
        }
    }
    let mut out = Vec::with_capacity(n);
    while let Some(Reverse((_, id))) = ready.pop() {
        out.extend(members[id].iter().map(|&m| nodes[m].to_owned()));
        for &after in &next[id] {
            in_degree[after] -= 1;
            if in_degree[after] == 0 {
                ready.push(Reverse((PhaseId(nodes[members[after][0]].into()), after)));
            }
        }
    }
    out
}

/// The order `listeners` run in: each entry is `(jar, slot, listener)` in
/// the jars' load order and each jar's registration order; the answer is the
/// `(jar, slot)` pairs in dispatch order. `orderings` are every jar's
/// `addPhaseOrdering` calls.
pub fn dispatch_order(
    listeners: &[(usize, usize, &GameListener)],
    orderings: &[&PhaseOrdering],
) -> Vec<(usize, usize)> {
    // Fabric fields in the order their first listener registered.
    let mut fields: Vec<&str> = Vec::new();
    for (_, _, listener) in listeners {
        if matches!(listener.order, ListenerOrder::Phase(_))
            && !fields.contains(&listener.source.as_str())
        {
            fields.push(&listener.source);
        }
    }
    let ranks: Vec<BTreeMap<String, usize>> = fields
        .iter()
        .map(|field| {
            let phases = listeners.iter().filter_map(|(_, _, l)| match &l.order {
                ListenerOrder::Phase(id) if l.source == *field => Some(id.as_str()),
                _ => None,
            });
            let links: Vec<(&str, &str)> = orderings
                .iter()
                .filter(|o| o.source == *field)
                .map(|o| (o.first.as_str(), o.second.as_str()))
                .collect();
            sort_phases(phases, &links)
                .into_iter()
                .enumerate()
                .map(|(rank, id)| (id, rank))
                .collect()
        })
        .collect();
    let mut keyed: Vec<((u8, usize, usize), (usize, usize))> = listeners
        .iter()
        .map(|(jar, slot, listener)| {
            let group = match &listener.order {
                ListenerOrder::Phase(id) => {
                    let field = fields
                        .iter()
                        .position(|f| *f == listener.source)
                        .unwrap_or(0);
                    (
                        0,
                        field,
                        ranks[field].get(id).copied().unwrap_or(usize::MAX),
                    )
                }
                ListenerOrder::Priority(priority) => (1, 0, usize::from(*priority)),
            };
            (group, (*jar, *slot))
        })
        .collect();
    // Stable: equal keys keep the load and registration order.
    keyed.sort_by_key(|(key, _)| *key);
    keyed.into_iter().map(|(_, at)| at).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unordered_phases_sort_by_identifier_path_first() {
        // `Identifier.compareTo` compares paths, then namespaces.
        assert_eq!(
            sort_phases(["zzz:early", "fabric:default", "aaa:late"], &[]),
            ["fabric:default", "zzz:early", "aaa:late"]
        );
        assert_eq!(
            identifier_cmp("b:same", "a:same"),
            Ordering::Greater,
            "namespace breaks a path tie"
        );
    }

    #[test]
    fn orderings_win_over_identifier_order() {
        // `mymod:late` sorts after `fabric:default` by id; an ordering moves
        // it first. The two phases it frees are then taken by id: `after`
        // before `default`.
        let order = sort_phases(
            ["fabric:default", "mymod:late", "mymod:after"],
            &[
                ("mymod:late", "fabric:default"),
                ("mymod:late", "mymod:after"),
            ],
        );
        assert_eq!(order, ["mymod:late", "mymod:after", "fabric:default"]);
    }

    #[test]
    fn a_cycle_runs_as_one_group_in_identifier_order() {
        // a → b → a is one component: its phases in id order, and a phase
        // ordered before the cycle still runs first.
        let order = sort_phases(
            ["x:b", "x:a", "x:z"],
            &[("x:b", "x:a"), ("x:a", "x:b"), ("x:z", "x:a")],
        );
        assert_eq!(order, ["x:z", "x:a", "x:b"]);
    }

    #[test]
    fn priorities_read_markers_and_annotation_values() {
        assert_eq!(
            priority_of("enum/net/neoforged/bus/api/EventPriority/HIGHEST"),
            Some(0)
        );
        assert_eq!(
            priority_of("Lnet/minecraftforge/eventbus/api/EventPriority;.LOW"),
            Some(3)
        );
        assert_eq!(priority_of("enum/net/minecraft/core/Direction/UP"), None);
    }
}
