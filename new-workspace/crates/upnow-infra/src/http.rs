use std::collections::BTreeMap;
use std::time::Duration;

use reqwest::blocking::Client;

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

    #[must_use]
    pub fn fake(responses: impl IntoIterator<Item = (String, HttpResponse)>) -> Self {
        Self::Fake(FakeHttpClient::new(responses))
    }

    #[must_use]
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
pub struct FakeHttpClient {
    responses: BTreeMap<String, HttpBytesResponse>,
}

impl FakeHttpClient {
    #[must_use]
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

    #[must_use]
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
    #[must_use]
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

#[must_use]
pub fn env_base_url(env: &Env, var_name: &str, default: &str) -> String {
    env.var(var_name)
        .and_then(|value| {
            let trimmed = value.trim().trim_end_matches('/').to_owned();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        })
        .unwrap_or_else(|| default.to_owned())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        HTTP_TIMEOUT, HTTP_USER_AGENT, HttpBytesResponse, HttpClient, HttpResponse, HttpSettings,
        env_base_url,
    };
    use crate::Env;

    #[test]
    fn default_http_settings_preserve_timeout_and_user_agent() {
        let settings = HttpSettings::default_client_settings();

        assert_eq!(settings.timeout, HTTP_TIMEOUT);
        assert_eq!(settings.timeout, Duration::from_secs(8));
        assert_eq!(settings.user_agent, HTTP_USER_AGENT);
        assert!(settings.user_agent.starts_with("upnow/"));
    }

    #[test]
    fn base_url_override_trims_and_removes_trailing_slash() {
        let env = Env::fixed([(
            "UPNOW_TEST_BASE_URL".to_owned(),
            "  https://example.test///  ".to_owned(),
        )]);

        assert_eq!(
            env_base_url(&env, "UPNOW_TEST_BASE_URL", "https://default.test"),
            "https://example.test"
        );
    }

    #[test]
    fn base_url_override_ignores_empty_values() {
        let env = Env::fixed([("UPNOW_TEST_BASE_URL".to_owned(), " / ".to_owned())]);

        assert_eq!(
            env_base_url(&env, "UPNOW_TEST_BASE_URL", "https://default.test"),
            "https://default.test"
        );
    }

    #[test]
    fn fake_http_client_returns_registered_responses() {
        let client = HttpClient::fake([(
            "https://example.test/data".to_owned(),
            HttpResponse {
                status: 200,
                body: "body".to_owned(),
            },
        )]);

        let response = client
            .get_text("https://example.test/data")
            .expect("registered response should be returned");

        assert_eq!(response.status, 200);
        assert_eq!(response.body, "body");
    }

    #[test]
    fn fake_http_client_returns_registered_byte_responses() {
        let client = HttpClient::fake_bytes([(
            "https://example.test/data".to_owned(),
            HttpBytesResponse {
                status: 200,
                body: vec![0, 159, 146, 150],
            },
        )]);

        let response = client
            .get_bytes("https://example.test/data")
            .expect("registered response should be returned");

        assert_eq!(response.status, 200);
        assert_eq!(response.body, [0, 159, 146, 150]);
    }

    #[test]
    fn fake_http_client_errors_for_unregistered_urls() {
        let client = HttpClient::fake([]);

        assert!(client.get_text("https://example.test/missing").is_err());
    }

    #[test]
    fn fake_http_client_rejects_non_success_statuses() {
        let client = HttpClient::fake([(
            "https://example.test/missing".to_owned(),
            HttpResponse {
                status: 404,
                body: "not found".to_owned(),
            },
        )]);

        assert!(client.get_text("https://example.test/missing").is_err());
    }
}
