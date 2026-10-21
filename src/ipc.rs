//! Protocol between clients (CLI, GUI) and the root daemon: one JSON object
//! per line over a Unix socket. Reading the GPU never needs the daemon;
//! everything that writes goes through it.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::profile::{FanMode, Profile};

pub const PROTOCOL_VERSION: u32 = 1;

/// `$WNVCTL_SOCKET`, else `/run/wnvctl/wnvctl.sock`.
pub fn socket_path() -> PathBuf {
    std::env::var_os("WNVCTL_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/run/wnvctl/wnvctl.sock"))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// Protocol version and state of every GPU.
    Status,
    /// Applies a profile to a GPU (UUID or index).
    Apply { gpu: String, profile: Profile },
    /// Back to driver defaults, fans to automatic.
    Reset { gpu: String },
    /// Profile applied when the daemon starts (`None` removes it).
    SetStartup {
        gpu: String,
        profile: Option<Profile>,
    },
}

impl Request {
    /// Requests any local user may send.
    pub fn is_read_only(&self) -> bool {
        matches!(self, Request::Status)
    }
}

/// What the daemon is doing with one GPU.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpuStatus {
    pub uuid: String,
    pub index: u32,
    pub name: String,
    /// Last profile applied through the daemon.
    pub active: Option<Profile>,
    /// Name of the profile applied at boot, if any.
    pub startup: Option<String>,
    pub fan_mode: FanMode,
    /// Speed last written by the fan curve.
    pub fan_speed: Option<u32>,
    /// The fans are in manual mode but not because of us (another tool such
    /// as CoolerControl or GreenWithEnvy is driving them).
    pub external_fan_control: bool,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
    Status {
        version: u32,
        daemon: String,
        gpus: Vec<GpuStatus>,
    },
    Done {
        /// Human-readable list of what was written.
        changes: Vec<String>,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error(
        "the wnvctl daemon is not running ({0}). Enable it with: sudo systemctl enable --now wnvctl"
    )]
    Connect(std::io::Error),
    #[error("daemon connection: {0}")]
    Io(#[from] std::io::Error),
    #[error("bad reply from the daemon: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("{0}")]
    Daemon(String),
}

/// Sends one request and waits for the reply.
pub fn call(req: &Request) -> Result<Reply, ClientError> {
    let stream = UnixStream::connect(socket_path()).map_err(ClientError::Connect)?;
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut line = serde_json::to_string(req)?;
    line.push('\n');
    (&stream).write_all(line.as_bytes())?;
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply)?;
    match serde_json::from_str(&reply)? {
        Reply::Error { message } => Err(ClientError::Daemon(message)),
        r => Ok(r),
    }
}

/// `call` for requests answered with [`Reply::Done`].
pub fn call_done(req: &Request) -> Result<Vec<String>, ClientError> {
    match call(req)? {
        Reply::Done { changes } => Ok(changes),
        other => Err(ClientError::Daemon(format!("unexpected reply {other:?}"))),
    }
}

pub fn status() -> Result<Vec<GpuStatus>, ClientError> {
    match call(&Request::Status)? {
        Reply::Status { gpus, .. } => Ok(gpus),
        other => Err(ClientError::Daemon(format!("unexpected reply {other:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_format_is_stable() {
        let r = Request::Reset { gpu: "0".into() };
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            r#"{"cmd":"reset","gpu":"0"}"#
        );
        let back: Request = serde_json::from_str(r#"{"cmd":"status"}"#).unwrap();
        assert_eq!(back, Request::Status);
        let apply = Request::Apply {
            gpu: "GPU-x".into(),
            profile: Profile::default(),
        };
        let s = serde_json::to_string(&apply).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&s).unwrap(), apply);
        assert!(!apply.is_read_only());
    }
}
