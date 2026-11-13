//! Brr's scoped client protocol. Connection adapters supply caller authority.
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientRole {
    Desktop,
    Mobile,
    Node,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Channel {
    System,
    Fleet,
    Sessions,
    Chat,
    Agents,
    Questions,
    Approvals,
    Projects,
    Sync,
    Desktop,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Request {
    pub id: String,
    pub channel: Channel,
    pub method: String,
    #[serde(default)]
    pub params: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_epoch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_node_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

impl ApiError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for ApiError {}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reply {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
}

impl Reply {
    pub fn from_result(id: String, result: Result<Value, ApiError>) -> Self {
        match result {
            Ok(result) => Self {
                id,
                result: Some(result),
                error: None,
            },
            Err(error) => Self {
                id,
                result: None,
                error: Some(error),
            },
        }
    }
}

impl Request {
    pub fn validate(&self, role: ClientRole) -> Result<(), ApiError> {
        if self.channel == Channel::Desktop && role != ClientRole::Desktop {
            return Err(ApiError::new(
                "forbidden",
                "Desktop commands require local desktop IPC",
            ));
        }
        if self.channel == Channel::Fleet
            && self.method == "node.register"
            && role != ClientRole::Node
        {
            return Err(ApiError::new(
                "forbidden",
                "Reverse node registration requires a paired node credential",
            ));
        }
        let policy = self.policy()?;
        if policy.mutating && !self.operation_id.as_deref().is_some_and(valid_id) {
            return Err(ApiError::new(
                "operation_id_required",
                "A stable operationId is required for this command",
            ));
        }
        if policy.live && !self.expected_epoch.as_deref().is_some_and(valid_id) {
            return Err(ApiError::new(
                "epoch_required",
                "The current runtime epoch is required for a live command",
            ));
        }
        if !valid_id(&self.id) {
            return Err(ApiError::new(
                "invalid_request",
                "A bounded request id is required",
            ));
        }
        if self.channel == Channel::Projects
            && matches!(self.method.as_str(), "save" | "delete")
            && !self
                .expected_revision
                .as_deref()
                .and_then(|revision| revision.parse::<i64>().ok())
                .is_some_and(|revision| revision > 0)
        {
            return Err(ApiError::new(
                "revision_required",
                "A positive expectedRevision is required for this project change",
            ));
        }
        if matches!(
            (self.channel, self.method.as_str()),
            (Channel::Fleet, "init" | "join") | (Channel::Sync, "import")
        ) && role != ClientRole::Desktop
        {
            return Err(ApiError::new(
                "forbidden",
                "This setup operation requires local IPC",
            ));
        }
        Ok(())
    }

    pub fn policy(&self) -> Result<CommandPolicy, ApiError> {
        use Channel::*;
        if self.channel == Desktop {
            return desktop_policy(self);
        }
        let method = self.method.as_str();
        let read = match self.channel {
            System => matches!(method, "status" | "account" | "models" | "login.status"),
            Fleet => matches!(method, "status" | "nodes" | "devices"),
            Sessions => matches!(
                method,
                "list" | "read" | "turns" | "attachment.preview" | "attachment.read"
            ),
            Chat => matches!(
                method,
                "drafts.list"
                    | "drafts.get"
                    | "queue.list"
                    | "attachments.list"
                    | "attachments.preview"
                    | "recovery.list"
            ),
            Agents => matches!(
                method,
                "agents.list"
                    | "agents.read"
                    | "agents.drafts"
                    | "agents.sessions"
                    | "memory.projects"
                    | "memory.list"
                    | "memory.search"
                    | "skills.list"
                    | "tasks.list"
                    | "tasks.proposals"
                    | "workflows.list"
                    | "agents.settings"
                    | "agents.events"
                    | "agents.decisions"
            ),
            Questions => method == "list",
            Approvals => method == "list",
            Projects => method == "list",
            Sync => matches!(
                method,
                "status" | "records" | "conflicts" | "history" | "previewProfile"
            ),
            Desktop => matches!(
                method,
                "engine_connect"
                    | "engine_request"
                    | "chat_request"
                    | "agents_request"
                    | "tickets_request"
                    | "intelligent_ui_request"
                    | "engine_respond"
                    | "project_request"
                    | "browser_request"
            ),
        };
        if read {
            return Ok(CommandPolicy {
                mutating: false,
                live: false,
            });
        }
        let write = match self.channel {
            System => matches!(method, "login.start" | "login.cancel" | "logout"),
            Fleet => matches!(
                method,
                "init"
                    | "join"
                    | "invitation.create"
                    | "node.register"
                    | "node.remove"
                    | "device.revoke"
            ),
            Sessions => matches!(
                method,
                "start" | "resume" | "send" | "steer" | "stop" | "rename" | "archive" | "delete"
            ),
            Chat => matches!(
                method,
                "foreground.submit"
                    | "drafts.save"
                    | "drafts.promote"
                    | "drafts.delete"
                    | "queue.pause"
                    | "queue.resume"
                    | "queue.remove"
                    | "queue.edit"
                    | "queue.steer"
                    | "attachments.importBytes"
                    | "attachments.remove"
                    | "tasks.assign"
                    | "recovery.restore"
            ),
            Agents => matches!(
                method,
                "agents.interview"
                    | "agents.edit"
                    | "agents.reject"
                    | "agents.approve"
                    | "agents.open"
                    | "memory.revise"
                    | "memory.forget"
                    | "memory.undo"
                    | "skills.revoke"
                    | "tasks.create"
                    | "tasks.control"
                    | "tasks.approve"
                    | "tasks.schedule"
                    | "workflows.create"
                    | "workflows.control"
                    | "agents.settings.update"
            ),
            Questions => matches!(method, "answer" | "dismiss" | "draft"),
            Approvals => method == "answer",
            Projects => matches!(method, "create" | "save" | "moveChat" | "delete"),
            Sync => matches!(
                method,
                "resolve" | "approveProfile" | "synchronize" | "import"
            ),
            Desktop => false,
        };
        if !write {
            return Err(ApiError::new(
                "unsupported_method",
                "This command is not part of the Brr client API",
            ));
        }
        let live = matches!(
            (self.channel, method),
            (Sessions, "steer" | "stop") | (Approvals, "answer")
        );
        Ok(CommandPolicy {
            mutating: true,
            live,
        })
    }
}

fn desktop_policy(request: &Request) -> Result<CommandPolicy, ApiError> {
    let method = request.params["method"].as_str().unwrap_or_default();
    let policy = match request.method.as_str() {
        "engine_connect" => Some(CommandPolicy {
            mutating: false,
            live: false,
        }),
        "engine_request" => raw_runtime_policy(method),
        "chat_request" => desktop_chat_policy(method),
        "agents_request" => {
            if method.starts_with("questions.") {
                domain_policy(Channel::Questions, method.trim_start_matches("questions."))
            } else {
                domain_policy(Channel::Agents, method)
            }
        }
        "project_request" => match method {
            "list" | "inspect" => Some(CommandPolicy {
                mutating: false,
                live: false,
            }),
            "create" | "save" | "move" | "delete" | "archive" | "approveWorktree" => {
                Some(CommandPolicy {
                    mutating: true,
                    live: false,
                })
            }
            _ => None,
        },
        "tickets_request" => match method {
            "projects" | "agents" | "list" | "read" | "proposals" => Some(CommandPolicy {
                mutating: false,
                live: false,
            }),
            "create" | "update" | "move" | "comment" | "proposeRun" | "approveRun"
            | "rejectRun" => Some(CommandPolicy {
                mutating: true,
                live: false,
            }),
            _ => None,
        },
        "intelligent_ui_request" => match method {
            "state.read" | "document.read" => Some(CommandPolicy {
                mutating: false,
                live: false,
            }),
            "state.write" | "action.claim" | "action.prepare" | "form" | "button" | "input"
            | "select" | "slider" => Some(CommandPolicy {
                mutating: true,
                live: false,
            }),
            _ => None,
        },
        "engine_respond" => Some(CommandPolicy {
            mutating: true,
            live: true,
        }),
        "browser_request" => Some(CommandPolicy {
            mutating: true,
            live: false,
        }),
        _ => None,
    };
    policy.ok_or_else(|| {
        ApiError::new(
            "unsupported_method",
            "This desktop operation is not part of the scoped host API",
        )
    })
}

fn desktop_chat_policy(method: &str) -> Option<CommandPolicy> {
    match method {
        // Native path selection and staged byte uploads are local desktop
        // capabilities. They are deliberately absent from Channel::Chat.
        "attachments.import"
        | "attachments.upload.begin"
        | "attachments.upload.chunk"
        | "attachments.upload.commit"
        | "attachments.upload.cancel"
        | "recovery.cancel" => Some(CommandPolicy {
            mutating: true,
            live: false,
        }),
        "attachments.upload.status" => Some(CommandPolicy {
            mutating: false,
            live: false,
        }),
        _ => domain_policy(Channel::Chat, method),
    }
}

fn raw_runtime_policy(method: &str) -> Option<CommandPolicy> {
    let read = matches!(
        method,
        "account/read"
            | "model/list"
            | "thread/list"
            | "thread/read"
            | "thread/turns/list"
            | "skills/list"
            | "config/read"
            | "config/value/read"
            | "mcpServer/list"
            | "mcpServerStatus/list"
            | "mcpServer/prompts/list"
            | "mcpServer/prompt/get"
            | "mcpServer/completion/complete"
            | "mcpServer/resource/read"
            | "plugin/list"
            | "plugin/search"
            | "plugin/read"
            | "app/list"
            | "app/installed"
            | "hooks/list"
            | "brr/project/inspect"
            | "account/login/status"
            | "thread/loaded/list"
    );
    if read {
        return Some(CommandPolicy {
            mutating: false,
            live: false,
        });
    }
    let write = matches!(
        method,
        "account/login/start"
            | "account/login/cancel"
            | "account/logout"
            | "thread/start"
            | "thread/resume"
            | "turn/start"
            | "turn/steer"
            | "turn/interrupt"
            | "thread/archive"
            | "thread/unarchive"
            | "thread/delete"
            | "thread/name/set"
            | "config/value/write"
            | "config/batchWrite"
            | "config/mcpServer/reload"
            | "mcpServer/tool/call"
            | "mcpServer/oauth/login"
            | "plugin/install"
            | "plugin/uninstall"
            | "marketplace/add"
            | "marketplace/remove"
            | "marketplace/upgrade"
            | "brr/worktree/approve"
    );
    write.then_some(CommandPolicy {
        mutating: true,
        live: matches!(method, "turn/steer" | "turn/interrupt"),
    })
}

fn domain_policy(channel: Channel, method: &str) -> Option<CommandPolicy> {
    use Channel::*;
    let read = match channel {
        Chat => matches!(
            method,
            "drafts.list"
                | "drafts.get"
                | "queue.list"
                | "attachments.list"
                | "attachments.preview"
                | "recovery.list"
                | "submissions.get"
                | "submissions.list"
                | "queue.state"
                | "operations.get"
                | "operations.list"
                | "references.search"
                | "branches.pending"
        ),
        Agents => matches!(
            method,
            "agents.list"
                | "agents.read"
                | "agents.drafts"
                | "agents.sessions"
                | "memory.projects"
                | "memory.list"
                | "memory.search"
                | "skills.list"
                | "tasks.list"
                | "tasks.proposals"
                | "workflows.list"
                | "agents.settings"
                | "agents.events"
                | "agents.decisions"
        ),
        Questions => method == "list",
        Projects => method == "list" || method == "inspect",
        _ => false,
    };
    if read {
        return Some(CommandPolicy {
            mutating: false,
            live: false,
        });
    }
    let write = match channel {
        Chat => matches!(
            method,
            "foreground.submit"
                | "tasks.assign"
                | "queue.pause"
                | "queue.resume"
                | "queue.remove"
                | "queue.edit"
                | "queue.steer"
                | "drafts.save"
                | "drafts.promote"
                | "drafts.delete"
                | "attachments.importBytes"
                | "attachments.remove"
                | "recovery.restore"
                | "references.resolve"
                | "branches.create"
                | "branches.recover"
        ),
        Agents => matches!(
            method,
            "agents.interview"
                | "agents.edit"
                | "agents.reject"
                | "agents.approve"
                | "agents.open"
                | "memory.revise"
                | "memory.forget"
                | "memory.undo"
                | "skills.revoke"
                | "tasks.create"
                | "tasks.control"
                | "tasks.approve"
                | "tasks.schedule"
                | "workflows.create"
                | "workflows.control"
                | "agents.settings.update"
        ),
        Questions => matches!(method, "answer" | "dismiss" | "draft" | "saveDraft"),
        Projects => matches!(method, "create" | "save" | "moveChat" | "delete"),
        _ => false,
    };
    write.then_some(CommandPolicy {
        mutating: true,
        live: false,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandPolicy {
    pub mutating: bool,
    pub live: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EventCursor {
    pub epoch: String,
    pub sequence: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub node_id: String,
    pub epoch: String,
    pub sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_epoch: Option<String>,
    pub message: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventReplay {
    pub epoch: String,
    pub latest: u64,
    pub requires_snapshot: bool,
    pub events: Vec<Event>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeSummary {
    pub node_id: String,
    pub name: String,
    pub platform: String,
    pub online: bool,
    pub primary: bool,
    pub authenticated: bool,
    pub runtime_connected: bool,
    pub epoch: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotError {
    pub scope: String,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub protocol_version: u32,
    pub node_id: String,
    pub epoch: String,
    pub cursor: EventCursor,
    pub nodes: Vec<NodeSummary>,
    pub account: Option<Value>,
    pub models: Vec<Value>,
    pub threads: Vec<Value>,
    pub projects: Vec<Value>,
    pub agents: Vec<Value>,
    pub tasks: Vec<Value>,
    pub questions: Vec<Value>,
    pub approvals: Vec<Value>,
    pub sync: Value,
    pub errors: Vec<SnapshotError>,
}

fn valid_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(channel: Channel, method: &str) -> Request {
        Request {
            id: "request-1".into(),
            channel,
            method: method.into(),
            params: json!({}),
            operation_id: None,
            expected_epoch: None,
            expected_revision: None,
            target_node_id: None,
        }
    }

    #[test]
    fn paired_mobile_cannot_invoke_desktop_runtime_configuration() {
        let mut command = request(Channel::Desktop, "engine_request");
        command.params = json!({"method":"config/value/write","params":{"keyPath":"model_provider","value":"other"}});
        assert_eq!(
            command.validate(ClientRole::Mobile).unwrap_err().code,
            "forbidden"
        );
    }

    #[test]
    fn session_channel_cannot_be_used_as_an_arbitrary_runtime_tunnel() {
        let command = request(Channel::Sessions, "account/login/start");
        assert_eq!(
            command.validate(ClientRole::Mobile).unwrap_err().code,
            "unsupported_method"
        );
        assert!(request(Channel::Sessions, "list")
            .validate(ClientRole::Mobile)
            .is_ok());
    }

    #[test]
    fn saved_attachment_reads_need_no_execution_or_operation_fence() {
        for method in ["attachment.preview", "attachment.read"] {
            let mut command = request(Channel::Sessions, method);
            command.params = json!({"threadId":"saved-thread","sha256":"a".repeat(64)});
            command.target_node_id = Some("original-owner".into());
            command.validate(ClientRole::Mobile).unwrap();
            let policy = command.policy().unwrap();
            assert!(!policy.mutating);
            assert!(!policy.live);
        }
    }

    #[test]
    fn remote_mutations_require_a_stable_operation_identity() {
        let mut command = request(Channel::Agents, "tasks.create");
        assert_eq!(
            command.validate(ClientRole::Mobile).unwrap_err().code,
            "operation_id_required"
        );
        command.operation_id = Some("op-1".into());
        assert!(command.validate(ClientRole::Mobile).is_ok());
    }

    #[test]
    fn desktop_nested_runtime_mutations_use_operation_and_live_epoch_fences() {
        let mut start = request(Channel::Desktop, "engine_request");
        start.params = json!({"method":"turn/start","params":{"threadId":"thread-1"}});
        assert!(start.policy().unwrap().mutating);
        start.operation_id = Some("turn-start-once".into());
        assert!(start.validate(ClientRole::Desktop).is_ok());

        let mut steer = request(Channel::Desktop, "engine_request");
        steer.params = json!({"method":"turn/steer","params":{"threadId":"thread-1"}});
        assert!(steer.policy().unwrap().live);
        steer.operation_id = Some("turn-steer-once".into());
        assert_eq!(
            steer.validate(ClientRole::Desktop).unwrap_err().code,
            "epoch_required"
        );
        steer.expected_epoch = Some("runtime-epoch-1".into());
        assert!(steer.validate(ClientRole::Desktop).is_ok());
    }

    #[test]
    fn desktop_nested_read_allowlist_and_agents_policy_are_classified() {
        let mut read = request(Channel::Desktop, "engine_request");
        read.params = json!({"method":"thread/read","params":{"threadId":"thread-1"}});
        assert!(!read.policy().unwrap().mutating);
        let mut agent_read = request(Channel::Desktop, "agents_request");
        agent_read.params = json!({"method":"agents.list","params":{}});
        assert!(!agent_read.policy().unwrap().mutating);
        let mut agent_write = request(Channel::Desktop, "agents_request");
        agent_write.params = json!({"method":"agents.reject","params":{"draftId":"draft-1"}});
        assert!(agent_write.policy().unwrap().mutating);
        let mut unknown = request(Channel::Desktop, "engine_request");
        unknown.params = json!({"method":"not/a/known-method","params":{}});
        assert_eq!(unknown.policy().unwrap_err().code, "unsupported_method");
    }

    #[test]
    fn selected_host_paths_can_be_imported_only_through_desktop_chat_ipc() {
        let mut local = request(Channel::Desktop, "chat_request");
        local.operation_id = Some("desktop-attachment-import".into());
        local.params = json!({"method":"attachments.import","params":{"ownerId":"draft-1","paths":["/picked/notes.txt"]}});
        assert!(local.validate(ClientRole::Desktop).is_ok());
        assert!(local.policy().unwrap().mutating);

        let mut remote = request(Channel::Chat, "attachments.import");
        remote.operation_id = Some("remote-path-import".into());
        assert_eq!(remote.policy().unwrap_err().code, "unsupported_method");

        assert_eq!(local.validate(ClientRole::Node).unwrap_err().code, "forbidden");
        assert_eq!(local.validate(ClientRole::Mobile).unwrap_err().code, "forbidden");

        let mut recovery_cancel = request(Channel::Desktop, "chat_request");
        recovery_cancel.operation_id = Some("desktop-recovery-cancel".into());
        recovery_cancel.params = json!({"method":"recovery.cancel","params":{"submissionId":"submission-1","expectedRevision":2}});
        assert!(recovery_cancel.validate(ClientRole::Desktop).is_ok());
        assert!(recovery_cancel.policy().unwrap().mutating);
        let mut remote_recovery_cancel = request(Channel::Chat, "recovery.cancel");
        remote_recovery_cancel.operation_id = Some("remote-recovery-cancel".into());
        assert_eq!(remote_recovery_cancel.policy().unwrap_err().code, "unsupported_method");
    }

    #[test]
    fn staged_attachment_upload_methods_are_desktop_local_and_receipt_reads_are_read_only() {
        for method in [
            "attachments.upload.begin",
            "attachments.upload.chunk",
            "attachments.upload.commit",
            "attachments.upload.cancel",
        ] {
            let mut local = request(Channel::Desktop, "chat_request");
            local.operation_id = Some(format!("desktop-{method}"));
            local.params = json!({"method":method,"params":{"ownerId":"draft-1","uploadId":"upload-1"}});
            assert!(local.validate(ClientRole::Desktop).is_ok(), "{method} should be local Desktop API");
            assert!(local.policy().unwrap().mutating, "{method} must have a durable operation receipt");
            assert_eq!(local.validate(ClientRole::Node).unwrap_err().code, "forbidden");
            assert_eq!(local.validate(ClientRole::Mobile).unwrap_err().code, "forbidden");

            let mut remote = request(Channel::Chat, method);
            remote.operation_id = Some("remote-staged-upload".into());
            assert_eq!(remote.policy().unwrap_err().code, "unsupported_method");
        }

        let mut status = request(Channel::Desktop, "chat_request");
        status.params = json!({"method":"attachments.upload.status","params":{"ownerId":"draft-1","uploadId":"upload-1"}});
        assert!(status.validate(ClientRole::Desktop).is_ok());
        assert!(!status.policy().unwrap().mutating);
        let mut remote_status = request(Channel::Chat, "attachments.upload.status");
        assert_eq!(remote_status.policy().unwrap_err().code, "unsupported_method");
    }

    #[test]
    fn live_approval_requires_the_runtime_generation_that_created_it() {
        let mut command = request(Channel::Approvals, "answer");
        command.operation_id = Some("op-1".into());
        assert_eq!(
            command.validate(ClientRole::Mobile).unwrap_err().code,
            "epoch_required"
        );
        command.expected_epoch = Some("runtime-generation-1".into());
        assert!(command.validate(ClientRole::Mobile).is_ok());
    }

    #[test]
    fn only_a_paired_node_can_complete_reverse_registration() {
        let mut command = request(Channel::Fleet, "node.register");
        command.operation_id = Some("register-node".into());
        assert!(command.validate(ClientRole::Node).is_ok());
        assert_eq!(
            command.validate(ClientRole::Mobile).unwrap_err().code,
            "forbidden"
        );
        assert_eq!(
            command.validate(ClientRole::Desktop).unwrap_err().code,
            "forbidden"
        );
    }

    #[test]
    fn scoped_projects_allow_only_supported_operations_and_require_revision_cas() {
        for method in ["archive", "inspect", "approveWorktree"] {
            assert_eq!(
                request(Channel::Projects, method)
                    .policy()
                    .unwrap_err()
                    .code,
                "unsupported_method"
            );
        }
        let mut save = request(Channel::Projects, "save");
        save.operation_id = Some("project-save".into());
        assert_eq!(
            save.validate(ClientRole::Mobile).unwrap_err().code,
            "revision_required"
        );
        save.expected_revision = Some("2".into());
        assert!(save.validate(ClientRole::Mobile).is_ok());

        let mut delete = request(Channel::Projects, "delete");
        delete.operation_id = Some("project-delete".into());
        delete.expected_revision = Some("0".into());
        assert_eq!(
            delete.validate(ClientRole::Mobile).unwrap_err().code,
            "revision_required"
        );

        let mut desktop = request(Channel::Desktop, "project_request");
        desktop.params = json!({"method":"save","params":{"path":"/owner/project"}});
        desktop.operation_id = Some("desktop-project-save".into());
        assert!(desktop.validate(ClientRole::Desktop).is_ok());
    }
}
