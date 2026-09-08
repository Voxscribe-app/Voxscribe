pub mod client;
pub mod server;

use serde::{Deserialize, Serialize};

use crate::core::state::{Event, Snapshot};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Ping,
    Status,
    Subscribe,
    Toggle {
        #[serde(default)]
        language: Option<String>,
    },
    Start {
        #[serde(default)]
        language: Option<String>,
    },
    Stop,
    Cancel,
    Submit,
    Pause,
    Resume,
    Reload,
    SetBackend {
        id: String,
    },
    SetModel {
        name: String,
    },
    ModelUnload,
    ModelReload,
    ModelToggle,
    TranscribeFile {
        path: String,
    },
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Ok,
    Status(Box<Snapshot>),
    Text { text: String },
    Event(Event),
    Error { message: String },
}

impl Response {
    pub fn error(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
        }
    }

    pub fn is_error(&self) -> bool {
        matches!(self, Self::Error { .. })
    }
}

pub fn encode<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    let mut line = serde_json::to_string(value)?;
    line.push('\n');
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip_through_their_wire_form() {
        let cases = vec![
            Request::Ping,
            Request::Toggle {
                language: Some("it".into()),
            },
            Request::Start { language: None },
            Request::SetModel {
                name: "medium.en".into(),
            },
            Request::TranscribeFile {
                path: "/tmp/a.wav".into(),
            },
        ];
        for request in cases {
            let line = encode(&request).unwrap();
            assert!(line.ends_with('\n'));
            let parsed: Request = serde_json::from_str(line.trim()).unwrap();
            assert_eq!(parsed, request);
        }
    }

    #[test]
    fn optional_language_may_be_omitted_entirely() {
        let parsed: Request = serde_json::from_str(r#"{"cmd":"toggle"}"#).unwrap();
        assert_eq!(parsed, Request::Toggle { language: None });
    }

    #[test]
    fn the_command_tag_is_snake_case_so_shell_clients_can_hand_write_it() {
        let line = encode(&Request::ModelUnload).unwrap();
        assert_eq!(line.trim(), r#"{"cmd":"model_unload"}"#);
    }

    #[test]
    fn responses_round_trip_and_report_failure() {
        let response = Response::error("boom");
        assert!(response.is_error());
        let line = encode(&response).unwrap();
        let parsed: Response = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(parsed, response);
        assert!(!Response::Ok.is_error());
    }
}
