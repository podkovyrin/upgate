//! Infrastructure crate for the `upgate` rebuild.
#![allow(clippy::must_use_candidate, clippy::return_self_not_must_use)]

use std::fmt;

pub mod clock;
pub mod env;
pub mod http;
pub mod logging;
pub mod parallel;
pub mod process;

pub use clock::Clock;
pub use env::Env;
pub use http::{
    FakeHttpClient, HTTP_TIMEOUT, HTTP_USER_AGENT, HttpBytesResponse, HttpClient, HttpHeader,
    HttpResponse, HttpSettings, blocking_client, env_base_url,
};
pub use logging::{LoggingOptions, init_logging};
pub use parallel::{effective_parallelism, run_ordered_parallel, run_ordered_parallel_stoppable};
pub use process::{
    CommandCheck, CommandFailure, CommandOutput, CommandSpec, CommandStartEvent, FakeProcess,
    MUTATION_ENABLE_NOTICE, MUTATION_SKIP_NOTICE, MutationMode, ProcessRunner,
    command_exists_in_env, status_allowed,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InfraError {
    ProcessSpawn {
        command: String,
        detail: String,
    },
    CommandFailed(CommandFailure),
    OutputUtf8 {
        command: String,
        stream: &'static str,
        detail: String,
    },
    HttpClientBuild {
        detail: String,
    },
    HttpRequest {
        url: String,
        detail: String,
    },
    HttpStatus {
        url: String,
        status: u16,
    },
    HttpBody {
        url: String,
        detail: String,
    },
    JsonParse {
        command: String,
        detail: String,
    },
    Logging {
        detail: String,
    },
    FakeProcessState {
        detail: String,
    },
    ParallelPoolBuild {
        label: String,
        detail: String,
    },
    ParallelWorkerPanic {
        label: String,
    },
}

impl InfraError {
    pub fn is_interruption(&self) -> bool {
        matches!(self, Self::CommandFailed(failure) if failure.is_interruption())
    }
}

impl fmt::Display for InfraError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProcessSpawn { command, detail } => {
                write!(formatter, "failed to run {command}: {detail}")
            }
            Self::CommandFailed(failure) => failure.fmt(formatter),
            Self::OutputUtf8 {
                command,
                stream,
                detail,
            } => {
                write!(
                    formatter,
                    "{command} {stream} was not valid UTF-8: {detail}"
                )
            }
            Self::HttpClientBuild { detail } => {
                write!(formatter, "failed to build HTTP client: {detail}")
            }
            Self::HttpRequest { url, detail } => {
                write!(formatter, "HTTP request failed for {url}: {detail}")
            }
            Self::HttpStatus { url, status } => {
                write!(formatter, "HTTP request failed for {url}: status {status}")
            }
            Self::HttpBody { url, detail } => {
                write!(
                    formatter,
                    "failed to read HTTP response body from {url}: {detail}"
                )
            }
            Self::JsonParse { command, detail } => {
                write!(
                    formatter,
                    "failed to parse JSON output from {command}: {detail}"
                )
            }
            Self::Logging { detail } => write!(formatter, "failed to initialize logging: {detail}"),
            Self::FakeProcessState { detail } => {
                write!(formatter, "fake process state unavailable: {detail}")
            }
            Self::ParallelPoolBuild { label, detail } => {
                write!(formatter, "failed to build {label} thread pool: {detail}")
            }
            Self::ParallelWorkerPanic { label } => {
                write!(formatter, "{label} worker thread panicked")
            }
        }
    }
}

impl std::error::Error for InfraError {}
