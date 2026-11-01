//! Snapshot refs. A `snapshot` numbers the window's elements (`#1`, `#2`, …);
//! the agent (the model) acts with `ref=e<n>`. One book per lane, holding only
//! the LATEST snapshot of that lane (its epoch): a ref from an older snapshot is
//! STALE_REF, the browser channel's rule.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct RefEntry {
    pub index: u32,
    pub role: String,
    pub name: String,
    pub bounds: (f64, f64, f64, f64),
    /// The chained selector Terminator built for the node (window root → node).
    pub selector: Option<String>,
    pub pid: u32,
    pub process: String,
    pub window_title: String,
}

#[derive(Debug, Default, Clone)]
pub struct LaneBook {
    pub epoch: u64,
    pub pid: u32,
    pub process: String,
    pub window_title: String,
    pub entries: HashMap<u32, RefEntry>,
}

#[derive(Debug)]
pub enum RefError {
    BadRef(String),
    Stale { reference: String, epoch: u64 },
}

#[derive(Default)]
pub struct RefBook {
    lanes: Mutex<HashMap<String, LaneBook>>,
    epochs: AtomicU64,
}

/// "e12", "E12", "#12", "12" → 12
pub fn parse_ref(s: &str) -> Option<u32> {
    let t = s.trim().trim_start_matches(['e', 'E', '#']);
    t.parse::<u32>().ok().filter(|n| *n > 0)
}

pub type IndexMap = HashMap<u32, (String, String, (f64, f64, f64, f64), Option<String>)>;

impl RefBook {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the lane's book with this snapshot; returns the new epoch.
    pub fn record(&self, lane: &str, pid: u32, process: &str, window_title: &str, index_map: &IndexMap) -> u64 {
        let epoch = self.epochs.fetch_add(1, Ordering::Relaxed) + 1;
        let mut entries = HashMap::with_capacity(index_map.len());
        for (idx, (role, name, bounds, selector)) in index_map {
            entries.insert(
                *idx,
                RefEntry {
                    index: *idx,
                    role: role.clone(),
                    name: name.clone(),
                    bounds: *bounds,
                    selector: selector.clone(),
                    pid,
                    process: process.to_string(),
                    window_title: window_title.to_string(),
                },
            );
        }
        let book = LaneBook { epoch, pid, process: process.to_string(), window_title: window_title.to_string(), entries };
        self.lanes.lock().unwrap().insert(lane.to_string(), book);
        epoch
    }

    pub fn lane(&self, lane: &str) -> Option<LaneBook> {
        self.lanes.lock().unwrap().get(lane).cloned()
    }

    pub fn resolve(&self, lane: &str, reference: &str) -> Result<RefEntry, RefError> {
        let idx = parse_ref(reference).ok_or_else(|| RefError::BadRef(reference.to_string()))?;
        let lanes = self.lanes.lock().unwrap();
        let book = lanes.get(lane).ok_or_else(|| RefError::Stale { reference: reference.to_string(), epoch: 0 })?;
        book.entries
            .get(&idx)
            .cloned()
            .ok_or_else(|| RefError::Stale { reference: reference.to_string(), epoch: book.epoch })
    }

    /// The ref of an element seen in the lane's latest snapshot with this role and name.
    pub fn find_by_role_name(&self, lane: &str, role: &str, name: &str) -> Option<u32> {
        let lanes = self.lanes.lock().unwrap();
        let book = lanes.get(lane)?;
        book.entries
            .values()
            .find(|e| e.role.eq_ignore_ascii_case(role) && e.name == name)
            .map(|e| e.index)
    }

    pub fn drop_lane(&self, lane: &str) -> usize {
        self.lanes.lock().unwrap().remove(lane).map(|b| b.entries.len()).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refs_resolve_and_go_stale() {
        let book = RefBook::new();
        let mut m: IndexMap = HashMap::new();
        m.insert(3, ("Button".into(), "Save".into(), (1.0, 2.0, 3.0, 4.0), Some("role:Window >> role:Button && name:Save".into())));
        let e1 = book.record("L", 42, "notepad", "Untitled", &m);
        assert_eq!(book.resolve("L", "e3").unwrap().name, "Save");
        assert_eq!(book.resolve("L", "#3").unwrap().pid, 42);
        assert!(matches!(book.resolve("L", "e9"), Err(RefError::Stale { .. })));
        assert!(matches!(book.resolve("L", "zz"), Err(RefError::BadRef(_))));
        assert!(matches!(book.resolve("other", "e3"), Err(RefError::Stale { .. })));
        let e2 = book.record("L", 42, "notepad", "Untitled", &HashMap::new());
        assert!(e2 > e1);
        assert!(matches!(book.resolve("L", "e3"), Err(RefError::Stale { .. })));
        assert_eq!(book.drop_lane("L"), 0);
        assert_eq!(parse_ref("E12"), Some(12));
        assert_eq!(parse_ref("e0"), None);
    }
}
