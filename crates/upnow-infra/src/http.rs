use std::collections::BTreeMap;
use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::header::{HeaderName, HeaderValue};

use crate::{Env, InfraError};

pub const HTTP_TIMEOUT: Duration = Duration::from_secs(8);
pub const HTTP_USER_AGENT: &str = concat!("upnow/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpSettings {
    pub user_agent: String,
    pub timeout: Duration,
}

#[derive(Debug, Clone)]
pub enum HttpClient {
    Real(Client),
    Fake(FakeHttpClient),
}

impl HttpClient {
    /// Builds a real blocking HTTP client from explicit settings.
    ///
    /// # Errors
    ///
    /// Returns an error when reqwest cannot construct the client.
    pub fn real(settings: &HttpSettings) -> Result<Self, InfraError> {
        Ok(Self::Real(blocking_client(settings)?))
    }
    pub fn fake(responses: impl IntoIterator<Item = (String, HttpResponse)>) -> Self {
        Self::Fake(FakeHttpClient::new(responses))
    }
    pub fn fake_bytes(responses: impl IntoIterator<Item = (String, HttpBytesResponse)>) -> Self {
        Self::Fake(FakeHttpClient::new_bytes(responses))
    }

    /// Sends a GET request and returns the status code plus response body.
    ///
    /// # Errors
    ///
    /// Returns an error when the HTTP request fails, the response body cannot
    /// be read, or a fake client has no response for the URL.
    pub fn get_text(&self, url: &str) -> Result<HttpResponse, InfraError> {
        self.get_text_with_headers(url, [])
    }

    /// Sends a GET request with request-specific headers.
    ///
    /// # Errors
    ///
    /// Returns an error when a header is invalid, the HTTP request fails, the
    /// response body cannot be read, or a fake client has no response for the
    /// URL. Fake clients match only on URL and ignore headers.
    pub fn get_text_with_headers(
        &self,
        url: &str,
        headers: impl IntoIterator<Item = HttpHeader>,
    ) -> Result<HttpResponse, InfraError> {
        match self {
            Self::Real(client) => {
                let mut request = client.get(url);
                for header in headers {
                    let name = HeaderName::from_bytes(header.name.as_bytes()).map_err(|err| {
                        InfraError::HttpRequest {
                            url: url.to_owned(),
                            detail: err.to_string(),
                        }
                    })?;
                    let value = HeaderValue::from_str(&header.value).map_err(|err| {
                        InfraError::HttpRequest {
                            url: url.to_owned(),
                            detail: err.to_string(),
                        }
                    })?;
                    request = request.header(name, value);
                }
                let response = request.send().map_err(|err| InfraError::HttpRequest {
                    url: url.to_owned(),
                    detail: err.to_string(),
                })?;
                let status = response.status().as_u16();
                if !response.status().is_success() {
                    return Err(InfraError::HttpStatus {
                        url: url.to_owned(),
                        status,
                    });
                }
                let body = response.text().map_err(|err| InfraError::HttpBody {
                    url: url.to_owned(),
                    detail: err.to_string(),
                })?;
                Ok(HttpResponse { status, body })
            }
            Self::Fake(fake) => fake.get_text(url),
        }
    }

    /// Sends a GET request and returns the status code plus raw response body bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when the HTTP request fails, the response body cannot
    /// be read, or a fake client has no response for the URL.
    pub fn get_bytes(&self, url: &str) -> Result<HttpBytesResponse, InfraError> {
        match self {
            Self::Real(client) => {
                let response = client
                    .get(url)
                    .send()
                    .map_err(|err| InfraError::HttpRequest {
                        url: url.to_owned(),
                        detail: err.to_string(),
                    })?;
                let status = response.status().as_u16();
                if !response.status().is_success() {
                    return Err(InfraError::HttpStatus {
                        url: url.to_owned(),
                        status,
                    });
                }
                let body = response.bytes().map_err(|err| InfraError::HttpBody {
                    url: url.to_owned(),
                    detail: err.to_string(),
                })?;
                Ok(HttpBytesResponse {
                    status,
                    body: body.to_vec(),
                })
            }
            Self::Fake(fake) => fake.get_bytes(url),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpHeader {
    pub name: String,
    pub value: String,
}

impl HttpHeader {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeHttpClient {
    responses: BTreeMap<String, HttpBytesResponse>,
}

impl FakeHttpClient {
    pub fn new(responses: impl IntoIterator<Item = (String, HttpResponse)>) -> Self {
        Self {
            responses: responses
                .into_iter()
                .map(|(url, response)| {
                    (
                        url,
                        HttpBytesResponse {
                            status: response.status,
                            body: response.body.into_bytes(),
                        },
                    )
                })
                .collect(),
        }
    }
    pub fn new_bytes(responses: impl IntoIterator<Item = (String, HttpBytesResponse)>) -> Self {
        Self {
            responses: responses.into_iter().collect(),
        }
    }

    fn get_text(&self, url: &str) -> Result<HttpResponse, InfraError> {
        let response = self.get_bytes(url)?;
        let body = String::from_utf8(response.body).map_err(|err| InfraError::HttpBody {
            url: url.to_owned(),
            detail: err.to_string(),
        })?;

        Ok(HttpResponse {
            status: response.status,
            body,
        })
    }

    fn get_bytes(&self, url: &str) -> Result<HttpBytesResponse, InfraError> {
        let response = self
            .responses
            .get(url)
            .cloned()
            .ok_or_else(|| InfraError::HttpRequest {
                url: url.to_owned(),
                detail: "fake HTTP response was not registered".to_owned(),
            })?;

        if (200..=299).contains(&response.status) {
            Ok(response)
        } else {
            Err(InfraError::HttpStatus {
                url: url.to_owned(),
                status: response.status,
            })
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpBytesResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Default for HttpSettings {
    fn default() -> Self {
        Self {
            user_agent: HTTP_USER_AGENT.to_owned(),
            timeout: HTTP_TIMEOUT,
        }
    }
}

impl HttpSettings {
    pub fn default_client_settings() -> Self {
        Self::default()
    }
}

/// Builds a blocking HTTP client from explicit settings.
///
/// # Errors
///
/// Returns an error when reqwest cannot construct the client.
pub fn blocking_client(settings: &HttpSettings) -> Result<Client, InfraError> {
    Client::builder()
        .user_agent(settings.user_agent.clone())
        .timeout(settings.timeout)
        .build()
        .map_err(|err| InfraError::HttpClientBuild {
            detail: err.to_string(),
        })
}
pub fn env_base_url(env: &Env, var_name: &str, default: &str) -> String {
    env.non_empty_var(var_name)
        .map(|value| value.trim_end_matches('/').to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.to_owned())
}
