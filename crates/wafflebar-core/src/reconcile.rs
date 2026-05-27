//! Keyed reconciliation: a **pure, GTK-free** diff between an old and new [`View`] child list.
//!
//! This is the React/Vue keying model, not GTK's `ListModel` machinery (the View tree is
//! heterogeneous — buttons, labels, separators, nested containers — so a flat `ListModel` is the
//! wrong shape). Each child gets a key: the plugin's explicit key ([`View::explicit_key`]) when set
//! (e.g. a tasklist entry keyed by toplevel handle id), otherwise `position + variant tag`. The
//! diff matches new children to old by key and emits, per new child, whether to keep / update /
//! recurse / create it, plus the keys that were removed.
//!
//! Keys live in `View`, not in the renderer's private state — the same v1→v2 process-isolation
//! discipline as the rest of the boundary: the host (renderer) applies these patches to real
//! widgets, but the *decision* of what changed is serializable data computed here.
//!
//! Scope is deliberately correctness, not cleverness: synchronous, no widget pooling, no batching.
//! Containers ([`View::Row`]/[`View::Col`]) recurse; everything else is a leaf compared by `==`.

use crate::view::View;

/// The reconcile key for a child at `index` in its parent: explicit key if the plugin set one,
/// else `position + variant tag` (so positional children stay stable and a `Label`/`Icon` swap at
/// the same index doesn't alias).
pub fn child_key(view: &View, index: usize) -> String {
    match view.explicit_key() {
        Some(k) => format!("k:{k}"),
        None => format!("p:{index}:{}", view.variant_tag()),
    }
}

/// What to do with a single *new* child during apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChildPatch {
    /// Key absent from the old list → build a fresh widget.
    Create,
    /// Key present and the node is unchanged → reuse the existing widget as-is.
    Keep,
    /// Key present, leaf content changed (or a container's own chrome changed) → rebuild this node.
    Update,
    /// Key present and it's a matched container → reuse the widget, recurse into its children.
    Recurse(ListPatch),
}

/// The diff of one container's children: a patch per *new* child (1:1, in new order) plus the keys
/// that disappeared (present in old, absent in new) and so must be destroyed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ListPatch {
    pub children: Vec<ChildPatch>,
    pub destroyed: Vec<String>,
}

/// Create/destroy/update tallies over a patch tree — the operation record the reconcile tests
/// assert against (a keyed diff that rebuilds the world would show inflated numbers here).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    pub creates: usize,
    pub updates: usize,
    pub destroys: usize,
}

impl std::ops::AddAssign for Counts {
    fn add_assign(&mut self, rhs: Self) {
        self.creates += rhs.creates;
        self.updates += rhs.updates;
        self.destroys += rhs.destroys;
    }
}

impl ListPatch {
    /// True when nothing changed (all children kept, nothing destroyed).
    pub fn is_noop(&self) -> bool {
        self.destroyed.is_empty() && self.children.iter().all(|c| *c == ChildPatch::Keep)
    }

    /// Aggregate operation counts over the whole patch tree (recurses into `Recurse`).
    pub fn counts(&self) -> Counts {
        let mut c = Counts {
            destroys: self.destroyed.len(),
            ..Counts::default()
        };
        for child in &self.children {
            match child {
                ChildPatch::Create => c.creates += 1,
                ChildPatch::Update => c.updates += 1,
                ChildPatch::Keep => {}
                ChildPatch::Recurse(sub) => c += sub.counts(),
            }
        }
        c
    }
}

/// Diff two child lists by key. `old`/`new` are the children of the same container across an
/// update (the slot root is modeled as a one-element list).
pub fn diff_children(old: &[View], new: &[View]) -> ListPatch {
    // Index old children by key (position-derived keys are unique by construction; explicit keys
    // are the plugin's responsibility to keep unique — duplicates just match the first).
    let old_keyed: Vec<(String, &View)> = old
        .iter()
        .enumerate()
        .map(|(i, v)| (child_key(v, i), v))
        .collect();

    let mut children = Vec::with_capacity(new.len());
    let mut matched = vec![false; old_keyed.len()];

    for (i, nv) in new.iter().enumerate() {
        let nk = child_key(nv, i);
        match old_keyed.iter().position(|(ok, _)| *ok == nk) {
            Some(pos) => {
                matched[pos] = true;
                children.push(diff_node(old_keyed[pos].1, nv));
            }
            None => children.push(ChildPatch::Create),
        }
    }

    let destroyed = old_keyed
        .iter()
        .zip(&matched)
        .filter(|(_, m)| !**m)
        .map(|((k, _), _)| k.clone())
        .collect();

    ListPatch { children, destroyed }
}

/// Decide the patch for a matched key (same key in old and new).
fn diff_node(old: &View, new: &View) -> ChildPatch {
    match (old, new) {
        // Matched containers: recurse if their own chrome (gap/classes) is unchanged; otherwise the
        // container itself changed, so rebuild the whole subtree.
        (
            View::Row { children: oc, gap: og, classes: ocl },
            View::Row { children: nc, gap: ng, classes: ncl },
        )
        | (
            View::Col { children: oc, gap: og, classes: ocl },
            View::Col { children: nc, gap: ng, classes: ncl },
        ) => {
            if og != ng || ocl != ncl {
                ChildPatch::Update
            } else {
                let sub = diff_children(oc, nc);
                if sub.is_noop() {
                    ChildPatch::Keep
                } else {
                    ChildPatch::Recurse(sub)
                }
            }
        }
        // Leaves (and Row-vs-Col / variant changes): unchanged → keep, otherwise rebuild this node.
        // Note: `Button` and `Popover` are leaves here even though they carry child Views — their
        // content is rebuilt on change, not finely reconciled into. For `Popover` this is cheap
        // because popovers are typically closed; if a future consumer hosts a high-frequency widget
        // in popover content, upgrade it to a recursed container with keyed-diff into the content.
        _ if old == new => ChildPatch::Keep,
        _ => ChildPatch::Update,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::ActionId;

    /// A tasklist-style entry: an icon+label button keyed by a stable id.
    fn entry(id: u64, title: &str, active: bool) -> View {
        let mut b = View::row(vec![View::icon("app", 16), View::label(title)], 4)
            .button(ActionId::new(format!("activate:{id}")))
            .with_key(format!("win:{id}"));
        if active {
            b = b.with_class("active");
        }
        b
    }

    fn row(children: Vec<View>) -> View {
        View::row(children, 2)
    }

    #[test]
    fn closing_middle_entry_destroys_one_keeps_the_rest() {
        // tasklist canary: 3 windows keyed by handle id; close the middle one.
        let old = [row(vec![entry(0, "a", false), entry(1, "b", false), entry(2, "c", false)])];
        let new = [row(vec![entry(0, "a", false), entry(2, "c", false)])];
        let patch = diff_children(&old, &new);
        let counts = patch.counts();
        assert_eq!(counts.destroys, 1, "exactly one entry destroyed");
        assert_eq!(counts.creates, 0, "no entry recreated");
        assert_eq!(counts.updates, 0, "the surviving entries are untouched");
    }

    #[test]
    fn switching_active_tag_updates_exactly_two() {
        // tags canary: active class moves from tag 0 to tag 1 — two nodes change, nothing else.
        let tag = |i: u64, active: bool| {
            let mut b = View::label(format!("{i}"))
                .button(ActionId::new(format!("tag:{i}")))
                .with_key(format!("tag:{i}"));
            if active {
                b = b.with_class("active");
            }
            b
        };
        let old = [row((0..9).map(|i| tag(i, i == 0)).collect())];
        let new = [row((0..9).map(|i| tag(i, i == 1)).collect())];
        let counts = diff_children(&old, &new).counts();
        assert_eq!(counts.creates, 0);
        assert_eq!(counts.destroys, 0);
        assert_eq!(counts.updates, 2, "old-active loses the class, new-active gains it");
    }

    #[test]
    fn identical_views_are_a_noop() {
        let v = [row(vec![entry(0, "a", false), entry(1, "b", true)])];
        let patch = diff_children(&v.clone(), &v);
        assert!(patch.is_noop());
        assert_eq!(patch.counts(), Counts::default());
    }

    #[test]
    fn appending_an_entry_creates_only_it() {
        let old = [row(vec![entry(0, "a", false)])];
        let new = [row(vec![entry(0, "a", false), entry(1, "b", false)])];
        let counts = diff_children(&old, &new).counts();
        assert_eq!(counts, Counts { creates: 1, updates: 0, destroys: 0 });
    }

    #[test]
    fn popover_content_change_is_one_update() {
        // Popover is a reconcile leaf: changing its content rebuilds the node (one Update), the
        // leaf-reconcile path D1 relies on. (Popovers are usually closed; rebuild is cheap.)
        let popover = |label: &str| View::Popover {
            trigger: Box::new(View::icon("tray-item", 16)),
            content: Box::new(View::label(label)),
            classes: Vec::new(),
        };
        let old = [row(vec![popover("Open")])];
        let new = [row(vec![popover("Close")])];
        let counts = diff_children(&old, &new).counts();
        assert_eq!(counts, Counts { creates: 0, updates: 1, destroys: 0 });
    }

    #[test]
    fn positional_children_without_keys_still_diff() {
        // No explicit keys (clock-style): position+variant keys keep a changed label localized.
        let old = [row(vec![View::label("12:00"), View::label("static")])];
        let new = [row(vec![View::label("12:01"), View::label("static")])];
        let counts = diff_children(&old, &new).counts();
        assert_eq!(counts, Counts { creates: 0, updates: 1, destroys: 0 });
    }
}
