use std::time::Duration;

use serde_json::Value;

use crate::setup::{fetch_json, loopback_agent, validate_comfy_url};

const ACTIVE_JOBS: &str = "/api/jobs?status=in_progress,pending&limit=100";
const VISIBLE_PENDING_JOBS: usize = 2;

#[derive(Debug, PartialEq, Eq)]
pub struct QueueStatus {
    pub running: usize,
    pub queued: usize,
    pub running_ids: Vec<String>,
    pub pending_ids: Vec<String>,
}

impl QueueStatus {
    pub fn is_idle(&self) -> bool {
        self.running == 0 && self.queued == 0
    }
}

pub fn check(url: &str) -> Result<QueueStatus, String> {
    let origin = validate_comfy_url(url)?;
    let agent = loopback_agent(Duration::from_secs(2));
    fetch_json(&agent, &origin, ACTIVE_JOBS)
        .and_then(|response| from_jobs(&response))
        .or_else(|| {
            fetch_json(&agent, &origin, "/queue").and_then(|response| from_queue(&response))
        })
        .ok_or_else(|| {
            "Could not verify the ComfyUI queue. Check that ComfyUI is running, then refresh."
                .into()
        })
}

fn from_jobs(response: &Value) -> Option<QueueStatus> {
    let jobs = response.get("jobs")?.as_array()?;
    let pagination = response.get("pagination")?;
    // Both counts must describe the same snapshot. A partial page can omit a
    // running job, even when its returned rows all say pending.
    if pagination.get("offset")?.as_u64()? != 0
        || pagination.get("has_more")?.as_bool()?
        || pagination.get("total")?.as_u64()? != jobs.len() as u64
    {
        return None;
    }
    let mut status = QueueStatus {
        running: 0,
        queued: 0,
        running_ids: Vec::new(),
        pending_ids: Vec::new(),
    };
    for job in jobs {
        match job.get("status")?.as_str()? {
            "in_progress" => {
                status.running += 1;
                if let Some(id) = job.get("id").and_then(Value::as_str) {
                    status.running_ids.push(id.into());
                }
            }
            "pending" => {
                status.queued += 1;
                push_pending_id(
                    &mut status.pending_ids,
                    job.get("id").and_then(Value::as_str),
                );
            }
            _ => return None,
        }
    }
    Some(status)
}

fn from_queue(response: &Value) -> Option<QueueStatus> {
    let running = response.get("queue_running")?.as_array()?;
    let pending = response.get("queue_pending")?.as_array()?;
    let mut running_ids = Vec::with_capacity(running.len());
    for item in running {
        item.get(0)?.as_f64()?;
        running_ids.push(item.get(1)?.as_str()?.to_owned());
    }
    let mut pending_ids = Vec::new();
    for item in pending {
        item.get(0)?.as_f64()?;
        push_pending_id(&mut pending_ids, Some(item.get(1)?.as_str()?));
    }
    Some(QueueStatus {
        running: running.len(),
        queued: pending.len(),
        running_ids,
        pending_ids,
    })
}

fn push_pending_id(ids: &mut Vec<String>, id: Option<&str>) {
    if let Some(id) = id
        && ids.len() < VISIBLE_PENDING_JOBS
        && !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        && !ids.iter().any(|entry| entry == id)
    {
        ids.push(id.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::ErrorKind;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::time::Instant;

    fn with_fixture(
        responses: &[(&str, &str, &str)],
        assertion: impl FnOnce(Result<QueueStatus, String>),
    ) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::scope(|threads| {
            threads.spawn(move || {
                for (endpoint, status, body) in responses {
                    let deadline = Instant::now() + Duration::from_secs(3);
                    let mut stream = loop {
                        match listener.accept() {
                            Ok((stream, _)) => break stream,
                            Err(error) if error.kind() == ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                            {
                                std::thread::sleep(Duration::from_millis(5));
                            }
                            Err(error) => panic!("Expected request for {endpoint}: {error}"),
                        }
                    };
                    stream.set_nonblocking(false).unwrap();
                    stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                    let mut request = BufReader::new(&mut stream);
                    let mut line = String::new();
                    request.read_line(&mut line).unwrap();
                    assert_eq!(line.trim_end(), format!("GET {endpoint} HTTP/1.1"));
                    loop {
                        line.clear();
                        if request.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                            break;
                        }
                    }
                    write!(
                        stream,
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len(),
                    )
                    .unwrap();
                }
            });
            assertion(check(&url));
        });
    }

    #[test]
    fn counts_running_and_queued_from_one_modern_snapshot() {
        with_fixture(
            &[(
                ACTIVE_JOBS,
                "200 OK",
                r#"{"jobs":[{"id":"render-id","status":"in_progress"},{"id":"queued-1","status":"pending"},{"status":"pending"}],"pagination":{"offset":0,"total":3,"has_more":false}}"#,
            )],
            |result| {
                let status = result.unwrap();
                assert_eq!(status.running, 1);
                assert_eq!(status.queued, 2);
                assert_eq!(status.running_ids, ["render-id"]);
                assert_eq!(status.pending_ids, ["queued-1"]);
                assert!(!status.is_idle());
            },
        );
    }

    #[test]
    fn uses_legacy_queue_when_modern_endpoint_is_missing() {
        with_fixture(
            &[
                (ACTIVE_JOBS, "404 Not Found", "{}"),
                (
                    "/queue",
                    "200 OK",
                    r#"{"queue_running":[],"queue_pending":[]}"#,
                ),
            ],
            |result| assert!(result.unwrap().is_idle()),
        );
    }

    #[test]
    fn malformed_or_partial_snapshots_cannot_claim_idle() {
        assert!(
            from_jobs(&json!({
                "jobs": [], "pagination": {"offset": 0, "total": 1, "has_more": true},
            }))
            .is_none()
        );
        assert!(from_queue(&json!({"queue_running":[]})).is_none());
        with_fixture(
            &[
                (ACTIVE_JOBS, "200 OK", r#"{"jobs":[]}"#),
                (
                    "/queue",
                    "200 OK",
                    r#"{"queue_running":[null],"queue_pending":[]}"#,
                ),
            ],
            |result| assert!(result.is_err()),
        );
    }

    #[test]
    fn counts_legacy_entries_without_using_prompt_contents() {
        let status = from_queue(&json!({
            "queue_running": [[0, "running prompt", {"discarded": "workflow"}, {}, []]],
            "queue_pending": [[1, "pending-prompt", {}, {}, []]]
        }))
        .unwrap();
        assert_eq!(
            status,
            QueueStatus {
                running: 1,
                queued: 1,
                running_ids: vec!["running prompt".into()],
                pending_ids: vec!["pending-prompt".into()]
            }
        );
    }

    #[test]
    fn pending_rows_are_bounded_without_changing_queue_counts() {
        let result = from_jobs(&json!({
            "jobs": [
                {"id":"not a job id", "status":"pending"},
                {"id":"pending-a", "status":"pending"},
                {"id":"pending-a", "status":"pending"},
                {"id":"pending-b", "status":"pending"},
                {"id":"pending-c", "status":"pending"},
            ],
            "pagination": {"offset":0,"total":5,"has_more":false},
        }))
        .unwrap();
        assert_eq!(result.queued, 5);
        assert_eq!(result.pending_ids, ["pending-a", "pending-b"]);
    }

    #[test]
    fn rejects_non_loopback_or_credentials_before_sending_requests() {
        assert!(check("http://example.com:8188").is_err());
        assert!(check("http://user:pass@127.0.0.1:8188").is_err());
    }
}
