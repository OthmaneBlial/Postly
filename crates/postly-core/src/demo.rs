//! A small loopback-only API and a real, editable starter workspace.

use std::{
    fs,
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use crate::{Assertion, Collection, HeaderEntry, KeyValue, Request, ResponseExample, Workspace};
use serde_json::{json, Value};

/// The listener belongs to its creator and stops when the handle is dropped.
pub struct DemoServer {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl DemoServer {
    pub fn start(port: u16) -> io::Result<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let worker = thread::Builder::new()
            .name("postly-example-api".into())
            .spawn(move || {
                while !stopping.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let _ = serve(&mut stream);
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(_) => break,
                    }
                }
            })?;
        Ok(Self {
            address,
            stop,
            worker: Some(worker),
        })
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.address)
    }
}

impl Drop for DemoServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn orders(status: Option<&str>) -> Value {
    let rows = [
        json!({"id":"ord_1001","customer":"Ada","status":"paid","total":42.50,"currency":"EUR"}),
        json!({"id":"ord_1002","customer":"Lin","status":"pending","total":18.00,"currency":"EUR"}),
        json!({"id":"ord_1003","customer":"Sam","status":"paid","total":75.00,"currency":"EUR"}),
    ];
    let data: Vec<_> = rows
        .into_iter()
        .filter(|row| status.is_none_or(|s| row["status"] == s))
        .collect();
    json!({"count":data.len(),"orders":data})
}

fn serve(stream: &mut TcpStream) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_millis(250)))?;
    stream.set_write_timeout(Some(Duration::from_millis(250)))?;
    let mut request = Vec::new();
    let mut buffer = [0u8; 1024];
    let deadline = std::time::Instant::now() + Duration::from_millis(500);
    while request.len() < 8192 && std::time::Instant::now() < deadline {
        let length = stream.read(&mut buffer)?;
        if length == 0 {
            return Ok(());
        }
        request.extend_from_slice(&buffer[..length]);
        if request.windows(4).any(|part| part == b"\r\n\r\n") {
            break;
        }
    }
    let text = String::from_utf8_lossy(&request);
    let mut first = text.lines().next().unwrap_or_default().split_whitespace();
    let method = first.next().unwrap_or_default();
    let path = first.next().unwrap_or_default();
    let (status, body) = if method != "GET" {
        (
            "405 Method Not Allowed",
            json!({"error":"This example supports GET requests."}),
        )
    } else if let Ok(url) = url::Url::parse(&format!("http://localhost{path}")) {
        match url.path() {
            "/health" => (
                "200 OK",
                json!({"status":"ok","service":"Postly Orders","local":true}),
            ),
            "/orders" => {
                let filter = url
                    .query_pairs()
                    .find(|(key, _)| key == "status")
                    .map(|(_, value)| value.into_owned());
                (
                    "200 OK",
                    orders(filter.as_deref().filter(|s| !s.is_empty())),
                )
            }
            _ => (
                "404 Not Found",
                json!({"error":"Not found","routes":["/health","/orders?status=paid"]}),
            ),
        }
    } else {
        ("400 Bad Request", json!({"error":"Invalid request target"}))
    };
    let body = serde_json::to_vec_pretty(&body)?;
    write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len())?;
    stream.write_all(&body)
}

/// Create a starter only in a new or empty directory. Existing user data is
/// never merged or overwritten by onboarding.
pub fn create_workspace(root: &Path, base_url: &str) -> Result<Workspace, String> {
    if root.exists()
        && fs::read_dir(root)
            .map_err(|e| e.to_string())?
            .next()
            .is_some()
    {
        return Err(
            "Choose a new or empty folder for the example. Existing files are preserved.".into(),
        );
    }
    let workspace = Workspace::init(root, "Postly Orders").map_err(|e| e.to_string())?;
    let mut collection = Collection::new("Orders API");
    collection.description = Some("A real API on your machine. Try a request, change a filter, save and run the same files from your terminal.".into());
    collection
        .variables
        .insert("baseUrl".into(), base_url.into());
    let collection = workspace
        .create_collection(&collection)
        .map_err(|e| e.to_string())?;
    let mut health = Request::new("01 Health", "GET", "{{baseUrl}}/health");
    health.description =
        Some("Send this request to check that your local example API is running.".into());
    health.assertions = vec![
        Assertion::Status { expected: 200 },
        Assertion::JsonPointerEquals {
            pointer: "/status".into(),
            expected: json!("ok"),
        },
    ];
    let mut request = Request::new("02 List orders", "GET", "{{baseUrl}}/orders");
    request.description = Some("Change status from paid to pending, or disable it to see all three orders. Save, then inspect the TOML diff in Git.".into());
    request.query.push(KeyValue::enabled("status", "paid"));
    request.assertions = vec![
        Assertion::Status { expected: 200 },
        Assertion::BodyIsJson,
        Assertion::JsonPointerPresent {
            pointer: "/orders/0/id".into(),
        },
    ];
    request.examples.push(ResponseExample {
        name: "Paid orders".into(),
        status: Some(200),
        status_text: Some("OK".into()),
        headers: vec![HeaderEntry::enabled("Content-Type", "application/json")],
        cookies: vec![],
        body: Some(serde_json::to_string_pretty(&orders(Some("paid"))).map_err(|e| e.to_string())?),
        original_request: None,
        delay_ms: 0,
    });
    for request in [health, request] {
        workspace
            .save_request(&collection, &request)
            .map_err(|e| e.to_string())?;
    }
    fs::write(
        root.join(".gitignore"),
        ".postly/\nenvironments/*.postly-env.toml\n.env\n",
    )
    .map_err(|e| e.to_string())?;
    fs::write(
        root.join("README.md"),
        include_str!("../../../examples/orders/README.md"),
    )
    .map_err(|e| e.to_string())?;
    Ok(workspace)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn starter_runs_real_requests_and_stops_its_server() {
        let server = DemoServer::start(0).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let workspace = create_workspace(directory.path(), &server.url()).unwrap();
        let collections = workspace.collections().unwrap();
        let requests = workspace.requests(&collections[0]).unwrap();
        assert_eq!(requests.len(), 2);
        let engine = crate::HttpEngine::new(&crate::EngineOptions::default()).unwrap();
        let context = crate::VariableContext {
            collection: collections[0].collection.variables.clone(),
            ..Default::default()
        };
        for (_, request) in requests {
            let response = engine.execute(&request, &context).await.unwrap();
            assert!(crate::evaluate_response_assertions(&request.assertions, &response).is_empty());
        }
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let response: Value = client
            .get(format!("{}/orders?status=pending", server.url()))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(response["count"], 1);
        assert_eq!(response["orders"][0]["id"], "ord_1002");
        assert_eq!(
            client
                .post(format!("{}/orders", server.url()))
                .send()
                .await
                .unwrap()
                .status(),
            405
        );
        let address = server.address;
        drop(server);
        assert!(TcpStream::connect(address).is_err());
    }

    #[test]
    fn starter_preserves_existing_files() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("notes.txt"), "keep").unwrap();
        assert!(create_workspace(directory.path(), "http://127.0.0.1:1234").is_err());
        assert_eq!(
            fs::read_to_string(directory.path().join("notes.txt")).unwrap(),
            "keep"
        );
        assert!(!directory.path().join("postly.toml").exists());
    }
}
