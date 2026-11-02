# Agent implementation contracts

Shared Rust crate: runtime/brr/agents, package brr-agent-core, async sqlx 0.9 SQLite. AgentStore is Clone, has pub(crate) pool: SqlitePool and root: Arc<PathBuf>. Common helpers: pub(crate) fn id() -> String, now() -> i64 (epoch seconds), digest(&impl Serialize) -> Result<String,String>. Public APIs return Result<T,String>. Child modules profiles, memory, tasks, schedule. AgentStore::open(root) initializes each module. Public event API events(after,limit) returns monotonic events; common events(seq,kind,entity_id,payload,created_at), operation_results(operation_id PRIMARY KEY,kind,result). Use one shared database. Public types serialize camelCase. Identity bindings are persisted and immutable.

## Frontend wire contract

All calls use invoke('agents_request',{method,params}); root integrates this trusted native endpoint. Frontend uses same rpc event bus 'engine-event' with methods brr/agents/changed and brr/tasks/changed plus normal runtime thread/turn events.

- agents.list {} => {data: AgentProfile[]}
- agents.read {agentId} => AgentProfile
- agents.interview {model,effort} => {thread: Thread}
- agents.edit {agentId} => {thread: Thread}
- agents.drafts {threadId?} => {data: ProfileDraft[]}
- agents.reject {draftId,hash} => {}
- agents.workspace.inspect {cwd} => {git:boolean,commands:SetupCommand[]}
- agents.approve {draftId,hash} => {agent: AgentProfile} (records user review, executes approved harness creation/application and is idempotent)
- agents.sessions {agentId} => {data: Thread[]}
- agents.open {agentId,model?,effort?,cwd?,readOnly?} => {thread: Thread}
- memory.projects {} => {data: canonicalProjectId[]}
- memory.list {agentId?,projectId?} => {data: MemoryRecord[],sources: MemorySource[]}
- memory.search {agentId?,projectId?,query} => {data: MemoryRecord[]}
- memory.revise {agentId?,projectId?,id,revision,content} => MemoryRecord
- memory.forget {agentId?,projectId?,id} => {}
- memory.undo {eventId} => {}
- skills.list {agentId?,projectId?} => {data: LearnedSkill[]}
- skills.revoke {agentId?,projectId?,id} => {}
- tasks.list {agentId?} => {data: Task[]}
- tasks.create {agentId,title,instructions,acceptanceCriteria:string[],cwd?,projectId?,model?,effort?,dependencies?:string[],context?,sourceThreadId?} => Task
- tasks.control {taskId,action:'pause'|'resume'|'cancel'|'retry'} => Task
- tasks.proposals {agentId?} => {data: DelegationProposal[]}
- tasks.approve {proposalId,hash} => {data: Task[]}
- tasks.schedule {proposalId,hash,name,expression,timezone} => Workflow (atomic reviewed graph scheduling; exact retries reuse the workflow)
- workflows.list {agentId?} => {data: Workflow[]}
- workflows.create {name,agentId,title,instructions,acceptanceCriteria,cwd?,model?,effort?,expression,timezone} => Workflow (root adapts to typed core input)
- workflows.control {workflowId,paused:boolean} => Workflow
- agents.settings {} => {maxBackground:4,maxPerAgent:2}
- agents.settings.update {maxBackground,maxPerAgent} => settings
- agents.events {after?:number,limit?:number} => {data: AgentEvent[]}
- agents.decisions {} => {data: decision[]} (host pending approvals + delegation proposals)

ProfileCandidate/Profile fields: name,description,instructions,soul,model,effort. AgentProfile adds id,revision,createdAt,updatedAt. ProfileDraft: id,threadId,agentId?:string,candidate:ProfileCandidate,hash,status.
MemoryRecord: id,scope:'agent'|'user'|'project',scopeId,kind,content,revision,sourceIds:string[],createdAt,updatedAt,eventId?:number. LearnedSkill: id,name,description,content,revision,scope,scopeId,sourceIds:string[],createdAt,updatedAt.
Task: id,agentId,title,instructions,acceptanceCriteria,context,cwd?,projectId?,model?,effort?,dependencies:string[],status,reason?,runId?,threadId?,result?,evidence?:string[],createdAt,updatedAt. DelegationProposal: id,parentTaskId,hash,status,tasks:{key,task:TaskInput,dependsOn:string[]}[]. Workflow: id,name,agentId,expression,timezone,paused,nextRunAt?,lastRunAt?,lastStatus?.

UI owner creates src/lib/agents.ts and src/components/AgentsPanel.tsx/.css and tests ONLY; parent integrates App.tsx and main composer. AgentsPanel props: models:Model[], desktop:boolean, online:boolean, onOpenThread(thread:Thread):void, onClose():void. Agent Chat tab offers New session and saved sessions; onOpenThread hands off to full existing App chat. Parent may later embed current transcript to preserve chat-first layout. Include interview/create draft review; tabs Tasks/Memory/Skills/Profile; assign tasks, schedules, decisions, settings. Parent handles composer agent selection and @mentions.

## Execution authority

The host captures a task run lease in runtime stdout order before asynchronously handling a dynamic tool request. Tool handling and task controls share a per-task guard; stale requests cannot acquire a successor run's authority. The model never chooses this fence. Runtime interrupt acknowledgement is authoritative: the pinned app-server responds after TurnAborted for a specified turn.

New session bindings determine recall and policy, not renderer-supplied identity fields. Cold resume reapplies binding-derived configuration and trusted instructions; runtime history restores its persisted tool catalog. Opening scheduler-owned task history uses thread/read and cannot alter its model or permissions.

The worker persists exact revisioned evidence batches bounded by serialized source content (40 sources / 14000 bytes), requeues ephemeral reflection after restart and counts it against global background capacity. Source publication rejects stale revisions. Failed workers and continuations compare their original fence before changing state or releasing resources.
