#![cfg(all(feature = "host", feature = "network"))]

use brr_agent_core::{shared_entity_id, SharedTable};
use brr_node::service::{Service, ServiceConfig};
use brr_remote_protocol::{
    Channel, EventCursor, NodeSummary, Request, Snapshot, SnapshotError, PROTOCOL_VERSION,
};
use serde_json::{json, Value};
use std::{path::Path, sync::Arc};

async fn service(path: &Path) -> Arc<Service> {
    Service::open(ServiceConfig {
        data_dir: path.join("data"),
        runtime_path: path.join("missing-runtime"),
        name: "Entity API test".into(),
        endpoint_hosts: vec!["localhost".into()],
    })
    .await
    .unwrap()
}

fn request(channel: Channel, method: &str, params: Value) -> Request {
    Request {
        id: "entity-api".into(),
        channel,
        method: method.into(),
        params,
        operation_id: Some("stable-operation".into()),
        expected_epoch: None,
        expected_revision: None,
        target_node_id: None,
    }
}

async fn seed_agent_entity(
    service: &Service,
    table: SharedTable,
    local: &str,
    origin: &str,
    key: &str,
) {
    let store = service.host().agent_store();
    match table {
        SharedTable::AgentProfiles => {
            sqlx::query("INSERT INTO agent_profiles(id,revision,candidate,created_at,updated_at) VALUES(?,1,'{}',1,1)")
                .bind(local).execute(store.pool()).await.unwrap();
        }
        SharedTable::MemorySources => {
            sqlx::query("INSERT INTO memory_sources(source_id,revision,thread_id,content,active,updated_at) VALUES(?,1,'thread-raw','evidence',1,1)")
                .bind(local).execute(store.pool()).await.unwrap();
        }
        SharedTable::MemoryRecords => {
            sqlx::query("INSERT INTO memory_records(id,scope,scope_id,kind,content,revision,active,created_at,updated_at) VALUES(?,'user','user-id','fact','body',1,1,1,1)")
                .bind(local).execute(store.pool()).await.unwrap();
        }
        SharedTable::LearnedSkills => {
            sqlx::query("INSERT INTO learned_skills(id,name,description,content,revision,scope,scope_id,active,created_at,updated_at) VALUES(?,'name','description','body',1,'user','user-id',1,1,1)")
                .bind(local).execute(store.pool()).await.unwrap();
        }
        _ => panic!("unsupported test table"),
    }
    sqlx::query("INSERT INTO fleet_shared_identity(origin_node_id,table_name,entity_key,local_id) VALUES(?,?,?,?)")
        .bind(origin).bind(entity_table_name(table)).bind(key).bind(local).execute(store.pool()).await.unwrap();
}

fn entity_table_name(table: SharedTable) -> &'static str {
    match table {
        SharedTable::AgentProfiles => "agent_profiles",
        SharedTable::MemorySources => "memory_sources",
        SharedTable::MemoryRecords => "memory_records",
        SharedTable::LearnedSkills => "learned_skills",
        _ => panic!("unsupported test table"),
    }
}

#[tokio::test]
async fn canonical_ids_localize_to_each_owners_exact_sql_ids_and_publicize_back() {
    let primary_dir = tempfile::tempdir().unwrap();
    let worker_dir = tempfile::tempdir().unwrap();
    let primary = service(primary_dir.path()).await;
    let worker = service(worker_dir.path()).await;
    let primary_id = primary.node_id().to_owned();
    let worker_id = worker.node_id().to_owned();

    let profile_key = "owner-profile-key";
    let profile_global = shared_entity_id(&primary_id, SharedTable::AgentProfiles, profile_key);
    seed_agent_entity(
        &primary,
        SharedTable::AgentProfiles,
        profile_key,
        &primary_id,
        profile_key,
    )
    .await;
    seed_agent_entity(
        &worker,
        SharedTable::AgentProfiles,
        &profile_global,
        &primary_id,
        profile_key,
    )
    .await;

    let source_key = "owner-source-key";
    let source_global = shared_entity_id(&primary_id, SharedTable::MemorySources, source_key);
    seed_agent_entity(
        &primary,
        SharedTable::MemorySources,
        source_key,
        &primary_id,
        source_key,
    )
    .await;
    seed_agent_entity(
        &worker,
        SharedTable::MemorySources,
        &source_global,
        &primary_id,
        source_key,
    )
    .await;
    let memory_key = "owner-memory-key";
    let memory_global = shared_entity_id(&primary_id, SharedTable::MemoryRecords, memory_key);
    seed_agent_entity(
        &primary,
        SharedTable::MemoryRecords,
        memory_key,
        &primary_id,
        memory_key,
    )
    .await;
    seed_agent_entity(
        &worker,
        SharedTable::MemoryRecords,
        &memory_global,
        &primary_id,
        memory_key,
    )
    .await;
    let skill_key = "owner-skill-key";
    let skill_global = shared_entity_id(&primary_id, SharedTable::LearnedSkills, skill_key);
    seed_agent_entity(
        &primary,
        SharedTable::LearnedSkills,
        skill_key,
        &primary_id,
        skill_key,
    )
    .await;
    seed_agent_entity(
        &worker,
        SharedTable::LearnedSkills,
        &skill_global,
        &primary_id,
        skill_key,
    )
    .await;

    let project = primary
        .host()
        .project_store()
        .create(brr_host::projects::ProjectInput {
            name: "Shared project".into(),
            folders: vec![],
            primary_folder: None,
            model: None,
            effort: None,
            permission: None,
        })
        .await
        .unwrap();
    let project_global = shared_entity_id(&primary_id, SharedTable::Projects, &project.id);
    let rows = primary
        .host()
        .project_store()
        .shared_snapshot(&primary_id)
        .await
        .unwrap();
    worker
        .host()
        .project_store()
        .materialize_shared_snapshot(&worker_id, &rows, "test-git-head")
        .await
        .unwrap();

    let command = request(
        Channel::Agents,
        "tasks.create",
        json!({
            "agentId": profile_global,
            "projectId": project_global,
            "task": {"agentId":profile_global,"projectId":project_global},
            "nodes": [{"task":{"agentId":profile_global,"projectId":project_global}}]
        }),
    );
    let primary_local = primary.localize_scoped(&command).await.unwrap();
    let worker_local = worker.localize_scoped(&command).await.unwrap();
    assert_eq!(primary_local.params["agentId"], profile_key);
    assert_eq!(worker_local.params["agentId"], profile_global);
    assert_eq!(primary_local.params["projectId"], project.id);
    assert_eq!(worker_local.params["projectId"], project_global);
    assert_eq!(worker_local.params["task"]["agentId"], profile_global);
    assert_eq!(
        worker_local.params["nodes"][0]["task"]["projectId"],
        project_global
    );

    let revise = request(
        Channel::Agents,
        "memory.revise",
        json!({
            "id": memory_global,
            "agentId": profile_global,
            "projectId": project_global,
            "sourceIds": [format!("{source_global}@1")]
        }),
    );
    let localized = worker.localize_scoped(&revise).await.unwrap();
    assert_eq!(localized.params["id"], memory_global);
    assert_eq!(
        localized.params["sourceIds"][0],
        format!("{source_global}@1")
    );

    let mut public_profile = json!({"id":profile_global,"name":"Profile"});
    primary
        .publicize_scoped(
            &request(Channel::Agents, "agents.read", json!({})),
            &mut public_profile,
        )
        .await
        .unwrap();
    assert_eq!(public_profile["id"], profile_global);
    assert_eq!(public_profile["originNodeId"], primary_id);

    let mut public_memory = json!({
        "data":[{"id":memory_global,"scope":"user","scopeId":"user-id","sourceIds":[format!("{source_global}@1")]}],
        "sources":[{"sourceId":source_global,"revision":1,"threadId":"thread-raw"}]
    });
    worker
        .publicize_scoped(
            &request(Channel::Agents, "memory.list", json!({})),
            &mut public_memory,
        )
        .await
        .unwrap();
    assert_eq!(public_memory["data"][0]["id"], memory_global);
    assert_eq!(
        public_memory["data"][0]["sourceIds"][0],
        format!("{source_global}@1")
    );
    assert_eq!(public_memory["sources"][0]["sourceId"], source_global);
    assert_eq!(public_memory["sources"][0]["originNodeId"], primary_id);
    assert_eq!(public_memory["sources"][0]["threadId"], "thread-raw");
}

#[tokio::test]
async fn unknown_canonical_ids_fail_closed_and_desktop_payloads_are_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let service = service(dir.path()).await;
    let unknown = shared_entity_id("absent-node", SharedTable::AgentProfiles, "missing");
    let error = service
        .localize_scoped(&request(
            Channel::Agents,
            "agents.read",
            json!({"agentId":unknown}),
        ))
        .await
        .unwrap_err();
    assert_eq!(error.code, "unknown_shared_entity");

    let desktop = request(
        Channel::Desktop,
        "engine_request",
        json!({
            "params":{"tool":{"agentId":unknown,"projectId":"raw-project"}},
            "sourceIds":["arbitrary@1"]
        }),
    );
    assert_eq!(
        service.localize_scoped(&desktop).await.unwrap().params,
        desktop.params
    );
}

#[tokio::test]
async fn snapshot_publication_keeps_runtime_ids_and_publicizes_only_shared_catalog_entities() {
    let dir = tempfile::tempdir().unwrap();
    let service = service(dir.path()).await;
    let node = service.node_id().to_owned();
    let profile_id = "local-profile";
    seed_agent_entity(
        &service,
        SharedTable::AgentProfiles,
        profile_id,
        &node,
        profile_id,
    )
    .await;
    let project = service
        .host()
        .project_store()
        .create(brr_host::projects::ProjectInput {
            name: "Local project".into(),
            folders: vec![],
            primary_folder: None,
            model: None,
            effort: None,
            permission: None,
        })
        .await
        .unwrap();
    let mut snapshot = Snapshot {
        protocol_version: PROTOCOL_VERSION,
        node_id: node.clone(),
        epoch: "owner-epoch".into(),
        cursor: EventCursor {
            epoch: "owner-epoch".into(),
            sequence: 1,
        },
        nodes: vec![NodeSummary {
            node_id: node.clone(),
            name: "local".into(),
            platform: "test".into(),
            online: true,
            primary: true,
            authenticated: true,
            runtime_connected: false,
            epoch: "owner-epoch".into(),
        }],
        account: None,
        models: vec![],
        threads: vec![json!({"id":"raw-thread"})],
        projects: vec![serde_json::to_value(&project).unwrap()],
        agents: vec![json!({"id":profile_id,"name":"Profile"})],
        tasks: vec![json!({"id":"raw-task","agentId":profile_id,"projectId":project.id})],
        questions: vec![],
        approvals: vec![],
        sync: json!({}),
        errors: vec![SnapshotError {
            scope: "runtime".into(),
            message: "offline".into(),
        }],
    };
    service.publicize_snapshot(&mut snapshot).await.unwrap();
    assert_eq!(
        snapshot.agents[0]["id"],
        shared_entity_id(&node, SharedTable::AgentProfiles, profile_id)
    );
    assert_eq!(snapshot.agents[0]["originNodeId"], node);
    assert_eq!(
        snapshot.projects[0]["id"],
        shared_entity_id(&node, SharedTable::Projects, &project.id)
    );
    assert_eq!(snapshot.tasks[0]["id"], "raw-task");
    assert_eq!(snapshot.tasks[0]["agentId"], snapshot.agents[0]["id"]);
    assert_eq!(snapshot.threads[0]["id"], "raw-thread");
}
