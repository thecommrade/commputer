use anyhow::Result;
use serde_json::Value;

/// Fetch and display a list of compute jobs from the running node's RPC.
pub async fn cmd_jobs_list(rpc_port: u16) -> Result<()> {
    let url = format!("http://127.0.0.1:{}/jobs", rpc_port);
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await?;

    let body: Value = resp.json().await?;
    let jobs = body.get("jobs").and_then(|j| j.as_array());

    match jobs {
        Some(arr) if !arr.is_empty() => {
            println!("{:<20} {:<12} {:<10} {:<10} {:<8}", "JOB ID", "STATUS", "BUDGET", "CPU", "GPU MB");
            println!("{}", "-".repeat(64));
            for job in arr {
                let id = job.get("job_id_hex").and_then(|v| v.as_str()).unwrap_or("?");
                let status = job.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                let budget = job.get("comme_budget").and_then(|v| v.as_u64()).unwrap_or(0);
                let cpu = job.get("cpu_cores").and_then(|v| v.as_u64()).unwrap_or(0);
                let gpu = job.get("gpu_vram_mb").and_then(|v| v.as_u64()).unwrap_or(0);
                println!("{:<20} {:<12} {:<10} {:<10} {:<8}", id, status, budget, cpu, gpu);
            }
            let total = body.get("total_count").and_then(|v| v.as_u64()).unwrap_or(0);
            let pending = body.get("pending_count").and_then(|v| v.as_u64()).unwrap_or(0);
            let active = body.get("active_count").and_then(|v| v.as_u64()).unwrap_or(0);
            println!("\nTotal: {}  Pending: {}  Active: {}", total, pending, active);
        }
        _ => {
            println!("No compute jobs found.");
        }
    }
    Ok(())
}

/// Submit a new compute job via RPC.
pub async fn cmd_jobs_submit(rpc_port: u16, spec_json: &str) -> Result<()> {
    let url = format!("http://127.0.0.1:{}/jobs/submit", rpc_port);
    let client = reqwest::Client::new();

    // Parse and validate the spec JSON
    let spec: Value = serde_json::from_str(spec_json)
        .map_err(|e| anyhow::anyhow!("Invalid job spec JSON: {}", e))?;

    let resp = client
        .post(&url)
        .json(&spec)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await?;

    let body: Value = resp.json().await?;
    let accepted = body.get("accepted").and_then(|v| v.as_bool()).unwrap_or(false);

    if accepted {
        let job_id = body.get("job_id_hex").and_then(|v| v.as_str()).unwrap_or("unknown");
        println!("Job submitted successfully.");
        println!("Job ID: {}", job_id);
    } else {
        let error = body.get("error").and_then(|v| v.as_str()).unwrap_or("Unknown error");
        println!("Job submission failed: {}", error);
    }
    Ok(())
}

/// Query the status of a specific compute job via RPC.
pub async fn cmd_jobs_status(rpc_port: u16, job_id: &str) -> Result<()> {
    let url = format!("http://127.0.0.1:{}/jobs/{}", rpc_port, job_id);
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await?;

    let body: Value = resp.json().await?;
    let status = body.get("status").and_then(|v| v.as_str()).unwrap_or("unknown");
    let result_hash = body.get("result_hash").and_then(|v| v.as_str());
    let executor = body.get("executor_hex").and_then(|v| v.as_str());

    println!("Job:      {}", job_id);
    println!("Status:   {}", status);
    if let Some(hash) = result_hash {
        println!("Result:   {}", hash);
    }
    if let Some(exec) = executor {
        println!("Executor: {}", exec);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_format_list() {
        let port = 9944u16;
        let url = format!("http://127.0.0.1:{}/jobs", port);
        assert_eq!(url, "http://127.0.0.1:9944/jobs");
    }

    #[test]
    fn test_url_format_submit() {
        let port = 9944u16;
        let url = format!("http://127.0.0.1:{}/jobs/submit", port);
        assert_eq!(url, "http://127.0.0.1:9944/jobs/submit");
    }

    #[test]
    fn test_url_format_status() {
        let port = 9944u16;
        let job_id = "abc123";
        let url = format!("http://127.0.0.1:{}/jobs/{}", port, job_id);
        assert_eq!(url, "http://127.0.0.1:9944/jobs/abc123");
    }

    #[test]
    fn test_parse_valid_spec_json() {
        let spec = r#"{"cpu_cores": 4, "ram_mb": 1024}"#;
        let parsed: Result<Value, _> = serde_json::from_str(spec);
        assert!(parsed.is_ok());
    }

    #[test]
    fn test_parse_invalid_spec_json() {
        let spec = "not json at all{{{";
        let parsed: Result<Value, _> = serde_json::from_str(spec);
        assert!(parsed.is_err());
    }
}
