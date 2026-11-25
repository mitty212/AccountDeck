use std::collections::HashSet;

#[derive(Default)]
pub struct SnapshotQueue {
    pending: HashSet<String>,
    final_required: HashSet<String>,
}
impl SnapshotQueue {
    pub fn contains(&self, id: &str) -> bool {
        self.pending.contains(id)
    }
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
    pub fn request(&mut self, id: &str, final_save: bool) -> bool {
        if !self.pending.insert(id.into()) {
            if final_save {
                self.final_required.insert(id.into());
            }
            false
        } else {
            true
        }
    }
    pub fn completed(&mut self, id: &str) -> bool {
        self.pending.remove(id);
        self.final_required.remove(id)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn final_save_after_inflight_periodic_requires_new_capture() {
        let mut q = SnapshotQueue::default();
        assert!(q.request("A", false));
        assert!(!q.request("A", true));
        assert!(q.completed("A"));
        assert!(q.request("A", false));
        assert!(!q.completed("A"));
        assert!(q.is_empty());
    }
    #[test]
    fn final_captures_remain_independent_per_profile() {
        let mut q = SnapshotQueue::default();
        assert!(q.request("A", true));
        assert!(q.request("B", false));
        assert!(!q.request("B", true));
        assert!(!q.completed("A"));
        assert!(q.contains("B"));
        assert!(q.completed("B"));
    }
}
