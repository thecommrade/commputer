// checks/port_reachability.rs — pre-flight port-bind probes
//
// WHAT IT DOES:
//   Best-effort TCP `bind` test: if we can bind the configured P2P/RPC port
//   right now, the node should be able to bind it 5 seconds from now.
//   Catches the common "stale node still running / port already taken"
//   failure mode before the real node starts and fights for the socket.
//
// WHERE IT SHOULD GO:
//   src/doctor/src/checks/port_reachability.rs
//
// WIRING REQUIRED:
//   None — uses only stdlib.

use std::net::TcpListener;

use crate::{CheckResult, Severity};

pub fn check_p2p_port(port: u16) -> CheckResult {
    bind_test("p2p", port, "0.0.0.0")
}

pub fn check_rpc_port(port: u16) -> CheckResult {
    bind_test("rpc", port, "127.0.0.1")
}

fn bind_test(label: &str, port: u16, bind_addr: &str) -> CheckResult {
    let addr = format!("{}:{}", bind_addr, port);
    match TcpListener::bind(&addr) {
        Ok(listener) => {
            // Drop closes immediately — release for the real node.
            drop(listener);
            CheckResult::ok(
                &format!("net.port.{}", label),
                format!("port {} ({}) is free to bind", port, label),
            )
        }
        Err(e) => CheckResult {
            check: format!("net.port.{}", label),
            severity: Severity::Error,
            message: format!("cannot bind {}: {}", addr, e),
            suggestion: Some(format!(
                "another process is using port {}; run `ss -ltnp | grep :{}` or `lsof -i :{}`",
                port, port, port
            )),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener as Tl;

    #[test]
    fn free_port_passes() {
        // Pick a free port via OS, drop, re-test.
        let l = Tl::bind("0.0.0.0:0").unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        let r = check_p2p_port(port);
        assert_eq!(r.severity, Severity::Info);
    }

    #[test]
    fn busy_port_errors() {
        let l = Tl::bind("0.0.0.0:0").unwrap();
        let port = l.local_addr().unwrap().port();
        // Keep listener alive — port is now busy.
        let r = check_p2p_port(port);
        assert_eq!(r.severity, Severity::Error);
        assert!(r.message.contains(&port.to_string()));
        drop(l);
    }
}
