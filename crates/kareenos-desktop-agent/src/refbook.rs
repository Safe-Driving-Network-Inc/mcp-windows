//! Snapshot refs. A `snapshot` numbers the window's elements (`#1`, `#2`, …);
//! the agent (the model) acts with `ref=e<n>`. One book per lane, holding only
//! the LATEST snapshot of that lane (its epoch): a ref from an older snapshot is
//! STALE_REF, the browser channel's rule.
//!
//! A ref is turned back into a live element by its role/name path AND its place.
//! Terminator's path is the same for unnamed siblings (five `[Edit]` fields of one
//! form all read `… >> role:Edit`), so the path alone always lands on the first of
//! them — the 2026-11-02 dogfood bug where every value of a form went into its
//! first box. `pick_candidate` is the rule that chooses among the live matches.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// (x, y, width, height) in screen pixels, as the snapshot printed them.
pub type Bounds = (f64, f64, f64, f64);

/// Two bounds are "the same place" when every side is within this many pixels.
pub const BOUNDS_TOLERANCE_PX: f64 = 2.0;

#[derive(Debug, Clone)]
pub struct RefEntry {
    pub index: u32,
    pub role: String,
    pub name: String,
    pub bounds: Bounds,
    /// The chained selector Terminator built for the node (window root → node).
    pub selector: Option<String>,
    pub pid: u32,
    pub process: String,
    pub window_title: String,
}

/// What a snapshot was taken in: the refs' home.
#[derive(Debug, Default, Clone)]
pub struct Root {
    pub pid: u32,
    pub process: String,
    pub window_title: String,
    /// Native handle of the window (None when unknown).
    pub hwnd: Option<isize>,
    /// The window's bounds at snapshot time: a later move shifts every ref by the same delta.
    pub window_bounds: Option<Bounds>,
}

#[derive(Debug, Default, Clone)]
pub struct LaneBook {
    pub epoch: u64,
    pub pid: u32,
    pub process: String,
    pub window_title: String,
    pub hwnd: Option<isize>,
    pub window_bounds: Option<Bounds>,
    pub entries: HashMap<u32, RefEntry>,
}

impl LaneBook {
    /// Where the ref sits among this snapshot's refs of the same role and name, in
    /// snapshot (tree) order, and how many there are. The fallback key when the
    /// window scrolled or re-laid out: the control is still the n-th of its kind.
    pub fn ordinal(&self, index: u32) -> Option<(usize, usize)> {
        let me = self.entries.get(&index)?;
        let mut same: Vec<u32> = self
            .entries
            .values()
            .filter(|e| e.role.eq_ignore_ascii_case(&me.role) && e.name == me.name)
            .map(|e| e.index)
            .collect();
        same.sort_unstable();
        let pos = same.iter().position(|i| *i == index)?;
        Some((pos, same.len()))
    }
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

pub fn same_size(a: Bounds, b: Bounds) -> bool {
    (a.2 - b.2).abs() <= BOUNDS_TOLERANCE_PX && (a.3 - b.3).abs() <= BOUNDS_TOLERANCE_PX
}

pub fn same_place(a: Bounds, b: Bounds) -> bool {
    (a.0 - b.0).abs() <= BOUNDS_TOLERANCE_PX && (a.1 - b.1).abs() <= BOUNDS_TOLERANCE_PX && same_size(a, b)
}

/// Among the live elements that match a ref's role and name inside its window
/// (`candidates` in tree order, bounds when readable), the one the snapshot
/// numbered — None when none is at the ref's place (the caller answers STALE_REF).
/// `stored` are the ref's bounds at snapshot time, `shift` how far the window moved
/// since (None = unknown), `ordinal` the ref's position among the snapshot's refs
/// of the same role and name. In order: a single candidate; the candidate at the
/// stored place (where the window is now, then where it was); the n-th of its kind
/// when it still has the stored size (scrolled or re-laid out); nothing.
pub fn pick_candidate(stored: Bounds, shift: Option<(f64, f64)>, ordinal: Option<usize>, candidates: &[Option<Bounds>]) -> Option<usize> {
    if candidates.is_empty() {
        return None;
    }
    if candidates.len() == 1 {
        return Some(0);
    }
    let mut places = vec![stored];
    if let Some((dx, dy)) = shift {
        if dx.abs() > BOUNDS_TOLERANCE_PX || dy.abs() > BOUNDS_TOLERANCE_PX {
            places.insert(0, (stored.0 + dx, stored.1 + dy, stored.2, stored.3));
        }
    }
    for place in places {
        let hits: Vec<usize> = candidates
            .iter()
            .enumerate()
            .filter(|(_, c)| c.map(|c| same_place(c, place)).unwrap_or(false))
            .map(|(i, _)| i)
            .collect();
        match hits.len() {
            0 => continue,
            1 => return Some(hits[0]),
            _ => return Some(ordinal.filter(|o| hits.contains(o)).unwrap_or(hits[0])),
        }
    }
    let o = ordinal?;
    match candidates.get(o) {
        Some(Some(c)) if same_size(*c, stored) => Some(o),
        _ => None,
    }
}

pub type IndexMap = HashMap<u32, (String, String, Bounds, Option<String>)>;

impl RefBook {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the lane's book with this snapshot; returns the new epoch.
    pub fn record(&self, lane: &str, root: &Root, index_map: &IndexMap) -> u64 {
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
                    pid: root.pid,
                    process: root.process.clone(),
                    window_title: root.window_title.clone(),
                },
            );
        }
        let book = LaneBook {
            epoch,
            pid: root.pid,
            process: root.process.clone(),
            window_title: root.window_title.clone(),
            hwnd: root.hwnd,
            window_bounds: root.window_bounds,
            entries,
        };
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

    /// The ref of an element seen in the lane's latest snapshot: the one at these
    /// bounds when they are known (unnamed siblings share role and name), else the
    /// first with this role and name.
    pub fn find_ref(&self, lane: &str, role: &str, name: &str, bounds: Option<Bounds>) -> Option<u32> {
        let lanes = self.lanes.lock().unwrap();
        let book = lanes.get(lane)?;
        let mut same: Vec<&RefEntry> =
            book.entries.values().filter(|e| e.role.eq_ignore_ascii_case(role) && e.name == name).collect();
        same.sort_unstable_by_key(|e| e.index);
        if let Some(b) = bounds {
            if let Some(e) = same.iter().find(|e| same_place(e.bounds, b)) {
                return Some(e.index);
            }
        }
        same.first().map(|e| e.index)
    }

    pub fn drop_lane(&self, lane: &str) -> usize {
        self.lanes.lock().unwrap().remove(lane).map(|b| b.entries.len()).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> Root {
        Root { pid: 42, process: "notepad".into(), window_title: "Untitled".into(), hwnd: Some(7), window_bounds: Some((100.0, 100.0, 800.0, 600.0)) }
    }

    #[test]
    fn refs_resolve_and_go_stale() {
        let book = RefBook::new();
        let mut m: IndexMap = HashMap::new();
        m.insert(3, ("Button".into(), "Save".into(), (1.0, 2.0, 3.0, 4.0), Some("role:Window >> role:Button && name:Save".into())));
        let e1 = book.record("L", &root(), &m);
        assert_eq!(book.resolve("L", "e3").unwrap().name, "Save");
        assert_eq!(book.resolve("L", "#3").unwrap().pid, 42);
        assert!(matches!(book.resolve("L", "e9"), Err(RefError::Stale { .. })));
        assert!(matches!(book.resolve("L", "zz"), Err(RefError::BadRef(_))));
        assert!(matches!(book.resolve("other", "e3"), Err(RefError::Stale { .. })));
        let lane = book.lane("L").unwrap();
        assert_eq!(lane.hwnd, Some(7));
        assert_eq!(lane.window_bounds, Some((100.0, 100.0, 800.0, 600.0)));
        let e2 = book.record("L", &root(), &HashMap::new());
        assert!(e2 > e1);
        assert!(matches!(book.resolve("L", "e3"), Err(RefError::Stale { .. })));
        assert_eq!(book.drop_lane("L"), 0);
        assert_eq!(parse_ref("E12"), Some(12));
        assert_eq!(parse_ref("e0"), None);
    }

    /// The New company form of 2026-11-02: five unnamed [Edit] fields sharing one path.
    fn form() -> (RefBook, Vec<Bounds>) {
        let fields: Vec<Bounds> = vec![
            (1201.0, 598.0, 959.0, 64.0),
            (1201.0, 721.0, 959.0, 64.0),
            (1201.0, 843.0, 959.0, 64.0),
            (1201.0, 966.0, 959.0, 64.0),
            (1201.0, 1088.0, 959.0, 201.0),
        ];
        let mut m: IndexMap = HashMap::new();
        m.insert(1, ("Window".into(), "New company".into(), (1140.0, 362.0, 1081.0, 1281.0), Some("role:Window && name:New company".into())));
        for (i, b) in fields.iter().enumerate() {
            m.insert(5 + i as u32, ("Edit".into(), String::new(), *b, Some("role:Window && name:New company >> role:Edit".into())));
        }
        m.insert(10, ("Button".into(), "Cancel".into(), (1898.0, 1510.0, 137.0, 72.0), Some("role:Window && name:New company >> role:Button && name:Cancel".into())));
        let book = RefBook::new();
        book.record("L", &Root { pid: 13900, process: "KimuraCrm".into(), window_title: "New company".into(), hwnd: Some(525450), window_bounds: Some((1140.0, 362.0, 1081.0, 1281.0)) }, &m);
        (book, fields)
    }

    #[test]
    fn unnamed_siblings_keep_their_place() {
        let (book, fields) = form();
        let lane = book.lane("L").unwrap();
        // e5..e9 are the 1st..5th Edit of the snapshot; the button is the only one of its kind.
        assert_eq!(lane.ordinal(5), Some((0, 5)));
        assert_eq!(lane.ordinal(7), Some((2, 5)));
        assert_eq!(lane.ordinal(9), Some((4, 5)));
        assert_eq!(lane.ordinal(10), Some((0, 1)));
        assert_eq!(lane.ordinal(99), None);
        // find_ref: by bounds when known (the Phone field is e7, not e5), else the first of the kind.
        assert_eq!(book.find_ref("L", "Edit", "", Some(fields[2])), Some(7));
        assert_eq!(book.find_ref("L", "edit", "", None), Some(5));
        assert_eq!(book.find_ref("L", "Button", "Cancel", None), Some(10));
        assert_eq!(book.find_ref("L", "Button", "Save", None), None);

        let live: Vec<Option<Bounds>> = fields.iter().map(|b| Some(*b)).collect();
        // The window did not move: each ref picks its own field — the bug was picking index 0 for all.
        for (i, f) in fields.iter().enumerate() {
            assert_eq!(pick_candidate(*f, Some((0.0, 0.0)), Some(i), &live), Some(i), "field {i}");
            assert_eq!(pick_candidate(*f, None, None, &live), Some(i), "field {i} without context");
        }
        // The window moved by (+300, -40): every live field shifted; the shift finds the right one.
        let moved: Vec<Option<Bounds>> = fields.iter().map(|b| Some((b.0 + 300.0, b.1 - 40.0, b.2, b.3))).collect();
        assert_eq!(pick_candidate(fields[2], Some((300.0, -40.0)), Some(2), &moved), Some(2));
        // Unknown shift (window moved, bounds unknown): the n-th of its kind with the same size.
        assert_eq!(pick_candidate(fields[2], None, Some(2), &moved), Some(2));
        assert_eq!(pick_candidate(fields[4], None, Some(4), &moved), Some(4));
        // The n-th of its kind has another size now (the form re-laid out): stale.
        let relaid: Vec<Option<Bounds>> = vec![Some((0.0, 0.0, 100.0, 30.0)), Some((0.0, 50.0, 100.0, 30.0)), Some((0.0, 100.0, 100.0, 30.0))];
        assert_eq!(pick_candidate(fields[2], None, Some(2), &relaid), None);
        // No ordinal and nothing at the place: stale; one live match is always it; none is none.
        assert_eq!(pick_candidate(fields[2], None, None, &moved), None);
        assert_eq!(pick_candidate(fields[2], None, None, &live[1..2]), Some(0));
        assert_eq!(pick_candidate(fields[2], None, Some(2), &[]), None);
        // Two live elements at the same place (stacked duplicates): the ordinal decides, else the first.
        let stacked: Vec<Option<Bounds>> = vec![Some(fields[0]), Some(fields[1]), Some(fields[1]), None];
        assert_eq!(pick_candidate(fields[1], None, Some(2), &stacked), Some(2));
        assert_eq!(pick_candidate(fields[1], None, Some(0), &stacked), Some(1));
        assert_eq!(pick_candidate(fields[1], None, None, &stacked), Some(1));
        // A pixel of rounding is still the same place; more is not.
        assert!(same_place((10.0, 10.0, 50.0, 20.0), (11.0, 9.0, 51.0, 19.0)));
        assert!(!same_place((10.0, 10.0, 50.0, 20.0), (13.0, 10.0, 50.0, 20.0)));
    }
}
