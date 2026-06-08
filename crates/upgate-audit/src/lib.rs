//! Shared security-audit service for OSV lookups.
#![allow(clippy::must_use_candidate, clippy::return_self_not_must_use)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Display};
use std::sync::{Arc, Condvar, Mutex};

use serde::{Deserialize, Serialize};
use upgate_domain::{AuditFinding, AuditLookupResult, AuditQuery};
use upgate_infra::{Env, HttpClient, InfraError, env_base_url, run_ordered_parallel};

const OSV_BASE_URL_ENV: &str = "upgate_OSV_BASE_URL";
const DEFAULT_OSV_BASE_URL: &str = "https://api.osv.dev";
const DEFAULT_CHUNK_SIZE: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditError {
    Infra(String),
    Json(String),
    CachePoisoned,
}

impl Display for AuditError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Infra(detail) | Self::Json(detail) => formatter.write_str(detail),
            Self::CachePoisoned => formatter.write_str("audit cache state is unavailable"),
        }
    }
}

impl std::error::Error for AuditError {}

#[derive(Debug, Clone)]
pub struct AuditService {
    inner: Arc<AuditServiceInner>,
}

#[derive(Debug)]
struct AuditServiceInner {
    http: HttpClient,
    base_url: String,
    concurrency: usize,
    chunk_size: usize,
    query_lock: Mutex<()>,
    cache: Mutex<BTreeMap<AuditQuery, AuditLookupResult>>,
    permits: AuditPermits,
}

#[derive(Debug)]
struct AuditPermits {
    available: Mutex<usize>,
    changed: Condvar,
}

impl AuditService {
    pub fn new(http: HttpClient, env: &Env, concurrency: usize) -> Self {
        let concurrency = concurrency.max(1);
        Self {
            inner: Arc::new(AuditServiceInner {
                http,
                base_url: env_base_url(env, OSV_BASE_URL_ENV, DEFAULT_OSV_BASE_URL),
                concurrency,
                chunk_size: DEFAULT_CHUNK_SIZE,
                query_lock: Mutex::new(()),
                cache: Mutex::new(BTreeMap::new()),
                permits: AuditPermits {
                    available: Mutex::new(concurrency),
                    changed: Condvar::new(),
                },
            }),
        }
    }

    /// Queries OSV for supported package/version identities.
    ///
    /// # Errors
    ///
    /// Returns an error when the process-local cache cannot be accessed.
    pub fn query(
        &self,
        queries: impl IntoIterator<Item = AuditQuery>,
    ) -> Result<BTreeMap<AuditQuery, AuditLookupResult>, AuditError> {
        let requested = queries.into_iter().collect::<BTreeSet<_>>();
        if requested.is_empty() {
            return Ok(BTreeMap::new());
        }

        let _query_lock = self
            .inner
            .query_lock
            .lock()
            .map_err(|_| AuditError::CachePoisoned)?;
        let cached = self
            .inner
            .cache
            .lock()
            .map_err(|_| AuditError::CachePoisoned)?;
        let missing = requested
            .iter()
            .filter(|query| !cached.contains_key(*query))
            .cloned()
            .collect::<Vec<_>>();
        drop(cached);

        if !missing.is_empty() {
            let chunks = missing
                .chunks(self.inner.chunk_size)
                .map(<[AuditQuery]>::to_vec)
                .collect::<Vec<_>>();
            let chunk_results = run_ordered_parallel(
                chunks,
                self.inner.concurrency,
                "OSV audit requests",
                |chunk| self.query_chunk(chunk),
            )
            .map_err(|err| AuditError::Infra(err.to_string()))?;

            let mut cache = self
                .inner
                .cache
                .lock()
                .map_err(|_| AuditError::CachePoisoned)?;
            for chunk in chunk_results {
                for (query, result) in chunk {
                    cache.insert(query, result);
                }
            }
        }

        let cache = self
            .inner
            .cache
            .lock()
            .map_err(|_| AuditError::CachePoisoned)?;
        Ok(requested
            .into_iter()
            .filter_map(|query| cache.get(&query).cloned().map(|result| (query, result)))
            .collect())
    }

    fn query_chunk(&self, queries: Vec<AuditQuery>) -> BTreeMap<AuditQuery, AuditLookupResult> {
        match self.query_chunk_inner(&queries) {
            Ok(results) => results,
            Err(err) => queries
                .into_iter()
                .map(|query| {
                    (
                        query,
                        AuditLookupResult::LookupFailed {
                            detail: err.to_string(),
                        },
                    )
                })
                .collect(),
        }
    }

    fn query_chunk_inner(
        &self,
        queries: &[AuditQuery],
    ) -> Result<BTreeMap<AuditQuery, AuditLookupResult>, AuditError> {
        let request = OsvBatchRequest {
            queries: queries.iter().map(OsvQuery::from).collect(),
        };
        let body =
            serde_json::to_string(&request).map_err(|err| AuditError::Json(err.to_string()))?;
        let url = format!("{}/v1/querybatch", self.inner.base_url);
        let _permit = self.acquire_request_permit()?;
        let response = self
            .inner
            .http
            .post_json_text(&url, &body, [])
            .map_err(|err| AuditError::Infra(err.to_string()))?;
        parse_batch_response(queries, &response.body)
    }

    fn acquire_request_permit(&self) -> Result<RequestPermit<'_>, AuditError> {
        let mut available = self
            .inner
            .permits
            .available
            .lock()
            .map_err(|_| AuditError::CachePoisoned)?;
        while *available == 0 {
            available = self
                .inner
                .permits
                .changed
                .wait(available)
                .map_err(|_| AuditError::CachePoisoned)?;
        }
        *available -= 1;
        drop(available);
        Ok(RequestPermit { service: self })
    }
}

struct RequestPermit<'a> {
    service: &'a AuditService,
}

impl Drop for RequestPermit<'_> {
    fn drop(&mut self) {
        if let Ok(mut available) = self.service.inner.permits.available.lock() {
            *available += 1;
            self.service.inner.permits.changed.notify_one();
        }
    }
}

fn parse_batch_response(
    queries: &[AuditQuery],
    raw: &str,
) -> Result<BTreeMap<AuditQuery, AuditLookupResult>, AuditError> {
    let response: OsvBatchResponse =
        serde_json::from_str(raw).map_err(|err| AuditError::Json(err.to_string()))?;
    if response.results.len() != queries.len() {
        return Err(AuditError::Json(format!(
            "OSV returned {} results for {} queries",
            response.results.len(),
            queries.len()
        )));
    }

    Ok(queries
        .iter()
        .cloned()
        .zip(response.results)
        .map(|(query, result)| (query, audit_result_from_osv(result)))
        .collect())
}

fn audit_result_from_osv(result: OsvQueryResult) -> AuditLookupResult {
    if result.vulns.is_empty() {
        return AuditLookupResult::Clean;
    }
    AuditLookupResult::Vulnerable {
        findings: result
            .vulns
            .into_iter()
            .map(|vuln| {
                let severity = vuln.severity();
                AuditFinding {
                    id: vuln.id,
                    aliases: vuln.aliases,
                    summary: vuln.summary,
                    severity,
                    references: vuln
                        .references
                        .into_iter()
                        .filter_map(|reference| reference.url)
                        .collect(),
                }
            })
            .collect(),
    }
}

#[derive(Debug, Serialize)]
struct OsvBatchRequest {
    queries: Vec<OsvQuery>,
}

#[derive(Debug, Serialize)]
struct OsvQuery {
    version: String,
    package: OsvPackage,
}

impl From<&AuditQuery> for OsvQuery {
    fn from(value: &AuditQuery) -> Self {
        Self {
            version: value.version.as_str().to_owned(),
            package: OsvPackage {
                name: value.subject.package_name.as_str().to_owned(),
                ecosystem: value.subject.ecosystem.as_str().to_owned(),
            },
        }
    }
}

#[derive(Debug, Serialize)]
struct OsvPackage {
    name: String,
    ecosystem: String,
}

#[derive(Debug, Deserialize)]
struct OsvBatchResponse {
    #[serde(default)]
    results: Vec<OsvQueryResult>,
}

#[derive(Debug, Deserialize)]
struct OsvQueryResult {
    #[serde(default)]
    vulns: Vec<OsvVulnerability>,
}

#[derive(Debug, Deserialize)]
struct OsvVulnerability {
    id: String,
    #[serde(default)]
    aliases: Vec<String>,
    summary: Option<String>,
    #[serde(default)]
    severity: Vec<OsvSeverity>,
    #[serde(default)]
    references: Vec<OsvReference>,
    #[serde(default)]
    database_specific: serde_json::Value,
}

impl OsvVulnerability {
    fn severity(&self) -> Option<String> {
        self.severity
            .first()
            .map(|severity| severity.score.clone())
            .or_else(|| {
                self.database_specific
                    .get("severity")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned)
            })
    }
}

#[derive(Debug, Deserialize)]
struct OsvSeverity {
    score: String,
}

#[derive(Debug, Deserialize)]
struct OsvReference {
    url: Option<String>,
}

impl From<InfraError> for AuditError {
    fn from(value: InfraError) -> Self {
        Self::Infra(value.to_string())
    }
}
