use crate::{bridge::Engine, projects::ProjectInput, worktrees::{ResolvedFolder, SessionWorkspace}};
use serde_json::json;
use std::{path::Path, sync::Arc};
use tokio::process::Command;

async fn engine_at(directory: &Path) -> Engine {
    let script = directory.join("reference-runtime.py");
    std::fs::write(&script, r#"
import json,sys
last_turn=None
for line in sys.stdin:
    request=json.loads(line)
    if 'id' not in request: continue
    method=request['method']
    if method=='initialize': result={'capabilities':{}}
    elif method=='account/read': result={'account':{'type':'chatgpt'}}
    elif method=='model/list': result={'data':[{'model':'test-model','isDefault':True,'defaultReasoningEffort':'medium'}]}
    elif method=='thread/list': result={'data':[],'nextCursor':None}
    elif method=='thread/read': result={'thread':{'id':request['params']['threadId'],'cwd':'.','turns':[]}}
    elif method=='turn/start':
        last_turn=request['params']
        result={'turn':{'id':'mapped-turn'}}
    elif method=='test/state': result={'turn':last_turn}
    else: result={}
    print(json.dumps({'id':request['id'],'result':result}),flush=True)
"#).unwrap();
    let mut command = Command::new("python3");
    command.arg(script).current_dir(directory);
    Engine::connect(command, Arc::new(|_| {})).await.unwrap()
}

fn project_input(folders: Vec<String>, primary_folder: Option<String>) -> ProjectInput {
    ProjectInput {
        name: "Reference project".into(),
        folders,
        primary_folder,
        model: None,
        effort: None,
        permission: None,
    }
}

#[tokio::test]
async fn project_scoped_references_use_registered_roots_and_reject_renderer_paths() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("project");
    let secondary = home.path().join("secondary");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(secondary.join("lib")).unwrap();
    std::fs::write(root.join("src/example.rs"), "fn example() {}\n").unwrap();
    std::fs::write(secondary.join("lib/other.rs"), "fn other() {}\n").unwrap();
    let outside = home.path().join("outside.txt");
    std::fs::write(&outside, "secret").unwrap();
    let init = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .status()
        .unwrap();
    assert!(init.success());
    std::process::Command::new("git")
        .args(["add", "src/example.rs"])
        .current_dir(&root)
        .status()
        .unwrap();
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&secondary)
        .status()
        .unwrap();
    std::process::Command::new("git")
        .args(["add", "lib/other.rs"])
        .current_dir(&secondary)
        .status()
        .unwrap();

    let engine = engine_at(home.path()).await;
    let project = engine
        .agents
        .projects
        .create(project_input(
            vec![
                secondary.to_string_lossy().into_owned(),
                root.to_string_lossy().into_owned(),
            ],
            Some(root.to_string_lossy().into_owned()),
        ))
        .await
        .unwrap();
    let canonical_root = std::fs::canonicalize(&root).unwrap();
    let found = engine
        .agents
        .chat_request(
            "references.search",
            json!({"projectId":project.id,"query":"example.rs"}),
        )
        .await
        .unwrap();
    assert_eq!(found[0]["id"], "src/example.rs");
    assert_eq!(
        found[0]["path"],
        canonical_root
            .join("src/example.rs")
            .to_string_lossy()
            .as_ref()
    );

    let secondary_root = std::fs::canonicalize(&secondary).unwrap();
    let secondary_found = engine
        .agents
        .chat_request(
            "references.search",
            json!({"projectId":project.id,"query":"other.rs"}),
        )
        .await
        .unwrap();
    assert_eq!(
        secondary_found[0]["id"],
        secondary_root
            .join("lib/other.rs")
            .to_string_lossy()
            .as_ref()
    );
    let secondary_resolved = engine
        .agents
        .chat_request(
            "references.resolve",
            json!({"projectId":project.id,"id":secondary_found[0]["id"],"type":"file"}),
        )
        .await
        .unwrap();
    assert_eq!(
        secondary_resolved["root"],
        secondary_root.to_string_lossy().as_ref()
    );
    assert_eq!(secondary_resolved["path"], "lib/other.rs");

    let resolved = engine
        .agents
        .chat_request(
            "references.resolve",
            json!({"projectId":project.id,"id":"src/example.rs","type":"file"}),
        )
        .await
        .unwrap();
    assert_eq!(resolved["root"], canonical_root.to_string_lossy().as_ref());
    assert_eq!(resolved["path"], "src/example.rs");
    assert_eq!(
        resolved["absolutePath"],
        canonical_root
            .join("src/example.rs")
            .to_string_lossy()
            .as_ref()
    );

    let outside = engine
        .agents
        .chat_request(
            "references.resolve",
            json!({"projectId":project.id,"id":outside.to_string_lossy().as_ref(),"type":"file"}),
        )
        .await;
    assert!(
        outside.is_err(),
        "renderer-supplied absolute paths outside registered roots must be rejected"
    );
    assert!(engine.agents.chat_request("references.resolve", json!({"projectId":project.id,"threadId":"some-thread","id":"src/example.rs","type":"file"})).await.is_err());
    assert!(engine
        .agents
        .chat_request(
            "references.resolve",
            json!({"cwd":root,"id":"src/example.rs","type":"file"})
        )
        .await
        .is_err());

    let fallback_project = engine
        .agents
        .projects
        .create(project_input(
            vec![
                root.to_string_lossy().into_owned(),
                secondary.to_string_lossy().into_owned(),
            ],
            Some(root.to_string_lossy().into_owned()),
        ))
        .await
        .unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    let fallback_found = engine
        .agents
        .chat_request(
            "references.search",
            json!({"projectId":fallback_project.id,"query":"other.rs"}),
        )
        .await
        .unwrap();
    assert_eq!(
        fallback_found[0]["id"], "lib/other.rs",
        "a missing primary folder should fall back to the first available registered folder"
    );

    let empty = engine
        .agents
        .projects
        .create(project_input(vec![], None))
        .await
        .unwrap();
    let no_folders = engine
        .agents
        .chat_request(
            "references.resolve",
            json!({"projectId":empty.id,"id":"src/example.rs","type":"file"}),
        )
        .await
        .unwrap_err();
    assert!(no_folders.to_lowercase().contains("folder"));
    engine.shutdown().await;
}

#[tokio::test]
async fn secondary_project_reference_maps_to_saved_isolated_folder_for_open_and_submit() {
    let home = tempfile::tempdir().unwrap();
    let source_primary = home.path().join("source-primary");
    let source_secondary = home.path().join("source-secondary");
    std::fs::create_dir_all(&source_primary).unwrap();
    std::fs::create_dir_all(source_secondary.join("lib")).unwrap();
    let original_file = source_secondary.join("lib/other.rs");
    std::fs::write(&original_file, "fn source_only() {}\n").unwrap();
    for root in [&source_primary, &source_secondary] {
        assert!(std::process::Command::new("git").args(["init", "-q"]).current_dir(root).status().unwrap().success());
    }
    assert!(std::process::Command::new("git").args(["add", "lib/other.rs"]).current_dir(&source_secondary).status().unwrap().success());

    let engine = engine_at(home.path()).await;
    let canonical_primary = std::fs::canonicalize(&source_primary).unwrap();
    let canonical_secondary = std::fs::canonicalize(&source_secondary).unwrap();
    let project = engine.agents.projects.create(project_input(
        vec![canonical_primary.to_string_lossy().into_owned(), canonical_secondary.to_string_lossy().into_owned()],
        Some(canonical_primary.to_string_lossy().into_owned()),
    )).await.unwrap();
    let matches = engine.agents.chat_request("references.search", json!({"projectId":project.id,"query":"other.rs"})).await.unwrap();
    let source_id = matches[0]["id"].as_str().unwrap();
    assert_eq!(source_id, canonical_secondary.join("lib/other.rs").to_string_lossy().as_ref());

    let actual_primary = home.path().join("isolated-primary");
    let actual_secondary = home.path().join("isolated-secondary");
    std::fs::create_dir_all(&actual_primary).unwrap();
    std::fs::create_dir_all(actual_secondary.join("lib")).unwrap();
    let actual_file = actual_secondary.join("lib/other.rs");
    std::fs::write(&actual_file, "fn isolated_copy() {}\n").unwrap();
    let actual_primary = std::fs::canonicalize(actual_primary).unwrap();
    let actual_secondary = std::fs::canonicalize(actual_secondary).unwrap();
    let thread_id = "isolated-reference-thread";
    let snapshot = SessionWorkspace {
        cwd: actual_primary.to_string_lossy().into_owned(),
        roots: vec![actual_primary.to_string_lossy().into_owned(), actual_secondary.to_string_lossy().into_owned()],
        folders: vec![
            ResolvedFolder { source: canonical_primary.to_string_lossy().into_owned(), path: actual_primary.to_string_lossy().into_owned() },
            ResolvedFolder { source: canonical_secondary.to_string_lossy().into_owned(), path: actual_secondary.to_string_lossy().into_owned() },
        ],
        worktrees: vec![],
        scope_project_id: Some(project.id.clone()),
        project_revision: Some(project.revision),
    };
    engine.agents.worktrees.save_session(thread_id, &snapshot).unwrap();
    std::fs::remove_file(&original_file).unwrap();

    let resolved = engine.agents.chat_request("references.resolve", json!({"threadId":thread_id,"id":source_id,"type":"file"})).await.unwrap();
    assert_eq!(resolved["root"], actual_secondary.to_string_lossy().as_ref());
    assert_eq!(resolved["path"], "lib/other.rs");
    assert_eq!(resolved["absolutePath"], actual_secondary.join("lib/other.rs").to_string_lossy().as_ref());

    let submitted = engine.agents.chat_request("foreground.submit", json!({
        "id":"mapped-reference-submit", "threadId":thread_id,
        "document":{"version":1,"text":"see","references":[{"id":source_id,"name":"other.rs","type":"file","start":0,"end":3}],"quotes":[]},
        "attachmentIds":[], "settings":{"model":"test-model","effort":"medium","mode":"default"}, "delivery":"send"
    })).await.unwrap();
    assert_eq!(submitted["submission"]["status"], "accepted");
    let state = engine.agents.rpc.call("test/state", json!({})).await.unwrap();
    let context: serde_json::Value = serde_json::from_str(state["turn"]["additionalContext"]["brr_selected_context"]["value"].as_str().unwrap()).unwrap();
    assert_eq!(context[0]["path"], actual_secondary.join("lib/other.rs").to_string_lossy().as_ref());
    assert!(!context.to_string().contains(&canonical_secondary.to_string_lossy().to_string()), "submission context must use the isolated path and must not access the original source file");
    engine.shutdown().await;
}
