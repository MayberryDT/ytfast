//! The queue in play order, edited as YouTube Music does: Play next goes
//! right after the current song; Add to queue after earlier additions but
//! before the rest of the list; shuffle keeps the additions, in order,
//! right after the current song, and turning it off goes back to the list's
//! order. Positions are play-order indices, as Up next shows them; entries
//! also carry ids that stay put while positions shift.

use serde::{Deserialize, Serialize};

use crate::model::Track;

#[derive(Clone)]
pub(super) struct Entry {
    pub id: u64,
    pub track: Track,
    /// Added with Play next or Add to queue and not reached yet.
    pub added: bool,
}

#[derive(Default)]
pub(super) struct Queue {
    /// Play order.
    entries: Vec<Entry>,
    /// Entry ids in list order: what turning shuffle off goes back to.
    context: Vec<u64>,
    next_id: u64,
}

/// The queue as session.json keeps it.
#[derive(Serialize, Deserialize)]
pub(super) struct Snapshot {
    entries: Vec<SavedEntry>,
    /// List order, as indices into `entries`.
    context: Vec<usize>,
}

#[derive(Serialize, Deserialize)]
struct SavedEntry {
    track: Track,
    added: bool,
}

impl Queue {
    fn entry(&mut self, track: Track, added: bool) -> Entry {
        self.next_id += 1;
        Entry {
            id: self.next_id,
            track,
            added,
        }
    }

    /// A new list: play order and list order alike.
    pub fn replace(&mut self, tracks: Vec<Track>) {
        self.entries = tracks.into_iter().map(|t| self.entry(t, false)).collect();
        self.context = self.entries.iter().map(|e| e.id).collect();
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, pos: usize) -> Option<&Entry> {
        self.entries.get(pos)
    }

    pub fn track(&self, pos: usize) -> Option<&Track> {
        self.entries.get(pos).map(|e| &e.track)
    }

    pub fn id(&self, pos: usize) -> Option<u64> {
        self.entries.get(pos).map(|e| e.id)
    }

    pub fn position(&self, id: u64) -> Option<usize> {
        self.entries.iter().position(|e| e.id == id)
    }

    pub fn last(&self) -> Option<&Track> {
        self.entries.last().map(|e| &e.track)
    }

    pub fn video_ids(&self) -> std::collections::HashSet<String> {
        self.entries
            .iter()
            .map(|e| e.track.video_id.clone())
            .collect()
    }

    /// The tracks in play order.
    pub fn tracks(&self) -> Vec<Track> {
        self.entries.iter().map(|e| e.track.clone()).collect()
    }

    /// The entry at `pos` is playing: it is no longer an addition waiting.
    pub fn reached(&mut self, pos: usize) {
        if let Some(entry) = self.entries.get_mut(pos) {
            entry.added = false;
        }
    }

    /// How many additions wait right after `current`.
    fn additions(&self, current: usize) -> usize {
        self.entries
            .iter()
            .skip(current + 1)
            .take_while(|e| e.added)
            .count()
    }

    fn context_index(&self, id: u64) -> Option<usize> {
        self.context.iter().position(|&c| c == id)
    }

    /// Inserts additions at play position `at`, and in list order after
    /// the entry `after` (the end when there is none).
    fn insert_added(&mut self, at: usize, after: Option<u64>, tracks: Vec<Track>) {
        let entries: Vec<Entry> = tracks.into_iter().map(|t| self.entry(t, true)).collect();
        let list_at = after
            .and_then(|id| self.context_index(id))
            .map_or(self.context.len(), |i| i + 1);
        self.context
            .splice(list_at..list_at, entries.iter().map(|e| e.id));
        let at = at.min(self.entries.len());
        self.entries.splice(at..at, entries);
    }

    /// Play next: right after the current song, before earlier additions.
    pub fn play_next(&mut self, current: Option<usize>, tracks: Vec<Track>) {
        let at = current.map_or(self.entries.len(), |c| c + 1);
        let after = current.and_then(|c| self.id(c));
        self.insert_added(at, after, tracks);
    }

    /// Add to queue: after earlier additions, before the rest of the list.
    pub fn add_to_queue(&mut self, current: Option<usize>, tracks: Vec<Track>) {
        let (at, after) = match current {
            Some(c) => {
                let last = c + self.additions(c);
                (last + 1, self.id(last))
            }
            None => (self.entries.len(), self.entries.last().map(|e| e.id)),
        };
        self.insert_added(at, after, tracks);
    }

    pub fn remove(&mut self, pos: usize) -> Option<Entry> {
        if pos >= self.entries.len() {
            return None;
        }
        let entry = self.entries.remove(pos);
        self.context.retain(|&id| id != entry.id);
        Some(entry)
    }

    /// Moves the entry at `from` to `to` (both play positions; `to` counts
    /// after the entry is taken out). Landing among the additions after the
    /// current song makes it one; landing elsewhere makes it part of the list.
    pub fn move_entry(&mut self, from: usize, to: usize, current: Option<usize>, shuffled: bool) {
        if from >= self.entries.len() || from == to {
            return;
        }
        let current_id = current.and_then(|c| self.id(c));
        let entry = self.entries.remove(from);
        let moved = entry.id;
        let to = to.min(self.entries.len());
        self.entries.insert(to, entry);
        let current = current_id.and_then(|id| self.position(id));
        let added = match (current, to.checked_sub(1).and_then(|p| self.entries.get(p))) {
            (Some(c), Some(before)) if to > c => before.id == self.entries[c].id || before.added,
            _ => false,
        };
        if Some(moved) != current_id {
            self.entries[to].added = added;
        }
        if !shuffled {
            // Unshuffled, the list order is the play order the person made.
            self.context = self.entries.iter().map(|e| e.id).collect();
        }
    }

    /// Everything after `current` goes; `current` stays.
    pub fn clear_after(&mut self, current: usize) {
        self.entries.truncate(current + 1);
        let kept: std::collections::HashSet<u64> = self.entries.iter().map(|e| e.id).collect();
        self.context.retain(|id| kept.contains(id));
    }

    /// Appends to the list (more of a long playlist, or the autoplay radio).
    pub fn extend(&mut self, tracks: Vec<Track>) {
        for track in tracks {
            let entry = self.entry(track, false);
            self.context.push(entry.id);
            self.entries.push(entry);
        }
    }

    /// Shuffles: `current` first, then the additions waiting after it in
    /// their order, then everything else at random. Returns the new position
    /// of `current` (0).
    pub fn shuffle(&mut self, current: usize) -> usize {
        if current >= self.entries.len() {
            return 0;
        }
        let additions = self.additions(current);
        let mut rest: Vec<Entry> = Vec::new();
        let mut head: Vec<Entry> = Vec::new();
        for (i, entry) in std::mem::take(&mut self.entries).into_iter().enumerate() {
            if i >= current && i <= current + additions {
                head.push(entry);
            } else {
                rest.push(entry);
            }
        }
        fastrand::shuffle(&mut rest);
        head.extend(rest);
        self.entries = head;
        0
    }

    /// Back to list order; returns the new position of `current`.
    pub fn unshuffle(&mut self, current: usize) -> usize {
        let current_id = self.id(current);
        let mut by_id: std::collections::HashMap<u64, Entry> = std::mem::take(&mut self.entries)
            .into_iter()
            .map(|e| (e.id, e))
            .collect();
        self.entries = self
            .context
            .iter()
            .filter_map(|id| by_id.remove(id))
            .collect();
        // Anything missing from the list order (none expected) keeps playing too.
        self.entries.extend(by_id.into_values());
        current_id.and_then(|id| self.position(id)).unwrap_or(0)
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            entries: self
                .entries
                .iter()
                .map(|e| SavedEntry {
                    track: e.track.clone(),
                    added: e.added,
                })
                .collect(),
            context: self
                .context
                .iter()
                .filter_map(|&id| self.position(id))
                .collect(),
        }
    }

    /// A saved queue, if it is whole.
    pub fn restore(snapshot: Snapshot) -> Option<Self> {
        let mut queue = Queue::default();
        let n = snapshot.entries.len();
        let mut seen = vec![false; n];
        for &i in &snapshot.context {
            if i >= n || std::mem::replace(&mut seen[i], true) {
                return None;
            }
        }
        if seen.iter().any(|s| !s) {
            return None;
        }
        queue.entries = snapshot
            .entries
            .into_iter()
            .map(|e| queue.entry(e.track, e.added))
            .collect();
        queue.context = snapshot
            .context
            .iter()
            .map(|&i| queue.entries[i].id)
            .collect();
        Some(queue)
    }
}
