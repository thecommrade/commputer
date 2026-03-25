use serde::{Deserialize, Serialize};
use std::collections::BinaryHeap;
use std::cmp::Ordering;

/// An entry in the prioritized job queue.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct QueueEntry {
    pub job_id: [u8; 32],
    /// Flagship L2 jobs get priority over regular jobs.
    pub is_flagship: bool,
    /// Budget per resource unit -- higher means higher priority.
    pub budget_per_resource_unit: u64,
    /// Insertion sequence for FIFO tie-breaking.
    pub sequence: u64,
}

impl Ord for QueueEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // Flagship jobs always come first
        match (self.is_flagship, other.is_flagship) {
            (true, false) => return Ordering::Greater,
            (false, true) => return Ordering::Less,
            _ => {}
        }
        // Then by budget per resource unit (higher is better)
        match self.budget_per_resource_unit.cmp(&other.budget_per_resource_unit) {
            Ordering::Equal => {}
            ord => return ord,
        }
        // Tie-break: lower sequence (earlier submission) wins
        other.sequence.cmp(&self.sequence)
    }
}

impl PartialOrd for QueueEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A priority queue for compute jobs.
/// Flagship jobs are dequeued first, then by budget/resource ratio (descending).
#[derive(Debug, Default)]
pub struct PrioritizedJobQueue {
    heap: BinaryHeap<QueueEntry>,
    next_sequence: u64,
}

impl PrioritizedJobQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a job to the queue.
    pub fn enqueue(
        &mut self,
        job_id: [u8; 32],
        is_flagship: bool,
        budget_per_resource_unit: u64,
    ) {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        self.heap.push(QueueEntry {
            job_id,
            is_flagship,
            budget_per_resource_unit,
            sequence,
        });
    }

    /// Remove and return the highest-priority job ID.
    pub fn dequeue(&mut self) -> Option<[u8; 32]> {
        self.heap.pop().map(|e| e.job_id)
    }

    /// Peek at the highest-priority job without removing it.
    pub fn peek(&self) -> Option<&QueueEntry> {
        self.heap.peek()
    }

    /// Number of jobs in the queue.
    pub fn len(&self) -> usize {
        self.heap.len()
    }

    /// Whether the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fifo_same_priority() {
        let mut q = PrioritizedJobQueue::new();
        q.enqueue([1u8; 32], false, 100);
        q.enqueue([2u8; 32], false, 100);
        q.enqueue([3u8; 32], false, 100);
        assert_eq!(q.dequeue().unwrap(), [1u8; 32]);
        assert_eq!(q.dequeue().unwrap(), [2u8; 32]);
        assert_eq!(q.dequeue().unwrap(), [3u8; 32]);
    }

    #[test]
    fn test_flagship_first() {
        let mut q = PrioritizedJobQueue::new();
        q.enqueue([1u8; 32], false, 1000);
        q.enqueue([2u8; 32], true, 1);
        // Flagship should come first despite lower budget
        assert_eq!(q.dequeue().unwrap(), [2u8; 32]);
        assert_eq!(q.dequeue().unwrap(), [1u8; 32]);
    }

    #[test]
    fn test_higher_budget_first() {
        let mut q = PrioritizedJobQueue::new();
        q.enqueue([1u8; 32], false, 10);
        q.enqueue([2u8; 32], false, 100);
        q.enqueue([3u8; 32], false, 50);
        assert_eq!(q.dequeue().unwrap(), [2u8; 32]);
        assert_eq!(q.dequeue().unwrap(), [3u8; 32]);
        assert_eq!(q.dequeue().unwrap(), [1u8; 32]);
    }

    #[test]
    fn test_empty_queue() {
        let mut q = PrioritizedJobQueue::new();
        assert!(q.dequeue().is_none());
        assert!(q.is_empty());
    }

    #[test]
    fn test_len() {
        let mut q = PrioritizedJobQueue::new();
        assert_eq!(q.len(), 0);
        q.enqueue([1u8; 32], false, 10);
        q.enqueue([2u8; 32], false, 20);
        assert_eq!(q.len(), 2);
        q.dequeue();
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn test_peek() {
        let mut q = PrioritizedJobQueue::new();
        q.enqueue([1u8; 32], false, 10);
        q.enqueue([2u8; 32], true, 5);
        let top = q.peek().unwrap();
        assert!(top.is_flagship);
        assert_eq!(top.job_id, [2u8; 32]);
        // peek doesn't remove
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn test_mixed_flagship_and_budget() {
        let mut q = PrioritizedJobQueue::new();
        q.enqueue([1u8; 32], false, 100);
        q.enqueue([2u8; 32], true, 10);
        q.enqueue([3u8; 32], true, 50);
        q.enqueue([4u8; 32], false, 200);
        // Flagships first (by budget within flagship tier)
        assert_eq!(q.dequeue().unwrap(), [3u8; 32]); // flagship, budget 50
        assert_eq!(q.dequeue().unwrap(), [2u8; 32]); // flagship, budget 10
        // Then regular by budget
        assert_eq!(q.dequeue().unwrap(), [4u8; 32]); // regular, budget 200
        assert_eq!(q.dequeue().unwrap(), [1u8; 32]); // regular, budget 100
    }
}
