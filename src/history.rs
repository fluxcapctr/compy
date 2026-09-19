//! Undo history over value snapshots, as `DocumentHistory` keeps it: every entry holds the state before and
//! after, pixel surfaces are shared by reference so snapshots cost nothing, and entries fall off the far end
//! past a count or a byte budget.

pub struct History<S> {
    past: Vec<Entry<S>>,
    future: Vec<Entry<S>>,
    pending: Option<(String, S)>,
    depth: usize,
    revision: u64,
    saved: u64,
    next: u64,
    pub entry_limit: usize,
    pub byte_limit: usize,
}

struct Entry<S> {
    name: String,
    before: S,
    after: S,
    /// The revisions the two states carried, so returning to a saved state reads as unmodified.
    revision_before: u64,
    revision_after: u64,
    at: std::time::Instant,
}

/// Same-named edits closer together than this merge when the caller asks (slider steps).
const MERGE_WINDOW: std::time::Duration = std::time::Duration::from_millis(1500);

impl<S: Clone + PartialEq> History<S> {
    pub fn new(entry_limit: usize, byte_limit: usize) -> Self {
        History { past: Vec::new(), future: Vec::new(), pending: None, depth: 0, revision: 0, saved: 0, next: 1, entry_limit, byte_limit }
    }

    pub fn can_undo(&self) -> bool { self.depth == 0 && !self.past.is_empty() }
    pub fn can_redo(&self) -> bool { self.depth == 0 && !self.future.is_empty() }
    pub fn undo_name(&self) -> Option<&str> { self.past.last().map(|e| e.name.as_str()) }
    pub fn redo_name(&self) -> Option<&str> { self.future.last().map(|e| e.name.as_str()) }
    pub fn is_modified(&self) -> bool { self.revision != self.saved }
    pub fn mark_saved(&mut self) { self.saved = self.revision; }
    /// A recovered document is unsaved work even before its first edit.
    pub fn mark_unsaved(&mut self) { self.saved = u64::MAX; }
    /// An edit is open (its changes are on screen but not yet a step).
    pub fn is_editing(&self) -> bool { self.depth > 0 }

    /// Starts an edit; nested begins fold into the outermost one.
    pub fn begin(&mut self, name: &str, state: S) {
        if self.depth == 0 { self.pending = Some((name.to_string(), state)); }
        self.depth += 1;
    }

    /// Abandons the edit in progress, returning the state it started from when the outermost edit ends.
    pub fn cancel(&mut self) -> Option<S> {
        if self.depth == 0 { return None; }
        self.depth -= 1;
        if self.depth > 0 { return None; }
        self.pending.take().map(|(_, before)| before)
    }

    /// Ends an edit. An edit that changed nothing leaves history (and redo) alone. `bytes` measures what a
    /// list of states retains beyond `current`, for the byte budget.
    pub fn end(&mut self, state: S, bytes: impl Fn(&[&S], &S) -> usize) { self.end_with(state, false, bytes); }

    /// `merge` folds the edit into the previous entry when that has the same name, nothing was undone in
    /// between, and it was made within the last second and a half.
    pub fn end_with(&mut self, state: S, merge: bool, bytes: impl Fn(&[&S], &S) -> usize) {
        if self.depth == 0 { return; }
        self.depth -= 1;
        if self.depth > 0 { return; }
        let Some((name, before)) = self.pending.take() else { return };
        if before == state { return; }
        let now = std::time::Instant::now();
        if merge && self.future.is_empty() {
            if let Some(last) = self.past.last_mut() {
                if last.name == name && last.after == before && now.duration_since(last.at) < MERGE_WINDOW {
                    last.after = state.clone();
                    last.at = now;
                    self.revision = self.next;
                    self.next += 1;
                    last.revision_after = self.revision;
                    self.trim(&state, bytes);
                    return;
                }
            }
        }
        let revision_before = self.revision;
        self.revision = self.next;
        self.next += 1;
        self.past.push(Entry { name, before, after: state.clone(), revision_before, revision_after: self.revision, at: now });
        self.future.clear();
        self.trim(&state, bytes);
    }

    pub fn undo(&mut self) -> Option<S> {
        if !self.can_undo() { return None; }
        let entry = self.past.pop()?;
        let state = entry.before.clone();
        self.revision = entry.revision_before;
        self.future.push(entry);
        Some(state)
    }

    pub fn redo(&mut self) -> Option<S> {
        if !self.can_redo() { return None; }
        let entry = self.future.pop()?;
        let state = entry.after.clone();
        self.revision = entry.revision_after;
        self.past.push(entry);
        Some(state)
    }

    fn trim(&mut self, current: &S, bytes: impl Fn(&[&S], &S) -> usize) {
        loop {
            let held: Vec<&S> = self.past.iter().chain(self.future.iter()).flat_map(|e| [&e.before, &e.after]).collect();
            let over = held.len() / 2 > self.entry_limit || bytes(&held, current) > self.byte_limit;
            if !over { break; }
            if !self.past.is_empty() { self.past.remove(0); }
            else if !self.future.is_empty() { self.future.remove(0); }
            else { break; }
        }
    }
}
