// Copyright 2025 Lablup Inc. and Jeongkyu Shin
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! JSON status-endpoint probe (`all-smi api --json-probe NAME=URL`).
//!
//! Some services only publish their state on a loopback status endpoint
//! (a model server's `/health` or `/stats`). This probe issues one plain
//! `GET` per collection cycle, keeps the numeric and boolean fields of the
//! JSON object it gets back (top level and one level of nesting, as
//! `outer.inner`), and reports each field's per-second change since the
//! previous cycle, so a remote viewer can read counters as rates without
//! reaching the endpoint itself.
//!
//! Plain `http://` only, short timeouts, a capped response size, and a
//! capped field count: the probe never writes to the endpoint and a slow
//! or hostile one cannot stall the collection loop for long.

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::str::FromStr;
use std::time::{Duration, Instant};

use serde_json::Value;

/// Connect / read / write timeout for one probe request.
const TIMEOUT: Duration = Duration::from_millis(800);
/// Largest response accepted.
const MAX_BODY: usize = 256 * 1024;
/// Fields kept per probe.
pub const MAX_FIELDS: usize = 64;
/// Longest field name kept.
const MAX_KEY_LEN: usize = 64;

/// One `NAME=URL` pair from the command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonProbeSpec {
    pub name: String,
    pub url: String,
}

impl FromStr for JsonProbeSpec {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (name, url) = s
            .split_once('=')
            .ok_or_else(|| format!("expected NAME=URL, got {s:?}"))?;
        let name = name.trim();
        if name.is_empty()
            || name.len() > 32
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(format!(
                "probe name {name:?} must be 1-32 characters of [A-Za-z0-9_-]"
            ));
        }
        let parsed = url::Url::parse(url.trim()).map_err(|e| format!("bad URL {url:?}: {e}"))?;
        if parsed.scheme() != "http" || parsed.host_str().is_none() {
            return Err(format!(
                "{url:?}: only http://host[:port]/path URLs are supported"
            ));
        }
        Ok(Self {
            name: name.to_string(),
            url: parsed.to_string(),
        })
    }
}

/// What one probe returned in one cycle.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct JsonProbeSample {
    pub name: String,
    /// The endpoint answered 2xx with a JSON object.
    pub up: bool,
    pub values: BTreeMap<String, f64>,
    /// Per-second change since the previous cycle, for fields present in
    /// both cycles that did not decrease (a decrease means a restart).
    pub rates: BTreeMap<String, f64>,
}

impl JsonProbeSample {
    pub fn value(&self, key: &str) -> Option<f64> {
        self.values.get(key).copied()
    }

    pub fn rate(&self, key: &str) -> Option<f64> {
        self.rates.get(key).copied()
    }
}

/// One endpoint's answer: whether it was 2xx, and its fields.
type Fetched = (bool, BTreeMap<String, f64>);

/// Polls the configured endpoints and remembers the previous values.
pub struct JsonProber {
    specs: Vec<JsonProbeSpec>,
    previous: HashMap<String, (Instant, BTreeMap<String, f64>)>,
}

impl JsonProber {
    pub fn new(specs: Vec<JsonProbeSpec>) -> Self {
        Self {
            specs,
            previous: HashMap::new(),
        }
    }

    pub fn sample(&mut self) -> Vec<JsonProbeSample> {
        let specs = self.specs.clone();
        // One thread per endpoint, so a slow one costs one timeout per
        // cycle rather than one per probe.
        let fetched: Vec<(Option<Fetched>, Instant)> = std::thread::scope(|scope| {
            let handles: Vec<_> = specs
                .iter()
                .map(|spec| {
                    scope.spawn(|| {
                        let result = http_get(&spec.url).ok().and_then(|(status, body)| {
                            let json: Value = serde_json::from_slice(&body).ok()?;
                            Some(((200..300).contains(&status), flatten(&json)))
                        });
                        (result, Instant::now())
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or((None, Instant::now())))
                .collect()
        });
        specs
            .iter()
            .zip(fetched)
            .map(|(spec, (fetched, now))| match fetched {
                Some((up, values)) => {
                    let rates = self.rates(&spec.name, now, &values);
                    self.previous
                        .insert(spec.name.clone(), (now, values.clone()));
                    JsonProbeSample {
                        name: spec.name.clone(),
                        up,
                        values,
                        rates,
                    }
                }
                None => {
                    self.previous.remove(&spec.name);
                    JsonProbeSample {
                        name: spec.name.clone(),
                        ..Default::default()
                    }
                }
            })
            .collect()
    }

    fn rates(
        &self,
        name: &str,
        now: Instant,
        values: &BTreeMap<String, f64>,
    ) -> BTreeMap<String, f64> {
        let Some((then, previous)) = self.previous.get(name) else {
            return BTreeMap::new();
        };
        let dt = now.duration_since(*then).as_secs_f64();
        if dt <= 0.0 {
            return BTreeMap::new();
        }
        values
            .iter()
            .filter_map(|(k, v)| {
                let p = previous.get(k)?;
                (v >= p).then(|| (k.clone(), (v - p) / dt))
            })
            .collect()
    }
}

/// Numeric and boolean fields of a JSON object, top level and one level
/// down (`cache.hits`), capped at [`MAX_FIELDS`].
pub fn flatten(json: &Value) -> BTreeMap<String, f64> {
    let mut out = BTreeMap::new();
    let Value::Object(map) = json else {
        return out;
    };
    for (key, value) in map {
        match value {
            Value::Object(inner) => {
                for (k, v) in inner {
                    insert(&mut out, format!("{key}.{k}"), v);
                }
            }
            other => insert(&mut out, key.clone(), other),
        }
    }
    out
}

fn insert(out: &mut BTreeMap<String, f64>, key: String, value: &Value) {
    if out.len() >= MAX_FIELDS
        || key.len() > MAX_KEY_LEN
        || !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    {
        return;
    }
    let number = match value {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        _ => None,
    };
    if let Some(n) = number.filter(|n| n.is_finite()) {
        out.insert(key, n);
    }
}

/// Minimal HTTP/1.0 `GET`: one request, `Connection: close`, body read to
/// the end. Returns the status code and body.
fn http_get(url: &str) -> Result<(u16, Vec<u8>), String> {
    let parsed = url::Url::parse(url).map_err(|e| e.to_string())?;
    let host = parsed.host_str().ok_or("no host")?;
    let port = parsed.port_or_known_default().unwrap_or(80);
    let addr = (host, port)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .next()
        .ok_or("no address")?;
    let mut stream = TcpStream::connect_timeout(&addr, TIMEOUT).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(TIMEOUT))
        .and_then(|()| stream.set_write_timeout(Some(TIMEOUT)))
        .map_err(|e| e.to_string())?;
    let path = match parsed.query() {
        Some(q) => format!("{}?{q}", parsed.path()),
        None => parsed.path().to_string(),
    };
    let request = format!(
        "GET {path} HTTP/1.0\r\nHost: {host}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| e.to_string())?;

    let mut raw = Vec::new();
    stream
        .take((MAX_BODY + 8192) as u64)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("no header terminator")?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or("no status line")?;
    let body = raw[split + 4..].to_vec();
    if body.len() > MAX_BODY {
        return Err("response too large".to_string());
    }
    Ok((status, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn spec_parsing() {
        let spec: JsonProbeSpec = "og=http://127.0.0.1:10001/og/stats".parse().unwrap();
        assert_eq!(spec.name, "og");
        assert_eq!(spec.url, "http://127.0.0.1:10001/og/stats");
        assert!("og".parse::<JsonProbeSpec>().is_err());
        assert!("bad name=http://h/x".parse::<JsonProbeSpec>().is_err());
        assert!("x=https://h/x".parse::<JsonProbeSpec>().is_err());
    }

    #[test]
    fn flatten_keeps_numbers_bools_and_one_nested_level() {
        let json: Value = serde_json::from_str(
            r#"{"steps": 102, "box_s": 0.77, "ok": true, "name": "x", "list": [1],
                "cache": {"hits": 3, "deep": {"x": 1}}, "bad key": 1}"#,
        )
        .unwrap();
        let flat = flatten(&json);
        assert_eq!(flat.get("steps"), Some(&102.0));
        assert_eq!(flat.get("box_s"), Some(&0.77));
        assert_eq!(flat.get("ok"), Some(&1.0));
        assert_eq!(flat.get("cache.hits"), Some(&3.0));
        assert!(!flat.contains_key("name"));
        assert!(!flat.contains_key("list"));
        assert!(!flat.contains_key("cache.deep"));
        assert!(!flat.contains_key("bad key"));
        assert!(flatten(&Value::Null).is_empty());
    }

    #[test]
    fn samples_a_local_endpoint_and_reports_rates() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            for steps in [10, 40] {
                let (mut conn, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut buf = [0u8; 256];
                while !request.ends_with(b"\r\n\r\n") {
                    let n = conn.read(&mut buf).unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&buf[..n]);
                }
                assert!(request.starts_with(b"GET /og/stats HTTP/1.0\r\n"));
                let body = format!(r#"{{"steps": {steps}, "inflight": 1}}"#);
                write!(
                    conn,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let spec: JsonProbeSpec = format!("og=http://127.0.0.1:{port}/og/stats")
            .parse()
            .unwrap();
        let mut prober = JsonProber::new(vec![spec]);
        let first = prober.sample();
        assert!(first[0].up);
        assert_eq!(first[0].value("steps"), Some(10.0));
        assert!(first[0].rates.is_empty());
        std::thread::sleep(Duration::from_millis(20));
        let second = prober.sample();
        assert!(second[0].rate("steps").unwrap() > 0.0);
        assert_eq!(second[0].rate("inflight"), Some(0.0));
        server.join().unwrap();

        // Nothing listening any more: reported down, no values.
        let third = prober.sample();
        assert!(!third[0].up);
        assert!(third[0].values.is_empty());
    }
}
