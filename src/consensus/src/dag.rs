use std::collections::{HashMap, HashSet};
use commputer_core::block::BlockHash;
use commputer_core::proof::ResourceChannel;
use commputer_core::identity::Address;

/// A vertex in the multi-channel DAG.
/// Each resource channel produces vertices at its own rate.
/// Vertices reference parents within their channel and cross-channel.
#[derive(Debug, Clone)]
pub struct DagVertex {
    /// Hash of this vertex.
    pub hash: BlockHash,
    /// Which resource channel produced this vertex.
    pub channel: ResourceChannel,
    /// Height within this channel's sub-DAG.
    pub channel_height: u64,
    /// Parent vertices within the same channel.
    pub channel_parents: Vec<BlockHash>,
    /// Cross-channel references (one per other active channel).
    pub cross_refs: Vec<BlockHash>,
    /// The validator that produced this vertex.
    pub producer: Address,
    /// Timestamp (unix millis).
    pub timestamp_ms: u64,
    /// Payload hash (proof data, transactions, etc.).
    pub payload_hash: [u8; 32],
    /// Snowball finality status.
    pub finalized: bool,
}

/// The multi-channel DAG.
/// Five channels produce vertices independently, cross-referencing each other.
/// This avoids bottlenecking on the slowest proof type.
#[derive(Debug)]
pub struct Dag {
    /// All vertices by hash.
    vertices: HashMap<BlockHash, DagVertex>,
    /// Tips (latest unfinalized vertices) per channel.
    tips: HashMap<ResourceChannel, Vec<BlockHash>>,
    /// Finalized vertices in causal order.
    finalized_order: Vec<BlockHash>,
}

impl Dag {
    /// Create an empty DAG with tip tracking for all five channels.
    pub fn new() -> Self {
        let mut tips = HashMap::new();
        for channel in ResourceChannel::ALL {
            tips.insert(channel, Vec::new());
        }
        Self {
            vertices: HashMap::new(),
            tips,
            finalized_order: Vec::new(),
        }
    }

    /// Insert a new vertex into the DAG.
    /// Returns an error if parents are unknown.
    pub fn insert(&mut self, vertex: DagVertex) -> Result<(), DagError> {
        // Verify all parents exist.
        for parent in &vertex.channel_parents {
            if !self.vertices.contains_key(parent) {
                return Err(DagError::UnknownParent(*parent));
            }
        }
        for xref in &vertex.cross_refs {
            if !self.vertices.contains_key(xref) {
                return Err(DagError::UnknownParent(*xref));
            }
        }

        let hash = vertex.hash;
        let channel = vertex.channel;

        // Remove parents from tips (they're no longer tips if they have a child).
        if let Some(channel_tips) = self.tips.get_mut(&channel) {
            channel_tips.retain(|t| !vertex.channel_parents.contains(t));
            channel_tips.push(hash);
        }

        self.vertices.insert(hash, vertex);
        Ok(())
    }

    /// Get a vertex by hash.
    pub fn get(&self, hash: &BlockHash) -> Option<&DagVertex> {
        self.vertices.get(hash)
    }

    /// Get current tips for a channel.
    pub fn tips(&self, channel: &ResourceChannel) -> &[BlockHash] {
        self.tips.get(channel).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Get all current tips across all channels.
    pub fn all_tips(&self) -> Vec<BlockHash> {
        self.tips.values().flat_map(|v| v.iter().copied()).collect()
    }

    /// Mark a vertex as finalized (decided by Snowball).
    pub fn finalize(&mut self, hash: &BlockHash) -> bool {
        if let Some(vertex) = self.vertices.get_mut(hash)
            && !vertex.finalized {
                vertex.finalized = true;
                self.finalized_order.push(*hash);
                return true;
            }
        false
    }

    /// Get the finalized chain in causal order.
    pub fn finalized_chain(&self) -> &[BlockHash] {
        &self.finalized_order
    }

    /// Total number of vertices in the DAG.
    /// Total number of vertices in the DAG.
    pub fn len(&self) -> usize {
        self.vertices.len()
    }

    /// Returns true if the DAG contains no vertices.
    pub fn is_empty(&self) -> bool {
        self.vertices.is_empty()
    }

    /// Get ancestors of a vertex up to a given depth.
    pub fn ancestors(&self, hash: &BlockHash, max_depth: usize) -> HashSet<BlockHash> {
        let mut result = HashSet::new();
        let mut frontier = vec![*hash];
        let mut depth = 0;

        while depth < max_depth && !frontier.is_empty() {
            let mut next_frontier = Vec::new();
            for h in &frontier {
                if let Some(v) = self.vertices.get(h) {
                    for parent in &v.channel_parents {
                        if result.insert(*parent) {
                            next_frontier.push(*parent);
                        }
                    }
                    for xref in &v.cross_refs {
                        if result.insert(*xref) {
                            next_frontier.push(*xref);
                        }
                    }
                }
            }
            frontier = next_frontier;
            depth += 1;
        }

        result
    }
}

impl Default for Dag {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DagError {
    #[error("unknown parent vertex: {0}")]
    UnknownParent(BlockHash),
    #[error("duplicate vertex: {0}")]
    Duplicate(BlockHash),
}

#[cfg(test)]
mod tests {
    use super::*;
    use commputer_core::identity::Address;

    fn make_vertex(
        n: u8,
        channel: ResourceChannel,
        height: u64,
        parents: Vec<BlockHash>,
    ) -> DagVertex {
        let mut hash = [0u8; 32];
        hash[0] = n;
        DagVertex {
            hash: BlockHash(hash),
            channel,
            channel_height: height,
            channel_parents: parents,
            cross_refs: Vec::new(),
            producer: Address([0u8; 32]),
            timestamp_ms: 0,
            payload_hash: [0u8; 32],
            finalized: false,
        }
    }

    #[test]
    fn insert_and_tip_tracking() {
        let mut dag = Dag::new();
        let v1 = make_vertex(1, ResourceChannel::Processing, 0, vec![]);
        let h1 = v1.hash;
        dag.insert(v1).unwrap();

        assert_eq!(dag.tips(&ResourceChannel::Processing), &[h1]);

        let v2 = make_vertex(2, ResourceChannel::Processing, 1, vec![h1]);
        let h2 = v2.hash;
        dag.insert(v2).unwrap();

        // v1 is no longer a tip, v2 is.
        assert_eq!(dag.tips(&ResourceChannel::Processing), &[h2]);
    }

    #[test]
    fn cross_channel_refs() {
        let mut dag = Dag::new();
        let v_cpu = make_vertex(1, ResourceChannel::Processing, 0, vec![]);
        let h_cpu = v_cpu.hash;
        dag.insert(v_cpu).unwrap();

        let mut v_gpu = make_vertex(2, ResourceChannel::Gpu, 0, vec![]);
        v_gpu.cross_refs = vec![h_cpu]; // GPU vertex references CPU vertex.
        dag.insert(v_gpu).unwrap();

        assert_eq!(dag.len(), 2);
    }

    #[test]
    fn unknown_parent_rejected() {
        let mut dag = Dag::new();
        let fake_parent = BlockHash([99u8; 32]);
        let v = make_vertex(1, ResourceChannel::Storage, 0, vec![fake_parent]);
        assert!(dag.insert(v).is_err());
    }

    #[test]
    fn finalization() {
        let mut dag = Dag::new();
        let v = make_vertex(1, ResourceChannel::Ram, 0, vec![]);
        let h = v.hash;
        dag.insert(v).unwrap();

        assert!(dag.finalize(&h));
        assert!(!dag.finalize(&h)); // Already finalized.
        assert_eq!(dag.finalized_chain(), &[h]);
    }
}
