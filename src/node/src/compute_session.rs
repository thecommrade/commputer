#![allow(dead_code)]
use std::collections::HashMap;
use serde::{Deserialize, Serialize};

/// Resource allocation for a compute session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceAlloc {
    pub cpu_cores: u16,
    pub gpu_vram_mb: u64,
    pub ram_mb: u64,
}

/// A persistent compute session that can hold multiple jobs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputeSession {
    pub session_id: String,
    pub holder_address: String,
    pub resource_allocation: ResourceAlloc,
    pub created_at: u64,
    pub expires_at: u64,
    pub jobs: Vec<String>,
}

/// Streaming result chunk from a long-running compute job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamingResult {
    pub job_id: String,
    pub chunk_index: u64,
    pub data: Vec<u8>,
    pub is_final: bool,
}

/// Manages active compute sessions.
pub struct SessionManager {
    pub sessions: HashMap<String, ComputeSession>,
    next_id: u64,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            next_id: 1,
        }
    }

    /// Create a new compute session, returns session_id.
    pub fn create_session(
        &mut self,
        holder: &str,
        resources: ResourceAlloc,
        duration_secs: u64,
        current_time: u64,
    ) -> String {
        let session_id = format!("session-{}", self.next_id);
        self.next_id += 1;
        let session = ComputeSession {
            session_id: session_id.clone(),
            holder_address: holder.to_string(),
            resource_allocation: resources,
            created_at: current_time,
            expires_at: current_time + duration_secs,
            jobs: Vec::new(),
        };
        self.sessions.insert(session_id.clone(), session);
        session_id
    }

    /// Scale a session's resources up or down.
    pub fn scale_session(
        &mut self,
        session_id: &str,
        new_resources: ResourceAlloc,
    ) -> Result<(), String> {
        match self.sessions.get_mut(session_id) {
            Some(session) => {
                session.resource_allocation = new_resources;
                Ok(())
            }
            None => Err(format!("Session {} not found", session_id)),
        }
    }

    /// Close and remove a session.
    pub fn close_session(&mut self, session_id: &str) -> Result<(), String> {
        match self.sessions.remove(session_id) {
            Some(_) => Ok(()),
            None => Err(format!("Session {} not found", session_id)),
        }
    }

    /// Add a job to an existing session.
    pub fn add_job(&mut self, session_id: &str, job_id: &str) -> Result<(), String> {
        match self.sessions.get_mut(session_id) {
            Some(session) => {
                session.jobs.push(job_id.to_string());
                Ok(())
            }
            None => Err(format!("Session {} not found", session_id)),
        }
    }

    /// Get a session by ID.
    pub fn get_session(&self, session_id: &str) -> Option<&ComputeSession> {
        self.sessions.get(session_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_session() {
        let mut mgr = SessionManager::new();
        let resources = ResourceAlloc {
            cpu_cores: 4,
            gpu_vram_mb: 8192,
            ram_mb: 16384,
        };
        let sid = mgr.create_session("alice", resources, 3600, 1000);
        assert!(sid.starts_with("session-"));
        assert_eq!(mgr.sessions.len(), 1);

        let session = mgr.get_session(&sid).unwrap();
        assert_eq!(session.holder_address, "alice");
        assert_eq!(session.expires_at, 4600);
    }

    #[test]
    fn test_scale_session() {
        let mut mgr = SessionManager::new();
        let resources = ResourceAlloc {
            cpu_cores: 2,
            gpu_vram_mb: 4096,
            ram_mb: 8192,
        };
        let sid = mgr.create_session("bob", resources, 1800, 500);

        let new_resources = ResourceAlloc {
            cpu_cores: 8,
            gpu_vram_mb: 16384,
            ram_mb: 32768,
        };
        assert!(mgr.scale_session(&sid, new_resources).is_ok());

        let session = mgr.get_session(&sid).unwrap();
        assert_eq!(session.resource_allocation.cpu_cores, 8);
    }

    #[test]
    fn test_close_session() {
        let mut mgr = SessionManager::new();
        let resources = ResourceAlloc {
            cpu_cores: 1,
            gpu_vram_mb: 0,
            ram_mb: 1024,
        };
        let sid = mgr.create_session("charlie", resources, 600, 0);
        assert_eq!(mgr.sessions.len(), 1);
        assert!(mgr.close_session(&sid).is_ok());
        assert_eq!(mgr.sessions.len(), 0);
        assert!(mgr.close_session(&sid).is_err());
    }

    #[test]
    fn test_add_job_to_session() {
        let mut mgr = SessionManager::new();
        let resources = ResourceAlloc {
            cpu_cores: 4,
            gpu_vram_mb: 0,
            ram_mb: 4096,
        };
        let sid = mgr.create_session("dave", resources, 3600, 100);
        mgr.add_job(&sid, "job-1").unwrap();
        mgr.add_job(&sid, "job-2").unwrap();

        let session = mgr.get_session(&sid).unwrap();
        assert_eq!(session.jobs.len(), 2);
    }

    #[test]
    fn test_streaming_result() {
        let chunk = StreamingResult {
            job_id: "job-abc".to_string(),
            chunk_index: 0,
            data: vec![1, 2, 3, 4],
            is_final: false,
        };
        assert!(!chunk.is_final);
        assert_eq!(chunk.chunk_index, 0);

        let final_chunk = StreamingResult {
            job_id: "job-abc".to_string(),
            chunk_index: 5,
            data: vec![],
            is_final: true,
        };
        assert!(final_chunk.is_final);
    }
}
